#!/bin/sh
# Builds the engine for the web app into src/engine/ at the repository root.
set -e
cd "$(dirname "$0")"
target=$(cargo metadata --format-version 1 --no-deps | sed 's/.*"target_directory":"\([^"]*\)".*/\1/')
RUSTFLAGS="-C target-feature=+simd128" cargo build --release --target wasm32-unknown-unknown
wasm-bindgen --target web --no-typescript --out-dir ../../src/engine "$target/wasm32-unknown-unknown/release/pdf_editor_web.wasm"
