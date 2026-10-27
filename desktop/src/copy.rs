//! Copies object graphs from a source document into the output.

use crate::writer::{OutId, PdfWriter, reference};
use anyhow::Result;
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use std::collections::HashMap;

/// Keys never copied from a page dictionary: the output builds its own page
/// tree, and thumbnails and article beads point at the old document.
const DROPPED_PAGE_KEYS: [&[u8]; 4] = [b"Parent", b"Thumb", b"B", b"StructParents"];

/// Copies objects of one source, each at most once.
pub struct Copier<'a> {
    src: &'a Document,
    ids: HashMap<ObjectId, OutId>,
    pending: Vec<ObjectId>,
    /// Source pages that are in the output, and their output ids: links
    /// to them keep working.
    pub page_targets: HashMap<ObjectId, OutId>,
    /// Streams to write instead of the source object.
    pub replacements: HashMap<ObjectId, Object>,
}

impl<'a> Copier<'a> {
    pub fn new(src: &'a Document) -> Self {
        Copier { src, ids: HashMap::new(), pending: Vec::new(), page_targets: HashMap::new(), replacements: HashMap::new() }
    }

    pub fn source(&self) -> &'a Document {
        self.src
    }

    pub fn map_ref(&mut self, w: &mut PdfWriter, id: ObjectId) -> OutId {
        if let Some(&target) = self.page_targets.get(&id) {
            return target;
        }
        if let Some(&out) = self.ids.get(&id) {
            return out;
        }
        let out = w.alloc();
        self.ids.insert(id, out);
        self.pending.push(id);
        out
    }

    /// Converts an object, mapping its references (and queueing them).
    pub fn convert(&mut self, w: &mut PdfWriter, object: &Object) -> Object {
        match object {
            Object::Reference(id) => reference(self.map_ref(w, *id)),
            Object::Array(items) => Object::Array(items.iter().map(|o| self.convert(w, o)).collect()),
            Object::Dictionary(dict) => Object::Dictionary(self.convert_dict(w, dict)),
            Object::Stream(stream) => {
                Object::Stream(Stream::new(self.convert_dict(w, &stream.dict), stream.content.clone()))
            }
            other => other.clone(),
        }
    }

    pub fn convert_dict(&mut self, w: &mut PdfWriter, dict: &Dictionary) -> Dictionary {
        let mut out = Dictionary::new();
        for (key, value) in dict.iter() {
            out.set(key.clone(), self.convert(w, value));
        }
        out
    }

    /// Converts a page dictionary for the output page tree.
    pub fn convert_page(&mut self, w: &mut PdfWriter, page: &Dictionary) -> Dictionary {
        let mut out = Dictionary::new();
        for (key, value) in page.iter() {
            if !DROPPED_PAGE_KEYS.contains(&key.as_slice()) {
                out.set(key.clone(), self.convert(w, value));
            }
        }
        out
    }

    /// Writes every queued object.
    pub fn flush(&mut self, w: &mut PdfWriter) -> Result<()> {
        while let Some(id) = self.pending.pop() {
            let out = self.ids[&id];
            if let Some(replacement) = self.replacements.get(&id).cloned() {
                let converted = self.convert(w, &replacement);
                w.write(out, &converted)?;
                continue;
            }
            let object = match self.src.get_object(id) {
                Ok(o) => o,
                // A dangling reference is null by definition.
                Err(_) => &Object::Null,
            };
            if is_document_structure(object) {
                // Pages not in the output, page tree nodes and catalogs
                // would drag the whole source document along.
                w.write(out, &Object::Null)?;
                continue;
            }
            let converted = self.convert(w, object);
            w.write(out, &converted)?;
        }
        Ok(())
    }
}

fn is_document_structure(object: &Object) -> bool {
    let dict = match object {
        Object::Dictionary(d) => d,
        _ => return false,
    };
    matches!(dict.get(b"Type").and_then(Object::as_name), Ok(b"Page" | b"Pages" | b"Catalog"))
}
