const TARGET_DPI = {
  color: { low: 180, medium: 150, high: 120 },
  gray: { low: 200, medium: 150, high: 120 },
  bw: { low: 260, medium: 200, high: 150 },
};

import { binarizeCanvasAdaptive, binarizeCanvasOtsu, isOpenCvReady } from "./imageBinarization.js";

const BW_THRESHOLD = 160;

function getPageSizeInches(page) {
  const widthIn = page.pageSizePts.width / 72;
  const heightIn = page.pageSizePts.height / 72;
  return { widthIn, heightIn };
}

function getOriginalDpi(page) {
  const { widthIn, heightIn } = getPageSizeInches(page);
  const dpiX = page.originalCanvas.width / widthIn;
  const dpiY = page.originalCanvas.height / heightIn;
  return { dpiX, dpiY, dpi: Math.min(dpiX, dpiY) };
}

function getTargetDpi(mode, compression) {
  const band = TARGET_DPI[mode] || TARGET_DPI.gray;
  return band[compression] || band.medium;
}

function getOutputDpi(page, mode, compression) {
  const { dpi } = getOriginalDpi(page);
  if (compression === "none") return dpi;
  return Math.min(dpi, getTargetDpi(mode, compression));
}

function getOutputPixelSize(page, outputDpi) {
  const { widthIn, heightIn } = getPageSizeInches(page);
  const width = Math.max(1, Math.round(widthIn * outputDpi));
  const height = Math.max(1, Math.round(heightIn * outputDpi));
  return { width, height };
}

function makeScaledCanvas(source, width, height, { fillWhite = false, smooth = true } = {}) {
  if (source.width === width && source.height === height) {
    const copy = document.createElement("canvas");
    copy.width = width;
    copy.height = height;
    copy.getContext("2d").drawImage(source, 0, 0);
    return copy;
  }
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext("2d");
  ctx.imageSmoothingEnabled = smooth;
  if (smooth) {
    ctx.imageSmoothingQuality = "high";
  }
  if (fillWhite) {
    ctx.fillStyle = "#fff";
    ctx.fillRect(0, 0, width, height);
  }
  ctx.drawImage(source, 0, 0, width, height);
  return canvas;
}

function quantizeBw(data) {
  for (let i = 0; i < data.length; i += 4) {
    const gray = Math.round(0.299 * data[i] + 0.587 * data[i + 1] + 0.114 * data[i + 2]);
    const value = gray > BW_THRESHOLD ? 255 : 0;
    data[i] = value;
    data[i + 1] = value;
    data[i + 2] = value;
    data[i + 3] = 255;
  }
}

function encodeBwPng1Bit(canvas, mode) {
  if (!isOpenCvReady()) {
    throw new Error("OpenCV not ready for B/W conversion.");
  }
  const usedOpenCv = mode === "bw-otsu" ? binarizeCanvasOtsu(canvas) : binarizeCanvasAdaptive(canvas);
  const ctx = canvas.getContext("2d");
  const imgData = ctx.getImageData(0, 0, canvas.width, canvas.height);
  if (!usedOpenCv) {
    throw new Error("OpenCV failed to binarize B/W image.");
  }

  const rowBytes = Math.ceil(canvas.width / 8);
  const packed = new Uint8Array(rowBytes * canvas.height);
  let out = 0;
  for (let y = 0; y < canvas.height; y += 1) {
    let byte = 0;
    let bit = 7;
    for (let x = 0; x < canvas.width; x += 1) {
      const i = (y * canvas.width + x) * 4;
      const v = imgData.data[i] > 127 ? 1 : 0;
      byte |= v << bit;
      bit -= 1;
      if (bit < 0) {
        packed[out++] = byte;
        byte = 0;
        bit = 7;
      }
    }
    if (bit !== 7) packed[out++] = byte;
  }

  if (typeof UPNG.encodeLL === "function") {
    try {
      return new Uint8Array(UPNG.encodeLL([packed.buffer], canvas.width, canvas.height, 1, 0, 1));
    } catch {
      // fall through to RGBA encoding below
    }
  }

  return new Uint8Array(UPNG.encode([imgData.data.buffer], canvas.width, canvas.height, 0));
}

function convertToGreyscale(canvas) {
  const ctx = canvas.getContext("2d");
  const imgData = ctx.getImageData(0, 0, canvas.width, canvas.height);
  const data = imgData.data;

  for (let i = 0; i < data.length; i += 4) {
    const gray = Math.round(0.299 * data[i] + 0.587 * data[i + 1] + 0.114 * data[i + 2]);
    data[i] = gray;
    data[i + 1] = gray;
    data[i + 2] = gray;
    // data[i + 3] remains unchanged (alpha)
  }

  ctx.putImageData(imgData, 0, 0);
}

function applySubtleBlur(canvas, blurRadius = 0.4) {
  const temp = document.createElement("canvas");
  temp.width = canvas.width;
  temp.height = canvas.height;
  const ctx = temp.getContext("2d");
  ctx.filter = `blur(${blurRadius}px)`;
  ctx.drawImage(canvas, 0, 0);

  // Copy blurred result back to original canvas
  canvas.getContext("2d").drawImage(temp, 0, 0);
}

function encodePngFromCanvas(canvas, { bw = false, mode = "bw" } = {}) {
  const ctx = canvas.getContext("2d");
  const imgData = ctx.getImageData(0, 0, canvas.width, canvas.height);
  if (bw) {
    return encodeBwPng1Bit(canvas, mode);
  }
  const encoded = UPNG.encode([imgData.data.buffer], canvas.width, canvas.height, 0);
  return new Uint8Array(encoded);
}

function canvasToJpegBytes(canvas, quality) {
  return new Promise((resolve) => {
    canvas.toBlob(
      (blob) => {
        if (!blob) {
          const dataUrl = canvas.toDataURL("image/jpeg", quality);
          fetch(dataUrl)
            .then((res) => res.arrayBuffer())
            .then((fallback) => resolve(new Uint8Array(fallback)));
          return;
        }
        blob.arrayBuffer().then((buffer) => resolve(new Uint8Array(buffer)));
      },
      "image/jpeg",
      quality
    );
  });
}

export async function prepareImageForPdf({ page, compression, quality }) {
  const mode = page.mode;
  const compressionEnabled = compression !== "none";
  const useOriginal =
    (!compressionEnabled && mode === "color") || (compressionEnabled && mode !== "color" && mode !== "bw" && mode !== "gray");
  const sourceCanvas = mode === "bw" || mode === "bw-otsu" ? page.canvas : useOriginal ? page.originalCanvas : page.canvas;

  const outputDpi = getOutputDpi(page, mode, compression);
  const outputSize = compressionEnabled ? getOutputPixelSize(page, outputDpi) : { width: sourceCanvas.width, height: sourceCanvas.height };
  const fillWhite = compressionEnabled && mode !== "color";
  const smooth = !(mode === "bw");
  const workingCanvas = compressionEnabled
    ? makeScaledCanvas(sourceCanvas, outputSize.width, outputSize.height, { fillWhite, smooth })
    : sourceCanvas;

  if (compressionEnabled && mode === "color") {
    return { bytes: await canvasToJpegBytes(workingCanvas, quality), useJpeg: true };
  }

  if (compressionEnabled && mode === "gray") {
    convertToGreyscale(workingCanvas);
    applySubtleBlur(workingCanvas, 0.4);
    return { bytes: await canvasToJpegBytes(workingCanvas, quality), useJpeg: true };
  }

  if (mode === "bw" || mode === "bw-otsu") {
    return { bytes: encodePngFromCanvas(workingCanvas, { bw: true, mode }), useJpeg: false };
  }

  return { bytes: encodePngFromCanvas(workingCanvas), useJpeg: false };
}

export function canvasToPngFile(canvas, name) {
  const bytes = encodePngFromCanvas(canvas);
  return new File([bytes], name, { type: "image/png" });
}
