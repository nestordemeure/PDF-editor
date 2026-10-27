//! Turns one page into an output image (and text layer). Runs on worker
//! threads; owns no output state.

use crate::encode::{self, EncodedImage, ImageFilter};
use crate::geometry::{Matrix, Rect};
use crate::model::{ColorMode, Compression, Page};
use crate::ocr::{self, Engine};
use crate::pdfimage::{self, ImageCodec, ImageInfo};
use crate::pixel::{self, Raster};
use crate::render;
use crate::source::{Source, SourcePage};
use anyhow::{Context, Result, bail};
use lopdf::ObjectId;

/// Resolution for OCR and for rasterizing vector pages.
pub const WORK_PPI: f64 = 300.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PageRoute {
    /// Typeset page copied as PDF objects.
    Copied,
    /// Original image stream reused (maybe losslessly repacked).
    PassThrough,
    /// Original image decoded, processed and re-encoded.
    Reencoded,
    /// Page rendered by MuPDF, processed and encoded.
    Rendered,
}

pub enum ImageOut {
    /// The source image object, optionally with smaller lossless data.
    PassThrough { id: ObjectId, smaller: Option<EncodedImage> },
    Encoded(EncodedImage),
}

pub struct PageOutput {
    /// Output page size in points.
    pub size: (f64, f64),
    pub image: ImageOut,
    /// Maps the image's unit square to output page space.
    pub image_ctm: Matrix,
    /// Text layer operators, when OCR ran.
    pub text: Option<Vec<u8>>,
    pub route: PageRoute,
}

/// Pixel grid of an image and where it lands on the output page.
struct Placed {
    raster: Raster,
    /// Maps pixel coordinates (y down) to output points (y up).
    px_to_out: Matrix,
}

impl Placed {
    fn ppi(&self) -> f64 {
        72.0 / self.px_to_out.det().abs().sqrt()
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
        Ok(Placed { raster, px_to_out: Matrix::translate(x0 as f64, y0 as f64).then(self.px_to_out) })
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
        Ok(Placed { raster, px_to_out })
    }

    /// Unit square of the raster to output space (for the `cm` operator).
    fn unit_ctm(&self) -> Matrix {
        let (w, h) = (self.raster.width as f64, self.raster.height as f64);
        Matrix::new(w, 0.0, 0.0, -h, 0.0, h).then(self.px_to_out)
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
        Ok(Placed { raster: self.raster.rotate_cw(turns), px_to_out: new_to_old.then(m) })
    }
}

/// Settings shared by every page of a save.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub compression: Compression,
    pub ocr: bool,
}

/// Processes a raster (not preserved) page.
pub fn process_page(source: &Source, page: &Page, settings: Settings, engine: Option<&mut Engine>) -> Result<PageOutput> {
    let sp = &source.pages[page.index];
    let display = sp.display_size;
    let to_out = page.view.display_to_output(display);
    let out_size = page.view.output_size(display);

    let embedded = if page.kind == crate::model::PageKind::Scan { sp.scan_image() } else { None };
    let info = match embedded {
        Some((id, _)) => pdfimage::inspect(&source.doc, id)?,
        None => None,
    };

    // Untouched scan pages keep their image stream.
    if let Some((id, unit_to_display)) = embedded {
        if !page.has_pixel_ops() {
            let image_ctm = unit_to_display.then(to_out);
            let reuse = settings.compression == Compression::None || info.is_none();
            if reuse {
                let smaller = match info {
                    Some(info) => lossless_shrink(source, id, &info)?,
                    None => None,
                };
                let text = match engine {
                    Some(engine) => Some(ocr_layer(engine, acquire(source, sp, page, embedded, info, to_out, out_size)?)?),
                    None => None,
                };
                return Ok(PageOutput { size: out_size, image: ImageOut::PassThrough { id, smaller }, image_ctm, text, route: PageRoute::PassThrough });
            }
        }
    }

    let placed = acquire(source, sp, page, embedded, info, to_out, out_size)?;
    let route = if info.is_some() { PageRoute::Reencoded } else { PageRoute::Rendered };
    let source_ppi = placed.ppi();
    let ops = page.pixel_ops();

    let text = match engine {
        Some(engine) => {
            let ocr_input = Placed { raster: placed.raster.clone(), px_to_out: placed.px_to_out }.limit_ppi(WORK_PPI)?;
            let scale = ocr_input.ppi() / 300.0;
            let gray = pixel::apply_cleanup(ocr_input.raster, ops, scale).to_gray();
            Some(ocr_layer(engine, Placed { raster: gray, px_to_out: ocr_input.px_to_out })?)
        }
        None => None,
    };

    let target_ppi = settings.compression.max_dpi(page.color).unwrap_or(f64::INFINITY);
    let work = placed.limit_ppi(target_ppi)?;
    let scale = work.ppi() / 300.0;
    let raster = pixel::apply_pixel_ops(work.raster, ops, scale);
    let image_ctm = Placed { raster: raster.clone(), px_to_out: work.px_to_out }.unit_ctm();

    let encoded = if page.color.is_bilevel() {
        encode::encode_jbig2(&raster)?
    } else {
        let quality = match settings.compression.jpeg_quality() {
            Some(q) => Some(q),
            // No compression: match a JPEG source's quality, stay lossless otherwise.
            None => match (embedded, info) {
                (Some((id, _)), Some(ImageInfo { codec: ImageCodec::Jpeg, .. })) => {
                    let stream = pdfimage::image_stream(&source.doc, id)?;
                    Some(encode::jpeg_quality(&stream.content).unwrap_or(90).clamp(50, 95))
                }
                _ => None,
            },
        };
        match quality {
            Some(q) => encode::encode_jpeg(&raster, q)?,
            None => encode::encode_flate_png(&raster),
        }
    };

    // Recompressing an untouched color scan must pay for itself.
    if let (Some((id, unit_to_display)), Some(info)) = (embedded, info) {
        if !page.has_pixel_ops() {
            let share = (out_size.0 * out_size.1) / unit_to_display.then(to_out).det().abs().max(1e-9);
            if encoded.data.len() as f64 >= info.encoded_len as f64 * share.min(1.0) {
                let smaller = lossless_shrink(source, id, &info)?;
                return Ok(PageOutput {
                    size: out_size,
                    image: ImageOut::PassThrough { id, smaller },
                    image_ctm: unit_to_display.then(to_out),
                    text,
                    route: PageRoute::PassThrough,
                });
            }
        }
    }
    let _ = source_ppi;
    Ok(PageOutput { size: out_size, image: ImageOut::Encoded(encoded), image_ctm, text, route })
}

/// The page's pixels, cropped to the output page.
fn acquire(
    source: &Source,
    sp: &SourcePage,
    page: &Page,
    embedded: Option<(ObjectId, Matrix)>,
    info: Option<ImageInfo>,
    to_out: Matrix,
    out_size: (f64, f64),
) -> Result<Placed> {
    let placed = match (embedded, info) {
        (Some((id, unit_to_display)), Some(info)) => {
            let raster = pdfimage::decode(&source.doc, id, &info)?;
            let (w, h) = (raster.width as f64, raster.height as f64);
            let px_to_unit = Matrix::new(1.0 / w, 0.0, 0.0, -1.0 / h, 0.0, 1.0);
            Placed { raster, px_to_out: px_to_unit.then(unit_to_display).then(to_out) }
        }
        _ => {
            // Render at the scan's own resolution, or the working one.
            let ppi = match embedded {
                Some((id, unit_to_display)) => native_ppi(source, id, unit_to_display)?.unwrap_or(WORK_PPI),
                None => WORK_PPI,
            }
            .min(600.0);
            let ppp = ppi / 72.0;
            let raster = render::render_page(&source.render_path, page.index, ppp, sp.display_size)?;
            let (w, h) = (raster.width as f64, raster.height as f64);
            let (dw, dh) = sp.display_size;
            let px_to_display = Matrix::new(dw / w, 0.0, 0.0, -dh / h, 0.0, dh);
            Placed { raster, px_to_out: px_to_display.then(to_out) }
        }
    };
    placed.crop_to_page(out_size)
}

fn native_ppi(source: &Source, id: ObjectId, unit_to_display: Matrix) -> Result<Option<f64>> {
    let dict = &pdfimage::image_stream(&source.doc, id)?.dict;
    let width = dict.get(b"Width").and_then(|o| o.as_i64()).ok();
    let bbox = unit_to_display.unit_bbox();
    let extent = if unit_to_display.a.abs() > unit_to_display.b.abs() { bbox.width() } else { bbox.height() };
    Ok(width.map(|w| w as f64 * 72.0 / extent.max(1e-6)))
}

fn ocr_layer(engine: &mut Engine, placed: Placed) -> Result<Vec<u8>> {
    let up = Placed { raster: placed.raster.to_gray(), px_to_out: placed.px_to_out }.upright()?;
    let ppi = up.ppi().round() as u32;
    let lines = engine.recognize(&up.raster, ppi)?;
    Ok(ocr::text_layer(&lines, up.px_to_out))
}

/// A strictly smaller lossless encoding of an image, if one exists: JPEGs
/// repacked as progressive, Flate images refiltered, bilevel ones as JBIG2.
pub fn lossless_shrink(source: &Source, id: ObjectId, info: &ImageInfo) -> Result<Option<EncodedImage>> {
    let stream = pdfimage::image_stream(&source.doc, id)?;
    let candidate = match info.codec {
        ImageCodec::Jpeg => EncodedImage {
            width: info.width,
            height: info.height,
            channels: info.channels,
            bits_per_component: 8,
            filter: ImageFilter::Dct,
            data: encode::repack_jpeg(&stream.content)?,
        },
        ImageCodec::Flate8 => encode::encode_flate_png(&pdfimage::decode(&source.doc, id, info)?),
        ImageCodec::Flate1 => encode::encode_jbig2(&pdfimage::decode(&source.doc, id, info)?)?,
    };
    Ok((candidate.data.len() < info.encoded_len).then_some(candidate))
}

/// Recompresses an image of a preserved page when that makes it smaller.
pub fn recompress_preserved_image(source: &Source, id: ObjectId, compression: Compression) -> Result<Option<EncodedImage>> {
    let Some(info) = pdfimage::inspect(&source.doc, id)? else { return Ok(None) };
    let Some(quality) = compression.jpeg_quality() else { return lossless_shrink(source, id, &info) };
    let candidate = match info.codec {
        ImageCodec::Flate1 => encode::encode_jbig2(&pdfimage::decode(&source.doc, id, &info)?)?,
        _ => encode::encode_jpeg(&pdfimage::decode(&source.doc, id, &info)?, quality)?,
    };
    if candidate.data.len() < info.encoded_len {
        return Ok(Some(candidate));
    }
    lossless_shrink(source, id, &info)
}

/// True when the page goes through `process_page` rather than being copied.
pub fn is_raster(page: &Page) -> bool {
    !page.is_preserved()
}

pub fn color_tag(pages: &[Page]) -> &'static str {
    let mut counts = std::collections::HashMap::new();
    for p in pages {
        *counts.entry(p.color).or_insert(0usize) += 1;
    }
    let best = [ColorMode::Gray, ColorMode::BwFlat, ColorMode::BwAdaptive, ColorMode::Color]
        .into_iter()
        .max_by_key(|m| counts.get(m).copied().unwrap_or(0))
        .unwrap_or(ColorMode::Color);
    best.file_tag()
}
