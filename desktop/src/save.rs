//! Save orchestration: worker threads process pages while this thread
//! writes finished objects to disk, so memory stays bounded by the number
//! of pages in flight.

use crate::assemble::Assembler;
pub use crate::assemble::SaveReport;
use crate::model::{Compression, Page};
use crate::ocr::{self, Engine};
use crate::pipeline::{self, PageOutput, PageRoute, Settings};
use crate::source::Source;
use anyhow::{Context, Result, anyhow, bail};
use rayon::prelude::*;
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct SaveOptions {
    pub compression: Compression,
    /// Tesseract language(s), e.g. `eng` or `eng+fra`; `None` skips OCR.
    pub ocr_language: Option<String>,
    /// Worker threads; 0 means one per logical CPU.
    pub threads: usize,
    /// Overrides the compression level's maximum resolution.
    pub max_dpi: Option<f64>,
    /// Overrides the compression level's JPEG quality (libjpeg scale).
    pub jpeg_quality: Option<u8>,
}

pub type PageRouteKey = PageRoute;

/// Progress callback: (done, total, message).
pub type Progress<'a> = &'a (dyn Fn(usize, usize, &str) + Sync);

thread_local! {
    static ENGINE: RefCell<Option<Engine>> = const { RefCell::new(None) };
}

fn with_engine<T>(language: &str, f: impl FnOnce(&mut Engine) -> Result<T>) -> Result<T> {
    ENGINE.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.as_ref().is_none_or(|e| e.language() != language) {
            *slot = Some(Engine::new(language)?);
        }
        f(slot.as_mut().unwrap())
    })
}

pub fn save(sources: &[Arc<Source>], pages: &[Page], options: &SaveOptions, out_path: &Path, progress: Progress, cancel: &AtomicBool) -> Result<SaveReport> {
    if pages.is_empty() {
        bail!("no pages to save");
    }
    let start = Instant::now();
    let dir = out_path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let tmp = tempfile::Builder::new().prefix(".pdf-editor-").suffix(".pdf").tempfile_in(dir)?.into_temp_path();
    let mut report = write_document(sources, pages, options, &tmp, progress, cancel)?;
    // Temporary files are private; the result should not be.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))?;
    tmp.persist(out_path).map_err(|e| anyhow!("cannot write {}: {}", out_path.display(), e.error))?;
    report.bytes = std::fs::metadata(out_path)?.len();
    report.seconds = start.elapsed().as_secs_f64();
    Ok(report)
}

fn write_document(sources: &[Arc<Source>], pages: &[Page], options: &SaveOptions, path: &Path, progress: Progress, cancel: &AtomicBool) -> Result<SaveReport> {
    let has_raster = pages.iter().any(|p| !p.is_preserved());
    let ocr_language = options.ocr_language.clone().filter(|_| has_raster);
    if let Some(lang) = &ocr_language {
        ocr::ensure_languages(lang, &|message| progress(0, pages.len(), message))?;
        // Fail before any work when the model does not load.
        Engine::new(lang)?;
    }
    let file = std::fs::File::create(path).with_context(|| format!("cannot create {}", path.display()))?;
    let out = Box::new(std::io::BufWriter::with_capacity(1 << 20, file));
    let mut doc = Assembler::new(sources.to_vec(), pages.to_vec(), ocr_language.is_some(), out)?;

    let threads = if options.threads == 0 { std::thread::available_parallelism().map_or(4, |n| n.get()) } else { options.threads };
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build()?;

    let total = pages.len();
    let done = AtomicUsize::new(0);
    let tick = |message: &str| {
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        progress(n, total, message);
    };

    // Preserved pages: recompress their images in parallel, then copy.
    let images = doc.preserved_images();
    if !images.is_empty() {
        progress(0, total, "Recompressing images of text pages");
    }
    let replaced: Vec<_> = pool.install(|| {
        images
            .par_iter()
            .map(|&task| -> Result<Option<_>> {
                if cancel.load(Ordering::Relaxed) {
                    bail!("cancelled");
                }
                let source = &sources[task.source];
                let image = match pipeline::image_job(source, task.id, options.compression, task.shown_ppi)? {
                    Some(job) => pipeline::run_image_job(&job),
                    None => Ok(None),
                }
                .with_context(|| format!("image {:?} of {}", task.id, source.name))?;
                Ok(image.map(|e| (task, e)))
            })
            .filter_map(Result::transpose)
            .collect::<Result<_>>()
    })?;
    for (task, encoded) in replaced {
        doc.replace_image(task, encoded)?;
    }
    doc.write_preserved_pages(&mut || tick("Copying text pages"))?;

    // Raster pages: workers produce, this thread writes.
    let raster = doc.raster_pages();
    let settings = Settings { compression: options.compression, ocr: ocr_language.is_some(), max_dpi: options.max_dpi, jpeg_quality: options.jpeg_quality };
    let (tx, rx) = mpsc::sync_channel::<(usize, Result<PageOutput>)>(threads);
    let mut first_error: Option<anyhow::Error> = None;
    std::thread::scope(|scope| -> Result<()> {
        let raster = &raster;
        let ocr_language = &ocr_language;
        scope.spawn(move || {
            let pool = pool;
            pool.install(|| {
                raster.par_iter().for_each_with(tx, |tx, &i| {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let page = &pages[i];
                    let source = &sources[page.source];
                    let result = match ocr_language {
                        Some(lang) => with_engine(lang, |engine| pipeline::process_page(source, page, settings, Some(engine))),
                        None => pipeline::process_page(source, page, settings, None),
                    }
                    .with_context(|| format!("page {} ({} page {})", i + 1, source.name, page.index + 1));
                    if result.is_err() {
                        cancel.store(true, Ordering::Relaxed);
                    }
                    let _ = tx.send((i, result));
                });
            });
            // MuPDF documents must close before their thread's context goes.
            pool.broadcast(|_| crate::render::close_documents());
        });
        for (i, result) in rx {
            match result.and_then(|output| doc.write_raster_page(i, output)) {
                Ok(()) => tick(if settings.ocr { "Processing and recognizing pages" } else { "Processing pages" }),
                Err(e) => {
                    cancel.store(true, Ordering::Relaxed);
                    first_error.get_or_insert(e);
                }
            }
        }
        Ok(())
    })?;
    if let Some(e) = first_error {
        return Err(e);
    }
    if cancel.load(Ordering::Relaxed) {
        bail!("cancelled");
    }
    doc.finish()
}

/// Default output name: source name (or "merged"), compression, dominant
/// color mode, and "ocr".
pub fn default_file_name(sources: &[Arc<Source>], pages: &[Page], compression: Compression, ocr: bool) -> String {
    let used: HashSet<usize> = pages.iter().map(|p| p.source).collect();
    let names: HashSet<&str> = used.iter().map(|&s| sources[s].name.as_str()).collect();
    let base = if names.len() == 1 { names.into_iter().next().unwrap().to_owned() } else { "merged".to_owned() };
    let mut parts = vec![base];
    for tag in [compression.file_tag(), pipeline::color_tag(pages), if ocr { "ocr" } else { "" }] {
        if !tag.is_empty() {
            parts.push(tag.to_owned());
        }
    }
    format!("{}.pdf", parts.join("_"))
}
