//! Long operations (loading, saving) on background threads, reporting to
//! the UI through channels.

use egui::Context;
use pdf_editor::model::{Page, PageKind};
use pdf_editor::save::{SaveOptions, SaveReport, save};
use pdf_editor::source::Source;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

/// What to do with pages that have real text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextChoice {
    Keep,
    Strip,
    Rasterize,
}

pub enum Event {
    /// The loader asks what to do with a file's text pages.
    AskText { name: String, count: usize, reply: Sender<TextChoice> },
    Loaded { source: Box<Source>, kinds: Vec<PageKind>, warning: Option<String> },
    LoadFailed(String),
    Saved(Result<(PathBuf, SaveReport), String>),
    /// The job ended (successfully or not).
    Finished,
}

#[derive(Clone, Default)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub message: String,
}

/// A running job, shared with the thread doing it.
pub struct Job {
    pub progress: Arc<Mutex<Progress>>,
    pub cancel: Arc<AtomicBool>,
    pub cancellable: bool,
}

impl Job {
    fn new(cancellable: bool) -> Job {
        Job { progress: Default::default(), cancel: Default::default(), cancellable }
    }

    pub fn snapshot(&self) -> Progress {
        self.progress.lock().unwrap().clone()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub struct Jobs {
    ctx: Context,
    tx: Sender<Event>,
    pub rx: Receiver<Event>,
    pub current: Option<Job>,
}

impl Jobs {
    pub fn new(ctx: Context) -> Self {
        let (tx, rx) = channel();
        Jobs { ctx, tx, rx, current: None }
    }

    pub fn busy(&self) -> bool {
        self.current.is_some()
    }

    pub fn load(&mut self, paths: Vec<PathBuf>) {
        let job = Job::new(false);
        let progress = job.progress.clone();
        self.current = Some(job);
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let total = paths.len();
            for (i, path) in paths.into_iter().enumerate() {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                *progress.lock().unwrap() = Progress { done: i, total, message: format!("Loading {name}") };
                ctx.request_repaint();
                match load_one(&path, &tx, &ctx) {
                    Ok(event) => {
                        let _ = tx.send(event);
                    }
                    Err(e) => {
                        let _ = tx.send(Event::LoadFailed(format!("{}: {e:#}", path.display())));
                    }
                }
                ctx.request_repaint();
            }
            let _ = tx.send(Event::Finished);
            ctx.request_repaint();
        });
    }

    pub fn save(&mut self, sources: Vec<Arc<Source>>, pages: Vec<Page>, options: SaveOptions, path: PathBuf) {
        let job = Job::new(true);
        let (progress, cancel) = (job.progress.clone(), job.cancel.clone());
        self.current = Some(job);
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let report = {
                let ctx = ctx.clone();
                let on_progress = move |done: usize, total: usize, message: &str| {
                    *progress.lock().unwrap() = Progress { done, total, message: message.to_owned() };
                    ctx.request_repaint();
                };
                save(&sources, &pages, &options, &path, &on_progress, &cancel)
            };
            let _ = tx.send(Event::Saved(report.map(|r| (path, r)).map_err(|e| format!("{e:#}"))));
            let _ = tx.send(Event::Finished);
            ctx.request_repaint();
        });
    }
}

fn load_one(path: &std::path::Path, tx: &Sender<Event>, ctx: &Context) -> anyhow::Result<Event> {
    let mut source = Source::open(path)?;
    let text_pages: Vec<usize> = (0..source.pages.len()).filter(|&i| source.pages[i].is_text_page).collect();
    let mut choice = TextChoice::Keep;
    if !text_pages.is_empty() {
        let (reply, answer) = channel();
        let _ = tx.send(Event::AskText { name: source.name.clone(), count: text_pages.len(), reply });
        ctx.request_repaint();
        choice = answer.recv().unwrap_or(TextChoice::Keep);
    }
    let mut warning = None;
    if choice == TextChoice::Strip {
        let skipped = source.strip_text(&text_pages)?;
        if skipped > 0 {
            warning = Some(format!(
                "{}: {skipped} content stream(s) contain inline images and were left untouched; some text may remain.",
                source.name
            ));
        }
    }
    let kinds = source
        .pages
        .iter()
        .map(|p| match (p.is_text_page, choice) {
            (false, _) | (true, TextChoice::Rasterize) => PageKind::Scan,
            (true, TextChoice::Keep) => PageKind::Text,
            (true, TextChoice::Strip) => PageKind::TextStripped,
        })
        .collect();
    Ok(Event::Loaded { source: Box::new(source), kinds, warning })
}
