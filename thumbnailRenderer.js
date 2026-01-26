/**
 * Thumbnail renderer for low-resolution page previews.
 *
 * Generates small thumbnails from PDF pages with operations applied.
 * Thumbnails are used for the page grid display and preview panel.
 */

import { applyModeToCanvas, removeShading, enhanceContrast } from "./imageColorModes.js";
import { OperationType, getEffectiveColorMode } from "./pageModel.js";

// Default thumbnail width in pixels
const THUMBNAIL_WIDTH = 300;

/**
 * Renders a PDF page to a thumbnail canvas
 * @param {Object} params
 * @param {Object} params.pdfDoc - PDF.js document
 * @param {number} params.pageIndex - Page index (0-based)
 * @param {number} params.maxWidth - Maximum thumbnail width
 * @returns {Promise<{canvas: HTMLCanvasElement, pageSizePts: {width: number, height: number}}>}
 */
export async function renderPdfPageThumbnail({ pdfDoc, pageIndex, maxWidth = THUMBNAIL_WIDTH }) {
  const page = await pdfDoc.getPage(pageIndex + 1); // PDF.js uses 1-based indexing
  const viewport = page.getViewport({ scale: 1 });

  // Calculate scale to fit within maxWidth
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

/**
 * Rotates a canvas by 90 degrees clockwise
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
 * Applies operations to a thumbnail canvas
 * @param {HTMLCanvasElement} canvas - Source canvas (will be modified or replaced)
 * @param {Array} operations - List of operations to apply
 * @returns {HTMLCanvasElement} - Result canvas (may be different from input)
 */
export function applyOperationsToThumbnail(canvas, operations) {
  let current = canvas;

  // First pass: apply geometric operations (rotate, split)
  for (const op of operations) {
    if (op.type === OperationType.ROTATE) {
      const times = ((op.degrees / 90) % 4 + 4) % 4;
      for (let i = 0; i < times; i++) {
        current = rotateCanvas90(current);
      }
    } else if (op.type === OperationType.SPLIT) {
      current = cropCanvasHalf(current, op.side);
    }
  }

  // Second pass: apply pixel operations
  // Find the last color mode operation
  const colorMode = getEffectiveColorMode(operations);
  if (colorMode !== "color") {
    current = applyModeToCanvas(colorMode, current);
  }

  // Apply shading removal if present
  const hasRemoveShading = operations.some(op => op.type === OperationType.REMOVE_SHADING);
  if (hasRemoveShading) {
    removeShading(current);
  }

  // Apply contrast enhancement if present
  const hasEnhanceContrast = operations.some(op => op.type === OperationType.ENHANCE_CONTRAST);
  if (hasEnhanceContrast) {
    enhanceContrast(current);
  }

  return current;
}

/**
 * Generates a thumbnail for a page with its operations applied
 * @param {Object} params
 * @param {Object} params.pdfDoc - PDF.js document
 * @param {Object} params.page - Page object from pageModel
 * @param {number} params.maxWidth - Maximum thumbnail width
 * @returns {Promise<HTMLCanvasElement>}
 */
export async function generateThumbnail({ pdfDoc, page, maxWidth = THUMBNAIL_WIDTH }) {
  const { canvas } = await renderPdfPageThumbnail({
    pdfDoc,
    pageIndex: page.sourcePageIndex,
    maxWidth,
  });

  return applyOperationsToThumbnail(canvas, page.operations);
}

/**
 * Updates a page's thumbnail after operations change
 * @param {Object} params
 * @param {Object} params.pdfDoc - PDF.js document
 * @param {Object} params.page - Page object (will be mutated)
 * @param {number} params.maxWidth - Maximum thumbnail width
 */
export async function updatePageThumbnail({ pdfDoc, page, maxWidth = THUMBNAIL_WIDTH }) {
  page.thumbnail = await generateThumbnail({ pdfDoc, page, maxWidth });
}

/**
 * Batch update thumbnails for multiple pages
 * @param {Object} params
 * @param {Object} params.pdfDoc - PDF.js document
 * @param {Array} params.pages - Array of page objects
 * @param {Function} params.onProgress - Progress callback (index, total)
 * @param {number} params.maxWidth - Maximum thumbnail width
 */
export async function updateThumbnailsBatch({ pdfDoc, pages, onProgress, maxWidth = THUMBNAIL_WIDTH }) {
  for (let i = 0; i < pages.length; i++) {
    await updatePageThumbnail({ pdfDoc, page: pages[i], maxWidth });
    if (onProgress) {
      onProgress(i + 1, pages.length);
    }
  }
}
