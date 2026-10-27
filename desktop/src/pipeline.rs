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

/// Unit square of a `width × height` raster to output space (for `cm`).
fn unit_ctm(width: u32, height: u32, px_to_out: Matrix) -> Matrix {
    let (w, h) = (width as f64, height as f64);
    Matrix::new(w, 0.0, 0.0, -h, 0.0, h).then(px_to_out)
}

/// Settings shared by every page of a save.
#[derive(Clone, Copy, Debug)]
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

    /// True when sources keep their resolution and quality.
    fn keeps_originals(&self) -> bool {
        self.compression == Compression::None && self.max_dpi.is_none() && self.jpeg_quality.is_none()
    }
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
            let reuse = settings.keeps_originals() || info.is_none();
            if reuse {
                let smaller = match info {
                    Some(info) => lossless_shrink(source, id, &info)?,
                    None => None,
                };
                let text = match engine {
                    Some(engine) => Some(ocr_layer(engine, acquire(source, sp, page, embedded, info, to_out, out_size, true)?.limit_ppi(WORK_PPI)?)?),
                    None => None,
                };
                return Ok(PageOutput { size: out_size, image: ImageOut::PassThrough { id, smaller }, image_ctm, text, route: PageRoute::PassThrough });
            }
        }
    }

    let gray_only = page.color != ColorMode::Color;
    let placed = acquire(source, sp, page, embedded, info, to_out, out_size, gray_only)?;
    let route = if info.is_some() { PageRoute::Reencoded } else { PageRoute::Rendered };
    let ops = page.pixel_ops();
    let target_ppi = settings.max_dpi(page.color);

    // OCR reads the cleaned gray image at up to WORK_PPI; the output uses
    // the level's resolution. When both are the same, clean up once.
    let same = placed.ppi().min(WORK_PPI) == placed.ppi().min(target_ppi);
    let ocr_placed = match (&engine, same) {
        (Some(_), false) => Some(Placed { raster: placed.raster.clone(), px_to_out: placed.px_to_out }.limit_ppi(WORK_PPI)?),
        _ => None,
    };
    let work = placed.limit_ppi(target_ppi)?;
    let scale = work.ppi() / 300.0;
    let cleaned = pixel::apply_cleanup(work.raster, ops, scale);
    let text = match engine {
        Some(engine) => {
            let ocr_input = match ocr_placed {
                Some(p) => {
                    let s = p.ppi() / 300.0;
                    Placed { raster: pixel::apply_cleanup(p.raster, ops, s).to_gray(), px_to_out: p.px_to_out }
                }
                None => Placed { raster: cleaned.to_gray(), px_to_out: work.px_to_out },
            };
            Some(ocr_layer(engine, ocr_input)?)
        }
        None => None,
    };
    let image_ctm = unit_ctm(cleaned.width, cleaned.height, work.px_to_out);
    let raster = pixel::apply_color_mode(cleaned, page.color, scale);

    let encoded = if page.color.is_bilevel() {
        encode::encode_jbig2(&raster)?
    } else {
        let quality = match settings.jpeg_quality() {
            Some(q) => Some(q),
            // No compression: match a JPEG source's quality, stay lossless otherwise.
            None => match (embedded, info) {
                (Some((id, _)), Some(info @ ImageInfo { codec: ImageCodec::Jpeg | ImageCodec::FlateJpeg, .. })) => {
                    let stream = pdfimage::image_stream(&source.doc, id)?;
                    Some(encode::jpeg_quality(&pdfimage::jpeg_data(stream, &info)?).unwrap_or(90).clamp(50, 95))
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
    gray: bool,
) -> Result<Placed> {
    let placed = match (embedded, info) {
        (Some((id, unit_to_display)), Some(info)) => {
            let raster = pdfimage::decode(&source.doc, id, &info, gray)?;
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
            let mut raster = render::render_page(&source.render_path, page.index, ppp, sp.display_size)?;
            if gray {
                raster = raster.to_gray();
            }
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
        ImageCodec::Jpeg | ImageCodec::FlateJpeg => {
            let jpeg = pdfimage::jpeg_data(stream, info)?;
            // Progressive JPEGs already have optimized entropy coding.
            let data = if encode::is_progressive_jpeg(&jpeg) {
                jpeg.into_owned()
            } else {
                let repacked = encode::repack_jpeg(&jpeg)?;
                if repacked.len() < jpeg.len() { repacked } else { jpeg.into_owned() }
            };
            let plain = EncodedImage { width: info.width, height: info.height, channels: info.channels, bits_per_component: 8, filter: ImageFilter::Dct, data };
            encode::wrap_jpeg(plain)
        }
        ImageCodec::Flate8 => encode::encode_flate_png(&pdfimage::decode(&source.doc, id, info, false)?),
        ImageCodec::Flate1 => encode::encode_jbig2(&pdfimage::decode(&source.doc, id, info, false)?)?,
    };
    Ok((candidate.data.len() < info.encoded_len).then_some(candidate))
}

/// Recompresses an image of a preserved page when that makes it smaller.
pub fn recompress_preserved_image(source: &Source, id: ObjectId, compression: Compression) -> Result<Option<EncodedImage>> {
    let Some(info) = pdfimage::inspect(&source.doc, id)? else { return Ok(None) };
    let Some(quality) = compression.jpeg_quality() else { return lossless_shrink(source, id, &info) };
    let candidate = match info.codec {
        ImageCodec::Flate1 => encode::encode_jbig2(&pdfimage::decode(&source.doc, id, &info, false)?)?,
        _ => encode::encode_jpeg(&pdfimage::decode(&source.doc, id, &info, false)?, quality)?,
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

/// Preview of a page from a render of its source page (display space, any
/// resolution): the same view and pixel operations as the saved page.
pub fn preview(base: &Raster, page: &Page, display_size: (f64, f64)) -> Raster {
    let c = page.view.crop;
    let (w, h) = (base.width as f64, base.height as f64);
    let cropped = base.crop((c.x0 * w).round() as u32, (c.y0 * h).round() as u32, (c.x1 * w).round() as u32, (c.y1 * h).round() as u32);
    let turned = cropped.rotate_cw(page.view.quarter_turns);
    let ppi = base.width as f64 * 72.0 / display_size.0;
    pixel::apply_pixel_ops(turned, page.pixel_ops(), ppi / 300.0)
}
