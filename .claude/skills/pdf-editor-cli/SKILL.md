---
name: pdf-editor-cli
description: Run the desktop PDF editor engine from the command line (pdf-editor-cli) to process PDFs, check page classification, validate outputs, and benchmark size, speed and memory against ocrmypdf. Use when you test or measure the Rust app in desktop/.
---

# pdf-editor-cli

The CLI runs the same engine as the desktop app. Use it to test changes without the GUI.

## Build

```bash
cd desktop && cargo build --release
B=~/.cache/cargo-target/pdf-editor/release/pdf-editor-cli
```

## Commands

```bash
$B --info IN.pdf                                   # classification per page: TEXT, SCAN or RENDER
$B IN.pdf [MORE.pdf ...] -o OUT.pdf                # defaults: --compression high, scans gray, --ocr eng
$B IN.pdf -o OUT.pdf --compression none --mode color --ocr none   # pass-through
$B IN.pdf -o OUT.pdf --mode bw-adaptive --remove-shading --enhance-contrast
$B IN.pdf -o OUT.pdf --split --rotate 1            # split every page, then a quarter turn
$B IN.pdf -o OUT.pdf --text strip                  # keep | strip | rasterize text pages
$B IN.pdf -o OUT.pdf --threads 8                   # 0 = one per logical CPU
$B IN.pdf -o OUT.pdf --compression low --mode color --dpi 600 --jpeg-quality 75   # override the level (benchmarks)
```

- `--mode` applies to scan pages. If you give it, it also applies to text pages, which then rasterize.
- `-o DIR` writes the default file name into DIR.
- The last line reports pages, input and output bytes, load and save time, peak RSS, page routes (`Copied`, `PassThrough`, `ExactGray`, `Reencoded`, `Rendered`) and recompressed images.
- `--jpeg-quality` uses the libjpeg scale; the engine maps it to zenjpeg settings of equal fidelity.

## Validate an output

```bash
qpdf --check OUT.pdf               # structure; any WARNING (e.g. "JPEG data is corrupt") is a bug
pdfimages -list OUT.pdf            # codec, bits, resolution, size per image
pdftoppm -f 2 -l 2 -r 72 -png OUT.pdf /tmp/p    # visual check
pdftotext -f 2 -l 2 OUT.pdf -       # OCR text and reading order
pdftotext -bbox OUT.pdf /tmp/b.html # word boxes, to check OCR alignment
pdffonts OUT.pdf                    # text pages must keep their fonts
```

## Benchmark

1. Use `test data/book330.pdf` (330 pages). If it is missing, make 30 distinct copies of `test data/input.pdf` and join them with `qpdf --empty --pages c1.pdf ... c30.pdf -- book330.pdf`. Do not pass the same file 30 times: qpdf then shares the images and the test is not real.
2. Do not run two benchmarks or a compile at the same time.
3. Run the reference and the CLI with `/usr/bin/time -f "%e s %M KB"`:

```bash
/usr/bin/time -f "%e s %M KB" ocrmypdf -O3 --redo-ocr -q book330.pdf ref.pdf
/usr/bin/time -f "%e s %M KB" $B book330.pdf -o ours.pdf --compression none --mode color
```

4. Compare output size, wall time and peak memory. Also compare the OCR text with `pdftotext`.
