/**
 * Pixel operations on ImageData, shared by thumbnails (main thread) and the
 * save pipeline (Web Worker). Everything here is DOM-free so it runs in a
 * worker; encoding uses OffscreenCanvas when available.
 *
 * Canonical pixel order (applyPixelPipeline): remove shading -> enhance
 * contrast -> color mode (binarization last, on real gray values).
 */

import { OperationType, getEffectiveColorMode } from "./pageModel.js";
import { encode as encodeCcittG4, binarizeToBitPacked } from "../vendor/ccitt-g4-encoder.mjs";
import encodeMozJpeg from "../vendor/jsquash-jpeg/encode.js";
import { deflate } from "../vendor/pako.mjs";

function clamp(value, min, max) {
  return Math.max(min, Math.min(max, value));
}

function getGrayscale(data, length) {
  const gray = new Uint8Array(length);
  for (let i = 0; i < length; i++) {
    const idx = i * 4;
    gray[i] = (0.299 * data[idx] + 0.587 * data[idx + 1] + 0.114 * data[idx + 2]) | 0;
  }
  return gray;
}

function writeGrayToRgb(data, values) {
  for (let i = 0; i < values.length; i++) {
    const idx = i * 4;
    const value = values[i];
    data[idx] = value;
    data[idx + 1] = value;
    data[idx + 2] = value;
  }
}

function grayscaleData(imageData) {
  writeGrayToRgb(imageData.data, getGrayscale(imageData.data, imageData.width * imageData.height));
}

// ============================================
// Binarization
// ============================================

function binarizeOtsuData(imageData) {
  const gray = getGrayscale(imageData.data, imageData.width * imageData.height);

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

  for (let i = 0; i < gray.length; i++) gray[i] = gray[i] > threshold ? 255 : 0;
  writeGrayToRgb(imageData.data, gray);
}

/**
 * Adaptive (local mean) threshold. Block size scales with resolution
 * (~minDim/50) so thumbnails and full-resolution renders binarize alike.
 */
function binarizeAdaptiveData(imageData) {
  const width = imageData.width;
  const height = imageData.height;
  const gray = getGrayscale(imageData.data, width * height);

  const minDim = Math.min(width, height);
  const blockSizeRaw = clamp(Math.round(minDim / 50), 3, 75);
  const blockSize = blockSizeRaw % 2 === 1 ? blockSizeRaw : blockSizeRaw + 1;
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
  const out = new Uint8Array(width * height);
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

      out[y * width + x] = gray[y * width + x] > sum / count - C ? 255 : 0;
    }
  }
  writeGrayToRgb(imageData.data, out);
}

// ============================================
// Shading removal / contrast
// ============================================

function boxBlurHorizontal(src, dst, width, height, radius) {
  for (let y = 0; y < height; y++) {
    const row = y * width;
    const initialEnd = Math.min(radius, width - 1);
    let sum = 0;
    for (let x = 0; x <= initialEnd; x++) sum += src[row + x];
    let count = initialEnd + 1;
    for (let x = 0; x < width; x++) {
      dst[row + x] = sum / count;
      const add = x + radius + 1;
      if (add < width) { sum += src[row + add]; count++; }
      const remove = x - radius;
      if (remove >= 0) { sum -= src[row + remove]; count--; }
    }
  }
}

function boxBlurVertical(src, dst, width, height, radius) {
  for (let x = 0; x < width; x++) {
    const initialEnd = Math.min(radius, height - 1);
    let sum = 0;
    for (let y = 0; y <= initialEnd; y++) sum += src[y * width + x];
    let count = initialEnd + 1;
    for (let y = 0; y < height; y++) {
      dst[y * width + x] = sum / count;
      const add = y + radius + 1;
      if (add < height) { sum += src[add * width + x]; count++; }
      const remove = y - radius;
      if (remove >= 0) { sum -= src[remove * width + x]; count--; }
    }
  }
}

/**
 * Three-pass separable box blur (close to a Gaussian), used as the
 * low-frequency estimate for shading removal.
 */
function boxBlurGray(gray, width, height, radius) {
  let a = Float32Array.from(gray);
  let b = new Float32Array(gray.length);
  for (let pass = 0; pass < 3; pass++) {
    boxBlurHorizontal(a, b, width, height, radius);
    boxBlurVertical(b, a, width, height, radius);
  }
  return a;
}

/**
 * High-pass filter to remove low-frequency shading (scanner shadows, page
 * curvature). `scale` adjusts the blur radius to the canvas resolution
 * (1 = 300 DPI full page) so previews and exports at different DPIs match.
 */
function removeShadingData(imageData, scale = 1, strength = 1.2) {
  const width = imageData.width;
  const height = imageData.height;
  const radius = Math.max(2, Math.round(20 * scale));

  const gray = getGrayscale(imageData.data, width * height);
  const blurred = boxBlurGray(gray, width, height, radius);

  // Keep the details (original - blurred) on a white background:
  // background (diff ~ 0) becomes white; text (diff < 0) stays dark.
  const out = new Uint8Array(width * height);
  for (let i = 0; i < gray.length; i++) {
    const diff = (gray[i] - blurred[i]) * strength;
    out[i] = clamp(255 + diff, 0, 255);
  }
  writeGrayToRgb(imageData.data, out);
}

/**
 * Min-max normalization to maximize contrast
 */
function enhanceContrastData(imageData) {
  const gray = getGrayscale(imageData.data, imageData.width * imageData.height);

  let min = 255;
  let max = 0;
  for (let i = 0; i < gray.length; i++) {
    if (gray[i] < min) min = gray[i];
    if (gray[i] > max) max = gray[i];
  }

  const range = max - min;
  if (range <= 0) return;

  for (let i = 0; i < gray.length; i++) {
    gray[i] = ((gray[i] - min) / range) * 255;
  }
  writeGrayToRgb(imageData.data, gray);
}

// ============================================
// Pipeline
// ============================================

export function needsPixelWork(operations) {
  return getEffectiveColorMode(operations) !== "color"
    || operations.some(op => op.type === OperationType.REMOVE_SHADING)
    || operations.some(op => op.type === OperationType.ENHANCE_CONTRAST);
}

/**
 * Applies a page's pixel operations to imageData in the canonical order.
 * Geometric operations (rotate/split) must already be applied.
 * @returns {string} The effective color mode
 */
export function applyPixelPipeline(imageData, operations, { shadingScale = 1 } = {}) {
  if (operations.some(op => op.type === OperationType.REMOVE_SHADING)) {
    removeShadingData(imageData, shadingScale);
  }
  if (operations.some(op => op.type === OperationType.ENHANCE_CONTRAST)) {
    enhanceContrastData(imageData);
  }

  const colorMode = getEffectiveColorMode(operations);
  if (colorMode === "gray") {
    grayscaleData(imageData);
  } else if (colorMode === "bw") {
    binarizeAdaptiveData(imageData);
  } else if (colorMode === "bw-otsu") {
    binarizeOtsuData(imageData);
  }
  return colorMode;
}

// ============================================
// Encoding
// ============================================

function isBwMode(mode) {
  return mode === "bw" || mode === "bw-otsu";
}

/**
 * Packs a binarized image into 1 bit per pixel (rows byte-aligned, 1 = white)
 */
function packImageDataTo1Bit(imageData) {
  const { data, width, height } = imageData;
  const rowBytes = Math.ceil(width / 8);
  const packed = new Uint8Array(rowBytes * height);
  for (let y = 0; y < height; y++) {
    const rowOffset = y * rowBytes;
    for (let x = 0; x < width; x++) {
      if (data[(y * width + x) * 4] > 127) {
        packed[rowOffset + (x >> 3)] |= 0x80 >> (x & 7);
      }
    }
  }
  return packed;
}

/**
 * Extracts the 8-bit grayscale channel
 */
function extractGray8(imageData) {
  return getGrayscale(imageData.data, imageData.width * imageData.height);
}

async function imageDataToBlobBytes(imageData, type, quality) {
  let blob;
  if (typeof OffscreenCanvas !== "undefined") {
    const canvas = new OffscreenCanvas(imageData.width, imageData.height);
    canvas.getContext("2d").putImageData(imageData, 0, 0);
    blob = await canvas.convertToBlob({ type, quality });
  } else {
    const canvas = document.createElement("canvas");
    canvas.width = imageData.width;
    canvas.height = imageData.height;
    canvas.getContext("2d").putImageData(imageData, 0, 0);
    blob = await new Promise((resolve, reject) => {
      canvas.toBlob(b => (b ? resolve(b) : reject(new Error(`Canvas encoding to ${type} failed.`))), type, quality);
    });
    canvas.width = 0;
    canvas.height = 0;
  }
  return new Uint8Array(await blob.arrayBuffer());
}

/**
 * Encodes a processed page image for embedding.
 * Returns { kind, ... } where kind is:
 * - "ccitt-g4": CCITT Group 4 compressed bilevel data in `raw`
 * - "raw-gray": Flate-compressed DeviceGray samples (bitsPerComponent 1 or 8)
 *   in `raw` (compressed here, in the worker, so full-resolution pages are
 *   never held uncompressed)
 * - "png" / "jpeg": encoded bytes in `bytes`
 * `ocrBytes`/`ocrMime` hold an encoded image for the OCR engine when requested.
 */
export async function encodeProcessedImage(imageData, { colorMode, compression, jpegQuality, needOcrImage }) {
  const width = imageData.width;
  const height = imageData.height;

  if (isBwMode(colorMode)) {
    const ocrBytes = needOcrImage ? await imageDataToBlobBytes(imageData, "image/png") : null;
    try {
      // CCITT G4 compresses bilevel scans far better than Flate
      const raw = encodeCcittG4(binarizeToBitPacked(imageData, 128), width, height);
      return { kind: "ccitt-g4", raw, width, height, ocrBytes, ocrMime: "image/png" };
    } catch (e) {
      // Fall back to packed 1-bit + Flate
      const raw = deflate(packImageDataTo1Bit(imageData));
      return { kind: "raw-gray", bitsPerComponent: 1, raw, width, height, ocrBytes, ocrMime: "image/png" };
    }
  }

  if (compression === "none") {
    if (colorMode === "gray") {
      const raw = deflate(extractGray8(imageData));
      const ocrBytes = needOcrImage ? await imageDataToBlobBytes(imageData, "image/png") : null;
      return { kind: "raw-gray", bitsPerComponent: 8, raw, width, height, ocrBytes, ocrMime: "image/png" };
    }
    const bytes = await imageDataToBlobBytes(imageData, "image/png");
    return { kind: "png", bytes, width, height, ocrBytes: bytes, ocrMime: "image/png" };
  }

  const bytes = await encodeJpegBytes(imageData, jpegQuality, colorMode === "gray");
  return { kind: "jpeg", bytes, width, height, ocrBytes: bytes, ocrMime: "image/jpeg" };
}

/**
 * JPEG via MozJPEG (WASM): single-channel output for grayscale pages and
 * better entropy coding than the canvas encoder. Falls back to the canvas
 * encoder if the WASM module is unavailable.
 */
async function encodeJpegBytes(imageData, quality01, grayscale) {
  try {
    const buffer = await encodeMozJpeg(imageData, {
      quality: Math.round(quality01 * 100),
      color_space: grayscale ? 1 /* grayscale */ : 3 /* YCbCr */,
    });
    return new Uint8Array(buffer);
  } catch (e) {
    return await imageDataToBlobBytes(imageData, "image/jpeg", quality01);
  }
}
