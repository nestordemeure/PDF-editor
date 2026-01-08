import { binarizeCanvasAdaptive, binarizeCanvasOtsu } from "./imageBinarization.js";

function getCanvasContext(canvas, readFrequently = false) {
  return canvas.getContext("2d", readFrequently ? { willReadFrequently: true } : undefined);
}

function applyGrayscale(canvas) {
  const ctx = getCanvasContext(canvas, true);
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

function applyPosterize(canvas, levels) {
  const ctx = getCanvasContext(canvas, true);
  const imgData = ctx.getImageData(0, 0, canvas.width, canvas.height);
  const data = imgData.data;
  const step = 255 / (levels - 1);
  for (let i = 0; i < data.length; i += 4) {
    const gray = Math.round(0.299 * data[i] + 0.587 * data[i + 1] + 0.114 * data[i + 2]);
    const bucket = Math.round(gray / step) * step;
    const value = Math.max(0, Math.min(255, bucket));
    data[i] = value;
    data[i + 1] = value;
    data[i + 2] = value;
  }
  ctx.putImageData(imgData, 0, 0);
}

function applyThreshold(canvas, threshold = 160) {
  const ctx = getCanvasContext(canvas, true);
  const imgData = ctx.getImageData(0, 0, canvas.width, canvas.height);
  const data = imgData.data;
  for (let i = 0; i < data.length; i += 4) {
    const gray = Math.round(0.299 * data[i] + 0.587 * data[i + 1] + 0.114 * data[i + 2]);
    const value = gray > threshold ? 255 : 0;
    data[i] = value;
    data[i + 1] = value;
    data[i + 2] = value;
  }
  ctx.putImageData(imgData, 0, 0);
}

function cloneCanvas(source) {
  const copy = document.createElement("canvas");
  copy.width = source.width;
  copy.height = source.height;
  copy.getContext("2d").drawImage(source, 0, 0);
  return copy;
}

// High-pass filter to remove low-frequency shading (scanner shadows, page curvature)
export function removeShading(canvas, blurRadius = 20, strength = 1.2) {
  const ctx = getCanvasContext(canvas, true);
  const width = canvas.width;
  const height = canvas.height;

  // Get original image data
  const originalData = ctx.getImageData(0, 0, width, height);

  // Create temporary canvas for blurred version (low frequencies)
  const tempCanvas = document.createElement("canvas");
  tempCanvas.width = width;
  tempCanvas.height = height;
  const tempCtx = tempCanvas.getContext("2d");

  // Draw blurred version to capture low-frequency components (shading)
  tempCtx.filter = `blur(${blurRadius}px)`;
  tempCtx.drawImage(canvas, 0, 0);

  // Get blurred image data
  const blurredData = tempCtx.getImageData(0, 0, width, height);

  // Subtract low frequencies from original (high-pass filter)
  const result = originalData;

  // Apply high-pass filter: add details back to white background
  for (let i = 0; i < result.data.length; i += 4) {
    // Convert to grayscale
    let grayOriginal = 0.299 * originalData.data[i] + 0.587 * originalData.data[i + 1] + 0.114 * originalData.data[i + 2];
    let grayBlurred = 0.299 * blurredData.data[i] + 0.587 * blurredData.data[i + 1] + 0.114 * blurredData.data[i + 2];

    // High-pass filter: get the details (original - blurred)
    const diff = (grayOriginal - grayBlurred) * strength;

    // Add details to white background (255 + diff)
    // Background (diff≈0) becomes white (255)
    // Text (diff<0) becomes darker than white
    const value = Math.max(0, Math.min(255, 255 + diff));

    // Apply to all RGB channels
    result.data[i] = value;
    result.data[i + 1] = value;
    result.data[i + 2] = value;
    // Alpha channel stays the same
  }

  // Put the processed data back
  ctx.putImageData(result, 0, 0);
}

// Min-max normalization to maximize contrast
export function enhanceContrast(canvas) {
  const ctx = getCanvasContext(canvas, true);
  const width = canvas.width;
  const height = canvas.height;

  // Get image data
  const imgData = ctx.getImageData(0, 0, width, height);
  const data = imgData.data;

  // Step 1: Find min and max values
  let min = 255;
  let max = 0;

  for (let i = 0; i < data.length; i += 4) {
    // Convert to grayscale
    const gray = 0.299 * data[i] + 0.587 * data[i + 1] + 0.114 * data[i + 2];
    if (gray < min) min = gray;
    if (gray > max) max = gray;
  }

  // Step 2: Apply min-max normalization
  const range = max - min;

  for (let i = 0; i < data.length; i += 4) {
    // Convert to grayscale
    const gray = 0.299 * data[i] + 0.587 * data[i + 1] + 0.114 * data[i + 2];

    // Min-max normalization: stretch [min, max] to [0, 255]
    const normalized = range > 0 ? ((gray - min) / range) * 255 : gray;

    // Apply to all RGB channels
    data[i] = normalized;
    data[i + 1] = normalized;
    data[i + 2] = normalized;
    // Alpha channel stays the same
  }

  // Put the processed data back
  ctx.putImageData(imgData, 0, 0);
}

export function applyModeToCanvas(mode, originalCanvas) {
  const copy = cloneCanvas(originalCanvas);
  if (mode === "gray") {
    applyGrayscale(copy);
  } else if (mode === "bw") {
    const usedOpenCv = binarizeCanvasAdaptive(copy);
    if (!usedOpenCv) {
      console.warn("OpenCV not ready; skipping B/W conversion.");
    }
  } else if (mode === "bw-otsu") {
    const usedOpenCv = binarizeCanvasOtsu(copy);
    if (!usedOpenCv) {
      console.warn("OpenCV not ready; skipping B/W Otsu conversion.");
    }
  }
  return copy;
}
