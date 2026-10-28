/** Buffers of the typed arrays in a value, to transfer instead of copy. */
export function transferables(value, out = []) {
  if (ArrayBuffer.isView(value)) {
    out.push(value.buffer);
  } else if (value && typeof value === "object") {
    for (const item of Object.values(value)) transferables(item, out);
  }
  return out;
}
