//! The engine for the web app. One `Library` (in one worker) holds the
//! source documents and writes the output; page and image jobs run in other
//! workers through the free functions, which need no source.

use anyhow::{Context, Result, bail};
use pdf_editor::assemble::{Assembler, ImageTask, single_page_pdf};
use pdf_editor::geometry::Matrix;
use pdf_editor::model::{ColorMode, Compression, Page, PageKind, Side, View};
use pdf_editor::pipeline::{self, ImageJob, PageJob, PageOutput, Settings};
use pdf_editor::pixel::Raster;
use pdf_editor::source::Source;
use pdf_editor::textlayer::{self, OcrLine};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::sync::Arc;
use wasm_bindgen::prelude::*;

type JsResult<T> = std::result::Result<T, JsError>;

fn js_error(e: anyhow::Error) -> JsError {
    JsError::new(&format!("{e:#}"))
}

fn to_js<T: Serialize>(value: &T) -> JsResult<JsValue> {
    value.serialize(&serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true)).map_err(|e| JsError::new(&e.to_string()))
}

fn from_js<T: for<'de> Deserialize<'de>>(value: JsValue) -> JsResult<T> {
    serde_wasm_bindgen::from_value(value).map_err(|e| JsError::new(&e.to_string()))
}

#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
}

/// A page operation of the web app's page model.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum WebOp {
    Rotate { degrees: i32 },
    Split { side: String },
    ColorMode { mode: String },
    RemoveShading,
    EnhanceContrast,
}

/// A page as the web app describes it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebPage {
    source: usize,
    index: usize,
    /// "scan", "text" or "stripped".
    kind: String,
    operations: Vec<WebOp>,
}

fn color_mode(mode: &str) -> Result<ColorMode> {
    Ok(match mode {
        "color" => ColorMode::Color,
        "color-text" => ColorMode::ColorText,
        "gray" => ColorMode::Gray,
        "gray-text" => ColorMode::GrayText,
        "bw-otsu" => ColorMode::BwFlat,
        "bw" => ColorMode::BwAdaptive,
        _ => bail!("unknown color mode '{mode}'"),
    })
}

fn compression(level: &str) -> Result<Compression> {
    Ok(match level {
        "none" => Compression::None,
        "low" => Compression::Low,
        "medium" => Compression::Medium,
        "high" => Compression::High,
        _ => bail!("unknown compression level '{level}'"),
    })
}

fn to_page(web: WebPage) -> Result<Page> {
    let kind = match web.kind.as_str() {
        "scan" => PageKind::Scan,
        "text" => PageKind::Text,
        "stripped" => PageKind::TextStripped,
        other => bail!("unknown page kind '{other}'"),
    };
    let mut page = Page { source: web.source, index: web.index, kind, view: View::default(), color: ColorMode::Color, remove_shading: false, enhance_contrast: false };
    for op in web.operations {
        match op {
            WebOp::Rotate { degrees } => {
                for _ in 0..(degrees / 90).rem_euclid(4) {
                    page.view = page.view.rotated_cw();
                }
            }
            WebOp::Split { side } => {
                let side = match side.as_str() {
                    "left" => Side::Left,
                    "right" => Side::Right,
                    _ => bail!("unknown split side '{side}'"),
                };
                page.view = page.view.split(side);
            }
            WebOp::ColorMode { mode } => page.color = color_mode(&mode)?,
            WebOp::RemoveShading => page.remove_shading = true,
            WebOp::EnhanceContrast => page.enhance_contrast = true,
        }
    }
    Ok(page)
}

fn rgba_to_raster(rgba: Vec<u8>, width: u32, height: u32) -> Result<Raster> {
    if rgba.len() != width as usize * height as usize * 4 {
        bail!("{} bytes for a {width}x{height} RGBA image", rgba.len());
    }
    let mut rgb = rgba;
    let pixels = rgb.len() / 4;
    for i in 0..pixels {
        rgb.copy_within(i * 4..i * 4 + 3, i * 3);
    }
    rgb.truncate(pixels * 3);
    Raster::new(width, height, 3, rgb)
}

fn raster_to_rgba(r: &Raster) -> Vec<u8> {
    let channels = r.data.len() / (r.width as usize * r.height as usize).max(1);
    let mut out = Vec::with_capacity(r.width as usize * r.height as usize * 4);
    match channels {
        1 => r.data.iter().for_each(|&v| out.extend_from_slice(&[v, v, v, 255])),
        _ => r.data.chunks_exact(3).for_each(|p| out.extend_from_slice(&[p[0], p[1], p[2], 255])),
    }
    out
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    /// Size as displayed, in points.
    width: f64,
    height: f64,
    /// Has a text layer that is not just OCR over a scan.
    text: bool,
    /// "TEXT", "SCAN" (one decodable image) or "RENDER".
    route_hint: &'static str,
    ppi_factor: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoadedSource {
    id: usize,
    was_encrypted: bool,
    pages: Vec<PageInfo>,
}

/// Output bytes not yet handed to JavaScript.
#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Save {
    doc: Assembler,
    out: Sink,
    settings: Settings,
    images: Vec<ImageTask>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SavePlan {
    /// Output indices of the pages that need a page job.
    raster: Vec<usize>,
    /// Images of text pages that may be recompressed.
    images: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JobMessage {
    #[serde(with = "serde_bytes_compat")]
    job: Vec<u8>,
    /// Pixels per point of the render the job needs, in display space.
    render: Option<f64>,
    source: usize,
    index: usize,
}

/// Serializes bytes as a `Uint8Array`.
mod serde_bytes_compat {
    pub fn serialize<S: serde::Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(bytes)
    }
}

/// The save report and the last output bytes.
#[derive(Serialize)]
struct Finished {
    report: pdf_editor::assemble::SaveReport,
    #[serde(with = "serde_bytes_compat")]
    rest: Vec<u8>,
}

/// OCR result for a page, as JavaScript sends it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OcrResult {
    lines: Vec<OcrLine>,
    /// Pixel to page matrix of the recognized image, `[a b c d e f]`.
    px_to_out: [f64; 6],
}

/// Source documents and the save in progress.
#[wasm_bindgen]
#[derive(Default)]
pub struct Library {
    sources: Vec<Arc<Source>>,
    save: Option<Save>,
}

#[wasm_bindgen]
impl Library {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Library {
        Library::default()
    }

    /// Loads a PDF; `name` is its file name without extension.
    pub fn load(&mut self, name: String, bytes: &[u8]) -> JsResult<JsValue> {
        let source = Source::from_bytes(name, bytes).map_err(js_error)?;
        let pages = source
            .pages
            .iter()
            .map(|p| PageInfo {
                width: p.display_size.0,
                height: p.display_size.1,
                text: p.is_text_page,
                route_hint: if p.is_text_page { "TEXT" } else if p.scan_image().is_some() { "SCAN" } else { "RENDER" },
                ppi_factor: p.ppi_factor,
            })
            .collect();
        let id = self.sources.len();
        let loaded = LoadedSource { id, was_encrypted: source.was_encrypted, pages };
        self.sources.push(Arc::new(source));
        to_js(&loaded)
    }

    /// The source as a file without encryption, for rendering.
    pub fn rewritten(&mut self, id: usize) -> JsResult<Vec<u8>> {
        self.source_mut(id).map_err(js_error)?.rewritten().map_err(js_error)
    }

    /// Removes all text from the given pages. Returns how many streams
    /// could not be processed (their text remains).
    pub fn strip_text(&mut self, id: usize, pages: Vec<u32>) -> JsResult<usize> {
        let indices: Vec<usize> = pages.into_iter().map(|i| i as usize).collect();
        self.source_mut(id).map_err(js_error)?.strip_text(&indices).map_err(js_error)
    }

    /// Starts a save of `pages` (the web page model).
    pub fn begin_save(&mut self, pages: JsValue, level: String, ocr: bool) -> JsResult<JsValue> {
        let web: Vec<WebPage> = from_js(pages)?;
        let pages = web.into_iter().map(|p| self.checked_page(p)).collect::<Result<Vec<_>>>().map_err(js_error)?;
        let sources = self.sources.clone();
        let settings = Settings { compression: compression(&level).map_err(js_error)?, ocr, max_dpi: None, jpeg_quality: None };
        let out = Sink::default();
        let doc = Assembler::new(sources, pages, ocr, Box::new(out.clone())).map_err(js_error)?;
        let images = doc.preserved_images();
        let plan = SavePlan { raster: doc.raster_pages(), images: images.len() };
        self.save = Some(Save { doc, out, settings, images });
        to_js(&plan)
    }

    /// The recompression job of image `k` of text pages; empty when the
    /// image cannot be decoded.
    pub fn image_job(&self, k: usize) -> JsResult<Vec<u8>> {
        let save = self.save()?;
        let task = save.images.get(k).ok_or_else(|| JsError::new("no such image"))?;
        let source = &save.doc.sources()[task.source];
        match pipeline::image_job(source, task.id, save.settings.compression, task.shown_ppi).map_err(js_error)? {
            Some(job) => postcard::to_stdvec(&job).map_err(|e| JsError::new(&e.to_string())),
            None => Ok(Vec::new()),
        }
    }

    /// Replaces image `k` of text pages with the result of its job.
    pub fn replace_image(&mut self, k: usize, encoded: &[u8]) -> JsResult<()> {
        let save = self.save_mut()?;
        let task = *save.images.get(k).ok_or_else(|| JsError::new("no such image"))?;
        let encoded = postcard::from_bytes(encoded).map_err(|e| JsError::new(&e.to_string()))?;
        save.doc.replace_image(task, encoded).map_err(js_error)
    }

    /// Copies the text pages; call after the image replacements.
    pub fn write_text_pages(&mut self) -> JsResult<()> {
        self.save_mut()?.doc.write_preserved_pages(&mut || {}).map_err(js_error)
    }

    /// The job of raster page `i`.
    pub fn page_job(&self, i: usize) -> JsResult<JsValue> {
        let save = self.save()?;
        let page = save.doc.pages().get(i).ok_or_else(|| JsError::new("no such page"))?;
        let source = &save.doc.sources()[page.source];
        let job = pipeline::page_job(source, page, save.settings).with_context(|| format!("page {} ({} page {})", i + 1, source.name, page.index + 1)).map_err(js_error)?;
        let bytes = postcard::to_stdvec(&job).map_err(|e| JsError::new(&e.to_string()))?;
        to_js(&JobMessage { job: bytes, render: job.render, source: page.source, index: page.index })
    }

    /// Writes raster page `i` from its job's output and, with OCR, the
    /// recognized lines.
    pub fn add_page(&mut self, i: usize, output: &[u8], ocr: JsValue) -> JsResult<()> {
        let save = self.save_mut()?;
        let mut output: PageOutput = postcard::from_bytes(output).map_err(|e| JsError::new(&e.to_string()))?;
        if !ocr.is_null() && !ocr.is_undefined() {
            let ocr: OcrResult = from_js(ocr)?;
            let [a, b, c, d, e, f] = ocr.px_to_out;
            output.text = Some(textlayer::text_layer(&ocr.lines, Matrix::new(a, b, c, d, e, f)));
        }
        save.doc.write_raster_page(i, output).map_err(js_error)
    }

    /// Output bytes written since the last call.
    pub fn take_output(&mut self) -> Vec<u8> {
        match &self.save {
            Some(save) => std::mem::take(&mut *save.out.0.borrow_mut()),
            None => Vec::new(),
        }
    }

    /// Ends the save; the last bytes come from `take_output`.
    pub fn finish(&mut self) -> JsResult<JsValue> {
        let save = self.save.take().ok_or_else(|| JsError::new("no save in progress"))?;
        let report = save.doc.finish().map_err(js_error)?;
        let rest = std::mem::take(&mut *save.out.0.borrow_mut());
        to_js(&Finished { report, rest })
    }

    pub fn cancel_save(&mut self) {
        self.save = None;
    }

    /// The job of `page` saved alone at `level`, for a full-scale preview;
    /// null when the page is copied as it is. Independent of any save.
    pub fn preview_job(&self, page: JsValue, level: String) -> JsResult<JsValue> {
        let page = self.checked_page(from_js(page)?).map_err(js_error)?;
        if page.is_preserved() {
            return Ok(JsValue::NULL);
        }
        let settings = Settings { compression: compression(&level).map_err(js_error)?, ocr: false, max_dpi: None, jpeg_quality: None };
        let source = &self.sources[page.source];
        let job = pipeline::page_job(source, &page, settings).map_err(js_error)?;
        let bytes = postcard::to_stdvec(&job).map_err(|e| JsError::new(&e.to_string()))?;
        to_js(&JobMessage { job: bytes, render: job.render, source: page.source, index: page.index })
    }

    /// A PDF of `page` alone, as a save at `level` writes it: from `output`,
    /// the result of its preview job, or copied when it has none.
    pub fn preview_pdf(&self, page: JsValue, level: String, output: Option<Vec<u8>>) -> JsResult<Vec<u8>> {
        let page = self.checked_page(from_js(page)?).map_err(js_error)?;
        let output: Option<PageOutput> = output.map(|bytes| postcard::from_bytes(&bytes)).transpose().map_err(|e| JsError::new(&e.to_string()))?;
        single_page_pdf(self.sources.clone(), page, compression(&level).map_err(js_error)?, output).map_err(js_error)
    }
}

impl Library {
    fn checked_page(&self, web: WebPage) -> Result<Page> {
        let page = to_page(web)?;
        if self.sources.get(page.source).is_none_or(|s| page.index >= s.pages.len()) {
            bail!("page {} of source {} does not exist", page.index + 1, page.source);
        }
        Ok(page)
    }

    fn source_mut(&mut self, id: usize) -> Result<&mut Source> {
        if self.save.is_some() {
            bail!("a save is in progress");
        }
        let slot = self.sources.get_mut(id).context("no such source")?;
        Arc::get_mut(slot).context("the source is in use")
    }

    fn save(&self) -> JsResult<&Save> {
        self.save.as_ref().ok_or_else(|| JsError::new("no save in progress"))
    }

    fn save_mut(&mut self) -> JsResult<&mut Save> {
        self.save.as_mut().ok_or_else(|| JsError::new("no save in progress"))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OcrImage {
    #[serde(with = "serde_bytes_compat")]
    gray: Vec<u8>,
    width: u32,
    height: u32,
    ppi: u32,
    px_to_out: [f64; 6],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PageResult {
    #[serde(with = "serde_bytes_compat")]
    output: Vec<u8>,
    ocr: Option<OcrImage>,
}

/// Runs a page job. `rendered` is the RGBA render it asks for, if any.
#[wasm_bindgen]
pub fn run_page_job(job: &[u8], rendered: Option<Vec<u8>>, width: u32, height: u32) -> JsResult<JsValue> {
    let job: PageJob = postcard::from_bytes(job).map_err(|e| JsError::new(&e.to_string()))?;
    let rendered = rendered.map(|rgba| rgba_to_raster(rgba, width, height)).transpose().map_err(js_error)?;
    let mut output = pipeline::run_page_job(&job, rendered).map_err(js_error)?;
    let ocr = output.ocr.take().map(|input| {
        let m = input.px_to_out;
        OcrImage { width: input.raster.width, height: input.raster.height, ppi: input.ppi, px_to_out: [m.a, m.b, m.c, m.d, m.e, m.f], gray: input.raster.data }
    });
    let output = postcard::to_stdvec(&output).map_err(|e| JsError::new(&e.to_string()))?;
    to_js(&PageResult { output, ocr })
}

/// Runs an image job; empty when the image is best kept as it is.
#[wasm_bindgen]
pub fn run_image_job(job: &[u8]) -> JsResult<Vec<u8>> {
    let job: ImageJob = postcard::from_bytes(job).map_err(|e| JsError::new(&e.to_string()))?;
    match pipeline::run_image_job(&job).map_err(js_error)? {
        Some(encoded) => postcard::to_stdvec(&encoded).map_err(|e| JsError::new(&e.to_string())),
        None => Ok(Vec::new()),
    }
}

#[derive(Serialize)]
struct Preview {
    #[serde(with = "serde_bytes_compat")]
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

/// The thumbnail of a page from an RGBA render of its source page (display
/// space): the same view and pixel operations as the saved page.
#[wasm_bindgen]
pub fn preview(rgba: Vec<u8>, width: u32, height: u32, page: JsValue, display_width: f64, display_height: f64, ppi_factor: f64) -> JsResult<JsValue> {
    let page = to_page(from_js(page)?).map_err(js_error)?;
    let base = rgba_to_raster(rgba, width, height).map_err(js_error)?;
    let out = pipeline::preview(&base, &page, (display_width, display_height), ppi_factor);
    to_js(&Preview { rgba: raster_to_rgba(&out), width: out.width, height: out.height })
}
