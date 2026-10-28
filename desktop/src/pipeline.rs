//! Turns one page into an output image (and the input of its text layer).
//!
//! `page_job` reads what a raster page needs from its source; `run_page_job`
//! does the work without the source, so it runs on any thread, or in another
//! WebAssembly instance.

use crate::encode::{self, EncodedImage, ImageFilter};
use crate::geometry::{Matrix, Rect};
use crate::model::{ColorMode, Compression, Page, PageKind, PixelOps};
use crate::pdfimage::{self, ImageCodec, ImageInfo, StoredImage};
use crate::pixel::{self, Raster};
use crate::source::Source;
use anyhow::{Context, Result, bail};
use lopdf::ObjectId;
use serde::{Deserialize, Serialize};

/// Resolution for OCR and for rasterizing vector pages.
pub const WORK_PPI: f64 = 300.0;

/// JPEG quality of the ink color layer: it is tiny, and artifacts would
/// show as blotchy letters.
const INK_JPEG_QUALITY: u8 = 85;

/// Largest page render (an A3 page at 500 DPI).
const MAX_RENDER_PIXELS: f64 = 50e6;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PageRoute {
    /// Typeset page copied as PDF objects.
    Copied,
    /// Original image stream reused (maybe wrapped in Flate).
    PassThrough,
    /// Original image decoded, processed and re-encoded.
    Reencoded,
    /// Page rendered, processed and encoded.
    Rendered,
    /// Luma channel of the original JPEG, without re-encoding.
    ExactGray,
}

#[derive(Serialize, Deserialize)]
pub enum ImageOut {
    /// A new image derived from a source image without decoding it (exact
    /// grayscale), shared by every page that shows that image.
    Derived { id: ObjectId, image: EncodedImage },
    /// The source image object, optionally with smaller lossless data.
    PassThrough { id: ObjectId, smaller: Option<EncodedImage> },
    Encoded(EncodedImage),
}

#[derive(Serialize, Deserialize)]
pub struct PageOutput {
    /// Output page size in points.
    pub size: (f64, f64),
    pub image: ImageOut,
    /// Maps the image's unit square to output page space.
    pub image_ctm: Matrix,
    /// What OCR reads, when the save has OCR. The caller recognizes it and
    /// sets `text`.
    #[serde(skip)]
    pub ocr: Option<OcrInput>,
    /// Text layer operators.
    pub text: Option<Vec<u8>>,
    /// Sharp text drawn over `image` (text and image layers).
    pub mask: Option<MaskLayer>,
    pub route: PageRoute,
}

/// Ink colors painted through a bilevel mask (JBIG2).
#[derive(Serialize, Deserialize)]
pub struct MaskLayer {
    pub mask: EncodedImage,
    pub foreground: EncodedImage,
    /// Maps the unit square of both to output page space.
    pub ctm: Matrix,
}

/// An upright gray image for OCR, and where its pixels land on the page.
pub struct OcrInput {
    pub raster: Raster,
    /// Maps pixels (y down) to output points (y up).
    pub px_to_out: Matrix,
    pub ppi: u32,
}

/// Pixel grid of an image and where it lands on the output page.
struct Placed {
    raster: Raster,
    /// Maps pixel coordinates (y down) to output points (y up).
    px_to_out: Matrix,
    /// Declared to physical resolution (`SourcePage::ppi_factor`).
    ppi_factor: f64,
}

impl Placed {
    /// Physical resolution, which all resolution choices use.
    fn ppi(&self) -> f64 {
        72.0 / self.px_to_out.det().abs().sqrt() * self.ppi_factor
    }

    /// Crops to the pixels that land on the output page.
    fn crop_to_page(self, out_size: (f64, f64)) -> Result<Placed> {
        let inv = self.px_to_out.invert().context("degenerate image placement")?;
        let r = Rect::new(0.0, 0.0, out_size.0, out_size.1).transform(inv);
        let (w, h) = (self.raster.width as f64, self.raster.height as f64);
        let x0 = r.x0.max(0.0).min(w).floor() as u32;
        let y0 = r.y0.max(0.0).min(h).floor() as u32;
        let x1 = r.x1.max(0.0).min(w).ceil() as u32;
        let y1 = r.y1.max(0.0).min(h).ceil() as u32;
        if x1 <= x0 || y1 <= y0 {
            bail!("image does not reach the page");
        }
        let raster = self.raster.crop(x0, y0, x1, y1);
        Ok(Placed { raster, px_to_out: Matrix::translate(x0 as f64, y0 as f64).then(self.px_to_out), ppi_factor: self.ppi_factor })
    }

    /// Downsamples so the resolution does not exceed `ppi`.
    fn limit_ppi(self, ppi: f64) -> Result<Placed> {
        let current = self.ppi();
        if current <= ppi * 1.02 {
            return Ok(self);
        }
        let f = ppi / current;
        let (w, h) = (self.raster.width, self.raster.height);
        let nw = ((w as f64 * f).round() as u32).max(1);
        let nh = ((h as f64 * f).round() as u32).max(1);
        let raster = self.raster.resize(nw, nh)?;
        let px_to_out = Matrix::scale(w as f64 / nw as f64, h as f64 / nh as f64).then(self.px_to_out);
        Ok(Placed { raster, px_to_out, ppi_factor: self.ppi_factor })
    }


    /// Rotates the pixels so text is upright on the output page, for OCR.
    fn upright(self) -> Result<Placed> {
        let m = self.px_to_out;
        let (w, h) = (self.raster.width as f64, self.raster.height as f64);
        // Output y is up, pixel y is down: upright means a > 0 and d < 0.
        let (turns, new_to_old) = if m.a > 0.0 && m.d < 0.0 {
            (0, Matrix::IDENTITY)
        } else if m.a < 0.0 && m.d > 0.0 {
            (2, Matrix::new(-1.0, 0.0, 0.0, -1.0, w, h))
        } else if m.b < 0.0 && m.c < 0.0 {
            (1, Matrix::new(0.0, -1.0, 1.0, 0.0, 0.0, h))
        } else if m.b > 0.0 && m.c > 0.0 {
            (3, Matrix::new(0.0, 1.0, -1.0, 0.0, w, 0.0))
        } else {
            bail!("mirrored image placement is not supported for OCR");
        };
        Ok(Placed { raster: self.raster.rotate_cw(turns), px_to_out: new_to_old.then(m), ppi_factor: self.ppi_factor })
    }
}

/// Unit square of a `width × height` raster to output space (for `cm`).
fn unit_ctm(width: u32, height: u32, px_to_out: Matrix) -> Matrix {
    let (w, h) = (width as f64, height as f64);
    Matrix::new(w, 0.0, 0.0, -h, 0.0, h).then(px_to_out)
}

/// Settings shared by every page of a save.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub compression: Compression,
    pub ocr: bool,
    /// Overrides the level's maximum resolution (benchmarks, experts).
    pub max_dpi: Option<f64>,
    /// Overrides the level's JPEG quality (libjpeg scale).
    pub jpeg_quality: Option<u8>,
}

impl Settings {
    fn max_dpi(&self, mode: ColorMode) -> f64 {
        self.max_dpi.or(self.compression.max_dpi(mode)).unwrap_or(f64::INFINITY)
    }

    fn jpeg_quality(&self) -> Option<u8> {
        self.jpeg_quality.or(self.compression.jpeg_quality())
    }

    /// Resolution of the image layer under the text layer: the level's,
    /// so pictures look as in a gray or color page.
    fn background_dpi(&self, mode: ColorMode) -> f64 {
        self.max_dpi.or(self.compression.max_dpi(mode)).unwrap_or(200.0)
    }

    /// True when sources keep their resolution and quality.
    fn keeps_originals(&self) -> bool {
        self.compression == Compression::None && self.max_dpi.is_none() && self.jpeg_quality.is_none()
    }
}

/// What a raster page will be made from, decided without decoding pixels.
struct Plan {
    embedded: Option<(ObjectId, Matrix)>,
    info: Option<ImageInfo>,
    /// The pixel operations change nothing on the embedded image.
    ops_are_noop: bool,
    /// The intended route; re-encoding still falls back to pass-through
    /// when it is not smaller, and exact gray to re-encoding when it fails.
    route: PageRoute,
}

fn plan(source: &Source, page: &Page, settings: Settings) -> Result<Plan> {
    let sp = &source.pages[page.index];
    let embedded = if page.kind == PageKind::Scan { sp.scan_image() } else { None };
    let info = match embedded {
        Some((id, _)) => pdfimage::inspect(&source.doc, id)?,
        None => None,
    };
    let cleanup = page.remove_shading || page.enhance_contrast;
    let ops_are_noop = !cleanup
        && match (page.color, info) {
            (ColorMode::Color, _) => true,
            (ColorMode::Gray, Some(i)) => i.channels == 1,
            (ColorMode::BwFlat, Some(i)) => i.codec == ImageCodec::Flate1,
            _ => false,
        };
    let color_jpeg = info.is_some_and(|i| matches!(i.codec, ImageCodec::Jpeg | ImageCodec::FlateJpeg) && i.channels == 3);
    let route = match (embedded, info) {
        (Some(_), _) if ops_are_noop && (settings.keeps_originals() || info.is_none()) => PageRoute::PassThrough,
        (Some(_), Some(_)) if page.color == ColorMode::Gray && !cleanup && settings.keeps_originals() && color_jpeg => PageRoute::ExactGray,
        (Some(_), Some(_)) => PageRoute::Reencoded,
        _ => PageRoute::Rendered,
    };
    Ok(Plan { embedded, info, ops_are_noop, route })
}

/// The route a page is meant to take at save time.
pub fn intended_route(source: &Source, page: &Page, settings: Settings) -> Result<PageRoute> {
    if page.is_preserved() {
        return Ok(PageRoute::Copied);
    }
    Ok(plan(source, page, settings)?.route)
}

/// Everything a raster page needs from its source.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PageJob {
    pub settings: Settings,
    pub ops: PixelOps,
    pub out_size: (f64, f64),
    /// Maps display space to output space.
    pub to_out: Matrix,
    /// Size of the source page as displayed, in points.
    pub display_size: (f64, f64),
    pub ppi_factor: f64,
    pub route: PageRoute,
    pub ops_are_noop: bool,
    /// The scan image that is the whole page, and its placement in display space.
    pub embedded: Option<(ObjectId, Matrix)>,
    /// The scan image's data, when this module can decode it.
    pub image: Option<StoredImage>,
    /// Pixels per point of the render of the page (display space) that
    /// `run_page_job` needs, when there is no image to decode.
    pub render: Option<f64>,
    /// JPEG quality of the image the page is made of, for "No compression".
    pub source_quality: Option<u8>,
}

/// Reads what a raster (not preserved) page needs from its source.
pub fn page_job(source: &Source, page: &Page, settings: Settings) -> Result<PageJob> {
    let sp = &source.pages[page.index];
    let Plan { embedded, info, ops_are_noop, route } = plan(source, page, settings)?;
    let image = match (embedded, info) {
        (Some((id, _)), Some(info)) => Some(pdfimage::extract(&source.doc, id, &info)?),
        _ => None,
    };
    let main = embedded.or_else(|| if page.kind == PageKind::Scan { sp.main_image() } else { None });
    let needs_pixels = route != PageRoute::PassThrough || settings.ocr;
    let render = match image {
        None if needs_pixels => {
            // Render at the resolution of the page's main image, if it has
            // one, else at the working resolution.
            let ppi = match main {
                Some((id, unit_to_display)) => native_ppi(source, id, unit_to_display)?.unwrap_or(WORK_PPI),
                None => WORK_PPI / sp.ppi_factor,
            }
            .clamp(72.0 / sp.ppi_factor, 600.0);
            // Bound memory: no render above MAX_RENDER_PIXELS.
            let (dw, dh) = sp.display_size;
            let cap = (MAX_RENDER_PIXELS / (dw / 72.0 * dh / 72.0)).sqrt();
            Some(ppi.min(cap) / 72.0)
        }
        _ => None,
    };
    // No compression: match the quality of the JPEG the page is made of.
    let source_quality = match main {
        Some((id, _)) if settings.jpeg_quality().is_none() && !page.color.is_bilevel() => source_jpeg_quality(source, id)?,
        _ => None,
    };
    Ok(PageJob {
        settings,
        ops: page.pixel_ops(),
        out_size: page.view.output_size(sp.display_size),
        to_out: page.view.display_to_output(sp.display_size),
        display_size: sp.display_size,
        ppi_factor: sp.ppi_factor,
        route,
        ops_are_noop,
        embedded,
        image,
        render,
        source_quality,
    })
}

/// Processes a raster page. `rendered` is the render `job.render` asks for.
pub fn run_page_job(job: &PageJob, rendered: Option<Raster>) -> Result<PageOutput> {
    let mut rendered = rendered;
    let ocr = job.settings.ocr;
    let color = job.ops.color;
    let out = |image, image_ctm, ocr, route| PageOutput { size: job.out_size, image, image_ctm, ocr, text: None, mask: None, route };

    if let Some((id, unit_to_display)) = job.embedded {
        let image_ctm = unit_to_display.then(job.to_out);
        if job.route == PageRoute::PassThrough {
            let smaller = match &job.image {
                Some(image) => lossless_shrink(image)?,
                None => None,
            };
            let ocr = if ocr { Some(ocr_input(acquire(job, rendered.take(), true)?.limit_ppi(WORK_PPI)?)?) } else { None };
            return Ok(out(ImageOut::PassThrough { id, smaller }, image_ctm, ocr, PageRoute::PassThrough));
        }
        // Grayscale without re-encoding: keep the JPEG's luma channel.
        if let (PageRoute::ExactGray, Some(stored)) = (job.route, &job.image)
            && let Some(image) = exact_gray(stored)? {
                let ocr = if ocr { Some(ocr_input(acquire(job, rendered.take(), true)?.limit_ppi(WORK_PPI)?)?) } else { None };
                return Ok(out(ImageOut::Derived { id, image }, image_ctm, ocr, PageRoute::ExactGray));
            }
    }

    let gray_only = color.image_mode() != ColorMode::Color;
    let placed = acquire(job, rendered.take(), gray_only)?;
    let route = if job.image.is_some() { PageRoute::Reencoded } else { PageRoute::Rendered };
    // Text layers keep text at up to the working resolution.
    let layers = color.is_layered();
    let target_ppi = if layers { WORK_PPI } else { job.settings.max_dpi(color) };

    // OCR reads the cleaned gray image at up to WORK_PPI; the output uses
    // the level's resolution. When both are the same, clean up once.
    let same = placed.ppi().min(WORK_PPI) == placed.ppi().min(target_ppi);
    let ocr_placed = match (ocr, same) {
        (true, false) => Some(Placed { raster: placed.raster.clone(), ..placed }.limit_ppi(WORK_PPI)?),
        _ => None,
    };
    let work = placed.limit_ppi(target_ppi)?;
    let work_ppi = work.ppi();
    let scale = work_ppi / 300.0;
    let cleaned = pixel::apply_cleanup(work.raster, job.ops, scale);
    let ocr = match (ocr, ocr_placed) {
        (false, _) => None,
        (true, Some(p)) => {
            let s = p.ppi() / 300.0;
            Some(ocr_input(Placed { raster: pixel::apply_cleanup(p.raster, job.ops, s).to_gray(), ..p })?)
        }
        (true, None) => Some(ocr_input(Placed { raster: cleaned.to_gray(), px_to_out: work.px_to_out, ppi_factor: work.ppi_factor })?),
    };
    let image_ctm = unit_ctm(cleaned.width, cleaned.height, work.px_to_out);
    let raster = pixel::apply_color_mode(cleaned, color, scale);

    if layers {
        let f = (job.settings.background_dpi(color) / work_ppi).min(1.0);
        let size = (((raster.width as f64 * f).round() as u32).max(1), ((raster.height as f64 * f).round() as u32).max(1));
        // A coarse bilevel mask has staircase edges, and its threshold
        // takes the soft edge pixels for ink: draw it from the page
        // enlarged to the level's text resolution.
        let text_dpi = job.settings.compression.text_dpi();
        let split = if work_ppi < 0.9 * text_dpi {
            let k = text_dpi / work_ppi;
            let up = raster.resize((raster.width as f64 * k).round() as u32, (raster.height as f64 * k).round() as u32)?;
            crate::mrc::separate(&up, text_dpi / 300.0, size)?
        } else {
            crate::mrc::separate(&raster, scale, size)?
        };
        let quality = job.settings.jpeg_quality().or(job.source_quality).unwrap_or(75);
        let background = encode::encode_jpeg(&split.background, quality)?;
        let mask = MaskLayer { mask: encode::encode_jbig2(&split.mask)?, foreground: encode::encode_jpeg(&split.foreground, INK_JPEG_QUALITY)?, ctm: image_ctm };
        let mut output = out(ImageOut::Encoded(background), image_ctm, ocr, route);
        output.mask = Some(mask);
        return Ok(output);
    }

    let encoded = if color.is_bilevel() {
        encode::encode_jbig2(&raster)?
    } else {
        // No compression: the quality of the source JPEG, else lossless.
        match job.settings.jpeg_quality().or(job.source_quality) {
            Some(q) => encode::encode_jpeg(&raster, q)?,
            None => encode::encode_flate_png(&raster),
        }
    };

    // Recompressing an untouched scan must pay for itself.
    if let (Some((id, unit_to_display)), Some(image)) = (job.embedded, &job.image)
        && job.ops_are_noop {
            let share = (job.out_size.0 * job.out_size.1) / unit_to_display.then(job.to_out).det().abs().max(1e-9);
            if encoded.data.len() as f64 >= image.info.encoded_len as f64 * share.min(1.0) {
                let smaller = lossless_shrink(image)?;
                return Ok(out(ImageOut::PassThrough { id, smaller }, unit_to_display.then(job.to_out), ocr, PageRoute::PassThrough));
            }
        }
    Ok(out(ImageOut::Encoded(encoded), image_ctm, ocr, route))
}

/// The page's pixels, cropped to the output page.
fn acquire(job: &PageJob, rendered: Option<Raster>, gray: bool) -> Result<Placed> {
    let placed = match (job.embedded, &job.image) {
        (Some((_, unit_to_display)), Some(image)) => {
            let raster = image.decode(gray)?;
            let (w, h) = (raster.width as f64, raster.height as f64);
            let px_to_unit = Matrix::new(1.0 / w, 0.0, 0.0, -1.0 / h, 0.0, 1.0);
            Placed { raster, px_to_out: px_to_unit.then(unit_to_display).then(job.to_out), ppi_factor: job.ppi_factor }
        }
        _ => {
            let mut raster = rendered.context("the page needs a render")?;
            if gray {
                raster = raster.to_gray();
            }
            let (w, h) = (raster.width as f64, raster.height as f64);
            let (dw, dh) = job.display_size;
            let px_to_display = Matrix::new(dw / w, 0.0, 0.0, -dh / h, 0.0, dh);
            Placed { raster, px_to_out: px_to_display.then(job.to_out), ppi_factor: job.ppi_factor }
        }
    };
    placed.crop_to_page(job.out_size)
}

/// The luma channel of a YCbCr JPEG image as a grayscale JPEG, made from
/// its coefficients (exact), unless it is more than 10% larger than the
/// original: scans of gray paper carry almost no chroma, and the output is
/// baseline where originals are often progressive.
/// `None` when the image is not a YCbCr JPEG.
fn exact_gray(image: &StoredImage) -> Result<Option<EncodedImage>> {
    let info = &image.info;
    if !matches!(info.codec, ImageCodec::Jpeg | ImageCodec::FlateJpeg) || info.channels != 3 {
        return Ok(None);
    }
    let Ok(gray) = crate::jpeggray::luma_only(&image.jpeg_data()?) else { return Ok(None) };
    let image = encode::wrap_jpeg(EncodedImage { width: info.width, height: info.height, channels: 1, bits_per_component: 8, filter: ImageFilter::Dct, data: gray });
    Ok((image.data.len() * 100 <= info.encoded_len * 110).then_some(image))
}

/// Quality of a JPEG image XObject, `None` for other codecs.
fn source_jpeg_quality(source: &Source, id: ObjectId) -> Result<Option<u8>> {
    let Some(info) = pdfimage::inspect(&source.doc, id)? else { return Ok(None) };
    if !matches!(info.codec, ImageCodec::Jpeg | ImageCodec::FlateJpeg) {
        return Ok(None);
    }
    let image = pdfimage::extract(&source.doc, id, &info)?;
    Ok(Some(encode::jpeg_quality(&image.jpeg_data()?).unwrap_or(90).clamp(50, 95)))
}

fn native_ppi(source: &Source, id: ObjectId, unit_to_display: Matrix) -> Result<Option<f64>> {
    let dict = &pdfimage::image_stream(&source.doc, id)?.dict;
    let width = dict.get(b"Width").and_then(|o| o.as_i64()).ok();
    let bbox = unit_to_display.unit_bbox();
    let extent = if unit_to_display.a.abs() > unit_to_display.b.abs() { bbox.width() } else { bbox.height() };
    Ok(width.map(|w| w as f64 * 72.0 / extent.max(1e-6)))
}

fn ocr_input(placed: Placed) -> Result<OcrInput> {
    let up = Placed { raster: placed.raster.to_gray(), ..placed }.upright()?;
    let ppi = up.ppi().round() as u32;
    Ok(OcrInput { raster: up.raster, px_to_out: up.px_to_out, ppi })
}

/// A strictly smaller lossless encoding of an image, if one exists: JPEGs
/// wrapped in Flate, Flate images refiltered, bilevel ones as JBIG2.
pub fn lossless_shrink(image: &StoredImage) -> Result<Option<EncodedImage>> {
    let info = &image.info;
    let candidate = match info.codec {
        // The JPEG itself stays as it is; only a Flate layer may change.
        ImageCodec::Jpeg | ImageCodec::FlateJpeg => {
            let data = image.jpeg_data()?.into_owned();
            let plain = EncodedImage { width: info.width, height: info.height, channels: info.channels, bits_per_component: 8, filter: ImageFilter::Dct, data };
            encode::wrap_jpeg(plain)
        }
        ImageCodec::Flate8 => encode::encode_flate_png(&image.decode(false)?),
        ImageCodec::Flate1 => encode::encode_jbig2(&image.decode(false)?)?,
    };
    Ok((candidate.data.len() < info.encoded_len).then_some(candidate))
}

/// An image of a preserved page to recompress.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageJob {
    pub image: StoredImage,
    pub compression: Compression,
    /// Its resolution where it is drawn largest.
    pub shown_ppi: Option<f64>,
}

/// The recompression job of an image, `None` when it cannot be decoded.
pub fn image_job(source: &Source, id: ObjectId, compression: Compression, shown_ppi: Option<f64>) -> Result<Option<ImageJob>> {
    let Some(info) = pdfimage::inspect_with(&source.doc, id, true)? else { return Ok(None) };
    Ok(Some(ImageJob { image: pdfimage::extract(&source.doc, id, &info)?, compression, shown_ppi }))
}

/// Recompresses an image of a preserved page when that makes it smaller.
/// At compression levels, images far sharper than twice the level's
/// resolution are downsampled. Images with a soft mask (graphics) stay
/// lossless.
pub fn run_image_job(job: &ImageJob) -> Result<Option<EncodedImage>> {
    let (image, info) = (&job.image, &job.image.info);
    let Some(quality) = job.compression.jpeg_quality() else { return lossless_shrink(image) };
    let mut raster = image.decode(false)?;
    let target = 2.0 * job.compression.max_dpi(ColorMode::Color).unwrap_or(f64::INFINITY);
    if let Some(ppi) = job.shown_ppi.filter(|&p| p > 1.5 * target && info.codec != ImageCodec::Flate1) {
        let f = target / ppi;
        let (w, h) = (((raster.width as f64 * f).round() as u32).max(1), ((raster.height as f64 * f).round() as u32).max(1));
        raster = raster.resize(w, h)?;
    }
    let candidate = match info.codec {
        ImageCodec::Flate1 => encode::encode_jbig2(&raster)?,
        _ if info.has_smask || info.codec == ImageCodec::Flate8 && raster.width != info.width => encode::encode_flate_png(&raster),
        _ => encode::encode_jpeg(&raster, quality)?,
    };
    if candidate.data.len() < info.encoded_len {
        return Ok(Some(candidate));
    }
    lossless_shrink(image)
}

/// Processes a raster page with MuPDF rendering and Tesseract.
#[cfg(feature = "native")]
pub fn process_page(source: &Source, page: &Page, settings: Settings, engine: Option<&mut crate::ocr::Engine>) -> Result<PageOutput> {
    let job = page_job(source, page, settings)?;
    let rendered = match job.render {
        Some(ppp) => Some(crate::render::render_page(&source.render_path, page.index, ppp, job.display_size)?),
        None => None,
    };
    let mut output = run_page_job(&job, rendered)?;
    if let (Some(engine), Some(input)) = (engine, output.ocr.take()) {
        let lines = engine.recognize(&input.raster, input.ppi)?;
        output.text = Some(crate::textlayer::text_layer(&lines, input.px_to_out));
    }
    Ok(output)
}

pub fn color_tag(pages: &[Page]) -> &'static str {
    let mut counts = std::collections::HashMap::new();
    for p in pages {
        *counts.entry(p.color).or_insert(0usize) += 1;
    }
    let best = [ColorMode::Gray, ColorMode::GrayText, ColorMode::BwFlat, ColorMode::BwAdaptive, ColorMode::ColorText, ColorMode::Color]
        .into_iter()
        .max_by_key(|m| counts.get(m).copied().unwrap_or(0))
        .unwrap_or(ColorMode::Color);
    best.file_tag()
}

/// Preview of a page from a render of its source page (display space, any
/// resolution): the same view and pixel operations as the saved page.
pub fn preview(base: &Raster, page: &Page, display_size: (f64, f64), ppi_factor: f64) -> Raster {
    let c = page.view.crop;
    let (w, h) = (base.width as f64, base.height as f64);
    let cropped = base.crop((c.x0 * w).round() as u32, (c.y0 * h).round() as u32, (c.x1 * w).round() as u32, (c.y1 * h).round() as u32);
    let turned = cropped.rotate_cw(page.view.quarter_turns);
    let ppi = base.width as f64 * 72.0 / display_size.0 * ppi_factor;
    let scale = ppi / 300.0;
    let out = pixel::apply_pixel_ops(turned, page.pixel_ops(), scale);
    // Previews are coarser than any image layer: the background keeps their resolution.
    match page.color.is_layered() {
        true => crate::mrc::separate(&out, scale, (out.width, out.height)).map_or(out, |l| crate::mrc::composite(&l)),
        false => out,
    }
}
