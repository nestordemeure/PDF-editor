//! Prints original and losslessly repacked sizes of each scan JPEG.
fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).unwrap();
    let source = pdf_editor::source::Source::open(path.as_ref())?;
    for (i, p) in source.pages.iter().enumerate() {
        let Some((id, _)) = p.scan_image() else { continue };
        let data = &pdf_editor::pdfimage::image_stream(&source.doc, id)?.content;
        let t = std::time::Instant::now();
        let packed = pdf_editor::encode::repack_jpeg(data);
        println!("page {}: {} -> {:?} in {:?}", i + 1, data.len(), packed.map(|p| p.len()), t.elapsed());
    }
    Ok(())
}
