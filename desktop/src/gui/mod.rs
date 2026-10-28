//! The application window.

mod document;
mod jobs;
mod thumbnails;

use document::Document;
use egui::{Align, Align2, Color32, CornerRadius, FontId, Id, Key, Layout, Modifiers, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use jobs::{Event, Jobs, TextChoice};
use pdf_editor::model::{ColorMode, Compression, PageKind};
use pdf_editor::pipeline::PageRoute;
use pdf_editor::save::{SaveOptions, default_file_name};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use thumbnails::Thumbnails;

const CARD_WIDTH: f32 = 150.0;
const CARD_HEIGHT: f32 = 242.0;
const THUMB_HEIGHT: f32 = 210.0;

pub struct App {
    doc: Document,
    thumbs: Thumbnails,
    jobs: Jobs,
    compression: Compression,
    /// `None` disables OCR.
    ocr_language: Option<String>,
    languages: Vec<String>,
    status: String,
    errors: Vec<String>,
    text_question: Option<(String, usize, Sender<TextChoice>)>,
    preview: Option<u64>,
    /// Last clicked page, for shift-click ranges.
    anchor: Option<u64>,
    /// Index of the page being dragged.
    dragging: Option<usize>,
    last_dir: Option<PathBuf>,
    close_after_job: bool,
    /// Save plan summary for (document revision, compression).
    plan: Option<((u64, Compression), String)>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> Self {
        let ctx = cc.egui_ctx.clone();
        ctx.all_styles_mut(enlarge_text);
        let mut languages: Vec<String> = pdf_editor::ocr::COMMON_LANGUAGES.iter().map(|(c, _)| c.to_string()).collect();
        for lang in pdf_editor::ocr::available_languages() {
            if !languages.contains(&lang) {
                languages.push(lang);
            }
        }
        let ocr_language = Some("eng".to_owned());
        let mut app = App {
            doc: Document::default(),
            thumbs: Thumbnails::new(ctx.clone()),
            jobs: Jobs::new(ctx),
            compression: Compression::High,
            ocr_language,
            languages,
            status: "Load PDFs to start.".into(),
            errors: Vec::new(),
            text_question: None,
            preview: None,
            anchor: None,
            dragging: None,
            last_dir: None,
            close_after_job: false,
            plan: None,
        };
        if !files.is_empty() {
            app.load(files);
        }
        app
    }

    fn load(&mut self, paths: Vec<PathBuf>) {
        if self.jobs.busy() || paths.is_empty() {
            return;
        }
        if let Some(dir) = paths[0].parent() {
            self.last_dir = Some(dir.to_path_buf());
        }
        self.jobs.load(paths);
    }

    fn pick_and_load(&mut self) {
        let mut dialog = rfd::FileDialog::new().add_filter("PDF", &["pdf", "PDF"]).set_title("Load PDFs");
        if let Some(dir) = &self.last_dir {
            dialog = dialog.set_directory(dir);
        }
        if let Some(paths) = dialog.pick_files() {
            self.load(paths);
        }
    }

    fn pick_and_save(&mut self) {
        if self.jobs.busy() || self.doc.pages.is_empty() {
            return;
        }
        let pages = self.doc.page_list();
        let name = default_file_name(&self.doc.sources, &pages, self.compression, self.ocr_language.is_some());
        let mut dialog = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).set_file_name(&name).set_title("Save PDF");
        if let Some(dir) = &self.last_dir {
            dialog = dialog.set_directory(dir);
        }
        let Some(path) = dialog.save_file() else { return };
        let options = SaveOptions { compression: self.compression, ocr_language: self.ocr_language.clone(), threads: 0, max_dpi: None, jpeg_quality: None };
        self.status = "Saving...".into();
        self.jobs.save(self.doc.sources.clone(), pages, options, path);
    }

    fn handle_events(&mut self) {
        while let Ok(event) = self.jobs.rx.try_recv() {
            match event {
                Event::AskText { name, count, reply } => self.text_question = Some((name, count, reply)),
                Event::Loaded { source, kinds, warning } => {
                    let name = source.name.clone();
                    let count = kinds.len();
                    self.doc.add_source(*source, kinds);
                    self.status = format!("Loaded {name}: {count} pages ({} total).", self.doc.pages.len());
                    if let Some(w) = warning {
                        self.errors.push(w);
                    }
                }
                Event::LoadFailed(e) if e == "cancelled" => self.status = "Loading cancelled.".into(),
                Event::LoadFailed(e) => {
                    self.status = "Loading failed.".into();
                    self.errors.push(e);
                }
                Event::Saved(Ok((path, report))) => {
                    let mut routes: Vec<String> = report.routes.iter().map(|(r, n)| format!("{n} {r:?}")).collect();
                    routes.sort();
                    self.status = format!(
                        "Saved {} ({:.1} MB, {} pages in {:.1} s: {}).",
                        path.display(),
                        report.bytes as f64 / 1e6,
                        report.pages,
                        report.seconds,
                        routes.join(", ")
                    );
                }
                Event::Saved(Err(e)) => {
                    self.status = "Save failed.".into();
                    if e != "cancelled" {
                        self.errors.push(format!("Save failed: {e}"));
                    } else {
                        self.status = "Save cancelled.".into();
                    }
                }
                Event::Finished => self.jobs.current = None,
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if self.text_question.is_some() || !self.errors.is_empty() {
            return;
        }
        let typing = ctx.memory(|m| m.focused().is_some());
        let pressed = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        if pressed(Modifiers::COMMAND, Key::O) {
            self.pick_and_load();
        }
        if pressed(Modifiers::COMMAND, Key::S) {
            self.pick_and_save();
        }
        if self.jobs.busy() {
            if pressed(Modifiers::NONE, Key::Escape)
                && let Some(job) = &self.jobs.current {
                    job.cancel();
                }
            return;
        }
        if typing {
            return;
        }
        if pressed(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) || pressed(Modifiers::COMMAND, Key::Y) {
            self.doc.redo();
        }
        if pressed(Modifiers::COMMAND, Key::Z) {
            self.doc.undo();
        }
        if pressed(Modifiers::COMMAND, Key::A) {
            self.doc.set_all_selected(true);
        }
        if pressed(Modifiers::NONE, Key::Escape) {
            self.doc.set_all_selected(false);
        }
        if pressed(Modifiers::NONE, Key::Delete) {
            self.doc.delete_selected();
        }
        if pressed(Modifiers::NONE, Key::R) {
            self.doc.rotate_selected();
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let busy = self.jobs.busy();
            if ui.add_enabled(!busy, egui::Button::new("📂 Load PDFs")).on_hover_text("Ctrl+O. Loaded files are appended.").clicked() {
                self.pick_and_load();
            }
            egui::ComboBox::from_id_salt("compression").selected_text(self.compression.label()).show_ui(ui, |ui| {
                for c in Compression::ALL {
                    ui.selectable_value(&mut self.compression, c, c.label());
                }
            });
            let ocr_label = self.ocr_language.as_deref().map_or("No OCR".to_owned(), language_label);
            egui::ComboBox::from_id_salt("ocr").selected_text(ocr_label).height(400.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.ocr_language, None, "No OCR");
                for lang in self.languages.clone() {
                    let label = language_label(&lang);
                    ui.selectable_value(&mut self.ocr_language, Some(lang), label);
                }
                ui.selectable_value(&mut self.ocr_language, Some(MULTI.to_owned()), language_label(MULTI));
            })
            .response
            .on_hover_text("Missing languages are downloaded (a few MB) on the first save.");
            let can_save = !busy && !self.doc.pages.is_empty();
            if ui.add_enabled(can_save, egui::Button::new("💾 Save PDF")).on_hover_text("Ctrl+S").clicked() {
                self.pick_and_save();
            }
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(job) = &self.jobs.current {
                let p = job.snapshot();
                let fraction = if p.total > 0 { p.done as f32 / p.total as f32 } else { 0.0 };
                ui.add(egui::ProgressBar::new(fraction).desired_width(260.0).text(format!("{}/{}", p.done, p.total)));
                ui.label(&p.message);
                if job.cancelling() {
                    ui.label("Cancelling…");
                } else if ui.button("Cancel").on_hover_text("Esc").clicked() {
                    job.cancel();
                }
                ui.spinner();
            } else {
                ui.label(&self.status);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.weak(self.plan_summary()).on_hover_text("How the pages will be saved with the current settings");
                });
            }
        });
    }

    /// "Save plan" line: how many pages take each route.
    fn plan_summary(&mut self) -> String {
        let key = (self.doc.revision, self.compression);
        if let Some((k, text)) = &self.plan
            && *k == key {
                return text.clone();
            }
        let settings = pdf_editor::pipeline::Settings { compression: self.compression, ocr: false, max_dpi: None, jpeg_quality: None };
        let mut counts: Vec<(PageRoute, usize)> = Vec::new();
        let mut failed = None;
        for e in &self.doc.pages {
            match pdf_editor::pipeline::intended_route(&self.doc.sources[e.page.source], &e.page, settings) {
                Ok(route) => match counts.iter_mut().find(|(r, _)| *r == route) {
                    Some((_, n)) => *n += 1,
                    None => counts.push((route, 1)),
                },
                Err(err) => failed = Some(format!("{err:#}")),
            }
        }
        let label = |r: PageRoute| match r {
            PageRoute::Copied => "text pages copied",
            PageRoute::PassThrough => "original images kept",
            PageRoute::ExactGray => "exact grayscale",
            PageRoute::Reencoded => "re-encoded",
            PageRoute::Rendered => "rendered",
        };
        counts.sort_by_key(|(r, _)| *r as u8);
        let mut text = counts.iter().map(|(r, n)| format!("{n} {}", label(*r))).collect::<Vec<_>>().join(" · ");
        if let Some(e) = failed {
            text = format!("{text} · plan error: {e}");
        }
        if !text.is_empty() {
            text = format!("Save plan: {text}");
        }
        self.plan = Some((key, text.clone()));
        text
    }

    fn operations(&mut self, ui: &mut egui::Ui) {
        ui.heading("Selected pages");
        ui.add_space(4.0);
        let enabled = !self.jobs.busy() && !self.doc.selected.is_empty();
        let wide = |ui: &mut egui::Ui, text: &str, hint: &str| {
            ui.add_enabled(enabled, egui::Button::new(text).min_size(Vec2::new(ui.available_width(), 30.0))).on_hover_text(hint).clicked()
        };
        if wide(ui, "🗑 Delete pages", "Delete") {
            self.doc.delete_selected();
        }
        if wide(ui, "⟳ Rotate 90°", "R") {
            self.doc.rotate_selected();
        }
        if wide(ui, "✂ Split double pages", "Keep the left and right halves as two pages") {
            for (old, left) in self.doc.split_selected() {
                if self.preview == Some(old) {
                    self.preview = Some(left);
                }
            }
        }
        if wide(ui, "🔆 Remove shadows", "Flatten uneven lighting and paper tint") {
            self.doc.remove_shading_selected();
        }
        if wide(ui, "🌓 Enhance contrast", "Stretch the gray range") {
            self.doc.enhance_contrast_selected();
        }
        ui.add_space(4.0);
        let current = self.doc.pages.iter().find(|e| self.doc.is_selected(e.id)).map(|e| e.page.color);
        ui.add_enabled_ui(enabled, |ui| {
            egui::ComboBox::from_id_salt("color")
                .width(ui.available_width() - 8.0)
                .selected_text(current.map_or("Color mode", ColorMode::label))
                .show_ui(ui, |ui| {
                    for mode in ColorMode::ALL {
                        if ui.selectable_label(current == Some(mode), mode.label()).clicked() {
                            self.doc.set_color_selected(mode);
                        }
                    }
                });
        });
        ui.separator();
        let idle = !self.jobs.busy();
        ui.horizontal(|ui| {
            if ui.add_enabled(idle && self.doc.can_undo(), egui::Button::new("↺ Undo")).on_hover_text("Ctrl+Z").clicked() {
                self.doc.undo();
            }
            if ui.add_enabled(idle && self.doc.can_redo(), egui::Button::new("↻ Redo")).on_hover_text("Ctrl+Shift+Z").clicked() {
                self.doc.redo();
            }
        });
        ui.separator();
        ui.small("Click to select, Shift+click for a range, drag to reorder. Ctrl+A selects all.");
        ui.small("Pages marked \"text\" keep their real text and are copied as-is unless a pixel operation is applied.");
    }

    fn preview_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Preview");
        let Some(entry) = self.preview.and_then(|id| self.doc.pages.iter().position(|e| e.id == id)) else {
            ui.label("Click a page to preview it.");
            return;
        };
        let page = self.doc.pages[entry].page.clone();
        ui.label(format!("Page #{}", entry + 1));
        let avail = ui.available_size();
        let ppp = ui.ctx().pixels_per_point();
        // Width buckets avoid re-rendering on every resize step.
        let width_px = ((avail.x * ppp / 200.0).ceil() * 200.0).clamp(200.0, 2400.0) as u32;
        let texture = self.thumbs.get(&self.doc.sources, &page, width_px).or_else(|| self.thumbs.any_for(&page));
        if let Some(t) = texture {
            let size = t.size_vec2();
            let scale = (avail.x / size.x).min(avail.y / size.y);
            ui.add(egui::Image::new(&t).fit_to_exact_size(size * scale));
        } else if let Some(e) = self.thumbs.error(&page, width_px) {
            ui.colored_label(Color32::RED, e);
        } else {
            ui.spinner();
        }
    }

    fn page_grid(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let mut all = self.doc.all_selected();
            if ui.checkbox(&mut all, "Select all").changed() {
                self.doc.set_all_selected(all);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let n = self.doc.pages.len();
                ui.label(format!("{n} page{}, {} selected", if n == 1 { "" } else { "s" }, self.doc.selected.len()));
            });
        });
        ui.separator();
        let ppp = ui.ctx().pixels_per_point();
        let thumb_px = if ppp > 1.5 { 320 } else { 240 };
        let pointer = ui.ctx().pointer_interact_pos();
        let mut rects: Vec<Rect> = Vec::with_capacity(self.doc.pages.len());
        let mut clicked: Option<(u64, bool)> = None;
        let mut drag_started: Option<usize> = None;
        let mut drag_released = false;
        let busy = self.jobs.busy();

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(10.0);
                for i in 0..self.doc.pages.len() {
                    let entry = self.doc.pages[i].clone();
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(CARD_WIDTH, CARD_HEIGHT), Sense::hover());
                    rects.push(rect);
                    let resp = ui.interact(rect, Id::new(("card", entry.id)), Sense::click_and_drag());
                    if resp.clicked() {
                        clicked = Some((entry.id, ui.input(|i| i.modifiers.shift)));
                    }
                    if resp.drag_started() && !busy {
                        drag_started = Some(i);
                    }
                    if resp.drag_stopped() {
                        drag_released = true;
                    }
                    if !ui.is_rect_visible(rect) {
                        continue;
                    }
                    let selected = self.doc.is_selected(entry.id);
                    let painter = ui.painter_at(rect.expand(2.0));
                    let visuals = ui.visuals();
                    let fill = if selected { visuals.selection.bg_fill.linear_multiply(0.35) } else { visuals.faint_bg_color };
                    let stroke = if selected {
                        Stroke::new(2.0, visuals.selection.stroke.color)
                    } else if resp.hovered() {
                        Stroke::new(1.0, visuals.widgets.hovered.fg_stroke.color)
                    } else {
                        Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color)
                    };
                    painter.rect(rect, CornerRadius::same(6), fill, stroke, StrokeKind::Inside);
                    let thumb_area = Rect::from_min_size(rect.min + Vec2::new(5.0, 5.0), Vec2::new(CARD_WIDTH - 10.0, THUMB_HEIGHT - 10.0));
                    let texture = self.thumbs.get(&self.doc.sources, &entry.page, thumb_px).or_else(|| self.thumbs.any_for(&entry.page));
                    match texture {
                        Some(t) => {
                            let size = t.size_vec2();
                            let scale = (thumb_area.width() / size.x).min(thumb_area.height() / size.y);
                            let r = Rect::from_center_size(thumb_area.center(), size * scale);
                            painter.image(t.id(), r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                        }
                        None => {
                            let text = if self.thumbs.error(&entry.page, thumb_px).is_some() { "render error" } else { "…" };
                            painter.text(thumb_area.center(), Align2::CENTER_CENTER, text, FontId::proportional(18.0), visuals.weak_text_color());
                        }
                    }
                    let row_y = rect.max.y - 15.0;
                    let text_color = visuals.text_color();
                    painter.text(Pos2::new(rect.min.x + 8.0, row_y), Align2::LEFT_CENTER, format!("#{}", i + 1), FontId::proportional(17.0), text_color);
                    let badge = match entry.page.kind {
                        PageKind::Text => Some("text"),
                        PageKind::TextStripped => Some("stripped"),
                        PageKind::Scan => None,
                    };
                    if let Some(badge) = badge {
                        let color = if entry.page.is_preserved() { Color32::from_rgb(60, 140, 90) } else { Color32::from_rgb(170, 120, 40) };
                        let pos = Pos2::new(rect.center().x, row_y);
                        let galley = painter.layout_no_wrap(badge.to_owned(), FontId::proportional(14.0), Color32::WHITE);
                        let r = Rect::from_center_size(pos, galley.size() + Vec2::new(8.0, 2.0));
                        painter.rect_filled(r, CornerRadius::same(4), color);
                        painter.galley(r.min + Vec2::new(4.0, 1.0), galley, Color32::WHITE);
                    }
                    let check = Rect::from_center_size(Pos2::new(rect.max.x - 16.0, row_y), Vec2::splat(17.0));
                    painter.rect_stroke(check, CornerRadius::same(3), Stroke::new(1.5, text_color), StrokeKind::Inside);
                    if selected {
                        painter.text(check.center(), Align2::CENTER_CENTER, "✔", FontId::proportional(15.0), text_color);
                    }
                }
            });

            // Drag to reorder: show where the page would go.
            if let (Some(from), Some(pos)) = (self.dragging, pointer) {
                let target = drop_target(&rects, pos);
                if let Some(t) = target {
                    let x = if t < rects.len() { rects[t].min.x - 5.0 } else { rects[rects.len() - 1].max.x + 5.0 };
                    let r = rects[t.min(rects.len() - 1)];
                    ui.painter().line_segment([Pos2::new(x, r.min.y), Pos2::new(x, r.max.y)], Stroke::new(3.0, ui.visuals().selection.stroke.color));
                }
                // Scroll when dragging near the edges.
                let clip = ui.clip_rect();
                if pos.y < clip.min.y + 40.0 {
                    ui.scroll_with_delta(Vec2::new(0.0, 12.0));
                } else if pos.y > clip.max.y - 40.0 {
                    ui.scroll_with_delta(Vec2::new(0.0, -12.0));
                }
                if drag_released
                    && let Some(t) = target {
                        self.doc.move_page(from, t);
                    }
                ui.ctx().request_repaint();
            }
        });

        if let Some(i) = drag_started {
            self.dragging = Some(i);
        }
        if drag_released {
            self.dragging = None;
        }
        if let Some((id, shift)) = clicked {
            match (shift, self.anchor) {
                (true, Some(anchor)) => self.doc.select_range(anchor, id),
                _ => self.doc.toggle(id),
            }
            self.anchor = Some(id);
            self.preview = Some(id);
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some((name, count, reply)) = &self.text_question {
            let mut answer = None;
            egui::Modal::new(Id::new("text-question")).show(ctx, |ui| {
                ui.set_max_width(600.0);
                ui.heading("This PDF contains text");
                ui.label(format!("{name} has {count} page(s) with real (typeset) text."));
                ui.add_space(8.0);
                let choice = |ui: &mut egui::Ui, title: &str, detail: &str| {
                    let r = ui.add(egui::Button::new(egui::RichText::new(title).strong()).min_size(Vec2::new(580.0, 34.0)));
                    ui.small(detail);
                    ui.add_space(6.0);
                    r.clicked()
                };
                if choice(ui, "Keep the text", "Pages are saved as-is: text, fonts and quality preserved; embedded images still compressed. Best for born-digital PDFs.") {
                    answer = Some(TextChoice::Keep);
                }
                if choice(ui, "Strip the text", "All text is permanently removed; images and graphics keep their original quality (no re-OCR). Best for deleting a junk OCR layer.") {
                    answer = Some(TextChoice::Strip);
                }
                if choice(ui, "Rasterize", "Pages are converted to images and treated like scans: cleanup, recompression, and fresh OCR if enabled.") {
                    answer = Some(TextChoice::Rasterize);
                }
                if ui.button("Cancel loading").clicked() {
                    if let Some(job) = &self.jobs.current {
                        job.cancel();
                    }
                    answer = Some(TextChoice::Keep);
                }
            });
            if let Some(a) = answer {
                let _ = reply.send(a);
                self.text_question = None;
            }
        }
        if let Some(error) = self.errors.first().cloned() {
            let mut dismissed = false;
            egui::Modal::new(Id::new("error")).show(ctx, |ui| {
                ui.set_max_width(680.0);
                ui.heading("Error");
                ui.label(&error);
                ui.add_space(8.0);
                if ui.button("OK").clicked() {
                    dismissed = true;
                }
            });
            if dismissed {
                self.errors.remove(0);
            }
        }
    }
}

/// The web app's multi-language choice.
/// Text sizes 1.4 times the egui defaults, with padding to match.
fn enlarge_text(style: &mut egui::Style) {
    use egui::TextStyle;
    for (text_style, size) in [(TextStyle::Small, 14.0), (TextStyle::Body, 17.5), (TextStyle::Button, 17.5), (TextStyle::Heading, 24.0), (TextStyle::Monospace, 16.0)] {
        if let Some(font) = style.text_styles.get_mut(&text_style) {
            font.size = size;
        }
    }
    style.spacing.button_padding = Vec2::new(8.0, 4.0);
    style.spacing.interact_size.y = 26.0;
}

const MULTI: &str = "eng+fra+deu+spa+ita+por";

fn language_label(code: &str) -> String {
    let name = if code == MULTI {
        "Multi (EN/FR/DE/ES/IT/PT)".to_owned()
    } else {
        pdf_editor::ocr::COMMON_LANGUAGES.iter().find(|(c, _)| *c == code).map_or(code.to_owned(), |(_, n)| n.to_string())
    };
    let missing = code.split('+').any(|l| !pdf_editor::ocr::is_installed(l));
    if missing { format!("{name} (download)") } else { name }
}

/// Insertion index for a drop at `pos`: before the card under the pointer,
/// or after it when the pointer is on its right half.
fn drop_target(rects: &[Rect], pos: Pos2) -> Option<usize> {
    let row = rects.iter().enumerate().filter(|(_, r)| pos.y >= r.min.y - 5.0 && pos.y <= r.max.y + 5.0).collect::<Vec<_>>();
    let (i, r) = row.iter().min_by(|a, b| (a.1.center().x - pos.x).abs().total_cmp(&(b.1.center().x - pos.x).abs()))?;
    Some(if pos.x < r.center().x { *i } else { i + 1 })
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.thumbs.poll();
        self.handle_events();
        self.shortcuts(&ctx);

        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if !dropped.is_empty() {
            self.load(dropped);
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.jobs.busy() {
            // Let a save finish cleanly (or cancel it) before closing.
            if let Some(job) = &self.jobs.current {
                job.cancel();
            }
            self.close_after_job = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if self.close_after_job && !self.jobs.busy() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui);
            ui.add_space(2.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.add_space(2.0);
            self.status_bar(ui);
        });
        egui::Panel::left("operations").resizable(false).default_size(250.0).show(ui, |ui| {
            ui.add_space(6.0);
            self.operations(ui);
        });
        egui::Panel::right("preview").resizable(true).default_size(480.0).min_size(360.0).show(ui, |ui| {
            ui.add_space(6.0);
            self.preview_panel(ui);
        });
        egui::CentralPanel::default().show(ui, |ui| self.page_grid(ui));
        self.dialogs(&ctx);
        if self.jobs.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}
