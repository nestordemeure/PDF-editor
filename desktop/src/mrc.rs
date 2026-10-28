//! Text and image layers (mixed raster content): a page becomes a
//! low-resolution background with the text removed, and a very
//! low-resolution ink color image painted through a full-resolution
//! bilevel text mask. Text stays sharp in lossless JBIG2; JPEG only codes
//! smooth images.

use crate::model::ColorMode;
use crate::pixel::{self, Raster};

/// The layers of a page.
pub struct Layers {
    /// Bilevel, 0 where ink is drawn, at the input resolution.
    pub mask: Raster,
    /// The page without its text, at the requested size.
    pub background: Raster,
    /// Ink colors, at about `INK_PPI`.
    pub foreground: Raster,
}

/// Components taller or wider than this (inches) are pictures or rules,
/// and stay in the background.
const MAX_GLYPH_INCHES: f64 = 0.6;
/// Components inside a window with more ink than this share are parts of
/// a picture (halftone dots, texture).
const MAX_INK_DENSITY: f32 = 0.65;
/// Where a glyph's edge is cut, from its ink (0) to the paper (1): halfway
/// loses the hairlines of soft scans, more makes letters bold.
const EDGE_CUT: f64 = 0.7;
/// Ink color changes slowly: its layer has this resolution.
const INK_PPI: f64 = 16.0;

/// Splits a cleaned-up page (`scale` = resolution / 300 DPI) into layers;
/// the background gets `background_size`.
pub fn separate(cleaned: &Raster, scale: f64, background_size: (u32, u32)) -> anyhow::Result<Layers> {
    let (w, h) = (cleaned.width as usize, cleaned.height as usize);
    let gray = cleaned.to_gray();
    let ink: Vec<bool> = pixel::apply_color_mode(gray.clone(), ColorMode::BwAdaptive, scale).data.iter().map(|&v| v < 128).collect();
    let (glyphs, keep) = text_components(&ink, &gray.data, w, h, scale);
    drop(ink);

    let mask: Vec<u8> = keep.iter().map(|&k| if k { 0 } else { 255 }).collect();
    // Letter edges are softer than the threshold: keep them out of the
    // background too.
    let hole = dilate(&glyphs, w, h, ((1.5 * scale).round() as usize).max(1));
    drop(glyphs);
    // Paper under the text, from a coarse estimate of the page around it.
    let coarse_factor = ((4.0 * scale).round() as u32).max(1);
    let coarse = fill_downsample(cleaned, &hole, coarse_factor, false);
    let mut filled = cleaned.clone();
    let (c, f, cw) = (cleaned.channels as usize, coarse_factor as usize, coarse.width as usize);
    for y in 0..h {
        for x in 0..w {
            if hole[y * w + x] {
                let from = ((y / f) * cw + x / f) * c;
                filled.data[(y * w + x) * c..(y * w + x + 1) * c].copy_from_slice(&coarse.data[from..from + c]);
            }
        }
    }
    let background = if background_size == (filled.width, filled.height) { filled } else { filled.resize(background_size.0, background_size.1)? };
    // Ink color: the darkest quarter of the ink in each block (edges are
    // lighter than strokes).
    let not_ink: Vec<bool> = keep.iter().map(|&k| !k).collect();
    let ink_factor = ((300.0 * scale / INK_PPI).round() as u32).max(1);
    let mut foreground = fill_downsample(cleaned, &not_ink, ink_factor, true);
    smooth(&mut foreground);
    Ok(Layers { mask: Raster { width: w as u32, height: h as u32, channels: 1, data: mask }, background, foreground })
}

/// The connected components (8-connected) of `ink` that look like glyphs
/// (small enough, not in dense ink), and their ink. Adaptive thresholds
/// find letters well but take their soft edges for ink: each glyph is cut
/// at a gray level `EDGE_CUT` of the way from its ink to the paper around it.
fn text_components(ink: &[bool], gray: &[u8], w: usize, h: usize, scale: f64) -> (Vec<bool>, Vec<bool>) {
    let mut label = vec![u32::MAX; w * h];
    let mut parent: Vec<u32> = Vec::new();
    fn find(parent: &mut [u32], mut x: u32) -> u32 {
        while parent[x as usize] != x {
            parent[x as usize] = parent[parent[x as usize] as usize];
            x = parent[x as usize];
        }
        x
    }
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if !ink[i] {
                continue;
            }
            let mut best = u32::MAX;
            let neighbors = [(x > 0).then(|| i - 1), (y > 0 && x > 0).then(|| i - w - 1), (y > 0).then(|| i - w), (y > 0 && x + 1 < w).then(|| i - w + 1)];
            for n in neighbors.into_iter().flatten() {
                if label[n] != u32::MAX {
                    let r = find(&mut parent, label[n]);
                    if best == u32::MAX {
                        best = r;
                    } else if r != best {
                        let (a, b) = (r.min(best), r.max(best));
                        parent[b as usize] = a;
                        best = a;
                    }
                }
            }
            if best == u32::MAX {
                best = parent.len() as u32;
                parent.push(best);
            }
            label[i] = best;
        }
    }
    // Bounding boxes and pixel counts per root.
    let mut boxes: Vec<(usize, usize, usize, usize, usize)> = vec![(usize::MAX, usize::MAX, 0, 0, 0); parent.len()];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if label[i] == u32::MAX {
                continue;
            }
            let r = find(&mut parent, label[i]) as usize;
            label[i] = r as u32;
            let b = &mut boxes[r];
            b.0 = b.0.min(x);
            b.1 = b.1.min(y);
            b.2 = b.2.max(x);
            b.3 = b.3.max(y);
            b.4 += 1;
        }
    }
    let max_side = (MAX_GLYPH_INCHES * 300.0 * scale) as usize;
    let density = InkDensity::new(ink, w, h, ((0.08 * 300.0 * scale) as usize).max(2));
    let mut glyph = vec![false; parent.len()];
    let mut dense = vec![0usize; parent.len()];
    for y in 0..h {
        for x in 0..w {
            let l = label[y * w + x];
            if l != u32::MAX && density.at(x, y) > MAX_INK_DENSITY {
                dense[l as usize] += 1;
            }
        }
    }
    for (r, b) in boxes.iter().enumerate() {
        if b.4 == 0 {
            continue;
        }
        let (bw, bh) = (b.2 - b.0 + 1, b.3 - b.1 + 1);
        glyph[r] = bw <= max_side && bh <= max_side && dense[r] * 2 < b.4;
    }
    let mut cut = vec![0u8; parent.len()];
    let mut values: Vec<u8> = Vec::new();
    for (r, b) in boxes.iter().enumerate() {
        if !glyph[r] {
            continue;
        }
        // Ink: the darkest tenth of the glyph. Paper: the lightest tenth
        // of its box, one stroke wider.
        values.clear();
        let (x0, y0, x1, y1) = (b.0.saturating_sub(2), b.1.saturating_sub(2), (b.2 + 3).min(w), (b.3 + 3).min(h));
        let mut glyph_values: Vec<u8> = Vec::new();
        for y in y0..y1 {
            for x in x0..x1 {
                let i = y * w + x;
                if label[i] == r as u32 {
                    glyph_values.push(gray[i]);
                } else {
                    values.push(gray[i]);
                }
            }
        }
        let ink_level = percentile(&mut glyph_values, 0.1);
        let paper_level = if values.is_empty() { 255 } else { percentile(&mut values, 0.9) };
        cut[r] = (ink_level as f64 + EDGE_CUT * (paper_level as f64 - ink_level as f64)).round() as u8;
    }
    let glyphs: Vec<bool> = label.iter().map(|&l| l != u32::MAX && glyph[l as usize]).collect();
    let keep = (0..w * h).map(|i| glyphs[i] && gray[i] < cut[label[i] as usize]).collect();
    (glyphs, keep)
}

/// The value below which `share` of `values` lie (reorders them).
fn percentile(values: &mut [u8], share: f64) -> u8 {
    if values.is_empty() {
        return 0;
    }
    let k = ((values.len() - 1) as f64 * share) as usize;
    *values.select_nth_unstable(k).1
}

/// Share of ink in the square window of half side `radius` around a pixel,
/// counted on cells of `CELL`² pixels.
struct InkDensity {
    integral: Vec<u32>,
    cw: usize,
    ch: usize,
    radius: usize,
}

const CELL: usize = 4;

impl InkDensity {
    fn new(ink: &[bool], w: usize, h: usize, radius: usize) -> Self {
        let (cw, ch) = (w.div_ceil(CELL), h.div_ceil(CELL));
        let mut cells = vec![0u32; cw * ch];
        for y in 0..h {
            for x in 0..w {
                cells[(y / CELL) * cw + x / CELL] += ink[y * w + x] as u32;
            }
        }
        let mut integral = vec![0u32; (cw + 1) * (ch + 1)];
        for y in 0..ch {
            let mut row = 0u32;
            for x in 0..cw {
                row += cells[y * cw + x];
                integral[(y + 1) * (cw + 1) + x + 1] = integral[y * (cw + 1) + x + 1] + row;
            }
        }
        InkDensity { integral, cw, ch, radius: (radius / CELL).max(1) }
    }

    fn at(&self, x: usize, y: usize) -> f32 {
        let (w, r, i) = (self.cw, self.radius, &self.integral);
        let (x, y) = (x / CELL, y / CELL);
        let (x0, x1) = (x.saturating_sub(r), (x + r + 1).min(w));
        let (y0, y1) = (y.saturating_sub(r), (y + r + 1).min(self.ch));
        let sum = i[y1 * (w + 1) + x1] + i[y0 * (w + 1) + x0] - i[y0 * (w + 1) + x1] - i[y1 * (w + 1) + x0];
        sum as f32 / ((x1 - x0) * (y1 - y0) * CELL * CELL) as f32
    }
}

/// Square dilation of a mask.
fn dilate(mask: &[bool], w: usize, h: usize, radius: usize) -> Vec<bool> {
    let mut rows = vec![false; w * h];
    for y in 0..h {
        let line = &mask[y * w..(y + 1) * w];
        let mut count = 0usize;
        for x in 0..(w + radius) {
            if x < w && line[x] {
                count += 1;
            }
            if x >= 2 * radius + 1 && line[x - 2 * radius - 1] {
                count -= 1;
            }
            if x >= radius && x - radius < w {
                rows[y * w + x - radius] = count > 0;
            }
        }
    }
    let mut out = vec![false; w * h];
    for x in 0..w {
        let mut count = 0usize;
        for y in 0..(h + radius) {
            if y < h && rows[y * w + x] {
                count += 1;
            }
            if y >= 2 * radius + 1 && rows[(y - 2 * radius - 1) * w + x] {
                count -= 1;
            }
            if y >= radius && y - radius < h {
                out[(y - radius) * w + x] = count > 0;
            }
        }
    }
    out
}

/// Averages `factor`² blocks over the pixels outside `hole` (with
/// `darkest`, over the darkest quarter of them); blocks that are all hole
/// take the mean of their filled neighbors.
fn fill_downsample(r: &Raster, hole: &[bool], factor: u32, darkest: bool) -> Raster {
    let (w, h, c) = (r.width as usize, r.height as usize, r.channels as usize);
    let f = factor.max(1) as usize;
    let (bw, bh) = (w.div_ceil(f), h.div_ceil(f));
    let mut sum = vec![0u64; bw * bh * c];
    let mut count = vec![0u32; bw * bh];
    let luma = |i: usize| if c == 1 { r.data[i] as u32 } else { (77 * r.data[i * 3] as u32 + 150 * r.data[i * 3 + 1] as u32 + 29 * r.data[i * 3 + 2] as u32) >> 8 };
    let mut members: Vec<(u32, usize)> = Vec::new();
    for by in 0..bh {
        for bx in 0..bw {
            members.clear();
            for y in by * f..((by + 1) * f).min(h) {
                for x in bx * f..((bx + 1) * f).min(w) {
                    let i = y * w + x;
                    if !hole[i] {
                        members.push((luma(i), i));
                    }
                }
            }
            if darkest && members.len() > 3 {
                let quarter = members.len() / 4;
                members.select_nth_unstable(quarter);
                members.truncate(quarter);
            }
            let b = by * bw + bx;
            count[b] = members.len() as u32;
            for &(_, i) in &members {
                for k in 0..c {
                    sum[b * c + k] += r.data[i * c + k] as u64;
                }
            }
        }
    }
    let mut data = vec![0u8; bw * bh * c];
    let mut known: Vec<bool> = count.iter().map(|&n| n > 0).collect();
    for b in 0..bw * bh {
        if known[b] {
            for k in 0..c {
                data[b * c + k] = (sum[b * c + k] / count[b] as u64) as u8;
            }
        }
    }
    // Grow known values into holes, a ring at a time.
    for _ in 0..(bw.max(bh)) {
        let mut changed = false;
        let mut next = known.clone();
        for y in 0..bh {
            for x in 0..bw {
                let b = y * bw + x;
                if known[b] {
                    continue;
                }
                let mut acc = [0u32; 3];
                let mut n = 0;
                for (dx, dy) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)] {
                    let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                    if nx < 0 || ny < 0 || nx >= bw as i64 || ny >= bh as i64 {
                        continue;
                    }
                    let nb = ny as usize * bw + nx as usize;
                    if known[nb] {
                        for k in 0..c {
                            acc[k] += data[nb * c + k] as u32;
                        }
                        n += 1;
                    }
                }
                if n > 0 {
                    for k in 0..c {
                        data[b * c + k] = (acc[k] / n) as u8;
                    }
                    next[b] = true;
                    changed = true;
                }
            }
        }
        known = next;
        if !changed {
            break;
        }
    }
    Raster { width: bw as u32, height: bh as u32, channels: c as u8, data }
}

/// The page as a reader draws it: ink color through the mask, background
/// elsewhere. Previews show this.
pub fn composite(layers: &Layers) -> Raster {
    let (m, bg, fg) = (&layers.mask, &layers.background, &layers.foreground);
    let (w, h, c) = (m.width as usize, m.height as usize, bg.channels as usize);
    let mut data = Vec::with_capacity(m.pixel_count() * c);
    for y in 0..h {
        for x in 0..w {
            let src = if m.data[y * w + x] == 0 { fg } else { bg };
            let (sx, sy) = (x * src.width as usize / w, y * src.height as usize / h);
            let i = (sy * src.width as usize + sx) * c;
            data.extend_from_slice(&src.data[i..i + c]);
        }
    }
    Raster { width: m.width, height: m.height, channels: c as u8, data }
}

/// 3×3 mean, which keeps JPEG from spending bytes on block-to-block noise.
fn smooth(r: &mut Raster) {
    let (w, h, c) = (r.width as usize, r.height as usize, r.channels as usize);
    let src = r.data.clone();
    for y in 0..h {
        for x in 0..w {
            for k in 0..c {
                let (mut sum, mut n) = (0u32, 0u32);
                for ny in y.saturating_sub(1)..(y + 2).min(h) {
                    for nx in x.saturating_sub(1)..(x + 2).min(w) {
                        sum += src[(ny * w + nx) * c + k] as u32;
                        n += 1;
                    }
                }
                r.data[(y * w + x) * c + k] = (sum / n) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 300 DPI gray page: 3 mm "letters" on a row, and a 1 inch dark picture.
    fn page() -> Raster {
        let (w, h) = (900u32, 600u32);
        let mut data = vec![235u8; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let letter = (100..135).contains(&y) && x >= 60 && x < 460 && (x - 60) % 40 < 25;
                let picture = (300..600).contains(&x) && (250..550).contains(&y);
                if letter {
                    data[(y * w + x) as usize] = 20;
                } else if picture {
                    data[(y * w + x) as usize] = 60;
                }
            }
        }
        Raster::new(w, h, 1, data).unwrap()
    }

    #[test]
    fn letters_go_to_the_mask_and_pictures_stay_in_the_background() {
        let r = page();
        let layers = separate(&r, 1.0, (300, 200)).unwrap();
        let at = |x: usize, y: usize| layers.mask.data[y * 900 + x];
        assert_eq!(at(70, 110), 0, "a letter is in the mask");
        assert_eq!(at(450, 400), 255, "the picture is not");
        // The letter is gone from the background, the picture is kept.
        let bg = |x: usize, y: usize| layers.background.data[(y / 3) * layers.background.width as usize + x / 3];
        assert!(bg(70, 110) > 200, "background under a letter is paper: {}", bg(70, 110));
        assert!(bg(450, 400) < 200, "the picture stays dark: {}", bg(450, 400));
        // Ink is dark; drawn back, the letter is dark again.
        assert!(composite(&layers).data[110 * 900 + 70] < 60);
    }

    #[test]
    fn a_blank_page_has_an_empty_mask() {
        let r = Raster::new(300, 300, 1, vec![240; 90000]).unwrap();
        let layers = separate(&r, 1.0, (150, 150)).unwrap();
        assert!(layers.mask.data.iter().all(|&v| v == 255));
        assert!(layers.background.data.iter().all(|&v| v == 240));
    }
}
