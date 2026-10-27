//! Fidelity of zenjpeg YCbCr against MozJPEG on a color scan page.
use enough::Unstoppable;
use pdf_editor::pixel::Raster;

fn psnr(a: &Raster, b: &Raster) -> f64 {
    let mse: f64 = a.data.iter().zip(&b.data).map(|(&x, &y)| (x as f64 - y as f64).powi(2)).sum::<f64>() / a.data.len() as f64;
    10.0 * (255.0f64 * 255.0 / mse).log10()
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let source = pdf_editor::source::Source::open(args[1].as_ref())?;
    let (id, _) = source.pages[0].scan_image().unwrap();
    let info = pdf_editor::pdfimage::inspect(&source.doc, id)?.unwrap();
    let full = pdf_editor::pdfimage::decode(&source.doc, id, &info, false)?;
    let f = 120.0 / 290.0;
    let rgb = full.resize((full.width as f64 * f) as u32, (full.height as f64 * f) as u32)?;
    for q in [50u8, 60, 75, 85, 95] {
        let data = mozjpeg_rs::Encoder::baseline_optimized().quality(q).encode_rgb(&rgb.data, rgb.width, rgb.height).unwrap();
        let back = pdf_editor::encode::decode_jpeg(&data, 3)?;
        println!("moz q{q:3} {:8} bytes {:6.2} dB", data.len(), psnr(&rgb, &back));
    }
    for q in [10u8, 15, 20, 25, 30, 35, 40, 50, 60, 70, 80, 90] {
        use zenjpeg::encoder::{ChromaSubsampling, EncoderConfig, PixelLayout, Quality};
        let mut enc = EncoderConfig::ycbcr(Quality::ApproxMozjpeg(q), ChromaSubsampling::Quarter).encode_from_bytes(rgb.width, rgb.height, PixelLayout::Rgb8Srgb).unwrap();
        enc.push_packed(&rgb.data, Unstoppable).unwrap();
        let data = enc.finish().unwrap();
        let back = pdf_editor::encode::decode_jpeg(&data, 3)?;
        println!("zen q{q:3} {:8} bytes {:6.2} dB", data.len(), psnr(&rgb, &back));
    }
    Ok(())
}
