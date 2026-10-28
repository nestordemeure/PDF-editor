//! Builds the output document as pages come: copied text pages, raster
//! pages, the page tree. Each object is written at once, so memory does not
//! grow with the document. Has no threads: callers run the page jobs.

use crate::copy::Copier;
use crate::encode::{self, EncodedImage, ImageFilter};
use crate::model::Page;
use crate::pipeline::{ImageOut, PageOutput, PageRoute};
use crate::source::{Source, inherited};
use crate::textlayer;
use crate::writer::{OutId, PdfWriter, format_number, reference};
use anyhow::{Context, Result, bail};
use lopdf::{Dictionary, Object, ObjectId};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::Arc;

#[derive(Clone, Debug, Default, Serialize)]
pub struct SaveReport {
    pub pages: usize,
    pub routes: HashMap<PageRoute, usize>,
    pub recompressed_images: usize,
    /// Text runs removed from cropped text pages (outside their half).
    pub hidden_text_removed: usize,
    /// Cropped text pages whose content could not be filtered (their
    /// hidden text remains).
    pub hidden_text_kept_pages: usize,
    pub bytes: u64,
    pub seconds: f64,
}

/// Output object ids decided before any page is written.
struct Layout {
    catalog: OutId,
    pages_root: OutId,
    page_ids: Vec<OutId>,
    font: Option<OutId>,
}

/// An image of preserved pages that may be recompressed, and its
/// resolution where it is drawn largest.
#[derive(Clone, Copy, Debug)]
pub struct ImageTask {
    pub source: usize,
    pub id: ObjectId,
    pub shown_ppi: Option<f64>,
}

pub struct Assembler {
    w: PdfWriter,
    sources: Vec<Arc<Source>>,
    pages: Vec<Page>,
    /// One per source, so shared objects are written once.
    copiers: Vec<Copier>,
    layout: Layout,
    shared_images: HashMap<(usize, ObjectId, bool), OutId>,
    written: Vec<bool>,
    report: SaveReport,
}

impl Assembler {
    /// Starts a document of `pages`. With `ocr`, raster pages get a text
    /// layer font.
    pub fn new(sources: Vec<Arc<Source>>, pages: Vec<Page>, ocr: bool, out: Box<dyn Write>) -> Result<Assembler> {
        if pages.is_empty() {
            bail!("no pages to save");
        }
        let mut w = PdfWriter::new(out)?;
        let has_raster = pages.iter().any(|p| !p.is_preserved());
        let layout = Layout {
            catalog: w.alloc(),
            pages_root: w.alloc(),
            page_ids: pages.iter().map(|_| w.alloc()).collect(),
            font: (ocr && has_raster).then(|| w.alloc()),
        };
        if let Some(font) = layout.font {
            write_ocr_font(&mut w, font)?;
        }
        let mut copiers: Vec<Copier> = sources.iter().map(|s| Copier::new(s.clone())).collect();
        for (i, page) in pages.iter().enumerate() {
            copiers[page.source].page_targets.entry(sources[page.source].pages[page.index].id).or_insert(layout.page_ids[i]);
        }
        let report = SaveReport { pages: pages.len(), ..Default::default() };
        let written = vec![false; pages.len()];
        Ok(Assembler { w, sources, pages, copiers, layout, shared_images: HashMap::new(), written, report })
    }

    pub fn pages(&self) -> &[Page] {
        &self.pages
    }

    pub fn sources(&self) -> &[Arc<Source>] {
        &self.sources
    }

    /// Indices of the pages that go through the raster pipeline.
    pub fn raster_pages(&self) -> Vec<usize> {
        (0..self.pages.len()).filter(|&i| !self.pages[i].is_preserved()).collect()
    }

    fn preserved_pages(&self) -> Vec<usize> {
        (0..self.pages.len()).filter(|&i| self.pages[i].is_preserved()).collect()
    }

    /// The images of preserved pages, each once.
    pub fn preserved_images(&self) -> Vec<ImageTask> {
        let (sources, pages) = (&self.sources, &self.pages);
        let preserved = self.preserved_pages();
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
        images.into_iter().map(|(source, id)| ImageTask { source, id, shown_ppi: shown_ppi.get(&(source, id)).copied() }).collect()
    }

    /// Writes `encoded` in place of an image of preserved pages.
    pub fn replace_image(&mut self, task: ImageTask, encoded: EncodedImage) -> Result<()> {
        let original = &self.sources[task.source].doc.get_object(task.id)?.as_stream()?.dict;
        let stream = lopdf::Stream::new(replacement_dict(original, &encoded), encoded.data);
        self.copiers[task.source].replacements.insert(task.id, Object::Stream(stream));
        self.report.recompressed_images += 1;
        Ok(())
    }

    /// Copies every preserved page; call after `replace_image`. `tick`
    /// runs after each page.
    pub fn write_preserved_pages(&mut self, tick: &mut dyn FnMut()) -> Result<()> {
        let preserved = self.preserved_pages();
        let (mut filtered, removed, failed) = filter_cropped_text(&self.sources, &self.pages, &preserved)?;
        self.report.hidden_text_removed = removed;
        self.report.hidden_text_kept_pages = failed;
        for i in preserved {
            let content = filtered.remove(&i);
            let page = &self.pages[i];
            write_preserved_page(&mut self.w, &mut self.copiers[page.source], &self.sources[page.source], page, self.layout.page_ids[i], self.layout.pages_root, content)?;
            self.written[i] = true;
            *self.report.routes.entry(PageRoute::Copied).or_default() += 1;
            tick();
        }
        Ok(())
    }

    /// Writes raster page `index`, in any order.
    pub fn write_raster_page(&mut self, index: usize, output: PageOutput) -> Result<()> {
        if self.written[index] {
            bail!("page {} written twice", index + 1);
        }
        let route = output.route;
        let source = self.pages[index].source;
        write_raster_page(&mut self.w, &mut self.copiers[source], &mut self.shared_images, source, output, &self.layout, index)?;
        self.written[index] = true;
        *self.report.routes.entry(route).or_default() += 1;
        Ok(())
    }

    /// Bytes written so far.
    pub fn position(&self) -> u64 {
        self.w.position()
    }

    /// Writes the page tree and the cross-reference stream.
    pub fn finish(mut self) -> Result<SaveReport> {
        if let Some(missing) = self.written.iter().position(|&w| !w) {
            bail!("page {} was not written", missing + 1);
        }
        let w = &mut self.w;
        for copier in &mut self.copiers {
            copier.flush(w)?;
        }
        let layout = &self.layout;
        let mut root = Dictionary::new();
        root.set("Type", Object::Name(b"Pages".to_vec()));
        root.set("Kids", Object::Array(layout.page_ids.iter().map(|&id| reference(id)).collect()));
        root.set("Count", Object::Integer(self.pages.len() as i64));
        w.write(layout.pages_root, &Object::Dictionary(root))?;
        let mut catalog = Dictionary::new();
        catalog.set("Type", Object::Name(b"Catalog".to_vec()));
        catalog.set("Pages", reference(layout.pages_root));
        w.write(layout.catalog, &Object::Dictionary(catalog))?;
        let info = w.alloc();
        let mut info_dict = Dictionary::new();
        info_dict.set("Producer", Object::string_literal("PDF Editor"));
        w.write(info, &Object::Dictionary(info_dict))?;
        let catalog = layout.catalog;
        self.report.bytes = self.w.finish(catalog, Some(info))?;
        Ok(self.report)
    }
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

/// Cropped text pages (halves of spreads) without the text and graphics
/// they no longer show. All crops of a source page are decided together:
/// filtering replaces the shared content with one copy per crop, which pays
/// only when the copies together are about the original's size (a spread
/// whose halves each hold their own text). Returns, per output page, the
/// new content, plus the removed run count and the pages that could not be
/// filtered.
fn filter_cropped_text(sources: &[Arc<Source>], pages: &[Page], preserved: &[usize]) -> Result<(HashMap<usize, Vec<u8>>, usize, usize)> {
    let mut groups: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
    for &i in preserved {
        if !pages[i].view.is_identity() {
            groups.entry((pages[i].source, pages[i].index)).or_default().push(i);
        }
    }
    let (mut filtered, mut removed, mut failed) = (HashMap::new(), 0, 0);
    for ((s, index), members) in groups {
        let (source, sp) = (&sources[s], &sources[s].pages[index]);
        let resources = match inherited(&source.doc, sp.id, b"Resources") {
            Some(Object::Dictionary(d)) => d,
            _ => Dictionary::new(),
        };
        let content = crate::content::page_content_bytes(&source.doc, sp.id)?;
        let mut results = Vec::new();
        for &i in &members {
            let crop = pages[i].view.crop_in_display(sp.display_size).transform(sp.user_to_display.invert().context("degenerate page")?);
            match crate::textcrop::drop_text_outside(&source.doc, &resources, &content, crop) {
                Ok(Some(c)) => results.push((i, c)),
                Ok(None) => {}
                Err(_) => failed += 1,
            }
        }
        // Every crop must be filtered, or the shared original stays anyway.
        let total: usize = results.iter().map(|(_, c)| c.content.len()).sum();
        if results.len() == members.len() && total * 100 <= content.len() * 110 {
            for (i, c) in results {
                removed += c.removed;
                filtered.insert(i, c.content);
            }
        }
    }
    Ok((filtered, removed, failed))
}

/// Copies a text page, with `content` replacing its own when given.
fn write_preserved_page(w: &mut PdfWriter, copier: &mut Copier, source: &Source, page: &Page, out_id: OutId, parent: OutId, filtered: Option<Vec<u8>>) -> Result<()> {
    let sp = &source.pages[page.index];
    let crop = page.view.crop_in_display(sp.display_size).transform(sp.user_to_display.invert().context("degenerate page")?);
    let mut dict = source.doc.get_dictionary(sp.id)?.clone();
    if filtered.is_some() {
        dict.remove(b"Contents");
    }
    let mut out = copier.convert_page(w, &dict);
    if let Some(content) = filtered {
        let id = w.alloc();
        let mut d = Dictionary::new();
        d.set("Filter", Object::Name(b"FlateDecode".to_vec()));
        w.write_stream(id, d, encode::zlib(&content))?;
        out.set("Contents", reference(id));
    }
    for key in [&b"MediaBox"[..], b"Resources"] {
        if !out.has(key)
            && let Some(value) = inherited(&source.doc, sp.id, key) {
                let converted = copier.convert(w, &value);
                out.set(key.to_vec(), converted);
            }
    }
    // The view becomes a crop box and a rotation: text stays text.
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
    let f = format_number;
    let draw = |m: crate::geometry::Matrix, name: &str| {
        format!("q 0 0 {} {} re W n {} {} {} {} {} {} cm /{name} Do Q\n", f(pw), f(ph), f(m.a), f(m.b), f(m.c), f(m.d), f(m.e), f(m.f))
    };
    let mut content = draw(output.image_ctm, "Im0").into_bytes();
    let mask_id = match &output.mask {
        Some(layer) => {
            let (mask, fg) = (w.alloc(), w.alloc());
            let e = &layer.mask;
            let mut d = Dictionary::new();
            d.set("Type", Object::Name(b"XObject".to_vec()));
            d.set("Subtype", Object::Name(b"Image".to_vec()));
            d.set("Width", Object::Integer(e.width as i64));
            d.set("Height", Object::Integer(e.height as i64));
            d.set("ImageMask", Object::Boolean(true));
            d.set("BitsPerComponent", Object::Integer(1));
            d.set("Filter", Object::Name(b"JBIG2Decode".to_vec()));
            w.write_stream(mask, d, e.data.clone())?;
            let mut d = image_dict(&layer.foreground);
            d.set("Mask", reference(mask));
            w.write_stream(fg, d, layer.foreground.data.clone())?;
            content.extend_from_slice(draw(layer.ctm, "Im1").as_bytes());
            Some(fg)
        }
        None => None,
    };
    if let Some(text) = &output.text {
        content.extend_from_slice(text);
    }
    let content_id = w.alloc();
    let mut content_dict = Dictionary::new();
    content_dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
    w.write_stream(content_id, content_dict, encode::zlib(&content))?;

    let mut xobjects = Dictionary::new();
    xobjects.set("Im0", reference(image_id));
    if let Some(id) = mask_id {
        xobjects.set("Im1", reference(id));
    }
    let mut resources = Dictionary::new();
    resources.set("XObject", Object::Dictionary(xobjects));
    if let (Some(font), Some(_)) = (layout.font, &output.text) {
        let mut fonts = Dictionary::new();
        fonts.set(textlayer::FONT_NAME, reference(font));
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
    d.set("Length1", Object::Integer(textlayer::GLYPHLESS_FONT.len() as i64));
    d.set("Filter", name("FlateDecode"));
    w.write_stream(file, d, encode::zlib(textlayer::GLYPHLESS_FONT))?;

    // Every CID maps to glyph 1.
    let map: Vec<u8> = (0..65536).flat_map(|_| [0u8, 1u8]).collect();
    let mut d = Dictionary::new();
    d.set("Filter", name("FlateDecode"));
    w.write_stream(cid_map, d, encode::zlib(&map))?;

    let mut d = Dictionary::new();
    d.set("Filter", name("FlateDecode"));
    w.write_stream(to_unicode, d, encode::zlib(textlayer::TO_UNICODE_CMAP.as_bytes()))?;
    Ok(())
}

