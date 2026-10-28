# CLAUDE.md (desktop app)

The PDF editor's engine in Rust, with a native Linux app. The GUI uses egui (eframe). The command-line tool `pdf-editor-cli` runs the same engine. Use the CLI for tests and benchmarks: see the `pdf-editor-cli` skill in `.claude/skills/`. The web app runs this engine as WebAssembly (`web/`, see `../CLAUDE.md`).

## Build

```bash
cargo build --release      # binaries: pdf-editor (GUI), pdf-editor-cli
cargo test --release
./install.sh               # menu entry + ~/.local/bin symlinks to the release build
```

- The target directory must have no spaces: MuPDF's Makefile fails on them. The repository path has a space, so `desktop/.cargo/config.toml` (untracked) sets `target-dir = "/home/nestor/.cache/cargo-target/pdf-editor"`.
- This machine has `libclang` without its headers. The same config sets `BINDGEN_EXTRA_CLANG_ARGS = "-I/usr/lib/gcc/x86_64-linux-gnu/13/include"`.
- All native code is static: MuPDF (`mupdf` crate), Tesseract and Leptonica (`tesseract-rs` builds them). Only the Tesseract language files are on disk.
- The `native` feature (default) adds MuPDF, Tesseract, threads, files and the GUI. Without it the library builds for `wasm32-unknown-unknown`: `cargo check --lib --no-default-features --target wasm32-unknown-unknown`. `web/build.sh` builds the web engine into `../src/engine/`.

## Code organization

| File | Role |
|---|---|
| `src/model.rs` | Page, view (crop and quarter turns), color mode, compression levels and their DPI and JPEG quality |
| `src/geometry.rs` | PDF matrices and rectangles |
| `src/source.rs` | Loads a PDF with lopdf (decrypts with the empty password), page geometry, text-page classification, text stripping |
| `src/content.rs` | Content stream walker (what a page paints, the scan image and its placement) and the byte-exact text blanker |
| `src/pdfimage.rs` | Inspects and decodes image XObjects: JPEG, Flate 8-bit, Flate 1-bit, gray or RGB only |
| `src/pixel.rs` | `Raster` and the pixel pipeline (`apply_pixel_ops`): shading removal, contrast, gray, Otsu, Sauvola |
| `src/jpeggray.rs` | Exact grayscale: luma DCT coefficients of a YCbCr JPEG written as a one-channel JPEG (own baseline Huffman writer) |
| `src/encode.rs` | zenjpeg encode and decode, Flate wrapping of JPEGs; JBIG2 (lossless generic); Flate with PNG predictors |
| `src/render.rs` | MuPDF rendering with per-thread documents |
| `src/ocr.rs` | Tesseract C API (statically linked), language files (native) |
| `src/textlayer.rs` | The invisible text layer from OCR lines, and its font |
| `src/pipeline.rs` | `page_job` reads what a raster page needs from its source; `run_page_job` makes its output image and OCR input; image jobs for text pages; `preview` for the UI |
| `src/textcrop.rs` | Removes text runs outside a cropped text page (own byte-preserving tokenizer, glyph widths from the fonts) |
| `src/copy.rs` | Copies object graphs from a source into the output |
| `src/writer.rs` | Streaming PDF writer with object streams and an xref stream |
| `src/assemble.rs` | Builds the output: copied text pages, raster pages, page tree. No threads |
| `src/save.rs` | Native save orchestration: worker pool, bounded channel, temporary file (native) |
| `web/` | The engine's JavaScript interface for the web app (`wasm-bindgen`) |
| `src/gui/` | egui app: `document.rs` (state, undo), `thumbnails.rs` (background renders), `jobs.rs` (load and save threads), `mod.rs` (layout) |

Dependency direction: `gui` → `save` → `assemble`/`pipeline` → `pixel`/`encode`/`render`/`ocr`/`pdfimage` → `model`/`geometry`. The library (`src/lib.rs`) has no UI code.

## Page routes at save

`pipeline::plan` decides the intended route without decoding pixels; `pipeline::intended_route` exposes it (the GUI status bar shows the counts). Keep the decision there, not in callers.


- **Copied**: a text page with no pixel operations. The page objects are copied. The view becomes `/MediaBox`, `/CropBox` and `/Rotate`, so split and rotation keep the text. Cropped text pages (`save::filter_cropped_text`) get their own content without what lies wholly outside them: text runs with their positioning (the next kept run is re-placed with an absolute `TL`/`Tm`), emptied text objects (their text state is restored before the next one), paths, images and forms. All crops of a source page are decided together, and accepted only when their contents add up to at most 110% of the shared original (real spreads); else the shared content stays. Never re-encode content streams with lopdf (`Content::encode`): it changed the rendering of real pages; cut bytes instead.
- **PassThrough**: a scan page with no pixel operations, at "No compression" or when re-encoding is not smaller. The original image stream is reused, wrapped in Flate when that saves 1%. Do not use zenjpeg's `restructure`: its output has stray bytes before scan markers (libjpeg warns "extraneous bytes").
- **ExactGray**: "No compression" + grayscale (no cleanup) on a YCbCr JPEG scan. The luma coefficients are kept as they are; accepted up to 10% above the original size (it is baseline, originals are often progressive).
- **Reencoded**: the scan image is decoded at its own resolution, cropped, downsampled to the level's DPI, processed and encoded.
- **Rendered**: MuPDF renders the page at the resolution of its main image (the largest, if it covers half the page), else 300 DPI, capped at 50 Mpx; then as above. At "No compression" it uses that image's JPEG quality, so rendered scans do not grow.

## Invariants

- **Page and image jobs do not read the source.** `page_job` copies what `run_page_job` needs (`PageJob`, `pdfimage::StoredImage`): the web app runs jobs in other WebAssembly instances. Rendering and OCR happen outside the job, through `PageJob::render` and `PageOutput::ocr`.
- **Preview must equal output.** Thumbnails and saves both call `pixel::apply_pixel_ops`. Every pixel operation takes `scale` (resolution / 300 DPI) so its result does not depend on the working resolution.
- **Pixel operations run on the image in its own orientation.** Rotation and split only change the placement matrix. Only OCR rotates pixels (to upright).
- **Memory is bounded by pages in flight.** Workers send finished pages through a `sync_channel` of capacity = threads; the writer puts each object on disk at once. Do not collect per-page results in memory.
- **MuPDF documents live in thread-locals.** Call `render::close_documents()` on every pool thread (`pool.broadcast`) before the pool drops. Else the process aborts at thread exit.
- **Tesseract's C API defaults to one text block.** `Engine::new` sets `PSM_AUTO`. Without it, columns merge into one line.
- **Fail loudly.** A page that fails stops the save with its page number. Designed fallbacks are only the equal-quality ones: keep the original stream when a re-encode is not smaller, render with MuPDF when an image codec is not decodable.
- **Resolution choices use physical resolution.** Pages longer than 17 in (image converters write 72 or 96 DPI) count as A4: `SourcePage::ppi_factor` scales the declared resolution. Use `Placed::ppi()`, never the raw matrix, for DPI limits, pixel-operation scale and the resolution given to Tesseract.
- **jbig2enc-rust leaves one `FF` before the `FF AC` end marker** of generic regions. `encode::drop_mq_padding` removes it (the output then equals C jbig2enc byte for byte). Keep it until upstream fixes the flush.
- **JPEG quality numbers are libjpeg numbers.** `encode::encode_jpeg` maps them to zenjpeg settings of equal fidelity (`zenjpeg_setting`). Do not pass libjpeg numbers to zenjpeg directly.

## Testing

`cargo test --release` runs unit tests and `tests/save.rs`: end-to-end saves of synthetic PDFs built with lopdf (routes, pass-through bytes, exact gray, downsampling, JBIG2, split text pages, merge order, OCR). Add a test there for each new route or option.

```bash
B=~/.cache/cargo-target/pdf-editor/release/pdf-editor-cli
$B --info "../test data/input.pdf"                     # classification per page
$B "../test data/input.pdf" -o /tmp/out.pdf --compression high --mode gray
qpdf --check /tmp/out.pdf; pdfimages -list /tmp/out.pdf; pdftotext /tmp/out.pdf -
```

Test files in `../test data/` (not in git): `input.pdf` (11-page scan with an OCR layer), `classic.pdf` (53-page typeset R NEWS), `classic-encrypted.pdf` (RC4), `classic-aes256.pdf` (AES-256), `book330.pdf` (30 distinct copies of `input.pdf`, for benchmarks). Benchmark against `ocrmypdf -O3 --redo-ocr`.
