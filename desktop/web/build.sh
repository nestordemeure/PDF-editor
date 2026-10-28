#!/bin/sh
# Builds the engine for the web app into src/engine/ at the repository root.
set -e
cd "$(dirname "$0")"

# jbig2enc-rust times its work with std::time::Instant, which panics in
# browsers: build a copy that uses web_time instead.
jbig2=jbig2enc-rust-0.5.4
if [ ! -d "patched/$jbig2" ]; then
    mkdir -p patched
    curl -sSfL "https://static.crates.io/crates/jbig2enc-rust/$jbig2.crate" | tar xz -C patched
    patch -s -d "patched/$jbig2" -p1 < jbig2enc-web-time.patch
fi

target=$(cargo metadata --format-version 1 --no-deps | sed 's/.*"target_directory":"\([^"]*\)".*/\1/')
RUSTFLAGS="-C target-feature=+simd128" cargo build --release --target wasm32-unknown-unknown
wasm-bindgen --target web --no-typescript --out-dir ../../src/engine "$target/wasm32-unknown-unknown/release/pdf_editor_web.wasm"
