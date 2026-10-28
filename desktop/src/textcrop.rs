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
                if bytes.len() % 2 != 0 {
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

/// Removes text runs that lie entirely outside `keep` (user space) from a
/// page's content. Returns `None` when nothing was removed.
pub fn drop_text_outside(doc: &Document, resources: &Dictionary, content: &[u8], keep: Rect) -> Result<Option<Cropped>> {
    let ops = tokenize(content).context("content stream not understood")?;
    let fonts: HashMap<Vec<u8>, Metrics> = match resources.get(b"Font").ok().and_then(|o| doc.dereference(o).ok()) {
        Some((_, Object::Dictionary(fonts))) => fonts
            .iter()
            .filter_map(|(name, f)| Some((name.clone(), Metrics::load(doc, doc.dereference(f).ok()?.1.as_dict().ok()?))))
            .collect(),
        _ => HashMap::new(),
    };

    let number = |o: &Object| o.as_float().ok().map(f64::from);
    let mut ctm = Matrix::IDENTITY;
    let mut stack = Vec::new();
    let mut state = TextState { size: 0.0, char_spacing: 0.0, word_spacing: 0.0, horizontal_scale: 1.0, leading: 0.0, clips: false };
    let mut font: Option<&Metrics> = None;
    let (mut tm, mut tlm) = (Matrix::IDENTITY, Matrix::IDENTITY);

    // Runs: indices of show operators, their box in user space (None when
    // unmeasurable), closed at each positioning operator.
    let mut drop = vec![false; ops.len()];
    let mut run: Vec<usize> = Vec::new();
    let mut run_box: Option<Rect> = None;
    let mut measurable = true;
    let close = |run: &mut Vec<usize>, run_box: &mut Option<Rect>, measurable: &mut bool, drop: &mut Vec<bool>| {
        if !run.is_empty() && *measurable {
            if let Some(b) = run_box {
                if b.intersect(&keep).area() <= 0.0 {
                    for &i in run.iter() {
                        drop[i] = true;
                    }
                }
            }
        }
        run.clear();
        *run_box = None;
        *measurable = true;
    };

    for (i, op) in ops.iter().enumerate() {
        let a = &op.operands;
        let num = |k: usize| a.get(k).and_then(number).unwrap_or(0.0);
        match op.operator.as_str() {
            "q" => stack.push(ctm),
            "Q" => ctm = stack.pop().unwrap_or(ctm),
            "cm" => {
                if let Some(m) = a.iter().map(number).collect::<Option<Vec<f64>>>().and_then(|v| Matrix::from_slice(&v)) {
                    ctm = m.then(ctm);
                }
            }
            "BT" => {
                close(&mut run, &mut run_box, &mut measurable, &mut drop);
                tm = Matrix::IDENTITY;
                tlm = Matrix::IDENTITY;
            }
            "ET" => close(&mut run, &mut run_box, &mut measurable, &mut drop),
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
                close(&mut run, &mut run_box, &mut measurable, &mut drop);
                match op.operator.as_str() {
                    "Td" => tlm = Matrix::translate(num(0), num(1)).then(tlm),
                    "TD" => {
                        state.leading = -num(1);
                        tlm = Matrix::translate(num(0), num(1)).then(tlm);
                    }
                    "Tm" => {
                        if let Some(m) = a.iter().map(number).collect::<Option<Vec<f64>>>().and_then(|v| Matrix::from_slice(&v)) {
                            tlm = m;
                        }
                    }
                    _ => tlm = Matrix::translate(0.0, -state.leading).then(tlm),
                }
                tm = tlm;
            }
            "Tj" | "TJ" | "'" | "\"" => {
                if matches!(op.operator.as_str(), "'" | "\"") {
                    close(&mut run, &mut run_box, &mut measurable, &mut drop);
                    if op.operator == "\"" {
                        state.word_spacing = num(0);
                        state.char_spacing = num(1);
                    }
                    tlm = Matrix::translate(0.0, -state.leading).then(tlm);
                    tm = tlm;
                }
                // Text space advance of this show.
                let mut advance = Some(0.0);
                let strings: Vec<&Object> = match op.operator.as_str() {
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
                run.push(i);
                // Clipping text shapes what is drawn after it: never remove it.
                if state.clips {
                    measurable = false;
                }
                match advance {
                    Some(adv) => {
                        let glyphs = Rect::new(0.0, -0.3 * state.size, adv, state.size);
                        let b = glyphs.transform(tm.then(ctm));
                        run_box = Some(match run_box {
                            Some(r) => Rect::new(r.x0.min(b.x0), r.y0.min(b.y0), r.x1.max(b.x1), r.y1.max(b.y1)),
                            None => b,
                        });
                        tm = Matrix::translate(adv, 0.0).then(tm);
                    }
                    None => measurable = false,
                }
            }
            _ => {}
        }
    }
    close(&mut run, &mut run_box, &mut measurable, &mut drop);

    let removed = drop.iter().filter(|&&d| d).count();
    if removed == 0 {
        return Ok(None);
    }
    let mut out = Vec::with_capacity(content.len());
    let mut at = 0;
    for (op, gone) in ops.iter().zip(&drop) {
        if !gone {
            continue;
        }
        out.extend_from_slice(&content[at..op.start]);
        // Keep the line move of ' and ".
        match op.operator.as_str() {
            "'" => out.extend_from_slice(b" T* "),
            "\"" => {
                let num = |k: usize| op.operands.get(k).and_then(|o| o.as_float().ok()).unwrap_or(0.0);
                out.extend_from_slice(format!(" {} Tw {} Tc T* ", num(0), num(1)).as_bytes());
            }
            _ => out.push(b' '),
        }
        at = op.end;
    }
    out.extend_from_slice(&content[at..]);
    Ok(Some(Cropped { content: out, removed }))
}

/// An operator with its operands, and the byte span they occupy.
struct RawOp {
    operator: String,
    operands: Vec<Object>,
    start: usize,
    end: usize,
}

/// Splits a content stream into operators. `None` for inline images
/// (binary data) and syntax this reader does not follow.
fn tokenize(data: &[u8]) -> Option<Vec<RawOp>> {
    let mut lexer = Lexer { data, pos: 0 };
    let mut ops = Vec::new();
    let mut operands = Vec::new();
    let mut start = None;
    loop {
        lexer.skip_space();
        let token_start = lexer.pos;
        let Some(token) = lexer.next()? else { break };
        start.get_or_insert(token_start);
        match token {
            Token::Object(o) => operands.push(o),
            Token::Operator(name) => {
                if name == "BI" {
                    return None;
                }
                ops.push(RawOp { operator: name, operands: std::mem::take(&mut operands), start: start.take().unwrap(), end: lexer.pos });
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
        assert_eq!(out.removed, 1);
        let text = String::from_utf8(out.content).unwrap();
        assert_eq!(text, "BT /F1 10 Tf 50 700 Td (left one) Tj (more) Tj 300 0 Td   -300 -12 Td (left two) Tj ET");
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
