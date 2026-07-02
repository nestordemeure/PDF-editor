/**
 * Pass-through support for "classic" PDFs (pages with a real text layer).
 *
 * Instead of rasterizing such pages, the save path copies them verbatim with
 * pdf-lib (text, fonts and vectors preserved), applies rotation via the
 * native /Rotate key, and recompresses the images embedded in them.
 *
 * Image recompression is fail-safe by design: an image stream is only
 * replaced when we fully understand its encoding AND the re-encoded version
 * is smaller; everything else is left byte-for-byte untouched.
 */

import { OperationType } from "./pageModel.js";
import { encodeJpegBytes } from "./imagePixelOps.js";
import { inflate, deflate } from "../vendor/pako.mjs";

// qpdf WASM module instance, loaded lazily on the first encrypted PDF
let qpdfModulePromise = null;

async function getQpdfModule() {
  if (!qpdfModulePromise) {
    qpdfModulePromise = (async () => {
      const moduleUrl = new URL("../vendor/qpdf.mjs", import.meta.url);
      const { default: QPDF } = await import(moduleUrl.href);
      return QPDF({ noInitialRun: true, print: () => {}, printErr: () => {} });
    })();
  }
  return qpdfModulePromise;
}

/**
 * Strips the encryption from a PDF so pdf-lib can pass its pages through.
 * Only works for files with an empty user password (the common "DRM-only"
 * case — anything PDF.js opened without prompting). Returns the decrypted
 * bytes, or null when decryption fails.
 * @param {ArrayBuffer|Uint8Array} bytes
 * @returns {Promise<ArrayBuffer|null>}
 */
export async function tryDecryptPdf(bytes) {
  try {
    const qpdf = await getQpdfModule();
    qpdf.FS.writeFile("/input.pdf", bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes));
    let exitCode;
    try {
      exitCode = qpdf.callMain(["--decrypt", "--password=", "/input.pdf", "/output.pdf"]);
    } finally {
      qpdf.FS.unlink("/input.pdf");
    }
    // 0 = success, 3 = success with warnings (output still written)
    if (exitCode !== 0 && exitCode !== 3) return null;
    const output = qpdf.FS.readFile("/output.pdf");
    qpdf.FS.unlink("/output.pdf");
    return output.buffer;
  } catch (error) {
    console.warn("PDF decryption failed:", error);
    return null;
  }
}

/**
 * Operations that can be expressed natively on a preserved page.
 * Anything else (split, color mode, shading, contrast) forces rasterization
 * so the preview always matches the output.
 */
export function hasOnlyPreservationSafeOps(page) {
  return page.operations.every(op => op.type === OperationType.ROTATE);
}

/**
 * Net clockwise rotation (degrees, 0/90/180/270) from a page's operations
 */
export function netRotationDegrees(operations) {
  let total = 0;
  for (const op of operations) {
    if (op.type === OperationType.ROTATE) total += op.degrees;
  }
  return ((total % 360) + 360) % 360;
}

/**
 * Loads pdf-lib documents for every source that has pages eligible for
 * pass-through. Returns Map(sourceId -> PDFDocument | null); null means the
 * source can't be preserved (encrypted or unparseable) and its pages fall
 * back to the raster pipeline.
 */
export async function loadPreservableLibDocs({ pdfSources, pages, PDFDocument }) {
  const libDocs = new Map();
  const wanted = new Set(
    pages.filter(page => page.isClassic && hasOnlyPreservationSafeOps(page)).map(page => page.sourceId)
  );

  for (const sourceId of wanted) {
    const source = pdfSources.get ? pdfSources.get(sourceId) : pdfSources[sourceId];
    let libDoc = null;
    if (source && source.bytes && !source.encrypted) {
      try {
        // pdf-lib cannot decrypt; encrypted files throw here and fall back
        libDoc = await PDFDocument.load(source.bytes, { updateMetadata: false });
      } catch (error) {
        console.warn("Source not preservable, rasterizing its pages:", error);
        libDoc = null;
      }
    }
    libDocs.set(sourceId, libDoc);
  }
  return libDocs;
}

/**
 * True when this page can be copied verbatim instead of rasterized
 */
export function canPreservePage(page, libDocs) {
  return Boolean(page.isClassic && hasOnlyPreservationSafeOps(page) && libDocs.get(page.sourceId));
}

/**
 * Copies all preserved pages into targetDoc, batched per source so shared
 * resources (fonts, images) are copied once. Rotation is applied via the
 * native /Rotate key (added to any rotation the source page already has).
 * The returned pages are not yet in the target's page tree.
 * @returns {Promise<Map<number, PDFPage>>} final page index -> copied page
 */
export async function copyPreservedPages({ targetDoc, pages, preserved, libDocs }) {
  const { degrees } = window.PDFLib;

  const bySource = new Map();
  pages.forEach((page, finalIndex) => {
    if (!preserved[finalIndex]) return;
    if (!bySource.has(page.sourceId)) bySource.set(page.sourceId, []);
    bySource.get(page.sourceId).push({ finalIndex, page });
  });

  const copiedByFinalIndex = new Map();
  for (const [sourceId, entries] of bySource) {
    const copied = await targetDoc.copyPages(libDocs.get(sourceId), entries.map(e => e.page.sourcePageIndex));
    copied.forEach((pdfPage, i) => {
      const { finalIndex, page } = entries[i];
      const extra = netRotationDegrees(page.operations);
      if (extra !== 0) {
        const current = pdfPage.getRotation().angle || 0;
        pdfPage.setRotation(degrees((((current + extra) % 360) + 360) % 360));
      }
      copiedByFinalIndex.set(finalIndex, pdfPage);
    });
  }
  return copiedByFinalIndex;
}

// ============================================
// Text stripping
// ============================================

const CHAR = {
  LPAREN: 40, RPAREN: 41, LT: 60, GT: 62, LBRACKET: 91, RBRACKET: 93,
  LBRACE: 123, RBRACE: 125, SLASH: 47, PERCENT: 37, BACKSLASH: 92,
};

function isPdfWhitespace(c) {
  return c === 32 || c === 10 || c === 13 || c === 9 || c === 12 || c === 0;
}

function isPdfDelimiter(c) {
  return c === CHAR.LPAREN || c === CHAR.RPAREN || c === CHAR.LT || c === CHAR.GT ||
    c === CHAR.LBRACKET || c === CHAR.RBRACKET || c === CHAR.LBRACE || c === CHAR.RBRACE ||
    c === CHAR.SLASH || c === CHAR.PERCENT;
}

/**
 * Blanks (overwrites with spaces) every text-showing operator (Tj, TJ, ', ")
 * together with its operands in a content stream, leaving byte offsets — and
 * therefore everything else — untouched.
 * @returns {{bytes: Uint8Array, changed: boolean} | null} null = stream not
 *   understood (e.g. inline images); caller must keep the original.
 */
function blankTextShowingOps(data) {
  const out = data.slice();
  const n = data.length;
  let i = 0;
  let groupStart = -1; // start of the operands accumulated for the next operator
  let changed = false;

  while (i < n) {
    const c = data[i];
    if (isPdfWhitespace(c)) { i++; continue; }
    if (groupStart === -1) groupStart = i;

    if (c === CHAR.LPAREN) { // literal string
      i++;
      let depth = 1;
      while (i < n && depth > 0) {
        const ch = data[i];
        if (ch === CHAR.BACKSLASH) i += 2;
        else { if (ch === CHAR.LPAREN) depth++; else if (ch === CHAR.RPAREN) depth--; i++; }
      }
      continue;
    }
    if (c === CHAR.LT) { // hex string or dict open
      if (data[i + 1] === CHAR.LT) { i += 2; continue; }
      i++;
      while (i < n && data[i] !== CHAR.GT) i++;
      i++;
      continue;
    }
    if (c === CHAR.GT) { i += data[i + 1] === CHAR.GT ? 2 : 1; continue; } // dict close
    if (c === CHAR.SLASH) { // name
      i++;
      while (i < n && !isPdfWhitespace(data[i]) && !isPdfDelimiter(data[i])) i++;
      continue;
    }
    if (c === CHAR.PERCENT) { // comment
      while (i < n && data[i] !== 10 && data[i] !== 13) i++;
      continue;
    }
    if (c === CHAR.LBRACKET || c === CHAR.RBRACKET || c === CHAR.LBRACE || c === CHAR.RBRACE) {
      i++;
      continue;
    }

    // number or operator token
    const tokenStart = i;
    while (i < n && !isPdfWhitespace(data[i]) && !isPdfDelimiter(data[i])) i++;
    const first = data[tokenStart];
    const isNumber = (first >= 48 && first <= 57) || first === 43 || first === 45 || first === 46;
    if (isNumber) continue;

    const op = String.fromCharCode.apply(null, data.subarray(tokenStart, i));
    if (op === "BI") return null; // inline image: binary data would derail the scan
    if (op === "Tj" || op === "TJ" || op === "'" || op === '"') {
      out.fill(32, groupStart, i);
      changed = true;
    }
    groupStart = -1;
  }
  return { bytes: out, changed };
}

/**
 * Returns the decoded bytes of a content/form stream, or null when the
 * stream uses a filter we don't handle
 */
function decodeSimpleStream(context, stream) {
  const { PDFName, PDFRawStream } = window.PDFLib;
  if (!(stream instanceof PDFRawStream)) return null;
  if (stream.dict.get(PDFName.of("DecodeParms"))) return null;
  const filter = stream.dict.get(PDFName.of("Filter"));
  if (!filter) return stream.getContents();
  if (singleFilterName(context, stream.dict) !== "/FlateDecode") return null;
  try {
    return inflate(stream.getContents());
  } catch (e) {
    return null;
  }
}

/**
 * Blanks the text ops of the stream at `ref` and swaps in the edited copy,
 * keeping every other dict entry. No-op when the stream isn't understood.
 */
function stripTextFromStreamRef(context, ref) {
  const { PDFName, PDFNumber, PDFRawStream } = window.PDFLib;
  const stream = context.lookup(ref);
  const decoded = decodeSimpleStream(context, stream);
  if (!decoded) return;
  const result = blankTextShowingOps(decoded);
  if (!result || !result.changed) return;

  const deflated = deflate(result.bytes);
  const newDict = stream.dict.clone(context);
  newDict.set(PDFName.of("Filter"), PDFName.of("FlateDecode"));
  newDict.set(PDFName.of("Length"), PDFNumber.of(deflated.length));
  context.assign(ref, PDFRawStream.of(newDict, deflated));
}

/**
 * Recursively strips text from the Form XObjects of a resources dict
 */
function stripTextFromFormXObjects(context, resources, seen, depth = 0) {
  const { PDFName, PDFDict, PDFRef } = window.PDFLib;
  if (!resources || depth > 4) return;
  const xobjects = context.lookup(resources.get(PDFName.of("XObject")));
  if (!(xobjects instanceof PDFDict)) return;
  for (const [, value] of xobjects.entries()) {
    if (!(value instanceof PDFRef) || seen.has(value.toString())) continue;
    seen.add(value.toString());
    const stream = context.lookup(value);
    if (!stream || !stream.dict) continue;
    const subtype = asName(context, stream.dict.get(PDFName.of("Subtype")));
    if (subtype !== "/Form") continue;
    stripTextFromStreamRef(context, value);
    const nested = context.lookup(stream.dict.get(PDFName.of("Resources")));
    if (nested instanceof PDFDict) stripTextFromFormXObjects(context, nested, seen, depth + 1);
  }
}

/**
 * Removes all text (visible and invisible) from the given pages of a PDF,
 * keeping images and vector graphics untouched. Used at load time so
 * thumbnails, preview and save all see the stripped document.
 * @returns {Promise<Uint8Array|null>} new PDF bytes, or null on failure
 */
export async function stripTextFromPdf(bytes, pageIndices, PDFDocument) {
  const { PDFName, PDFArray, PDFRef, PDFDict } = window.PDFLib;
  try {
    const doc = await PDFDocument.load(bytes, { updateMetadata: false });
    const context = doc.context;
    const seenForms = new Set();

    for (const pageIndex of pageIndices) {
      const page = doc.getPage(pageIndex);
      const contents = page.node.get(PDFName.of("Contents"));
      const resolved = context.lookup(contents);
      if (contents instanceof PDFRef && !(resolved instanceof PDFArray)) {
        stripTextFromStreamRef(context, contents);
      } else if (resolved instanceof PDFArray) {
        for (let i = 0; i < resolved.size(); i++) {
          const entry = resolved.get(i);
          if (entry instanceof PDFRef) stripTextFromStreamRef(context, entry);
        }
      }
      const resources = page.node.Resources();
      if (resources instanceof PDFDict) stripTextFromFormXObjects(context, resources, seenForms);
    }

    return await doc.save({ useObjectStreams: true });
  } catch (error) {
    console.warn("Text stripping failed:", error);
    return null;
  }
}

// ============================================
// In-place image recompression
// ============================================

function asName(context, value) {
  const resolved = context.lookup(value);
  const { PDFName } = window.PDFLib;
  return resolved instanceof PDFName ? resolved.asString() : null;
}

function asNumber(context, value) {
  const resolved = context.lookup(value);
  const { PDFNumber } = window.PDFLib;
  return resolved instanceof PDFNumber ? resolved.asNumber() : null;
}

/**
 * Resolves the single filter name of an image stream ("/DCTDecode" etc.),
 * or null when there are zero or multiple filters.
 */
function singleFilterName(context, dict) {
  const { PDFName, PDFArray } = window.PDFLib;
  const filter = context.lookup(dict.get(PDFName.of("Filter")));
  if (filter instanceof PDFName) return filter.asString();
  if (filter instanceof PDFArray && filter.size() === 1) return asName(context, filter.get(0));
  return null;
}

/**
 * Undoes PNG row predictors (types 0-4) on 8-bit data.
 * Returns the unfiltered samples, or null if the data is malformed.
 */
function undoPngPredictor(data, columns, colors) {
  const rowBytes = columns * colors;
  const rows = Math.floor(data.length / (rowBytes + 1));
  if (rows * (rowBytes + 1) !== data.length) return null;

  const out = new Uint8Array(rowBytes * rows);
  const bpp = colors;
  for (let y = 0; y < rows; y++) {
    const filter = data[y * (rowBytes + 1)];
    const src = y * (rowBytes + 1) + 1;
    const dst = y * rowBytes;
    const prev = dst - rowBytes;
    for (let x = 0; x < rowBytes; x++) {
      const raw = data[src + x];
      const left = x >= bpp ? out[dst + x - bpp] : 0;
      const up = y > 0 ? out[prev + x] : 0;
      let value;
      if (filter === 0) value = raw;
      else if (filter === 1) value = raw + left;
      else if (filter === 2) value = raw + up;
      else if (filter === 3) value = raw + ((left + up) >> 1);
      else if (filter === 4) {
        const upLeft = y > 0 && x >= bpp ? out[prev + x - bpp] : 0;
        const p = left + up - upLeft;
        const pa = Math.abs(p - left);
        const pb = Math.abs(p - up);
        const pc = Math.abs(p - upLeft);
        value = raw + (pa <= pb && pa <= pc ? left : pb <= pc ? up : upLeft);
      } else return null;
      out[dst + x] = value & 0xff;
    }
  }
  return out;
}

/**
 * Undoes TIFF predictor 2 (horizontal differencing) on 8-bit data, in place
 */
function undoTiffPredictor(data, columns, colors) {
  const rowBytes = columns * colors;
  for (let row = 0; row + rowBytes <= data.length; row += rowBytes) {
    for (let x = colors; x < rowBytes; x++) {
      data[row + x] = (data[row + x] + data[row + x - colors]) & 0xff;
    }
  }
  return data;
}

/**
 * Inspects an image XObject and, when its encoding is fully understood,
 * returns { imageData, isGray, srcLength }; returns null (skip) otherwise.
 */
async function decodeEligibleImage(context, stream) {
  const { PDFName, PDFDict, PDFRawStream } = window.PDFLib;
  if (!(stream instanceof PDFRawStream)) return null;
  const dict = stream.dict;
  const get = key => dict.get(PDFName.of(key));

  if (asName(context, get("Subtype")) !== "/Image") return null;
  // Any mask, custom decode range or image-mask semantics -> hands off
  if (get("SMask") || get("Mask") || get("Decode") || get("ImageMask")) return null;
  if (asNumber(context, get("BitsPerComponent")) !== 8) return null;

  const colorSpace = asName(context, get("ColorSpace"));
  if (colorSpace !== "/DeviceRGB" && colorSpace !== "/DeviceGray") return null;
  const isGray = colorSpace === "/DeviceGray";
  const channels = isGray ? 1 : 3;

  const width = asNumber(context, get("Width"));
  const height = asNumber(context, get("Height"));
  if (!width || !height || width * height > 50e6) return null;

  const filter = singleFilterName(context, dict);
  const contents = stream.getContents();

  if (filter === "/DCTDecode") {
    if (get("DecodeParms")) return null;
    let bitmap;
    try {
      bitmap = await createImageBitmap(new Blob([contents], { type: "image/jpeg" }));
    } catch (e) {
      return null; // CMYK or otherwise browser-undecodable JPEG
    }
    if (bitmap.width !== width || bitmap.height !== height) {
      bitmap.close();
      return null;
    }
    const canvas = new OffscreenCanvas(width, height);
    const ctx = canvas.getContext("2d");
    ctx.drawImage(bitmap, 0, 0);
    bitmap.close();
    return { imageData: ctx.getImageData(0, 0, width, height), isGray, srcLength: contents.length };
  }

  if (filter === "/FlateDecode") {
    const parms = context.lookup(get("DecodeParms"));
    let predictor = 1;
    if (parms) {
      if (!(parms instanceof PDFDict)) return null;
      const parmGet = key => parms.get(PDFName.of(key));
      predictor = asNumber(context, parmGet("Predictor")) ?? 1;
      const parmColors = asNumber(context, parmGet("Colors")) ?? 1;
      const parmBpc = asNumber(context, parmGet("BitsPerComponent")) ?? 8;
      const parmColumns = asNumber(context, parmGet("Columns")) ?? 1;
      if (parmColors !== channels || parmBpc !== 8) return null;
      if (predictor !== 1 && parmColumns !== width) return null;
    }

    let samples;
    try {
      samples = inflate(stream.getContents());
    } catch (e) {
      return null;
    }
    if (predictor >= 10 && predictor <= 15) {
      samples = undoPngPredictor(samples, width, channels);
      if (!samples) return null;
    } else if (predictor === 2) {
      samples = undoTiffPredictor(samples, width, channels);
    } else if (predictor !== 1) {
      return null;
    }
    if (samples.length < width * height * channels) return null;

    const rgba = new Uint8ClampedArray(width * height * 4);
    for (let i = 0, n = width * height; i < n; i++) {
      const s = i * channels;
      rgba[i * 4] = samples[s];
      rgba[i * 4 + 1] = samples[isGray ? s : s + 1];
      rgba[i * 4 + 2] = samples[isGray ? s : s + 2];
      rgba[i * 4 + 3] = 255;
    }
    return { imageData: new ImageData(rgba, width, height), isGray, srcLength: contents.length };
  }

  return null;
}

/**
 * Recompresses the images referenced by the given preserved pages, replacing
 * each stream in place (so images shared between pages are swapped once and
 * everywhere) only when the JPEG re-encode is strictly smaller.
 * @returns {Promise<number>} number of images replaced
 */
export async function recompressPreservedImages({ pdfDoc, pdfPages, jpegQuality, onStatus, yieldToUi }) {
  const { PDFName, PDFDict, PDFRef } = window.PDFLib;
  const context = pdfDoc.context;

  // Collect unique image refs across all preserved pages
  const refs = [];
  const seen = new Set();
  for (const page of pdfPages) {
    const resources = page.node.Resources();
    if (!resources) continue;
    const xobjects = context.lookup(resources.get(PDFName.of("XObject")));
    if (!(xobjects instanceof PDFDict)) continue;
    for (const [, value] of xobjects.entries()) {
      if (value instanceof PDFRef && !seen.has(value.toString())) {
        seen.add(value.toString());
        refs.push(value);
      }
    }
  }

  let replaced = 0;
  for (let i = 0; i < refs.length; i++) {
    if (onStatus) onStatus(`Recompressing embedded images ${i + 1}/${refs.length}`);

    const stream = context.lookup(refs[i]);
    let decoded;
    try {
      decoded = await decodeEligibleImage(context, stream);
    } catch (e) {
      decoded = null;
    }
    if (!decoded) continue;

    const { imageData, isGray, srcLength } = decoded;
    const newBytes = await encodeJpegBytes(imageData, jpegQuality, isGray);
    if (newBytes.length >= srcLength) continue;

    const dict = {
      Type: "XObject",
      Subtype: "Image",
      Width: imageData.width,
      Height: imageData.height,
      ColorSpace: isGray ? "DeviceGray" : "DeviceRGB",
      BitsPerComponent: 8,
      Filter: "DCTDecode",
    };
    if (stream.dict.get(PDFName.of("Interpolate"))) dict.Interpolate = true;
    context.assign(refs[i], context.stream(newBytes, dict));
    replaced += 1;

    if (yieldToUi) await yieldToUi();
  }
  return replaced;
}
