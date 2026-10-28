//! Desktop app: PDF cleanup for scanned books.

mod gui;

fn main() -> eframe::Result {
    pdf_editor::init_process();
    let files: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    // 128 px: a 256 px icon exceeds the X11 request size and the window gets none.
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-128.png")).expect("bundled icon is a valid PNG");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_icon(icon)
            .with_title("PDF Editor")
            .with_app_id("pdf-editor")
            .with_inner_size([1400.0, 900.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("PDF Editor", options, Box::new(|cc| Ok(Box::new(gui::App::new(cc, files)))))
}
