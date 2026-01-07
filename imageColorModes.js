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

export function applyModeToCanvas(mode, originalCanvas) {
  const copy = cloneCanvas(originalCanvas);
  if (mode === "gray") {
    applyGrayscale(copy);
  } else if (mode === "gray4") {
    applyPosterize(copy, 16);
  } else if (mode === "gray-jpeg") {
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
