# PDF Editor

In-browser cleanup tool for scanned books. Everything runs client-side: no server, no upload.

Open the [GitHub Pages version](https://nestordemeure.github.io/PDF-editor/), or run `./serve.sh` and open <http://localhost:8000> for local testing.

## What it does

- Load one or more PDFs and merge them.
- Reorder pages by dragging; select pages to edit (or select all).
- Rotate 90°, split double pages down the middle, delete pages.
- Clean up scans: grayscale, black & white (flat or adaptive), shadow removal, contrast enhancement.
- Undo / redo.
- Optional OCR (17 languages) that adds an invisible, selectable text layer.
- Export a compact PDF: B&W pages are CCITT G4 compressed, grayscale/color pages use MozJPEG, with four compression levels from lossless to high.

## Usage

1. **Load PDFs** and wait for the thumbnails.
2. Select pages, then apply operations from the left sidebar. Click a page to preview it — the preview shows exactly what will be exported.
3. Pick a compression level and OCR language, then **Save PDF**.

A typical book scan: select all, B&W (adaptive), Remove Shadows, high compression, OCR — a few hundred pages come out at ~25 KB per page with selectable text.
