//! Source PDFs: loading, decryption, page geometry, classification and
//! text stripping.

use crate::content::{self, PageContent, PlacedImage};
use crate::geometry::{Matrix, Rect, rotate_cw_quarters};
use anyhow::{Context, Result, bail};
use lopdf::{Dictionary, Document, LoadOptions, Object, ObjectId, Stream};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Pages whose images cover less than this share of the page, and that
/// have text, are typeset pages. Scans with an OCR layer cover it all.
const SCAN_COVERAGE: f64 = 0.9;

pub struct Source {
    /// File name without extension.
    pub name: String,
    /// The file MuPDF renders: the original, or a rewritten copy.
    pub render_path: PathBuf,
    pub doc: Document,
    pub pages: Vec<SourcePage>,
    pub was_encrypted: bool,
    /// Keeps the rewritten copy alive.
    _rewritten: Option<tempfile::TempPath>,
}

pub struct SourcePage {
    pub id: ObjectId,
    /// Visible box in user space (crop box clipped to the media box).
    pub page_box: Rect,
    /// Clockwise quarter turns from `/Rotate`.
    pub quarter_turns: u32,
    /// Size as displayed, in points.
    pub display_size: (f64, f64),
    /// Maps user space to display space (points, origin bottom-left, y up).
    pub user_to_display: Matrix,
    /// What the page paints, or why it could not be read.
    pub content: Result<PageContent, String>,
    pub is_text_page: bool,
}

impl SourcePage {
    /// The scan image that is the whole visible page, with its placement
    /// in display space.
    pub fn scan_image(&self) -> Option<(ObjectId, Matrix)> {
        let content = self.content.as_ref().ok()?;
        let PlacedImage { id, ctm, .. } = content.sole_image(&self.page_box)?;
        Some((id?, ctm.then(self.user_to_display)))
    }
}

impl Source {
    pub fn open(path: &Path) -> Result<Source> {
        let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        // An empty password opens unprotected files and "owner-only"
        // encrypted ones (the usual DRM case).
        let doc = Document::load_mem_with_options(&bytes, LoadOptions::with_password(""))
            .with_context(|| format!("cannot open {} (encrypted with a user password?)", path.display()))?;
        let was_encrypted = doc.was_encrypted();
        drop(bytes);

        let mut source = Source {
            name,
            render_path: path.to_path_buf(),
            doc,
            pages: Vec::new(),
            was_encrypted,
            _rewritten: None,
        };
        source.pages = source.read_pages()?;
        if source.was_encrypted {
            // MuPDF must see exactly the decrypted objects we copy from.
            source.rewrite_render_copy()?;
        }
        Ok(source)
    }

    fn read_pages(&self) -> Result<Vec<SourcePage>> {
        let doc = &self.doc;
        let mut pages = Vec::new();
        for (number, id) in doc.get_pages() {
            let media = inherited_rect(doc, id, b"MediaBox")?.with_context(|| format!("page {number} has no MediaBox"))?;
            let page_box = match inherited_rect(doc, id, b"CropBox")? {
                Some(crop) if crop.intersect(&media).area() > 0.0 => crop.intersect(&media),
                _ => media,
            };
            let rotate = inherited(doc, id, b"Rotate").and_then(|o| o.as_i64().ok()).unwrap_or(0);
            if rotate % 90 != 0 {
                bail!("page {number} has /Rotate {rotate}, not a multiple of 90");
            }
            let quarter_turns = (rotate.rem_euclid(360) / 90) as u32;
            let user_to_display = Matrix::translate(-page_box.x0, -page_box.y0)
                .then(rotate_cw_quarters(page_box.width(), page_box.height(), quarter_turns));
            let display_size = if quarter_turns % 2 == 1 {
                (page_box.height(), page_box.width())
            } else {
                (page_box.width(), page_box.height())
            };
            let resources = match inherited(doc, id, b"Resources") {
                Some(Object::Dictionary(d)) => d.clone(),
                _ => Dictionary::new(),
            };
            let content = content::analyze_page(doc, id, &resources).map_err(|e| format!("{e:#}"));
            let is_text_page = match &content {
                Ok(c) => c.has_text && c.image_area() < SCAN_COVERAGE * page_box.area(),
                Err(_) => false,
            };
            pages.push(SourcePage { id, page_box, quarter_turns, display_size, user_to_display, content, is_text_page });
        }
        Ok(pages)
    }

    /// Writes the current object model to a temporary file and renders from it.
    fn rewrite_render_copy(&mut self) -> Result<()> {
        let mut file = tempfile::Builder::new().prefix("pdf-editor-").suffix(".pdf").tempfile()?;
        let mut buffer = Vec::new();
        self.doc.trailer.remove(b"Encrypt");
        self.doc.save_to(&mut buffer).context("cannot rewrite the source PDF")?;
        file.write_all(&buffer)?;
        let path = file.into_temp_path();
        self.render_path = path.to_path_buf();
        self._rewritten = Some(path);
        Ok(())
    }

    /// Removes all text, visible or not, from the given pages (content
    /// streams and the forms they use). Images and vector graphics are
    /// untouched. Returns how many streams could not be processed (inline
    /// images), whose text therefore remains.
    pub fn strip_text(&mut self, page_indices: &[usize]) -> Result<usize> {
        let mut streams: Vec<ObjectId> = Vec::new();
        let mut seen = HashSet::new();
        for &index in page_indices {
            let id = self.pages[index].id;
            for content_id in self.doc.get_page_contents(id) {
                if seen.insert(content_id) {
                    streams.push(content_id);
                }
            }
            if let Some(Object::Dictionary(resources)) = inherited(&self.doc, id, b"Resources") {
                collect_forms(&self.doc, &resources, &mut seen, &mut streams);
            }
        }

        let mut skipped = 0;
        for id in streams {
            let stream = self.doc.get_object(id)?.as_stream()?;
            let data = stream.decompressed_content().with_context(|| format!("stream {id:?}"))?;
            match content::blank_text_operators(&data) {
                None => skipped += 1,
                Some((_, false)) => {}
                Some((blanked, true)) => {
                    let mut dict = stream.dict.clone();
                    dict.remove(b"DecodeParms");
                    dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
                    let compressed = crate::encode::zlib(&blanked);
                    self.doc.objects.insert(id, Object::Stream(Stream::new(dict, compressed)));
                }
            }
        }
        self.rewrite_render_copy()?;
        // Re-analyze: the stripped pages no longer have text.
        let kinds: Vec<bool> = self.pages.iter().map(|p| p.is_text_page).collect();
        self.pages = self.read_pages()?;
        for (page, was_text) in self.pages.iter_mut().zip(kinds) {
            page.is_text_page = was_text;
        }
        Ok(skipped)
    }
}

fn collect_forms(doc: &Document, resources: &Dictionary, seen: &mut HashSet<ObjectId>, out: &mut Vec<ObjectId>) {
    let Ok(xobjects) = resources.get_deref(b"XObject", doc).and_then(Object::as_dict) else { return };
    for (_, entry) in xobjects.iter() {
        let Ok(id) = entry.as_reference() else { continue };
        if !seen.insert(id) {
            continue;
        }
        let Ok(stream) = doc.get_object(id).and_then(Object::as_stream) else { continue };
        if stream.dict.get(b"Subtype").and_then(Object::as_name).ok() != Some(b"Form") {
            continue;
        }
        out.push(id);
        if let Ok(Object::Dictionary(inner)) = stream.dict.get_deref(b"Resources", doc) {
            collect_forms(doc, inner, seen, out);
        }
    }
}

/// Looks up a page attribute, following `/Parent` for inheritable keys.
pub fn inherited(doc: &Document, page_id: ObjectId, key: &[u8]) -> Option<Object> {
    let mut node = doc.get_dictionary(page_id).ok()?;
    for _ in 0..64 {
        if let Ok(value) = node.get(key) {
            return doc.dereference(value).ok().map(|(_, o)| o.clone());
        }
        node = node.get(b"Parent").ok().and_then(|p| p.as_reference().ok()).and_then(|id| doc.get_dictionary(id).ok())?;
    }
    None
}

fn inherited_rect(doc: &Document, page_id: ObjectId, key: &[u8]) -> Result<Option<Rect>> {
    let Some(value) = inherited(doc, page_id, key) else { return Ok(None) };
    let values: Vec<f64> = value
        .as_array()
        .context("page box is not an array")?
        .iter()
        .map(|o| doc.dereference(o).ok().and_then(|(_, o)| o.as_float().ok()).map(f64::from))
        .collect::<Option<_>>()
        .context("page box has a non-number")?;
    match values.as_slice() {
        [x0, y0, x1, y1] => Ok(Some(Rect::new(*x0, *y0, *x1, *y1))),
        _ => bail!("page box does not have 4 numbers"),
    }
}
