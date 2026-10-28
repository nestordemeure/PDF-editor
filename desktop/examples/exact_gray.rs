//! Reports exact-grayscale sizes per scan page.
fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).unwrap();
    let source = pdf_editor::source::Source::open(path.as_ref())?;
    for (i, p) in source.pages.iter().enumerate() {
        let Some((id, _)) = p.scan_image() else { continue };
        let Some(info) = pdf_editor::pdfimage::inspect(&source.doc, id)? else { continue };
        let jpeg = pdf_editor::pdfimage::jpeg_data(pdf_editor::pdfimage::image_stream(&source.doc, id)?, &info)?;
        match pdf_editor::jpeggray::luma_only(&jpeg) {
            Ok(g) => {
                println!("page {}: original {} gray {}", i + 1, jpeg.len(), g.len());
            }
            Err(e) => println!("page {}: {e}", i + 1),
        }
    }
    Ok(())
}
