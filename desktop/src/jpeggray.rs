//! Exact grayscale from a color JPEG: keep the luma channel's DCT
//! coefficients and quantization table, drop the chroma channels, and
//! write a single-component JPEG. No pixel is decoded or re-quantized.

use anyhow::{Context, Result, anyhow, bail};
use enough::Unstoppable;

/// Natural (row-major) index of each zigzag position.
const ZIGZAG_TO_NATURAL: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47,
    55, 62, 63,
];

/// Frame facts needed to know that component 0 is luma.
struct Frame {
    width: u16,
    height: u16,
    component_ids: Vec<u8>,
    adobe_transform: Option<u8>,
}

fn read_frame(data: &[u8]) -> Result<Frame> {
    if data.get(0..2) != Some(&[0xFF, 0xD8]) {
        bail!("not a JPEG");
    }
    let mut i = 2;
    let mut adobe_transform = None;
    while i + 4 <= data.len() {
        if data[i] != 0xFF {
            bail!("bad JPEG marker");
        }
        let marker = data[i + 1];
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        let seg = data.get(i + 4..i + 2 + len).context("truncated JPEG segment")?;
        if marker == 0xEE && seg.starts_with(b"Adobe") && seg.len() >= 12 {
            adobe_transform = Some(seg[11]);
        }
        if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            let height = u16::from_be_bytes([seg[1], seg[2]]);
            let width = u16::from_be_bytes([seg[3], seg[4]]);
            let n = seg[5] as usize;
            let component_ids = (0..n).map(|c| seg[6 + 3 * c]).collect();
            return Ok(Frame { width, height, component_ids, adobe_transform });
        }
        i += 2 + len;
    }
    bail!("JPEG has no frame header")
}

/// The luma channel of a YCbCr JPEG as a grayscale JPEG.
pub fn luma_only(data: &[u8]) -> Result<Vec<u8>> {
    let frame = read_frame(data)?;
    if frame.component_ids.len() != 3 {
        bail!("JPEG has {} components, not YCbCr", frame.component_ids.len());
    }
    // Adobe transform 0, or components named R, G, B: stored as RGB.
    if frame.adobe_transform == Some(0) || frame.component_ids == [b'R', b'G', b'B'] {
        bail!("JPEG stores RGB, not YCbCr");
    }
    let decoded = zenjpeg::decoder::DecodeConfig::new()
        .decode_coefficients(data, Unstoppable)
        .map_err(|e| anyhow!("JPEG coefficients: {e}"))?;
    let y = decoded.components.first().context("JPEG without components")?;
    if y.id != frame.component_ids[0] {
        bail!("first decoded component is not the frame's first component");
    }
    let qt = decoded
        .quant_tables
        .get(y.quant_table_idx as usize)
        .and_then(|t| *t)
        .context("missing luma quantization table")?;
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (bw, bh) = (w.div_ceil(8), h.div_ceil(8));
    if bw > y.blocks_wide || bh > y.blocks_high {
        bail!("luma block grid {}x{} is smaller than the image", y.blocks_wide, y.blocks_high);
    }
    let block = |bx: usize, by: usize| &y.coeffs[(by * y.blocks_wide + bx) * 64..(by * y.blocks_wide + bx + 1) * 64];
    let blocks = (0..bh).flat_map(|by| (0..bw).map(move |bx| (bx, by))).map(|(bx, by)| block(bx, by));
    Ok(write_gray_jpeg(frame.width, frame.height, &qt, blocks))
}

/// Writes a baseline single-component JPEG from zigzag-ordered blocks, with
/// optimized Huffman tables.
fn write_gray_jpeg<'a>(width: u16, height: u16, quant_natural: &[u16; 64], blocks: impl Iterator<Item = &'a [i16]> + Clone) -> Vec<u8> {
    // Pass 1: symbol statistics.
    let mut dc_freq = [0u32; 257];
    let mut ac_freq = [0u32; 257];
    let mut prev = 0i32;
    for b in blocks.clone() {
        let diff = b[0] as i32 - prev;
        prev = b[0] as i32;
        dc_freq[bit_size(diff) as usize] += 1;
        for_each_ac(b, |symbol, _, _| ac_freq[symbol as usize] += 1);
    }
    let dc = HuffmanTable::optimal(&dc_freq);
    let ac = HuffmanTable::optimal(&ac_freq);

    let mut out = vec![0xFF, 0xD8];
    // Quantization table, zigzag order; 16-bit entries need an extended frame.
    let wide = quant_natural.iter().any(|&q| q > 255);
    out.extend_from_slice(&[0xFF, 0xDB]);
    out.extend_from_slice(&(if wide { 131u16 } else { 67u16 }).to_be_bytes());
    out.push(if wide { 0x10 } else { 0x00 });
    for &n in &ZIGZAG_TO_NATURAL {
        if wide {
            out.extend_from_slice(&quant_natural[n].to_be_bytes());
        } else {
            out.push(quant_natural[n] as u8);
        }
    }
    out.extend_from_slice(&[0xFF, if wide { 0xC1 } else { 0xC0 }, 0, 11, 8]);
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&[1, 1, 0x11, 0]);
    dc.write_dht(&mut out, 0x00);
    ac.write_dht(&mut out, 0x10);
    out.extend_from_slice(&[0xFF, 0xDA, 0, 8, 1, 1, 0x00, 0, 63, 0]);

    // Pass 2: entropy-coded data.
    let mut bits = BitWriter::new(&mut out);
    let mut prev = 0i32;
    for b in blocks {
        let diff = b[0] as i32 - prev;
        prev = b[0] as i32;
        let size = bit_size(diff);
        dc.emit(&mut bits, size as u8);
        bits.put(magnitude_bits(diff, size), size);
        for_each_ac(b, |symbol, value, size| {
            ac.emit(&mut bits, symbol);
            if size > 0 {
                bits.put(magnitude_bits(value, size), size);
            }
        });
    }
    bits.finish();
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}

/// Calls `f(symbol, value, size)` for each AC symbol of a zigzag block:
/// run/size pairs, ZRL (0xF0) and EOB (0x00).
fn for_each_ac(block: &[i16], mut f: impl FnMut(u8, i32, u32)) {
    let mut run = 0u8;
    for &c in &block[1..64] {
        if c == 0 {
            run += 1;
            continue;
        }
        while run > 15 {
            f(0xF0, 0, 0);
            run -= 16;
        }
        let size = bit_size(c as i32);
        f((run << 4) | size as u8, c as i32, size);
        run = 0;
    }
    if run > 0 {
        f(0x00, 0, 0);
    }
}

fn bit_size(v: i32) -> u32 {
    32 - v.unsigned_abs().leading_zeros()
}

/// JPEG's magnitude bits: the value itself, or `value - 1` for negatives.
fn magnitude_bits(v: i32, size: u32) -> u32 {
    let v = if v < 0 { v - 1 } else { v };
    (v as u32) & ((1u32 << size) - 1)
}

struct HuffmanTable {
    /// Number of codes of each length 1..=16.
    counts: [u8; 16],
    symbols: Vec<u8>,
    /// Code and length of each symbol.
    codes: [(u16, u8); 256],
}

impl HuffmanTable {
    /// Code lengths per JPEG Annex K.2, limited to 16 bits (K.3).
    fn optimal(freq_in: &[u32; 257]) -> HuffmanTable {
        let mut freq: Vec<i64> = freq_in.iter().map(|&f| f as i64).collect();
        freq[256] = 1; // Reserves the all-ones code.
        let mut size = [0usize; 257];
        let mut others = [-1i32; 257];
        loop {
            let mut v1: i32 = -1;
            for i in 0..257 {
                if freq[i] > 0 && (v1 < 0 || freq[i] <= freq[v1 as usize]) {
                    v1 = i as i32;
                }
            }
            let mut v2: i32 = -1;
            for i in 0..257 {
                if freq[i] > 0 && i as i32 != v1 && (v2 < 0 || freq[i] <= freq[v2 as usize]) {
                    v2 = i as i32;
                }
            }
            if v2 < 0 {
                break;
            }
            let (mut a, mut b) = (v1 as usize, v2 as usize);
            freq[a] += freq[b];
            freq[b] = 0;
            size[a] += 1;
            while others[a] >= 0 {
                a = others[a] as usize;
                size[a] += 1;
            }
            others[a] = b as i32;
            size[b] += 1;
            while others[b] >= 0 {
                b = others[b] as usize;
                size[b] += 1;
            }
        }
        let mut bits = [0i32; 33];
        for &s in &size {
            if s > 0 {
                bits[s] += 1;
            }
        }
        let mut i = 32;
        while i > 16 {
            while bits[i] > 0 {
                let mut j = i - 2;
                while bits[j] == 0 {
                    j -= 1;
                }
                bits[i] -= 2;
                bits[i - 1] += 1;
                bits[j + 1] += 2;
                bits[j] -= 1;
            }
            i -= 1;
        }
        while bits[i] == 0 {
            i -= 1;
        }
        bits[i] -= 1; // Drop the reserved code.

        let mut order: Vec<usize> = (0..256).filter(|&s| size[s] > 0).collect();
        order.sort_by_key(|&s| (size[s], s));
        let mut counts = [0u8; 16];
        for (l, c) in counts.iter_mut().enumerate() {
            *c = bits[l + 1] as u8;
        }
        // Lengths after limiting: assign in order of the original lengths.
        let mut lengths = Vec::with_capacity(order.len());
        for (l, &c) in counts.iter().enumerate() {
            lengths.extend(std::iter::repeat_n(l as u8 + 1, c as usize));
        }
        let mut codes = [(0u16, 0u8); 256];
        let mut code = 0u32;
        let mut last_len = 0u8;
        for (k, &s) in order.iter().enumerate() {
            let len = lengths[k];
            code <<= len - last_len;
            last_len = len;
            codes[s] = (code as u16, len);
            code += 1;
        }
        HuffmanTable { counts, symbols: order.iter().map(|&s| s as u8).collect(), codes }
    }

    fn write_dht(&self, out: &mut Vec<u8>, class_and_id: u8) {
        out.extend_from_slice(&[0xFF, 0xC4]);
        out.extend_from_slice(&((3 + 16 + self.symbols.len()) as u16).to_be_bytes());
        out.push(class_and_id);
        out.extend_from_slice(&self.counts);
        out.extend_from_slice(&self.symbols);
    }

    fn emit(&self, bits: &mut BitWriter, symbol: u8) {
        let (code, len) = self.codes[symbol as usize];
        debug_assert!(len > 0, "symbol {symbol:#x} has no code");
        bits.put(code as u32, len as u32);
    }
}

struct BitWriter<'a> {
    out: &'a mut Vec<u8>,
    acc: u64,
    n: u32,
}

impl<'a> BitWriter<'a> {
    fn new(out: &'a mut Vec<u8>) -> Self {
        BitWriter { out, acc: 0, n: 0 }
    }

    fn put(&mut self, value: u32, size: u32) {
        if size == 0 {
            return;
        }
        self.acc = (self.acc << size) | (value as u64 & ((1u64 << size) - 1));
        self.n += size;
        while self.n >= 8 {
            self.n -= 8;
            let byte = (self.acc >> self.n) as u8;
            self.out.push(byte);
            if byte == 0xFF {
                self.out.push(0x00);
            }
        }
    }

    /// Pads the last byte with ones.
    fn finish(&mut self) {
        if self.n > 0 {
            let pad = 8 - self.n;
            self.put((1 << pad) - 1, pad);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pixel::Raster;

    #[test]
    fn luma_coefficients_survive_exactly() {
        let (w, h) = (93u32, 61u32);
        let data: Vec<u8> = (0..w * h).flat_map(|i| [(i * 7 % 256) as u8, (i * 3 % 256) as u8, (i % 97) as u8]).collect();
        let rgb = Raster::new(w, h, 3, data).unwrap();
        let mut color = crate::encode::encode_jpeg(&rgb, 80).unwrap().data;
        if color.first() != Some(&0xFF) {
            let mut raw = Vec::new();
            std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(&color[..]), &mut raw).unwrap();
            color = raw;
        }
        let gray = luma_only(&color).unwrap();
        let dec = zenjpeg::decoder::DecodeConfig::new();
        let a = dec.decode_coefficients(&color, Unstoppable).unwrap();
        let b = dec.decode_coefficients(&gray, Unstoppable).unwrap();
        assert_eq!(b.components.len(), 1);
        let (ya, yb) = (&a.components[0], &b.components[0]);
        for by in 0..(h as usize).div_ceil(8) {
            for bx in 0..(w as usize).div_ceil(8) {
                assert_eq!(ya.block_at(bx, by), yb.block_at(bx, by), "block {bx},{by}");
            }
        }
        let pixels = crate::encode::decode_jpeg(&gray, 1).unwrap();
        assert_eq!((pixels.width, pixels.height), (w, h));
    }
}
