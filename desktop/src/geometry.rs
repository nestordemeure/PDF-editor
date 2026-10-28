//! Affine matrices and rectangles in PDF conventions.
//!
//! A matrix `[a b c d e f]` maps `(x, y)` to `(a x + c y + e, b x + d y + f)`.
//! `m.then(n)` applies `m` first, then `n` (the PDF product `m × n`).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Matrix {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Matrix {
    pub const IDENTITY: Matrix = Matrix { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    pub fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Matrix { a, b, c, d, e, f }
    }

    pub fn from_slice(v: &[f64]) -> Option<Self> {
        match v {
            [a, b, c, d, e, f] => Some(Matrix::new(*a, *b, *c, *d, *e, *f)),
            _ => None,
        }
    }

    pub fn translate(tx: f64, ty: f64) -> Self {
        Matrix::new(1.0, 0.0, 0.0, 1.0, tx, ty)
    }

    pub fn scale(sx: f64, sy: f64) -> Self {
        Matrix::new(sx, 0.0, 0.0, sy, 0.0, 0.0)
    }

    pub fn then(self, n: Matrix) -> Matrix {
        Matrix {
            a: self.a * n.a + self.b * n.c,
            b: self.a * n.b + self.b * n.d,
            c: self.c * n.a + self.d * n.c,
            d: self.c * n.b + self.d * n.d,
            e: self.e * n.a + self.f * n.c + n.e,
            f: self.e * n.b + self.f * n.d + n.f,
        }
    }

    pub fn apply(self, x: f64, y: f64) -> (f64, f64) {
        (self.a * x + self.c * y + self.e, self.b * x + self.d * y + self.f)
    }

    pub fn det(self) -> f64 {
        self.a * self.d - self.b * self.c
    }

    pub fn invert(self) -> Option<Matrix> {
        let det = self.det();
        if det.abs() < 1e-12 {
            return None;
        }
        let a = self.d / det;
        let b = -self.b / det;
        let c = -self.c / det;
        let d = self.a / det;
        Some(Matrix { a, b, c, d, e: -(self.e * a + self.f * c), f: -(self.e * b + self.f * d) })
    }

    /// True when the matrix maps axis-aligned rectangles to axis-aligned
    /// rectangles (scales, flips and quarter turns only).
    pub fn is_axis_aligned(self) -> bool {
        let scale = self.a.abs().max(self.b.abs()).max(self.c.abs()).max(self.d.abs());
        let eps = scale * 1e-6;
        (self.b.abs() <= eps && self.c.abs() <= eps) || (self.a.abs() <= eps && self.d.abs() <= eps)
    }

    pub fn to_array(self) -> [f64; 6] {
        [self.a, self.b, self.c, self.d, self.e, self.f]
    }

    /// Bounding box of the unit square under this matrix.
    pub fn unit_bbox(self) -> Rect {
        Rect::new(0.0, 0.0, 1.0, 1.0).transform(self)
    }
}

/// Clockwise quarter turns of a `width × height` box with its lower-left
/// corner at the origin, mapped back to the positive quadrant (y up).
pub fn rotate_cw_quarters(width: f64, height: f64, quarter_turns: u32) -> Matrix {
    let mut m = Matrix::IDENTITY;
    let (mut w, mut h) = (width, height);
    for _ in 0..quarter_turns % 4 {
        // (x, y) -> (y, w - x): a clockwise turn in y-up coordinates.
        m = m.then(Matrix::new(0.0, -1.0, 1.0, 0.0, 0.0, w));
        std::mem::swap(&mut w, &mut h);
    }
    m
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Rect { x0: x0.min(x1), y0: y0.min(y1), x1: x0.max(x1), y1: y0.max(y1) }
    }

    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f64 {
        self.y1 - self.y0
    }

    pub fn area(&self) -> f64 {
        self.width().max(0.0) * self.height().max(0.0)
    }

    pub fn transform(&self, m: Matrix) -> Rect {
        let corners = [
            m.apply(self.x0, self.y0),
            m.apply(self.x1, self.y0),
            m.apply(self.x0, self.y1),
            m.apply(self.x1, self.y1),
        ];
        let xs = corners.map(|p| p.0);
        let ys = corners.map(|p| p.1);
        Rect {
            x0: xs.iter().cloned().fold(f64::INFINITY, f64::min),
            y0: ys.iter().cloned().fold(f64::INFINITY, f64::min),
            x1: xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
            y1: ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        }
    }

    pub fn intersect(&self, o: &Rect) -> Rect {
        let x0 = self.x0.max(o.x0);
        let y0 = self.y0.max(o.y0);
        Rect { x0, y0, x1: self.x1.min(o.x1).max(x0), y1: self.y1.min(o.y1).max(y0) }
    }

    pub fn contains(&self, o: &Rect, tolerance: f64) -> bool {
        o.x0 >= self.x0 - tolerance
            && o.y0 >= self.y0 - tolerance
            && o.x1 <= self.x1 + tolerance
            && o.y1 <= self.y1 + tolerance
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
    }

    #[test]
    fn then_applies_left_first() {
        let m = Matrix::translate(1.0, 0.0).then(Matrix::scale(2.0, 2.0));
        assert!(close(m.apply(0.0, 0.0), (2.0, 0.0)));
    }

    #[test]
    fn invert_round_trips() {
        let m = Matrix::new(0.0, -2.0, 3.0, 0.0, 5.0, 7.0);
        let r = m.then(m.invert().unwrap());
        assert!(close(r.apply(3.0, 4.0), (3.0, 4.0)));
    }

    #[test]
    fn quarter_turn_maps_box_into_positive_quadrant() {
        // A 10x20 box turned clockwise becomes 20x10: its top-left corner
        // (0, 20) becomes the top-right corner (20, 10).
        let m = rotate_cw_quarters(10.0, 20.0, 1);
        assert!(close(m.apply(0.0, 20.0), (20.0, 10.0)));
        assert!(close(m.apply(10.0, 0.0), (0.0, 0.0)));
        let full = rotate_cw_quarters(10.0, 20.0, 4);
        assert!(close(full.apply(3.0, 4.0), (3.0, 4.0)));
    }
}
