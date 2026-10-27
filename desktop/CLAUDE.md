# CLAUDE.md (desktop app)

Native Linux rewrite of the web app in Rust. The GUI uses egui (eframe). The command-line tool `pdf-editor-cli` runs the same engine. Use the CLI for tests and benchmarks: see the `pdf-editor-cli` skill in `.claude/skills/`.

## Build

```bash
cargo build --release      # binaries: pdf-editor (GUI), pdf-editor-cli
cargo test --release
./install.sh               # menu entry + ~/.local/bin symlinks to the release build
```

- The target directory must have no spaces: MuPDF's Makefile fails on them. The repository path has a space, so `desktop/.cargo/config.toml` (untracked) sets `target-dir = "/home/nestor/.cache/cargo-target/pdf-editor"`.
- This machine has `libclang` without its headers. The same config sets `BINDGEN_EXTRA_CLANG_ARGS = "-I/usr/lib/gcc/x86_64-linux-gnu/13/include"`.
- All native code is static: MuPDF (`mupdf` crate), Tesseract and Leptonica (`tesseract-rs` builds them). Only the Tesseract language files are on disk.

## Code organization

| File | Role |
|---|---|
| `src/model.rs` | Page, view (crop and quarter turns), color mode, compression levels and their DPI and JPEG quality |
| `src/geometry.rs` | PDF matrices and rectangles |
| `src/source.rs` | Loads a PDF with lopdf (decrypts with the empty password), page geometry, text-page classification, text stripping |
| `src/content.rs` | Content stream walker (what a page paints, the scan image and its placement) and the byte-exact text blanker |
| `src/pdfimage.rs` | Inspects and decodes image XObjects: JPEG, Flate 8-bit, Flate 1-bit, gray or RGB only |
| `src/pixel.rs` | `Raster` and the pixel pipeline (`apply_pixel_ops`): shading removal, contrast, gray, Otsu, Sauvola |
| `src/encode.rs` | zenjpeg encode, decode and lossless repack; JBIG2 (lossless generic); Flate with PNG predictors |
| `src/render.rs` | MuPDF rendering with per-thread documents |
| `src/ocr.rs` | Tesseract C API (statically linked) and the invisible text layer |
| `src/pipeline.rs` | One raster page to an output image and text layer; `preview` for the UI |
| `src/copy.rs` | Copies object graphs from a source into the output |
| `src/writer.rs` | Streaming PDF writer with object streams and an xref stream |
| `src/save.rs` | Save orchestration: worker pool, bounded channel, page tree, OCR font |
| `src/gui/` | egui app: `document.rs` (state, undo), `thumbnails.rs` (background renders), `jobs.rs` (load and save threads), `mod.rs` (layout) |

Dependency direction: `gui` → `save` → `pipeline` → `pixel`/`encode`/`render`/`ocr`/`pdfimage` → `model`/`geometry`. The library (`src/lib.rs`) has no UI code.

## Page routes at save

- **Copied**: a text page with no pixel operations. The page objects are copied. The view becomes `/CropBox` and `/Rotate`, so split and rotation keep the text.
- **PassThrough**: a scan page with no pixel operations, at "No compression" or when re-encoding is not smaller. The original image stream is reused. A lossless repack replaces it only when strictly smaller.
- **Reencoded**: the scan image is decoded at its own resolution, cropped, downsampled to the level's DPI, processed and encoded.
- **Rendered**: MuPDF renders the page at the resolution of its main image (the largest, if it covers half the page), else 300 DPI, capped at 50 Mpx; then as above. At "No compression" it uses that image's JPEG quality, so rendered scans do not grow.

## Invariants

- **Preview must equal output.** Thumbnails and saves both call `pixel::apply_pixel_ops`. Every pixel operation takes `scale` (resolution / 300 DPI) so its result does not depend on the working resolution.
- **Pixel operations run on the image in its own orientation.** Rotation and split only change the placement matrix. Only OCR rotates pixels (to upright).
- **Memory is bounded by pages in flight.** Workers send finished pages through a `sync_channel` of capacity = threads; the writer puts each object on disk at once. Do not collect per-page results in memory.
- **MuPDF documents live in thread-locals.** Call `render::close_documents()` on every pool thread (`pool.broadcast`) before the pool drops. Else the process aborts at thread exit.
- **Tesseract's C API defaults to one text block.** `Engine::new` sets `PSM_AUTO`. Without it, columns merge into one line.
- **Fail loudly.** A page that fails stops the save with its page number. Designed fallbacks are only the equal-quality ones: keep the original stream when a re-encode is not smaller, render with MuPDF when an image codec is not decodable.
- **JPEG quality numbers are libjpeg numbers.** `encode::encode_jpeg` maps them to zenjpeg settings of equal fidelity (`zenjpeg_setting`). Do not pass libjpeg numbers to zenjpeg directly.

## Testing

```bash
B=~/.cache/cargo-target/pdf-editor/release/pdf-editor-cli
$B --info "../test data/input.pdf"                     # classification per page
$B "../test data/input.pdf" -o /tmp/out.pdf --compression high --mode gray
qpdf --check /tmp/out.pdf; pdfimages -list /tmp/out.pdf; pdftotext /tmp/out.pdf -
```

Test files in `../test data/` (not in git): `input.pdf` (11-page scan with an OCR layer), `classic.pdf` (53-page typeset R NEWS), `classic-encrypted.pdf` (RC4), `classic-aes256.pdf` (AES-256), `book330.pdf` (30 distinct copies of `input.pdf`, for benchmarks). Benchmark against `ocrmypdf -O3 --redo-ocr`.
