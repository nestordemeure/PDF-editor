# PDF Editor

In-browser cleanup tool for scanned books. Everything runs client-side: no server, no upload.

Open the [GitHub Pages version](https://nestordemeure.github.io/PDF-editor/), or run `./serve.sh` and open <http://localhost:8000> for local testing.

It runs the same engine as the native Linux version in [`desktop/`](desktop/README.md), compiled to WebAssembly, so both make the same files. The native version is faster (about three times with OCR) and handles bigger books.

## What it does

- Load one or more PDFs and merge them; loading again appends to the current pages.
- Reorder pages by dragging; select pages to edit (or select all).
- Rotate 90°, split double pages down the middle, delete pages.
- Clean up scans: grayscale, black & white (flat or adaptive), shadow removal, contrast enhancement.
- Keep typeset pages as text (even when rotated or split), strip their text, or rasterize them. Encrypted files that open without a password work too.
- Undo / redo.
- Optional OCR with Tesseract that adds an invisible, selectable text layer.
- Export a compact PDF: at "No compression" scans pass through untouched and grayscale keeps the JPEG's own data; B&W pages use lossless JBIG2; other pages use JPEG, at three compression levels.

## Usage

1. **Load PDFs** and wait for the thumbnails.
2. Select pages, then apply operations from the left sidebar. Click a page to preview it — the preview shows exactly what will be exported.
3. Pick a compression level and OCR language, then **Save PDF**.

A typical book scan: select all, B&W (adaptive), Remove Shadows, high compression, OCR — a few hundred pages come out at ~25 KB per page with selectable text.

## Development

The page has no build step. The engine in `src/engine/` is built from the Rust code with `desktop/web/build.sh`, which needs Rust with the `wasm32-unknown-unknown` target and `wasm-bindgen-cli` at the version in `desktop/web/Cargo.toml`.