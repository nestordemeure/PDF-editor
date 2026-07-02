/**
 * Pure-JS binarization (no external dependencies).
 *
 * - Otsu: global threshold maximizing between-class variance.
 * - Adaptive: local mean threshold computed with an integral image,
 *   block size scaled to the canvas resolution so thumbnails and
 *   full-resolution renders binarize consistently.
 */

function clamp(value, min, max) {
  return Math.max(min, Math.min(max, value));
}

function normalizeOdd(value) {
  const num = Math.max(3, Math.floor(value));
  return num % 2 === 1 ? num : num + 1;
}

function getGrayscale(data, width, height) {
  const gray = new Uint8Array(width * height);
  for (let i = 0; i < gray.length; i++) {
    const idx = i * 4;
    gray[i] = (0.299 * data[idx] + 0.587 * data[idx + 1] + 0.114 * data[idx + 2]) | 0;
  }
  return gray;
}

function writeBinary(data, gray, isWhite) {
  for (let i = 0; i < gray.length; i++) {
    const value = isWhite(i) ? 255 : 0;
    const idx = i * 4;
    data[idx] = value;
    data[idx + 1] = value;
    data[idx + 2] = value;
  }
}

/**
 * Global Otsu threshold, applied in place.
 */
export function binarizeCanvasOtsu(canvas) {
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  const imgData = ctx.getImageData(0, 0, canvas.width, canvas.height);
  const gray = getGrayscale(imgData.data, canvas.width, canvas.height);

  const histogram = new Uint32Array(256);
  for (let i = 0; i < gray.length; i++) histogram[gray[i]]++;

  const total = gray.length;
  let sum = 0;
  for (let i = 0; i < 256; i++) sum += i * histogram[i];

  let sumB = 0, wB = 0;
  let maxVariance = 0, threshold = 128;
  for (let i = 0; i < 256; i++) {
    wB += histogram[i];
    if (wB === 0) continue;
    const wF = total - wB;
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

  writeBinary(imgData.data, gray, i => gray[i] > threshold);
  ctx.putImageData(imgData, 0, 0);
}

/**
 * Adaptive (local mean) threshold, applied in place.
 * Block size scales with resolution: ~minDim/50, clamped to [15, 75] at
 * full resolution but allowed down to 3 for small thumbnails.
 */
export function binarizeCanvasAdaptive(canvas) {
  const width = canvas.width;
  const height = canvas.height;
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  const imgData = ctx.getImageData(0, 0, width, height);
  const gray = getGrayscale(imgData.data, width, height);

  const minDim = Math.min(width, height);
  const blockSize = normalizeOdd(clamp(Math.round(minDim / 50), 3, 75));
  const C = clamp(Math.round(blockSize * 0.2), 2, 20);

  // Integral image for fast local means
  const integral = new Float64Array((width + 1) * (height + 1));
  for (let y = 0; y < height; y++) {
    let rowSum = 0;
    for (let x = 0; x < width; x++) {
      rowSum += gray[y * width + x];
      integral[(y + 1) * (width + 1) + (x + 1)] = integral[y * (width + 1) + (x + 1)] + rowSum;
    }
  }

  const halfBlock = Math.floor(blockSize / 2);
  const data = imgData.data;
  for (let y = 0; y < height; y++) {
    const y1 = Math.max(0, y - halfBlock);
    const y2 = Math.min(height, y + halfBlock + 1);
    for (let x = 0; x < width; x++) {
      const x1 = Math.max(0, x - halfBlock);
      const x2 = Math.min(width, x + halfBlock + 1);

      const count = (x2 - x1) * (y2 - y1);
      const sum = integral[y2 * (width + 1) + x2]
                - integral[y1 * (width + 1) + x2]
                - integral[y2 * (width + 1) + x1]
                + integral[y1 * (width + 1) + x1];

      const value = gray[y * width + x] > sum / count - C ? 255 : 0;
      const idx = (y * width + x) * 4;
      data[idx] = value;
      data[idx + 1] = value;
      data[idx + 2] = value;
    }
  }

  ctx.putImageData(imgData, 0, 0);
}
