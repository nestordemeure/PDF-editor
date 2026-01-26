/**
 * PDF Editor - Main Application
 *
 * Non-destructive editing model:
 * - Stores source PDF bytes (small) instead of full-resolution canvases (large)
 * - Pages store references + operation lists
 * - Thumbnails provide low-res previews
 * - Full rendering happens at save time using worker pool
 */

import { createPage, createPageSnapshot, cloneOperations, getEffectiveColorMode } from "./pageModel.js";
import { renderPdfPageThumbnail, updatePageThumbnail, updateThumbnailsBatch } from "./thumbnailRenderer.js";
import { applyColorModeToSelection, rotateSelection, splitSelection, deleteSelection, removeShadingSelection, enhanceContrastSelection } from "./tools.js";
import { savePdf, terminatePool } from "./saveManager.js";

// DOM Elements
const fileInput = document.getElementById("fileInput");
const rotateBtn = document.getElementById("rotateBtn");
const splitBtn = document.getElementById("splitBtn");
const colorModeSelect = document.getElementById("colorMode");
const deleteBtn = document.getElementById("deleteBtn");
const removeShadingBtn = document.getElementById("removeShadingBtn");
const enhanceContrastBtn = document.getElementById("enhanceContrastBtn");
const undoBtn = document.getElementById("undoBtn");
const redoBtn = document.getElementById("redoBtn");
const saveBtn = document.getElementById("saveBtn");
const selectAllToggle = document.getElementById("selectAll");
const pageGrid = document.getElementById("pageGrid");
const pageCount = document.getElementById("pageCount");
const progressBar = document.getElementById("progressBar");
const statusText = document.getElementById("status");
const compressionLevel = document.getElementById("compressionLevel");
const previewCanvas = document.getElementById("previewCanvas");
const previewLabel = document.getElementById("previewLabel");
const ocrLang = document.getElementById("ocrLang");

// PDF.js setup
const pdfjsLib = window["pdfjs-dist/build/pdf"];
pdfjsLib.GlobalWorkerOptions.workerSrc = "https://cdn.jsdelivr.net/npm/pdfjs-dist@3.11.174/build/pdf.worker.min.js";

// pdf-lib
const { PDFDocument } = window.PDFLib;

// OpenCV initialization (used for B&W modes in thumbnails)
window.cvReady = false;
window.onOpenCvReady = () => {
  if (!window.cv) return;
  if (window.cv.Mat) {
    window.cvReady = true;
    return;
  }
  window.cv.onRuntimeInitialized = () => {
    window.cvReady = true;
  };
};

// State
let pages = [];
let history = [];
let future = [];
let sortable = null;
let activePreviewId = null;
let scribeModule = null;

// Source PDF storage
let sourcePdfBytes = null;  // ArrayBuffer of the loaded PDF
let sourcePdfDoc = null;    // PDF.js document for thumbnail rendering

const sourceFileNames = new Set();

// ============================================
// Utility Functions
// ============================================

function yieldToUi() {
  return new Promise(resolve => requestAnimationFrame(() => resolve()));
}

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

function endProgress() {
  progressBar.value = 0;
  progressBar.hidden = true;
}

function getFileStem(filename) {
  if (!filename) return "file";
  const lastDot = filename.lastIndexOf(".");
  return lastDot > 0 ? filename.slice(0, lastDot) : filename;
}

function sanitizeFilenamePart(value, maxLength = 40) {
  const cleaned = (value || "")
    .replace(/[/\\?%*:|"<>]/g, "")
    .replace(/\s+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^[-_.]+|[-_.]+$/g, "")
    .slice(0, maxLength);
  return cleaned || "file";
}

function compressionLabel(value) {
  if (value === "none") return "";
  if (value === "low") return "lowcomp";
  if (value === "medium") return "medcomp";
  if (value === "high") return "highcomp";
  return "medcomp";
}

function modeLabel(mode) {
  if (mode === "bw") return "bwprog";
  if (mode === "bw-otsu") return "bw";
  if (mode === "gray") return "gray";
  return "";
}

function mostCommonModeLabel() {
  if (pages.length === 0) return "color";
  const counts = { "": 0, gray: 0, bw: 0, bwprog: 0 };
  for (const page of pages) {
    const mode = getEffectiveColorMode(page.operations);
    const label = modeLabel(mode);
    counts[label] = (counts[label] || 0) + 1;
  }
  let best = "";
  for (const label of ["gray", "bw", "bwprog", ""]) {
    if (counts[label] > (counts[best] || 0)) best = label;
  }
  return best;
}

function buildOutputFilename({ compression, ocrUsed }) {
  const baseName = sourceFileNames.size === 1 ? Array.from(sourceFileNames)[0] : "merged";
  const parts = [baseName, compressionLabel(compression), mostCommonModeLabel(), ocrUsed ? "ocr" : ""]
    .filter(Boolean)
    .map(part => sanitizeFilenamePart(part, 24));
  return `${parts.join("_")}.pdf`;
}

function updatePageCount() {
  pageCount.textContent = `${pages.length} page${pages.length === 1 ? "" : "s"}`;
}

// ============================================
// History Management (Lightweight)
// ============================================

/**
 * Creates a lightweight snapshot of the current state.
 * Only stores page metadata and operation lists, not pixel data.
 */
function createStateSnapshot() {
  return {
    pages: pages.map(page => ({
      id: page.id,
      sourcePageIndex: page.sourcePageIndex,
      pageSizePts: { ...page.pageSizePts },
      operations: cloneOperations(page.operations),
      selected: page.selected,
    })),
  };
}

/**
 * Restores state from a snapshot.
 * Thumbnails need to be regenerated.
 */
async function restoreStateFromSnapshot(snapshot) {
  // Map old pages by ID for thumbnail reuse
  const oldPagesById = new Map(pages.map(p => [p.id, p]));

  // Restore pages
  pages = snapshot.pages.map(snap => {
    const oldPage = oldPagesById.get(snap.id);
    return {
      id: snap.id,
      sourcePageIndex: snap.sourcePageIndex,
      pageSizePts: { ...snap.pageSizePts },
      operations: cloneOperations(snap.operations),
      selected: snap.selected,
      thumbnail: oldPage?.thumbnail || null, // Reuse if available
    };
  });

  // Regenerate missing thumbnails
  const pagesNeedingThumbnails = pages.filter(p => !p.thumbnail);
  if (pagesNeedingThumbnails.length > 0 && sourcePdfDoc) {
    setStatus("Regenerating thumbnails...");
    await updateThumbnailsBatch({
      pdfDoc: sourcePdfDoc,
      pages: pagesNeedingThumbnails,
      onProgress: (i, total) => setProgress(i, total),
    });
    endProgress();
  }
}

function pushHistory() {
  const snapshot = createStateSnapshot();
  history.push(snapshot);
  if (history.length > 50) {
    history.shift();
  }
  future = [];
}

// ============================================
// Page Selection
// ============================================

function getSelectedPages() {
  return pages.filter(page => page.selected);
}

function syncSelectAll() {
  if (pages.length === 0) {
    selectAllToggle.checked = false;
    return;
  }
  selectAllToggle.checked = pages.every(page => page.selected);
}

// ============================================
// Rendering
// ============================================

function renderPages() {
  pageGrid.innerHTML = "";

  pages.forEach((page, index) => {
    const card = document.createElement("div");
    card.className = "page-card";
    card.dataset.pageId = page.id;

    // Create canvas from thumbnail
    const canvas = document.createElement("canvas");
    canvas.className = "page-canvas";
    if (page.thumbnail) {
      canvas.width = page.thumbnail.width;
      canvas.height = page.thumbnail.height;
      canvas.getContext("2d").drawImage(page.thumbnail, 0, 0);
    } else {
      canvas.width = 100;
      canvas.height = 140;
    }

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

    card.addEventListener("click", event => {
      if (event.target.tagName.toLowerCase() === "input") return;
      page.selected = !page.selected;
      checkbox.checked = page.selected;
      syncSelectAll();
      setPreview(page);
    });

    pageGrid.appendChild(card);
  });

  updatePageCount();
  syncSelectAll();
  setupSortable();
  updatePreviewAfterRender();
}

function setupSortable() {
  if (sortable) {
    sortable.destroy();
  }
  sortable = new Sortable(pageGrid, {
    animation: 150,
    onStart: evt => {
      evt.item.classList.add("dragging");
    },
    onEnd: evt => {
      evt.item.classList.remove("dragging");
      pushHistory();
      const order = Array.from(pageGrid.children).map(child => child.dataset.pageId);
      pages.sort((a, b) => order.indexOf(a.id) - order.indexOf(b.id));
      renderPages();
    },
  });
}

function setPreview(page) {
  activePreviewId = page.id;
  const ctx = previewCanvas.getContext("2d");
  if (page.thumbnail) {
    previewCanvas.width = page.thumbnail.width;
    previewCanvas.height = page.thumbnail.height;
    ctx.clearRect(0, 0, previewCanvas.width, previewCanvas.height);
    ctx.drawImage(page.thumbnail, 0, 0);
  } else {
    previewCanvas.width = 100;
    previewCanvas.height = 140;
    ctx.clearRect(0, 0, previewCanvas.width, previewCanvas.height);
  }
  previewLabel.textContent = `Previewing page #${pages.findIndex(p => p.id === page.id) + 1}`;
}

function updatePreviewAfterRender() {
  if (!pages.length) {
    previewCanvas.width = 0;
    previewCanvas.height = 0;
    previewLabel.textContent = "Click a page to preview it.";
    return;
  }
  const page = pages.find(p => p.id === activePreviewId) || pages[0];
  setPreview(page);
}

// ============================================
// PDF Loading
// ============================================

async function loadScribe() {
  if (scribeModule) return scribeModule;
  const moduleUrl = new URL("./vendor/scribe.js", import.meta.url);
  const module = await import(moduleUrl.href);
  scribeModule = module.default || module;
  return scribeModule;
}

async function handleFiles(files) {
  if (!files.length) return;

  // Currently we only support loading one PDF at a time in the new model
  // (Multiple PDFs would require storing multiple source buffers)
  const file = Array.from(files).find(f => f.type === "application/pdf");
  if (!file) return;

  setStatus(`Loading ${file.name}...`);
  setProgress(0, 1);

  try {
    // Store source PDF bytes
    sourcePdfBytes = await file.arrayBuffer();

    // Load PDF.js document for thumbnail rendering
    const loadingTask = pdfjsLib.getDocument({ data: sourcePdfBytes.slice(0) });
    sourcePdfDoc = await loadingTask.promise;

    // Track filename
    const baseName = sanitizeFilenamePart(getFileStem(file.name));
    if (baseName) {
      sourceFileNames.clear();
      sourceFileNames.add(baseName);
    }

    // Create page objects with thumbnails
    const newPages = [];
    const numPages = sourcePdfDoc.numPages;

    for (let i = 0; i < numPages; i++) {
      setStatus(`Loading page ${i + 1}/${numPages}`);
      setProgress(i + 1, numPages);

      const { canvas: thumbnail, pageSizePts } = await renderPdfPageThumbnail({
        pdfDoc: sourcePdfDoc,
        pageIndex: i,
      });

      const page = createPage({
        sourcePageIndex: i,
        pageSizePts,
        thumbnail,
      });

      // Apply default grayscale mode
      page.operations.push({ type: "colorMode", mode: "gray" });

      // Update thumbnail to show grayscale
      await updatePageThumbnail({ pdfDoc: sourcePdfDoc, page });

      newPages.push(page);
      await yieldToUi();
    }

    // Replace pages (for now, single PDF support)
    pages = newPages;
    pushHistory();

    setStatus(`Loaded ${numPages} page${numPages === 1 ? "" : "s"}.`);
    endProgress();
    renderPages();
  } catch (error) {
    console.error("Failed to load PDF:", error);
    setStatus(`Error loading PDF: ${error.message}`);
    endProgress();
  }
}

// ============================================
// Event Handlers
// ============================================

fileInput.addEventListener("change", event => {
  handleFiles(Array.from(event.target.files));
  event.target.value = "";
});

rotateBtn.addEventListener("click", async () => {
  const selected = getSelectedPages();
  if (selected.length === 0 || !sourcePdfDoc) return;

  pushHistory();
  setProgress(0, selected.length);
  setStatus(`Rotating ${selected.length} page${selected.length === 1 ? "" : "s"}...`);

  await rotateSelection({ pages, pdfDoc: sourcePdfDoc, setProgress, setStatus, yieldToUi });

  renderPages();
  endProgress();
  setStatus("Rotation complete.");
});

colorModeSelect.addEventListener("change", async () => {
  const selected = getSelectedPages();
  if (selected.length === 0 || !sourcePdfDoc) return;

  const mode = colorModeSelect.value;
  pushHistory();
  setProgress(0, selected.length);
  setStatus(`Applying color mode to ${selected.length} page${selected.length === 1 ? "" : "s"}...`);

  await applyColorModeToSelection({ pages, mode, pdfDoc: sourcePdfDoc, setProgress, setStatus, yieldToUi });

  renderPages();
  endProgress();
  setStatus("Color mode updated.");
});

splitBtn.addEventListener("click", async () => {
  const selected = getSelectedPages();
  if (selected.length === 0 || !sourcePdfDoc) return;

  pushHistory();
  setProgress(0, pages.length);
  setStatus("Splitting pages...");

  const nextPages = await splitSelection({ pages, pdfDoc: sourcePdfDoc, setProgress, setStatus, yieldToUi });
  pages = nextPages;

  renderPages();
  endProgress();
  setStatus("Split complete.");
});

deleteBtn.addEventListener("click", async () => {
  const selected = getSelectedPages();
  if (selected.length === 0) return;

  pushHistory();
  setProgress(0, pages.length);
  setStatus(`Deleting ${selected.length} page${selected.length === 1 ? "" : "s"}...`);

  const nextPages = await deleteSelection({ pages, setProgress, setStatus, yieldToUi });
  pages = nextPages;

  renderPages();
  endProgress();
  setStatus("Delete complete.");
});

removeShadingBtn.addEventListener("click", async () => {
  const selected = getSelectedPages();
  if (selected.length === 0 || !sourcePdfDoc) return;

  pushHistory();
  setProgress(0, selected.length);
  setStatus(`Removing shading from ${selected.length} page${selected.length === 1 ? "" : "s"}...`);

  await removeShadingSelection({ pages, pdfDoc: sourcePdfDoc, setProgress, setStatus, yieldToUi });

  renderPages();
  endProgress();
  setStatus("Shading removal complete.");
});

enhanceContrastBtn.addEventListener("click", async () => {
  const selected = getSelectedPages();
  if (selected.length === 0 || !sourcePdfDoc) return;

  pushHistory();
  setProgress(0, selected.length);
  setStatus(`Enhancing contrast for ${selected.length} page${selected.length === 1 ? "" : "s"}...`);

  await enhanceContrastSelection({ pages, pdfDoc: sourcePdfDoc, setProgress, setStatus, yieldToUi });

  renderPages();
  endProgress();
  setStatus("Contrast enhancement complete.");
});

selectAllToggle.addEventListener("change", () => {
  const checked = selectAllToggle.checked;
  pages.forEach(page => {
    page.selected = checked;
  });
  renderPages();
});

undoBtn.addEventListener("click", async () => {
  if (history.length <= 1) return; // Keep at least the initial state

  const currentState = createStateSnapshot();
  future.push(currentState);

  history.pop(); // Remove current state
  const previousState = history[history.length - 1];

  if (previousState) {
    await restoreStateFromSnapshot(previousState);
    renderPages();
    setStatus("Undo complete.");
  }
});

redoBtn.addEventListener("click", async () => {
  if (future.length === 0) return;

  const nextState = future.pop();
  history.push(nextState);

  await restoreStateFromSnapshot(nextState);
  renderPages();
  setStatus("Redo complete.");
});

saveBtn.addEventListener("click", async () => {
  if (pages.length === 0 || !sourcePdfBytes) return;

  setStatus("Preparing to save...");
  setProgress(0, 1);

  try {
    const compression = compressionLevel.value;
    const lang = ocrLang.value;

    // Load scribe.js if OCR is needed
    let scribe = null;
    if (lang && lang !== "none") {
      setStatus("Loading OCR engine...");
      scribe = await loadScribe();
    }

    // Save using the worker pool
    const { pdfBytes, ocrUsed } = await savePdf({
      pdfBytes: sourcePdfBytes,
      pages,
      options: {
        compression,
        ocrLang: lang,
        scribeModule: scribe,
        PDFDocument,
      },
      onProgress: (value, max) => setProgress(value, max),
      onStatus: message => setStatus(message),
    });

    // Download the result
    const blob = new Blob([pdfBytes], { type: "application/pdf" });
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    const outputName = buildOutputFilename({ compression, ocrUsed });
    link.download = outputName;
    document.body.appendChild(link);
    link.click();
    link.remove();
    URL.revokeObjectURL(url);

    endProgress();
    setStatus(`Saved ${outputName}`);
  } catch (error) {
    console.error("Save failed:", error);
    setStatus(`Save failed: ${error.message}`);
    endProgress();
  }
});

// Cleanup on page unload
window.addEventListener("beforeunload", () => {
  terminatePool();
});

// Initial status
setStatus("Load a PDF to begin.");
