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

Grayscale and color pages are JPEG-compressed with MozJPEG via
`@jsquash/jpeg` (Apache-2.0), vendored under `vendor/jsquash-jpeg/`. To update it:

Raw DeviceGray streams are Flate-compressed with `pako` ((MIT AND Zlib)),
vendored as `vendor/pako.mjs`. To update it:

```bash
npm install pako@latest
cp node_modules/pako/dist/pako.mjs vendor/pako.mjs
cp node_modules/pako/LICENSE vendor/LICENSE-pako.txt
```

```bash
npm install @jsquash/jpeg@latest
cp node_modules/@jsquash/jpeg/{encode.js,meta.js,utils.js,LICENSE} vendor/jsquash-jpeg/
cp node_modules/@jsquash/jpeg/codec/LICENSE.codec.md vendor/jsquash-jpeg/
cp node_modules/@jsquash/jpeg/codec/enc/mozjpeg_enc.{js,wasm} vendor/jsquash-jpeg/codec/enc/
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
