//! Background thumbnail and preview rendering. The UI thread asks for an
//! image and gets it some frames later; it never waits for MuPDF.

use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use pdf_editor::model::{Page, PageKind, PixelOps, SourceId};
use pdf_editor::pixel::Raster;
use pdf_editor::source::Source;
use pdf_editor::{pipeline, render};
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
        }
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
    textures: HashMap<ImageKey, TextureHandle>,
    pending: HashSet<ImageKey>,
    errors: HashMap<ImageKey, String>,
    /// Large images, oldest first, evicted beyond a few.
    large: VecDeque<ImageKey>,
}

/// Width (in pixels) under which images count as thumbnails and stay cached.
const LARGE: u32 = 400;
const KEPT_LARGE: usize = 6;

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
            pending: HashSet::new(),
            errors: HashMap::new(),
            large: VecDeque::new(),
        }
    }

    /// Collects finished renders; call once per frame.
    pub fn poll(&mut self) {
        while let Ok((key, result)) = self.rx.try_recv() {
            self.pending.remove(&key);
            match result {
                Ok(image) => {
                    let texture = self.ctx.load_texture(format!("{key:?}"), image, TextureOptions::LINEAR);
                    if key.width > LARGE {
                        self.large.push_back(key.clone());
                        while self.large.len() > KEPT_LARGE {
                            if let Some(old) = self.large.pop_front() {
                                self.textures.remove(&old);
                            }
                        }
                    }
                    self.textures.insert(key, texture);
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
        let key = ImageKey::new(page, width);
        if let Some(t) = self.textures.get(&key) {
            return Some(t.clone());
        }
        if self.pending.contains(&key) || self.errors.contains_key(&key) {
            return None;
        }
        self.pending.insert(key.clone());
        let source = sources[page.source].clone();
        let page = page.clone();
        let (tx, ctx, bases) = (self.tx.clone(), self.ctx.clone(), self.bases.clone());
        self.pool.spawn(move || {
            let result = make_image(&source, &page, width, &bases).map_err(|e| format!("{e:#}"));
            let _ = tx.send((key, result));
            ctx.request_repaint();
        });
        None
    }

    /// Any image of the page already made, for display while the exact one renders.
    pub fn any_for(&self, page: &Page) -> Option<TextureHandle> {
        self.textures.iter().find(|(k, _)| k.source == page.source && k.index == page.index).map(|(_, t)| t.clone())
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
                bases.lock().unwrap().insert(key, b.clone());
            }
            b
        }
    };
    let image = pipeline::preview(&base, page, sp.display_size).to_rgb();
    Ok(ColorImage::from_rgb([image.width as usize, image.height as usize], &image.data))
}
