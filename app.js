const fileInput = document.getElementById("fileInput");
const rotateBtn = document.getElementById("rotateBtn");
const splitBtn = document.getElementById("splitBtn");
const bwBtn = document.getElementById("bwBtn");
const colorBtn = document.getElementById("colorBtn");
const deleteBtn = document.getElementById("deleteBtn");
const undoBtn = document.getElementById("undoBtn");
const redoBtn = document.getElementById("redoBtn");
const saveBtn = document.getElementById("saveBtn");
const selectAllToggle = document.getElementById("selectAll");
const pageGrid = document.getElementById("pageGrid");
const pageCount = document.getElementById("pageCount");
const progressBar = document.getElementById("progressBar");
const statusText = document.getElementById("status");
const compressToggle = document.getElementById("compressToggle");

const pdfjsLib = window["pdfjs-dist/build/pdf"];
pdfjsLib.GlobalWorkerOptions.workerSrc = "https://cdn.jsdelivr.net/npm/pdfjs-dist@3.11.174/build/pdf.worker.min.js";

const { PDFDocument } = window.PDFLib;

let pages = [];
let history = [];
let future = [];
let sortable = null;

function setStatus(message) {
  statusText.textContent = message;
}

function setProgress(value, max) {
  if (max === 0) {
    progressBar.hidden = true;
    return;
  }
  progressBar.hidden = false;
  progressBar.max = max;
  progressBar.value = value;
}

function updatePageCount() {
  pageCount.textContent = `${pages.length} page${pages.length === 1 ? "" : "s"}`;
}

function pushHistory() {
  const snapshot = pages.map((page) => ({
    id: page.id,
    rotation: page.rotation,
    bw: page.bw,
    dataUrl: page.canvas.toDataURL("image/png"),
    originalDataUrl: page.originalCanvas.toDataURL("image/png"),
    width: page.canvas.width,
    height: page.canvas.height,
  }));
  history.push(snapshot);
  if (history.length > 50) {
    history.shift();
  }
  future = [];
}

async function restoreSnapshot(snapshot) {
  const restored = [];
  for (const item of snapshot) {
    const canvas = await dataUrlToCanvas(item.dataUrl, item.width, item.height);
    const originalCanvas = await dataUrlToCanvas(item.originalDataUrl, item.width, item.height);
    restored.push({
      id: item.id,
      rotation: item.rotation,
      bw: item.bw,
      canvas,
      originalCanvas,
      selected: false,
    });
  }
  pages = restored;
  renderPages();
}

function dataUrlToCanvas(dataUrl, width, height) {
  return new Promise((resolve) => {
    const img = new Image();
    img.onload = () => {
      const canvas = document.createElement("canvas");
      canvas.width = width;
      canvas.height = height;
      const ctx = canvas.getContext("2d");
      ctx.drawImage(img, 0, 0, width, height);
      resolve(canvas);
    };
    img.src = dataUrl;
  });
}

function getSelectedPages() {
  return pages.filter((page) => page.selected);
}

function syncSelectAll() {
  if (pages.length === 0) {
    selectAllToggle.checked = false;
    return;
  }
  selectAllToggle.checked = pages.every((page) => page.selected);
}

function renderPages() {
  pageGrid.innerHTML = "";
  pages.forEach((page, index) => {
    const card = document.createElement("div");
    card.className = "page-card";
    card.dataset.pageId = page.id;

    const canvas = document.createElement("canvas");
    canvas.className = "page-canvas";
    canvas.width = page.canvas.width;
    canvas.height = page.canvas.height;
    canvas.getContext("2d").drawImage(page.canvas, 0, 0);

    const meta = document.createElement("div");
    meta.className = "page-meta";

    const label = document.createElement("span");
    label.className = "page-tag";
    label.textContent = `#${index + 1}`;

    const checkbox = document.createElement("input");
    checkbox.type = "checkbox";
    checkbox.checked = page.selected;
    checkbox.addEventListener("change", () => {
      page.selected = checkbox.checked;
      syncSelectAll();
    });

    meta.appendChild(label);
    meta.appendChild(checkbox);

    card.appendChild(canvas);
    card.appendChild(meta);

    card.addEventListener("click", (event) => {
      if (event.target.tagName.toLowerCase() === "input") return;
      page.selected = !page.selected;
      checkbox.checked = page.selected;
      syncSelectAll();
    });

    pageGrid.appendChild(card);
  });

  updatePageCount();
  syncSelectAll();
  setupSortable();
}

function setupSortable() {
  if (sortable) {
    sortable.destroy();
  }
  sortable = new Sortable(pageGrid, {
    animation: 150,
    onStart: (evt) => {
      evt.item.classList.add("dragging");
    },
    onEnd: (evt) => {
      evt.item.classList.remove("dragging");
      pushHistory();
      const order = Array.from(pageGrid.children).map((child) => child.dataset.pageId);
      pages.sort((a, b) => order.indexOf(a.id) - order.indexOf(b.id));
      renderPages();
    },
  });
}

async function renderPdfToPages(file) {
  const buffer = await file.arrayBuffer();
  const loadingTask = pdfjsLib.getDocument({ data: buffer });
  const pdf = await loadingTask.promise;
  const renderedPages = [];
  for (let i = 1; i <= pdf.numPages; i += 1) {
    setStatus(`Rendering ${file.name} page ${i}/${pdf.numPages}`);
    setProgress(i, pdf.numPages);
    const page = await pdf.getPage(i);
    const viewport = page.getViewport({ scale: 1.5 });
    const canvas = document.createElement("canvas");
    canvas.width = viewport.width;
    canvas.height = viewport.height;
    const ctx = canvas.getContext("2d");
    await page.render({ canvasContext: ctx, viewport }).promise;
    const originalCanvas = document.createElement("canvas");
    originalCanvas.width = canvas.width;
    originalCanvas.height = canvas.height;
    originalCanvas.getContext("2d").drawImage(canvas, 0, 0);
    renderedPages.push({
      id: `p_${Date.now()}_${Math.random().toString(16).slice(2)}`,
      rotation: 0,
      bw: false,
      canvas,
      originalCanvas,
      selected: false,
    });
  }
  setProgress(0, 0);
  return renderedPages;
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

function cloneCanvas(source) {
  const copy = document.createElement("canvas");
  copy.width = source.width;
  copy.height = source.height;
  copy.getContext("2d").drawImage(source, 0, 0);
  return copy;
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

async function handleFiles(files) {
  if (!files.length) return;
  pushHistory();
  setStatus("Loading PDFs...");
  const loaded = [];
  for (const file of files) {
    if (file.type !== "application/pdf") continue;
    const newPages = await renderPdfToPages(file);
    loaded.push(...newPages);
  }
  pages = pages.concat(loaded);
  setStatus(`Loaded ${loaded.length} page${loaded.length === 1 ? "" : "s"}.`);
  renderPages();
}

fileInput.addEventListener("change", (event) => {
  handleFiles(Array.from(event.target.files));
  event.target.value = "";
});

rotateBtn.addEventListener("click", () => {
  const selected = getSelectedPages();
  if (selected.length === 0) return;
  pushHistory();
  selected.forEach((page) => {
    page.canvas = rotateCanvas(page.canvas, true);
    page.originalCanvas = rotateCanvas(page.originalCanvas, true);
    page.rotation = (page.rotation + 90) % 360;
  });
  renderPages();
});

bwBtn.addEventListener("click", () => {
  const selected = getSelectedPages();
  if (selected.length === 0) return;
  pushHistory();
  selected.forEach((page) => {
    if (!page.bw) {
      const copy = cloneCanvas(page.originalCanvas);
      applyGrayscale(copy);
      page.canvas = copy;
      page.bw = true;
    }
  });
  renderPages();
});

colorBtn.addEventListener("click", () => {
  const selected = getSelectedPages();
  if (selected.length === 0) return;
  pushHistory();
  selected.forEach((page) => {
    if (page.bw) {
      page.canvas = cloneCanvas(page.originalCanvas);
      page.bw = false;
    }
  });
  renderPages();
});

splitBtn.addEventListener("click", () => {
  const selected = getSelectedPages();
  if (selected.length === 0) return;
  pushHistory();
  const nextPages = [];
  pages.forEach((page) => {
    if (!page.selected) {
      nextPages.push(page);
      return;
    }
    const [leftOriginal, rightOriginal] = splitCanvas(page.originalCanvas);
    let left = leftOriginal;
    let right = rightOriginal;
    if (page.bw) {
      left = cloneCanvas(leftOriginal);
      right = cloneCanvas(rightOriginal);
      applyGrayscale(left);
      applyGrayscale(right);
    }
    nextPages.push({
      id: `p_${Date.now()}_${Math.random().toString(16).slice(2)}`,
      rotation: page.rotation,
      bw: page.bw,
      canvas: left,
      originalCanvas: leftOriginal,
      selected: false,
    });
    nextPages.push({
      id: `p_${Date.now()}_${Math.random().toString(16).slice(2)}`,
      rotation: page.rotation,
      bw: page.bw,
      canvas: right,
      originalCanvas: rightOriginal,
      selected: false,
    });
  });
  pages = nextPages;
  renderPages();
});

deleteBtn.addEventListener("click", () => {
  const selected = getSelectedPages();
  if (selected.length === 0) return;
  pushHistory();
  pages = pages.filter((page) => !page.selected);
  renderPages();
});

selectAllToggle.addEventListener("change", () => {
  const checked = selectAllToggle.checked;
  pages.forEach((page) => {
    page.selected = checked;
  });
  renderPages();
});

undoBtn.addEventListener("click", async () => {
  if (history.length === 0) return;
  const snapshot = history.pop();
  const currentSnapshot = pages.map((page) => ({
    id: page.id,
    rotation: page.rotation,
    bw: page.bw,
    dataUrl: page.canvas.toDataURL("image/png"),
    originalDataUrl: page.originalCanvas.toDataURL("image/png"),
    width: page.canvas.width,
    height: page.canvas.height,
  }));
  future.push(currentSnapshot);
  await restoreSnapshot(snapshot);
});

redoBtn.addEventListener("click", async () => {
  if (future.length === 0) return;
  const snapshot = future.pop();
  const currentSnapshot = pages.map((page) => ({
    id: page.id,
    rotation: page.rotation,
    bw: page.bw,
    dataUrl: page.canvas.toDataURL("image/png"),
    originalDataUrl: page.originalCanvas.toDataURL("image/png"),
    width: page.canvas.width,
    height: page.canvas.height,
  }));
  history.push(currentSnapshot);
  await restoreSnapshot(snapshot);
});

saveBtn.addEventListener("click", async () => {
  if (pages.length === 0) return;
  setStatus("Saving PDF...");
  const pdfDoc = await PDFDocument.create();
  const useJpeg = compressToggle.checked;
  let index = 0;
  for (const page of pages) {
    index += 1;
    setProgress(index, pages.length);
    const dataUrl = page.canvas.toDataURL(useJpeg ? "image/jpeg" : "image/png", useJpeg ? 0.75 : undefined);
    const data = await fetch(dataUrl).then((res) => res.arrayBuffer());
    const image = useJpeg ? await pdfDoc.embedJpg(data) : await pdfDoc.embedPng(data);
    const pdfPage = pdfDoc.addPage([page.canvas.width, page.canvas.height]);
    pdfPage.drawImage(image, {
      x: 0,
      y: 0,
      width: page.canvas.width,
      height: page.canvas.height,
    });
  }
  const pdfBytes = await pdfDoc.save();
  const blob = new Blob([pdfBytes], { type: "application/pdf" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = "edited.pdf";
  document.body.appendChild(link);
  link.click();
  link.remove();
  URL.revokeObjectURL(url);
  setProgress(0, 0);
  setStatus("Saved edited.pdf");
});

setStatus("Load one or more PDFs to begin.");
