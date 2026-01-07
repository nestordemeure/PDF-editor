const DEFAULT_BW_OPTIONS = {
  blockSize: null,
  C: null,
  openKernelSize: 3,
  closeKernelSize: 3,
  openIterations: 1,
  closeIterations: 1,
};

export function isOpenCvReady() {
  return Boolean(window.cvReady && window.cv && window.cv.Mat);
}

function clamp(value, min, max) {
  return Math.max(min, Math.min(max, value));
}

function normalizeOdd(value, fallback) {
  const num = Number.isFinite(value) ? Math.floor(value) : fallback;
  if (num < 3) return 3;
  return num % 2 === 1 ? num : num + 1;
}

function resolveBwOptions(canvas, options) {
  const opts = { ...DEFAULT_BW_OPTIONS, ...options };
  const minDim = Math.min(canvas.width, canvas.height);
  const impliedBlock = Math.round(minDim / 50);
  const blockSize = normalizeOdd(opts.blockSize ?? impliedBlock, 31);
  const clampedBlock = normalizeOdd(clamp(blockSize, 15, 75), 31);
  const impliedC = Math.round(clampedBlock * 0.2);
  const C = Number.isFinite(opts.C) ? opts.C : clamp(impliedC, 6, 20);
  return { ...opts, blockSize: clampedBlock, C };
}

export function binarizeCanvasAdaptive(canvas, options = {}) {
  if (!isOpenCvReady()) return false;
  const cv = window.cv;
  const opts = resolveBwOptions(canvas, options);
  const blockSize = normalizeOdd(opts.blockSize, 31);

  const src = cv.imread(canvas);
  const gray = new cv.Mat();
  const bw = new cv.Mat();
  const rgba = new cv.Mat();

  try {
    cv.cvtColor(src, gray, cv.COLOR_RGBA2GRAY);
    cv.adaptiveThreshold(
      gray,
      bw,
      255,
      cv.ADAPTIVE_THRESH_GAUSSIAN_C,
      cv.THRESH_BINARY,
      blockSize,
      opts.C
    );

    cv.cvtColor(bw, rgba, cv.COLOR_GRAY2RGBA);
    cv.imshow(canvas, rgba);
    return true;
  } finally {
    src.delete();
    gray.delete();
    bw.delete();
    rgba.delete();
  }
}

export function binarizeCanvasOtsu(canvas) {
  if (!isOpenCvReady()) return false;
  const cv = window.cv;
  const src = cv.imread(canvas);
  const gray = new cv.Mat();
  const bw = new cv.Mat();
  const rgba = new cv.Mat();

  try {
    cv.cvtColor(src, gray, cv.COLOR_RGBA2GRAY);
    cv.threshold(gray, bw, 0, 255, cv.THRESH_BINARY | cv.THRESH_OTSU);
    cv.cvtColor(bw, rgba, cv.COLOR_GRAY2RGBA);
    cv.imshow(canvas, rgba);
    return true;
  } finally {
    src.delete();
    gray.delete();
    bw.delete();
    rgba.delete();
  }
}
