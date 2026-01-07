# PDF Editor

## Goal

A fully online (HTML, CSS, Javascript) pdf editor that can live in a Github page.

It will run on book scans, helping us clean them up.

It needs to be able to:
* load pdfs (one or more)
* displaying all pages
* being able to reorder pages
* let you select pages (some or all) for editing
    * be able to rotate pages (90degrees)
    * be able to split pages down the middle (to split double pages)
    * be able to delete pages
* run OCR on the PDF (Tesseract?)
* compress the images / pdf
* set pages to black and white (no need to save useless color information)
* save the pdf
* have undo / redo buttons in case of mistake

Note that we want a simple minimal interface, no need for functionality out of scope.

Try not to reinvent the wheel, use existing libraries and framework where possible.

CSS-wise, use an existing framework.