//! Writes a page's content before and after text cropping (left half).
use pdf_editor::model::Side;
fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let source = pdf_editor::source::Source::open(a[1].as_ref())?;
    let index: usize = a[2].parse::<usize>()? - 1;
    let sp = &source.pages[index];
    let view = pdf_editor::model::View::default().split(Side::Left);
    let crop = view.crop_in_display(sp.display_size).transform(sp.user_to_display.invert().unwrap());
    let resources = match pdf_editor::source::inherited(&source.doc, sp.id, b"Resources") {
        Some(lopdf::Object::Dictionary(d)) => d,
        _ => lopdf::Dictionary::new(),
    };
    let content = pdf_editor::content::page_content_bytes(&source.doc, sp.id)?;
    let cropped = pdf_editor::textcrop::drop_text_outside(&source.doc, &resources, &content, crop)?.unwrap();
    let dir = std::path::Path::new(&a[3]);
    std::fs::write(dir.join("original.txt"), &content)?;
    std::fs::write(dir.join("cropped.txt"), &cropped.content)?;
    println!("removed {}", cropped.removed);
    Ok(())
}
