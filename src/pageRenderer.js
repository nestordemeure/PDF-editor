/**
 * Page rendering pipeline, shared by thumbnails and the exporter.
 *
 * applyOperationsToCanvas is the single canonical pipeline:
 * geometric ops (rotate/split) -> remove shading -> enhance contrast -> color mode.
 * Binarization runs last so shading removal / contrast work on real gray values.
 * Because thumbnails and the save path use the same pipeline, previews match output.
 */

import { applyPixelPipeline, needsPixelWork } from "./imagePixelOps.js";
import { OperationType } from "./pageModel.js";

// Default thumbnail width in pixels
const THUMBNAIL_WIDTH = 300;

// Base (original colors, no operations) page renders, cached as small JPEG
// blobs so pixel-effect changes rebuild thumbnails without touching PDF.js.
// Keyed by sourceId:pageIndex, so split halves share one entry.
const baseThumbnailCache = new Map();

export function clearBaseThumbnailCache() {
  baseThumbnailCache.clear();
}

/**
 * Returns the base render of a source page (original colors, no operations),
 * from cache when possible. The returned canvas is owned by the caller.
 * @returns {Promise<{canvas: HTMLCanvasElement, pageSizePts: {width: number, height: number}}>}
 */
export async function getBasePageCanvas({ pdfDoc, sourceId, pageIndex, maxWidth = THUMBNAIL_WIDTH }) {
  const key = `${sourceId}:${pageIndex}`;
  const cached = baseThumbnailCache.get(key);
  if (cached) {
    const bitmap = await createImageBitmap(cached.blob);
    const canvas = document.createElement("canvas");
    canvas.width = bitmap.width;
    canvas.height = bitmap.height;
    canvas.getContext("2d").drawImage(bitmap, 0, 0);
    bitmap.close();
    return { canvas, pageSizePts: cached.pageSizePts };
  }

  const { canvas, pageSizePts } = await renderPdfPageThumbnail({ pdfDoc, pageIndex, maxWidth });
  const blob = await new Promise(resolve => canvas.toBlob(resolve, "image/jpeg", 0.9));
  if (blob) baseThumbnailCache.set(key, { blob, pageSizePts });
  return { canvas, pageSizePts };
}

/**
 * True when the page is "classic" (typeset text, worth preserving as-is).
 *
 * Pages without extractable text are scans. Pages *with* text can still be
 * scans: OCR tools add an invisible text layer over the page image. Those
 * are detected by measuring how much of the page area is covered by painted
 * images — a page that is essentially one full-page image is a scan and
 * should go through the raster pipeline (compression, cleanup, re-OCR).
 */
export async function detectClassicPage(pdfDoc, pageIndex) {
  const page = await pdfDoc.getPage(pageIndex + 1);
  const textContent = await page.getTextContent();
  const hasText = textContent.items.some(item => item.str && item.str.trim().length > 0);
  if (!hasText) return false;

  // Sum the page-area fraction painted with images. Images are drawn as a
  // unit square through the CTM, so each one's area is |det(CTM)|; the
  // determinant composes multiplicatively, which lets us track a single
  // scalar through save/restore/transform instead of full matrices.
  const OPS = window.pdfjsLib.OPS;
  const opList = await page.getOperatorList();
  const [x1, y1, x2, y2] = page.view;
  const pageArea = Math.abs((x2 - x1) * (y2 - y1));

  let det = 1;
  const stack = [];
  let imageArea = 0;
  for (let i = 0; i < opList.fnArray.length; i++) {
    const fn = opList.fnArray[i];
    if (fn === OPS.save) {
      stack.push(det);
    } else if (fn === OPS.restore) {
      if (stack.length) det = stack.pop();
    } else if (fn === OPS.transform) {
      const [a, b, c, d] = opList.argsArray[i];
      det *= a * d - b * c;
    } else if (
      fn === OPS.paintImageXObject ||
      fn === OPS.paintInlineImageXObject ||
      fn === OPS.paintImageMaskXObject
    ) {
      imageArea += Math.abs(det);
    }
  }
  return imageArea < 0.9 * pageArea;
}

/**
 * Renders a PDF page to a canvas sized to fit maxWidth
 * @returns {Promise<{canvas: HTMLCanvasElement, pageSizePts: {width: number, height: number}}>}
 */
async function renderPdfPageThumbnail({ pdfDoc, pageIndex, maxWidth = THUMBNAIL_WIDTH }) {
  const page = await pdfDoc.getPage(pageIndex + 1); // PDF.js uses 1-based indexing
  const viewport = page.getViewport({ scale: 1 });

  const scale = maxWidth / viewport.width;
  const scaledViewport = page.getViewport({ scale });

  const canvas = document.createElement("canvas");
  canvas.width = Math.round(scaledViewport.width);
  canvas.height = Math.round(scaledViewport.height);

  const ctx = canvas.getContext("2d");
  await page.render({ canvasContext: ctx, viewport: scaledViewport }).promise;

  return {
    canvas,
    pageSizePts: { width: viewport.width, height: viewport.height },
  };
}

function releaseCanvas(canvas) {
  canvas.width = 0;
  canvas.height = 0;
}

/**
 * Rotates a canvas by 90 degrees clockwise
 */
export function rotateCanvas90(canvas) {
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
export function cropCanvasHalf(canvas, side) {
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
 * Applies a page's operations to a canvas. Consumes the input canvas
 * (intermediates are released); returns the resulting canvas.
 * @param {HTMLCanvasElement} canvas - Source canvas (treated as owned/throwaway)
 * @param {Array} operations - Operations to apply
 * @param {Object} [options]
 * @param {number} [options.shadingScale] - Resolution scale for shading removal
 *   (canvas resolution relative to a 300 DPI render of the page)
 * @returns {HTMLCanvasElement}
 */
export function applyOperationsToCanvas(canvas, operations, { shadingScale = 1 } = {}) {
  const current = applyGeometricOpsToCanvas(canvas, operations);

  if (needsPixelWork(operations)) {
    const ctx = current.getContext("2d", { willReadFrequently: true });
    const imageData = ctx.getImageData(0, 0, current.width, current.height);
    applyPixelPipeline(imageData, operations, { shadingScale });
    ctx.putImageData(imageData, 0, 0);
  }

  return current;
}

/**
 * Applies only the geometric operations (rotate/split), in the order they
 * were applied. Consumes the input canvas.
 */
export function applyGeometricOpsToCanvas(canvas, operations) {
  let current = canvas;

  for (const op of operations) {
    if (op.type === OperationType.ROTATE) {
      const times = ((op.degrees / 90) % 4 + 4) % 4;
      for (let i = 0; i < times; i++) {
        const rotated = rotateCanvas90(current);
        releaseCanvas(current);
        current = rotated;
      }
    } else if (op.type === OperationType.SPLIT) {
      const cropped = cropCanvasHalf(current, op.side);
      releaseCanvas(current);
      current = cropped;
    }
  }

  return current;
}

/**
 * Generates a thumbnail for a page with its operations applied
 */
async function generateThumbnail({ pdfDoc, page, maxWidth = THUMBNAIL_WIDTH }) {
  const { canvas, pageSizePts } = await getBasePageCanvas({
    pdfDoc,
    sourceId: page.sourceId,
    pageIndex: page.sourcePageIndex,
    maxWidth,
  });

  // Thumbnail resolution relative to a 300 DPI render of the source page
  const shadingScale = canvas.width / ((pageSizePts.width / 72) * 300);
  return applyOperationsToCanvas(canvas, page.operations, { shadingScale });
}

/**
 * Updates a page's thumbnail after operations change (mutates page.thumbnail)
 */
export async function updatePageThumbnail({ pdfDoc, page, maxWidth = THUMBNAIL_WIDTH }) {
  page.thumbnail = await generateThumbnail({ pdfDoc, page, maxWidth });
}
