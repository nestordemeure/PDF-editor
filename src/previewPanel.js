/**
 * The preview panel: the active page fitted to the panel, or zoomed on a
 * spot at full scale. The full-scale view is the page as a save at the
 * current compression level writes it, rendered by PDF.js. The zoom spot is
 * kept in fractions of the page, so other pages show the same spot.
 */

import { library, createPixelPool, enginePage } from "./engine.js";
import { renderPageRgba, THUMBNAIL_WIDTH } from "./pageRenderer.js";

// Resolution of full-scale renders. 100% zoom shows one of their pixels
// per screen pixel.
const FULL_SCALE_DPI = 300;
const MAX_RENDER_PIXELS = 20e6;
// Full-scale renders kept for revisits, in pixels (4 bytes each).
const CACHE_PIXELS = 40e6;
const MAX_ZOOM = 8;
const WHEEL_STEP = 1.25;
// Pointer travel (CSS pixels) that makes a press a drag, not a click.
const DRAG_THRESHOLD = 4;

/**
 * Wires the panel. `getRecord(page)` gives a page's source record,
 * `getPageNumber(page)` its position, `getCompression()` the save's level.
 * Returns { show(page), redraw() }.
 */
export function createPreviewPanel({ box, stage, canvas, label, fitButton, getRecord, getPageNumber, getCompression, openPdf }) {
  let page = null;
  // { cx, cy, z } when zoomed: the view center in page fractions, and
  // screen pixels per full-scale pixel.
  let zoom = null;
  // key -> { canvas, widthPts, heightPts }, least recently used first
  const cache = new Map();
  let pool = null;
  let rendering = false;
  let failed = null; // { key, message }

  const keyOf = (target, record) => JSON.stringify([enginePage(target, record), getCompression()]);

  async function renderFullScale(target, record, level) {
    if (!pool) pool = createPixelPool(1);
    const description = enginePage(target, record);
    const message = await library("preview_job", description, level);
    let output = null;
    if (message) {
      const rendered = message.render ? await renderPageRgba(record.pdfDoc, message.index, message.render) : null;
      output = (await pool.runPage(message.job, rendered)).output;
    }
    const pdf = await openPdf(await library("preview_pdf", description, level, output));
    try {
      const pdfPage = await pdf.getPage(1);
      const base = pdfPage.getViewport({ scale: 1 });
      let scale = FULL_SCALE_DPI / 72;
      const pixels = base.width * base.height * scale * scale;
      if (pixels > MAX_RENDER_PIXELS) scale *= Math.sqrt(MAX_RENDER_PIXELS / pixels);
      const viewport = pdfPage.getViewport({ scale });
      const out = document.createElement("canvas");
      out.width = Math.round(viewport.width);
      out.height = Math.round(viewport.height);
      await pdfPage.render({ canvasContext: out.getContext("2d"), viewport }).promise;
      pdfPage.cleanup();
      return { canvas: out, widthPts: base.width, heightPts: base.height };
    } finally {
      await pdf.destroy();
    }
  }

  function remember(key, entry) {
    cache.set(key, entry);
    let total = 0;
    for (const value of cache.values()) total += value.canvas.width * value.canvas.height;
    for (const [oldKey, value] of cache) {
      if (total <= CACHE_PIXELS || oldKey === key) break;
      total -= value.canvas.width * value.canvas.height;
      value.canvas.width = 0;
      value.canvas.height = 0;
      cache.delete(oldKey);
    }
  }

  // One render at a time; when it ends, the panel asks for what it shows then.
  async function requestFullScale() {
    if (rendering || !page || !zoom) return;
    const target = page;
    const record = getRecord(target);
    const key = keyOf(target, record);
    if (cache.has(key) || (failed && failed.key === key)) return;
    rendering = true;
    try {
      remember(key, await renderFullScale(target, record, getCompression()));
      failed = null;
    } catch (error) {
      console.error("Full-scale preview failed:", error);
      failed = { key, message: error.message };
    } finally {
      rendering = false;
    }
    draw();
  }

  /** Page size in points: exact from a full-scale render, else from the thumbnail. */
  function pageSizePts(full) {
    if (full) return { width: full.widthPts, height: full.heightPts };
    const info = getRecord(page).pages[page.sourcePageIndex];
    const ptsPerPixel = info.width / THUMBNAIL_WIDTH;
    return { width: page.thumbnail.width * ptsPerPixel, height: page.thumbnail.height * ptsPerPixel };
  }

  /**
   * Sizes the canvas to the page's shape, within the panel and the window.
   * Zoomed, it keeps that height and takes the panel's width.
   */
  function layout(aspect) {
    const chrome = box.offsetHeight - stage.offsetHeight;
    const maxHeight = Math.max(200, window.innerHeight - 32 - chrome);
    const fitWidth = Math.min(stage.clientWidth, maxHeight / aspect);
    const cssWidth = zoom ? stage.clientWidth : fitWidth;
    const cssHeight = fitWidth * aspect;
    canvas.style.width = `${cssWidth}px`;
    canvas.style.height = `${cssHeight}px`;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = Math.max(1, Math.round(cssWidth * dpr));
    canvas.height = Math.max(1, Math.round(cssHeight * dpr));
  }

  function draw() {
    fitButton.hidden = !zoom;
    canvas.classList.toggle("is-zoomed", Boolean(zoom));
    if (!page) {
      canvas.width = 0;
      canvas.height = 0;
      canvas.style.width = canvas.style.height = "0";
      label.textContent = "Click a page to preview it.";
      return;
    }
    const number = `Page #${getPageNumber(page)}`;
    if (!page.thumbnail) {
      layout(1.4);
      label.textContent = number;
      return;
    }
    const record = getRecord(page);
    const key = keyOf(page, record);
    const full = zoom ? cache.get(key) : null;
    if (full) {
      // Most recently used last
      cache.delete(key);
      cache.set(key, full);
    }
    const size = pageSizePts(full);
    layout(size.height / size.width);
    const ctx = canvas.getContext("2d");
    ctx.clearRect(0, 0, canvas.width, canvas.height);

    if (!zoom) {
      ctx.imageSmoothingEnabled = true;
      ctx.imageSmoothingQuality = "high";
      ctx.drawImage(page.thumbnail, 0, 0, canvas.width, canvas.height);
      label.textContent = `${number} · click to zoom to full scale`;
      return;
    }

    // Page size in full-scale pixels, then on screen
    const pageWidth = size.width * FULL_SCALE_DPI / 72;
    const pageHeight = size.height * FULL_SCALE_DPI / 72;
    zoom.z = Math.min(MAX_ZOOM, Math.max(zoom.z, canvas.width / pageWidth));
    const shownWidth = pageWidth * zoom.z;
    const shownHeight = pageHeight * zoom.z;
    zoom.cx = clampCenter(zoom.cx, canvas.width / shownWidth);
    zoom.cy = clampCenter(zoom.cy, canvas.height / shownHeight);
    const x = canvas.width / 2 - zoom.cx * shownWidth;
    const y = canvas.height / 2 - zoom.cy * shownHeight;

    ctx.fillStyle = "#e8e4dd";
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    // Enlarged full-scale pixels stay sharp; the thumbnail stands in blurred.
    ctx.imageSmoothingEnabled = !full || zoom.z < 1;
    ctx.imageSmoothingQuality = "high";
    ctx.drawImage(full ? full.canvas : page.thumbnail, x, y, shownWidth, shownHeight);

    const percent = `${Math.round(zoom.z * 100)}%`;
    if (full) {
      label.textContent = `${number} · ${percent} · drag to move, scroll to zoom, Esc to fit`;
    } else if (failed && failed.key === key) {
      label.textContent = `${number} · full-scale render failed: ${failed.message}`;
    } else {
      label.textContent = `${number} · ${percent} · rendering at full scale...`;
      requestFullScale();
    }
  }

  /** Keeps the view on the page; centers it when it is wider than the page. */
  function clampCenter(center, viewFraction) {
    if (viewFraction >= 1) return 0.5;
    return Math.min(1 - viewFraction / 2, Math.max(viewFraction / 2, center));
  }

  /** The page point (fractions) under a pointer event, and its offset from the canvas center in canvas pixels. */
  function pointAt(event) {
    const rect = canvas.getBoundingClientRect();
    const dx = (event.clientX - rect.left) / rect.width * canvas.width - canvas.width / 2;
    const dy = (event.clientY - rect.top) / rect.height * canvas.height - canvas.height / 2;
    if (!zoom) return { fx: 0.5 + dx / canvas.width, fy: 0.5 + dy / canvas.height, dx, dy };
    const { width, height } = shownSize();
    return { fx: zoom.cx + dx / width, fy: zoom.cy + dy / height, dx, dy };
  }

  /** Size of the zoomed page on the canvas. */
  function shownSize() {
    const full = cache.get(keyOf(page, getRecord(page)));
    const size = pageSizePts(full);
    return { width: size.width * FULL_SCALE_DPI / 72 * zoom.z, height: size.height * FULL_SCALE_DPI / 72 * zoom.z };
  }

  function exitZoom() {
    zoom = null;
    draw();
  }

  let drag = null;
  canvas.addEventListener("pointerdown", event => {
    if (!page || !page.thumbnail || event.button !== 0) return;
    canvas.setPointerCapture(event.pointerId);
    drag = { x: event.clientX, y: event.clientY, moved: false, start: zoom && { ...zoom } };
  });
  canvas.addEventListener("pointermove", event => {
    if (!drag) return;
    if (Math.hypot(event.clientX - drag.x, event.clientY - drag.y) > DRAG_THRESHOLD) drag.moved = true;
    if (!drag.moved || !zoom) return;
    canvas.classList.add("is-dragging");
    const rect = canvas.getBoundingClientRect();
    const { width, height } = shownSize();
    zoom.cx = drag.start.cx - (event.clientX - drag.x) / rect.width * canvas.width / width;
    zoom.cy = drag.start.cy - (event.clientY - drag.y) / rect.height * canvas.height / height;
    draw();
  });
  const endDrag = event => {
    if (!drag) return;
    const click = !drag.moved;
    drag = null;
    canvas.classList.remove("is-dragging");
    if (click && !zoom && event.type === "pointerup") {
      const { fx, fy } = pointAt(event);
      zoom = { cx: fx, cy: fy, z: 1 };
      draw();
    }
  };
  canvas.addEventListener("pointerup", endDrag);
  canvas.addEventListener("pointercancel", endDrag);
  canvas.addEventListener("dblclick", () => {
    if (zoom) exitZoom();
  });
  canvas.addEventListener("wheel", event => {
    if (!zoom || !page || !page.thumbnail) return;
    event.preventDefault();
    const { fx, fy, dx, dy } = pointAt(event);
    zoom.z = Math.min(MAX_ZOOM, zoom.z * (event.deltaY < 0 ? WHEEL_STEP : 1 / WHEEL_STEP));
    // Keep the point under the pointer in place
    const { width, height } = shownSize();
    zoom.cx = fx - dx / width;
    zoom.cy = fy - dy / height;
    draw();
  }, { passive: false });
  fitButton.addEventListener("click", exitZoom);
  document.addEventListener("keydown", event => {
    if (event.key === "Escape" && zoom && !document.querySelector("dialog[open]")) exitZoom();
  });
  new ResizeObserver(() => draw()).observe(stage);

  return {
    show(next) {
      page = next;
      draw();
    },
    redraw: draw,
  };
}

/**
 * Sizes the preview column. By default it takes the width that page cards
 * leave after whole columns; dragging `resizer` sets a width, which is
 * kept, and a double click restores the default.
 */
export function initPreviewResizer({ layout, resizer, pagesBox, pageGrid }) {
  const STORAGE_KEY = "previewWidth";
  const CARD_WIDTH = 180;
  const CARD_GAP = 12;
  const MIN_PREVIEW = 360;
  const MIN_PAGES = 200;

  let chosen = null;
  try {
    const stored = Number(localStorage.getItem(STORAGE_KEY));
    if (stored > 0) chosen = stored;
  } catch {
    // Storage blocked: the default width applies
  }

  // Width of the pages and preview columns together
  const shared = () => pagesBox.parentElement.offsetWidth + layout.querySelector(".preview-sidebar").offsetWidth;

  function defaultWidth(total) {
    const padding = pagesBox.offsetWidth - pageGrid.clientWidth;
    const room = total - padding - MIN_PREVIEW;
    const columns = Math.max(1, Math.floor((room + CARD_GAP) / (CARD_WIDTH + CARD_GAP)));
    return total - padding - (columns * CARD_WIDTH + (columns - 1) * CARD_GAP);
  }

  function apply() {
    if (getComputedStyle(resizer).display === "none") return;
    const total = shared();
    const width = chosen === null ? defaultWidth(total) : Math.min(Math.max(chosen, 240), total - MIN_PAGES);
    layout.style.setProperty("--preview-width", `${Math.round(width)}px`);
  }

  resizer.addEventListener("pointerdown", event => {
    if (event.button !== 0) return;
    event.preventDefault();
    resizer.setPointerCapture(event.pointerId);
    resizer.classList.add("is-active");
    const right = layout.getBoundingClientRect().right;
    const move = e => {
      chosen = right - e.clientX - resizer.offsetWidth / 2;
      apply();
    };
    const up = () => {
      resizer.classList.remove("is-active");
      resizer.removeEventListener("pointermove", move);
      resizer.removeEventListener("pointerup", up);
      resizer.removeEventListener("pointercancel", up);
      try {
        localStorage.setItem(STORAGE_KEY, String(Math.round(chosen)));
      } catch {
        // Storage blocked: the width lasts for this visit
      }
    };
    resizer.addEventListener("pointermove", move);
    resizer.addEventListener("pointerup", up);
    resizer.addEventListener("pointercancel", up);
  });
  resizer.addEventListener("dblclick", () => {
    chosen = null;
    try {
      localStorage.removeItem(STORAGE_KEY);
    } catch {
      // Nothing stored
    }
    apply();
  });
  window.addEventListener("resize", apply);
  apply();
}
