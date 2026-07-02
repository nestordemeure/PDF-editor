/**
 * Web Worker for the save pipeline.
 *
 * Receives a page's rendered pixels (geometric operations already applied on
 * the main thread), runs the heavy pixel work (shading removal, contrast,
 * binarization/grayscale) and encodes the result, off the main thread.
 */

import { applyPixelPipeline, encodeProcessedImage } from "./imagePixelOps.js";

self.onmessage = async (event) => {
  const { id, buffer, width, height, operations, shadingScale, compression, jpegQuality, needOcrImage } = event.data;
  try {
    const imageData = new ImageData(new Uint8ClampedArray(buffer), width, height);
    const colorMode = applyPixelPipeline(imageData, operations, { shadingScale });
    const result = await encodeProcessedImage(imageData, { colorMode, compression, jpegQuality, needOcrImage });

    const transfer = [];
    if (result.raw) transfer.push(result.raw.buffer);
    if (result.bytes) transfer.push(result.bytes.buffer);
    if (result.ocrBytes && result.ocrBytes !== result.bytes) transfer.push(result.ocrBytes.buffer);
    self.postMessage({ id, ...result }, transfer);
  } catch (error) {
    self.postMessage({ id, error: error && error.message ? error.message : String(error) });
  }
};
