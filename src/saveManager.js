/**
 * Save manager for orchestrating the PDF save process.
 *
 * Pages are rendered at the target DPI, run through the shared operation
 * pipeline (same one used for thumbnails), then embedded as compactly as
 * possible:
 * - B&W pages: CCITT G4 (fallback: raw 1-bit DeviceGray + FlateDecode).
 * - Grayscale, "No Compression": raw 8-bit DeviceGray + FlateDecode (lossless).
 * - Color, "No Compression": lossless PNG.
 * - Everything else: JPEG (MozJPEG) at a quality matching the compression level.
 */

import { applyGeometricOpsToCanvas } from "./pageRenderer.js";
import { applyPixelPipeline, encodeProcessedImage } from "./imagePixelOps.js";
import { getEffectiveColorMode } from "./pageModel.js";
import { forEachConcurrent } from "./pageCommands.js";
import { loadPreservableLibDocs, canPreservePage, copyPreservedPages, recompressPreservedImages } from "./classicPdf.js";

// Pages rendered concurrently during save (overlaps PDF.js decoding with
// main-thread rasterization and worker dispatch)
const SAVE_RENDER_CONCURRENCY = 3;

// Target DPI for compression levels
const TARGET_DPI = {
  color: { none: 300, low: 180, medium: 150, high: 120 },
  gray: { none: 300, low: 200, medium: 150, high: 120 },
  bw: { none: 300, low: 260, medium: 200, high: 150 },
};

function isBwMode(mode) {
  return mode === "bw" || mode === "bw-otsu";
}

/**
 * Gets target DPI based on mode and compression
 */
function getTargetDpi(mode, compression) {
  const modeKey = isBwMode(mode) ? "bw" : (mode === "gray" ? "gray" : "color");
  const dpiTable = TARGET_DPI[modeKey] || TARGET_DPI.gray;
  return dpiTable[compression] || dpiTable.medium;
}

/**
 * Renders a PDF page to canvas at specified DPI
 */
async function renderPdfPage(pdfDoc, pageIndex, dpi = 300) {
  const page = await pdfDoc.getPage(pageIndex + 1);
  const scale = dpi / 72;
  const scaledViewport = page.getViewport({ scale });

  const canvas = document.createElement("canvas");
  canvas.width = Math.round(scaledViewport.width);
  canvas.height = Math.round(scaledViewport.height);

  const ctx = canvas.getContext("2d");
  await page.render({ canvasContext: ctx, viewport: scaledViewport }).promise;

  return canvas;
}

/**
 * Releases canvas memory
 */
function releaseCanvas(canvas) {
  if (canvas) {
    canvas.width = 0;
    canvas.height = 0;
  }
}

/**
 * Yields to UI to prevent freezing
 */
function yieldToUi() {
  if (document.hidden) {
    // MessageChannel is not throttled in background tabs (unlike rAF which is paused)
    return new Promise(resolve => {
      const mc = new MessageChannel();
      mc.port1.onmessage = () => resolve();
      mc.port2.postMessage(undefined);
    });
  }
  return new Promise(resolve => requestAnimationFrame(() => resolve()));
}

/**
 * Embeds a rendered page image into the document and draws it on pdfPage
 * in the rectangle {x, y, width, height} (page coordinate space).
 */
async function drawRenderedImage(pdfDoc, pdfPage, rendered, { x = 0, y = 0, width, height }) {
  if (rendered.kind === "ccitt-g4" || rendered.kind === "raw-gray") {
    const { pushGraphicsState, popGraphicsState, concatTransformationMatrix, drawObject } = window.PDFLib;

    const dict = {
      Type: "XObject",
      Subtype: "Image",
      Width: rendered.width,
      Height: rendered.height,
      ColorSpace: "DeviceGray",
    };
    if (rendered.kind === "ccitt-g4") {
      dict.BitsPerComponent = 1;
      dict.Filter = "CCITTFaxDecode";
      dict.DecodeParms = { K: -1, Columns: rendered.width, Rows: rendered.height, BlackIs1: false };
    } else {
      // rendered.raw is already Flate-compressed by the encode step
      dict.BitsPerComponent = rendered.bitsPerComponent;
      dict.Filter = "FlateDecode";
    }

    const stream = pdfDoc.context.stream(rendered.raw, dict);
    const ref = pdfDoc.context.register(stream);
    const name = pdfPage.node.newXObject("Image", ref);
    pdfPage.pushOperators(
      pushGraphicsState(),
      concatTransformationMatrix(width, 0, 0, height, x, y),
      drawObject(name),
      popGraphicsState(),
    );
    return;
  }

  const image = rendered.kind === "jpeg"
    ? await pdfDoc.embedJpg(rendered.bytes)
    : await pdfDoc.embedPng(rendered.bytes);
  pdfPage.drawImage(image, { x, y, width, height });
}

/**
 * Creates a pool of save workers, or null when workers are unavailable
 */
function createSaveWorkerPool() {
  if (typeof Worker === "undefined") return null;

  let workers;
  try {
    const size = Math.min(4, Math.max(1, (navigator.hardwareConcurrency || 2) - 1));
    workers = Array.from({ length: size }, () => new Worker(new URL("./saveWorker.js", import.meta.url), { type: "module" }));
  } catch (e) {
    return null;
  }

  const pending = new Map();
  let nextId = 0;
  let nextWorker = 0;
  let failed = false;

  const failAll = (error) => {
    failed = true;
    for (const entry of pending.values()) entry.reject(error);
    pending.clear();
  };

  for (const worker of workers) {
    worker.onmessage = (event) => {
      const { id, error, ...result } = event.data;
      const entry = pending.get(id);
      if (!entry) return;
      pending.delete(id);
      if (error) entry.reject(new Error(error));
      else entry.resolve(result);
    };
    // Fires when the worker script itself fails to load or crashes
    worker.onerror = (event) => {
      failAll(new Error(event.message || "Save worker failed."));
    };
  }

  return {
    size: workers.length,
    process(payload, transfer) {
      if (failed) return Promise.reject(new Error("Save worker failed."));
      const id = nextId++;
      const worker = workers[nextWorker];
      nextWorker = (nextWorker + 1) % workers.length;
      return new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject });
        worker.postMessage({ id, ...payload }, transfer);
      });
    },
    terminate() {
      for (const worker of workers) worker.terminate();
    },
  };
}

/**
 * Renders a page and extracts its pixels (geometric ops applied)
 */
async function renderPageImageData(pdfDoc, page, targetDpi) {
  let canvas = await renderPdfPage(pdfDoc, page.sourcePageIndex, targetDpi);
  canvas = applyGeometricOpsToCanvas(canvas, page.operations);
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  const imageData = ctx.getImageData(0, 0, canvas.width, canvas.height);
  releaseCanvas(canvas);
  return imageData;
}

/**
 * Full single-page processing on the main thread (fallback when workers fail)
 */
async function processPageInline({ pdfDoc, page, targetDpi, compression, jpegQuality, needOcrImage }) {
  const imageData = await renderPageImageData(pdfDoc, page, targetDpi);
  const colorMode = applyPixelPipeline(imageData, page.operations, { shadingScale: targetDpi / 300 });
  return await encodeProcessedImage(imageData, { colorMode, compression, jpegQuality, needOcrImage });
}

/**
 * Renders all pages through the shared pipeline and encodes them.
 * Rendering happens on the main thread (PDF.js); the heavy pixel work and
 * encoding run in a pool of Web Workers, pipelined so the UI stays live.
 * Falls back to inline processing when workers are unavailable or fail.
 */
async function renderAllPages({ pages, getPdfDocForPage, jpegQuality = 0.85, compression = "high", needOcrImage = false, onProgress, onStatus }) {
  const results = new Array(pages.length);
  const pool = createSaveWorkerPool();
  let done = 0;

  const concurrency = pool ? SAVE_RENDER_CONCURRENCY : 1;
  await forEachConcurrent(pages, concurrency, async (page, i) => {
    const pdfDoc = getPdfDocForPage ? await getPdfDocForPage(page) : null;
    if (!pdfDoc) {
      throw new Error("Missing PDF source for page rendering.");
    }

    const targetDpi = getTargetDpi(getEffectiveColorMode(page.operations), compression);

    if (pool) {
      const imageData = await renderPageImageData(pdfDoc, page, targetDpi);
      results[i] = await pool
        .process({
          buffer: imageData.data.buffer,
          width: imageData.width,
          height: imageData.height,
          operations: page.operations,
          shadingScale: targetDpi / 300,
          compression,
          jpegQuality,
          needOcrImage,
        }, [imageData.data.buffer])
        .catch(() => ({ failed: true }));
    } else {
      results[i] = await processPageInline({ pdfDoc, page, targetDpi, compression, jpegQuality, needOcrImage });
    }

    done += 1;
    if (onStatus) onStatus(`Rendering page ${done}/${pages.length}`);
    if (onProgress) onProgress(done, pages.length);
    await yieldToUi();
  });
  if (pool) pool.terminate();

  // Reprocess inline any pages whose worker failed
  for (let i = 0; i < pages.length; i++) {
    if (!results[i] || results[i].failed) {
      if (onStatus) onStatus(`Reprocessing page ${i + 1}/${pages.length}...`);
      const page = pages[i];
      const pdfDoc = await getPdfDocForPage(page);
      const targetDpi = getTargetDpi(getEffectiveColorMode(page.operations), compression);
      results[i] = await processPageInline({ pdfDoc, page, targetDpi, compression, jpegQuality, needOcrImage });
      await yieldToUi();
    }
  }

  for (let i = 0; i < pages.length; i++) {
    results[i].pageSizePts = { ...pages[i].pageSizePts };
  }
  return results;
}

/**
 * Runs OCR on rendered pages using scribe.js
 */
async function runOcr({ renderedPages, lang, onProgress, onStatus, scribeModule }) {
  let doc = null;
  try {
    // Use most of the machine for recognition (scribe's default caps at 6
    // workers); must be set before init
    scribeModule.opt.workerN = Math.max(1, Math.min((navigator.hardwareConcurrency || 4) - 2, 12));
    await scribeModule.init({ ocr: true, font: true });

    // Convert to File objects for scribe.js
    const imageFiles = renderedPages.map((page, index) => {
      const blob = new Blob([page.ocrBytes], { type: page.ocrMime });
      const extension = page.ocrMime === "image/jpeg" ? "jpg" : "png";
      return new File([blob], `page_${String(index + 1).padStart(4, "0")}.${extension}`, { type: page.ocrMime });
    });

    doc = new scribeModule.ScribeDoc();

    // Progress: 'convert' messages arrive as pages finish recognition,
    // 'export' while writing the PDF; keep the reported progress monotonic
    const pageCount = renderedPages.length;
    const totalSteps = pageCount * 2;
    let recognized = 0;
    let exported = 0;
    doc.progressHandler = (message) => {
      if (!message || typeof message.n !== "number") return;
      if (message.type === "convert") recognized = Math.max(recognized, Math.min(message.n + 1, pageCount));
      if (message.type === "export") exported = Math.max(exported, Math.min(message.n + 1, pageCount));
      if (onProgress) onProgress(recognized + exported, totalSteps);
      if (onStatus) {
        onStatus(exported > 0
          ? `OCR: generating PDF ${exported}/${pageCount}`
          : `OCR: recognizing ${recognized}/${pageCount}`);
      }
    };

    await doc.importFiles(imageFiles);
    await doc.recognize({ langs: [lang] });
    const textPdf = await doc.exportData("pdf", { displayMode: "ebook" });

    if (!textPdf) return null;
    if (textPdf instanceof Uint8Array) return textPdf;
    if (textPdf instanceof ArrayBuffer) return new Uint8Array(textPdf);
    if (textPdf instanceof Blob) return new Uint8Array(await textPdf.arrayBuffer());

    return null;
  } catch (error) {
    console.error("OCR failed:", error);
    return null;
  } finally {
    if (doc) await doc.terminate().catch(() => {});
  }
}

/**
 * Main save function.
 *
 * Pages split into two groups:
 * - Preserved: "classic" pages (real text layer) from unencrypted sources
 *   whose only operations are rotations. These are copied verbatim with
 *   pdf-lib (text/fonts/vectors kept), rotated via /Rotate, and their
 *   embedded images recompressed in place when that shrinks them.
 * - Rasterized: everything else goes through the render + pixel pipeline +
 *   OCR path exactly as before. A document of pure scans takes this path
 *   for every page, unchanged.
 */
export async function savePdf({ pdfSources, pages, options, onProgress, onStatus }) {
  const { compression, ocrLang, scribeModule, PDFDocument } = options;

  // Build a sourceId -> pdfDoc lookup (window.pdfjsLib is set by app.js)
  const pdfjsLib = window.pdfjsLib;
  const pdfDocCache = new Map();

  const getPdfDocForPage = async (page) => {
    if (!page || !page.sourceId || !pdfSources) return null;
    if (!pdfDocCache.has(page.sourceId)) {
      const source = pdfSources.get ? pdfSources.get(page.sourceId) : pdfSources[page.sourceId];
      let promise;
      if (!source) promise = Promise.resolve(null);
      else if (source.pdfDoc) promise = Promise.resolve(source.pdfDoc);
      else if (source.bytes) promise = pdfjsLib.getDocument({ data: source.bytes.slice(0) }).promise;
      else promise = Promise.resolve(null);
      pdfDocCache.set(page.sourceId, promise);
    }
    return await pdfDocCache.get(page.sourceId);
  };

  const jpegQuality = compression === "low" ? 0.75 : compression === "medium" ? 0.60 : compression === "high" ? 0.50 : 0.85;

  // Phase 0: decide which pages can be preserved (copied, not rasterized)
  const libDocs = await loadPreservableLibDocs({ pdfSources, pages, PDFDocument });
  const preserved = pages.map(page => canPreservePage(page, libDocs));
  const rasterPages = pages.filter((page, i) => !preserved[i]);

  // Preserved pages already have real text, so OCR only concerns raster pages
  const wantOcr = Boolean(ocrLang && ocrLang !== "none" && scribeModule && rasterPages.length > 0);

  // Phase 1: Render the rasterized pages
  let renderedPages = [];
  if (rasterPages.length > 0) {
    if (onStatus) onStatus("Rendering pages...");
    renderedPages = await renderAllPages({
      pages: rasterPages,
      getPdfDocForPage,
      jpegQuality,
      compression,
      needOcrImage: wantOcr,
      onProgress,
      onStatus,
    });
  }

  let ocrUsed = false;
  let finalDoc = null;

  // Phase 2: OCR (if enabled). The scribe PDF holds the raster pages in
  // their relative order; preserved pages are inserted afterwards.
  if (wantOcr) {
    if (onStatus) onStatus("Running OCR...");

    const ocrPdfBytes = await runOcr({
      renderedPages,
      lang: ocrLang,
      onProgress,
      onStatus,
      scribeModule,
    });

    if (ocrPdfBytes) {
      ocrUsed = true;

      if (onStatus) onStatus("Embedding images into OCR PDF...");
      const ocrPdfDoc = await PDFDocument.load(ocrPdfBytes);
      const pageCount = Math.min(ocrPdfDoc.getPageCount(), renderedPages.length);

      for (let i = 0; i < pageCount; i++) {
        if (onProgress) onProgress(i + 1, pageCount);
        if (onStatus) onStatus(`Embedding image ${i + 1}/${pageCount}`);

        const rendered = renderedPages[i];
        const targetW = rendered.pageSizePts.width;
        const targetH = rendered.pageSizePts.height;

        const pdfPage = ocrPdfDoc.getPage(i);
        const { width: ocrW, height: ocrH } = pdfPage.getSize();

        // Scale OCR text layer from pixel coords to target point coords
        pdfPage.scaleContent(targetW / ocrW, targetH / ocrH);
        pdfPage.setSize(targetW, targetH);

        // Content drawn after scaleContent shares its transform, so draw at
        // the OCR page's original size to end up filling the target page
        await drawRenderedImage(ocrPdfDoc, pdfPage, rendered, {
          x: 0,
          y: 0,
          width: ocrW,
          height: ocrH,
        });

        await yieldToUi();
      }

      // Insert preserved pages at their final positions (ascending order
      // keeps earlier insertions and raster page order intact)
      if (preserved.some(Boolean)) {
        if (onStatus) onStatus("Copying original pages...");
        const copied = await copyPreservedPages({ targetDoc: ocrPdfDoc, pages, preserved, libDocs });
        for (const finalIndex of [...copied.keys()].sort((a, b) => a - b)) {
          ocrPdfDoc.insertPage(finalIndex, copied.get(finalIndex));
        }
        await recompressCopiedImages({ pdfDoc: ocrPdfDoc, copied, compression, jpegQuality, onStatus });
      }

      finalDoc = ocrPdfDoc;
    } else {
      if (onStatus) onStatus("OCR failed, saving without OCR...");
    }
  }

  // Phase 3: Create PDF without OCR if needed
  if (!finalDoc) {
    if (onStatus) onStatus("Creating PDF...");
    const outputPdf = await PDFDocument.create();
    const copied = preserved.some(Boolean)
      ? await copyPreservedPages({ targetDoc: outputPdf, pages, preserved, libDocs })
      : new Map();

    let rasterDone = 0;
    for (let i = 0; i < pages.length; i++) {
      if (onProgress) onProgress(i + 1, pages.length);
      if (onStatus) onStatus(`Adding page ${i + 1}/${pages.length}`);

      if (preserved[i]) {
        outputPdf.addPage(copied.get(i));
        continue;
      }

      const rendered = renderedPages[rasterDone];
      rasterDone += 1;
      const pdfPage = outputPdf.addPage([rendered.pageSizePts.width, rendered.pageSizePts.height]);
      await drawRenderedImage(outputPdf, pdfPage, rendered, {
        x: 0,
        y: 0,
        width: rendered.pageSizePts.width,
        height: rendered.pageSizePts.height,
      });

      await yieldToUi();
    }

    await recompressCopiedImages({ pdfDoc: outputPdf, copied, compression, jpegQuality, onStatus });
    finalDoc = outputPdf;
  }

  if (onStatus) onStatus("Finalizing PDF...");
  const finalPdfBytes = await finalDoc.save({ useObjectStreams: true });
  return { pdfBytes: finalPdfBytes, ocrUsed };
}

/**
 * Recompresses the embedded images of copied (preserved) pages, unless the
 * user asked for lossless output
 */
async function recompressCopiedImages({ pdfDoc, copied, compression, jpegQuality, onStatus }) {
  if (copied.size === 0 || compression === "none") return;
  const replaced = await recompressPreservedImages({
    pdfDoc,
    pdfPages: copied.values(),
    jpegQuality,
    onStatus,
    yieldToUi,
  });
  if (replaced > 0 && onStatus) onStatus(`Recompressed ${replaced} embedded image${replaced === 1 ? "" : "s"}.`);
}
