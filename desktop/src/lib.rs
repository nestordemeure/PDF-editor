//! Core of the PDF editor: everything except the user interface.

pub mod content;
pub mod copy;
pub mod encode;
pub mod geometry;
pub mod jpeggray;
pub mod model;
pub mod ocr;
pub mod pdfimage;
pub mod pipeline;
pub mod pixel;
pub mod render;
pub mod save;
pub mod source;
pub mod writer;

/// Process-wide setup; call first in `main`.
pub fn init_process() {
    // Tesseract must not start its own thread pool inside each worker.
    // SAFETY: called before any other thread exists.
    unsafe { std::env::set_var("OMP_THREAD_LIMIT", "1") };
}

/// Peak resident memory of this process, in megabytes.
pub fn peak_rss_mb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("VmHWM:")).and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok()))
        .map_or(0, |kb| kb / 1024)
}

