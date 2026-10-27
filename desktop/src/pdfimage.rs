//! Reading image XObjects: what they are, and decoding the ones whose
//! encoding is fully understood.

use crate::pixel::Raster;
use anyhow::{Context, Result, bail};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageCodec {
    Jpeg,
    /// JPEG inside Flate (`/Filter [/FlateDecode /DCTDecode]`).
    FlateJpeg,
    /// Flate (or no filter), 8 bits per component.
    Flate8,
    /// Flate (or no filter), 1 bit per component, gray.
    Flate1,
}

/// A decodable image: one of the codecs above, gray or RGB, no masks and
/// no decode arrays. Anything else is rendered by MuPDF instead.
#[derive(Clone, Copy, Debug)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    pub channels: u8,
    pub codec: ImageCodec,
    /// Size of the encoded stream.
    pub encoded_len: usize,
}

fn name<'a>(doc: &'a Document, dict: &'a Dictionary, key: &[u8]) -> Option<&'a [u8]> {
    dict.get_deref(key, doc).ok().and_then(|o| o.as_name().ok())
}

fn int(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<i64> {
    dict.get_deref(key, doc).ok().and_then(|o| o.as_i64().ok())
}

/// Channel count of a gray or RGB color space, `None` for anything else.
fn color_channels(doc: &Document, cs: &Object) -> Option<u8> {
    let cs = doc.dereference(cs).ok()?.1;
    match cs {
        Object::Name(n) => match n.as_slice() {
            b"DeviceGray" | b"CalGray" | b"G" => Some(1),
            b"DeviceRGB" | b"CalRGB" | b"RGB" => Some(3),
            _ => None,
        },
        Object::Array(a) => {
            let family = a.first()?.as_name().ok()?;
            match family {
                b"ICCBased" => {
                    let stream = doc.dereference(a.get(1)?).ok()?.1.as_stream().ok()?;
                    match int(doc, &stream.dict, b"N")? {
                        1 => Some(1),
                        3 => Some(3),
                        _ => None,
                    }
                }
                b"CalGray" => Some(1),
                b"CalRGB" => Some(3),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Single filter name, `Some(None)` when unfiltered, `None` for chains.
fn single_filter<'a>(doc: &'a Document, dict: &'a Dictionary) -> Option<Option<&'a [u8]>> {
    match dict.get_deref(b"Filter", doc) {
        Err(_) => Some(None),
        Ok(Object::Name(n)) => Some(Some(n.as_slice())),
        Ok(Object::Array(a)) if a.is_empty() => Some(None),
        Ok(Object::Array(a)) if a.len() == 1 => a[0].as_name().ok().map(Some),
        _ => None,
    }
}

pub fn image_stream(doc: &Document, id: ObjectId) -> Result<&Stream> {
    doc.get_object(id)?.as_stream().with_context(|| format!("image {id:?} is not a stream"))
}

/// Describes the image if this module can decode it exactly.
pub fn inspect(doc: &Document, id: ObjectId) -> Result<Option<ImageInfo>> {
    let stream = image_stream(doc, id)?;
    let dict = &stream.dict;
    if name(doc, dict, b"Subtype") != Some(b"Image") {
        bail!("XObject {id:?} is not an image");
    }
    for key in [&b"SMask"[..], b"Mask", b"Decode", b"ImageMask"] {
        if dict.has(key) && !matches!(dict.get_deref(key, doc), Ok(Object::Boolean(false)) | Ok(Object::Null)) {
            return Ok(None);
        }
    }
    let (Some(width), Some(height), Some(bpc)) =
        (int(doc, dict, b"Width"), int(doc, dict, b"Height"), int(doc, dict, b"BitsPerComponent"))
    else {
        return Ok(None);
    };
    if width <= 0 || height <= 0 || width * height > 400_000_000 {
        return Ok(None);
    }
    let Some(channels) = dict.get(b"ColorSpace").ok().and_then(|cs| color_channels(doc, cs)) else { return Ok(None) };
    let flate_jpeg = matches!(dict.get_deref(b"Filter", doc), Ok(Object::Array(a))
        if a.len() == 2 && a[0].as_name().ok() == Some(b"FlateDecode") && a[1].as_name().ok() == Some(b"DCTDecode"));
    let codec = match (single_filter(doc, dict), bpc) {
        _ if flate_jpeg && bpc == 8 => ImageCodec::FlateJpeg,
        (Some(Some(b"DCTDecode")), 8) => ImageCodec::Jpeg,
        (Some(None) | Some(Some(b"FlateDecode")), 8) => ImageCodec::Flate8,
        (Some(None) | Some(Some(b"FlateDecode")), 1) if channels == 1 => ImageCodec::Flate1,
        _ => return Ok(None),
    };
    if matches!(codec, ImageCodec::Jpeg | ImageCodec::FlateJpeg) && dict.has(b"DecodeParms") {
        return Ok(None);
    }
    Ok(Some(ImageInfo { width: width as u32, height: height as u32, channels, codec, encoded_len: stream.content.len() }))
}

/// The JPEG file inside a JPEG image stream.
pub fn jpeg_data<'a>(stream: &'a Stream, info: &ImageInfo) -> Result<std::borrow::Cow<'a, [u8]>> {
    match info.codec {
        ImageCodec::Jpeg => Ok(std::borrow::Cow::Borrowed(&stream.content)),
        ImageCodec::FlateJpeg => {
            let mut out = Vec::new();
            std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(&stream.content[..]), &mut out).context("Flate layer of a JPEG")?;
            Ok(std::borrow::Cow::Owned(out))
        }
        _ => bail!("not a JPEG image"),
    }
}

/// Decodes an image that `inspect` accepted, as gray when `gray` is set.
pub fn decode(doc: &Document, id: ObjectId, info: &ImageInfo, gray: bool) -> Result<Raster> {
    let raster = decode_native(doc, id, info)?;
    Ok(if gray { raster.to_gray() } else { raster })
}

fn decode_native(doc: &Document, id: ObjectId, info: &ImageInfo) -> Result<Raster> {
    let stream = image_stream(doc, id)?;
    let (w, h) = (info.width as usize, info.height as usize);
    match info.codec {
        ImageCodec::Jpeg | ImageCodec::FlateJpeg => {
            let raster = crate::encode::decode_jpeg(&jpeg_data(stream, info)?, info.channels)?;
            if (raster.width, raster.height) != (info.width, info.height) {
                bail!("JPEG {id:?} is {}x{}, its dictionary says {w}x{h}", raster.width, raster.height);
            }
            Ok(raster)
        }
        ImageCodec::Flate8 => {
            let mut data = stream.decompressed_content().with_context(|| format!("image {id:?}"))?;
            let need = w * h * info.channels as usize;
            if data.len() < need {
                bail!("image {id:?} has {} bytes, needs {need}", data.len());
            }
            data.truncate(need);
            Raster::new(info.width, info.height, info.channels, data)
        }
        ImageCodec::Flate1 => {
            let packed = stream.decompressed_content().with_context(|| format!("image {id:?}"))?;
            let row = w.div_ceil(8);
            if packed.len() < row * h {
                bail!("bilevel image {id:?} has {} bytes, needs {}", packed.len(), row * h);
            }
            let mut data = Vec::with_capacity(w * h);
            for y in 0..h {
                let line = &packed[y * row..(y + 1) * row];
                data.extend((0..w).map(|x| if line[x >> 3] & (0x80 >> (x & 7)) != 0 { 255 } else { 0 }));
            }
            Raster::new(info.width, info.height, 1, data)
        }
    }
}
