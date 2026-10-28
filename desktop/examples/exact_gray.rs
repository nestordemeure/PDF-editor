//! Reports exact-grayscale sizes per scan page.
fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).unwrap();
    let source = pdf_editor::source::Source::open(path.as_ref())?;
    for (i, p) in source.pages.iter().enumerate() {
        let Some((id, _)) = p.scan_image() else { continue };
        let Some(info) = pdf_editor::pdfimage::inspect(&source.doc, id)? else { continue };
        let image = pdf_editor::pdfimage::extract(&source.doc, id, &info)?;
        let jpeg = image.jpeg_data()?;
        match pdf_editor::jpeggray::luma_only(&jpeg) {
            Ok(g) => {
                println!("page {}: original {} gray {}", i + 1, jpeg.len(), g.len());
            }
            Err(e) => println!("page {}: {e}", i + 1),
        }
    }
    Ok(())
}
