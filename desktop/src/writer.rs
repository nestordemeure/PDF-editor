//! Streaming PDF writer: each object goes to disk as soon as it is ready,
//! so memory does not grow with the document.

use anyhow::{Context, Result, bail};
use lopdf::{Dictionary, Object, StringFormat};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

pub struct PdfWriter {
    out: BufWriter<File>,
    pos: u64,
    /// Where each object is; index 0 is the free-list head.
    entries: Vec<Entry>,
    /// Non-stream objects waiting to be packed into an object stream.
    packed: Vec<(OutId, Vec<u8>)>,
    packed_bytes: usize,
}

#[derive(Clone, Copy)]
enum Entry {
    Unwritten,
    Offset(u64),
    /// Object stream number and index inside it.
    Packed(OutId, u32),
}

/// Object number in the output file (generation is always 0).
pub type OutId = u32;

/// Objects per object stream: big enough to compress well, small enough
/// to keep memory flat.
const PACK_OBJECTS: usize = 200;
const PACK_BYTES: usize = 256 * 1024;

pub fn reference(id: OutId) -> Object {
    Object::Reference((id, 0))
}

impl PdfWriter {
    pub fn create(path: &Path) -> Result<Self> {
        let file = File::create(path).with_context(|| format!("cannot create {}", path.display()))?;
        let mut w = PdfWriter {
            out: BufWriter::with_capacity(1 << 20, file),
            pos: 0,
            entries: vec![Entry::Unwritten],
            packed: Vec::new(),
            packed_bytes: 0,
        };
        w.raw(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n")?;
        Ok(w)
    }

    fn raw(&mut self, bytes: &[u8]) -> Result<()> {
        self.out.write_all(bytes)?;
        self.pos += bytes.len() as u64;
        Ok(())
    }

    pub fn alloc(&mut self) -> OutId {
        self.entries.push(Entry::Unwritten);
        (self.entries.len() - 1) as OutId
    }

    fn claim(&mut self, id: OutId) -> Result<()> {
        if !matches!(self.entries[id as usize], Entry::Unwritten) {
            bail!("object {id} written twice");
        }
        Ok(())
    }

    /// Writes object `id`. References inside must already be output ids.
    /// Streams go to disk at once; other objects are packed into object
    /// streams.
    pub fn write(&mut self, id: OutId, object: &Object) -> Result<()> {
        self.claim(id)?;
        match object {
            Object::Stream(stream) => self.write_raw_stream(id, &stream.dict, &stream.content),
            other => {
                let mut buf = Vec::new();
                serialize(other, &mut buf);
                self.packed_bytes += buf.len();
                self.packed.push((id, buf));
                // Mark as taken until the pack is written.
                self.entries[id as usize] = Entry::Packed(0, 0);
                if self.packed.len() >= PACK_OBJECTS || self.packed_bytes >= PACK_BYTES {
                    self.flush_packed()?;
                }
                Ok(())
            }
        }
    }

    pub fn write_stream(&mut self, id: OutId, dict: Dictionary, content: Vec<u8>) -> Result<()> {
        self.claim(id)?;
        self.write_raw_stream(id, &dict, &content)
    }

    fn write_raw_stream(&mut self, id: OutId, dict: &Dictionary, content: &[u8]) -> Result<()> {
        self.entries[id as usize] = Entry::Offset(self.pos);
        let mut dict = dict.clone();
        dict.set("Length", Object::Integer(content.len() as i64));
        let mut buf = format!("{id} 0 obj\n").into_bytes();
        serialize(&Object::Dictionary(dict), &mut buf);
        buf.extend_from_slice(b"\nstream\n");
        self.raw(&buf)?;
        self.raw(content)?;
        self.raw(b"\nendstream\nendobj\n")
    }

    fn flush_packed(&mut self) -> Result<()> {
        if self.packed.is_empty() {
            return Ok(());
        }
        let stream_id = self.alloc();
        let packed = std::mem::take(&mut self.packed);
        self.packed_bytes = 0;
        let mut header = String::new();
        let mut body = Vec::new();
        for (index, (id, bytes)) in packed.iter().enumerate() {
            header.push_str(&format!("{id} {} ", body.len()));
            body.extend_from_slice(bytes);
            body.push(b'\n');
            self.entries[*id as usize] = Entry::Packed(stream_id, index as u32);
        }
        let mut data = header.into_bytes();
        let first = data.len();
        data.extend_from_slice(&body);
        let mut dict = Dictionary::new();
        dict.set("Type", Object::Name(b"ObjStm".to_vec()));
        dict.set("N", Object::Integer(packed.len() as i64));
        dict.set("First", Object::Integer(first as i64));
        dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
        self.write_raw_stream(stream_id, &dict, &crate::encode::zlib(&data))
    }

    /// Writes the cross-reference stream, and flushes the file.
    pub fn finish(mut self, root: OutId, info: Option<OutId>) -> Result<()> {
        self.flush_packed()?;
        let xref_id = self.alloc();
        if let Some(missing) = self.entries.iter().enumerate().skip(1).position(|(i, e)| matches!(e, Entry::Unwritten) && i != xref_id as usize) {
            bail!("object {} was allocated but never written", missing + 1);
        }
        let xref_pos = self.pos;
        self.entries[xref_id as usize] = Entry::Offset(xref_pos);
        let mut table = Vec::with_capacity(self.entries.len() * 8);
        for (i, entry) in self.entries.iter().enumerate() {
            let (kind, field2, field3): (u8, u64, u16) = match *entry {
                _ if i == 0 => (0, 0, 65535),
                Entry::Offset(offset) => (1, offset, 0),
                Entry::Packed(stream, index) => (2, stream as u64, index as u16),
                Entry::Unwritten => unreachable!(),
            };
            table.push(kind);
            table.extend_from_slice(&field2.to_be_bytes()[3..]);
            table.extend_from_slice(&field3.to_be_bytes());
        }
        let mut dict = Dictionary::new();
        dict.set("Type", Object::Name(b"XRef".to_vec()));
        dict.set("Size", Object::Integer(self.entries.len() as i64));
        dict.set("W", Object::Array(vec![1.into(), 5.into(), 2.into()]));
        dict.set("Root", reference(root));
        if let Some(info) = info {
            dict.set("Info", reference(info));
        }
        dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
        self.write_raw_stream(xref_id, &dict, &crate::encode::zlib(&table))?;
        self.raw(format!("startxref\n{xref_pos}\n%%EOF\n").as_bytes())?;
        self.out.flush()?;
        Ok(())
    }
}

pub fn serialize(object: &Object, out: &mut Vec<u8>) {
    match object {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Boolean(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Object::Integer(i) => out.extend_from_slice(i.to_string().as_bytes()),
        Object::Real(r) => out.extend_from_slice(format_number(*r as f64).as_bytes()),
        Object::Name(name) => write_name(name, out),
        Object::String(s, StringFormat::Hexadecimal) => {
            out.push(b'<');
            for b in s {
                out.extend_from_slice(format!("{b:02X}").as_bytes());
            }
            out.push(b'>');
        }
        Object::String(s, StringFormat::Literal) => {
            out.push(b'(');
            for &b in s {
                match b {
                    b'(' | b')' | b'\\' => out.extend_from_slice(&[b'\\', b]),
                    b'\r' => out.extend_from_slice(b"\\r"),
                    _ => out.push(b),
                }
            }
            out.push(b')');
        }
        Object::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                serialize(item, out);
            }
            out.push(b']');
        }
        Object::Dictionary(dict) => {
            out.extend_from_slice(b"<<");
            for (key, value) in dict.iter() {
                write_name(key, out);
                out.push(b' ');
                serialize(value, out);
            }
            out.extend_from_slice(b">>");
        }
        Object::Stream(_) => panic!("streams are only valid as top-level objects"),
        Object::Reference((id, _)) => out.extend_from_slice(format!("{id} 0 R").as_bytes()),
    }
}

fn write_name(name: &[u8], out: &mut Vec<u8>) {
    out.push(b'/');
    for &b in name {
        let regular = b.is_ascii_graphic() && !b"()<>[]{}/%#".contains(&b);
        if regular {
            out.push(b);
        } else {
            out.extend_from_slice(format!("#{b:02X}").as_bytes());
        }
    }
}

/// Shortest decimal form with at most 4 decimals, no exponent.
pub fn format_number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_compact() {
        assert_eq!(format_number(1.0), "1");
        assert_eq!(format_number(0.5), "0.5");
        assert_eq!(format_number(-0.00001), "0");
        assert_eq!(format_number(612.123456), "612.1235");
    }

    #[test]
    fn names_escape_delimiters() {
        let mut out = Vec::new();
        write_name(b"A B#", &mut out);
        assert_eq!(out, b"/A#20B#23");
    }
}
