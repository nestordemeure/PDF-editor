# CLAUDE.md

In-browser PDF cleanup tool for scanned books. Vanilla JS ES modules, no build step: serve the directory statically (`./serve.sh`) and open `index.html`. Big libraries (PDF.js, pdf-lib, Sortable) come from CDN script tags; small codecs are vendored under `vendor/`.

## Code organization

App code lives in `src/`; `index.html`, `style.css`, and `serve.sh` stay at the root (GitHub Pages serves the repo root). `test data/` holds a sample scan for testing.

| File | Role |
|---|---|
| `src/app.js` | Entry point: UI wiring, app state (`pages`, `sourcePdfs`), undo/redo history, incremental page-grid rendering, load/save handlers |
| `src/pageModel.js` | Page objects and the operation list model (`rotate`, `split`, `colorMode`, `removeShading`, `enhanceContrast`) |
| `src/pageCommands.js` | Commands that edit operation lists and update thumbnails; `forEachConcurrent` helper |
| `src/pageRenderer.js` | PDF.js page rendering, base-thumbnail cache, geometric canvas ops, `applyOperationsToCanvas` (the canonical pipeline) |
| `src/imagePixelOps.js` | DOM-free pixel operations on `ImageData` + `encodeProcessedImage` (G4 / raw-gray / MozJPEG / PNG) |
| `src/saveWorker.js` | Module worker: runs the pixel pipeline + encoding per page |
| `src/saveManager.js` | Save orchestration: render pages, worker pool, OCR (scribe.js), PDF assembly with pdf-lib |

Dependency direction: `app.js` → `pageCommands.js`/`saveManager.js` → `pageRenderer.js` → `imagePixelOps.js` → `pageModel.js` + `vendor/`. Keep it acyclic and keep DOM access out of `imagePixelOps.js`.

## Architecture

Editing is **non-destructive**: pages store a reference to source PDF bytes plus an operation list. Thumbnails (300px) preview the operations; full-resolution rendering happens only at save time, at a DPI chosen by color mode × compression level.

The **canonical operation order** is: geometric (rotate/split, in click order) → remove shading → enhance contrast → color mode (binarization last, on real gray values). Thumbnails and export share this pipeline (`applyOperationsToCanvas` / `applyPixelPipeline`), which is what guarantees the preview matches the output.

At save time, PDF.js renders on the main thread (pipelined, `SAVE_RENDER_CONCURRENCY`) while a pool of module workers does pixel processing and encoding, with an inline fallback if workers fail.

## Invariants — breaking these causes subtle bugs

- **Thumbnail canvases are replaced, never mutated in place.** Undo history snapshots and the page-grid card cache hold them by reference.
- **Pixel ops stay DOM-free** (functions on `ImageData` in `imagePixelOps.js`) so they run in workers. Encoding uses `OffscreenCanvas`.
- **Never embed B&W/gray pages via `embedPng`**: pdf-lib decodes PNGs and re-embeds them as 24-bit RGB (~2.6× larger). Use the low-level path in `drawRenderedImage` (`context.stream` + `newXObject` + `pushOperators`).
- **Flate-compress raw images in the encode step (in the worker), never at embed time** — lossless saves of a long book would otherwise hold gigabytes of raw pixels.
- **OCR embedding relies on a pdf-lib quirk**: `page.scaleContent()` caches the content stream *inside* the scale wrapper, so images drawn afterwards share the transform — that's why the image is drawn at the OCR page's original size, not the target size. Don't "simplify" this.
- The base-thumbnail cache (`baseThumbnailCache`) must hold the **pre-operation** render; it is what lets pixel-effect changes and deep undo skip PDF.js entirely.

## Encodings

- B&W: CCITT G4 (`vendor/ccitt-g4-encoder.mjs`), fallback packed 1-bit + Flate. No maintained JBIG2 *encoder* exists on npm (checked 2026-07); compiling jbig2enc to WASM would be the next ~2× win.
- Grayscale/color JPEG: MozJPEG WASM (`vendor/jsquash-jpeg/`), single-channel for grayscale, fallback `canvas.toBlob`.
- Lossless ("No Compression"): raw 8-bit DeviceGray + Flate for gray, PNG for color.
- PDF.js document striping (3 docs/file for parallel decode) was tried and reverted: ~2 s gain on 330 pages, not worth it.

## Vendored dependencies

Update with `npm install <pkg>@latest` then copy per `package.json` deps:

```bash
cp node_modules/ts-ccitt-g4-encoder/dist/index.mjs vendor/ccitt-g4-encoder.mjs
cp node_modules/pako/dist/pako.mjs vendor/pako.mjs
cp node_modules/@jsquash/jpeg/{encode.js,meta.js,utils.js} vendor/jsquash-jpeg/
cp node_modules/@jsquash/jpeg/codec/enc/mozjpeg_enc.{js,wasm} vendor/jsquash-jpeg/codec/enc/
# scribe.js OCR (AGPL): scribe.js + js/ lib/ tess/ fonts/ mupdf/ -> vendor/
```

Copy each package's LICENSE alongside (see existing `vendor/LICENSE-*` files).

## Development priorities

1. **Correctness**: preview must match output; never silently degrade image quality or grow file size — verify sizes stay equal-or-smaller on the test files.
2. **Output size**: small gains are worth pursuing — books are hundreds of pages, each an image.
3. **Responsiveness**: the UI must stay live during long operations (workers, throttled yields, progress in the status bar). Whole-book interactive operations should stay in low seconds.
4. Scale target: hundreds of pages per book. Benchmark claims with a real-size document, not the 11-page sample.

## Testing

No test suite; verify end-to-end headlessly: install `playwright` in a scratch dir (`PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1`, launch with `channel: 'chrome'`), serve the app with `python3 -m http.server`, upload `test data/input.pdf` (11-page book scan) via `#fileInput`, drive the buttons, and capture the download. Validate outputs with `qpdf --check`, `pdfimages -list` (bpc/colorspace/size per page), `pdftoppm` (visual), and `pdftotext -bbox` (OCR alignment). For performance claims, concatenate the sample into a ~330-page PDF with `qpdf` first.
