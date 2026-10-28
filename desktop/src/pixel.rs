//! Raster images and the pixel pipeline.
//!
//! Thumbnails, previews and saved pages all go through `apply_pixel_ops`,
//! which is what keeps the preview equal to the output. Every operation
//! takes a `scale` (resolution relative to 300 DPI) so its result does not
//! depend on the working resolution.

use crate::model::{ColorMode, PixelOps};
use anyhow::{Result, bail};
use fast_image_resize as fir;

/// 8-bit image, 1 (gray) or 3 (RGB) interleaved channels, rows packed.
#[derive(Clone, Debug, PartialEq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub channels: u8,
    pub data: Vec<u8>,
}

impl Raster {
    pub fn new(width: u32, height: u32, channels: u8, data: Vec<u8>) -> Result<Self> {
        if channels != 1 && channels != 3 {
            bail!("unsupported channel count {channels}");
        }
        if data.len() != width as usize * height as usize * channels as usize {
            bail!("raster buffer is {} bytes, expected {width}x{height}x{channels}", data.len());
        }
        Ok(Raster { width, height, channels, data })
    }

    pub fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }

    pub fn is_gray(&self) -> bool {
        self.channels == 1
    }

    /// Luma (ITU-R BT.601, as JPEG uses).
    pub fn to_gray(&self) -> Raster {
        if self.is_gray() {
            return self.clone();
        }
        let data = self
            .data
            .chunks_exact(3)
            .map(|p| ((77 * p[0] as u32 + 150 * p[1] as u32 + 29 * p[2] as u32 + 128) >> 8) as u8)
            .collect();
        Raster { width: self.width, height: self.height, channels: 1, data }
    }

    pub fn to_rgb(&self) -> Raster {
        if !self.is_gray() {
            return self.clone();
        }
        let data = self.data.iter().flat_map(|&v| [v, v, v]).collect();
        Raster { width: self.width, height: self.height, channels: 3, data }
    }

    /// Crops to the pixel rectangle `[x0, x1) × [y0, y1)`.
    pub fn crop(&self, x0: u32, y0: u32, x1: u32, y1: u32) -> Raster {
        let (x1, y1) = (x1.min(self.width), y1.min(self.height));
        let (x0, y0) = (x0.min(x1), y0.min(y1));
        if (x0, y0, x1, y1) == (0, 0, self.width, self.height) {
            return self.clone();
        }
        let c = self.channels as usize;
        let row = self.width as usize * c;
        let mut data = Vec::with_capacity((x1 - x0) as usize * (y1 - y0) as usize * c);
        for y in y0..y1 {
            let start = y as usize * row + x0 as usize * c;
            data.extend_from_slice(&self.data[start..start + (x1 - x0) as usize * c]);
        }
        Raster { width: x1 - x0, height: y1 - y0, channels: self.channels, data }
    }

    /// Rotates clockwise by `quarter_turns` × 90°.
    pub fn rotate_cw(&self, quarter_turns: u32) -> Raster {
        let q = quarter_turns % 4;
        if q == 0 {
            return self.clone();
        }
        let (w, h, c) = (self.width as usize, self.height as usize, self.channels as usize);
        let (nw, nh) = if q % 2 == 1 { (h, w) } else { (w, h) };
        let mut data = vec![0u8; self.data.len()];
        for y in 0..h {
            for x in 0..w {
                let (nx, ny) = match q {
                    1 => (h - 1 - y, x),
                    2 => (w - 1 - x, h - 1 - y),
                    _ => (y, w - 1 - x),
                };
                let src = (y * w + x) * c;
                let dst = (ny * nw + nx) * c;
                data[dst..dst + c].copy_from_slice(&self.data[src..src + c]);
            }
        }
        Raster { width: nw as u32, height: nh as u32, channels: self.channels, data }
    }

    /// Resamples to `width × height` (Catmull-Rom: sharp, little ringing).
    pub fn resize(&self, width: u32, height: u32) -> Result<Raster> {
        if (width, height) == (self.width, self.height) {
            return Ok(self.clone());
        }
        let pixel_type = if self.is_gray() { fir::PixelType::U8 } else { fir::PixelType::U8x3 };
        let src = fir::images::ImageRef::new(self.width, self.height, &self.data, pixel_type)?;
        let mut dst = fir::images::Image::new(width.max(1), height.max(1), pixel_type);
        let options = fir::ResizeOptions::new()
            .resize_alg(fir::ResizeAlg::Convolution(fir::FilterType::CatmullRom));
        fir::Resizer::new().resize(&src, &mut dst, &options)?;
        Raster::new(dst.width(), dst.height(), self.channels, dst.into_vec())
    }
}

/// Applies a page's pixel operations in canonical order: gray conversion
/// (for every mode but color), cleanup, then thresholding. Bilevel modes
/// return a gray raster holding only 0 and 255.
pub fn apply_pixel_ops(raster: Raster, ops: PixelOps, scale: f64) -> Raster {
    apply_color_mode(apply_cleanup(raster, ops, scale), ops.color, scale)
}

/// Gray conversion (unless the mode keeps color), shading removal, then
/// contrast: everything before thresholding.
pub fn apply_cleanup(raster: Raster, ops: PixelOps, scale: f64) -> Raster {
    let mut r = if ops.color.image_mode() == ColorMode::Color { raster } else { raster.to_gray() };
    if ops.remove_shading {
        r = remove_shading(&r, scale);
    }
    if ops.enhance_contrast {
        enhance_contrast(&mut r, scale);
    }
    r
}

/// The color half of the pipeline, last so thresholds see real gray values.
pub fn apply_color_mode(r: Raster, mode: ColorMode, scale: f64) -> Raster {
    match mode {
        ColorMode::Color | ColorMode::ColorText => r,
        ColorMode::Gray | ColorMode::GrayText => r.to_gray(),
        ColorMode::BwFlat => binarize_otsu(&r.to_gray()),
        ColorMode::BwAdaptive => binarize_sauvola(&r.to_gray(), scale),
    }
}

fn scaled_radius(base: f64, scale: f64, min: usize) -> usize {
    ((base * scale).round() as usize).max(min)
}

/// Flattens uneven lighting (scanner shadows, page curvature) by dividing
/// each channel by an estimate of the paper background. The background is
/// a max filter (which erases text strokes) followed by a smooth blur.
/// Where that filtered image is still textured, the area is a picture,
/// not paper, and is left as it is.
fn remove_shading(r: &Raster, scale: f64) -> Raster {
    let (w, h, c) = (r.width as usize, r.height as usize, r.channels as usize);
    let dilate = scaled_radius(12.0, scale, 1);
    let blur = scaled_radius(25.0, scale, 1);
    let paper_weight = {
        let luma = r.to_gray();
        let dilated = max_filter(&luma.data, w, h, dilate);
        let mut mean = dilated.clone();
        box_blur3(&mut mean, w, h, blur);
        let texture: Vec<u8> = dilated.iter().zip(&mean).map(|(&d, &m)| d.abs_diff(m)).collect();
        // Below 4 levels of texture: paper (weight 255); above 16: picture
        // (0). A wide blur keeps the transition invisible.
        let mut weight: Vec<u8> = texture.iter().map(|&t| ((16.0 - t as f32).clamp(0.0, 12.0) / 12.0 * 255.0) as u8).collect();
        box_blur3(&mut weight, w, h, blur * 3);
        weight.iter().map(|&v| v as f32 / 255.0).collect::<Vec<f32>>()
    };
    let mut out = vec![0u8; r.data.len()];
    for ch in 0..c {
        let plane: Vec<u8> = r.data.iter().skip(ch).step_by(c).copied().collect();
        let mut bg = max_filter(&plane, w, h, dilate);
        box_blur3(&mut bg, w, h, blur);
        for i in 0..w * h {
            // Within 8% of the background counts as paper: pure white.
            let b = bg[i].max(1) as f32 * 0.92;
            let v = plane[i] as f32;
            let corrected = (v * 255.0 / b).min(255.0);
            out[i * c + ch] = (v + paper_weight[i] * (corrected - v)).round() as u8;
        }
    }
    Raster { width: r.width, height: r.height, channels: r.channels, data: out }
}

/// Separable square max filter of the given radius.
fn max_filter(src: &[u8], w: usize, h: usize, radius: usize) -> Vec<u8> {
    let mut tmp = vec![0u8; src.len()];
    for y in 0..h {
        sliding_max(&src[y * w..(y + 1) * w], &mut tmp[y * w..(y + 1) * w], radius);
    }
    let mut out = vec![0u8; src.len()];
    let mut col = vec![0u8; h];
    let mut col_out = vec![0u8; h];
    for x in 0..w {
        for y in 0..h {
            col[y] = tmp[y * w + x];
        }
        sliding_max(&col, &mut col_out, radius);
        for y in 0..h {
            out[y * w + x] = col_out[y];
        }
    }
    out
}

/// Running maximum over `[i - radius, i + radius]` with a monotonic deque.
fn sliding_max(src: &[u8], dst: &mut [u8], radius: usize) {
    let n = src.len();
    let mut deque = std::collections::VecDeque::with_capacity(2 * radius + 2);
    let mut next = 0;
    for i in 0..n {
        let hi = (i + radius).min(n - 1);
        while next <= hi {
            while deque.back().is_some_and(|&j: &usize| src[j] <= src[next]) {
                deque.pop_back();
            }
            deque.push_back(next);
            next += 1;
        }
        while deque.front().is_some_and(|&j| j + radius < i) {
            deque.pop_front();
        }
        dst[i] = src[*deque.front().unwrap()];
    }
}

/// Three box blurs (close to a Gaussian), in place.
fn box_blur3(data: &mut [u8], w: usize, h: usize, radius: usize) {
    let mut tmp = vec![0u8; data.len()];
    for _ in 0..3 {
        for y in 0..h {
            box_blur_line(&data[y * w..(y + 1) * w], &mut tmp[y * w..(y + 1) * w], radius);
        }
        let mut col = vec![0u8; h];
        let mut col_out = vec![0u8; h];
        for x in 0..w {
            for y in 0..h {
                col[y] = tmp[y * w + x];
            }
            box_blur_line(&col, &mut col_out, radius);
            for y in 0..h {
                data[y * w + x] = col_out[y];
            }
        }
    }
}

/// Mean over `[i - radius, i + radius]`, clipped at the edges.
fn box_blur_line(src: &[u8], dst: &mut [u8], radius: usize) {
    let n = src.len();
    let mut sum: u32 = src[..(radius + 1).min(n)].iter().map(|&v| v as u32).sum();
    let mut count = (radius + 1).min(n) as u32;
    for i in 0..n {
        dst[i] = ((sum + count / 2) / count) as u8;
        if i + radius + 1 < n {
            sum += src[i + radius + 1] as u32;
            count += 1;
        }
        if i >= radius {
            sum -= src[i - radius] as u32;
            count -= 1;
        }
    }
}

/// Maps the ink median to black and the paper median to white. Only
/// pixels near paper count: a max filter erases text strokes, so where it
/// stays dark the area is a scanner border or a cover, not text. Otsu's
/// threshold then splits ink from paper. Where ink and paper do not
/// separate well (cloth, pictures), it stretches between the 0.5th and
/// 99.5th percentiles instead.
fn enhance_contrast(r: &mut Raster, scale: f64) {
    let gray = r.to_gray();
    let (w, h) = (gray.width as usize, gray.height as usize);
    let mut hist = [0u64; 256];
    for &v in &gray.data {
        hist[v as usize] += 1;
    }
    let (lo, hi) = percentile_range(&hist, 0.005);
    let local_paper = max_filter(&gray.data, w, h, scaled_radius(10.0, scale, 1));
    let (lo, hi) = match ink_range(&gray.data, &local_paper) {
        Some((ink_lo, ink_hi, separation)) => {
            let t = ((separation - 0.6) / 0.1).clamp(0.0, 1.0);
            (lo + t * (ink_lo - lo), hi + t * (ink_hi - hi))
        }
        None => (lo, hi),
    };
    if hi - lo < 16.0 {
        return;
    }
    let lut: Vec<u8> = (0..256).map(|v| ((v as f64 - lo) * 255.0 / (hi - lo)).round().clamp(0.0, 255.0) as u8).collect();
    for v in r.data.iter_mut() {
        *v = lut[*v as usize];
    }
}

/// Levels below and above which `tail` of the pixels lie.
fn percentile_range(hist: &[u64; 256], tail: f64) -> (f64, f64) {
    let limit = (hist.iter().sum::<u64>() as f64 * tail) as u64;
    let find = |bins: &mut dyn Iterator<Item = usize>| {
        let mut acc = 0;
        for i in bins {
            acc += hist[i];
            if acc > limit {
                return Some(i);
            }
        }
        None
    };
    let lo = find(&mut (0..256)).unwrap_or(0);
    let hi = find(&mut (0..256).rev()).unwrap_or(255);
    (lo as f64, hi as f64)
}

/// Ink median, paper median and Otsu's separation (0 to 1) of the pixels
/// near paper.
fn ink_range(gray: &[u8], local_paper: &[u8]) -> Option<(f64, f64, f64)> {
    // Largest gain: on pages without ink, it limits the paper noise.
    const MIN_RANGE: usize = 102;
    let mut paper_hist = [0u64; 256];
    for &v in local_paper {
        paper_hist[v as usize] += 1;
    }
    let paper_level = median(&paper_hist)?;
    let mut hist = [0u64; 256];
    for (&v, &p) in gray.iter().zip(local_paper) {
        if p as usize * 5 >= paper_level * 3 {
            hist[v as usize] += 1;
        }
    }
    let (t, separation) = otsu_split(&hist);
    let t = t as usize;
    let ink = median(&hist[..=t])?;
    let paper = median(&hist[t + 1..])? + t + 1;
    Some((ink.min(paper.saturating_sub(MIN_RANGE)) as f64, paper as f64, separation))
}

/// Index of the median of a histogram, `None` when it is empty.
fn median(hist: &[u64]) -> Option<usize> {
    let total: u64 = hist.iter().sum();
    let mut acc = 0;
    hist.iter().position(|&n| {
        acc += n;
        total > 0 && acc * 2 >= total
    })
}

/// Global threshold that best separates ink from paper.
pub fn otsu_threshold(gray: &Raster) -> u8 {
    let mut hist = [0u64; 256];
    for &v in &gray.data {
        hist[v as usize] += 1;
    }
    otsu_split(&hist).0
}

/// Otsu's threshold and its separation: the between-class variance over
/// the total variance.
fn otsu_split(hist: &[u64; 256]) -> (u8, f64) {
    let total = hist.iter().sum::<u64>() as f64;
    let sum: f64 = (0..256).map(|i| i as f64 * hist[i] as f64).sum();
    let (mut sum_b, mut w_b, mut best, mut threshold) = (0.0, 0.0, 0.0, 128u8);
    for i in 0..256 {
        w_b += hist[i] as f64;
        if w_b == 0.0 {
            continue;
        }
        let w_f = total - w_b;
        if w_f == 0.0 {
            break;
        }
        sum_b += i as f64 * hist[i] as f64;
        let m_b = sum_b / w_b;
        let m_f = (sum - sum_b) / w_f;
        let between = w_b * w_f * (m_b - m_f) * (m_b - m_f);
        if between > best {
            best = between;
            threshold = i as u8;
        }
    }
    let mean = sum / total;
    let variance: f64 = (0..256).map(|i| (i as f64 - mean).powi(2) * hist[i] as f64).sum::<f64>() / total;
    let separation = if variance > 0.0 { best / (total * total * variance) } else { 0.0 };
    (threshold, separation)
}

/// Otsu refined to the midpoint of the ink and paper means (isodata).
/// Otsu alone drifts toward the ink on pages that are mostly paper, which
/// thins strokes; the midpoint keeps their width.
pub fn flat_threshold(gray: &Raster) -> u8 {
    let mut hist = [0u64; 256];
    for &v in &gray.data {
        hist[v as usize] += 1;
    }
    let mut t = otsu_threshold(gray) as usize;
    for _ in 0..32 {
        let mean = |range: std::ops::Range<usize>| {
            let (n, sum) = range.fold((0u64, 0u64), |(n, s), i| (n + hist[i], s + hist[i] * i as u64));
            (n > 0).then(|| sum as f64 / n as f64)
        };
        let (Some(ink), Some(paper)) = (mean(0..t + 1), mean(t + 1..256)) else { break };
        let next = ((ink + paper) / 2.0).round() as usize;
        if next == t {
            break;
        }
        t = next.min(254);
    }
    t as u8
}

fn binarize_otsu(gray: &Raster) -> Raster {
    let t = flat_threshold(gray);
    let data = gray.data.iter().map(|&v| if v > t { 255 } else { 0 }).collect();
    Raster { width: gray.width, height: gray.height, channels: 1, data }
}

/// Sauvola local threshold: `T = m (1 + k (s / 128 - 1))` over a window
/// that scales with resolution. Running column sums keep memory linear in
/// the image width.
fn binarize_sauvola(gray: &Raster, scale: f64) -> Raster {
    const K: f64 = 0.2;
    let (w, h) = (gray.width as usize, gray.height as usize);
    let radius = scaled_radius(40.0, scale, 1);
    let src = &gray.data;
    let mut out = vec![0u8; w * h];
    let mut col_sum = vec![0u64; w];
    let mut col_sq = vec![0u64; w];
    let add_row = |y: usize, sign: i64, col_sum: &mut [u64], col_sq: &mut [u64]| {
        for x in 0..w {
            let v = src[y * w + x] as i64;
            col_sum[x] = (col_sum[x] as i64 + sign * v) as u64;
            col_sq[x] = (col_sq[x] as i64 + sign * v * v) as u64;
        }
    };
    for y in 0..(radius + 1).min(h) {
        add_row(y, 1, &mut col_sum, &mut col_sq);
    }
    for y in 0..h {
        let rows = ((y + radius + 1).min(h) - y.saturating_sub(radius)) as u64;
        let (mut s, mut sq) = (0u64, 0u64);
        for x in 0..(radius + 1).min(w) {
            s += col_sum[x];
            sq += col_sq[x];
        }
        for x in 0..w {
            let cols = ((x + radius + 1).min(w) - x.saturating_sub(radius)) as u64;
            let n = (rows * cols) as f64;
            let mean = s as f64 / n;
            let var = (sq as f64 / n - mean * mean).max(0.0);
            let t = mean * (1.0 + K * (var.sqrt() / 128.0 - 1.0));
            out[y * w + x] = if src[y * w + x] as f64 > t { 255 } else { 0 };
            if x + radius + 1 < w {
                s += col_sum[x + radius + 1];
                sq += col_sq[x + radius + 1];
            }
            if x >= radius {
                s -= col_sum[x - radius];
                sq -= col_sq[x - radius];
            }
        }
        if y + radius + 1 < h {
            add_row(y + radius + 1, 1, &mut col_sum, &mut col_sq);
        }
        if y >= radius {
            add_row(y - radius, -1, &mut col_sum, &mut col_sq);
        }
    }
    Raster { width: gray.width, height: gray.height, channels: 1, data: out }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray(w: u32, h: u32, f: impl Fn(u32, u32) -> u8) -> Raster {
        let data = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| f(x, y)).collect();
        Raster::new(w, h, 1, data).unwrap()
    }

    #[test]
    fn rotation_by_four_turns_is_identity() {
        let r = gray(5, 3, |x, y| (x * 10 + y) as u8);
        let turned = r.rotate_cw(1);
        assert_eq!((turned.width, turned.height), (3, 5));
        // The bottom-left pixel ends up top-left after a clockwise turn.
        assert_eq!(turned.data[0], r.data[(2 * 5) as usize]);
        assert_eq!(r.rotate_cw(1).rotate_cw(1).rotate_cw(1).rotate_cw(1), r);
    }

    #[test]
    fn sliding_max_matches_naive() {
        let src: Vec<u8> = (0..50).map(|i| ((i * 37) % 251) as u8).collect();
        let mut dst = vec![0; 50];
        sliding_max(&src, &mut dst, 3);
        for i in 0..50usize {
            let lo = i.saturating_sub(3);
            let hi = (i + 3).min(49);
            assert_eq!(dst[i], *src[lo..=hi].iter().max().unwrap());
        }
    }

    #[test]
    fn shading_removal_whitens_a_gradient_and_keeps_ink() {
        let r = gray(200, 200, |x, y| if (90..110).contains(&x) && (95..100).contains(&y) { 20 } else { (120 + x / 4) as u8 });
        let out = remove_shading(&r, 1.0);
        let bg = out.data[50 * 200 + 20];
        let ink = out.data[97 * 200 + 100];
        assert!(bg > 245, "background {bg}");
        assert!(ink < 60, "ink {ink}");
    }

    #[test]
    fn contrast_darkens_gray_text_despite_a_black_border() {
        // Paper 230, text strokes 130, and a black border at the left.
        let r = gray(200, 100, |x, y| if x < 10 { 0 } else if y % 10 < 2 && x % 7 < 3 { 130 } else { 230 });
        let mut out = r.clone();
        enhance_contrast(&mut out, 1.0);
        let at = |x: u32, y: u32| out.data[(y * 200 + x) as usize];
        assert!(at(14, 0) < 10, "text becomes black");
        assert_eq!(at(100, 5), 255, "paper becomes white");
    }

    #[test]
    fn contrast_does_not_blow_up_a_blank_page() {
        let r = gray(100, 100, |x, y| if (x * 7 + y * 13) % 50 == 0 { 240 } else { 250 });
        let mut out = r.clone();
        enhance_contrast(&mut out, 1.0);
        assert!(out.data.iter().all(|&v| v >= 225), "noise stays light");
    }

    #[test]
    fn sauvola_separates_text_on_uneven_paper() {
        let r = gray(300, 100, |x, y| if y % 20 < 3 && x % 7 < 3 { 30 } else { (100 + x / 3) as u8 });
        let out = binarize_sauvola(&r, 1.0);
        assert_eq!(out.data[300 + 1], 0);
        assert_eq!(out.data[10 * 300 + 250], 255);
        assert_eq!(out.data[10 * 300 + 5], 255);
    }
}
