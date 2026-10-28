//! Removes the text a cropped page no longer shows (the other half of a
//! split spread), so search and copy do not find it.
//!
//! Text-showing operators come in runs: after a positioning operator (`Td`,
//! `TD`, `Tm`, `T*`, `'`, `"`), each show starts where the previous one
//! ended. Removing a whole run leaves every other run in place, because
//! positioning starts from the line matrix, which shows do not move. A run
//! is removed only when its box, measured with the font's glyph widths, lies
//! entirely outside the crop. Runs that cannot be measured, and clipping
//! text (render modes 4 to 7), stay.
//!
//! A removed run takes its positioning operators with it; the next kept
//! run then starts with an absolute `TL` and `Tm` taken from the original
//! stream, so every kept glyph lands where it was. Paths and images drawn
//! entirely outside the crop go too, and forms whose bounding box is;
//! clipping paths stay. Text
//! objects left empty go whole, and the text state they set is restored
//! before the next kept one.
//!
//! Removal cuts the operators' bytes out of the original stream; every
//! other byte stays. (Re-encoding a parsed stream with lopdf is not safe:
//! it changed the rendering of real pages.)

use crate::geometry::{Matrix, Rect};
use anyhow::{Context, Result};
use lopdf::{Dictionary, Document, Object, StringFormat};
use std::collections::HashMap;

/// Glyph advances of a font, in thousandths of an em.
enum Metrics {
    /// One-byte codes.
    Simple { first: u32, widths: Vec<f64> },
    /// Two-byte codes that are CIDs (`Identity-H`/`-V`).
    Cid { default: f64, widths: HashMap<u32, f64> },
    Unknown,
}

impl Metrics {
    fn load(doc: &Document, font: &Dictionary) -> Metrics {
        let get = |d: &Dictionary, k: &[u8]| d.get(k).ok().and_then(|o| doc.dereference(o).ok()).map(|(_, o)| o.clone());
        let number = |o: &Object| o.as_float().ok().map(f64::from);
        match font.get(b"Subtype").and_then(Object::as_name) {
            Ok(b"Type0") => {
                let identity = matches!(get(font, b"Encoding"), Some(Object::Name(n)) if n == b"Identity-H" || n == b"Identity-V");
                let descendant = match get(font, b"DescendantFonts") {
                    Some(Object::Array(a)) => a.first().and_then(|o| doc.dereference(o).ok()).and_then(|(_, o)| o.as_dict().ok().cloned()),
                    _ => None,
                };
                let (true, Some(cid)) = (identity, descendant) else { return Metrics::Unknown };
                let default = get(&cid, b"DW").as_ref().and_then(number).unwrap_or(1000.0);
                let mut widths = HashMap::new();
                if let Some(Object::Array(w)) = get(&cid, b"W") {
                    let items: Vec<Object> = w.iter().map(|o| doc.dereference(o).map(|(_, o)| o.clone()).unwrap_or(Object::Null)).collect();
                    let mut i = 0;
                    while i < items.len() {
                        let Some(start) = number(&items[i]) else { return Metrics::Unknown };
                        match items.get(i + 1) {
                            Some(Object::Array(list)) => {
                                for (k, v) in list.iter().enumerate() {
                                    widths.insert(start as u32 + k as u32, number(v).unwrap_or(default));
                                }
                                i += 2;
                            }
                            Some(end) => {
                                let (Some(end), Some(width)) = (number(end), items.get(i + 2).and_then(number)) else { return Metrics::Unknown };
                                for c in start as u32..=end as u32 {
                                    widths.insert(c, width);
                                }
                                i += 3;
                            }
                            None => return Metrics::Unknown,
                        }
                    }
                }
                Metrics::Cid { default, widths }
            }
            Ok(b"Type1" | b"TrueType" | b"MMType1") => {
                let first = get(font, b"FirstChar").as_ref().and_then(number);
                match (first, get(font, b"Widths")) {
                    (Some(first), Some(Object::Array(w))) => Metrics::Simple {
                        first: first as u32,
                        widths: w.iter().map(|o| doc.dereference(o).ok().and_then(|(_, o)| number(o)).unwrap_or(0.0)).collect(),
                    },
                    // Standard 14 fonts without widths: unknown here.
                    _ => Metrics::Unknown,
                }
            }
            _ => Metrics::Unknown,
        }
    }

    /// Advance of a string in text space units, or `None` when unknown.
    fn advance(&self, bytes: &[u8], state: &TextState) -> Option<f64> {
        let scale = state.horizontal_scale;
        let glyph = |w: f64, is_space: bool| (w / 1000.0 * state.size + state.char_spacing + if is_space { state.word_spacing } else { 0.0 }) * scale;
        match self {
            Metrics::Simple { first, widths } => {
                Some(bytes.iter().map(|&b| glyph(widths.get((b as u32).wrapping_sub(*first) as usize).copied().unwrap_or(0.0), b == 32)).sum())
            }
            Metrics::Cid { default, widths } => {
                if !bytes.len().is_multiple_of(2) {
                    return None;
                }
                Some(bytes.chunks(2).map(|c| glyph(*widths.get(&((c[0] as u32) << 8 | c[1] as u32)).unwrap_or(default), false)).sum())
            }
            Metrics::Unknown => None,
        }
    }
}

#[derive(Clone, Copy)]
struct TextState {
    size: f64,
    char_spacing: f64,
    word_spacing: f64,
    horizontal_scale: f64,
    leading: f64,
    /// Render modes 4 to 7 add the glyphs to the clipping path.
    clips: bool,
}

/// What `drop_text_outside` did.
pub struct Cropped {
    pub content: Vec<u8>,
    pub removed: usize,
}

/// A run being collected: the positioning operators that place it, then
/// its shows, and its box in user space (`None` when unmeasurable).
#[derive(Default)]
struct Run {
    positioning: Vec<usize>,
    shows: Vec<usize>,
    bbox: Option<Rect>,
    unmeasurable: bool,
    /// Positioning was removed since the last kept run of this text object.
    out_of_sync: bool,
}

fn union(a: Option<Rect>, b: Rect) -> Rect {
    match a {
        Some(r) => Rect::new(r.x0.min(b.x0), r.y0.min(b.y0), r.x1.max(b.x1), r.y1.max(b.y1)),
        None => b,
    }
}

/// Removes, from a page's content, what lies entirely outside `keep` (user
/// space): text runs with their positioning, and paths and images drawn
/// there. Returns `None` when nothing was removed.
pub fn drop_text_outside(doc: &Document, resources: &Dictionary, content: &[u8], keep: Rect) -> Result<Option<Cropped>> {
    let ops = tokenize(content).context("content stream not understood")?;
    let lookup = |key: &[u8]| -> Option<Dictionary> {
        match resources.get(key).ok().and_then(|o| doc.dereference(o).ok()) {
            Some((_, Object::Dictionary(d))) => Some(d.clone()),
            _ => None,
        }
    };
    let fonts: HashMap<Vec<u8>, Metrics> = lookup(b"Font")
        .map(|fonts| {
            fonts.iter().filter_map(|(name, f)| Some((name.clone(), Metrics::load(doc, doc.dereference(f).ok()?.1.as_dict().ok()?)))).collect()
        })
        .unwrap_or_default();
    let xobjects = lookup(b"XObject").unwrap_or_default();
    // Where an XObject can paint, in its own space: the unit square for
    // images, the bounding box (through its matrix) for forms.
    let xobject_extent = |name: &[u8]| -> Option<Rect> {
        let stream = xobjects.get(name).ok().and_then(|o| doc.dereference(o).ok()).and_then(|(_, o)| o.as_stream().ok())?;
        let nums = |key: &[u8]| -> Option<Vec<f64>> {
            let (_, o) = doc.dereference(stream.dict.get(key).ok()?).ok()?;
            o.as_array().ok()?.iter().map(|v| v.as_float().ok().map(f64::from)).collect()
        };
        match stream.dict.get(b"Subtype").and_then(Object::as_name).ok()? {
            b"Image" => Some(Rect::new(0.0, 0.0, 1.0, 1.0)),
            b"Form" => {
                let b = nums(b"BBox")?;
                let bbox = Rect::new(*b.first()?, *b.get(1)?, *b.get(2)?, *b.get(3)?);
                let matrix = nums(b"Matrix").and_then(|m| Matrix::from_slice(&m)).unwrap_or(Matrix::IDENTITY);
                Some(bbox.transform(matrix))
            }
            _ => None,
        }
    };

    let number = |o: &Object| o.as_float().ok().map(f64::from);
    let outside = |b: Rect| b.intersect(&keep).area() <= 0.0;

    let mut drop = vec![false; ops.len()];
    // Absolute text position (leading, line matrix) to restore before an
    // operator, when removed positioning came before it.
    let mut resync: HashMap<usize, (f64, Matrix)> = HashMap::new();

    let mut ctm = Matrix::IDENTITY;
    let mut line_width = 1.0;
    let mut stack: Vec<(Matrix, f64)> = Vec::new();
    let mut state = TextState { size: 0.0, char_spacing: 0.0, word_spacing: 0.0, horizontal_scale: 1.0, leading: 0.0, clips: false };
    let mut font: Option<&Metrics> = None;
    let (mut tm, mut tlm) = (Matrix::IDENTITY, Matrix::IDENTITY);
    // Line matrix and leading before each operator, in the original stream.
    let mut before: Vec<(f64, Matrix)> = Vec::with_capacity(ops.len());

    let mut run = Run::default();
    let close = |run: &mut Run, drop: &mut Vec<bool>, resync: &mut HashMap<usize, (f64, Matrix)>, before: &Vec<(f64, Matrix)>| {
        if !run.shows.is_empty() {
            let removable = !run.unmeasurable && run.bbox.is_some_and(outside);
            if removable {
                for &i in run.positioning.iter().chain(&run.shows) {
                    drop[i] = true;
                }
                run.out_of_sync = true;
            } else if run.out_of_sync {
                let first = run.positioning.first().copied().unwrap_or(run.shows[0]);
                resync.insert(first, before[first]);
                run.out_of_sync = false;
            }
            run.positioning.clear();
        }
        run.shows.clear();
        run.bbox = None;
        run.unmeasurable = false;
    };

    // Path being built: its first operator and its box in user space.
    let mut path_start: Option<usize> = None;
    let mut path_box: Option<Rect> = None;
    let mut path_clips = false;

    for (i, op) in ops.iter().enumerate() {
        before.push((state.leading, tlm));
        let a = &op.operands;
        let num = |k: usize| op.numbers.get(k).copied().filter(|v| v.is_finite()).unwrap_or(0.0);
        let exact = || Matrix::from_slice(&op.numbers).filter(|_| op.numbers.iter().all(|v| v.is_finite()));
        let name = op.operator.as_str();
        match name {
            "q" => stack.push((ctm, line_width)),
            "Q" => (ctm, line_width) = stack.pop().unwrap_or((ctm, line_width)),
            "cm" => {
                if let Some(m) = exact() {
                    ctm = m.then(ctm);
                }
            }
            "w" => line_width = num(0),
            "BT" => {
                close(&mut run, &mut drop, &mut resync, &before);
                run.positioning.clear();
                run.out_of_sync = false;
                tm = Matrix::IDENTITY;
                tlm = Matrix::IDENTITY;
            }
            "ET" => {
                close(&mut run, &mut drop, &mut resync, &before);
                run.positioning.clear();
            }
            "Tf" => {
                font = a.first().and_then(|n| n.as_name().ok()).and_then(|n| fonts.get(n));
                state.size = num(1);
            }
            "Tc" => state.char_spacing = num(0),
            "Tw" => state.word_spacing = num(0),
            "Tz" => state.horizontal_scale = num(0) / 100.0,
            "TL" => state.leading = num(0),
            "Tr" => state.clips = num(0) >= 4.0,
            "Td" | "TD" | "Tm" | "T*" => {
                close(&mut run, &mut drop, &mut resync, &before);
                run.positioning.push(i);
                match name {
                    "Td" => tlm = Matrix::translate(num(0), num(1)).then(tlm),
                    "TD" => {
                        state.leading = -num(1);
                        tlm = Matrix::translate(num(0), num(1)).then(tlm);
                    }
                    "Tm" => {
                        if let Some(m) = exact() {
                            tlm = m;
                        }
                    }
                    _ => tlm = Matrix::translate(0.0, -state.leading).then(tlm),
                }
                tm = tlm;
            }
            "Tj" | "TJ" | "'" | "\"" => {
                if matches!(name, "'" | "\"") {
                    close(&mut run, &mut drop, &mut resync, &before);
                    if name == "\"" {
                        state.word_spacing = num(0);
                        state.char_spacing = num(1);
                    }
                    tlm = Matrix::translate(0.0, -state.leading).then(tlm);
                    tm = tlm;
                }
                let mut advance = Some(0.0);
                let strings: Vec<&Object> = match name {
                    "TJ" => a.first().and_then(|o| o.as_array().ok()).map(|v| v.iter().collect()).unwrap_or_default(),
                    "\"" => a.get(2).into_iter().collect(),
                    _ => a.first().into_iter().collect(),
                };
                for item in strings {
                    advance = match (advance, item, font) {
                        (Some(x), Object::String(s, _), Some(f)) => f.advance(s, &state).map(|w| x + w),
                        (Some(x), o, _) if number(o).is_some() => Some(x - number(o).unwrap() / 1000.0 * state.size * state.horizontal_scale),
                        _ => None,
                    };
                }
                run.shows.push(i);
                // Clipping text shapes what is drawn after it: never remove it.
                if state.clips {
                    run.unmeasurable = true;
                }
                match advance {
                    Some(adv) => {
                        let glyphs = Rect::new(0.0, -0.3 * state.size, adv, state.size);
                        run.bbox = Some(union(run.bbox, glyphs.transform(tm.then(ctm))));
                        tm = Matrix::translate(adv, 0.0).then(tm);
                    }
                    None => run.unmeasurable = true,
                }
            }
            // Paths: construction, then one painting operator.
            "m" | "l" | "c" | "v" | "y" | "re" | "h" => {
                path_start.get_or_insert(i);
                let points: Vec<(f64, f64)> = match name {
                    "re" => {
                        let (x, y, w, h) = (num(0), num(1), num(2), num(3));
                        vec![(x, y), (x + w, y + h)]
                    }
                    "h" => vec![],
                    _ => a.chunks(2).filter_map(|p| Some((number(&p[0])?, number(p.get(1)?)?))).collect(),
                };
                // "v" also uses the current point; it is already in the box.
                for (x, y) in points {
                    let (ux, uy) = ctm.apply(x, y);
                    path_box = Some(union(path_box, Rect::new(ux, uy, ux, uy)));
                }
            }
            "W" | "W*" => path_clips = true,
            "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "n" => {
                if let (Some(start), Some(bbox), false) = (path_start, path_box, path_clips) {
                    // Strokes reach half the line width past the path.
                    let scale = ctm.det().abs().sqrt();
                    let margin = line_width.max(1.0) * scale + 1.0;
                    let reach = Rect::new(bbox.x0 - margin, bbox.y0 - margin, bbox.x1 + margin, bbox.y1 + margin);
                    if outside(reach) {
                        for flag in &mut drop[start..=i] {
                            *flag = true;
                        }
                    }
                }
                path_start = None;
                path_box = None;
                path_clips = false;
            }
            "Do" => {
                let extent = a.first().and_then(|n| n.as_name().ok()).and_then(xobject_extent);
                if extent.is_some_and(|e| outside(e.transform(ctm))) {
                    drop[i] = true;
                }
            }
            _ => {}
        }
    }
    close(&mut run, &mut drop, &mut resync, &before);

    // Text objects left without any text go, with their text positioning
    // and text state (whose effect is restored below). Other operators in
    // them (colors, graphics state, marked content) are also valid outside
    // text objects and stay.
    let mut block_start = None;
    for (i, op) in ops.iter().enumerate() {
        match op.operator.as_str() {
            "BT" => block_start = Some(i),
            "ET" => {
                if let Some(start) = block_start.take() {
                    let inner = &ops[start + 1..i];
                    let has_text = inner.iter().any(|o| SHOWS.contains(&o.operator.as_str()));
                    let all_text_gone = (start + 1..i).filter(|&k| SHOWS.contains(&ops[k].operator.as_str())).all(|k| drop[k]);
                    let portable = inner.iter().all(|o| TEXT_ONLY.contains(&o.operator.as_str()) || OUTSIDE_TEXT_OK.contains(&o.operator.as_str()));
                    if has_text && all_text_gone && portable {
                        drop[start] = true;
                        drop[i] = true;
                        for k in start + 1..i {
                            if TEXT_ONLY.contains(&ops[k].operator.as_str()) {
                                drop[k] = true;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Text state persists across text objects: before each kept one,
    // restore what removed operators set in the original.
    let mut state_resync: HashMap<usize, Vec<String>> = HashMap::new();
    let (mut original, mut output) = (TextParams::default(), TextParams::default());
    for (i, op) in ops.iter().enumerate() {
        if op.operator == "BT" && !drop[i] {
            let fix = output.differences(&original);
            if !fix.is_empty() {
                state_resync.insert(i, fix);
                output = original.clone();
            }
        }
        original.apply(op);
        // A removed " leaves its spacing behind (see below).
        if !drop[i] || op.operator == "\"" {
            output.apply(op);
        }
        if let Some((leading, _)) = resync.get(&i) {
            output.leading = *leading;
        }
    }

    let removed = drop.iter().filter(|&&d| d).count();
    if removed == 0 {
        return Ok(None);
    }
    // Enough digits that restored positions match the original's to well
    // under a device pixel.
    let f = |v: f64| {
        let s = format!("{v:.6}");
        let s = s.trim_end_matches('0').trim_end_matches('.');
        if s == "-0" { "0".to_owned() } else { s.to_owned() }
    };
    let mut out = Vec::with_capacity(content.len() / 2);
    let mut at = 0;
    for (i, op) in ops.iter().enumerate() {
        let restore = resync.get(&i);
        let state = state_resync.get(&i);
        if !drop[i] && restore.is_none() && state.is_none() {
            continue;
        }
        // The gap before a removed operator is whitespace (or comments): it
        // goes with it, leaving one separator.
        if drop[i] && restore.is_none() && state.is_none() {
            // Kept bytes up to the previous operator's end.
            let previous_end = if i > 0 { ops[i - 1].end.max(at) } else { at };
            out.extend_from_slice(&content[at..previous_end]);
            if out.last().is_some_and(|c| !is_white(*c)) {
                out.push(b'\n');
            }
            at = op.end;
            if op.operator == "\"" {
                let n = |k: usize| op.numbers.get(k).copied().filter(|v| v.is_finite()).unwrap_or(0.0);
                out.extend_from_slice(format!("{} Tw {} Tc\n", f(n(0)), f(n(1))).as_bytes());
            }
            continue;
        }
        out.extend_from_slice(&content[at..op.start]);
        if let Some(ops) = state {
            out.push(b' ');
            out.extend_from_slice(ops.join(" ").as_bytes());
            out.push(b' ');
        }
        if let Some((leading, m)) = restore {
            out.extend_from_slice(format!(" {} TL {} {} {} {} {} {} Tm ", f(*leading), f(m.a), f(m.b), f(m.c), f(m.d), f(m.e), f(m.f)).as_bytes());
        }
        if drop[i] {
            // A removed " still sets the word and character spacing.
            if op.operator == "\"" {
                let n = |k: usize| op.numbers.get(k).copied().filter(|v| v.is_finite()).unwrap_or(0.0);
                out.extend_from_slice(format!(" {} Tw {} Tc ", f(n(0)), f(n(1))).as_bytes());
            } else {
                out.push(b' ');
            }
            at = op.end;
        } else {
            at = op.start;
        }
    }
    out.extend_from_slice(&content[at..]);
    Ok(Some(Cropped { content: out, removed }))
}

const SHOWS: [&str; 4] = ["Tj", "TJ", "'", "\""];

/// Operators that only position or style text, and the shows.
const TEXT_ONLY: [&str; 15] = ["Tj", "TJ", "'", "\"", "Td", "TD", "Tm", "T*", "Tf", "Tc", "Tw", "Tz", "TL", "Tr", "Ts"];

/// Operators valid both inside and outside text objects.
const OUTSIDE_TEXT_OK: [&str; 26] = [
    "w", "J", "j", "M", "d", "ri", "i", "gs", "CS", "cs", "SC", "SCN", "sc", "scn", "G", "g", "RG", "rg", "K", "k", "BMC", "BDC",
    "EMC", "MP", "DP", "Do",
];

/// Text state parameters, which outlive text objects.
#[derive(Clone, PartialEq)]
struct TextParams {
    font: Option<(Vec<u8>, f64)>,
    char_spacing: f64,
    word_spacing: f64,
    scale: f64,
    leading: f64,
    render: f64,
    rise: f64,
}

impl Default for TextParams {
    fn default() -> Self {
        TextParams { font: None, char_spacing: 0.0, word_spacing: 0.0, scale: 100.0, leading: 0.0, render: 0.0, rise: 0.0 }
    }
}

impl TextParams {
    fn apply(&mut self, op: &RawOp) {
        let n = |k: usize| op.numbers.get(k).copied().filter(|v| v.is_finite()).unwrap_or(0.0);
        match op.operator.as_str() {
            "Tf" => {
                if let Some(Object::Name(name)) = op.operands.first() {
                    self.font = Some((name.clone(), n(1)));
                }
            }
            "Tc" => self.char_spacing = n(0),
            "Tw" => self.word_spacing = n(0),
            "Tz" => self.scale = n(0),
            "TL" => self.leading = n(0),
            "TD" => self.leading = -n(1),
            "Tr" => self.render = n(0),
            "Ts" => self.rise = n(0),
            "\"" => {
                self.word_spacing = n(0);
                self.char_spacing = n(1);
            }
            _ => {}
        }
    }

    /// Operators that turn `self` into `target`.
    fn differences(&self, target: &TextParams) -> Vec<String> {
        let mut ops = Vec::new();
        let f = |v: f64| crate::writer::format_number(v);
        if self.font != target.font {
            // Names are kept as written in the stream, escapes included.
            if let Some((name, size)) = &target.font {
                ops.push(format!("/{} {} Tf", String::from_utf8_lossy(name), f(*size)));
            }
        }
        for (mine, theirs, op) in [
            (self.char_spacing, target.char_spacing, "Tc"),
            (self.word_spacing, target.word_spacing, "Tw"),
            (self.scale, target.scale, "Tz"),
            (self.leading, target.leading, "TL"),
            (self.render, target.render, "Tr"),
            (self.rise, target.rise, "Ts"),
        ] {
            if mine != theirs {
                ops.push(format!("{} {op}", f(theirs)));
            }
        }
        ops
    }
}

/// An operator with its operands, and the byte span they occupy.
struct RawOp {
    operator: String,
    operands: Vec<Object>,
    /// Top-level numeric operands at full precision (lopdf keeps `f32`).
    numbers: Vec<f64>,
    start: usize,
    end: usize,
}

/// Splits a content stream into operators. `None` for inline images
/// (binary data) and syntax this reader does not follow.
fn tokenize(data: &[u8]) -> Option<Vec<RawOp>> {
    let mut lexer = Lexer { data, pos: 0 };
    let mut ops = Vec::new();
    let mut operands = Vec::new();
    let mut numbers = Vec::new();
    let mut start = None;
    loop {
        lexer.skip_space();
        let token_start = lexer.pos;
        let Some(token) = lexer.next()? else { break };
        start.get_or_insert(token_start);
        match token {
            Token::Object(o) => {
                let text = std::str::from_utf8(&data[token_start..lexer.pos]).unwrap_or("");
                numbers.push(if matches!(o, Object::Integer(_) | Object::Real(_)) { text.parse().unwrap_or(f64::NAN) } else { f64::NAN });
                operands.push(o);
            }
            Token::Operator(name) => {
                if name == "BI" {
                    return None;
                }
                ops.push(RawOp {
                    operator: name,
                    operands: std::mem::take(&mut operands),
                    numbers: std::mem::take(&mut numbers),
                    start: start.take().unwrap(),
                    end: lexer.pos,
                });
            }
        }
    }
    Some(ops)
}

enum Token {
    Object(Object),
    Operator(String),
}

struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
}

fn is_white(c: u8) -> bool {
    matches!(c, b' ' | b'\n' | b'\r' | b'\t' | 0x0c | 0)
}

fn is_delimiter(c: u8) -> bool {
    matches!(c, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

impl Lexer<'_> {
    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    fn skip_space(&mut self) {
        while let Some(c) = self.peek() {
            if is_white(c) {
                self.pos += 1;
            } else if c == b'%' {
                while self.peek().is_some_and(|c| c != b'\n' && c != b'\r') {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    /// The next token; `Ok(None)` at the end; `None` (outer) on bad syntax.
    fn next(&mut self) -> Option<Option<Token>> {
        let Some(c) = self.peek() else { return Some(None) };
        Some(Some(match c {
            b'(' => Token::Object(Object::String(self.literal()?, StringFormat::Literal)),
            b'<' if self.data.get(self.pos + 1) == Some(&b'<') => Token::Object(self.dictionary()?),
            b'<' => Token::Object(Object::String(self.hex()?, StringFormat::Hexadecimal)),
            b'[' => Token::Object(self.array()?),
            b'/' => Token::Object(Object::Name(self.name())),
            b')' | b'>' | b']' | b'{' | b'}' => return None,
            _ => {
                let start = self.pos;
                while self.peek().is_some_and(|c| !is_white(c) && !is_delimiter(c)) {
                    self.pos += 1;
                }
                if self.pos == start {
                    return None;
                }
                let word = std::str::from_utf8(&self.data[start..self.pos]).ok()?;
                match word {
                    "true" => Token::Object(Object::Boolean(true)),
                    "false" => Token::Object(Object::Boolean(false)),
                    "null" => Token::Object(Object::Null),
                    _ if word.starts_with(|c: char| c.is_ascii_digit() || matches!(c, '+' | '-' | '.')) => {
                        if word.contains('.') {
                            Token::Object(Object::Real(word.parse().ok()?))
                        } else {
                            Token::Object(Object::Integer(word.parse().ok()?))
                        }
                    }
                    _ => Token::Operator(word.to_owned()),
                }
            }
        }))
    }

    fn literal(&mut self) -> Option<Vec<u8>> {
        self.pos += 1;
        let mut out = Vec::new();
        let mut depth = 1;
        loop {
            let c = self.peek()?;
            self.pos += 1;
            match c {
                b'(' => {
                    depth += 1;
                    out.push(c);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(out);
                    }
                    out.push(c);
                }
                b'\\' => {
                    let e = self.peek()?;
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        b'0'..=b'7' => {
                            let mut v = (e - b'0') as u32;
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + (d - b'0') as u32;
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(v as u8);
                        }
                        other => out.push(other),
                    }
                }
                _ => out.push(c),
            }
        }
    }

    fn hex(&mut self) -> Option<Vec<u8>> {
        self.pos += 1;
        let mut digits = Vec::new();
        loop {
            let c = self.peek()?;
            self.pos += 1;
            match c {
                b'>' => break,
                c if c.is_ascii_hexdigit() => digits.push((c as char).to_digit(16)? as u8),
                c if is_white(c) => {}
                _ => return None,
            }
        }
        if digits.len() % 2 == 1 {
            digits.push(0);
        }
        Some(digits.chunks(2).map(|p| p[0] << 4 | p[1]).collect())
    }

    fn name(&mut self) -> Vec<u8> {
        self.pos += 1;
        let start = self.pos;
        while self.peek().is_some_and(|c| !is_white(c) && !is_delimiter(c)) {
            self.pos += 1;
        }
        // Names only identify resources here; #xx escapes stay as written.
        self.data[start..self.pos].to_vec()
    }

    fn array(&mut self) -> Option<Object> {
        self.pos += 1;
        let mut items = Vec::new();
        loop {
            self.skip_space();
            if self.peek()? == b']' {
                self.pos += 1;
                return Some(Object::Array(items));
            }
            match self.next()?? {
                Token::Object(o) => items.push(o),
                Token::Operator(_) => return None,
            }
        }
    }

    /// Skips a dictionary operand (marked content properties).
    fn dictionary(&mut self) -> Option<Object> {
        self.pos += 2;
        loop {
            self.skip_space();
            if self.data.get(self.pos..self.pos + 2)? == b">>" {
                self.pos += 2;
                return Some(Object::Null);
            }
            match self.next()?? {
                Token::Object(_) => {}
                Token::Operator(_) => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    fn doc_with_font() -> (Document, Dictionary) {
        let mut doc = Document::with_version("1.5");
        let font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Test",
            "FirstChar" => 32, "Widths" => vec![Object::Integer(500); 95],
        });
        (doc, dictionary! { "Font" => dictionary! { "F1" => font } })
    }

    #[test]
    fn removes_runs_entirely_outside_and_keeps_layout() {
        let (doc, resources) = doc_with_font();
        // Left column at x = 50, right column at x = 350; 10 pt, 0.5 em glyphs.
        let content = b"BT /F1 10 Tf 50 700 Td (left one) Tj (more) Tj 300 0 Td (right one) Tj -300 -12 Td (left two) Tj ET";
        let keep = Rect::new(0.0, 0.0, 306.0, 792.0);
        let out = drop_text_outside(&doc, &resources, content, keep).unwrap().unwrap();
        assert_eq!(out.removed, 2, "the run and its Td");
        let text = String::from_utf8(out.content).unwrap();
        let words: Vec<&str> = text.split_whitespace().collect();
        // The removed run takes its Td; the next line is placed absolutely.
        assert_eq!(words.join(" "), "BT /F1 10 Tf 50 700 Td (left one) Tj (more) Tj 0 TL 1 0 0 1 350 700 Tm -300 -12 Td (left two) Tj ET");
    }

    #[test]
    fn removes_paths_and_images_outside_but_not_clips() {
        let (doc, resources) = doc_with_font();
        let keep = Rect::new(0.0, 0.0, 306.0, 792.0);
        let content = b"q 400 400 50 50 re f Q 100 100 m 200 200 l S 400 0 0 400 350 100 re W n 10 10 m 20 20 l S";
        let out = drop_text_outside(&doc, &resources, content, keep).unwrap().unwrap();
        let text = String::from_utf8(out.content).unwrap();
        assert!(!text.contains("400 400 50 50 re"), "{text}");
        assert!(text.contains("100 100 m 200 200 l S"));
        assert!(text.contains("re W n"), "clips stay");
    }

    #[test]
    fn empty_text_objects_go_and_their_state_is_restored() {
        let (doc, resources) = doc_with_font();
        let keep = Rect::new(0.0, 0.0, 306.0, 792.0);
        // The second object (right half) sets a font size, spacing and a
        // color that the third one inherits.
        let content = b"BT /F1 10 Tf 50 700 Td (left) Tj ET BT /F1 20 Tf 2 Tc 1 0 0 rg 400 700 Td (right) Tj ET BT 50 600 Td (inherits) Tj ET";
        let out = drop_text_outside(&doc, &resources, content, keep).unwrap().unwrap();
        let words: Vec<&str> = std::str::from_utf8(&out.content).unwrap().split_whitespace().collect();
        assert_eq!(words.join(" "), "BT /F1 10 Tf 50 700 Td (left) Tj ET 1 0 0 rg /F1 20 Tf 2 Tc BT 50 600 Td (inherits) Tj ET");
    }

    #[test]
    fn keeps_runs_that_reach_into_the_crop_or_cannot_be_measured() {
        let (doc, resources) = doc_with_font();
        // Starts at 250 and is 60 pt long: crosses the right half's edge at 306.
        let crossing = b"BT /F1 10 Tf 250 700 Td (abcdefghijkl) Tj ET";
        assert!(drop_text_outside(&doc, &resources, crossing, Rect::new(306.0, 0.0, 612.0, 792.0)).unwrap().is_none());
        let unknown_font = b"BT /F9 10 Tf 400 700 Td (far right) Tj ET";
        assert!(drop_text_outside(&doc, &resources, unknown_font, Rect::new(0.0, 0.0, 306.0, 792.0)).unwrap().is_none());
    }
}
