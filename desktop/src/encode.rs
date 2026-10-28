//! Image codecs: JPEG (zenjpeg), JBIG2 (lossless generic regions), Flate.

use serde::{Deserialize, Serialize};
use crate::pixel::Raster;
use anyhow::{Context, Result, anyhow, bail};
use enough::Unstoppable;
use std::io::Write;

/// An image ready to embed as a PDF image XObject.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncodedImage {
    pub width: u32,
    pub height: u32,
    /// 1 (DeviceGray) or 3 (DeviceRGB).
    pub channels: u8,
    pub bits_per_component: u8,
    pub filter: ImageFilter,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ImageFilter {
    Dct,
    /// JPEG wrapped in Flate, which shrinks entropy-coded data by a few percent.
    FlateDct,
    /// Flate with PNG predictors (`/Predictor 15`).
    FlatePng,
    Jbig2,
}

pub fn zlib(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::with_capacity(data.len() / 4), flate2::Compression::new(9));
    e.write_all(data).expect("writing to a Vec cannot fail");
    e.finish().expect("writing to a Vec cannot fail")
}

pub fn decode_jpeg(data: &[u8], channels: u8) -> Result<Raster> {
    use zenjpeg::decoder::{Decoder, PixelFormat};
    let format = if channels == 1 { PixelFormat::Gray } else { PixelFormat::Rgb };
    let result = Decoder::new().output_format(format).decode(data, Unstoppable).map_err(|e| anyhow!("JPEG decode: {e}"))?;
    let (w, h) = result.dimensions();
    let pixels = result.into_pixels_u8().context("JPEG decoder returned no 8-bit pixels")?;
    Raster::new(w, h, channels, pixels)
}

/// zenjpeg setting with the same fidelity (PSNR on a scanned page at
/// 120 DPI) as a libjpeg/MozJPEG quality. zenjpeg keeps more detail per
/// nominal step, and more in gray mode than in color mode.
fn zenjpeg_setting(libjpeg_quality: u8, gray: bool) -> u8 {
    const GRAY: [(f64, f64); 8] = [(0.0, 1.0), (25.0, 8.0), (50.0, 20.0), (60.0, 24.0), (75.0, 38.0), (85.0, 63.0), (95.0, 88.0), (100.0, 100.0)];
    const COLOR: [(f64, f64); 8] = [(0.0, 1.0), (25.0, 10.0), (50.0, 24.0), (60.0, 30.0), (75.0, 52.0), (85.0, 73.0), (95.0, 90.0), (100.0, 100.0)];
    let points = if gray { &GRAY } else { &COLOR };
    let q = libjpeg_quality.min(100) as f64;
    let i = points.iter().position(|p| p.0 >= q).unwrap_or(points.len() - 1).max(1);
    let ((x0, y0), (x1, y1)) = (points[i - 1], points[i]);
    (y0 + (q - x0) * (y1 - y0) / (x1 - x0)).round() as u8
}

/// Encodes at the fidelity of a libjpeg quality setting.
pub fn encode_jpeg(r: &Raster, quality: u8) -> Result<EncodedImage> {
    use zenjpeg::encoder::{ChromaSubsampling, EncoderConfig, PixelLayout, Quality};
    let q = Quality::ApproxMozjpeg(zenjpeg_setting(quality, r.is_gray()));
    let (config, layout) = if r.is_gray() {
        (EncoderConfig::grayscale(q), PixelLayout::Gray8Srgb)
    } else {
        (EncoderConfig::ycbcr(q, ChromaSubsampling::Quarter), PixelLayout::Rgb8Srgb)
    };
    let mut enc = config.encode_from_bytes(r.width, r.height, layout).map_err(|e| anyhow!("JPEG encoder: {e}"))?;
    enc.push_packed(&r.data, Unstoppable).map_err(|e| anyhow!("JPEG encode: {e}"))?;
    let data = enc.finish().map_err(|e| anyhow!("JPEG encode: {e}"))?;
    Ok(wrap_jpeg(EncodedImage { width: r.width, height: r.height, channels: r.channels, bits_per_component: 8, filter: ImageFilter::Dct, data }))
}

/// Wraps a JPEG in Flate when that saves at least 1%.
pub fn wrap_jpeg(e: EncodedImage) -> EncodedImage {
    let wrapped = zlib(&e.data);
    if wrapped.len() * 100 <= e.data.len() * 99 {
        EncodedImage { filter: ImageFilter::FlateDct, data: wrapped, ..e }
    } else {
        e
    }
}

/// Estimates the libjpeg quality setting from the luma quantization table.
pub fn jpeg_quality(data: &[u8]) -> Option<u8> {
    const STD_LUMA: [u32; 64] = [
        16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56, 14, 17, 22, 29,
        51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113, 92, 49, 64, 78, 87, 103, 121,
        120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
    ];
    const ZIGZAG: [usize; 64] = [
        0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
        28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61,
        54, 47, 55, 62, 63,
    ];
    let mut i = 2;
    while i + 4 <= data.len() {
        if data[i] != 0xFF {
            return None;
        }
        let marker = data[i + 1];
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if marker == 0xDB {
            let seg = data.get(i + 4..i + 2 + len)?;
            let (precision, table) = (seg[0] >> 4, seg[0] & 0x0F);
            if table != 0 {
                i += 2 + len;
                continue;
            }
            let zigzag: Vec<u32> = if precision == 0 {
                seg.get(1..65)?.iter().map(|&v| v as u32).collect()
            } else {
                seg.get(1..129)?.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]]) as u32).collect()
            };
            let mut values = [0u32; 64];
            for (k, &v) in zigzag.iter().enumerate() {
                values[ZIGZAG[k]] = v;
            }
            let scale: f64 = values.iter().zip(STD_LUMA.iter()).map(|(&q, &s)| q as f64 * 100.0 / s as f64).sum::<f64>() / 64.0;
            let quality = if scale <= 100.0 { (200.0 - scale) / 2.0 } else { 5000.0 / scale };
            return Some(quality.round().clamp(1.0, 100.0) as u8);
        }
        if marker == 0xDA {
            return None;
        }
        i += 2 + len;
    }
    None
}

/// Lossless JBIG2 (generic region) of a bilevel raster (0 = black).
pub fn encode_jbig2(r: &Raster) -> Result<EncodedImage> {
    if !r.is_gray() {
        bail!("JBIG2 needs a bilevel gray raster");
    }
    let bits: Vec<u8> = r.data.iter().map(|&v| (v < 128) as u8).collect();
    let result = jbig2enc_rust::encode_single_image_lossless(&bits, r.width, r.height, true)
        .map_err(|e| anyhow!("JBIG2 encode: {e:?}"))?;
    if result.global_data.is_some() {
        bail!("lossless JBIG2 produced a global dictionary");
    }
    Ok(EncodedImage {
        width: r.width,
        height: r.height,
        channels: 1,
        bits_per_component: 1,
        filter: ImageFilter::Jbig2,
        data: drop_mq_padding(&result.page_data)?,
    })
}

/// jbig2enc-rust ends each generic region with `FF FF AC`; the arithmetic
/// coder's flush must drop a final `FF` before the `FF AC` marker (as C
/// jbig2enc does), else readers report an extra byte.
fn drop_mq_padding(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        let header_start = i;
        let field = |at: usize, n: usize| data.get(at..at + n).context("truncated JBIG2 segment header");
        let number = u32::from_be_bytes(field(i, 4)?.try_into().unwrap());
        let flags = field(i + 4, 1)?[0];
        let refs = field(i + 5, 1)?[0] >> 5;
        if refs > 4 {
            bail!("JBIG2 segment {number} uses the long referred-to form");
        }
        let ref_size = if number <= 256 { 1 } else if number <= 65536 { 2 } else { 4 };
        i += 6 + refs as usize * ref_size + if flags & 0x40 != 0 { 4 } else { 1 };
        let length = u32::from_be_bytes(field(i, 4)?.try_into().unwrap()) as usize;
        i += 4;
        let body = data.get(i..i + length).context("truncated JBIG2 segment")?;
        let generic = matches!(flags & 0x3f, 38 | 39);
        if generic && body.ends_with(&[0xFF, 0xFF, 0xAC]) {
            out.extend_from_slice(&data[header_start..i - 4]);
            out.extend_from_slice(&((length - 1) as u32).to_be_bytes());
            out.extend_from_slice(&body[..length - 3]);
            out.extend_from_slice(&[0xFF, 0xAC]);
        } else {
            out.extend_from_slice(&data[header_start..i + length]);
        }
        i += length;
    }
    Ok(out)
}

/// Lossless 8-bit image: per-row PNG filter choice (as libpng does), then
/// zlib.
pub fn encode_flate_png(r: &Raster) -> EncodedImage {
    let c = r.channels as usize;
    let row = r.width as usize * c;
    let mut filtered = Vec::with_capacity((row + 1) * r.height as usize);
    let zero = vec![0u8; row];
    let mut candidate = vec![0u8; row];
    let mut best = vec![0u8; row];
    for y in 0..r.height as usize {
        let cur = &r.data[y * row..(y + 1) * row];
        let up = if y == 0 { &zero[..] } else { &r.data[(y - 1) * row..y * row] };
        let mut best_cost = u64::MAX;
        let mut best_type = 0u8;
        for filter in 0..5u8 {
            for x in 0..row {
                let a = if x >= c { cur[x - c] } else { 0 };
                let b = up[x];
                let cc = if x >= c { up[x - c] } else { 0 };
                let pred = match filter {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => ((a as u16 + b as u16) / 2) as u8,
                    _ => paeth(a, b, cc),
                };
                candidate[x] = cur[x].wrapping_sub(pred);
            }
            let cost: u64 = candidate.iter().map(|&v| (v as i8).unsigned_abs() as u64).sum();
            if cost < best_cost {
                best_cost = cost;
                best_type = filter;
                std::mem::swap(&mut best, &mut candidate);
            }
        }
        filtered.push(best_type);
        filtered.extend_from_slice(&best);
    }
    EncodedImage {
        width: r.width,
        height: r.height,
        channels: r.channels,
        bits_per_component: 8,
        filter: ImageFilter::FlatePng,
        data: zlib(&filtered),
    }
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(channels: u8) -> Raster {
        let (w, h) = (64u32, 48u32);
        let data = (0..w * h * channels as u32).map(|i| ((i * 7) % 256) as u8).collect();
        Raster::new(w, h, channels, data).unwrap()
    }

    #[test]
    fn jpeg_round_trip_keeps_size_and_quality_estimate() {
        let r = sample(3);
        let jpeg = encode_jpeg(&r, 80).unwrap();
        let data = match jpeg.filter {
            ImageFilter::FlateDct => {
                let mut out = Vec::new();
                std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(&jpeg.data[..]), &mut out).unwrap();
                out
            }
            _ => jpeg.data,
        };
        let back = decode_jpeg(&data, 3).unwrap();
        assert_eq!((back.width, back.height), (r.width, r.height));
        assert!(jpeg_quality(&data).is_some());
    }

    #[test]
    fn zenjpeg_mapping_is_monotonic() {
        assert_eq!(zenjpeg_setting(50, true), 20);
        assert_eq!(zenjpeg_setting(100, false), 100);
        for gray in [true, false] {
            assert!((1..=100).all(|q| zenjpeg_setting(q, gray) >= zenjpeg_setting(q - 1, gray)));
        }
    }

    #[test]
    fn jbig2_produces_a_page_stream() {
        let data = (0..200 * 100).map(|i| if (i / 7) % 3 == 0 { 0 } else { 255 }).collect();
        let r = Raster::new(200, 100, 1, data).unwrap();
        let e = encode_jbig2(&r).unwrap();
        assert!(!e.data.is_empty());
        assert!(!e.data.windows(3).any(|w| w == [0xFF, 0xFF, 0xAC]));
    }

    #[test]
    fn mq_padding_is_dropped_and_length_fixed() {
        // Segment 1, immediate lossless generic region (type 38), page 1,
        // 5 data bytes ending in FF FF AC.
        let segment = [0, 0, 0, 1, 38, 0, 1, 0, 0, 0, 5, 0x12, 0x34, 0xFF, 0xFF, 0xAC];
        let fixed = drop_mq_padding(&segment).unwrap();
        assert_eq!(fixed, [0, 0, 0, 1, 38, 0, 1, 0, 0, 0, 4, 0x12, 0x34, 0xFF, 0xAC]);
    }
}
