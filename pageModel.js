/**
 * Page data model for non-destructive editing.
 *
 * Instead of storing full-resolution canvases, we store:
 * - A reference to the source (PDF bytes + page index)
 * - A list of operations to apply
 * - A low-resolution thumbnail for display
 */

let pageIdCounter = 0;

/**
 * Creates a unique page ID
 */
export function generatePageId() {
  return `page_${Date.now()}_${pageIdCounter++}`;
}

/**
 * Creates a new page object
 * @param {Object} params
 * @param {string} params.sourceId - ID of the source PDF
 * @param {number} params.sourcePageIndex - Index in the source PDF (0-based)
 * @param {Object} params.pageSizePts - Page size in points { width, height }
 * @param {HTMLCanvasElement} params.thumbnail - Low-res thumbnail canvas
 * @returns {Object} Page object
 */
export function createPage({ sourceId, sourcePageIndex, pageSizePts, thumbnail }) {
  return {
    id: generatePageId(),
    sourceId,
    sourcePageIndex,
    pageSizePts: { ...pageSizePts },
    operations: [],
    thumbnail,
    selected: false,
  };
}

/**
 * Deep clones a page's operations array
 */
export function cloneOperations(operations) {
  return operations.map(op => ({ ...op }));
}

/**
 * Creates a snapshot of a page for history
 */
export function createPageSnapshot(page) {
  return {
    id: page.id,
    sourceId: page.sourceId,
    sourcePageIndex: page.sourcePageIndex,
    pageSizePts: { ...page.pageSizePts },
    operations: cloneOperations(page.operations),
    selected: page.selected,
  };
}

/**
 * Restores a page from a snapshot (thumbnail needs to be regenerated)
 */
export function restorePageFromSnapshot(snapshot, thumbnail) {
  return {
    id: snapshot.id,
    sourceId: snapshot.sourceId,
    sourcePageIndex: snapshot.sourcePageIndex,
    pageSizePts: { ...snapshot.pageSizePts },
    operations: cloneOperations(snapshot.operations),
    thumbnail,
    selected: snapshot.selected,
  };
}

// ============================================
// Operation Types
// ============================================

export const OperationType = {
  ROTATE: 'rotate',
  SPLIT: 'split',
  COLOR_MODE: 'colorMode',
  REMOVE_SHADING: 'removeShading',
  ENHANCE_CONTRAST: 'enhanceContrast',
};

/**
 * Creates a rotate operation
 * @param {number} degrees - Rotation in degrees (90, 180, 270)
 */
export function createRotateOp(degrees = 90) {
  return { type: OperationType.ROTATE, degrees };
}

/**
 * Creates a split operation
 * @param {'left'|'right'} side - Which half to keep
 */
export function createSplitOp(side) {
  return { type: OperationType.SPLIT, side };
}

/**
 * Creates a color mode operation
 * @param {'color'|'gray'|'bw'|'bw-otsu'} mode
 */
export function createColorModeOp(mode) {
  return { type: OperationType.COLOR_MODE, mode };
}

/**
 * Creates a remove shading operation
 */
export function createRemoveShadingOp() {
  return { type: OperationType.REMOVE_SHADING };
}

/**
 * Creates an enhance contrast operation
 */
export function createEnhanceContrastOp() {
  return { type: OperationType.ENHANCE_CONTRAST };
}

// ============================================
// Operation Helpers
// ============================================

/**
 * Gets the effective color mode from operations list
 * Returns the last color mode operation, or 'color' if none
 */
export function getEffectiveColorMode(operations) {
  for (let i = operations.length - 1; i >= 0; i--) {
    if (operations[i].type === OperationType.COLOR_MODE) {
      return operations[i].mode;
    }
  }
  return 'color';
}

/**
 * Gets the total rotation from operations list (in degrees, 0-359)
 */
export function getTotalRotation(operations) {
  let total = 0;
  for (const op of operations) {
    if (op.type === OperationType.ROTATE) {
      total += op.degrees;
    }
  }
  return ((total % 360) + 360) % 360;
}

/**
 * Calculates the effective page size after operations
 */
export function getEffectivePageSize(pageSizePts, operations) {
  let { width, height } = pageSizePts;

  for (const op of operations) {
    if (op.type === OperationType.ROTATE && (op.degrees === 90 || op.degrees === 270)) {
      [width, height] = [height, width];
    } else if (op.type === OperationType.SPLIT) {
      width = width / 2;
    }
  }

  return { width, height };
}
