//! Content stream reading: page classification, scan image placement and
//! text removal.

use crate::geometry::{Matrix, Rect};
use anyhow::{Context, Result, bail};
use lopdf::content::Content;
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::HashSet;

/// An image painted on a page.
#[derive(Clone, Copy, Debug)]
pub struct PlacedImage {
    /// `None` for inline images.
    pub id: Option<ObjectId>,
    /// Maps the image's unit square to page user space.
    pub ctm: Matrix,
    /// Clip rectangle in user space when the image is clipped by
    /// rectangles only; `Some(None)` would be unknown, see `complex_clip`.
    pub clip: Option<Rect>,
    pub complex_clip: bool,
}

/// What a page paints, gathered from its content streams and forms.
#[derive(Clone, Debug, Default)]
pub struct PageContent {
    pub has_text: bool,
    pub has_visible_text: bool,
    /// Path fills, strokes and shadings.
    pub has_vector_painting: bool,
    pub images: Vec<PlacedImage>,
}

impl PageContent {
    /// Sum of painted image areas in user space (overlaps count twice).
    pub fn image_area(&self) -> f64 {
        self.images.iter().map(|i| i.ctm.det().abs()).sum()
    }

    /// The single image that is the whole visible content of the page, if
    /// any: what a scan looks like, possibly with an invisible OCR layer.
    pub fn sole_image(&self, page_box: &Rect) -> Option<PlacedImage> {
        if self.images.len() != 1 || self.has_vector_painting || self.has_visible_text {
            return None;
        }
        let image = self.images[0];
        image.id?;
        if !image.ctm.is_axis_aligned() || image.complex_clip {
            return None;
        }
        // A clip may only cut what the page box cuts anyway.
        if let Some(clip) = image.clip {
            let visible = image.ctm.unit_bbox().intersect(page_box);
            if !clip.contains(&visible, 0.5) {
                return None;
            }
        }
        Some(image)
    }
}

#[derive(Clone, Copy)]
struct GraphicsState {
    ctm: Matrix,
    text_render_mode: i64,
    clip: Option<Rect>,
    complex_clip: bool,
}

struct Walker<'a> {
    doc: &'a Document,
    out: PageContent,
    visited_forms: HashSet<ObjectId>,
}

const MAX_FORM_DEPTH: usize = 12;

/// Walks a page's content (including nested forms) and reports what it paints.
pub fn analyze_page(doc: &Document, page_id: ObjectId, resources: &Dictionary) -> Result<PageContent> {
    let data = page_content_bytes(doc, page_id)?;
    let mut walker = Walker { doc, out: PageContent::default(), visited_forms: HashSet::new() };
    let state = GraphicsState { ctm: Matrix::IDENTITY, text_render_mode: 0, clip: None, complex_clip: false };
    walker.walk(&data, resources, state, 0)?;
    Ok(walker.out)
}

/// Decoded, concatenated content streams of a page.
pub fn page_content_bytes(doc: &Document, page_id: ObjectId) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    for id in doc.get_page_contents(page_id) {
        let stream = doc.get_object(id)?.as_stream()?;
        data.extend_from_slice(&stream.decompressed_content().with_context(|| format!("content stream {id:?}"))?);
        data.push(b'\n');
    }
    Ok(data)
}

fn number(o: &Object) -> Option<f64> {
    o.as_float().ok().map(f64::from)
}

fn numbers(operands: &[Object]) -> Option<Vec<f64>> {
    operands.iter().map(number).collect()
}

impl Walker<'_> {
    fn walk(&mut self, data: &[u8], resources: &Dictionary, initial: GraphicsState, depth: usize) -> Result<()> {
        let content = Content::decode(data).context("unreadable content stream")?;
        let mut gs = initial;
        let mut stack: Vec<GraphicsState> = Vec::new();
        let mut path_rects: Vec<Rect> = Vec::new();
        let mut path_complex = false;
        let mut pending_clip = false;

        for op in &content.operations {
            let args = &op.operands;
            match op.operator.as_str() {
                "q" => stack.push(gs),
                "Q" => {
                    if let Some(saved) = stack.pop() {
                        gs = saved;
                    }
                }
                "cm" => {
                    let m = numbers(args).and_then(|v| Matrix::from_slice(&v)).context("bad cm operands")?;
                    gs.ctm = m.then(gs.ctm);
                }
                "Tr" => gs.text_render_mode = args.first().and_then(|o| o.as_i64().ok()).unwrap_or(0),
                "Tj" | "'" | "\"" | "TJ" => {
                    if shows_glyphs(args) {
                        self.out.has_text = true;
                        if gs.text_render_mode != 3 && gs.text_render_mode != 7 {
                            self.out.has_visible_text = true;
                        }
                    }
                }
                "re" => {
                    if let Some([x, y, w, h]) = numbers(args).as_deref().and_then(|v| <&[f64; 4]>::try_from(v).ok()) {
                        if gs.ctm.is_axis_aligned() {
                            path_rects.push(Rect::new(*x, *y, x + w, y + h).transform(gs.ctm));
                        } else {
                            path_complex = true;
                        }
                    }
                }
                "m" | "l" | "c" | "v" | "y" | "h" => path_complex = true,
                "W" | "W*" => pending_clip = true,
                "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "S" | "s" | "n" => {
                    if op.operator != "n" {
                        self.out.has_vector_painting = true;
                    }
                    if pending_clip {
                        if path_complex || path_rects.len() != 1 {
                            gs.complex_clip = true;
                        } else {
                            let r = path_rects[0];
                            gs.clip = Some(gs.clip.map_or(r, |c| c.intersect(&r)));
                        }
                    }
                    path_rects.clear();
                    path_complex = false;
                    pending_clip = false;
                }
                "sh" => self.out.has_vector_painting = true,
                "BI" => self.out.images.push(PlacedImage {
                    id: None,
                    ctm: gs.ctm,
                    clip: gs.clip,
                    complex_clip: gs.complex_clip,
                }),
                "Do" => {
                    let name = args.first().and_then(|o| o.as_name().ok()).context("bad Do operand")?;
                    self.paint_xobject(name, resources, gs, depth)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn paint_xobject(&mut self, name: &[u8], resources: &Dictionary, gs: GraphicsState, depth: usize) -> Result<()> {
        let doc = self.doc;
        let xobjects = resources.get_deref(b"XObject", doc).and_then(Object::as_dict).context("Do without XObject resources")?;
        let entry = xobjects.get(name).with_context(|| format!("missing XObject /{}", String::from_utf8_lossy(name)))?;
        let id = entry.as_reference().ok();
        let stream = doc.dereference(entry)?.1.as_stream()?;
        let subtype = stream.dict.get(b"Subtype").and_then(Object::as_name).unwrap_or(b"");
        match subtype {
            b"Image" => self.out.images.push(PlacedImage { id, ctm: gs.ctm, clip: gs.clip, complex_clip: gs.complex_clip }),
            b"Form" => {
                if depth >= MAX_FORM_DEPTH {
                    bail!("forms nested deeper than {MAX_FORM_DEPTH}");
                }
                if let Some(id) = id
                    && !self.visited_forms.insert(id) {
                        // Already analyzed; repeated forms paint the same kinds of things.
                        return Ok(());
                    }
                let matrix = stream
                    .dict
                    .get(b"Matrix")
                    .ok()
                    .and_then(|m| m.as_array().ok())
                    .and_then(|a| numbers(a))
                    .and_then(|v| Matrix::from_slice(&v))
                    .unwrap_or(Matrix::IDENTITY);
                let form_resources = match stream.dict.get_deref(b"Resources", doc) {
                    Ok(Object::Dictionary(d)) => d,
                    _ => resources,
                };
                let data = stream.decompressed_content().context("form content stream")?;
                let inner = GraphicsState { ctm: matrix.then(gs.ctm), ..gs };
                self.walk(&data, form_resources, inner, depth + 1)?;
            }
            _ => {}
        }
        Ok(())
    }
}

fn shows_glyphs(args: &[Object]) -> bool {
    args.iter().any(|a| match a {
        Object::String(s, _) => !s.is_empty(),
        Object::Array(items) => items.iter().any(|i| matches!(i, Object::String(s, _) if !s.is_empty())),
        _ => false,
    })
}

/// Overwrites every text-showing operator (`Tj`, `TJ`, `'`, `"`) and its
/// operands with spaces, keeping every other byte in place. Returns `None`
/// when the stream has inline images, whose binary data defeats the scan.
pub fn blank_text_operators(data: &[u8]) -> Option<(Vec<u8>, bool)> {
    let mut out = data.to_vec();
    let n = data.len();
    let mut i = 0;
    let mut group_start: Option<usize> = None;
    let mut changed = false;
    let is_white = |c: u8| matches!(c, b' ' | b'\n' | b'\r' | b'\t' | 0x0c | 0);
    let is_delim = |c: u8| matches!(c, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%');

    while i < n {
        let c = data[i];
        if is_white(c) {
            i += 1;
            continue;
        }
        if group_start.is_none() {
            group_start = Some(i);
        }
        match c {
            b'(' => {
                i += 1;
                let mut depth = 1;
                while i < n && depth > 0 {
                    match data[i] {
                        b'\\' => i += 1,
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
            }
            b'<' if data.get(i + 1) == Some(&b'<') => i += 2,
            b'<' => {
                while i < n && data[i] != b'>' {
                    i += 1;
                }
                i += 1;
            }
            b'>' => i += if data.get(i + 1) == Some(&b'>') { 2 } else { 1 },
            b'/' => {
                i += 1;
                while i < n && !is_white(data[i]) && !is_delim(data[i]) {
                    i += 1;
                }
            }
            b'%' => {
                while i < n && data[i] != b'\n' && data[i] != b'\r' {
                    i += 1;
                }
            }
            b'[' | b']' | b'{' | b'}' => i += 1,
            _ => {
                let start = i;
                while i < n && !is_white(data[i]) && !is_delim(data[i]) {
                    i += 1;
                }
                let first = data[start];
                if first.is_ascii_digit() || matches!(first, b'+' | b'-' | b'.') {
                    continue;
                }
                let token = &data[start..i.max(start + 1)];
                if token == b"BI" {
                    return None;
                }
                if matches!(token, b"Tj" | b"TJ" | b"'" | b"\"") {
                    let from = group_start.unwrap_or(start);
                    out[from..i].fill(b' ');
                    changed = true;
                }
                group_start = None;
                if i == start {
                    i += 1;
                }
            }
        }
    }
    Some((out, changed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blanking_removes_only_text_operators() {
        let src = b"BT /F1 12 Tf 10 10 Td (Hello (nested)) Tj [(a) -20 (b)] TJ ET 0 0 m 5 5 l S";
        let (out, changed) = blank_text_operators(src).unwrap();
        assert!(changed);
        assert_eq!(out.len(), src.len());
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains("Hello") && !text.contains("TJ"));
        assert!(text.contains("/F1 12 Tf") && text.contains("0 0 m 5 5 l S"));
    }

    #[test]
    fn blanking_refuses_inline_images() {
        assert!(blank_text_operators(b"q BI /W 1 /H 1 ID \x00 EI Q").is_none());
    }
}
