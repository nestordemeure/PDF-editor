/**
 * PDF.js rendering: page thumbnails, and full renders for the engine.
 *
 * Thumbnails go through the engine's preview, the same view and pixel
 * operations as the save, so previews match the output.
 */

import { previewPage } from "./engine.js";

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
  // PDF.js keeps the decoded images of a page until told otherwise: a
  // scan is tens of megabytes.
  page.cleanup();

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
 * Applies a page's view and pixel operations to a render of its source
 * page. Consumes the input canvas; returns a new one.
 */
export async function applyOperationsToCanvas(canvas, page, record) {
  const imageData = canvas.getContext("2d", { willReadFrequently: true }).getImageData(0, 0, canvas.width, canvas.height);
  releaseCanvas(canvas);
  const result = await previewPage(imageData, page, record);
  const out = document.createElement("canvas");
  out.width = result.width;
  out.height = result.height;
  out.getContext("2d").putImageData(result, 0, 0);
  return out;
}

/**
 * Updates a page's thumbnail after its operations change (replaces
 * page.thumbnail). `record` is the page's source record.
 */
export async function updatePageThumbnail({ record, page, maxWidth = THUMBNAIL_WIDTH }) {
  const { canvas } = await getBasePageCanvas({
    pdfDoc: record.pdfDoc,
    sourceId: page.sourceId,
    pageIndex: page.sourcePageIndex,
    maxWidth,
  });
  page.thumbnail = await applyOperationsToCanvas(canvas, page, record);
}

/**
 * Renders a source page (display space) at `pixelsPerPoint`, as the engine
 * asks for pages without a decodable scan image.
 * @returns {Promise<{rgba: Uint8Array, width: number, height: number}>}
 */
export async function renderPageRgba(pdfDoc, pageIndex, pixelsPerPoint) {
  const page = await pdfDoc.getPage(pageIndex + 1);
  const viewport = page.getViewport({ scale: pixelsPerPoint });
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(viewport.width));
  canvas.height = Math.max(1, Math.round(viewport.height));
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  await page.render({ canvasContext: ctx, viewport }).promise;
  page.cleanup();
  const { data, width, height } = ctx.getImageData(0, 0, canvas.width, canvas.height);
  releaseCanvas(canvas);
  return { rgba: new Uint8Array(data.buffer, data.byteOffset, data.byteLength), width, height };
}
