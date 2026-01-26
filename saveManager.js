/**
 * Save manager for orchestrating the PDF save process.
 *
 * Processes pages in batches to control memory usage.
 * PDF rendering happens in main thread (where PDF.js works),
 * but we process and release pages in batches to avoid memory issues.
 */

import { applyModeToCanvas, removeShading, enhanceContrast } from "./imageColorModes.js";

// Batch size for memory control
const BATCH_SIZE = 4;

/**
 * Renders a PDF page to canvas at full resolution
 */
async function renderPdfPageFullRes(pdfDoc, pageIndex) {
  const page = await pdfDoc.getPage(pageIndex + 1);
  const viewport = page.getViewport({ scale: 1 });

  // Render at 300 DPI
  const scale = 300 / 72;
  const scaledViewport = page.getViewport({ scale });

  const canvas = document.createElement("canvas");
  canvas.width = Math.round(scaledViewport.width);
  canvas.height = Math.round(scaledViewport.height);

  const ctx = canvas.getContext("2d");
  await page.render({ canvasContext: ctx, viewport: scaledViewport }).promise;

  return canvas;
}

/**
 * Rotates a canvas 90 degrees clockwise
 */
function rotateCanvas90(canvas) {
  const rotated = document.createElement("canvas");
  rotated.width = canvas.height;
  rotated.height = canvas.width;
  const ctx = rotated.getContext("2d");
  ctx.translate(rotated.width, 0);
  ctx.rotate(Math.PI / 2);
  ctx.drawImage(canvas, 0, 0);
  return rotated;
}

/**
 * Crops a canvas to left or right half
 */
function cropCanvasHalf(canvas, side) {
  const mid = Math.floor(canvas.width / 2);
  const cropped = document.createElement("canvas");

  if (side === "left") {
    cropped.width = mid;
    cropped.height = canvas.height;
    cropped.getContext("2d").drawImage(canvas, 0, 0, mid, canvas.height, 0, 0, mid, canvas.height);
  } else {
    cropped.width = canvas.width - mid;
    cropped.height = canvas.height;
    cropped.getContext("2d").drawImage(canvas, mid, 0, canvas.width - mid, canvas.height, 0, 0, canvas.width - mid, canvas.height);
  }

  return cropped;
}

/**
 * Applies all operations to a full-resolution canvas
 */
function applyOperationsToCanvas(canvas, operations) {
  let current = canvas;

  for (const op of operations) {
    if (op.type === "rotate") {
      const times = ((op.degrees / 90) % 4 + 4) % 4;
      for (let i = 0; i < times; i++) {
        const rotated = rotateCanvas90(current);
        if (current !== canvas) {
          current.width = 0;
          current.height = 0;
        }
        current = rotated;
      }
    } else if (op.type === "split") {
      const cropped = cropCanvasHalf(current, op.side);
      if (current !== canvas) {
        current.width = 0;
        current.height = 0;
      }
      current = cropped;
    }
  }

  // Apply color mode
  let colorMode = "color";
  for (let i = operations.length - 1; i >= 0; i--) {
    if (operations[i].type === "colorMode") {
      colorMode = operations[i].mode;
      break;
    }
  }

  if (colorMode !== "color") {
    current = applyModeToCanvas(colorMode, current);
  }

  // Apply shading removal
  if (operations.some(op => op.type === "removeShading")) {
    removeShading(current);
  }

  // Apply contrast enhancement
  if (operations.some(op => op.type === "enhanceContrast")) {
    enhanceContrast(current);
  }

  return current;
}

/**
 * Converts canvas to image bytes
 */
async function canvasToImageBytes(canvas, format, quality) {
  return new Promise((resolve) => {
    const mimeType = format === "jpeg" ? "image/jpeg" : "image/png";
    canvas.toBlob(async (blob) => {
      const arrayBuffer = await blob.arrayBuffer();
      resolve({
        bytes: new Uint8Array(arrayBuffer),
        mimeType,
      });
    }, mimeType, format === "jpeg" ? quality : undefined);
  });
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
  return new Promise(resolve => requestAnimationFrame(() => resolve()));
}

/**
 * Renders all pages in batches and returns image data
 */
export async function renderAllPages({ pdfDoc, pages, outputFormat = "png", jpegQuality = 0.85, onProgress, onStatus }) {
  const results = [];

  for (let i = 0; i < pages.length; i++) {
    const page = pages[i];

    if (onStatus) onStatus(`Rendering page ${i + 1}/${pages.length}`);
    if (onProgress) onProgress(i + 1, pages.length);

    // Render from PDF
    let canvas = await renderPdfPageFullRes(pdfDoc, page.sourcePageIndex);

    // Apply operations
    canvas = applyOperationsToCanvas(canvas, page.operations);

    // Convert to image bytes
    const { bytes, mimeType } = await canvasToImageBytes(canvas, outputFormat, jpegQuality);

    // page.pageSizePts is already adjusted for operations (split, rotate) in tools.js
    results.push({
      bytes,
      mimeType,
      width: canvas.width,
      height: canvas.height,
      pageSizePts: { ...page.pageSizePts },
    });

    // Release memory
    releaseCanvas(canvas);

    // Yield to UI periodically
    if (i % BATCH_SIZE === 0) {
      await yieldToUi();
    }
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

    scribeModule.opt.progressHandler = (message) => {
      if (!message || typeof message.n !== "number") return;
      const stage = message.type || "ocr";
      const stageIndex = Math.max(0, stageOrder.indexOf(stage));
      const stepInStage = Math.min(message.n + 1, renderedPages.length);
      const overallStep = Math.min(stageIndex * renderedPages.length + stepInStage, totalSteps);

      if (onProgress) onProgress(overallStep, totalSteps);

      let stageMessage = "Processing...";
      if (stage === "importImage") stageMessage = `OCR: loading images ${stepInStage}/${renderedPages.length}`;
      if (stage === "convert") stageMessage = `OCR: recognizing ${stepInStage}/${renderedPages.length}`;
      if (stage === "export") stageMessage = `OCR: generating PDF ${stepInStage}/${renderedPages.length}`;
      if (onStatus) onStatus(stageMessage);
    };

    // Convert to File objects for scribe.js
    const imageFiles = renderedPages.map((page, index) => {
      const blob = new Blob([page.bytes], { type: page.mimeType });
      return new File([blob], `page_${String(index + 1).padStart(4, "0")}.png`, { type: page.mimeType });
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
export async function savePdf({ pdfBytes, pages, options, onProgress, onStatus }) {
  const { compression, ocrLang, scribeModule, PDFDocument } = options;

  // Get PDF.js document for rendering
  const pdfjsLib = window["pdfjs-dist/build/pdf"];
  const pdfDoc = await pdfjsLib.getDocument({ data: pdfBytes }).promise;

  // Determine output format
  const outputFormat = compression === "none" ? "png" : "jpeg";
  const jpegQuality = compression === "low" ? 0.75 : compression === "medium" ? 0.60 : compression === "high" ? 0.50 : 0.85;

  // Phase 1: Render all pages
  if (onStatus) onStatus("Rendering pages...");
  const renderedPages = await renderAllPages({
    pdfDoc,
    pages,
    outputFormat,
    jpegQuality,
    onProgress,
    onStatus,
  });

  let ocrUsed = false;
  let finalPdfBytes;

  // Phase 2: OCR (if enabled)
  if (ocrLang && ocrLang !== "none" && scribeModule) {
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
        const pdfPage = ocrPdfDoc.getPage(i);
        pdfPage.setSize(rendered.pageSizePts.width, rendered.pageSizePts.height);

        const image = rendered.mimeType === "image/jpeg"
          ? await ocrPdfDoc.embedJpg(rendered.bytes)
          : await ocrPdfDoc.embedPng(rendered.bytes);

        pdfPage.drawImage(image, {
          x: 0,
          y: 0,
          width: rendered.pageSizePts.width,
          height: rendered.pageSizePts.height,
        });

        if (i % BATCH_SIZE === 0) await yieldToUi();
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

      const image = rendered.mimeType === "image/jpeg"
        ? await outputPdf.embedJpg(rendered.bytes)
        : await outputPdf.embedPng(rendered.bytes);

      const pdfPage = outputPdf.addPage([rendered.pageSizePts.width, rendered.pageSizePts.height]);
      pdfPage.drawImage(image, {
        x: 0,
        y: 0,
        width: rendered.pageSizePts.width,
        height: rendered.pageSizePts.height,
      });

      if (i % BATCH_SIZE === 0) await yieldToUi();
    }

    if (onStatus) onStatus("Finalizing PDF...");
    finalPdfBytes = await outputPdf.save({ useObjectStreams: false });
  }

  return { pdfBytes: finalPdfBytes, ocrUsed };
}

// No worker pool needed anymore
export function terminatePool() {
  // No-op for compatibility
}
