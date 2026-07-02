/**
 * Tools for page operations.
 *
 * These functions modify the operation lists on pages and update thumbnails.
 * Geometric operations (rotate/split) transform the existing thumbnail in
 * place (exact and fast). Pixel operations (color mode, shading, contrast)
 * regenerate the thumbnail through the shared pipeline so the preview always
 * matches what the save will produce.
 */

import {
  generatePageId,
  createRotateOp,
  createSplitOp,
  createColorModeOp,
  createRemoveShadingOp,
  createEnhanceContrastOp,
  cloneOperations,
} from "./pageModel.js";
import { updatePageThumbnail, rotateCanvas90, cropCanvasHalf } from "./thumbnailRenderer.js";

// Concurrent PDF.js renders during thumbnail regeneration
export const THUMBNAIL_CONCURRENCY = 3;

/**
 * Runs fn over items with a bounded number in flight
 */
export async function forEachConcurrent(items, limit, fn) {
  let next = 0;
  const workers = Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (next < items.length) {
      const index = next++;
      await fn(items[index], index);
    }
  });
  await Promise.all(workers);
}

/**
 * Regenerates thumbnails for pages concurrently, reporting progress
 */
async function regenerateThumbnails(selected, { getPdfDocForPage, setProgress, setStatus, yieldToUi, statusLabel }) {
  let done = 0;
  await forEachConcurrent(selected, THUMBNAIL_CONCURRENCY, async page => {
    const pdfDoc = getPdfDocForPage ? getPdfDocForPage(page) : null;
    if (!pdfDoc) {
      throw new Error("Missing PDF source for page thumbnail.");
    }
    await updatePageThumbnail({ pdfDoc, page });
    done += 1;
    setProgress(done, selected.length);
    setStatus(`${statusLabel} ${done}/${selected.length}`);
    await yieldToUi();
  });
}

/**
 * Rotates selected pages by 90 degrees clockwise
 */
export async function rotateSelection({ pages, setProgress, setStatus, yieldToUi }) {
  const selected = pages.filter(page => page.selected);

  for (let i = 0; i < selected.length; i++) {
    const page = selected[i];

    page.operations.push(createRotateOp(90));

    // Swap page size
    page.pageSizePts = {
      width: page.pageSizePts.height,
      height: page.pageSizePts.width,
    };

    // Rotate existing thumbnail instead of re-rendering
    if (page.thumbnail) {
      page.thumbnail = rotateCanvas90(page.thumbnail);
    }

    setProgress(i + 1, selected.length);
    setStatus(`Rotating ${i + 1}/${selected.length}`);
    await yieldToUi();
  }
}

/**
 * Applies a color mode ('color', 'gray', 'bw', 'bw-otsu') to selected pages
 */
export async function applyColorModeToSelection({ pages, mode, getPdfDocForPage, setProgress, setStatus, yieldToUi }) {
  const selected = pages.filter(page => page.selected);

  for (const page of selected) {
    // Replace any existing color mode operation
    page.operations = page.operations.filter(op => op.type !== "colorMode");
    if (mode !== "color") {
      page.operations.push(createColorModeOp(mode));
    }
  }

  await regenerateThumbnails(selected, { getPdfDocForPage, setProgress, setStatus, yieldToUi, statusLabel: "Applying color mode" });
}

/**
 * Splits selected pages into left and right halves
 * @returns {Promise<Array>} New pages array with splits applied
 */
export async function splitSelection({ pages, setProgress, setStatus, yieldToUi }) {
  const nextPages = [];
  const selectedCount = pages.filter(p => p.selected).length;
  let processed = 0;

  for (let i = 0; i < pages.length; i++) {
    const page = pages[i];

    if (!page.selected) {
      nextPages.push(page);
    } else {
      const halfSize = { width: page.pageSizePts.width / 2, height: page.pageSizePts.height };

      const leftPage = {
        id: generatePageId(),
        sourceId: page.sourceId,
        sourcePageIndex: page.sourcePageIndex,
        pageSizePts: { ...halfSize },
        operations: [...cloneOperations(page.operations), createSplitOp("left")],
        thumbnail: page.thumbnail ? cropCanvasHalf(page.thumbnail, "left") : null,
        selected: false,
      };

      const rightPage = {
        id: generatePageId(),
        sourceId: page.sourceId,
        sourcePageIndex: page.sourcePageIndex,
        pageSizePts: { ...halfSize },
        operations: [...cloneOperations(page.operations), createSplitOp("right")],
        thumbnail: page.thumbnail ? cropCanvasHalf(page.thumbnail, "right") : null,
        selected: false,
      };

      nextPages.push(leftPage);
      nextPages.push(rightPage);

      processed++;
      setProgress(processed, selectedCount);
      setStatus(`Splitting ${processed}/${selectedCount}`);
      await yieldToUi();
    }
  }

  return nextPages;
}

/**
 * Deletes selected pages
 * @returns {Promise<Array>} New pages array without deleted pages
 */
export async function deleteSelection({ pages, setProgress, setStatus, yieldToUi }) {
  const selected = pages.filter(p => p.selected);
  const nextPages = pages.filter(p => !p.selected);

  setProgress(selected.length, selected.length);
  setStatus(`Deleted ${selected.length} page${selected.length === 1 ? "" : "s"}`);
  await yieldToUi();

  return nextPages;
}

/**
 * Removes shading from selected pages
 */
export async function removeShadingSelection({ pages, getPdfDocForPage, setProgress, setStatus, yieldToUi }) {
  const selected = pages.filter(page => page.selected && !page.operations.some(op => op.type === "removeShading"));

  for (const page of selected) {
    page.operations.push(createRemoveShadingOp());
  }

  await regenerateThumbnails(selected, { getPdfDocForPage, setProgress, setStatus, yieldToUi, statusLabel: "Removing shading" });
}

/**
 * Enhances contrast on selected pages
 */
export async function enhanceContrastSelection({ pages, getPdfDocForPage, setProgress, setStatus, yieldToUi }) {
  const selected = pages.filter(page => page.selected && !page.operations.some(op => op.type === "enhanceContrast"));

  for (const page of selected) {
    page.operations.push(createEnhanceContrastOp());
  }

  await regenerateThumbnails(selected, { getPdfDocForPage, setProgress, setStatus, yieldToUi, statusLabel: "Enhancing contrast" });
}
