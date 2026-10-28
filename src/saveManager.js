/**
 * Save orchestration. The engine decides and does everything that touches
 * the PDF (see desktop/CLAUDE.md, "Page routes at save"); this module moves
 * work between its workers:
 * - the Library worker plans the save, hands out one job per page and
 *   writes each finished page at once;
 * - pixel workers run the jobs (decode, clean up, encode);
 * - PDF.js renders the pages that have no decodable scan image;
 * - tesseract.js recognizes the text; the engine draws the text layer.
 * Pages in flight are bounded, so memory does not grow with the book.
 */

import { library, createPixelPool, enginePage } from "./engine.js";
import { renderPageRgba } from "./pageRenderer.js";
import { createOcr } from "./ocr.js";
import { forEachConcurrent } from "./pageCommands.js";

/**
 * Saves `pages` and returns { blob, report }. `sources` maps a sourceId to
 * its record ({ engineId, pdfDoc, pages }).
 */
export async function savePdf({ sources, pages, compression, ocrLang, onProgress, onStatus }) {
  const records = pages.map(page => sources.get(page.sourceId));
  const byEngineId = new Map([...sources.values()].map(record => [record.engineId, record]));
  const ocr = Boolean(ocrLang && ocrLang !== "none");
  const cores = navigator.hardwareConcurrency || 4;

  const plan = await library("begin_save", pages.map((page, i) => enginePage(page, records[i])), compression, ocr);
  const pool = createPixelPool(Math.max(1, Math.min(cores - 1, 8)));
  let ocrEngine = null;
  const chunks = [];
  const drain = async () => {
    const bytes = await library("take_output");
    if (bytes.length > 0) chunks.push(bytes);
  };

  try {
    if (ocr && plan.raster.length > 0) {
      onStatus("Loading the OCR engine...");
      ocrEngine = await createOcr(ocrLang, Math.max(1, Math.min(cores - 1, 8)));
    }

    // Images of text pages: recompressed when that makes them smaller.
    if (plan.images > 0) onStatus("Recompressing images of text pages...");
    await forEachConcurrent(Array.from({ length: plan.images }, (_, k) => k), pool.size, async k => {
      const job = await library("image_job", k);
      if (job.length === 0) return;
      const encoded = await pool.runImage(job);
      if (encoded.length > 0) await library("replace_image", k, encoded);
    });
    await library("write_text_pages");
    await drain();

    let done = pages.length - plan.raster.length;
    const inFlight = pool.size + (ocrEngine ? pool.size : 0);
    await forEachConcurrent(plan.raster, inFlight, async i => {
      const message = await library("page_job", i);
      const rendered = message.render
        ? await renderPageRgba(byEngineId.get(message.source).pdfDoc, message.index, message.render)
        : null;
      const result = await pool.runPage(message.job, rendered);
      let text = null;
      if (result.ocr) {
        text = { lines: await ocrEngine.recognize(result.ocr), pxToOut: result.ocr.pxToOut };
      }
      await library("add_page", i, result.output, text);
      await drain();
      done += 1;
      onStatus(`${ocr ? "Processing and recognizing" : "Processing"} page ${done}/${pages.length}`);
      onProgress(done, pages.length);
    });

    onStatus("Finishing the PDF...");
    const { report, rest } = await library("finish");
    chunks.push(rest);
    return { blob: new Blob(chunks, { type: "application/pdf" }), report };
  } catch (error) {
    await library("cancel_save");
    throw error;
  } finally {
    pool.terminate();
    if (ocrEngine) await ocrEngine.terminate();
  }
}
