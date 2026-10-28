//! End-to-end tests on synthetic PDFs: load, classify, save, and inspect
//! the output with lopdf.

use lopdf::{Document, Object, Stream, dictionary};
use pdf_editor::model::{ColorMode, Compression, Page, PageKind, Side};
use pdf_editor::pipeline::PageRoute;
use pdf_editor::pixel::Raster;
use pdf_editor::save::{SaveOptions, SaveReport, save};
use pdf_editor::source::Source;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// A gray page pattern: dark text-like bars on light, slightly uneven paper.
fn scan_raster(w: u32, h: u32) -> Raster {
    let data = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .flat_map(|(x, y)| {
            let ink = (y / 12) % 3 == 0 && (x / 9) % 5 != 0 && x > w / 10 && x < w * 9 / 10;
            let paper = 225 - (x * 20 / w) as u8;
            let v = if ink { 30 } else { paper };
            [v, v.saturating_sub(4), v.saturating_sub(10)]
        })
        .collect();
    Raster::new(w, h, 3, data).unwrap()
}

fn jpeg(r: &Raster) -> Vec<u8> {
    let e = pdf_editor::encode::encode_jpeg(r, 85).unwrap();
    match e.filter {
        pdf_editor::encode::ImageFilter::Dct => e.data,
        _ => {
            let mut out = Vec::new();
            std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(&e.data[..]), &mut out).unwrap();
            out
        }
    }
}

/// A PDF whose pages are full-page JPEG scans with an invisible text layer,
/// as OCR tools write them.
fn scan_pdf(path: &Path, pages: usize, image: &Raster) {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" });
    let mut kids = Vec::new();
    for _ in 0..pages {
        let img = doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image", "Width" => image.width as i64, "Height" => image.height as i64,
                "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8, "Filter" => "DCTDecode",
            },
            jpeg(image),
        ));
        let content = doc.add_object(Stream::new(dictionary! {}, b"q 612 0 0 792 0 0 cm /Im0 Do Q BT 3 Tr /F1 12 Tf 72 700 Td (hidden ocr) Tj ET".to_vec()));
        let page = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => content,
            "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => img }, "Font" => dictionary! { "F1" => font } },
        });
        kids.push(Object::Reference(page));
    }
    finish(doc, pages_id, kids, path);
}

/// A typeset page: visible text and a line, no images.
fn text_pdf(path: &Path, pages: usize, text: &str, size: f64) {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" });
    let mut kids = Vec::new();
    for _ in 0..pages {
        let ops = format!("BT /F1 {size} Tf 60 600 Td ({text}) Tj ET 60 500 m 550 500 l S");
        let content = doc.add_object(Stream::new(dictionary! {}, ops.into_bytes()));
        let page = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => content, "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        });
        kids.push(Object::Reference(page));
    }
    finish(doc, pages_id, kids, path);
}

fn finish(mut doc: Document, pages_id: lopdf::ObjectId, kids: Vec<Object>, path: &Path) {
    let count = kids.len() as i64;
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }));
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    doc.save(path).unwrap();
}

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        pdf_editor::init_process();
        Fixture { dir: tempfile::tempdir().unwrap() }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

fn run(sources: &[Arc<Source>], pages: &[Page], compression: Compression, ocr: Option<&str>, out: &Path) -> SaveReport {
    let options = SaveOptions { compression, ocr_language: ocr.map(str::to_owned), threads: 4, max_dpi: None, jpeg_quality: None };
    save(sources, pages, &options, out, &|_, _, _| {}, &AtomicBool::new(false)).unwrap()
}

fn pages_of(source: usize, src: &Source, kind_for_text: PageKind) -> Vec<Page> {
    (0..src.pages.len())
        .map(|i| Page::new(source, i, if src.pages[i].is_text_page { kind_for_text } else { PageKind::Scan }))
        .collect()
}

/// Image streams of the output, in page order.
fn output_images(path: &Path) -> Vec<Stream> {
    let doc = Document::load(path).unwrap();
    let mut out = Vec::new();
    for (_, id) in doc.get_pages() {
        let (resources, _) = doc.get_page_resources(id).unwrap();
        let xobjects = resources.unwrap().get(b"XObject").unwrap().as_dict().unwrap();
        for (_, r) in xobjects.iter() {
            out.push(doc.get_object(r.as_reference().unwrap()).unwrap().as_stream().unwrap().clone());
        }
    }
    out
}

fn inflate(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(data), &mut out).unwrap();
    out
}

fn route_count(report: &SaveReport, route: PageRoute) -> usize {
    report.routes.get(&route).copied().unwrap_or(0)
}

#[test]
fn pages_are_classified() {
    let f = Fixture::new();
    scan_pdf(&f.path("scan.pdf"), 2, &scan_raster(400, 518));
    text_pdf(&f.path("text.pdf"), 2, "Typeset text", 24.0);
    let scan = Source::open(&f.path("scan.pdf")).unwrap();
    let text = Source::open(&f.path("text.pdf")).unwrap();
    assert!(scan.pages.iter().all(|p| !p.is_text_page && p.scan_image().is_some()), "OCR'd scans are scans");
    assert!(text.pages.iter().all(|p| p.is_text_page));
}

#[test]
fn no_compression_keeps_original_images_byte_for_byte() {
    let f = Fixture::new();
    scan_pdf(&f.path("scan.pdf"), 3, &scan_raster(400, 518));
    let source = Arc::new(Source::open(&f.path("scan.pdf")).unwrap());
    let mut pages = pages_of(0, &source, PageKind::Text);
    pages.iter_mut().for_each(|p| p.color = ColorMode::Color);
    let report = run(std::slice::from_ref(&source), &pages, Compression::None, None, &f.path("out.pdf"));
    assert_eq!(route_count(&report, PageRoute::PassThrough), 3);
    let original = output_images(&f.path("scan.pdf"));
    let saved = output_images(&f.path("out.pdf"));
    assert_eq!(saved.len(), 3);
    for (a, b) in original.iter().zip(&saved) {
        // A Flate wrapper may be added around the JPEG; the JPEG stays as is.
        let jpeg = match b.dict.get(b"Filter").unwrap() {
            Object::Array(_) => b.decompressed_content().unwrap_or_else(|_| inflate(&b.content)),
            _ => b.content.clone(),
        };
        assert_eq!(a.content, jpeg, "pass-through must not touch the JPEG");
    }
    assert!(report.bytes <= std::fs::metadata(f.path("scan.pdf")).unwrap().len());
}

#[test]
fn no_compression_gray_keeps_the_luma_channel() {
    let f = Fixture::new();
    scan_pdf(&f.path("scan.pdf"), 2, &scan_raster(400, 518));
    let source = Arc::new(Source::open(&f.path("scan.pdf")).unwrap());
    let pages = pages_of(0, &source, PageKind::Text);
    let report = run(&[source], &pages, Compression::None, None, &f.path("out.pdf"));
    assert_eq!(route_count(&report, PageRoute::ExactGray), 2);
    for image in output_images(&f.path("out.pdf")) {
        assert_eq!(image.dict.get(b"ColorSpace").unwrap().as_name().unwrap(), b"DeviceGray");
        assert_eq!(image.dict.get(b"Width").unwrap().as_i64().unwrap(), 400);
    }
}

#[test]
fn high_compression_downsamples_and_shrinks() {
    let f = Fixture::new();
    // 400 px over 8.5 in is 47 DPI: make it 300 DPI so downsampling applies.
    scan_pdf(&f.path("scan.pdf"), 2, &scan_raster(2550, 3300));
    let source = Arc::new(Source::open(&f.path("scan.pdf")).unwrap());
    let pages = pages_of(0, &source, PageKind::Text);
    let report = run(&[source], &pages, Compression::High, None, &f.path("out.pdf"));
    assert_eq!(route_count(&report, PageRoute::Reencoded), 2);
    let width = output_images(&f.path("out.pdf"))[0].dict.get(b"Width").unwrap().as_i64().unwrap();
    assert_eq!(width, 1020, "8.5 in at 120 DPI");
    assert!(report.bytes < std::fs::metadata(f.path("scan.pdf")).unwrap().len());
}

#[test]
fn black_and_white_uses_clean_jbig2() {
    let f = Fixture::new();
    scan_pdf(&f.path("scan.pdf"), 1, &scan_raster(1275, 1650));
    let source = Arc::new(Source::open(&f.path("scan.pdf")).unwrap());
    for mode in [ColorMode::BwFlat, ColorMode::BwAdaptive] {
        let mut pages = pages_of(0, &source, PageKind::Text);
        pages[0].color = mode;
        run(std::slice::from_ref(&source), &pages, Compression::None, None, &f.path("out.pdf"));
        let image = &output_images(&f.path("out.pdf"))[0];
        assert_eq!(image.dict.get(b"Filter").unwrap().as_name().unwrap(), b"JBIG2Decode");
        assert!(!image.content.windows(3).any(|w| w == [0xFF, 0xFF, 0xAC]));
    }
}

#[test]
fn sharp_text_modes_paint_ink_through_a_full_resolution_jbig2_mask() {
    let f = Fixture::new();
    scan_pdf(&f.path("scan.pdf"), 1, &scan_raster(1275, 1650));
    let source = Arc::new(Source::open(&f.path("scan.pdf")).unwrap());
    for (mode, channels) in [(ColorMode::GrayText, 1), (ColorMode::ColorText, 3)] {
        let mut pages = pages_of(0, &source, PageKind::Text);
        pages[0].color = mode;
        run(std::slice::from_ref(&source), &pages, Compression::High, None, &f.path("out.pdf"));
        let doc = Document::load(f.path("out.pdf")).unwrap();
        let images = output_images(&f.path("out.pdf"));
        assert_eq!(images.len(), 2, "background and ink");
        let ink = images.iter().find(|i| i.dict.has(b"Mask")).expect("ink image with a mask");
        let mask = doc.get_object(ink.dict.get(b"Mask").unwrap().as_reference().unwrap()).unwrap().as_stream().unwrap();
        assert_eq!(mask.dict.get(b"ImageMask").unwrap(), &Object::Boolean(true));
        assert_eq!(mask.dict.get(b"Filter").unwrap().as_name().unwrap(), b"JBIG2Decode");
        assert_eq!(mask.dict.get(b"Width").unwrap().as_i64().unwrap(), 1700, "a 150 DPI scan gets a 200 DPI mask at high compression");
        let background = images.iter().find(|i| !i.dict.has(b"Mask")).unwrap();
        assert_eq!(background.dict.get(b"Width").unwrap().as_i64().unwrap(), 1020, "the level's resolution: 8.5 in at 120 DPI");
        let space = if channels == 1 { &b"DeviceGray"[..] } else { b"DeviceRGB" };
        assert_eq!(background.dict.get(b"ColorSpace").unwrap().as_name().unwrap(), space);
    }
}

#[test]
fn split_and_rotated_text_pages_stay_text() {
    let f = Fixture::new();
    text_pdf(&f.path("text.pdf"), 1, "Typeset text", 24.0);
    let source = Arc::new(Source::open(&f.path("text.pdf")).unwrap());
    let mut left = Page::new(0, 0, PageKind::Text);
    left.view = left.view.rotated_cw().split(Side::Left);
    let mut right = left.clone();
    right.view = Page::new(0, 0, PageKind::Text).view.rotated_cw().split(Side::Right);
    let report = run(&[source], &[left, right], Compression::High, None, &f.path("out.pdf"));
    assert_eq!(route_count(&report, PageRoute::Copied), 2);
    let doc = Document::load(f.path("out.pdf")).unwrap();
    let boxes: Vec<Vec<f32>> = doc
        .get_pages()
        .values()
        .map(|&id| {
            let page = doc.get_dictionary(id).unwrap();
            assert_eq!(page.get(b"Rotate").unwrap().as_i64().unwrap(), 90);
            page.get(b"CropBox").unwrap().as_array().unwrap().iter().map(|o| o.as_float().unwrap()).collect()
        })
        .collect();
    // After a quarter turn, the displayed left half is the bottom half.
    assert_eq!(boxes[0], vec![0.0, 0.0, 612.0, 396.0]);
    assert_eq!(boxes[1], vec![0.0, 396.0, 612.0, 792.0]);
}

/// A spread: one line of text on each half, with a font that has widths.
fn spread_pdf(path: &Path) {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        "FirstChar" => 32, "Widths" => vec![Object::Integer(556); 95],
    });
    let column = |x: u32, word: &str| {
        let lines: String = (0..20).map(|k| format!("0 -14 Td ({word} line {k} of the column) Tj ")).collect();
        format!("BT /F1 12 Tf {x} 720 Td {lines}ET ")
    };
    let ops = column(60, "LEFTPAGE") + &column(660, "RIGHTPAGE");
    let content = doc.add_object(Stream::new(dictionary! {}, ops.as_bytes().to_vec()));
    let page = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id, "MediaBox" => vec![0.into(), 0.into(), 1224.into(), 792.into()],
        "Contents" => content, "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
    });
    finish(doc, pages_id, vec![Object::Reference(page)], path);
}

#[test]
fn split_spreads_lose_the_other_halfs_text() {
    let f = Fixture::new();
    spread_pdf(&f.path("spread.pdf"));
    let source = Arc::new(Source::open(&f.path("spread.pdf")).unwrap());
    let mut left = Page::new(0, 0, PageKind::Text);
    let mut right = left.clone();
    left.view = left.view.split(Side::Left);
    right.view = right.view.split(Side::Right);
    let report = run(&[source], &[left, right], Compression::None, None, &f.path("out.pdf"));
    assert!(report.hidden_text_removed > 0);
    let doc = Document::load(f.path("out.pdf")).unwrap();
    let contents: Vec<String> = doc
        .get_pages()
        .values()
        .map(|&id| String::from_utf8_lossy(&pdf_editor::content::page_content_bytes(&doc, id).unwrap()).into_owned())
        .collect();
    assert!(contents[0].contains("LEFTPAGE line 19") && !contents[0].contains("RIGHTPAGE"));
    assert!(contents[1].contains("RIGHTPAGE") && !contents[1].contains("LEFTPAGE"));
}

#[test]
fn merged_sources_keep_page_order() {
    let f = Fixture::new();
    scan_pdf(&f.path("scan.pdf"), 2, &scan_raster(400, 518));
    text_pdf(&f.path("text.pdf"), 3, "Typeset text", 24.0);
    let a = Arc::new(Source::open(&f.path("scan.pdf")).unwrap());
    let b = Arc::new(Source::open(&f.path("text.pdf")).unwrap());
    let mut pages = pages_of(1, &b, PageKind::Text);
    pages.extend(pages_of(0, &a, PageKind::Text));
    let report = run(&[a, b], &pages, Compression::High, None, &f.path("out.pdf"));
    assert_eq!(report.pages, 5);
    let doc = Document::load(f.path("out.pdf")).unwrap();
    let kinds: Vec<bool> = doc
        .get_pages()
        .values()
        .map(|&id| doc.get_page_resources(id).unwrap().0.is_some_and(|r| r.has(b"XObject")))
        .collect();
    assert_eq!(kinds, vec![false, false, false, true, true], "text pages first, then scans");
}

#[test]
fn ocr_finds_the_words_of_a_rendered_page() {
    if !pdf_editor::ocr::is_installed("eng") {
        eprintln!("skipped: no English Tesseract model installed");
        return;
    }
    let f = Fixture::new();
    text_pdf(&f.path("text.pdf"), 1, "HELLO OCR WORLD", 48.0);
    let text = Source::open(&f.path("text.pdf")).unwrap();
    let render = pdf_editor::render::render_page(&text.render_path, 0, 300.0 / 72.0, text.pages[0].display_size).unwrap();
    pdf_editor::render::close_documents();
    scan_pdf(&f.path("scan.pdf"), 1, &render);
    let source = Arc::new(Source::open(&f.path("scan.pdf")).unwrap());
    let pages = pages_of(0, &source, PageKind::Text);
    run(&[source], &pages, Compression::High, Some("eng"), &f.path("out.pdf"));
    let doc = Document::load(f.path("out.pdf")).unwrap();
    let page = *doc.get_pages().values().next().unwrap();
    let content = String::from_utf8_lossy(&pdf_editor::content::page_content_bytes(&doc, page).unwrap()).into_owned();
    let hex: String = "HELLO".encode_utf16().map(|u| format!("{u:04X}")).collect();
    assert!(content.contains(&hex), "text layer should contain HELLO");
    assert!(content.contains("3 Tr"), "text layer must be invisible");
}
