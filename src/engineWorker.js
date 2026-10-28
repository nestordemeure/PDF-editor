/**
 * Module worker that owns the engine's Library: the source documents and
 * the output file being written. Calls arrive as { id, method, args } and
 * run one at a time; Uint8Array results are transferred back.
 */

import init, { Library } from "./engine/pdf_editor_web.js";
import { transferables } from "./transfer.js";

const ready = init();
let library = null;

self.onmessage = async (event) => {
  const { id, method, args } = event.data;
  try {
    await ready;
    if (!library) library = new Library();
    const result = library[method](...args);
    self.postMessage({ id, result }, transferables(result));
  } catch (error) {
    self.postMessage({ id, error: error && error.message ? error.message : String(error) });
  }
};
