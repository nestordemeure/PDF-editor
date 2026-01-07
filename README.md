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
* improve color mode algorithms
* sometimes pdfs are not scans but rather produced with Words, etc. how do we deal with that: do we convert them into images?

* improve compression handling:
  * all PNG should be exported with UPNG (already loaded in index.html)
  * no compression:
     * we should preserve original DPIs (note that pdfs can have varying DPIs, different on various pages)
     * we should use either original images (if no color mode was changed on them) or lossless PNG (if their color mode was changed)
  * if some level of compression is enabled: 
    * color images should be jpeg (we expect them to be photographs)
    * greyscale/b&w images should be PNG (they can be lossy according to the level of compression picked), we expect them to be text (mostly black on white)
    * use fixed DPI for outputs (use meaningful defaults for color / greyscale / b&w), using the maximum of the input image DPI and the target value for a given color mode / compression level