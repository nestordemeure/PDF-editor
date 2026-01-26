/**
 * Tools for page operations.
 *
 * These functions modify the operation lists on pages and update thumbnails.
 * The actual pixel operations are applied during save by the render worker.
 */

import {
  generatePageId,
  createRotateOp,
  createSplitOp,
  createColorModeOp,
  createRemoveShadingOp,
  createEnhanceContrastOp,
  cloneOperations,
} from "./pageModel.js";
import { updatePageThumbnail } from "./thumbnailRenderer.js";

/**
 * Rotates a thumbnail canvas 90 degrees clockwise
 */
function rotateThumbnail90(thumbnail) {
  if (!thumbnail) return null;

  const canvas = document.createElement("canvas");
  canvas.width = thumbnail.height;
  canvas.height = thumbnail.width;
  const ctx = canvas.getContext("2d");
  ctx.translate(canvas.width, 0);
  ctx.rotate(Math.PI / 2);
  ctx.drawImage(thumbnail, 0, 0);
  return canvas;
}

/**
 * Rotates selected pages by 90 degrees clockwise
 * @param {Object} params
 * @param {Array} params.pages - All pages
 * @param {Object} params.pdfDoc - PDF.js document (unused now, kept for API compatibility)
 * @param {Function} params.setProgress - Progress callback
 * @param {Function} params.setStatus - Status callback
 * @param {Function} params.yieldToUi - Yield to UI callback
 */
export async function rotateSelection({ pages, pdfDoc, setProgress, setStatus, yieldToUi }) {
  const selected = pages.filter(page => page.selected);

  for (let i = 0; i < selected.length; i++) {
    const page = selected[i];

    // Add rotate operation
    page.operations.push(createRotateOp(90));

    // Swap page size
    page.pageSizePts = {
      width: page.pageSizePts.height,
      height: page.pageSizePts.width,
    };

    // Rotate existing thumbnail instead of re-rendering
    page.thumbnail = rotateThumbnail90(page.thumbnail);

    setProgress(i + 1, selected.length);
    setStatus(`Rotating ${i + 1}/${selected.length}`);
    await yieldToUi();
  }
}

/**
 * Applies grayscale to a thumbnail canvas (in place)
 */
function applyGrayscaleToThumbnail(canvas) {
  if (!canvas) return;
  const ctx = canvas.getContext("2d");
  const imgData = ctx.getImageData(0, 0, canvas.width, canvas.height);
  const data = imgData.data;
  for (let i = 0; i < data.length; i += 4) {
    const gray = Math.round(0.299 * data[i] + 0.587 * data[i + 1] + 0.114 * data[i + 2]);
    data[i] = gray;
    data[i + 1] = gray;
    data[i + 2] = gray;
  }
  ctx.putImageData(imgData, 0, 0);
}

/**
 * Applies Otsu thresholding to a thumbnail (global threshold)
 */
function applyOtsuToThumbnail(canvas) {
  if (!canvas) return;
  const ctx = canvas.getContext("2d");
  const imgData = ctx.getImageData(0, 0, canvas.width, canvas.height);
  const data = imgData.data;

  // Build histogram
  const histogram = new Array(256).fill(0);
  for (let i = 0; i < data.length; i += 4) {
    const gray = Math.round(0.299 * data[i] + 0.587 * data[i + 1] + 0.114 * data[i + 2]);
    histogram[gray]++;
  }

  // Calculate Otsu threshold
  const total = data.length / 4;
  let sum = 0;
  for (let i = 0; i < 256; i++) sum += i * histogram[i];

  let sumB = 0, wB = 0, wF = 0;
  let maxVariance = 0, threshold = 128;

  for (let i = 0; i < 256; i++) {
    wB += histogram[i];
    if (wB === 0) continue;
    wF = total - wB;
    if (wF === 0) break;

    sumB += i * histogram[i];
    const mB = sumB / wB;
    const mF = (sum - sumB) / wF;
    const variance = wB * wF * (mB - mF) * (mB - mF);

    if (variance > maxVariance) {
      maxVariance = variance;
      threshold = i;
    }
  }

  // Apply threshold
  for (let i = 0; i < data.length; i += 4) {
    const gray = Math.round(0.299 * data[i] + 0.587 * data[i + 1] + 0.114 * data[i + 2]);
    const value = gray > threshold ? 255 : 0;
    data[i] = value;
    data[i + 1] = value;
    data[i + 2] = value;
  }

  ctx.putImageData(imgData, 0, 0);
}

/**
 * Applies adaptive thresholding to a thumbnail (local threshold)
 */
function applyAdaptiveToThumbnail(canvas, page) {
  if (!canvas) return;

  // Scale block size based on thumbnail resolution
  const fullResWidth = (page.pageSizePts.width / 72) * 300;
  const scale = canvas.width / fullResWidth;

  // Block size at full res is ~31, scale it down (minimum 3, must be odd)
  let blockSize = Math.round(31 * scale);
  blockSize = Math.max(3, blockSize);
  if (blockSize % 2 === 0) blockSize++;

  const C = Math.max(2, Math.round(10 * scale)); // Threshold offset

  const ctx = canvas.getContext("2d");
  const imgData = ctx.getImageData(0, 0, canvas.width, canvas.height);
  const data = imgData.data;
  const width = canvas.width;
  const height = canvas.height;

  // Convert to grayscale array
  const gray = new Uint8Array(width * height);
  for (let i = 0; i < gray.length; i++) {
    const idx = i * 4;
    gray[i] = Math.round(0.299 * data[idx] + 0.587 * data[idx + 1] + 0.114 * data[idx + 2]);
  }

  // Calculate integral image for fast local mean
  const integral = new Float64Array((width + 1) * (height + 1));
  for (let y = 0; y < height; y++) {
    let rowSum = 0;
    for (let x = 0; x < width; x++) {
      rowSum += gray[y * width + x];
      integral[(y + 1) * (width + 1) + (x + 1)] = integral[y * (width + 1) + (x + 1)] + rowSum;
    }
  }

  // Apply adaptive threshold
  const halfBlock = Math.floor(blockSize / 2);
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const x1 = Math.max(0, x - halfBlock);
      const y1 = Math.max(0, y - halfBlock);
      const x2 = Math.min(width, x + halfBlock + 1);
      const y2 = Math.min(height, y + halfBlock + 1);

      const count = (x2 - x1) * (y2 - y1);
      const sum = integral[y2 * (width + 1) + x2]
                - integral[y1 * (width + 1) + x2]
                - integral[y2 * (width + 1) + x1]
                + integral[y1 * (width + 1) + x1];

      const mean = sum / count;
      const value = gray[y * width + x] > mean - C ? 255 : 0;

      const idx = (y * width + x) * 4;
      data[idx] = value;
      data[idx + 1] = value;
      data[idx + 2] = value;
    }
  }

  ctx.putImageData(imgData, 0, 0);
}

/**
 * Applies a color mode to selected pages
 * @param {Object} params
 * @param {Array} params.pages - All pages
 * @param {string} params.mode - Color mode ('color', 'gray', 'bw', 'bw-otsu')
 * @param {Object} params.pdfDoc - PDF.js document for thumbnail updates
 * @param {Function} params.setProgress - Progress callback
 * @param {Function} params.setStatus - Status callback
 * @param {Function} params.yieldToUi - Yield to UI callback
 */
export async function applyColorModeToSelection({ pages, mode, pdfDoc, setProgress, setStatus, yieldToUi }) {
  const selected = pages.filter(page => page.selected);

  for (let i = 0; i < selected.length; i++) {
    const page = selected[i];

    // Remove any existing color mode operations and add new one
    page.operations = page.operations.filter(op => op.type !== "colorMode");
    if (mode !== "color") {
      page.operations.push(createColorModeOp(mode));
    }

    if (mode === "color") {
      // Restore original colors - need to re-render from PDF
      await updatePageThumbnail({ pdfDoc, page });
    } else if (mode === "gray") {
      // Apply grayscale directly to thumbnail
      applyGrayscaleToThumbnail(page.thumbnail);
    } else if (mode === "bw-otsu") {
      // Apply Otsu threshold directly to thumbnail
      applyOtsuToThumbnail(page.thumbnail);
    } else if (mode === "bw") {
      // Apply adaptive threshold directly to thumbnail
      applyAdaptiveToThumbnail(page.thumbnail, page);
    }

    setProgress(i + 1, selected.length);
    setStatus(`Applying color mode ${i + 1}/${selected.length}`);
    await yieldToUi();
  }
}

/**
 * Splits a thumbnail canvas into left or right half
 */
function splitThumbnail(thumbnail, side) {
  if (!thumbnail) return null;

  const mid = Math.floor(thumbnail.width / 2);
  const canvas = document.createElement("canvas");

  if (side === "left") {
    canvas.width = mid;
    canvas.height = thumbnail.height;
    canvas.getContext("2d").drawImage(thumbnail, 0, 0, mid, thumbnail.height, 0, 0, mid, thumbnail.height);
  } else {
    canvas.width = thumbnail.width - mid;
    canvas.height = thumbnail.height;
    canvas.getContext("2d").drawImage(thumbnail, mid, 0, thumbnail.width - mid, thumbnail.height, 0, 0, thumbnail.width - mid, thumbnail.height);
  }

  return canvas;
}

/**
 * Splits selected pages into left and right halves
 * @param {Object} params
 * @param {Array} params.pages - All pages
 * @param {Object} params.pdfDoc - PDF.js document (unused now, kept for API compatibility)
 * @param {Function} params.setProgress - Progress callback
 * @param {Function} params.setStatus - Status callback
 * @param {Function} params.yieldToUi - Yield to UI callback
 * @returns {Promise<Array>} New pages array with splits applied
 */
export async function splitSelection({ pages, pdfDoc, setProgress, setStatus, yieldToUi }) {
  const nextPages = [];
  const selectedCount = pages.filter(p => p.selected).length;
  let processed = 0;

  for (let i = 0; i < pages.length; i++) {
    const page = pages[i];

    if (!page.selected) {
      nextPages.push(page);
    } else {
      // Create left half page - reuse existing thumbnail
      const leftPage = {
        id: generatePageId(),
        sourcePageIndex: page.sourcePageIndex,
        pageSizePts: { width: page.pageSizePts.width / 2, height: page.pageSizePts.height },
        operations: [...cloneOperations(page.operations), createSplitOp("left")],
        thumbnail: splitThumbnail(page.thumbnail, "left"),
        selected: false,
      };

      // Create right half page - reuse existing thumbnail
      const rightPage = {
        id: generatePageId(),
        sourcePageIndex: page.sourcePageIndex,
        pageSizePts: { width: page.pageSizePts.width / 2, height: page.pageSizePts.height },
        operations: [...cloneOperations(page.operations), createSplitOp("right")],
        thumbnail: splitThumbnail(page.thumbnail, "right"),
        selected: false,
      };

      nextPages.push(leftPage);
      nextPages.push(rightPage);

      processed++;
      setProgress(processed, selectedCount);
      setStatus(`Splitting ${processed}/${selectedCount}`);
      await yieldToUi();
    }
  }

  return nextPages;
}

/**
 * Deletes selected pages
 * @param {Object} params
 * @param {Array} params.pages - All pages
 * @param {Function} params.setProgress - Progress callback
 * @param {Function} params.setStatus - Status callback
 * @param {Function} params.yieldToUi - Yield to UI callback
 * @returns {Promise<Array>} New pages array without deleted pages
 */
export async function deleteSelection({ pages, setProgress, setStatus, yieldToUi }) {
  const selected = pages.filter(p => p.selected);
  const nextPages = pages.filter(p => !p.selected);

  setProgress(selected.length, selected.length);
  setStatus(`Deleted ${selected.length} page${selected.length === 1 ? "" : "s"}`);
  await yieldToUi();

  return nextPages;
}

/**
 * Applies remove shading (high-pass filter) directly to a thumbnail
 * Uses scaled blur radius for thumbnail resolution
 */
function removeShadingFromThumbnail(canvas, page) {
  if (!canvas) return;

  // Estimate scale factor: thumbnail width vs full-res width (at 300 DPI)
  const fullResWidth = (page.pageSizePts.width / 72) * 300;
  const scale = canvas.width / fullResWidth;

  // Scale blur radius (20px at full-res)
  const blurRadius = Math.max(2, Math.round(20 * scale));
  const strength = 1.2;

  const ctx = canvas.getContext("2d");
  const width = canvas.width;
  const height = canvas.height;

  const originalData = ctx.getImageData(0, 0, width, height);

  // Create blurred version
  const tempCanvas = document.createElement("canvas");
  tempCanvas.width = width;
  tempCanvas.height = height;
  const tempCtx = tempCanvas.getContext("2d");
  tempCtx.filter = `blur(${blurRadius}px)`;
  tempCtx.drawImage(canvas, 0, 0);

  const blurredData = tempCtx.getImageData(0, 0, width, height);

  // High-pass filter
  for (let i = 0; i < originalData.data.length; i += 4) {
    const grayOrig = 0.299 * originalData.data[i] + 0.587 * originalData.data[i + 1] + 0.114 * originalData.data[i + 2];
    const grayBlur = 0.299 * blurredData.data[i] + 0.587 * blurredData.data[i + 1] + 0.114 * blurredData.data[i + 2];
    const diff = (grayOrig - grayBlur) * strength;
    const value = Math.max(0, Math.min(255, 255 + diff));
    originalData.data[i] = value;
    originalData.data[i + 1] = value;
    originalData.data[i + 2] = value;
  }

  ctx.putImageData(originalData, 0, 0);
}

/**
 * Removes shading from selected pages
 * @param {Object} params
 * @param {Array} params.pages - All pages
 * @param {Object} params.pdfDoc - PDF.js document (unused, kept for API compatibility)
 * @param {Function} params.setProgress - Progress callback
 * @param {Function} params.setStatus - Status callback
 * @param {Function} params.yieldToUi - Yield to UI callback
 */
export async function removeShadingSelection({ pages, pdfDoc, setProgress, setStatus, yieldToUi }) {
  const selected = pages.filter(page => page.selected);

  for (let i = 0; i < selected.length; i++) {
    const page = selected[i];

    // Add remove shading operation (if not already present)
    if (!page.operations.some(op => op.type === "removeShading")) {
      page.operations.push(createRemoveShadingOp());

      // Apply directly to thumbnail
      removeShadingFromThumbnail(page.thumbnail, page);
    }

    setProgress(i + 1, selected.length);
    setStatus(`Removing shading ${i + 1}/${selected.length}`);
    await yieldToUi();
  }
}

/**
 * Applies min-max contrast enhancement directly to a thumbnail
 */
function enhanceContrastOnThumbnail(canvas) {
  if (!canvas) return;

  const ctx = canvas.getContext("2d");
  const imgData = ctx.getImageData(0, 0, canvas.width, canvas.height);
  const data = imgData.data;

  // Find min and max
  let min = 255;
  let max = 0;
  for (let i = 0; i < data.length; i += 4) {
    const gray = 0.299 * data[i] + 0.587 * data[i + 1] + 0.114 * data[i + 2];
    if (gray < min) min = gray;
    if (gray > max) max = gray;
  }

  // Apply normalization
  const range = max - min;
  if (range > 0) {
    for (let i = 0; i < data.length; i += 4) {
      const gray = 0.299 * data[i] + 0.587 * data[i + 1] + 0.114 * data[i + 2];
      const normalized = ((gray - min) / range) * 255;
      data[i] = normalized;
      data[i + 1] = normalized;
      data[i + 2] = normalized;
    }
    ctx.putImageData(imgData, 0, 0);
  }
}

/**
 * Enhances contrast on selected pages
 * @param {Object} params
 * @param {Array} params.pages - All pages
 * @param {Object} params.pdfDoc - PDF.js document (unused, kept for API compatibility)
 * @param {Function} params.setProgress - Progress callback
 * @param {Function} params.setStatus - Status callback
 * @param {Function} params.yieldToUi - Yield to UI callback
 */
export async function enhanceContrastSelection({ pages, pdfDoc, setProgress, setStatus, yieldToUi }) {
  const selected = pages.filter(page => page.selected);

  for (let i = 0; i < selected.length; i++) {
    const page = selected[i];

    // Add enhance contrast operation (if not already present)
    if (!page.operations.some(op => op.type === "enhanceContrast")) {
      page.operations.push(createEnhanceContrastOp());

      // Apply directly to thumbnail
      enhanceContrastOnThumbnail(page.thumbnail);
    }

    setProgress(i + 1, selected.length);
    setStatus(`Enhancing contrast ${i + 1}/${selected.length}`);
    await yieldToUi();
  }
}
