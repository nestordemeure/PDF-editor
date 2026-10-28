/**
 * OCR with tesseract.js: the same Tesseract and models (tessdata_fast) as
 * the desktop app. Returns lines in the engine's format; the engine draws
 * the text layer.
 */

const TESSERACT_URL = "https://cdn.jsdelivr.net/npm/tesseract.js@6.0.1/dist/tesseract.esm.min.js";
const MODELS_URL = "https://cdn.jsdelivr.net/gh/tesseract-ocr/tessdata_fast@main";
const PSM_AUTO = "3";

/** Starts `size` Tesseract workers for `lang` (e.g. "eng+fra"). */
export async function createOcr(lang, size) {
  const module = await import(TESSERACT_URL);
  const Tesseract = module.default || module;
  const scheduler = Tesseract.createScheduler();
  const workers = await Promise.all(Array.from({ length: size }, async () => {
    const worker = await Tesseract.createWorker(lang.split("+"), Tesseract.OEM.LSTM_ONLY, {
      langPath: MODELS_URL,
      gzip: false,
    });
    // Find columns: Tesseract's API defaults to one uniform block.
    await worker.setParameters({ tessedit_pageseg_mode: PSM_AUTO });
    scheduler.addWorker(worker);
    return worker;
  }));
  if (workers.length === 0) throw new Error("No OCR worker started.");

  return {
    /** Recognizes an upright gray image ({ gray, width, height, ppi }). */
    async recognize({ gray, width, height, ppi }) {
      const { data } = await scheduler.addJob("recognize", toPgm(gray, width, height), { user_defined_dpi: String(ppi) }, { blocks: true, text: false });
      return engineLines(data.blocks || []);
    },
    async terminate() {
      await scheduler.terminate();
    },
  };
}

/** A binary PGM file, which Tesseract reads without decoding work. */
function toPgm(gray, width, height) {
  const header = new TextEncoder().encode(`P5\n${width} ${height}\n255\n`);
  const file = new Uint8Array(header.length + gray.length);
  file.set(header);
  file.set(gray, header.length);
  return file;
}

/** Lines with words, as the engine's `OcrLine` (pixel boxes, y down). */
function engineLines(blocks) {
  const lines = [];
  for (const block of blocks) {
    for (const paragraph of block.paragraphs) {
      for (const line of paragraph.lines) {
        const words = line.words
          .map(word => ({ text: word.text.trim(), bbox: box(word.bbox) }))
          .filter(word => word.text.length > 0);
        if (words.length === 0) continue;
        const b = line.bbox;
        const baseline = line.baseline && line.baseline.has_baseline
          ? [line.baseline.x0, line.baseline.y0, line.baseline.x1, line.baseline.y1]
          : [b.x0, b.y1, b.x1, b.y1];
        lines.push({ bbox: box(b), baseline, words });
      }
    }
  }
  return lines;
}

function box(b) {
  return [b.x0, b.y0, b.x1, b.y1];
}
