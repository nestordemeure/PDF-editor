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

import { applyOperationsToCanvas } from "./thumbnailRenderer.js";

// Target DPI for compression levels
const TARGET_DPI = {
  color: { none: 300, low: 180, medium: 150, high: 120 },
  gray: { none: 300, low: 200, medium: 150, high: 120 },
  bw: { none: 300, low: 260, medium: 200, high: 150 },
};

/**
 * Gets the effective color mode from operations
 */
function getColorMode(operations) {
  for (let i = operations.length - 1; i >= 0; i--) {
    if (operations[i].type === "colorMode") {
      return operations[i].mode;
    }
  }
  return "color";
}

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

function canvasToBlobBytes(canvas, mimeType, quality) {
  return new Promise((resolve, reject) => {
    canvas.toBlob(blob => {
      if (!blob) {
        reject(new Error(`Canvas encoding to ${mimeType} failed.`));
        return;
      }
      blob.arrayBuffer().then(buffer => resolve(new Uint8Array(buffer)), reject);
    }, mimeType, quality);
  });
}

/**
 * Packs a binarized canvas into 1 bit per pixel (rows byte-aligned, 1 = white)
 */
function packCanvasTo1Bit(canvas) {
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  const data = ctx.getImageData(0, 0, canvas.width, canvas.height).data;

  const rowBytes = Math.ceil(canvas.width / 8);
  const packed = new Uint8Array(rowBytes * canvas.height);
  for (let y = 0; y < canvas.height; y++) {
    const rowOffset = y * rowBytes;
    for (let x = 0; x < canvas.width; x++) {
      if (data[(y * canvas.width + x) * 4] > 127) {
        packed[rowOffset + (x >> 3)] |= 0x80 >> (x & 7);
      }
    }
  }
  return packed;
}

/**
 * Extracts the 8-bit grayscale channel from a canvas
 */
function extractGray8(canvas) {
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  const data = ctx.getImageData(0, 0, canvas.width, canvas.height).data;
  const gray = new Uint8Array(canvas.width * canvas.height);
  for (let i = 0; i < gray.length; i++) {
    const idx = i * 4;
    gray[i] = (0.299 * data[idx] + 0.587 * data[idx + 1] + 0.114 * data[idx + 2]) | 0;
  }
  return gray;
}

/**
 * Encodes a processed canvas for embedding.
 * Returns { kind, ... } where kind is:
 * - "raw-gray": raw DeviceGray samples (bitsPerComponent 1 or 8) in `raw`
 * - "png" / "jpeg": encoded bytes in `bytes`
 * `ocrBytes`/`ocrMime` hold an encoded image for the OCR engine when requested.
 */
async function encodeCanvas(canvas, { compression, jpegQuality, colorMode, needOcrImage }) {
  const width = canvas.width;
  const height = canvas.height;

  if (isBwMode(colorMode)) {
    const packed = packCanvasTo1Bit(canvas);
    let ocrBytes = null;
    if (needOcrImage) {
      // 1-bit grayscale PNG via UPNG (tiny); fall back to canvas PNG
      if (typeof UPNG !== "undefined" && typeof UPNG.encodeLL === "function") {
        try {
          ocrBytes = new Uint8Array(UPNG.encodeLL([packed.buffer], width, height, 1, 0, 1));
        } catch (e) {
          ocrBytes = null;
        }
      }
      if (!ocrBytes) ocrBytes = await canvasToBlobBytes(canvas, "image/png");
    }
    return { kind: "raw-gray", bitsPerComponent: 1, raw: packed, width, height, ocrBytes, ocrMime: "image/png" };
  }

  if (compression === "none") {
    if (colorMode === "gray") {
      const raw = extractGray8(canvas);
      const ocrBytes = needOcrImage ? await canvasToBlobBytes(canvas, "image/png") : null;
      return { kind: "raw-gray", bitsPerComponent: 8, raw, width, height, ocrBytes, ocrMime: "image/png" };
    }
    const bytes = await canvasToBlobBytes(canvas, "image/png");
    return { kind: "png", bytes, width, height, ocrBytes: bytes, ocrMime: "image/png" };
  }

  const bytes = await canvasToBlobBytes(canvas, "image/jpeg", jpegQuality);
  return { kind: "jpeg", bytes, width, height, ocrBytes: bytes, ocrMime: "image/jpeg" };
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
 * Renders all pages through the shared pipeline and encodes them
 */
export async function renderAllPages({ pages, getPdfDocForPage, jpegQuality = 0.85, compression = "high", needOcrImage = false, onProgress, onStatus }) {
  const results = [];

  for (let i = 0; i < pages.length; i++) {
    const page = pages[i];
    const pdfDoc = getPdfDocForPage ? await getPdfDocForPage(page) : null;
    if (!pdfDoc) {
      throw new Error("Missing PDF source for page rendering.");
    }

    if (onStatus) onStatus(`Rendering page ${i + 1}/${pages.length}`);
    if (onProgress) onProgress(i + 1, pages.length);

    const colorMode = getColorMode(page.operations);
    const targetDpi = getTargetDpi(colorMode, compression);

    // Render at target DPI (already downscaled for compression), then apply
    // operations with the same pipeline the thumbnails use
    let canvas = await renderPdfPage(pdfDoc, page.sourcePageIndex, targetDpi);
    canvas = applyOperationsToCanvas(canvas, page.operations, { shadingScale: targetDpi / 300 });

    const rendered = await encodeCanvas(canvas, { compression, jpegQuality, colorMode, needOcrImage });
    rendered.pageSizePts = { ...page.pageSizePts };
    results.push(rendered);

    releaseCanvas(canvas);
    await yieldToUi();
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
