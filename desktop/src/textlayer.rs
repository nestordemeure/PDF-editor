//! The invisible text layer drawn over page images, from OCR lines.

use crate::geometry::Matrix;
use crate::writer::format_number;
use serde::{Deserialize, Serialize};

/// Pixel box: left, top, right, bottom (y down).
pub type PixelBox = (i32, i32, i32, i32);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OcrLine {
    pub bbox: PixelBox,
    /// Two points on the baseline.
    pub baseline: (i32, i32, i32, i32),
    pub words: Vec<OcrWord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OcrWord {
    pub text: String,
    pub bbox: PixelBox,
}

/// Name of the text-layer font in page resources.
pub const FONT_NAME: &str = "OCR";

/// Content stream operators drawing invisible text (render mode 3) for
/// lines found on an upright image. `px_to_page` maps image pixels (y down)
/// to page points (y up).
pub fn text_layer(lines: &[OcrLine], px_to_page: Matrix) -> Vec<u8> {
    let sx = px_to_page.a.abs();
    let sy = px_to_page.d.abs();
    let f = format_number;
    let mut out = String::from("BT 3 Tr\n");
    for line in lines {
        let (bx1, by1, bx2, by2) = line.baseline;
        let slope = if bx2 != bx1 { (by2 - by1) as f64 / (bx2 - bx1) as f64 } else { 0.0 };
        let angle = (-(by2 - by1) as f64 * sy).atan2(((bx2 - bx1) as f64 * sx).max(1e-9));
        let (cos, sin) = (angle.cos(), angle.sin());
        // Glyphs span the em above the origin: the origin sits on the
        // line's bottom edge and the em is the line height.
        let descent = ((line.bbox.3 - by1) as f64).max(0.0);
        let size = ((line.bbox.3 - line.bbox.1) as f64 * sy).max(1.0);
        out.push_str(&format!("/{FONT_NAME} {} Tf\n", f(size)));
        for (i, word) in line.words.iter().enumerate() {
            let next = line.words.get(i + 1);
            let mut text = word.text.clone();
            if next.is_some() {
                text.push(' ');
            }
            let units: Vec<u16> = text.encode_utf16().collect();
            let x0 = word.bbox.0 as f64;
            let base_y = by1 as f64 + slope * (x0 - bx1 as f64) + descent;
            let span_px = match next {
                Some(n) => n.bbox.0 - word.bbox.0,
                None => word.bbox.2 - word.bbox.0,
            };
            let natural = units.len() as f64 * 0.5 * size;
            let stretch = (100.0 * span_px.max(1) as f64 * sx / natural).clamp(1.0, 10000.0);
            let (x, y) = px_to_page.apply(x0, base_y);
            out.push_str(&format!("{} {} {} {} {} {} Tm {} Tz <", f(cos), f(sin), f(-sin), f(cos), f(x), f(y), f(stretch)));
            for u in units {
                out.push_str(&format!("{u:04X}"));
            }
            out.push_str("> Tj\n");
        }
    }
    out.push_str("ET\n");
    out.into_bytes()
}

pub const GLYPHLESS_FONT: &[u8] = include_bytes!("../assets/glyphless.ttf");

pub const TO_UNICODE_CMAP: &str = "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n\
1 beginbfrange\n<0000> <FFFF> <0000>\nendbfrange\n\
endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n";
