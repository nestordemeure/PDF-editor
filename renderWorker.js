/**
 * Web Worker for rendering PDF pages at full resolution.
 *
 * This worker receives tasks to render and process PDF pages,
 * applying operations and returning PNG blobs.
 *
 * Uses OffscreenCanvas for rendering without DOM access.
 */

// Import PDF.js for rendering
importScripts("https://cdn.jsdelivr.net/npm/pdfjs-dist@3.11.174/build/pdf.min.js");

// Set PDF.js worker source (required even inside a worker)
pdfjsLib.GlobalWorkerOptions.workerSrc = "https://cdn.jsdelivr.net/npm/pdfjs-dist@3.11.174/build/pdf.worker.min.js";

// Cache the loaded PDF document
let cachedPdf = null;
let cachedPdfBytesLength = null;

/**
 * Loads a PDF document, using cache if same bytes (by length as heuristic)
 */
async function loadPdf(pdfBytes) {
  // Convert to Uint8Array if needed
  const bytes = pdfBytes instanceof Uint8Array ? pdfBytes : new Uint8Array(pdfBytes);

  // Check if we have the same PDF cached (use length as heuristic)
  if (cachedPdfBytesLength === bytes.length && cachedPdf) {
    return cachedPdf;
  }

  // Load new PDF (PDF.js will spawn its own nested worker)
  const loadingTask = pdfjsLib.getDocument({ data: bytes });
  cachedPdf = await loadingTask.promise;
  cachedPdfBytesLength = bytes.length;
  return cachedPdf;
}

/**
 * Renders a PDF page to an OffscreenCanvas at full resolution
 */
async function renderPdfPage(pdf, pageIndex, dpi = 300) {
  const page = await pdf.getPage(pageIndex + 1); // PDF.js uses 1-based indexing
  const viewport = page.getViewport({ scale: 1 });

  // Calculate scale for target DPI (PDF default is 72 DPI)
  const scale = dpi / 72;
  const scaledViewport = page.getViewport({ scale });

  const canvas = new OffscreenCanvas(
    Math.round(scaledViewport.width),
    Math.round(scaledViewport.height)
  );

  const ctx = canvas.getContext("2d");
  await page.render({ canvasContext: ctx, viewport: scaledViewport }).promise;

  return canvas;
}

/**
 * Rotates a canvas by 90 degrees clockwise
 */
function rotateCanvas90(canvas) {
  const rotated = new OffscreenCanvas(canvas.height, canvas.width);
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
  const width = side === "left" ? mid : canvas.width - mid;
  const cropped = new OffscreenCanvas(width, canvas.height);
  const ctx = cropped.getContext("2d");

  if (side === "left") {
    ctx.drawImage(canvas, 0, 0, mid, canvas.height, 0, 0, mid, canvas.height);
  } else {
    ctx.drawImage(canvas, mid, 0, canvas.width - mid, canvas.height, 0, 0, canvas.width - mid, canvas.height);
  }

  return cropped;
}

/**
 * Applies grayscale to a canvas
 */
function applyGrayscale(canvas) {
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
 * Applies Otsu thresholding for B&W conversion
 */
function applyOtsuThreshold(canvas) {
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

  let sumB = 0;
  let wB = 0;
  let wF = 0;
  let maxVariance = 0;
  let threshold = 0;

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
 * Simple adaptive thresholding (approximation without OpenCV)
 */
function applyAdaptiveThreshold(canvas, blockSize = 15, C = 10) {
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

  // Calculate integral image for fast local mean computation
  const integral = new Float64Array((width + 1) * (height + 1));
  for (let y = 0; y < height; y++) {
    let rowSum = 0;
    for (let x = 0; x < width; x++) {
      rowSum += gray[y * width + x];
      integral[(y + 1) * (width + 1) + (x + 1)] =
        integral[y * (width + 1) + (x + 1)] + rowSum;
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
      const sum =
        integral[y2 * (width + 1) + x2] -
        integral[y1 * (width + 1) + x2] -
        integral[y2 * (width + 1) + x1] +
        integral[y1 * (width + 1) + x1];

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
 * Removes shading using high-pass filter
 */
function removeShading(canvas, blurRadius = 20, strength = 1.2) {
  const ctx = canvas.getContext("2d");
  const width = canvas.width;
  const height = canvas.height;

  const originalData = ctx.getImageData(0, 0, width, height);

  // Create blurred version using simple box blur (approximation)
  const tempCanvas = new OffscreenCanvas(width, height);
  const tempCtx = tempCanvas.getContext("2d");

  // Use filter if available, otherwise skip (OffscreenCanvas filter support varies)
  try {
    tempCtx.filter = `blur(${blurRadius}px)`;
    tempCtx.drawImage(canvas, 0, 0);
  } catch {
    // Fallback: just copy (no blur)
    tempCtx.drawImage(canvas, 0, 0);
  }

  const blurredData = tempCtx.getImageData(0, 0, width, height);

  // Apply high-pass filter
  for (let i = 0; i < originalData.data.length; i += 4) {
    const grayOriginal = 0.299 * originalData.data[i] + 0.587 * originalData.data[i + 1] + 0.114 * originalData.data[i + 2];
    const grayBlurred = 0.299 * blurredData.data[i] + 0.587 * blurredData.data[i + 1] + 0.114 * blurredData.data[i + 2];

    const diff = (grayOriginal - grayBlurred) * strength;
    const value = Math.max(0, Math.min(255, 255 + diff));

    originalData.data[i] = value;
    originalData.data[i + 1] = value;
    originalData.data[i + 2] = value;
  }

  ctx.putImageData(originalData, 0, 0);
}

/**
 * Enhances contrast using min-max normalization
 */
function enhanceContrast(canvas) {
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
  for (let i = 0; i < data.length; i += 4) {
    const gray = 0.299 * data[i] + 0.587 * data[i + 1] + 0.114 * data[i + 2];
    const normalized = range > 0 ? ((gray - min) / range) * 255 : gray;
    data[i] = normalized;
    data[i + 1] = normalized;
    data[i + 2] = normalized;
  }

  ctx.putImageData(imgData, 0, 0);
}

/**
 * Applies all operations to a canvas
 */
function applyOperations(canvas, operations) {
  let current = canvas;

  // First pass: geometric operations
  for (const op of operations) {
    if (op.type === "rotate") {
      const times = ((op.degrees / 90) % 4 + 4) % 4;
      for (let i = 0; i < times; i++) {
        current = rotateCanvas90(current);
      }
    } else if (op.type === "split") {
      current = cropCanvasHalf(current, op.side);
    }
  }

  // Second pass: pixel operations
  // Find last color mode
  let colorMode = "color";
  for (let i = operations.length - 1; i >= 0; i--) {
    if (operations[i].type === "colorMode") {
      colorMode = operations[i].mode;
      break;
    }
  }

  if (colorMode === "gray") {
    applyGrayscale(current);
  } else if (colorMode === "bw-otsu") {
    applyOtsuThreshold(current);
  } else if (colorMode === "bw") {
    applyAdaptiveThreshold(current);
  }

  // Apply shading removal
  if (operations.some(op => op.type === "removeShading")) {
    removeShading(current);
  }

  // Apply contrast enhancement
  if (operations.some(op => op.type === "enhanceContrast")) {
    enhanceContrast(current);
  }

  return current;
}

/**
 * Processes a single page: render, apply operations, encode to PNG
 */
async function processPage({ pdfBytes, pageIndex, operations, outputFormat = "png", jpegQuality = 0.85 }) {
  const pdf = await loadPdf(pdfBytes);

  // Render at full resolution
  let canvas = await renderPdfPage(pdf, pageIndex);

  // Apply operations
  canvas = applyOperations(canvas, operations);

  // Encode to blob
  const mimeType = outputFormat === "jpeg" ? "image/jpeg" : "image/png";
  const quality = outputFormat === "jpeg" ? jpegQuality : undefined;
  const blob = await canvas.convertToBlob({ type: mimeType, quality });

  // Convert blob to ArrayBuffer for transfer
  const arrayBuffer = await blob.arrayBuffer();

  return {
    arrayBuffer,
    width: canvas.width,
    height: canvas.height,
    mimeType,
  };
}

// Message handler
self.onmessage = async (event) => {
  const { taskId, type, data } = event.data;

  try {
    let result;

    if (type === "processPage") {
      result = await processPage(data);
      // Transfer the ArrayBuffer back to main thread
      self.postMessage({ taskId, result }, [result.arrayBuffer]);
    } else {
      throw new Error(`Unknown task type: ${type}`);
    }
  } catch (error) {
    self.postMessage({ taskId, error: error.message });
  }
};
