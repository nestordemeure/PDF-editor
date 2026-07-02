/**
 * PDF Editor - Main Application
 *
 * Non-destructive editing model:
 * - Stores source PDF bytes (small) instead of full-resolution canvases (large)
 * - Pages store references + operation lists
 * - Thumbnails provide low-res previews
 * - Full rendering happens at save time
 */

import * as pdfjsLib from "https://cdn.jsdelivr.net/npm/pdfjs-dist@5.7.284/build/pdf.min.mjs";
import { createPage, cloneOperations, getEffectiveColorMode } from "./pageModel.js";
import { getBasePageCanvas, updatePageThumbnail, applyOperationsToCanvas, clearBaseThumbnailCache, detectClassicPage } from "./pageRenderer.js";
import { applyColorModeToSelection, rotateSelection, splitSelection, deleteSelection, removeShadingSelection, enhanceContrastSelection, forEachConcurrent, THUMBNAIL_CONCURRENCY } from "./pageCommands.js";
import { savePdf } from "./saveManager.js";
import { tryDecryptPdf } from "./classicPdf.js";

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

// PDF.js setup (saveManager reads window.pdfjsLib for its bytes fallback)
pdfjsLib.GlobalWorkerOptions.workerSrc = "https://cdn.jsdelivr.net/npm/pdfjs-dist@5.7.284/build/pdf.worker.min.mjs";
window.pdfjsLib = pdfjsLib;

// pdf-lib
const { PDFDocument } = window.PDFLib;

// State
let pages = [];
let history = [];
let future = [];
let sortable = null;
let activePreviewId = null;
let scribeModule = null;

// Source PDF storage
const sourcePdfs = new Map(); // sourceId -> { bytes, pdfDoc, name }
let sourceIdCounter = 0;

const sourceFileNames = new Set();

// ============================================
// Utility Functions
// ============================================

/**
 * Returns a yield function that only actually yields every `intervalMs`,
 * so tight per-page loops don't pay a frame (16ms) per iteration.
 */
function makeThrottledYield(intervalMs = 40) {
  let last = performance.now();
  return async () => {
    if (performance.now() - last >= intervalMs) {
      await yieldToUi();
      last = performance.now();
    }
  };
}

function yieldToUi() {
  if (document.hidden) {
    // MessageChannel is not throttled in background tabs (unlike rAF which is paused)
    return new Promise(resolve => {
      const mc = new MessageChannel();
      mc.port1.onmessage = () => resolve();
      mc.port2.postMessage(undefined);
    });
  }
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

function createSourceId() {
  sourceIdCounter += 1;
  return `source_${Date.now()}_${sourceIdCounter}`;
}

function getPdfDocForPage(page) {
  if (!page || !page.sourceId) return null;
  return sourcePdfs.get(page.sourceId)?.pdfDoc || null;
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
  const suffixParts = [compressionLabel(compression), mostCommonModeLabel(), ocrUsed ? "ocr" : ""]
    .filter(Boolean)
    .map(part => sanitizeFilenamePart(part, 24));
  const parts = [baseName, ...suffixParts].filter(Boolean);
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
 * Thumbnail canvases are stored by reference — safe because thumbnails are
 * always replaced, never mutated in place, after their initial creation.
 * pushHistory strips them from older snapshots to bound memory.
 */
function createStateSnapshot() {
  return {
    pages: pages.map(page => ({
      id: page.id,
      sourceId: page.sourceId,
      sourcePageIndex: page.sourcePageIndex,
      pageSizePts: { ...page.pageSizePts },
      operations: cloneOperations(page.operations),
      selected: page.selected,
      thumbnail: page.thumbnail,
      isClassic: page.isClassic,
    })),
  };
}

function operationsEqual(a, b) {
  if (a === b) return true;
  if (!a || !b || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    const left = a[i];
    const right = b[i];
    const leftKeys = Object.keys(left);
    const rightKeys = Object.keys(right);
    if (leftKeys.length !== rightKeys.length) return false;
    for (const key of leftKeys) {
      if (left[key] !== right[key]) return false;
    }
  }
  return true;
}

/**
 * Restores state from a snapshot.
 * Thumbnails need to be regenerated.
 */
async function restoreStateFromSnapshot(snapshot) {
  // Map old pages by ID for thumbnail reuse
  const oldPagesById = new Map(pages.map(p => [p.id, p]));

  // Restore pages, preferring the snapshot's own thumbnail, then the current
  // page's thumbnail when the operations still match
  pages = snapshot.pages.map(snap => {
    const oldPage = oldPagesById.get(snap.id);
    const reusable =
      oldPage &&
      oldPage.thumbnail &&
      oldPage.sourceId === snap.sourceId &&
      oldPage.sourcePageIndex === snap.sourcePageIndex &&
      operationsEqual(oldPage.operations, snap.operations);
    return {
      id: snap.id,
      sourceId: snap.sourceId,
      sourcePageIndex: snap.sourcePageIndex,
      pageSizePts: { ...snap.pageSizePts },
      operations: cloneOperations(snap.operations),
      selected: snap.selected,
      thumbnail: snap.thumbnail || (reusable ? oldPage.thumbnail : null),
      isClassic: snap.isClassic,
    };
  });

  // Regenerate missing thumbnails
  const pagesNeedingThumbnails = pages.filter(p => !p.thumbnail);
  if (pagesNeedingThumbnails.length > 0 && sourcePdfs.size > 0) {
    setStatus("Regenerating thumbnails...");
    let done = 0;
    await forEachConcurrent(pagesNeedingThumbnails, THUMBNAIL_CONCURRENCY, async page => {
      const pdfDoc = getPdfDocForPage(page);
      if (!pdfDoc) return;
      await updatePageThumbnail({ pdfDoc, page });
      done += 1;
      setProgress(done, pagesNeedingThumbnails.length);
    });
    endProgress();
  }
}

// How many recent snapshots keep thumbnail references (instant undo);
// older ones regenerate thumbnails on restore to bound memory.
const SNAPSHOTS_WITH_THUMBNAILS = 2;

function stripSnapshotThumbnails(snapshot) {
  for (const page of snapshot.pages) {
    page.thumbnail = null;
  }
}

function pushHistory() {
  const snapshot = createStateSnapshot();
  history.push(snapshot);
  if (history.length > 50) {
    history.shift();
  }
  for (let i = 0; i < history.length - SNAPSHOTS_WITH_THUMBNAILS; i++) {
    stripSnapshotThumbnails(history[i]);
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

// Card DOM cache: pageId -> { card, canvas, checkbox, label, thumbRef }
// Cards are reused across renders; only changed parts are updated.
const cardCache = new Map();

function getPageById(id) {
  return pages.find(page => page.id === id) || null;
}

function createCardEntry(pageId) {
  const card = document.createElement("div");
  card.className = "page-card";
  card.dataset.pageId = pageId;

  const canvas = document.createElement("canvas");
  canvas.className = "page-canvas";

  const meta = document.createElement("div");
  meta.className = "page-meta";

  const label = document.createElement("span");
  label.className = "page-tag";

  const badge = document.createElement("span");
  badge.className = "page-tag page-tag-classic";
  badge.textContent = "text";
  badge.title = "Typeset page: saved as-is (text and quality preserved) unless edited beyond rotation";
  badge.hidden = true;

  const checkbox = document.createElement("input");
  checkbox.type = "checkbox";
  // Look pages up by id at event time: page objects are replaced by undo/redo
  checkbox.addEventListener("change", () => {
    const page = getPageById(pageId);
    if (!page) return;
    page.selected = checkbox.checked;
    syncSelectAll();
  });

  meta.appendChild(label);
  meta.appendChild(badge);
  meta.appendChild(checkbox);
  card.appendChild(canvas);
  card.appendChild(meta);

  card.addEventListener("click", event => {
    if (event.target.tagName.toLowerCase() === "input") return;
    const page = getPageById(pageId);
    if (!page) return;
    page.selected = !page.selected;
    checkbox.checked = page.selected;
    syncSelectAll();
    setPreview(page);
  });

  return { card, canvas, checkbox, label, badge, thumbRef: undefined };
}

function drawCardThumbnail(entry, page) {
  if (page.thumbnail) {
    entry.canvas.width = page.thumbnail.width;
    entry.canvas.height = page.thumbnail.height;
    entry.canvas.getContext("2d").drawImage(page.thumbnail, 0, 0);
  } else {
    entry.canvas.width = 100;
    entry.canvas.height = 140;
  }
  entry.thumbRef = page.thumbnail;
}

function renderPages() {
  const seen = new Set();

  pages.forEach((page, index) => {
    let entry = cardCache.get(page.id);
    if (!entry) {
      entry = createCardEntry(page.id);
      cardCache.set(page.id, entry);
    }
    seen.add(page.id);

    entry.label.textContent = `#${index + 1}`;
    entry.badge.hidden = !page.isClassic;
    entry.checkbox.checked = page.selected;
    if (entry.thumbRef !== page.thumbnail) {
      drawCardThumbnail(entry, page);
    }

    // Move into position only if not already there
    const currentChild = pageGrid.children[index];
    if (currentChild !== entry.card) {
      pageGrid.insertBefore(entry.card, currentChild || null);
    }
  });

  // Drop cards for removed pages
  for (const [id, entry] of cardCache) {
    if (!seen.has(id)) {
      entry.card.remove();
      cardCache.delete(id);
    }
  }
  while (pageGrid.children.length > pages.length) {
    pageGrid.lastChild.remove();
  }

  updatePageCount();
  syncSelectAll();
  updatePreviewAfterRender();
}

// Sortable is set up once; it tolerates cards being added/removed
sortable = new Sortable(pageGrid, {
  animation: 150,
  onStart: evt => {
    evt.item.classList.add("dragging");
  },
  onEnd: evt => {
    evt.item.classList.remove("dragging");
    pushHistory();
    const order = new Map(Array.from(pageGrid.children).map((child, i) => [child.dataset.pageId, i]));
    pages.sort((a, b) => order.get(a.id) - order.get(b.id));
    renderPages();
  },
});

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
  const moduleUrl = new URL("../vendor/scribe.js", import.meta.url);
  const module = await import(moduleUrl.href);
  scribeModule = module.default || module;
  return scribeModule;
}

async function handleFiles(files) {
  if (!files.length) return;

  const pdfFiles = Array.from(files).filter(f => f.type === "application/pdf");
  if (pdfFiles.length === 0) return;

  setStatus(`Loading ${pdfFiles.length} PDF${pdfFiles.length === 1 ? "" : "s"}...`);
  setProgress(0, 1);

  try {
    // Loading appends to any already-loaded pages (so books split across
    // several PDFs can be merged); undo removes the appended pages.
    // Select all + delete (or a refresh) starts over.
    if (pages.length > 0) {
      pushHistory();
    }

    // Load all PDFs first to get counts
    const sources = [];
    for (const file of pdfFiles) {
      let bytes = await file.arrayBuffer();
      let pdfDoc = await pdfjsLib.getDocument({ data: bytes.slice(0) }).promise;
      const sourceId = createSourceId();
      const baseName = getFileStem(file.name) || "file";

      // getPermissions() is null for unencrypted files. pdf-lib cannot
      // decrypt, so strip the encryption with qpdf (WASM, loaded lazily);
      // if that fails the source's pages go through the raster pipeline.
      let encrypted = (await pdfDoc.getPermissions()) !== null;
      if (encrypted) {
        setStatus(`Decrypting ${file.name}...`);
        const decrypted = await tryDecryptPdf(bytes);
        if (decrypted) {
          await pdfDoc.destroy();
          bytes = decrypted;
          pdfDoc = await pdfjsLib.getDocument({ data: bytes.slice(0) }).promise;
          encrypted = false;
        }
      }

      sourcePdfs.set(sourceId, { bytes, pdfDoc, name: baseName, encrypted });
      if (baseName) sourceFileNames.add(baseName);

      sources.push({ sourceId, pdfDoc, numPages: pdfDoc.numPages, name: baseName, encrypted });
    }

    // Create page objects with thumbnails (a few PDF.js renders in flight)
    const totalPages = sources.reduce((sum, source) => sum + source.numPages, 0);
    const pageSpecs = sources.flatMap(source =>
      Array.from({ length: source.numPages }, (_, i) => ({ source, pageIndex: i }))
    );
    const newPages = new Array(pageSpecs.length);
    let loadedPages = 0;
    const throttledYield = makeThrottledYield();

    let classicInEncrypted = 0;
    await forEachConcurrent(pageSpecs, THUMBNAIL_CONCURRENCY, async ({ source, pageIndex }, specIndex) => {
      const { canvas: thumbnail, pageSizePts } = await getBasePageCanvas({
        pdfDoc: source.pdfDoc,
        sourceId: source.sourceId,
        pageIndex,
      });

      // Encrypted sources can't be passed through (pdf-lib cannot decrypt),
      // so their text pages are treated like scans; the alert below explains.
      let isClassic = await detectClassicPage(source.pdfDoc, pageIndex);
      if (isClassic && source.encrypted) {
        classicInEncrypted += 1;
        isClassic = false;
      }

      const page = createPage({
        sourceId: source.sourceId,
        sourcePageIndex: pageIndex,
        pageSizePts,
        thumbnail: null,
        isClassic,
      });

      if (isClassic) {
        // Text pages keep their original colors and pass through at save time
        // (text, fonts and vectors preserved) as long as they stay unedited
        page.thumbnail = thumbnail;
      } else {
        // Apply default grayscale mode to the already-rendered canvas
        // (avoids rendering every page twice on load)
        page.operations.push({ type: "colorMode", mode: "gray" });
        page.thumbnail = applyOperationsToCanvas(thumbnail, page.operations);
      }

      newPages[specIndex] = page;
      loadedPages += 1;
      setStatus(`Loading ${source.name || "file"} page ${loadedPages}/${totalPages}`);
      setProgress(loadedPages, totalPages);
      await throttledYield();
    });

    // Ask what to do with detected text pages: preserving keeps them
    // verbatim (best for born-digital PDFs); stripping treats them as scans
    // (cleanup + re-OCR — often better when the text comes from an old OCR).
    for (const source of sources) {
      const classicPages = newPages.filter(page => page.sourceId === source.sourceId && page.isClassic);
      if (classicPages.length === 0) continue;
      const keep = confirm(
        `"${source.name}" contains ${classicPages.length} page${classicPages.length === 1 ? "" : "s"} with real text.\n\n` +
        `OK — keep the text: these pages are saved as-is (text, fonts and quality preserved; embedded images still compressed).\n\n` +
        `Cancel — strip the text: treat them as scans (cleaned, recompressed and re-OCRed; often better when the text comes from an old OCR).`
      );
      if (keep) continue;
      for (const page of classicPages) {
        page.isClassic = false;
        page.operations.push({ type: "colorMode", mode: "gray" });
        // Thumbnail is still the base render here (classic pages get no
        // default operations), so the scan default can be applied directly
        if (page.thumbnail) page.thumbnail = applyOperationsToCanvas(page.thumbnail, page.operations);
      }
    }

    pages = pages.concat(newPages);
    if (history.length === 0) pushHistory();

    if (classicInEncrypted > 0) {
      alert(
        `This PDF is encrypted and could not be decrypted: ` +
        `${classicInEncrypted} text page${classicInEncrypted === 1 ? "" : "s"} will be rasterized (converted to images) on save.`
      );
    }

    setStatus(`Loaded ${totalPages} page${totalPages === 1 ? "" : "s"} (${pages.length} total).`);
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

// Commands are async and mutate shared state; only one may run at a time
let commandInProgress = false;
async function runExclusive(fn) {
  if (commandInProgress) {
    setStatus("Please wait for the current operation to finish.");
    return;
  }
  commandInProgress = true;
  try {
    await fn();
  } finally {
    commandInProgress = false;
  }
}

fileInput.addEventListener("change", event => {
  const files = Array.from(event.target.files);
  event.target.value = "";
  runExclusive(() => handleFiles(files));
});

rotateBtn.addEventListener("click", () => runExclusive(async () => {
  const selected = getSelectedPages();
  if (selected.length === 0 || sourcePdfs.size === 0) return;

  pushHistory();
  setProgress(0, selected.length);
  setStatus(`Rotating ${selected.length} page${selected.length === 1 ? "" : "s"}...`);

  await rotateSelection({ pages, setProgress, setStatus, yieldToUi: makeThrottledYield() });

  renderPages();
  endProgress();
  setStatus("Rotation complete.");
}));

colorModeSelect.addEventListener("change", () => runExclusive(async () => {
  const selected = getSelectedPages();
  if (selected.length === 0 || sourcePdfs.size === 0) return;

  const mode = colorModeSelect.value;
  pushHistory();
  setProgress(0, selected.length);
  setStatus(`Applying color mode to ${selected.length} page${selected.length === 1 ? "" : "s"}...`);

  await applyColorModeToSelection({ pages, mode, getPdfDocForPage, setProgress, setStatus, yieldToUi: makeThrottledYield() });

  renderPages();
  endProgress();
  setStatus("Color mode updated.");
}));

splitBtn.addEventListener("click", () => runExclusive(async () => {
  const selected = getSelectedPages();
  if (selected.length === 0 || sourcePdfs.size === 0) return;

  pushHistory();
  setProgress(0, pages.length);
  setStatus("Splitting pages...");

  const nextPages = await splitSelection({ pages, setProgress, setStatus, yieldToUi: makeThrottledYield() });
  pages = nextPages;

  renderPages();
  endProgress();
  setStatus("Split complete.");
}));

deleteBtn.addEventListener("click", () => runExclusive(async () => {
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
}));

removeShadingBtn.addEventListener("click", () => runExclusive(async () => {
  const selected = getSelectedPages();
  if (selected.length === 0 || sourcePdfs.size === 0) return;

  pushHistory();
  setProgress(0, selected.length);
  setStatus(`Removing shading from ${selected.length} page${selected.length === 1 ? "" : "s"}...`);

  await removeShadingSelection({ pages, getPdfDocForPage, setProgress, setStatus, yieldToUi: makeThrottledYield() });

  renderPages();
  endProgress();
  setStatus("Shading removal complete.");
}));

enhanceContrastBtn.addEventListener("click", () => runExclusive(async () => {
  const selected = getSelectedPages();
  if (selected.length === 0 || sourcePdfs.size === 0) return;

  pushHistory();
  setProgress(0, selected.length);
  setStatus(`Enhancing contrast for ${selected.length} page${selected.length === 1 ? "" : "s"}...`);

  await enhanceContrastSelection({ pages, getPdfDocForPage, setProgress, setStatus, yieldToUi: makeThrottledYield() });

  renderPages();
  endProgress();
  setStatus("Contrast enhancement complete.");
}));

selectAllToggle.addEventListener("change", () => {
  const checked = selectAllToggle.checked;
  pages.forEach(page => {
    page.selected = checked;
  });
  renderPages();
});

undoBtn.addEventListener("click", () => runExclusive(async () => {
  if (history.length === 0) return;

  const currentState = createStateSnapshot();
  future.push(currentState);

  const previousState = history.pop();
  if (!previousState) return;

  await restoreStateFromSnapshot(previousState);
  renderPages();
  setStatus("Undo complete.");
}));

redoBtn.addEventListener("click", () => runExclusive(async () => {
  if (future.length === 0) return;

  const currentState = createStateSnapshot();
  history.push(currentState);

  const nextState = future.pop();
  if (!nextState) return;

  await restoreStateFromSnapshot(nextState);
  renderPages();
  setStatus("Redo complete.");
}));

saveBtn.addEventListener("click", () => runExclusive(async () => {
  if (pages.length === 0 || sourcePdfs.size === 0) return;

  saveBtn.disabled = true;
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
      pdfSources: sourcePdfs,
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
  } finally {
    saveBtn.disabled = false;
  }
}));

// Initial status
setStatus("Load a PDF to begin.");
