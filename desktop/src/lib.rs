//! Core of the PDF editor: everything except the user interface.
//!
//! Without the `native` feature, the library has no threads, files, MuPDF
//! or Tesseract, and builds for WebAssembly.

pub mod assemble;
pub mod content;
pub mod copy;
pub mod encode;
pub mod geometry;
pub mod jpeggray;
pub mod model;
pub mod mrc;
#[cfg(feature = "native")]
pub mod ocr;
pub mod pdfimage;
pub mod pipeline;
pub mod pixel;
#[cfg(feature = "native")]
pub mod render;
#[cfg(feature = "native")]
pub mod save;
pub mod source;
pub mod textcrop;
pub mod textlayer;
pub mod writer;

/// Process-wide setup; call first in `main`.
#[cfg(feature = "native")]
pub fn init_process() {
    // Tesseract must not start its own thread pool inside each worker.
    // SAFETY: called before any other thread exists.
    unsafe { std::env::set_var("OMP_THREAD_LIMIT", "1") };
}

/// This app's data directory (`$XDG_DATA_HOME/pdf-editor`).
#[cfg(feature = "native")]
pub fn data_dir() -> std::path::PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share"));
    base.join("pdf-editor")
}

/// Peak resident memory of this process, in megabytes.
#[cfg(feature = "native")]
pub fn peak_rss_mb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("VmHWM:")).and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok()))
        .map_or(0, |kb| kb / 1024)
}
