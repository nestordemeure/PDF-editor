# PDF Editor (desktop)

A native Linux version of the [web app](../README.md) for cleaning up scanned books: the same features, rewritten in Rust for speed, bounded memory and smaller files. It ships as a graphical app and as a command-line tool with the same engine.

## What it does

- Loads and merges PDFs, including encrypted ones that open without a password.
- Reorders pages by dragging; rotates, splits double pages and deletes pages, with undo and redo.
- Cleans up scans: grayscale, black and white (Otsu or Sauvola), shadow removal and contrast enhancement.
- "Sharp text" modes split a scan into a full-resolution text layer and low-resolution image layers: text stays as sharp as the scan, pictures look as in grayscale or color, and files are about half the size of grayscale or less.
- Keeps typeset pages as text (even when rotated or split), strips their text, or rasterizes them.
- Adds an invisible OCR text layer with Tesseract, built into the binary; missing languages download on first use.
- Writes small files: at "No compression" original scan images pass through untouched and grayscale keeps the JPEG's own luma data; black and white pages and text layers use lossless JBIG2; other pages use JPEG from [zenjpeg](https://github.com/imazen/zenjpeg).

## Install

Needs Rust, a C/C++ compiler, CMake and libclang. Tesseract language files are read from `/usr/share/tesseract-ocr` when present.

```bash
cd desktop
./install.sh     # builds, then adds "PDF Editor" to the app menu
```

The build directory must not contain spaces (MuPDF's Makefile breaks on them). Set `CARGO_TARGET_DIR` or `build.target-dir` in `.cargo/config.toml` if the checkout path has one.

## Usage

Open **PDF Editor** from the menu, or `pdf-editor book.pdf`. Load PDFs, select pages, apply operations from the left panel, then pick a compression level and OCR language and save. Shortcuts: Ctrl+O, Ctrl+S, Ctrl+A, Ctrl+Z, Ctrl+Shift+Z, Delete, R.

```bash
pdf-editor-cli scan.pdf -o out.pdf --compression high --mode bw-adaptive --remove-shading --ocr eng
pdf-editor-cli book.pdf -o out.pdf --compression none --mode color   # keep the original images
pdf-editor-cli --info book.pdf                                          # how each page is classified
```

## Performance

Measured on a 20-thread laptop with OCR in English, against `ocrmypdf -O3 --redo-ocr`.

| Input | ocrmypdf -O3 | PDF Editor |
|---|---|---|
| 330-page scan, original color kept | 272 s, 4.4 GB peak | 32 s, 1.6 GB peak |
| 11-page scan at the same resolution and better fidelity (38.1 dB against 37.1 dB) | 3.74 MB, 9.1 s | 3.54 MB, 2.2 s |
| 255-page book scan | killed for lack of memory (12.5 GB) | 17 s, 1.25 GB peak, 30% of input at high compression |
