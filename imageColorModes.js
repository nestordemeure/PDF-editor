import { binarizeCanvasAdaptive, binarizeCanvasOtsu } from "./imageBinarization.js";

function getCanvasContext(canvas) {
  return canvas.getContext("2d", { willReadFrequently: true });
}

function applyGrayscale(canvas) {
  const ctx = getCanvasContext(canvas);
  const imgData = ctx.getImageData(0, 0, canvas.width, canvas.height);
  const data = imgData.data;
  for (let i = 0; i < data.length; i += 4) {
    const gray = (0.299 * data[i] + 0.587 * data[i + 1] + 0.114 * data[i + 2]) | 0;
    data[i] = gray;
    data[i + 1] = gray;
    data[i + 2] = gray;
  }
  ctx.putImageData(imgData, 0, 0);
}

/**
 * High-pass filter to remove low-frequency shading (scanner shadows, page curvature).
 * `scale` adjusts the blur radius to the canvas resolution (1 = 300 DPI full page)
 * so previews and exports at different DPIs produce the same result.
 */
export function removeShading(canvas, scale = 1, strength = 1.2) {
  const blurRadius = Math.max(2, Math.round(20 * scale));
  const ctx = getCanvasContext(canvas);
  const width = canvas.width;
  const height = canvas.height;

  const originalData = ctx.getImageData(0, 0, width, height);

  // Blurred version captures the low-frequency components (shading)
  const tempCanvas = document.createElement("canvas");
  tempCanvas.width = width;
  tempCanvas.height = height;
  const tempCtx = tempCanvas.getContext("2d", { willReadFrequently: true });
  tempCtx.filter = `blur(${blurRadius}px)`;
  tempCtx.drawImage(canvas, 0, 0);
  const blurredData = tempCtx.getImageData(0, 0, width, height);
  tempCanvas.width = 0;
  tempCanvas.height = 0;

  // High-pass: keep the details (original - blurred), on a white background.
  // Background (diff ~ 0) becomes white; text (diff < 0) stays dark.
  const data = originalData.data;
  const blurred = blurredData.data;
  for (let i = 0; i < data.length; i += 4) {
    const grayOriginal = 0.299 * data[i] + 0.587 * data[i + 1] + 0.114 * data[i + 2];
    const grayBlurred = 0.299 * blurred[i] + 0.587 * blurred[i + 1] + 0.114 * blurred[i + 2];
    const diff = (grayOriginal - grayBlurred) * strength;
    const value = Math.max(0, Math.min(255, 255 + diff));
    data[i] = value;
    data[i + 1] = value;
    data[i + 2] = value;
  }

  ctx.putImageData(originalData, 0, 0);
}

// Min-max normalization to maximize contrast
export function enhanceContrast(canvas) {
  const ctx = getCanvasContext(canvas);
  const imgData = ctx.getImageData(0, 0, canvas.width, canvas.height);
  const data = imgData.data;

  let min = 255;
  let max = 0;
  for (let i = 0; i < data.length; i += 4) {
    const gray = 0.299 * data[i] + 0.587 * data[i + 1] + 0.114 * data[i + 2];
    if (gray < min) min = gray;
    if (gray > max) max = gray;
  }

  const range = max - min;
  if (range <= 0) return;

  for (let i = 0; i < data.length; i += 4) {
    const gray = 0.299 * data[i] + 0.587 * data[i + 1] + 0.114 * data[i + 2];
    const normalized = ((gray - min) / range) * 255;
    data[i] = normalized;
    data[i + 1] = normalized;
    data[i + 2] = normalized;
  }
  ctx.putImageData(imgData, 0, 0);
}

/**
 * Applies a color mode to the canvas in place.
 */
export function applyModeToCanvas(mode, canvas) {
  if (mode === "gray") {
    applyGrayscale(canvas);
  } else if (mode === "bw") {
    binarizeCanvasAdaptive(canvas);
  } else if (mode === "bw-otsu") {
    binarizeCanvasOtsu(canvas);
  }
  return canvas;
}
