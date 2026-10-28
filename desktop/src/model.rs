//! Pages and their non-destructive edits.
//!
//! A page refers to a source page and holds edits: a view (crop and quarter
//! turns), a color mode and two cleanup flags. Pixel operations always run
//! in the canonical order: remove shading, enhance contrast, color mode.

use serde::{Deserialize, Serialize};
use crate::geometry::{Matrix, Rect, rotate_cw_quarters};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorMode {
    Color,
    /// Text as a sharp bilevel layer over color image layers (`mrc`).
    ColorText,
    Gray,
    /// Text as a sharp bilevel layer over gray image layers (`mrc`).
    GrayText,
    BwFlat,
    BwAdaptive,
}

impl ColorMode {
    pub const ALL: [ColorMode; 6] = [ColorMode::Color, ColorMode::ColorText, ColorMode::Gray, ColorMode::GrayText, ColorMode::BwFlat, ColorMode::BwAdaptive];

    pub fn label(self) -> &'static str {
        match self {
            ColorMode::Color => "Original colors",
            ColorMode::ColorText => "Original colors (sharp text)",
            ColorMode::Gray => "Grayscale",
            ColorMode::GrayText => "Grayscale (sharp text)",
            ColorMode::BwFlat => "B&W (flat)",
            ColorMode::BwAdaptive => "B&W (adaptive)",
        }
    }

    pub fn is_bilevel(self) -> bool {
        matches!(self, ColorMode::BwFlat | ColorMode::BwAdaptive)
    }

    /// Text and image layers.
    pub fn is_layered(self) -> bool {
        matches!(self, ColorMode::ColorText | ColorMode::GrayText)
    }

    /// The mode of the pixels: of the image layers for layered modes.
    pub fn image_mode(self) -> ColorMode {
        match self {
            ColorMode::ColorText => ColorMode::Color,
            ColorMode::GrayText => ColorMode::Gray,
            m => m,
        }
    }

    /// Short tag used in output file names.
    pub fn file_tag(self) -> &'static str {
        match self {
            ColorMode::Color => "",
            ColorMode::ColorText => "text",
            ColorMode::Gray => "gray",
            ColorMode::GrayText => "graytext",
            ColorMode::BwFlat => "bw",
            ColorMode::BwAdaptive => "bwprog",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Compression {
    /// Keep the source resolution and quality.
    None,
    Low,
    Medium,
    High,
}

impl Compression {
    pub const ALL: [Compression; 4] = [Compression::None, Compression::Low, Compression::Medium, Compression::High];

    pub fn label(self) -> &'static str {
        match self {
            Compression::None => "No compression",
            Compression::Low => "Low compression",
            Compression::Medium => "Medium compression",
            Compression::High => "High compression",
        }
    }

    pub fn file_tag(self) -> &'static str {
        match self {
            Compression::None => "",
            Compression::Low => "lowcomp",
            Compression::Medium => "medcomp",
            Compression::High => "highcomp",
        }
    }

    /// Maximum output resolution; `None` keeps the source resolution.
    pub fn max_dpi(self, mode: ColorMode) -> Option<f64> {
        let table = match mode.image_mode() {
            ColorMode::Color | ColorMode::ColorText => [180.0, 150.0, 120.0],
            ColorMode::Gray | ColorMode::GrayText => [200.0, 150.0, 120.0],
            ColorMode::BwFlat | ColorMode::BwAdaptive => [260.0, 200.0, 150.0],
        };
        match self {
            Compression::None => None,
            Compression::Low => Some(table[0]),
            Compression::Medium => Some(table[1]),
            Compression::High => Some(table[2]),
        }
    }

    /// JPEG quality for re-encoded pages; `None` keeps the source quality.
    pub fn jpeg_quality(self) -> Option<u8> {
        match self {
            Compression::None => None,
            Compression::Low => Some(75),
            Compression::Medium => Some(60),
            Compression::High => Some(50),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    Left,
    Right,
}

/// What a page is, decided at load time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PageKind {
    /// Treated as an image: rasterized, cleaned up and OCR'd.
    Scan,
    /// Typeset text, copied as-is while it has no pixel operations.
    Text,
    /// Typeset page whose text was removed at load; copied as-is.
    TextStripped,
}

/// Visible part of a source page: a crop in normalized display coordinates
/// (origin top-left, y down, `[0, 1]²`), then clockwise quarter turns.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub crop: Rect,
    pub quarter_turns: u32,
}

impl Default for View {
    fn default() -> Self {
        View { crop: Rect::new(0.0, 0.0, 1.0, 1.0), quarter_turns: 0 }
    }
}

impl View {
    pub fn rotated_cw(self) -> View {
        View { quarter_turns: (self.quarter_turns + 1) % 4, ..self }
    }

    /// Keeps one half of the page as it is currently displayed.
    pub fn split(self, side: Side) -> View {
        let half = match side {
            Side::Left => Rect::new(0.0, 0.0, 0.5, 1.0),
            Side::Right => Rect::new(0.5, 0.0, 1.0, 1.0),
        };
        // Map the half from view coordinates back to unrotated crop
        // coordinates: one inverse clockwise turn is (u, v) -> (v, 1 - u).
        let unturn = |(mut u, mut v): (f64, f64)| {
            for _ in 0..self.quarter_turns {
                (u, v) = (v, 1.0 - u);
            }
            (u, v)
        };
        let (s0, t0) = unturn((half.x0, half.y0));
        let (s1, t1) = unturn((half.x1, half.y1));
        let local = Rect::new(s0, t0, s1, t1);
        let c = self.crop;
        View {
            crop: Rect::new(
                c.x0 + local.x0 * c.width(),
                c.y0 + local.y0 * c.height(),
                c.x0 + local.x1 * c.width(),
                c.y0 + local.y1 * c.height(),
            ),
            quarter_turns: self.quarter_turns,
        }
    }

    pub fn is_identity(&self) -> bool {
        *self == View::default()
    }

    /// Output page size in points for a source page displayed at `size`.
    pub fn output_size(&self, display: (f64, f64)) -> (f64, f64) {
        let w = self.crop.width() * display.0;
        let h = self.crop.height() * display.1;
        if self.quarter_turns % 2 == 1 { (h, w) } else { (w, h) }
    }

    /// Crop rectangle in display space (points, origin bottom-left, y up).
    pub fn crop_in_display(&self, display: (f64, f64)) -> Rect {
        let c = self.crop;
        Rect::new(c.x0 * display.0, (1.0 - c.y1) * display.1, c.x1 * display.0, (1.0 - c.y0) * display.1)
    }

    /// Maps display space (points, y up) to output page space (points, y up).
    pub fn display_to_output(&self, display: (f64, f64)) -> Matrix {
        let crop = self.crop_in_display(display);
        Matrix::translate(-crop.x0, -crop.y0).then(rotate_cw_quarters(crop.width(), crop.height(), self.quarter_turns))
    }
}

pub type SourceId = usize;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub source: SourceId,
    /// Zero-based page index in the source.
    pub index: usize,
    pub kind: PageKind,
    pub view: View,
    pub color: ColorMode,
    pub remove_shading: bool,
    pub enhance_contrast: bool,
}

impl Page {
    pub fn new(source: SourceId, index: usize, kind: PageKind) -> Self {
        let color = if kind == PageKind::Scan { ColorMode::Gray } else { ColorMode::Color };
        Page { source, index, kind, view: View::default(), color, remove_shading: false, enhance_contrast: false }
    }

    pub fn has_pixel_ops(&self) -> bool {
        self.color != ColorMode::Color || self.remove_shading || self.enhance_contrast
    }

    /// Text pages are copied as PDF objects (text, fonts and vectors kept)
    /// until a pixel operation forces rasterization.
    pub fn is_preserved(&self) -> bool {
        self.kind != PageKind::Scan && !self.has_pixel_ops()
    }

    pub fn pixel_ops(&self) -> PixelOps {
        PixelOps { color: self.color, remove_shading: self.remove_shading, enhance_contrast: self.enhance_contrast }
    }
}

/// The pixel operations of a page, in canonical order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PixelOps {
    pub remove_shading: bool,
    pub enhance_contrast: bool,
    pub color: ColorMode,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: Rect, b: Rect) -> bool {
        (a.x0 - b.x0).abs() < 1e-9 && (a.y0 - b.y0).abs() < 1e-9 && (a.x1 - b.x1).abs() < 1e-9 && (a.y1 - b.y1).abs() < 1e-9
    }

    #[test]
    fn split_without_rotation_halves_horizontally() {
        let v = View::default().split(Side::Right);
        assert!(approx(v.crop, Rect::new(0.5, 0.0, 1.0, 1.0)));
    }

    #[test]
    fn split_after_rotation_halves_the_displayed_page() {
        // After one clockwise turn, the displayed left half is the
        // bottom half of the unrotated page.
        let v = View::default().rotated_cw().split(Side::Left);
        assert!(approx(v.crop, Rect::new(0.0, 0.5, 1.0, 1.0)));
        let v = View::default().rotated_cw().rotated_cw().split(Side::Left);
        assert!(approx(v.crop, Rect::new(0.5, 0.0, 1.0, 1.0)));
    }

    #[test]
    fn output_maps_crop_to_page_origin() {
        let v = View::default().split(Side::Right).rotated_cw();
        let m = v.display_to_output((200.0, 100.0));
        // Crop is x in [100, 200], y in [0, 100]; turned, the output is 100x100.
        assert_eq!(v.output_size((200.0, 100.0)), (100.0, 100.0));
        let r = Rect::new(100.0, 0.0, 200.0, 100.0).transform(m);
        assert!(approx(r, Rect::new(0.0, 0.0, 100.0, 100.0)));
    }
}
