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

  for (let i = 0; i < selected.length; i++) {
    const page = selected[i];

    // Replace any existing color mode operation
    page.operations = page.operations.filter(op => op.type !== "colorMode");
    if (mode !== "color") {
      page.operations.push(createColorModeOp(mode));
    }

    await regenerateThumbnail(page, getPdfDocForPage);

    setProgress(i + 1, selected.length);
    setStatus(`Applying color mode ${i + 1}/${selected.length}`);
    await yieldToUi();
  }
}

/**
 * Regenerates a page's thumbnail from its source through the shared pipeline
 */
async function regenerateThumbnail(page, getPdfDocForPage) {
  const pdfDoc = getPdfDocForPage ? getPdfDocForPage(page) : null;
  if (!pdfDoc) {
    throw new Error("Missing PDF source for page thumbnail.");
  }
  await updatePageThumbnail({ pdfDoc, page });
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
  const selected = pages.filter(page => page.selected);

  for (let i = 0; i < selected.length; i++) {
    const page = selected[i];

    if (!page.operations.some(op => op.type === "removeShading")) {
      page.operations.push(createRemoveShadingOp());
      await regenerateThumbnail(page, getPdfDocForPage);
    }

    setProgress(i + 1, selected.length);
    setStatus(`Removing shading ${i + 1}/${selected.length}`);
    await yieldToUi();
  }
}

/**
 * Enhances contrast on selected pages
 */
export async function enhanceContrastSelection({ pages, getPdfDocForPage, setProgress, setStatus, yieldToUi }) {
  const selected = pages.filter(page => page.selected);

  for (let i = 0; i < selected.length; i++) {
    const page = selected[i];

    if (!page.operations.some(op => op.type === "enhanceContrast")) {
      page.operations.push(createEnhanceContrastOp());
      await regenerateThumbnail(page, getPdfDocForPage);
    }

    setProgress(i + 1, selected.length);
    setStatus(`Enhancing contrast ${i + 1}/${selected.length}`);
    await yieldToUi();
  }
}
