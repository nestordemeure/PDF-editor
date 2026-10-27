//! Prints the OCR lines of one page. Usage: ocr_lines file.pdf page
fn main() -> anyhow::Result<()> {
    pdf_editor::init_process();
    let args: Vec<String> = std::env::args().collect();
    let source = pdf_editor::source::Source::open(args[1].as_ref())?;
    let index: usize = args[2].parse::<usize>()? - 1;
    let sp = &source.pages[index];
    let raster = pdf_editor::render::render_page(&source.render_path, index, 300.0 / 72.0, sp.display_size)?.to_gray();
    let mut engine = pdf_editor::ocr::Engine::new("eng")?;
    for line in engine.recognize(&raster, 300)? {
        let text: Vec<&str> = line.words.iter().map(|w| w.text.as_str()).collect();
        println!("{:?} {}", line.bbox, text.join(" "));
    }
    Ok(())
}
