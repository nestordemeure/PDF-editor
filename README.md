# PDF Editor

Minimal, in-browser PDF cleanup tool for scanned books.

## Usage

- For local testing (avoids CORS issues), run `./serve.sh` and open http://localhost:8000.
- Or open the GitHub Pages version: https://nestordemeure.github.io/PDF-editor/
- Load one or more PDFs.
- Select pages, then rotate, split, delete, or toggle B/W.
- Drag thumbnails to reorder.
- Click “Save PDF” to export.

## OCR

OCR is optional at export time and uses Scribe.js from `vendor/scribe.js`.
Scribe.js is AGPL-3.0 licensed; see `vendor/LICENSE-scribe.js-ocr.txt` for attribution.
To update the vendored file:

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

## TODO

Functionalities:
* sometimes pdfs are not scans but rather produced with Words, etc. how do we deal with that: do we convert them into images?
* clicking on a tool (ie changing all images to b&W) causes a freeze then a very fast progress bar then done. why freeze? we should hand the thread instantly
* memorize OCR so that i is not redone if we resave a document with no modification to pages?
* look into what ocrmypdf does for compressoin