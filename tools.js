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

function applyModeToCanvas(mode, originalCanvas) {
  const copy = cloneCanvas(originalCanvas);
  if (mode === "gray") {
    applyGrayscale(copy);
  } else if (mode === "gray4") {
    applyPosterize(copy, 16);
  } else if (mode === "bw") {
    applyThreshold(copy, 160);
  }
  return copy;
}

function rotateCanvas(canvas, clockwise = true) {
  const rotated = document.createElement("canvas");
  rotated.width = canvas.height;
  rotated.height = canvas.width;
  const ctx = rotated.getContext("2d");
  if (clockwise) {
    ctx.translate(rotated.width, 0);
    ctx.rotate(Math.PI / 2);
  } else {
    ctx.translate(0, rotated.height);
    ctx.rotate(-Math.PI / 2);
  }
  ctx.drawImage(canvas, 0, 0);
  return rotated;
}

function splitCanvas(canvas) {
  const mid = Math.floor(canvas.width / 2);
  const left = document.createElement("canvas");
  left.width = mid;
  left.height = canvas.height;
  left.getContext("2d").drawImage(canvas, 0, 0, mid, canvas.height, 0, 0, mid, canvas.height);

  const right = document.createElement("canvas");
  right.width = canvas.width - mid;
  right.height = canvas.height;
  right.getContext("2d").drawImage(canvas, mid, 0, canvas.width - mid, canvas.height, 0, 0, canvas.width - mid, canvas.height);

  return [left, right];
}

export async function applyColorModeToSelection({ pages, mode, setProgress, setStatus, yieldToUi }) {
  const selected = pages.filter((page) => page.selected);
  for (let i = 0; i < selected.length; i += 1) {
    const page = selected[i];
    page.mode = mode;
    page.canvas = applyModeToCanvas(mode, page.originalCanvas);
    setProgress(i + 1, selected.length);
    setStatus(`Applying color mode ${i + 1}/${selected.length}`);
    await yieldToUi();
  }
}

export async function rotateSelection({ pages, setProgress, setStatus, yieldToUi }) {
  const selected = pages.filter((page) => page.selected);
  for (let i = 0; i < selected.length; i += 1) {
    const page = selected[i];
    page.canvas = rotateCanvas(page.canvas, true);
    page.originalCanvas = rotateCanvas(page.originalCanvas, true);
    page.pageSizePts = { width: page.pageSizePts.height, height: page.pageSizePts.width };
    page.rotation = (page.rotation + 90) % 360;
    setProgress(i + 1, selected.length);
    setStatus(`Rotating ${i + 1}/${selected.length}`);
    await yieldToUi();
  }
}

export async function splitSelection({ pages, setProgress, setStatus, yieldToUi }) {
  const nextPages = [];
  for (let i = 0; i < pages.length; i += 1) {
    const page = pages[i];
    if (!page.selected) {
      nextPages.push(page);
    } else {
      const [leftOriginal, rightOriginal] = splitCanvas(page.originalCanvas);
      const left = applyModeToCanvas(page.mode, leftOriginal);
      const right = applyModeToCanvas(page.mode, rightOriginal);
      const leftSizePts = { width: page.pageSizePts.width / 2, height: page.pageSizePts.height };
      const rightSizePts = { width: page.pageSizePts.width / 2, height: page.pageSizePts.height };
      nextPages.push({
        id: `p_${Date.now()}_${Math.random().toString(16).slice(2)}`,
        rotation: page.rotation,
        mode: page.mode,
        canvas: left,
        originalCanvas: leftOriginal,
        selected: false,
        pageSizePts: leftSizePts,
      });
      nextPages.push({
        id: `p_${Date.now()}_${Math.random().toString(16).slice(2)}`,
        rotation: page.rotation,
        mode: page.mode,
        canvas: right,
        originalCanvas: rightOriginal,
        selected: false,
        pageSizePts: rightSizePts,
      });
    }
    setProgress(i + 1, pages.length);
    setStatus(`Splitting ${i + 1}/${pages.length}`);
    await yieldToUi();
  }
  return nextPages;
}

export async function deleteSelection({ pages, setProgress, setStatus, yieldToUi }) {
  const nextPages = [];
  for (let i = 0; i < pages.length; i += 1) {
    const page = pages[i];
    if (!page.selected) nextPages.push(page);
    setProgress(i + 1, pages.length);
    setStatus(`Deleting ${i + 1}/${pages.length}`);
    await yieldToUi();
  }
  return nextPages;
}
