//! Command-line front end: the same pipeline as the app, for scripts,
//! tests and benchmarks.

use anyhow::{Result, bail};
use clap::{Parser, ValueEnum};
use pdf_editor::model::{ColorMode, Compression, Page, PageKind, Side};
use pdf_editor::save::{SaveOptions, default_file_name, save};
use pdf_editor::source::Source;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

#[derive(Clone, Copy, ValueEnum)]
enum CompressionArg {
    None,
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, ValueEnum)]
enum ModeArg {
    Color,
    Gray,
    Bw,
    BwAdaptive,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum TextArg {
    /// Copy text pages as-is.
    Keep,
    /// Remove all text from text pages, then copy them.
    Strip,
    /// Treat text pages as scans.
    Rasterize,
}

#[derive(Parser)]
#[command(about = "Clean up and compress scanned books (same engine as the app).")]
struct Args {
    /// Input PDFs, concatenated in order.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,
    /// Output file, or directory for the default name.
    #[arg(short, long)]
    output: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "high")]
    compression: CompressionArg,
    /// Color mode of scan pages (default: gray). Setting it on text pages
    /// rasterizes them.
    #[arg(long, value_enum)]
    mode: Option<ModeArg>,
    /// What to do with pages that have real text.
    #[arg(long, value_enum, default_value = "keep")]
    text: TextArg,
    #[arg(long)]
    remove_shading: bool,
    #[arg(long)]
    enhance_contrast: bool,
    /// Clockwise quarter turns applied to every page.
    #[arg(long, default_value_t = 0)]
    rotate: u32,
    /// Split every page into its left and right halves.
    #[arg(long)]
    split: bool,
    /// Tesseract language(s), e.g. eng or eng+fra; "none" disables OCR.
    #[arg(long, default_value = "eng")]
    ocr: String,
    /// Worker threads (0: one per logical CPU).
    #[arg(long, default_value_t = 0)]
    threads: usize,
    /// Print how each page is classified and exit.
    #[arg(long)]
    info: bool,
}

fn main() -> Result<()> {
    pdf_editor::init_process();
    let args = Args::parse();
    let start = Instant::now();
    let mut sources = Vec::new();
    for path in &args.inputs {
        sources.push(Source::open(path)?);
    }
    let load_seconds = start.elapsed().as_secs_f64();

    if args.info {
        for source in &sources {
            println!("{} ({} pages{})", source.name, source.pages.len(), if source.was_encrypted { ", was encrypted" } else { "" });
            for (i, p) in source.pages.iter().enumerate() {
                let what = match &p.content {
                    Err(e) => format!("unreadable content: {e}"),
                    Ok(c) => format!(
                        "text={} visible_text={} vectors={} images={} coverage={:.2}",
                        c.has_text,
                        c.has_visible_text,
                        c.has_vector_painting,
                        c.images.len(),
                        c.image_area() / p.page_box.area()
                    ),
                };
                let kind = if p.is_text_page { "TEXT" } else if p.scan_image().is_some() { "SCAN" } else { "RENDER" };
                println!("  {:4} {kind:6} {:.0}x{:.0}pt {what}", i + 1, p.display_size.0, p.display_size.1);
            }
        }
        return Ok(());
    }

    let mode = args.mode.map(|m| match m {
        ModeArg::Color => ColorMode::Color,
        ModeArg::Gray => ColorMode::Gray,
        ModeArg::Bw => ColorMode::BwFlat,
        ModeArg::BwAdaptive => ColorMode::BwAdaptive,
    });
    let mut pages = Vec::new();
    for (s, source) in sources.iter_mut().enumerate() {
        let text_pages: Vec<usize> = (0..source.pages.len()).filter(|&i| source.pages[i].is_text_page).collect();
        if args.text == TextArg::Strip && !text_pages.is_empty() {
            let skipped = source.strip_text(&text_pages)?;
            if skipped > 0 {
                eprintln!("warning: {skipped} content streams of {} have inline images; their text was kept", source.name);
            }
        }
        for i in 0..source.pages.len() {
            let kind = match (source.pages[i].is_text_page, args.text) {
                (false, _) | (true, TextArg::Rasterize) => PageKind::Scan,
                (true, TextArg::Keep) => PageKind::Text,
                (true, TextArg::Strip) => PageKind::TextStripped,
            };
            let mut page = Page::new(s, i, kind);
            if let Some(mode) = mode {
                if kind == PageKind::Scan || args.mode.is_some() {
                    page.color = mode;
                }
            }
            page.remove_shading = args.remove_shading;
            page.enhance_contrast = args.enhance_contrast;
            for _ in 0..args.rotate % 4 {
                page.view = page.view.rotated_cw();
            }
            if args.split {
                let mut right = page.clone();
                page.view = page.view.split(Side::Left);
                right.view = right.view.split(Side::Right);
                pages.push(page);
                pages.push(right);
            } else {
                pages.push(page);
            }
        }
    }

    let sources: Vec<std::sync::Arc<Source>> = sources.into_iter().map(std::sync::Arc::new).collect();
    let compression = match args.compression {
        CompressionArg::None => Compression::None,
        CompressionArg::Low => Compression::Low,
        CompressionArg::Medium => Compression::Medium,
        CompressionArg::High => Compression::High,
    };
    let ocr_language = (args.ocr != "none").then(|| args.ocr.clone());
    let output = match &args.output {
        Some(p) if p.is_dir() => p.join(default_file_name(&sources, &pages, compression, ocr_language.is_some())),
        Some(p) => p.clone(),
        None => PathBuf::from(default_file_name(&sources, &pages, compression, ocr_language.is_some())),
    };
    if args.inputs.iter().any(|i| i.canonicalize().ok() == output.canonicalize().ok()) {
        bail!("output would overwrite an input");
    }

    let options = SaveOptions { compression, ocr_language, threads: args.threads };
    let cancel = AtomicBool::new(false);
    let last = std::sync::Mutex::new(Instant::now());
    let progress = |done: usize, total: usize, message: &str| {
        let mut last = last.lock().unwrap();
        if last.elapsed().as_millis() > 500 || done == total {
            eprintln!("{done}/{total} {message}");
            *last = Instant::now();
        }
    };
    let report = save(&sources, &pages, &options, &output, &progress, &cancel)?;
    let input_bytes: u64 = args.inputs.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
    let mut routes: Vec<_> = report.routes.iter().map(|(r, n)| format!("{r:?}={n}")).collect();
    routes.sort();
    println!(
        "{}: {} pages, {} -> {} bytes ({:.1}%), load {:.2}s, save {:.2}s, peak RSS {} MB, routes [{}], recompressed images {}",
        output.display(),
        report.pages,
        input_bytes,
        report.bytes,
        100.0 * report.bytes as f64 / input_bytes.max(1) as f64,
        load_seconds,
        report.seconds,
        pdf_editor::peak_rss_mb(),
        routes.join(" "),
        report.recompressed_images
    );
    Ok(())
}
