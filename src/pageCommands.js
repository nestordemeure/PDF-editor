/**
 * Tools for page operations.
 *
 * These functions modify the operation lists on pages and update thumbnails.
 * Rotation turns the existing thumbnail (pixel operations do not depend on
 * orientation). Everything else regenerates it through the engine's
 * preview, so the preview always matches what the save will produce:
 * contrast and adaptive B&W depend on what the crop keeps.
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
import { updatePageThumbnail, rotateCanvas90 } from "./pageRenderer.js";

// Concurrent PDF.js renders during thumbnail regeneration
export const THUMBNAIL_CONCURRENCY = 3;

/**
 * Runs fn over items with a bounded number in flight. After a failure no
 * new item starts; the first error is thrown once the others settle.
 */
export async function forEachConcurrent(items, limit, fn) {
  let next = 0;
  let failed = false;
  const workers = Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (next < items.length && !failed) {
      const index = next++;
      try {
        await fn(items[index], index);
      } catch (error) {
        failed = true;
        throw error;
      }
    }
  });
  const results = await Promise.allSettled(workers);
  const rejected = results.find(r => r.status === "rejected");
  if (rejected) throw rejected.reason;
}

/**
 * Regenerates thumbnails for pages concurrently, reporting progress
 */
export async function regenerateThumbnails(selected, { getRecordForPage, setProgress, setStatus, yieldToUi, statusLabel }) {
  let done = 0;
  await forEachConcurrent(selected, THUMBNAIL_CONCURRENCY, async page => {
    const record = getRecordForPage(page);
    if (!record) {
      throw new Error("Missing PDF source for page thumbnail.");
    }
    await updatePageThumbnail({ record, page });
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
export async function applyColorModeToSelection({ pages, mode, getRecordForPage, setProgress, setStatus, yieldToUi }) {
  const selected = pages.filter(page => page.selected);

  for (const page of selected) {
    // Replace any existing color mode operation
    page.operations = page.operations.filter(op => op.type !== "colorMode");
    if (mode !== "color") {
      page.operations.push(createColorModeOp(mode));
    }
  }

  await regenerateThumbnails(selected, { getRecordForPage, setProgress, setStatus, yieldToUi, statusLabel: "Applying color mode" });
}

/**
 * Splits selected pages into left and right halves
 * @returns {Promise<Array>} New pages array with splits applied
 */
export async function splitSelection({ pages, getRecordForPage, setProgress, setStatus, yieldToUi }) {
  const nextPages = [];
  const halves = [];

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
        thumbnail: null,
        selected: false,
        isClassic: page.isClassic,
        textStripped: page.textStripped,
      };

      const rightPage = {
        id: generatePageId(),
        sourceId: page.sourceId,
        sourcePageIndex: page.sourcePageIndex,
        pageSizePts: { ...halfSize },
        operations: [...cloneOperations(page.operations), createSplitOp("right")],
        thumbnail: null,
        selected: false,
        isClassic: page.isClassic,
        textStripped: page.textStripped,
      };

      nextPages.push(leftPage);
      nextPages.push(rightPage);
      halves.push(leftPage, rightPage);
    }
  }

  await regenerateThumbnails(halves, { getRecordForPage, setProgress, setStatus, yieldToUi, statusLabel: "Splitting" });
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
export async function removeShadingSelection({ pages, getRecordForPage, setProgress, setStatus, yieldToUi }) {
  const selected = pages.filter(page => page.selected && !page.operations.some(op => op.type === "removeShading"));

  for (const page of selected) {
    page.operations.push(createRemoveShadingOp());
  }

  await regenerateThumbnails(selected, { getRecordForPage, setProgress, setStatus, yieldToUi, statusLabel: "Removing shading" });
}

/**
 * Enhances contrast on selected pages
 */
export async function enhanceContrastSelection({ pages, getRecordForPage, setProgress, setStatus, yieldToUi }) {
  const selected = pages.filter(page => page.selected && !page.operations.some(op => op.type === "enhanceContrast"));

  for (const page of selected) {
    page.operations.push(createEnhanceContrastOp());
  }

  await regenerateThumbnails(selected, { getRecordForPage, setProgress, setStatus, yieldToUi, statusLabel: "Enhancing contrast" });
}
