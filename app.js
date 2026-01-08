import { canvasToPngFile, prepareImageForPdf } from "./imagePipeline.js";
import { applyColorModeToSelection, rotateSelection, splitSelection, deleteSelection } from "./tools.js";
import { applyModeToCanvas } from "./imageColorModes.js";

const fileInput = document.getElementById("fileInput");
const rotateBtn = document.getElementById("rotateBtn");
const splitBtn = document.getElementById("splitBtn");
const colorModeSelect = document.getElementById("colorMode");
const deleteBtn = document.getElementById("deleteBtn");
const undoBtn = document.getElementById("undoBtn");
const redoBtn = document.getElementById("redoBtn");
const saveBtn = document.getElementById("saveBtn");
const recompressToggle = document.getElementById("recompressToggle");
const selectAllToggle = document.getElementById("selectAll");
const pageGrid = document.getElementById("pageGrid");
const pageCount = document.getElementById("pageCount");
const progressBar = document.getElementById("progressBar");
const statusText = document.getElementById("status");
const compressionLevel = document.getElementById("compressionLevel");
const previewCanvas = document.getElementById("previewCanvas");
const previewLabel = document.getElementById("previewLabel");
const ocrLang = document.getElementById("ocrLang");

const pdfjsLib = window["pdfjs-dist/build/pdf"];
pdfjsLib.GlobalWorkerOptions.workerSrc = "https://cdn.jsdelivr.net/npm/pdfjs-dist@3.11.174/build/pdf.worker.min.js";

const { PDFDocument } = window.PDFLib;

let pages = [];
let history = [];
let future = [];
let sortable = null;
let activePreviewId = null;
let scribeModule = null;
let progressFloor = 0;
let progressLock = false;
const sourceFileNames = new Set();
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

function yieldToUi() {
  return new Promise((resolve) => requestAnimationFrame(() => resolve()));
}

function setStatus(message) {
  statusText.textContent = message;
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
    const label = modeLabel(page.mode);
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
    .map((part) => sanitizeFilenamePart(part, 24));
  return `${parts.join("_")}.pdf`;
}

function setProgress(value, max) {
  if (max === 0) {
    progressBar.hidden = true;
    return;
  }
  progressBar.hidden = false;
  progressBar.max = max;
  if (progressLock) {
    progressFloor = Math.max(progressFloor, value);
    progressBar.value = progressFloor;
  } else {
    progressBar.value = value;
  }
}

function endProgress() {
  progressLock = false;
  progressFloor = 0;
  progressBar.value = 0;
  progressBar.hidden = true;
}

function updatePageCount() {
  pageCount.textContent = `${pages.length} page${pages.length === 1 ? "" : "s"}`;
}

async function pushHistory({ label = "Saving undo snapshot", showProgress = true } = {}) {
  const snapshot = [];
  const total = pages.length;
  if (showProgress && total > 0) {
    setProgress(0, total);
    setStatus(`${label} 0/${total}`);
    await yieldToUi();
  }
  for (let i = 0; i < total; i += 1) {
    const page = pages[i];
    snapshot.push({
      id: page.id,
      rotation: page.rotation,
      mode: page.mode,
      dataUrl: page.canvas.toDataURL("image/png"),
      originalDataUrl: page.originalCanvas.toDataURL("image/png"),
      width: page.canvas.width,
      height: page.canvas.height,
      pageSizePts: { width: page.pageSizePts.width, height: page.pageSizePts.height },
    });
    if (showProgress) {
      setProgress(i + 1, total);
      setStatus(`${label} ${i + 1}/${total}`);
      await yieldToUi();
    }
  }
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
      mode: item.mode,
      canvas,
      originalCanvas,
      selected: false,
      pageSizePts: item.pageSizePts || { width: item.width, height: item.height },
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

function multiplyMatrix(a, b) {
  return [
    a[0] * b[0] + a[2] * b[1],
    a[1] * b[0] + a[3] * b[1],
    a[0] * b[2] + a[2] * b[3],
    a[1] * b[2] + a[3] * b[3],
    a[0] * b[4] + a[2] * b[5] + a[4],
    a[1] * b[4] + a[3] * b[5] + a[5],
  ];
}

async function getPdfImageObject(page, name, timeoutMs = 50) {
  if (!name || !page?.objs?.get) return null;
  return Promise.race([
    new Promise((resolve) => {
      try {
        page.objs.get(name, (img) => resolve(img || null));
      } catch {
        resolve(null);
      }
    }),
    new Promise((resolve) => {
      setTimeout(() => resolve(null), timeoutMs);
    }),
  ]);
}

function getImageMetricsFromCtm(ctm, width, height) {
  const widthPts = Math.hypot(ctm[0] * width, ctm[1] * width);
  const heightPts = Math.hypot(ctm[2] * height, ctm[3] * height);
  if (!Number.isFinite(widthPts) || !Number.isFinite(heightPts) || widthPts <= 0 || heightPts <= 0) return null;
  const dpiX = width / (widthPts / 72);
  const dpiY = height / (heightPts / 72);
  if (!Number.isFinite(dpiX) || !Number.isFinite(dpiY) || dpiX <= 0 || dpiY <= 0) return null;
  return { dpi: Math.min(dpiX, dpiY), areaPts: widthPts * heightPts };
}

async function getPageDpiFromImages(page, pageSizePts) {
  const opList = await page.getOperatorList();
  const ops = pdfjsLib.OPS || {};
  let ctm = [1, 0, 0, 1, 0, 0];
  const stack = [];
  let best = null;
  const maxDpi = 600;
  const pageAreaPts = pageSizePts ? pageSizePts.width * pageSizePts.height : null;
  const minCoverage = 0.25;
  const fullPageCoverage = 0.9;

  for (let i = 0; i < opList.fnArray.length; i += 1) {
    const fn = opList.fnArray[i];
    const args = opList.argsArray[i];

    if (fn === ops.save) {
      stack.push(ctm.slice());
      continue;
    }
    if (fn === ops.restore) {
      ctm = stack.pop() || [1, 0, 0, 1, 0, 0];
      continue;
    }
    if (fn === ops.transform) {
      ctm = multiplyMatrix(ctm, args);
      continue;
    }
    if (fn === ops.setTransform) {
      ctm = args;
      continue;
    }

    if (fn === ops.paintInlineImageXObject) {
      const inlineImg = args?.[0];
      if (inlineImg?.width && inlineImg?.height) {
        let metrics = getImageMetricsFromCtm(ctm, inlineImg.width, inlineImg.height);
        if (metrics && pageAreaPts && metrics.areaPts / pageAreaPts >= fullPageCoverage) {
          const dpiX = inlineImg.width / (pageSizePts.width / 72);
          const dpiY = inlineImg.height / (pageSizePts.height / 72);
          metrics = { dpi: Math.min(dpiX, dpiY), areaPts: pageAreaPts };
        }
        if (metrics && (!pageAreaPts || metrics.areaPts / pageAreaPts >= minCoverage) && (!best || metrics.areaPts > best.areaPts)) best = metrics;
      }
      continue;
    }

    if (fn === ops.paintImageXObject || fn === ops.paintImageXObjectRepeat) {
      const name = args?.[0];
      const img = await getPdfImageObject(page, name);
      const width = img?.width;
      const height = img?.height;
      if (width && height) {
        let metrics = getImageMetricsFromCtm(ctm, width, height);
        if (metrics && pageAreaPts && metrics.areaPts / pageAreaPts >= fullPageCoverage) {
          const dpiX = width / (pageSizePts.width / 72);
          const dpiY = height / (pageSizePts.height / 72);
          metrics = { dpi: Math.min(dpiX, dpiY), areaPts: pageAreaPts };
        }
        if (metrics && (!pageAreaPts || metrics.areaPts / pageAreaPts >= minCoverage) && (!best || metrics.areaPts > best.areaPts)) best = metrics;
      }
    }
  }

  if (!best?.dpi) return null;
  return Math.min(best.dpi, maxDpi);
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
    onStart: (evt) => {
      evt.item.classList.add("dragging");
    },
    onEnd: async (evt) => {
      evt.item.classList.remove("dragging");
      await pushHistory({ showProgress: false });
      const order = Array.from(pageGrid.children).map((child) => child.dataset.pageId);
      pages.sort((a, b) => order.indexOf(a.id) - order.indexOf(b.id));
      renderPages();
    },
  });
}

function setPreview(page) {
  activePreviewId = page.id;
  const ctx = previewCanvas.getContext("2d");
  previewCanvas.width = page.canvas.width;
  previewCanvas.height = page.canvas.height;
  ctx.clearRect(0, 0, previewCanvas.width, previewCanvas.height);
  ctx.drawImage(page.canvas, 0, 0);
  previewLabel.textContent = `Previewing page #${pages.findIndex((p) => p.id === page.id) + 1}`;
}

function updatePreviewAfterRender() {
  if (!pages.length) {
    previewCanvas.width = 0;
    previewCanvas.height = 0;
    previewLabel.textContent = "Click a page to preview it.";
    return;
  }
  const page = pages.find((p) => p.id === activePreviewId) || pages[0];
  setPreview(page);
}

async function loadScribe() {
  if (scribeModule) return scribeModule;
  const moduleUrl = new URL("./vendor/scribe.js", import.meta.url);
  const module = await import(moduleUrl.href);
  scribeModule = module.default || module;
  return scribeModule;
}

async function normalizePdfBytes(result) {
  if (!result) return null;
  if (result instanceof Uint8Array) return result;
  if (result instanceof ArrayBuffer) return new Uint8Array(result);
  if (result instanceof Blob) return new Uint8Array(await result.arrayBuffer());
  return null;
}

function concatUint8Arrays(chunks) {
  let total = 0;
  for (const chunk of chunks) total += chunk.length;
  const result = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    result.set(chunk, offset);
    offset += chunk.length;
  }
  return result;
}

function encodeAscii(text) {
  return new TextEncoder().encode(text);
}

async function recompressPdfBytesWithPako(pdfBytes, { onProgress, onStatus, yieldToUi } = {}) {
  if (!window.pako || !pdfBytes) return pdfBytes;
  const bytes = pdfBytes instanceof Uint8Array ? pdfBytes : new Uint8Array(pdfBytes);
  const decoder = new TextDecoder("iso-8859-1");
  const text = decoder.decode(bytes);
  const parts = [];
  let cursor = 0;
  let streamsRecompressed = 0;
  let streamsTotal = 0;
  let skippedNonZlib = 0;
  let skippedInflate = 0;
  let rawDeflateUsed = 0;
  const yieldEvery = 10;

  const streamToken = "stream";
  const endstreamToken = "endstream";

  function findStreamDataStart(streamIndex) {
    let i = streamIndex + streamToken.length;
    if (bytes[i] === 0x0d && bytes[i + 1] === 0x0a) return i + 2;
    if (bytes[i] === 0x0a || bytes[i] === 0x0d) return i + 1;
    return i;
  }

  function parseLength(dictText) {
    const match = dictText.match(/\/Length\s+(\d+)\b/);
    if (!match) return null;
    return Number.parseInt(match[1], 10);
  }

  function hasPredictor(dictText) {
    return /\/Predictor\s+\d+/.test(dictText);
  }

  function isOnlyFlate(dictText) {
    const match = dictText.match(/\/Filter\s*(\[[^\]]+\]|\/[A-Za-z0-9]+)\b/);
    if (!match) return false;
    const filterValue = match[1].trim();
    if (filterValue.startsWith("[")) {
      const names = filterValue.match(/\/[A-Za-z0-9]+/g) || [];
      return names.length === 1 && names[0] === "/FlateDecode";
    }
    return filterValue === "/FlateDecode";
  }

  function replaceLength(dictText, nextLength) {
    return dictText.replace(/\/Length\s+\d+\b/, `/Length ${nextLength}`);
  }

  function looksLikeZlibHeader(buffer) {
    if (!buffer || buffer.length < 2) return false;
    const cmf = buffer[0];
    const flg = buffer[1];
    if ((cmf & 0x0f) !== 8) return false;
    return ((cmf << 8) + flg) % 31 === 0;
  }

  function collectStreamEntries() {
    const entries = [];
    const regex = />>\s*stream[\r\n]/g;
    let match;
    while ((match = regex.exec(text)) !== null) {
      const dictEnd = match.index + 2;
      const dictStart = text.lastIndexOf("<<", match.index);
      if (dictStart < 0 || dictEnd <= dictStart) continue;
      const streamIndex = match.index + match[0].lastIndexOf(streamToken);
      const dataStart = findStreamDataStart(streamIndex);
      if (!Number.isFinite(dataStart) || dataStart <= streamIndex) continue;
      entries.push({ dictStart, dictEnd, streamIndex, dataStart });
    }
    return entries;
  }

  const streamEntries = collectStreamEntries();

  let predictorStreams = 0;
  let predictorBytes = 0;
  // Count eligible streams first for progress reporting.
  {
    for (const entry of streamEntries) {
      const { dictStart, dictEnd } = entry;
      const dictText = text.slice(dictStart + 2, dictEnd);
      const length = parseLength(dictText);
      if (!Number.isFinite(length)) continue;
      if (isOnlyFlate(dictText) && hasPredictor(dictText)) {
        predictorStreams += 1;
        predictorBytes += length;
        continue;
      }
      if (!isOnlyFlate(dictText) || hasPredictor(dictText)) continue;
      streamsTotal += 1;
    }
  }

  if (predictorStreams > 0) {
    console.info(
      `PDF recompress: skipped ${predictorStreams} predictor stream${predictorStreams === 1 ? "" : "s"} ` +
        `(${(predictorBytes / 1024).toFixed(1)} KB).`
    );
  }
  console.info(`PDF recompress: ${streamsTotal} eligible Flate stream${streamsTotal === 1 ? "" : "s"}.`);
  if (streamsTotal > 0 && onStatus) onStatus(`Optimizing ${streamsTotal} PDF stream${streamsTotal === 1 ? "" : "s"}...`);
  if (streamsTotal > 0 && onProgress) onProgress(0, streamsTotal);

  for (const entry of streamEntries) {
    const { dictStart, dictEnd, dataStart } = entry;
    const dictText = text.slice(dictStart + 2, dictEnd);
    if (!isOnlyFlate(dictText) || hasPredictor(dictText)) continue;
    const length = parseLength(dictText);
    if (!Number.isFinite(length)) continue;

    const dataEnd = dataStart + length;
    if (dataEnd > bytes.length) continue;
    const endstreamIndex = text.indexOf(endstreamToken, dataEnd);
    if (endstreamIndex === -1 || endstreamIndex - dataEnd > 2) continue;

    const originalData = bytes.subarray(dataStart, dataEnd);
    let inflated;
    if (looksLikeZlibHeader(originalData)) {
      try {
        inflated = window.pako.inflate(originalData);
      } catch (error) {
        skippedInflate += 1;
        continue;
      }
    } else {
      try {
        inflated = window.pako.inflateRaw(originalData);
        rawDeflateUsed += 1;
      } catch (error) {
        skippedNonZlib += 1;
        continue;
      }
    }
    const recompressed = window.pako.deflate(inflated, { level: 9 });
    const nextDictText = replaceLength(dictText, recompressed.length);
    const nextDictBytes = encodeAscii(`<<${nextDictText}>>`);

    parts.push(bytes.subarray(cursor, dictStart));
    parts.push(nextDictBytes);
    parts.push(bytes.subarray(dictEnd + 2, dataStart));
    parts.push(recompressed);
    parts.push(bytes.subarray(dataEnd, endstreamIndex + endstreamToken.length));
    cursor = endstreamIndex + endstreamToken.length;
    streamsRecompressed += 1;
    if (onProgress) onProgress(streamsRecompressed, streamsTotal || streamsRecompressed);
    if (yieldToUi && streamsRecompressed % yieldEvery === 0) {
      await yieldToUi();
    }
  }

  if (streamsRecompressed === 0) return bytes;
  parts.push(bytes.subarray(cursor));
  if (skippedNonZlib > 0) {
    console.info(
      `PDF recompress: skipped ${skippedNonZlib} stream${skippedNonZlib === 1 ? "" : "s"} ` +
        "without a valid zlib header."
    );
  }
  if (skippedInflate > 0) {
    console.info(
      `PDF recompress: skipped ${skippedInflate} stream${skippedInflate === 1 ? "" : "s"} ` +
        "that failed to inflate."
    );
  }
  if (rawDeflateUsed > 0) {
    console.info(
      `PDF recompress: ${rawDeflateUsed} stream${rawDeflateUsed === 1 ? "" : "s"} ` +
        "inflated using raw deflate fallback."
    );
  }
  return concatUint8Arrays(parts);
}

function rebuildXrefAndTrailer(pdfBytes) {
  const bytes = pdfBytes instanceof Uint8Array ? pdfBytes : new Uint8Array(pdfBytes);
  const decoder = new TextDecoder("iso-8859-1");
  const text = decoder.decode(bytes);
  const startxrefIndex = text.lastIndexOf("startxref");
  if (startxrefIndex === -1) return bytes;

  const trailerIndex = text.lastIndexOf("trailer", startxrefIndex);
  if (trailerIndex === -1) return bytes;
  const xrefIndex = text.lastIndexOf("xref", trailerIndex);
  if (xrefIndex === -1) return bytes;
  const trailerStart = text.indexOf("<<", trailerIndex);
  const trailerEnd = text.indexOf(">>", trailerStart);
  if (trailerStart === -1 || trailerEnd === -1) return bytes;
  const trailerDict = text.slice(trailerStart + 2, trailerEnd);

  const rootMatch = trailerDict.match(/\/Root\s+(\d+\s+\d+\s+R)\b/);
  if (!rootMatch) return bytes;
  const infoMatch = trailerDict.match(/\/Info\s+(\d+\s+\d+\s+R)\b/);
  const idMatch = trailerDict.match(/\/ID\s+\[[^\]]+\]/);
  const encryptMatch = trailerDict.match(/\/Encrypt\s+(\d+\s+\d+\s+R)\b/);

  const body = bytes.subarray(0, xrefIndex);
  const bodyText = decoder.decode(body);
  const objectRegex = /(^|[\r\n])(\d+)\s+(\d+)\s+obj\b/g;
  const objectOffsets = new Map();
  let match;
  while ((match = objectRegex.exec(bodyText)) !== null) {
    const offset = match.index + match[1].length;
    const objNum = Number.parseInt(match[2], 10);
    if (!Number.isFinite(objNum)) continue;
    objectOffsets.set(objNum, offset);
  }

  const maxObj = Math.max(0, ...objectOffsets.keys());
  const size = maxObj + 1;
  const xrefLines = [];
  xrefLines.push("xref\n");
  xrefLines.push(`0 ${size}\n`);
  xrefLines.push("0000000000 65535 f \n");
  for (let i = 1; i < size; i += 1) {
    const offset = objectOffsets.get(i);
    if (Number.isFinite(offset)) {
      xrefLines.push(`${String(offset).padStart(10, "0")} 00000 n \n`);
    } else {
      xrefLines.push("0000000000 00000 f \n");
    }
  }

  const trailerParts = [];
  trailerParts.push("trailer\n<<\n");
  trailerParts.push(`/Size ${size}\n`);
  trailerParts.push(`/Root ${rootMatch[1]}\n`);
  if (infoMatch) trailerParts.push(`/Info ${infoMatch[1]}\n`);
  if (encryptMatch) trailerParts.push(`/Encrypt ${encryptMatch[1]}\n`);
  if (idMatch) trailerParts.push(`${idMatch[0]}\n`);
  trailerParts.push(">>\n");

  const xrefOffset = body.length;
  const xrefBytes = encodeAscii(xrefLines.join(""));
  const trailerBytes = encodeAscii(trailerParts.join(""));
  const startxrefBytes = encodeAscii(`startxref\n${xrefOffset}\n%%EOF\n`);

  return concatUint8Arrays([body, xrefBytes, trailerBytes, startxrefBytes]);
}

async function buildPdfBytes({ compression, quality = 0.85 } = {}) {
  const pdfDoc = await PDFDocument.create();
  let index = 0;
  const totalSteps = pages.length + 1;
  setProgress(0, totalSteps);
  for (const page of pages) {
    index += 1;
    setProgress(index, totalSteps);
    setStatus(`Saving PDF... ${index}/${pages.length}`);
    const { bytes, useJpeg } = await prepareImageForPdf({ page, compression, quality });
    const image = useJpeg ? await pdfDoc.embedJpg(bytes) : await pdfDoc.embedPng(bytes);
    const pdfPage = pdfDoc.addPage([page.pageSizePts.width, page.pageSizePts.height]);
    pdfPage.drawImage(image, {
      x: 0,
      y: 0,
      width: page.pageSizePts.width,
      height: page.pageSizePts.height,
    });
    await yieldToUi();
  }
  setStatus("Finalizing PDF...");
  setProgress(totalSteps, totalSteps);
  await yieldToUi();
  return pdfDoc.save({ useObjectStreams: false });
}

async function runOcrAndCreateTextPdf() {
  const scribe = await loadScribe();
  await scribe.init({ ocr: true, font: true, pdf: true });
  scribe.opt.displayMode = "ebook";
  scribe.opt.intermediatePDF = false;
  progressLock = true;
  progressFloor = 0;
  const stageOrder = ["importImage", "convert", "export"];
  const totalSteps = Math.max(1, pages.length * stageOrder.length);
  scribe.opt.progressHandler = (message) => {
    if (!message || typeof message.n !== "number") return;
    const stage = message.type || "ocr";
    const stageIndex = Math.max(0, stageOrder.indexOf(stage));
    const stepInStage = Math.min(message.n + 1, pages.length);
    const overallStep = Math.min(stageIndex * pages.length + stepInStage, totalSteps);
    setProgress(overallStep, totalSteps);
    if (stage === "importImage") setStatus(`OCR: loading images ${stepInStage}/${pages.length}`);
    if (stage === "convert") setStatus(`OCR: recognizing ${stepInStage}/${pages.length}`);
    if (stage === "export") setStatus(`OCR: generating PDF ${stepInStage}/${pages.length}`);
  };

  const imageFiles = pages.map((page, index) =>
    canvasToPngFile(page.originalCanvas, `page_${String(index + 1).padStart(4, "0")}.png`)
  );

  await scribe.importFiles({ imageFiles });
  await scribe.recognize({ langs: [ocrLang.value] });
  const textPdf = await scribe.exportData("pdf");
  await scribe.clear();
  progressLock = false;
  return normalizePdfBytes(textPdf);
}

const DEBUG_DPI = false;

async function renderPdfToPages(file) {
  const buffer = await file.arrayBuffer();
  const loadingTask = pdfjsLib.getDocument({ data: buffer });
  const pdf = await loadingTask.promise;
  const renderedPages = [];
  for (let i = 1; i <= pdf.numPages; i += 1) {
    setStatus(`Rendering ${file.name} page ${i}/${pdf.numPages}`);
    setProgress(i, pdf.numPages);
    const page = await pdf.getPage(i);
    const baseViewport = page.getViewport({ scale: 1 });
    const pageDpi = await getPageDpiFromImages(page, { width: baseViewport.width, height: baseViewport.height });
    let scale = pageDpi ? pageDpi / 72 : 2.5;
    if (!Number.isFinite(scale) || scale <= 0) scale = 2.5;
    let viewport = page.getViewport({ scale });
    if (!Number.isFinite(viewport.width) || !Number.isFinite(viewport.height) || viewport.width < 1 || viewport.height < 1) {
      viewport = page.getViewport({ scale: 1 });
    }
    if (!Number.isFinite(viewport.width) || !Number.isFinite(viewport.height) || viewport.width < 1 || viewport.height < 1) {
      console.warn("Skipping zero-size page", { page: i, width: viewport.width, height: viewport.height });
      continue;
    }
    if (DEBUG_DPI) {
      console.log("Page DPI", { page: i, pageDpi, scale, base: { w: baseViewport.width, h: baseViewport.height }, viewport: { w: viewport.width, h: viewport.height } });
    }
    const canvas = document.createElement("canvas");
    canvas.width = Math.max(1, Math.round(viewport.width));
    canvas.height = Math.max(1, Math.round(viewport.height));
    const ctx = canvas.getContext("2d");
    await page.render({ canvasContext: ctx, viewport }).promise;
    const originalCanvas = document.createElement("canvas");
    originalCanvas.width = canvas.width;
    originalCanvas.height = canvas.height;
    originalCanvas.getContext("2d").drawImage(canvas, 0, 0);
    const processedCanvas = applyModeToCanvas("gray", originalCanvas);
    renderedPages.push({
      id: `p_${Date.now()}_${Math.random().toString(16).slice(2)}`,
      rotation: 0,
      mode: "gray",
      canvas: processedCanvas,
      originalCanvas,
      selected: false,
      pageSizePts: { width: baseViewport.width, height: baseViewport.height },
    });
  }
  setProgress(0, 0);
  return renderedPages;
}


async function handleFiles(files) {
  if (!files.length) return;
  await pushHistory({ showProgress: false });
  setStatus("Loading PDFs...");
  const loaded = [];
  for (const file of files) {
    if (file.type !== "application/pdf") continue;
    const baseName = sanitizeFilenamePart(getFileStem(file.name));
    if (baseName) sourceFileNames.add(baseName);
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

rotateBtn.addEventListener("click", async () => {
  const selected = getSelectedPages();
  if (selected.length === 0) return;
  progressLock = true;
  progressFloor = 0;
  await pushHistory();
  progressFloor = 0;
  setProgress(0, selected.length);
  setStatus(`Rotating ${selected.length} page${selected.length === 1 ? "" : "s"}...`);
  await rotateSelection({ pages, setProgress, setStatus, yieldToUi });
  progressLock = false;
  renderPages();
  endProgress();
  setStatus("Rotation complete.");
});

colorModeSelect.addEventListener("change", async () => {
  const selected = getSelectedPages();
  if (selected.length === 0) return;
  const mode = colorModeSelect.value;
  progressLock = true;
  progressFloor = 0;
  await pushHistory();
  progressFloor = 0;
  setProgress(0, selected.length);
  setStatus(`Applying color mode to ${selected.length} page${selected.length === 1 ? "" : "s"}...`);
  await applyColorModeToSelection({ pages, mode, setProgress, setStatus, yieldToUi });
  progressLock = false;
  renderPages();
  endProgress();
  setStatus("Color mode updated.");
});

splitBtn.addEventListener("click", async () => {
  const selected = getSelectedPages();
  if (selected.length === 0) return;
  progressLock = true;
  progressFloor = 0;
  await pushHistory();
  progressFloor = 0;
  setProgress(0, pages.length);
  setStatus(`Splitting pages...`);
  const nextPages = await splitSelection({ pages, setProgress, setStatus, yieldToUi });
  progressLock = false;
  pages = nextPages;
  renderPages();
  endProgress();
  setStatus("Split complete.");
});

deleteBtn.addEventListener("click", async () => {
  const selected = getSelectedPages();
  if (selected.length === 0) return;
  progressLock = true;
  progressFloor = 0;
  await pushHistory();
  progressFloor = 0;
  setProgress(0, pages.length);
  setStatus(`Deleting ${selected.length} page${selected.length === 1 ? "" : "s"}...`);
  const nextPages = await deleteSelection({ pages, setProgress, setStatus, yieldToUi });
  progressLock = false;
  pages = nextPages;
  renderPages();
  endProgress();
  setStatus("Delete complete.");
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
    mode: page.mode,
    dataUrl: page.canvas.toDataURL("image/png"),
    originalDataUrl: page.originalCanvas.toDataURL("image/png"),
    width: page.canvas.width,
    height: page.canvas.height,
    pageSizePts: { width: page.pageSizePts.width, height: page.pageSizePts.height },
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
    mode: page.mode,
    dataUrl: page.canvas.toDataURL("image/png"),
    originalDataUrl: page.originalCanvas.toDataURL("image/png"),
    width: page.canvas.width,
    height: page.canvas.height,
    pageSizePts: { width: page.pageSizePts.width, height: page.pageSizePts.height },
  }));
  history.push(currentSnapshot);
  await restoreSnapshot(snapshot);
});

saveBtn.addEventListener("click", async () => {
  if (pages.length === 0) return;
  setStatus("Saving PDF...");
  const compression = compressionLevel.value;
  const quality = compression === "low" ? 0.75 : compression === "medium" ? 0.60 : compression === "high" ? 0.50 : 0.85;
  let pdfBytes = await buildPdfBytes({ compression, quality });
  let ocrUsed = false;

  if (ocrLang.value !== "none") {
    try {
      setStatus("Running OCR on original images... (this can take a while)");
      setProgress(0, 0);
      const textPdfBytes = await runOcrAndCreateTextPdf();
      if (textPdfBytes) {
        ocrUsed = true;
        const textDoc = await PDFDocument.load(textPdfBytes);
        const pageCount = Math.min(textDoc.getPageCount(), pages.length);
        for (let i = 0; i < pageCount; i += 1) {
          setProgress(i + 1, pageCount);
          setStatus(`OCR: embedding images ${i + 1}/${pageCount}`);
          const imagePage = textDoc.getPage(i);
          imagePage.setSize(pages[i].pageSizePts.width, pages[i].pageSizePts.height);
          const { bytes, useJpeg } = await prepareImageForPdf({ page: pages[i], compression, quality });
          const image = useJpeg ? await textDoc.embedJpg(bytes) : await textDoc.embedPng(bytes);
          imagePage.drawImage(image, {
            x: 0,
            y: 0,
            width: imagePage.getWidth(),
            height: imagePage.getHeight(),
          });
          await yieldToUi();
        }
        setStatus("Finalizing PDF...");
        setProgress(pageCount, pageCount);
        await yieldToUi();
        pdfBytes = await textDoc.save({ useObjectStreams: false });
        setStatus("OCR complete. Saving searchable PDF...");
      } else {
        setStatus("OCR completed, but output was unreadable. Saving without OCR.");
      }
    } catch (error) {
      console.error(error);
      setStatus("OCR failed. Saving without OCR.");
    } finally {
      progressLock = false;
    }
  }

  if (recompressToggle?.checked) {
    setStatus("Optimizing PDF streams for size...");
    setProgress(0, 1);
    pdfBytes = rebuildXrefAndTrailer(
      await recompressPdfBytesWithPako(pdfBytes, {
        onProgress: setProgress,
        onStatus: setStatus,
        yieldToUi,
      })
    );
  }
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
});

setStatus("Load one or more PDFs to begin.");
