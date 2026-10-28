//! Save orchestration: worker threads process pages while this thread
//! writes finished objects to disk, so memory stays bounded by the number
//! of pages in flight.

use crate::copy::Copier;
use crate::encode::{self, EncodedImage, ImageFilter};
use crate::model::{Compression, Page};
use crate::ocr::{self, Engine};
use crate::pipeline::{self, ImageOut, PageOutput, PageRoute, Settings};
use crate::source::{Source, inherited};
use crate::writer::{OutId, PdfWriter, format_number, reference};
use anyhow::{Context, Result, anyhow, bail};
use lopdf::{Dictionary, Object, ObjectId};
use rayon::prelude::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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

#[derive(Clone, Debug, Default)]
pub struct SaveReport {
    pub pages: usize,
    pub routes: HashMap<PageRouteKey, usize>,
    pub recompressed_images: usize,
    pub bytes: u64,
    pub seconds: f64,
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

/// Output object ids decided before any page is written.
struct Layout {
    catalog: OutId,
    pages_root: OutId,
    page_ids: Vec<OutId>,
    font: Option<OutId>,
}

fn write_document(sources: &[Arc<Source>], pages: &[Page], options: &SaveOptions, path: &Path, progress: Progress, cancel: &AtomicBool) -> Result<SaveReport> {
    let mut w = PdfWriter::create(path)?;
    let raster: Vec<usize> = (0..pages.len()).filter(|&i| pipeline::is_raster(&pages[i])).collect();
    let ocr_language = options.ocr_language.clone().filter(|_| !raster.is_empty());
    if let Some(lang) = &ocr_language {
        ocr::ensure_languages(lang, &|message| progress(0, pages.len(), message))?;
        // Fail before any work when the model does not load.
        Engine::new(lang)?;
    }
    let layout = Layout {
        catalog: w.alloc(),
        pages_root: w.alloc(),
        page_ids: pages.iter().map(|_| w.alloc()).collect(),
        font: ocr_language.as_ref().map(|_| w.alloc()),
    };
    if let Some(font) = layout.font {
        write_ocr_font(&mut w, font)?;
    }

    let threads = if options.threads == 0 { std::thread::available_parallelism().map_or(4, |n| n.get()) } else { options.threads };
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build()?;
    let mut report = SaveReport { pages: pages.len(), ..Default::default() };

    // One copier per source, so shared objects are written once.
    let mut copiers: Vec<Copier> = sources.iter().map(|s| Copier::new(&s.doc)).collect();
    for (i, page) in pages.iter().enumerate() {
        copiers[page.source].page_targets.entry(sources[page.source].pages[page.index].id).or_insert(layout.page_ids[i]);
    }

    let total = pages.len();
    let done = AtomicUsize::new(0);
    let tick = |message: &str| {
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        progress(n, total, message);
    };

    // Preserved pages: recompress their images in parallel, then copy.
    let preserved: Vec<usize> = (0..pages.len()).filter(|&i| !pipeline::is_raster(&pages[i])).collect();
    if !preserved.is_empty() {
        let mut images: Vec<(usize, ObjectId)> = Vec::new();
        let mut seen = HashSet::new();
        for &i in &preserved {
            let page = &pages[i];
            let source = &sources[page.source];
            for id in page_images(source, source.pages[page.index].id) {
                if seen.insert((page.source, id)) {
                    images.push((page.source, id));
                }
            }
        }
        // Where each image is drawn largest: its lowest resolution on a page.
        let mut shown_ppi: HashMap<(usize, ObjectId), f64> = HashMap::new();
        for &i in &preserved {
            let page = &pages[i];
            let Ok(content) = &sources[page.source].pages[page.index].content else { continue };
            for placed in &content.images {
                let (Some(id), Ok(stream)) = (placed.id, sources[page.source].doc.get_object(placed.id.unwrap_or((0, 0))).and_then(Object::as_stream)) else { continue };
                let Ok(width) = stream.dict.get(b"Width").and_then(Object::as_i64) else { continue };
                let extent = (placed.ctm.a * placed.ctm.a + placed.ctm.b * placed.ctm.b).sqrt();
                let ppi = width as f64 * 72.0 / extent.max(1e-6);
                let entry = shown_ppi.entry((page.source, id)).or_insert(ppi);
                *entry = entry.min(ppi);
            }
        }
        progress(0, total, "Recompressing images of text pages");
        let replaced: Vec<((usize, ObjectId), EncodedImage)> = pool.install(|| {
            images
                .par_iter()
                .map(|&(s, id)| -> Result<Option<((usize, ObjectId), EncodedImage)>> {
                    if cancel.load(Ordering::Relaxed) {
                        bail!("cancelled");
                    }
                    let image = pipeline::recompress_preserved_image(&sources[s], id, options.compression, shown_ppi.get(&(s, id)).copied())
                        .with_context(|| format!("image {id:?} of {}", sources[s].name))?;
                    Ok(image.map(|e| ((s, id), e)))
                })
                .filter_map(Result::transpose)
                .collect::<Result<_>>()
        })?;
        report.recompressed_images = replaced.len();
        for ((s, id), encoded) in replaced {
            let original = &sources[s].doc.get_object(id)?.as_stream()?.dict;
            copiers[s].replacements.insert(id, Object::Stream(lopdf::Stream::new(replacement_dict(original, &encoded), encoded.data)));
        }
        for &i in &preserved {
            write_preserved_page(&mut w, &mut copiers[pages[i].source], &sources[pages[i].source], &pages[i], layout.page_ids[i], layout.pages_root)?;
            *report.routes.entry(PageRoute::Copied).or_default() += 1;
            tick("Copying text pages");
        }
    }

    // Raster pages: workers produce, this thread writes.
    let settings = Settings { compression: options.compression, ocr: ocr_language.is_some(), max_dpi: options.max_dpi, jpeg_quality: options.jpeg_quality };
    let (tx, rx) = mpsc::sync_channel::<(usize, Result<PageOutput>)>(threads);
    let mut shared_images: HashMap<(usize, ObjectId, bool), OutId> = HashMap::new();
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
            let written = result.and_then(|output| {
                let route = output.route;
                write_raster_page(&mut w, &mut copiers[pages[i].source], &mut shared_images, pages[i].source, output, &layout, i)?;
                *report.routes.entry(route).or_default() += 1;
                Ok(())
            });
            match written {
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
    for copier in &mut copiers {
        copier.flush(&mut w)?;
    }

    let mut root = Dictionary::new();
    root.set("Type", Object::Name(b"Pages".to_vec()));
    root.set("Kids", Object::Array(layout.page_ids.iter().map(|&id| reference(id)).collect()));
    root.set("Count", Object::Integer(pages.len() as i64));
    w.write(layout.pages_root, &Object::Dictionary(root))?;
    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", reference(layout.pages_root));
    w.write(layout.catalog, &Object::Dictionary(catalog))?;
    let info = w.alloc();
    let mut info_dict = Dictionary::new();
    info_dict.set("Producer", Object::string_literal("PDF Editor"));
    w.write(info, &Object::Dictionary(info_dict))?;
    w.finish(layout.catalog, Some(info))?;
    Ok(report)
}

/// Image XObjects used by a page, including those inside its forms.
fn page_images(source: &Source, page_id: ObjectId) -> Vec<ObjectId> {
    let doc = &source.doc;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = Vec::new();
    if let Some(Object::Dictionary(r)) = inherited(doc, page_id, b"Resources") {
        stack.push(r);
    }
    while let Some(resources) = stack.pop() {
        let Ok(xobjects) = resources.get_deref(b"XObject", doc).and_then(Object::as_dict) else { continue };
        for (_, entry) in xobjects.iter() {
            let Ok(id) = entry.as_reference() else { continue };
            if !seen.insert(id) {
                continue;
            }
            let Ok(stream) = doc.get_object(id).and_then(Object::as_stream) else { continue };
            match stream.dict.get(b"Subtype").and_then(Object::as_name) {
                Ok(b"Image") => out.push(id),
                Ok(b"Form") => {
                    if let Ok(Object::Dictionary(r)) = stream.dict.get_deref(b"Resources", doc) {
                        stack.push(r.clone());
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Image dictionary for new data, keeping the original's color space and
/// rendering hints.
fn replacement_dict(original: &Dictionary, encoded: &EncodedImage) -> Dictionary {
    let mut d = image_dict(encoded);
    for key in [&b"ColorSpace"[..], b"Interpolate", b"Intent", b"SMask"] {
        if let Ok(v) = original.get(key) {
            if key == b"ColorSpace" && encoded.channels == 1 && encoded.bits_per_component == 1 {
                continue;
            }
            d.set(key.to_vec(), v.clone());
        }
    }
    d
}

fn image_dict(e: &EncodedImage) -> Dictionary {
    let mut d = Dictionary::new();
    d.set("Type", Object::Name(b"XObject".to_vec()));
    d.set("Subtype", Object::Name(b"Image".to_vec()));
    d.set("Width", Object::Integer(e.width as i64));
    d.set("Height", Object::Integer(e.height as i64));
    d.set("ColorSpace", Object::Name(if e.channels == 1 { b"DeviceGray".to_vec() } else { b"DeviceRGB".to_vec() }));
    d.set("BitsPerComponent", Object::Integer(e.bits_per_component as i64));
    match e.filter {
        ImageFilter::Dct => d.set("Filter", Object::Name(b"DCTDecode".to_vec())),
        ImageFilter::FlateDct => d.set("Filter", Object::Array(vec![Object::Name(b"FlateDecode".to_vec()), Object::Name(b"DCTDecode".to_vec())])),
        ImageFilter::Jbig2 => d.set("Filter", Object::Name(b"JBIG2Decode".to_vec())),
        ImageFilter::FlatePng => {
            d.set("Filter", Object::Name(b"FlateDecode".to_vec()));
            let mut parms = Dictionary::new();
            parms.set("Predictor", Object::Integer(15));
            parms.set("Colors", Object::Integer(e.channels as i64));
            parms.set("BitsPerComponent", Object::Integer(8));
            parms.set("Columns", Object::Integer(e.width as i64));
            d.set("DecodeParms", Object::Dictionary(parms));
        }
    }
    d
}

fn number(v: f64) -> Object {
    if v.fract() == 0.0 { Object::Integer(v as i64) } else { Object::Real(v as f32) }
}

fn rect_object(x0: f64, y0: f64, x1: f64, y1: f64) -> Object {
    Object::Array(vec![number(x0), number(y0), number(x1), number(y1)])
}

fn write_preserved_page(w: &mut PdfWriter, copier: &mut Copier, source: &Source, page: &Page, out_id: OutId, parent: OutId) -> Result<()> {
    let sp = &source.pages[page.index];
    let dict = source.doc.get_dictionary(sp.id)?;
    let mut out = copier.convert_page(w, dict);
    for key in [&b"MediaBox"[..], b"Resources"] {
        if !out.has(key) {
            if let Some(value) = inherited(&source.doc, sp.id, key) {
                let converted = copier.convert(w, &value);
                out.set(key.to_vec(), converted);
            }
        }
    }
    // The view becomes a crop box and a rotation: text stays text.
    let crop = page.view.crop_in_display(sp.display_size).transform(sp.user_to_display.invert().context("degenerate page")?);
    // Both boxes: some tools (poppler's pdftoppm, printers) use the media box.
    out.set("MediaBox", rect_object(crop.x0, crop.y0, crop.x1, crop.y1));
    out.set("CropBox", rect_object(crop.x0, crop.y0, crop.x1, crop.y1));
    out.set("Rotate", Object::Integer((((sp.quarter_turns + page.view.quarter_turns) % 4) * 90) as i64));
    out.set("Parent", reference(parent));
    w.write(out_id, &Object::Dictionary(out))?;
    copier.flush(w)
}

fn write_raster_page(
    w: &mut PdfWriter,
    copier: &mut Copier,
    shared_images: &mut HashMap<(usize, ObjectId, bool), OutId>,
    source_index: usize,
    output: PageOutput,
    layout: &Layout,
    page_index: usize,
) -> Result<()> {
    let image_id = match output.image {
        ImageOut::Encoded(encoded) => {
            let id = w.alloc();
            w.write_stream(id, image_dict(&encoded), encoded.data)?;
            id
        }
        ImageOut::Derived { id, image } => match shared_images.get(&(source_index, id, true)) {
            Some(&out) => out,
            None => {
                let out = w.alloc();
                w.write_stream(out, image_dict(&image), image.data)?;
                shared_images.insert((source_index, id, true), out);
                out
            }
        },
        ImageOut::PassThrough { id, smaller } => match shared_images.get(&(source_index, id, false)) {
            Some(&out) => out,
            None => {
                if let Some(encoded) = smaller {
                    let original = &copier_source_dict(copier, id)?;
                    copier.replacements.insert(id, Object::Stream(lopdf::Stream::new(replacement_dict(original, &encoded), encoded.data)));
                }
                let out = copier.map_ref(w, id);
                copier.flush(w)?;
                shared_images.insert((source_index, id, false), out);
                out
            }
        },
    };

    let (pw, ph) = output.size;
    let m = output.image_ctm;
    let f = format_number;
    let mut content = format!(
        "q 0 0 {} {} re W n {} {} {} {} {} {} cm /Im0 Do Q\n",
        f(pw),
        f(ph),
        f(m.a),
        f(m.b),
        f(m.c),
        f(m.d),
        f(m.e),
        f(m.f)
    )
    .into_bytes();
    if let Some(text) = &output.text {
        content.extend_from_slice(text);
    }
    let content_id = w.alloc();
    let mut content_dict = Dictionary::new();
    content_dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
    w.write_stream(content_id, content_dict, encode::zlib(&content))?;

    let mut xobjects = Dictionary::new();
    xobjects.set("Im0", reference(image_id));
    let mut resources = Dictionary::new();
    resources.set("XObject", Object::Dictionary(xobjects));
    if let (Some(font), Some(_)) = (layout.font, &output.text) {
        let mut fonts = Dictionary::new();
        fonts.set(ocr::FONT_NAME, reference(font));
        resources.set("Font", Object::Dictionary(fonts));
    }
    let mut page = Dictionary::new();
    page.set("Type", Object::Name(b"Page".to_vec()));
    page.set("Parent", reference(layout.pages_root));
    page.set("MediaBox", rect_object(0.0, 0.0, pw, ph));
    page.set("Resources", Object::Dictionary(resources));
    page.set("Contents", reference(content_id));
    w.write(layout.page_ids[page_index], &Object::Dictionary(page))
}

fn copier_source_dict(copier: &Copier, id: ObjectId) -> Result<Dictionary> {
    Ok(copier.source().get_object(id)?.as_stream()?.dict.clone())
}

/// Tesseract's glyphless font: every code point is an invisible glyph half
/// an em wide, so the text layer can be stretched over any word.
fn write_ocr_font(w: &mut PdfWriter, font: OutId) -> Result<()> {
    let (cid_font, descriptor, file, cid_map, to_unicode) = (w.alloc(), w.alloc(), w.alloc(), w.alloc(), w.alloc());
    let name = |s: &str| Object::Name(s.as_bytes().to_vec());

    let mut d = Dictionary::new();
    d.set("Type", name("Font"));
    d.set("Subtype", name("Type0"));
    d.set("BaseFont", name("GlyphLessFont"));
    d.set("Encoding", name("Identity-H"));
    d.set("DescendantFonts", Object::Array(vec![reference(cid_font)]));
    d.set("ToUnicode", reference(to_unicode));
    w.write(font, &Object::Dictionary(d))?;

    let mut info = Dictionary::new();
    info.set("Registry", Object::string_literal("Adobe"));
    info.set("Ordering", Object::string_literal("Identity"));
    info.set("Supplement", Object::Integer(0));
    let mut d = Dictionary::new();
    d.set("Type", name("Font"));
    d.set("Subtype", name("CIDFontType2"));
    d.set("BaseFont", name("GlyphLessFont"));
    d.set("CIDSystemInfo", Object::Dictionary(info));
    d.set("FontDescriptor", reference(descriptor));
    d.set("CIDToGIDMap", reference(cid_map));
    d.set("DW", Object::Integer(500));
    w.write(cid_font, &Object::Dictionary(d))?;

    let mut d = Dictionary::new();
    d.set("Type", name("FontDescriptor"));
    d.set("FontName", name("GlyphLessFont"));
    d.set("Flags", Object::Integer(5));
    d.set("FontBBox", Object::Array(vec![0.into(), 0.into(), 500.into(), 1000.into()]));
    d.set("ItalicAngle", Object::Integer(0));
    d.set("Ascent", Object::Integer(1000));
    d.set("Descent", Object::Integer(0));
    d.set("CapHeight", Object::Integer(1000));
    d.set("StemV", Object::Integer(80));
    d.set("FontFile2", reference(file));
    w.write(descriptor, &Object::Dictionary(d))?;

    let mut d = Dictionary::new();
    d.set("Length1", Object::Integer(ocr::GLYPHLESS_FONT.len() as i64));
    d.set("Filter", name("FlateDecode"));
    w.write_stream(file, d, encode::zlib(ocr::GLYPHLESS_FONT))?;

    // Every CID maps to glyph 1.
    let map: Vec<u8> = (0..65536).flat_map(|_| [0u8, 1u8]).collect();
    let mut d = Dictionary::new();
    d.set("Filter", name("FlateDecode"));
    w.write_stream(cid_map, d, encode::zlib(&map))?;

    let mut d = Dictionary::new();
    d.set("Filter", name("FlateDecode"));
    w.write_stream(to_unicode, d, encode::zlib(ocr::TO_UNICODE_CMAP.as_bytes()))?;
    Ok(())
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
