//! Compares JPEG encoder settings on the first scan page of a PDF.
//! Usage: cargo run --release --example jpeg_bench -- file.pdf [ppi]

use enough::Unstoppable;
use pdf_editor::pixel::Raster;
use pdf_editor::source::Source;
use std::time::Instant;

fn psnr(a: &Raster, b: &Raster) -> f64 {
    let mse: f64 = a.data.iter().zip(&b.data).map(|(&x, &y)| (x as f64 - y as f64).powi(2)).sum::<f64>() / a.data.len() as f64;
    10.0 * (255.0f64 * 255.0 / mse).log10()
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let source = Source::open(args[1].as_ref())?;
    let ppi: f64 = args.get(2).map_or(150.0, |s| s.parse().unwrap());
    let (id, _) = source.pages[0].scan_image().expect("scan page");
    let info = pdf_editor::pdfimage::inspect(&source.doc, id)?.unwrap();
    let full = pdf_editor::pdfimage::decode(&source.doc, id, &info)?;
    let f = ppi / 290.0;
    let gray = full.to_gray().resize((full.width as f64 * f) as u32, (full.height as f64 * f) as u32)?;
    println!("gray {}x{}", gray.width, gray.height);
    for q in [15u8, 25, 35, 45] {
        use zenjpeg::encoder::{EncoderConfig, PixelLayout, Quality};
        let variants: Vec<(&str, EncoderConfig)> = vec![
            ("zen gray", EncoderConfig::grayscale(Quality::ApproxMozjpeg(q))),
            ("zen gray auto", EncoderConfig::grayscale(Quality::ApproxMozjpeg(q)).auto_optimize(true)),
            ("zen gray jpegli", EncoderConfig::grayscale(Quality::ApproxJpegli(q as f32))),
        ];
        for (name, enc) in [("moz baseline", mozjpeg_rs::Encoder::baseline_optimized()), ("moz max", mozjpeg_rs::Encoder::max_compression())] {
            let t = Instant::now();
            let data = enc.quality(q).encode_gray(&gray.data, gray.width, gray.height).unwrap();
            let ms = t.elapsed().as_millis();
            let back = pdf_editor::encode::decode_jpeg(&data, 1)?;
            println!("q{q:3} {name:18} {:8} bytes {:6.2} dB {ms:4} ms", data.len(), psnr(&gray, &back));
        }
        for (name, config) in variants {
            let t = Instant::now();
            let mut enc = config.encode_from_bytes(gray.width, gray.height, PixelLayout::Gray8Srgb).unwrap();
            enc.push_packed(&gray.data, Unstoppable).unwrap();
            let data = enc.finish().unwrap();
            let ms = t.elapsed().as_millis();
            let back = pdf_editor::encode::decode_jpeg(&data, 1)?;
            println!("q{q:3} {name:18} {:8} bytes {:6.2} dB {ms:4} ms", data.len(), psnr(&gray, &back));
        }
    }
    Ok(())
}
