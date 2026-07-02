/**
 * Save manager for orchestrating the PDF save process.
 *
 * Pages are rendered at the target DPI, run through the shared operation
 * pipeline (same one used for thumbnails), then embedded as compactly as
 * possible:
 * - B&W pages: raw 1-bit DeviceGray + FlateDecode (packed bits, ~24x smaller
 *   than letting pdf-lib re-expand a PNG to 24-bit RGB).
 * - Grayscale, "No Compression": raw 8-bit DeviceGray + FlateDecode (lossless).
 * - Color, "No Compression": lossless PNG.
 * - Everything else: JPEG at a quality matching the compression level.
 */

import { applyGeometricOpsToCanvas } from "./thumbnailRenderer.js";
import { applyPixelPipeline, encodeProcessedImage } from "./imagePixelOps.js";
import { getEffectiveColorMode } from "./pageModel.js";

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
  if (rendered.kind === "raw-gray") {
    const { pushGraphicsState, popGraphicsState, concatTransformationMatrix, drawObject } = window.PDFLib;
    const compressed = pako.deflate(rendered.raw);
    const stream = pdfDoc.context.stream(compressed, {
      Type: "XObject",
      Subtype: "Image",
      Width: rendered.width,
      Height: rendered.height,
      ColorSpace: "DeviceGray",
      BitsPerComponent: rendered.bitsPerComponent,
      Filter: "FlateDecode",
    });
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
export async function renderAllPages({ pages, getPdfDocForPage, jpegQuality = 0.85, compression = "high", needOcrImage = false, onProgress, onStatus }) {
  const results = new Array(pages.length);
  const pool = createSaveWorkerPool();
  const pending = []; // { index, promise } in dispatch order

  const settleOldest = async () => {
    const { index, promise } = pending.shift();
    results[index] = await promise; // marked { failed } on worker error
  };

  for (let i = 0; i < pages.length; i++) {
    const page = pages[i];
    const pdfDoc = getPdfDocForPage ? await getPdfDocForPage(page) : null;
    if (!pdfDoc) {
      throw new Error("Missing PDF source for page rendering.");
    }

    if (onStatus) onStatus(`Rendering page ${i + 1}/${pages.length}`);
    if (onProgress) onProgress(i + 1, pages.length);

    const targetDpi = getTargetDpi(getEffectiveColorMode(page.operations), compression);
    const params = { pdfDoc, page, targetDpi, compression, jpegQuality, needOcrImage };

    if (pool) {
      const imageData = await renderPageImageData(pdfDoc, page, targetDpi);
      const promise = pool
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
      pending.push({ index: i, promise });
      if (pending.length >= pool.size) await settleOldest();
    } else {
      results[i] = await processPageInline(params);
    }

    await yieldToUi();
  }

  while (pending.length) {
    if (onStatus) onStatus("Encoding pages...");
    await settleOldest();
  }
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
export async function runOcr({ renderedPages, lang, onProgress, onStatus, scribeModule }) {
  try {
    await scribeModule.init({ ocr: true, font: true, pdf: true });
    scribeModule.opt.displayMode = "ebook";
    scribeModule.opt.intermediatePDF = false;

    const stageOrder = ["importImage", "convert", "export"];
    const totalSteps = Math.max(1, renderedPages.length * stageOrder.length);
    let maxProgress = 0;

    scribeModule.opt.progressHandler = (message) => {
      if (!message || typeof message.n !== "number") return;
      const stage = message.type || "ocr";
      const stageIndex = Math.max(0, stageOrder.indexOf(stage));
      const stepInStage = Math.min(message.n + 1, renderedPages.length);
      const overallStep = Math.min(stageIndex * renderedPages.length + stepInStage, totalSteps);

      // Ensure progress never decreases (monotonic)
      maxProgress = Math.max(maxProgress, overallStep);
      if (onProgress) onProgress(maxProgress, totalSteps);

      // Derive displayed stage and step from maxProgress for monotonic status text
      const displayStageIndex = Math.min(Math.floor((maxProgress - 1) / renderedPages.length), stageOrder.length - 1);
      const displayStep = maxProgress - displayStageIndex * renderedPages.length;
      const displayStage = stageOrder[displayStageIndex] || "convert";

      let stageMessage = "Processing...";
      if (displayStage === "importImage") stageMessage = `OCR: loading images ${displayStep}/${renderedPages.length}`;
      if (displayStage === "convert") stageMessage = `OCR: recognizing ${displayStep}/${renderedPages.length}`;
      if (displayStage === "export") stageMessage = `OCR: generating PDF ${displayStep}/${renderedPages.length}`;
      if (onStatus) onStatus(stageMessage);
    };

    // Convert to File objects for scribe.js
    const imageFiles = renderedPages.map((page, index) => {
      const blob = new Blob([page.ocrBytes], { type: page.ocrMime });
      const extension = page.ocrMime === "image/jpeg" ? "jpg" : "png";
      return new File([blob], `page_${String(index + 1).padStart(4, "0")}.${extension}`, { type: page.ocrMime });
    });

    await scribeModule.importFiles({ imageFiles });
    await scribeModule.recognize({ langs: [lang] });
    const textPdf = await scribeModule.exportData("pdf");
    await scribeModule.clear();

    if (!textPdf) return null;
    if (textPdf instanceof Uint8Array) return textPdf;
    if (textPdf instanceof ArrayBuffer) return new Uint8Array(textPdf);
    if (textPdf instanceof Blob) return new Uint8Array(await textPdf.arrayBuffer());

    return null;
  } catch (error) {
    console.error("OCR failed:", error);
    return null;
  }
}

/**
 * Main save function
 */
export async function savePdf({ pdfSources, pages, options, onProgress, onStatus }) {
  const { compression, ocrLang, scribeModule, PDFDocument } = options;

  // Build a sourceId -> pdfDoc lookup
  const pdfjsLib = window["pdfjs-dist/build/pdf"];
  const pdfDocCache = new Map();

  const getPdfDocForPage = async (page) => {
    if (!page || !page.sourceId || !pdfSources) return null;
    if (pdfDocCache.has(page.sourceId)) {
      return await pdfDocCache.get(page.sourceId);
    }

    const source = pdfSources.get ? pdfSources.get(page.sourceId) : pdfSources[page.sourceId];
    if (!source) return null;
    if (source.pdfDoc) {
      pdfDocCache.set(page.sourceId, source.pdfDoc);
      return source.pdfDoc;
    }
    if (!source.bytes) return null;

    const loadingTask = pdfjsLib.getDocument({ data: source.bytes.slice(0) });
    const promise = loadingTask.promise;
    pdfDocCache.set(page.sourceId, promise);
    const pdfDoc = await promise;
    pdfDocCache.set(page.sourceId, pdfDoc);
    return pdfDoc;
  };

  const jpegQuality = compression === "low" ? 0.75 : compression === "medium" ? 0.60 : compression === "high" ? 0.50 : 0.85;
  const wantOcr = Boolean(ocrLang && ocrLang !== "none" && scribeModule);

  // Phase 1: Render all pages
  if (onStatus) onStatus("Rendering pages...");
  const renderedPages = await renderAllPages({
    pages,
    getPdfDocForPage,
    jpegQuality,
    compression,
    needOcrImage: wantOcr,
    onProgress,
    onStatus,
  });

  let ocrUsed = false;
  let finalPdfBytes;

  // Phase 2: OCR (if enabled)
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

      if (onStatus) onStatus("Finalizing PDF...");
      finalPdfBytes = await ocrPdfDoc.save({ useObjectStreams: false });
    } else {
      if (onStatus) onStatus("OCR failed, saving without OCR...");
    }
  }

  // Phase 3: Create PDF without OCR if needed
  if (!finalPdfBytes) {
    if (onStatus) onStatus("Creating PDF...");
    const outputPdf = await PDFDocument.create();

    for (let i = 0; i < renderedPages.length; i++) {
      if (onProgress) onProgress(i + 1, renderedPages.length);
      if (onStatus) onStatus(`Adding page ${i + 1}/${renderedPages.length}`);

      const rendered = renderedPages[i];
      const pdfPage = outputPdf.addPage([rendered.pageSizePts.width, rendered.pageSizePts.height]);
      await drawRenderedImage(outputPdf, pdfPage, rendered, {
        x: 0,
        y: 0,
        width: rendered.pageSizePts.width,
        height: rendered.pageSizePts.height,
      });

      await yieldToUi();
    }

    if (onStatus) onStatus("Finalizing PDF...");
    finalPdfBytes = await outputPdf.save({ useObjectStreams: false });
  }

  return { pdfBytes: finalPdfBytes, ocrUsed };
}
