/**
 * Access to the engine (the desktop app's Rust code, compiled to
 * WebAssembly in src/engine/):
 * - `library`: the worker that holds the sources and writes the output;
 * - `createPixelPool`: workers that run page and image jobs;
 * - `previewPage`: thumbnails, on the main thread.
 */

import init, { preview } from "./engine/pdf_editor_web.js";
import { transferables } from "./transfer.js";

const mainReady = init();

/** Calls into a worker as promises, one id per call. */
function workerClient(url) {
  const worker = new Worker(new URL(url, import.meta.url), { type: "module" });
  const pending = new Map();
  let nextId = 0;
  let failure = null;
  worker.onmessage = (event) => {
    const { id, result, error } = event.data;
    const entry = pending.get(id);
    pending.delete(id);
    if (error) entry.reject(new Error(error));
    else entry.resolve(result);
  };
  // The worker script did not load, or crashed.
  worker.onerror = (event) => {
    failure = new Error(`Engine worker failed: ${event.message || "could not start"}`);
    for (const entry of pending.values()) entry.reject(failure);
    pending.clear();
  };
  return {
    post(message, transfer = []) {
      if (failure) return Promise.reject(failure);
      const id = nextId++;
      return new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject });
        worker.postMessage({ id, ...message }, transfer);
      });
    },
    terminate() {
      worker.terminate();
    },
  };
}

let libraryClient = null;

/** Calls a method of the engine's Library (see desktop/web/src/lib.rs). */
export function library(method, ...args) {
  if (!libraryClient) libraryClient = workerClient("./engineWorker.js");
  return libraryClient.post({ method, args }, transferables(args));
}

/** Workers for page and image jobs; `size` jobs run at once. */
export function createPixelPool(size) {
  const workers = Array.from({ length: size }, () => workerClient("./pixelWorker.js"));
  const idle = [...workers];
  const waiting = [];
  const run = async (message) => {
    const worker = idle.pop() || await new Promise(resolve => waiting.push(resolve));
    try {
      return await worker.post(message, transferables(message));
    } finally {
      const next = waiting.shift();
      if (next) next(worker);
      else idle.push(worker);
    }
  };
  return {
    size,
    runPage: (job, rendered) => run({ kind: "page", job, rendered }),
    runImage: (job) => run({ kind: "image", job }),
    terminate() {
      for (const worker of workers) worker.terminate();
    },
  };
}

/** The engine's description of a page of the app's page model. */
export function enginePage(page, record) {
  return {
    source: record.engineId,
    index: page.sourcePageIndex,
    kind: page.isClassic ? (page.textStripped ? "stripped" : "text") : "scan",
    operations: page.operations,
  };
}

/**
 * Applies a page's view and pixel operations to `imageData`, a render of
 * its source page; returns new ImageData.
 */
export async function previewPage(imageData, page, record) {
  await mainReady;
  const info = record.pages[page.sourcePageIndex];
  const out = preview(
    new Uint8Array(imageData.data.buffer, imageData.data.byteOffset, imageData.data.byteLength),
    imageData.width, imageData.height, enginePage(page, record), info.width, info.height, info.ppiFactor,
  );
  return new ImageData(new Uint8ClampedArray(out.rgba.buffer, out.rgba.byteOffset, out.rgba.byteLength), out.width, out.height);
}
