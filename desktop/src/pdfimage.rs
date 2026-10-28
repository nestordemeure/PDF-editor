//! Reading image XObjects: what they are, and decoding the ones whose
//! encoding is fully understood.

use serde::{Deserialize, Serialize};
use crate::pixel::Raster;
use anyhow::{Context, Result, bail};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    pub channels: u8,
    pub codec: ImageCodec,
    /// Size of the encoded stream.
    pub encoded_len: usize,
    /// Has a soft mask (kept as it is when the image is replaced).
    pub has_smask: bool,
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
    inspect_with(doc, id, false)
}

/// As `inspect`; with `allow_smask`, images with a soft mask qualify too
/// (the mask is a separate image and stays valid when this one changes).
pub fn inspect_with(doc: &Document, id: ObjectId, allow_smask: bool) -> Result<Option<ImageInfo>> {
    let stream = image_stream(doc, id)?;
    let dict = &stream.dict;
    if name(doc, dict, b"Subtype") != Some(b"Image") {
        bail!("XObject {id:?} is not an image");
    }
    let present = |key: &[u8]| dict.has(key) && !matches!(dict.get_deref(key, doc), Ok(Object::Boolean(false)) | Ok(Object::Null));
    let has_smask = present(b"SMask");
    if (has_smask && !allow_smask) || [&b"Mask"[..], b"Decode", b"ImageMask"].into_iter().any(present) {
        return Ok(None);
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
    if matches!(codec, ImageCodec::Flate8 | ImageCodec::Flate1) && flate_parms(doc, dict).is_none() {
        return Ok(None);
    }
    Ok(Some(ImageInfo { width: width as u32, height: height as u32, channels, codec, encoded_len: stream.content.len(), has_smask }))
}

/// An image stream with what decoding it needs, independent of its
/// document: it can move to another thread or WebAssembly instance.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredImage {
    pub info: ImageInfo,
    /// The stream's encoded bytes.
    pub content: Vec<u8>,
    /// The content is Flate-compressed (8 or 1 bit images may be raw).
    pub flate: bool,
    /// Flate predictor parameters (`/DecodeParms`).
    pub parms: Vec<(Vec<u8>, i64)>,
}

/// Copies an image that `inspect` accepted out of its document.
pub fn extract(doc: &Document, id: ObjectId, info: &ImageInfo) -> Result<StoredImage> {
    let stream = image_stream(doc, id)?;
    let parms = match flate_parms(doc, &stream.dict) {
        Some(Some(d)) => d.iter().map(|(k, v)| Ok((k.clone(), v.as_i64().with_context(|| format!("image {id:?} has a non-integer decode parameter"))?))).collect::<Result<_>>()?,
        Some(None) => Vec::new(),
        None => bail!("image {id:?} has unsupported decode parameters"),
    };
    let flate = matches!(info.codec, ImageCodec::Flate8 | ImageCodec::Flate1) && single_filter(doc, &stream.dict) == Some(Some(&b"FlateDecode"[..]));
    Ok(StoredImage { info: *info, content: stream.content.clone(), flate, parms })
}

/// The `/DecodeParms` dictionary of a single-filter stream: `Some(None)`
/// when there is none, `None` when it is not a dictionary of integers.
fn flate_parms<'a>(doc: &'a Document, dict: &'a Dictionary) -> Option<Option<&'a Dictionary>> {
    let parms = match dict.get_deref(b"DecodeParms", doc) {
        Err(_) | Ok(Object::Null) => return Some(None),
        Ok(Object::Dictionary(d)) => d,
        Ok(Object::Array(a)) if a.len() == 1 => match doc.dereference(&a[0]).ok()?.1 {
            Object::Null => return Some(None),
            Object::Dictionary(d) => d,
            _ => return None,
        },
        _ => return None,
    };
    parms.iter().all(|(_, v)| v.as_i64().is_ok()).then_some(Some(parms))
}

impl StoredImage {
    /// The JPEG file inside a JPEG image.
    pub fn jpeg_data(&self) -> Result<std::borrow::Cow<'_, [u8]>> {
        match self.info.codec {
            ImageCodec::Jpeg => Ok(std::borrow::Cow::Borrowed(&self.content)),
            ImageCodec::FlateJpeg => {
                let mut out = Vec::new();
                std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(&self.content[..]), &mut out).context("Flate layer of a JPEG")?;
                Ok(std::borrow::Cow::Owned(out))
            }
            _ => bail!("not a JPEG image"),
        }
    }

    /// Decodes the image, as gray when `gray` is set.
    pub fn decode(&self, gray: bool) -> Result<Raster> {
        let raster = self.decode_native()?;
        Ok(if gray { raster.to_gray() } else { raster })
    }

    fn flate_content(&self) -> Result<Vec<u8>> {
        if !self.flate {
            return Ok(self.content.clone());
        }
        let mut dict = Dictionary::new();
        dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
        if !self.parms.is_empty() {
            let mut parms = Dictionary::new();
            for (k, v) in &self.parms {
                parms.set(k.clone(), Object::Integer(*v));
            }
            dict.set("DecodeParms", Object::Dictionary(parms));
        }
        Ok(Stream::new(dict, self.content.clone()).decompressed_content()?)
    }

    fn decode_native(&self) -> Result<Raster> {
        let info = &self.info;
        let (w, h) = (info.width as usize, info.height as usize);
        match info.codec {
            ImageCodec::Jpeg | ImageCodec::FlateJpeg => {
                let raster = crate::encode::decode_jpeg(&self.jpeg_data()?, info.channels)?;
                if (raster.width, raster.height) != (info.width, info.height) {
                    bail!("JPEG is {}x{}, its dictionary says {w}x{h}", raster.width, raster.height);
                }
                Ok(raster)
            }
            ImageCodec::Flate8 => {
                let mut data = self.flate_content().context("image data")?;
                let need = w * h * info.channels as usize;
                if data.len() < need {
                    bail!("image has {} bytes, needs {need}", data.len());
                }
                data.truncate(need);
                Raster::new(info.width, info.height, info.channels, data)
            }
            ImageCodec::Flate1 => {
                let packed = self.flate_content().context("image data")?;
                let row = w.div_ceil(8);
                if packed.len() < row * h {
                    bail!("bilevel image has {} bytes, needs {}", packed.len(), row * h);
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
}

/// Decodes an image that `inspect` accepted, as gray when `gray` is set.
pub fn decode(doc: &Document, id: ObjectId, info: &ImageInfo, gray: bool) -> Result<Raster> {
    extract(doc, id, info)?.decode(gray).with_context(|| format!("image {id:?}"))
}
