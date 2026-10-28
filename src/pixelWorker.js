/**
 * Module worker that runs page and image jobs of the engine. Jobs carry all
 * they need, so any number of these workers run in parallel.
 */

import init, { run_page_job, run_image_job } from "./engine/pdf_editor_web.js";
import { transferables } from "./transfer.js";

const ready = init();

self.onmessage = async (event) => {
  const { id, kind, job, rendered } = event.data;
  try {
    await ready;
    const result = kind === "page"
      ? run_page_job(job, rendered ? rendered.rgba : undefined, rendered ? rendered.width : 0, rendered ? rendered.height : 0)
      : run_image_job(job);
    self.postMessage({ id, result }, transferables(result));
  } catch (error) {
    self.postMessage({ id, error: error && error.message ? error.message : String(error) });
  }
};
