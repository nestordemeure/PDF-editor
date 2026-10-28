//! Background thumbnail and preview rendering. The UI thread asks for an
//! image and gets it some frames later; it never waits for MuPDF.

use egui::{ColorImage, Context, TextureFilter, TextureHandle, TextureOptions};
use pdf_editor::model::{Compression, Page, PageKind, PixelOps, SourceId};
use pdf_editor::pipeline::Settings;
use pdf_editor::pixel::Raster;
use pdf_editor::source::Source;
use pdf_editor::{assemble, pipeline, render};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

/// Everything that changes how a page looks, at one width.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ImageKey {
    source: SourceId,
    index: usize,
    kind: PageKind,
    /// View crop and turns, as exact bits.
    view: [u64; 5],
    ops: PixelOps,
    width: u32,
    /// Set for full-scale previews: the page as a save at this level writes it.
    full_scale: Option<Compression>,
}

impl ImageKey {
    pub fn new(page: &Page, width: u32) -> Self {
        let c = page.view.crop;
        ImageKey {
            source: page.source,
            index: page.index,
            kind: page.kind,
            view: [c.x0.to_bits(), c.y0.to_bits(), c.x1.to_bits(), c.y1.to_bits(), page.view.quarter_turns as u64],
            ops: page.pixel_ops(),
            width,
            full_scale: None,
        }
    }

    fn full_scale(page: &Page, compression: Compression) -> Self {
        ImageKey { full_scale: Some(compression), ..ImageKey::new(page, 0) }
    }

    fn is_large(&self) -> bool {
        self.width > LARGE || self.full_scale.is_some()
    }
}

type BaseKey = (SourceId, usize, u32);

/// Base renders (source page, no edits) shared with workers.
type BaseCache = Arc<Mutex<HashMap<BaseKey, Arc<Raster>>>>;

pub struct Thumbnails {
    pool: rayon::ThreadPool,
    ctx: Context,
    tx: Sender<(ImageKey, Result<ColorImage, String>)>,
    rx: Receiver<(ImageKey, Result<ColorImage, String>)>,
    bases: BaseCache,
    /// Textures and the frame they were last used in.
    textures: HashMap<ImageKey, (TextureHandle, u64)>,
    frame: u64,
    pending: HashSet<ImageKey>,
    errors: HashMap<ImageKey, String>,
    /// Large images, oldest first, evicted beyond a few.
    large: VecDeque<ImageKey>,
}

/// Width (in pixels) under which images count as thumbnails and stay cached.
const LARGE: u32 = 400;
const KEPT_LARGE: usize = 6;
/// Textures kept before the least recently used ones go (about 500 MB).
const KEPT_TEXTURES: usize = 1500;
/// Resolution of full-scale previews.
pub const FULL_SCALE_DPI: f64 = 300.0;
const MAX_FULL_SCALE_PIXELS: f64 = 20e6;
/// Longest texture side that all graphics drivers accept.
const MAX_TEXTURE_SIDE: f64 = 8192.0;

impl Thumbnails {
    pub fn new(ctx: Context) -> Self {
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get()).saturating_sub(2).max(1);
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).thread_name(|i| format!("thumbnail-{i}")).build().unwrap();
        let (tx, rx) = channel();
        Thumbnails {
            pool,
            ctx,
            tx,
            rx,
            bases: Default::default(),
            textures: HashMap::new(),
            frame: 0,
            pending: HashSet::new(),
            errors: HashMap::new(),
            large: VecDeque::new(),
        }
    }

    /// Collects finished renders; call once per frame.
    pub fn poll(&mut self) {
        self.frame += 1;
        if self.textures.len() > KEPT_TEXTURES {
            let mut ages: Vec<u64> = self.textures.values().map(|(_, used)| *used).collect();
            ages.sort_unstable();
            let cutoff = ages[self.textures.len() - KEPT_TEXTURES];
            self.textures.retain(|_, (_, used)| *used >= cutoff);
        }
        while let Ok((key, result)) = self.rx.try_recv() {
            self.pending.remove(&key);
            match result {
                Ok(image) => {
                    // Enlarged full-scale pixels stay sharp.
                    let options = match key.full_scale {
                        Some(_) => TextureOptions { magnification: TextureFilter::Nearest, ..TextureOptions::LINEAR },
                        None => TextureOptions::LINEAR,
                    };
                    let texture = self.ctx.load_texture(format!("{key:?}"), image, options);
                    if key.is_large() {
                        self.large.push_back(key.clone());
                        while self.large.len() > KEPT_LARGE {
                            if let Some(old) = self.large.pop_front() {
                                self.textures.remove(&old);
                            }
                        }
                    }
                    self.textures.insert(key, (texture, self.frame));
                }
                Err(e) => {
                    self.errors.insert(key, e);
                }
            }
        }
    }

    pub fn error(&self, page: &Page, width: u32) -> Option<&String> {
        self.errors.get(&ImageKey::new(page, width))
    }

    /// The page's image at `width` pixels, if ready; requests it otherwise.
    pub fn get(&mut self, sources: &[Arc<Source>], page: &Page, width: u32) -> Option<TextureHandle> {
        let source = sources[page.source].clone();
        let (page, bases) = (page.clone(), self.bases.clone());
        self.get_or_request(ImageKey::new(&page, width), move || make_image(&source, &page, width, &bases))
    }

    /// The page as a save at `compression` writes it, rendered at
    /// `FULL_SCALE_DPI`, if ready; requests it otherwise.
    pub fn get_full_scale(&mut self, sources: &[Arc<Source>], page: &Page, compression: Compression) -> Option<TextureHandle> {
        let (sources, page) = (sources.to_vec(), page.clone());
        self.get_or_request(ImageKey::full_scale(&page, compression), move || make_full_scale_image(sources, &page, compression))
    }

    pub fn full_scale_error(&self, page: &Page, compression: Compression) -> Option<&String> {
        self.errors.get(&ImageKey::full_scale(page, compression))
    }

    fn get_or_request(&mut self, key: ImageKey, make: impl FnOnce() -> anyhow::Result<ColorImage> + Send + 'static) -> Option<TextureHandle> {
        if let Some((t, used)) = self.textures.get_mut(&key) {
            *used = self.frame;
            return Some(t.clone());
        }
        if self.pending.contains(&key) || self.errors.contains_key(&key) {
            return None;
        }
        self.pending.insert(key.clone());
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.pool.spawn(move || {
            let result = make().map_err(|e| format!("{e:#}"));
            let _ = tx.send((key, result));
            ctx.request_repaint();
        });
        None
    }

    /// Any image of the page already made, for display while the exact one renders.
    pub fn any_for(&self, page: &Page) -> Option<TextureHandle> {
        self.textures.iter().find(|(k, _)| k.source == page.source && k.index == page.index && k.full_scale.is_none()).map(|(_, (t, _))| t.clone())
    }
}

impl Drop for Thumbnails {
    fn drop(&mut self) {
        // MuPDF documents must close before their thread's context goes.
        self.pool.broadcast(|_| render::close_documents());
    }
}

fn make_image(source: &Source, page: &Page, width: u32, bases: &BaseCache) -> anyhow::Result<ColorImage> {
    let sp = &source.pages[page.index];
    // Render the source page so the visible part fits a width x 1.4 width box.
    let (out_w, out_h) = page.view.output_size(sp.display_size);
    let ppp = (width as f64 / out_w.max(1.0)).min(1.4 * width as f64 / out_h.max(1.0));
    let base_width = (ppp * sp.display_size.0).round().max(1.0) as u32;
    let key = (page.source, page.index, base_width);
    let cached = bases.lock().unwrap().get(&key).cloned();
    let base = match cached {
        Some(b) => b,
        None => {
            let ppp = base_width as f64 / sp.display_size.0;
            let b = Arc::new(render::render_page(&source.render_path, page.index, ppp, sp.display_size)?);
            if width <= LARGE {
                let mut bases = bases.lock().unwrap();
                if bases.len() >= KEPT_TEXTURES {
                    bases.clear();
                }
                bases.insert(key, b.clone());
            }
            b
        }
    };
    let image = pipeline::preview(&base, page, sp.display_size, sp.ppi_factor).to_rgb();
    Ok(ColorImage::from_rgb([image.width as usize, image.height as usize], &image.data))
}

fn make_full_scale_image(sources: Vec<Arc<Source>>, page: &Page, compression: Compression) -> anyhow::Result<ColorImage> {
    let source = &sources[page.source];
    let settings = Settings { compression, ocr: false, max_dpi: None, jpeg_quality: None };
    let output = if page.is_preserved() { None } else { Some(pipeline::process_page(source, page, settings, None)?) };
    let (w, h) = page.view.output_size(source.pages[page.index].display_size);
    let ppp = (FULL_SCALE_DPI / 72.0).min((MAX_FULL_SCALE_PIXELS / (w * h).max(1.0)).sqrt()).min(MAX_TEXTURE_SIDE / w.max(h).max(1.0));
    let pdf = assemble::single_page_pdf(sources, page.clone(), compression, output)?;
    let image = render::render_first_page(&pdf, ppp)?;
    Ok(ColorImage::from_rgb([image.width as usize, image.height as usize], &image.data))
}
