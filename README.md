# PDF Editor

Minimal, in-browser PDF cleanup tool for scanned books.

## Usage

- For local testing (avoids CORS issues), run `./serve.sh` and open <http://localhost:8000>.
- Or open the GitHub Pages version: <https://nestordemeure.github.io/PDF-editor/>
- Load one or more PDFs.
- Select pages, then rotate, split, delete, or toggle B/W.
- Drag thumbnails to reorder.
- Click “Save PDF” to export.

## OCR

OCR is optional at export time and uses Scribe.js from `vendor/scribe.js`.
Scribe.js is AGPL-3.0 licensed; see `vendor/LICENSE-scribe.js-ocr.txt` for attribution.
To update the vendored file:

B&W pages are compressed with CCITT G4 via `ts-ccitt-g4-encoder` (MIT),
vendored as `vendor/ccitt-g4-encoder.mjs`. To update it:

```bash
npm install ts-ccitt-g4-encoder@latest
cp node_modules/ts-ccitt-g4-encoder/dist/index.mjs vendor/ccitt-g4-encoder.mjs
cp node_modules/ts-ccitt-g4-encoder/LICENSE vendor/LICENSE-ts-ccitt-g4-encoder.txt
```

```bash
npm install scribe.js-ocr@latest
cp node_modules/scribe.js-ocr/scribe.js vendor/scribe.js
cp node_modules/scribe.js-ocr/LICENSE vendor/LICENSE-scribe.js-ocr.txt
cp -R node_modules/scribe.js-ocr/js vendor/js
cp -R node_modules/scribe.js-ocr/lib vendor/lib
cp -R node_modules/scribe.js-ocr/tess vendor/tess
cp -R node_modules/scribe.js-ocr/fonts vendor/fonts
cp -R node_modules/scribe.js-ocr/mupdf vendor/mupdf
```
