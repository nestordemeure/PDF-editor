//! Page rendering with MuPDF. MuPDF objects are not `Send`, so each thread
//! keeps its own open documents.

use crate::pixel::Raster;
use anyhow::{Context, Result, bail};
use mupdf::{Colorspace, Document, Matrix};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

thread_local! {
    static DOCUMENTS: RefCell<HashMap<PathBuf, Document>> = RefCell::new(HashMap::new());
}

/// Renders a page (as displayed: crop box and `/Rotate` applied) to RGB at
/// `pixels_per_point`. `expected_size` is the display size the caller
/// computed, checked against MuPDF's.
pub fn render_page(path: &Path, index: usize, pixels_per_point: f64, expected_size: (f64, f64)) -> Result<Raster> {
    DOCUMENTS.with(|cell| {
        let mut docs = cell.borrow_mut();
        if !docs.contains_key(path) {
            let doc = Document::open(path.to_str().context("non UTF-8 path")?)
                .with_context(|| format!("MuPDF cannot open {}", path.display()))?;
            docs.insert(path.to_path_buf(), doc);
        }
        let doc = &docs[path];
        let page = doc.load_page(index as i32).with_context(|| format!("MuPDF cannot load page {}", index + 1))?;
        let bounds = page.bounds()?;
        let size = ((bounds.x1 - bounds.x0) as f64, (bounds.y1 - bounds.y0) as f64);
        if (size.0 - expected_size.0).abs() > 1.0 || (size.1 - expected_size.1).abs() > 1.0 {
            bail!("page {} is {size:?} in MuPDF but {expected_size:?} in the object model", index + 1);
        }
        let s = pixels_per_point as f32;
        let ctm = Matrix::new(s, 0.0, 0.0, s, -bounds.x0 * s, -bounds.y0 * s);
        let pixmap = page.to_pixmap(&ctm, &Colorspace::device_rgb(), false, true)?;
        let (w, h, n) = (pixmap.width(), pixmap.height(), pixmap.n() as usize);
        if n != 3 {
            bail!("MuPDF returned {n} channels");
        }
        let stride = pixmap.stride() as usize;
        let samples = pixmap.samples();
        let row = w as usize * 3;
        let mut data = Vec::with_capacity(row * h as usize);
        for y in 0..h as usize {
            data.extend_from_slice(&samples[y * stride..y * stride + row]);
        }
        Raster::new(w, h, 3, data)
    })
}

/// Closes this thread's MuPDF documents (call when sources change).
pub fn close_documents() {
    DOCUMENTS.with(|cell| cell.borrow_mut().clear());
}
