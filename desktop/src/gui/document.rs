//! The edited document: loaded sources, the page list, selection and
//! undo history. No UI code here.

use pdf_editor::model::{ColorMode, Page, PageKind, Side};
use pdf_editor::source::Source;
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub struct PageEntry {
    /// Stable identity across edits (for selection and widgets).
    pub id: u64,
    pub page: Page,
}

#[derive(Default)]
pub struct Document {
    pub sources: Vec<Arc<Source>>,
    pub pages: Vec<PageEntry>,
    pub selected: HashSet<u64>,
    undo: Vec<Vec<PageEntry>>,
    redo: Vec<Vec<PageEntry>>,
    next_id: u64,
    /// Changes whenever the pages change.
    pub revision: u64,
}

impl Document {
    fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Records the current pages before a change.
    fn checkpoint(&mut self) {
        self.undo.push(self.pages.clone());
        self.redo.clear();
        self.revision += 1;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) {
        if let Some(previous) = self.undo.pop() {
            self.revision += 1;
            self.redo.push(std::mem::replace(&mut self.pages, previous));
            self.prune_selection();
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.revision += 1;
            self.undo.push(std::mem::replace(&mut self.pages, next));
            self.prune_selection();
        }
    }

    fn prune_selection(&mut self) {
        let ids: HashSet<u64> = self.pages.iter().map(|p| p.id).collect();
        self.selected.retain(|id| ids.contains(id));
    }

    /// Appends the pages of a newly loaded source.
    pub fn add_source(&mut self, source: Source, kinds: Vec<PageKind>) {
        self.checkpoint();
        let index = self.sources.len();
        self.sources.push(Arc::new(source));
        for (i, kind) in kinds.into_iter().enumerate() {
            let id = self.new_id();
            self.pages.push(PageEntry { id, page: Page::new(index, i, kind) });
        }
    }

    pub fn is_selected(&self, id: u64) -> bool {
        self.selected.contains(&id)
    }

    pub fn toggle(&mut self, id: u64) {
        if !self.selected.remove(&id) {
            self.selected.insert(id);
        }
    }

    pub fn select_range(&mut self, from: u64, to: u64) {
        let pos = |id| self.pages.iter().position(|p| p.id == id);
        let (Some(a), Some(b)) = (pos(from), pos(to)) else { return };
        let ids: Vec<u64> = self.pages[a.min(b)..=a.max(b)].iter().map(|p| p.id).collect();
        self.selected.extend(ids);
    }

    pub fn set_all_selected(&mut self, selected: bool) {
        self.selected = if selected { self.pages.iter().map(|p| p.id).collect() } else { HashSet::new() };
    }

    pub fn all_selected(&self) -> bool {
        !self.pages.is_empty() && self.selected.len() == self.pages.len()
    }

    /// Applies `f` to every selected page; records one undo step if
    /// anything changed.
    fn edit_selected(&mut self, f: impl Fn(&mut Page)) {
        let mut pages = self.pages.clone();
        for entry in pages.iter_mut().filter(|e| self.selected.contains(&e.id)) {
            f(&mut entry.page);
        }
        if pages != self.pages {
            self.checkpoint();
            self.pages = pages;
        }
    }

    pub fn rotate_selected(&mut self) {
        self.edit_selected(|p| p.view = p.view.rotated_cw());
    }

    pub fn set_color_selected(&mut self, mode: ColorMode) {
        self.edit_selected(|p| p.color = mode);
    }

    pub fn remove_shading_selected(&mut self) {
        self.edit_selected(|p| p.remove_shading = true);
    }

    pub fn enhance_contrast_selected(&mut self) {
        self.edit_selected(|p| p.enhance_contrast = true);
    }

    /// Splits the selected pages. Returns (old id, left half id) pairs.
    pub fn split_selected(&mut self) -> Vec<(u64, u64)> {
        let mut halves = Vec::new();
        if self.selected.is_empty() {
            return halves;
        }
        self.checkpoint();
        let old = std::mem::take(&mut self.pages);
        for entry in old {
            if self.selected.contains(&entry.id) {
                let (left, right) = (self.new_id(), self.new_id());
                halves.push((entry.id, left));
                let mut l = entry.page.clone();
                l.view = l.view.split(Side::Left);
                let mut r = entry.page;
                r.view = r.view.split(Side::Right);
                self.pages.push(PageEntry { id: left, page: l });
                self.pages.push(PageEntry { id: right, page: r });
            } else {
                self.pages.push(entry);
            }
        }
        self.selected.clear();
        halves
    }

    pub fn delete_selected(&mut self) {
        if self.selected.is_empty() {
            return;
        }
        self.checkpoint();
        self.pages.retain(|e| !self.selected.contains(&e.id));
        self.selected.clear();
    }

    /// Moves the page at `from` so it ends up before the page now at `to`.
    pub fn move_page(&mut self, from: usize, to: usize) {
        if from >= self.pages.len() || to > self.pages.len() || to == from || to == from + 1 {
            return;
        }
        self.checkpoint();
        let entry = self.pages.remove(from);
        let to = if to > from { to - 1 } else { to };
        self.pages.insert(to, entry);
    }

    pub fn page_list(&self) -> Vec<Page> {
        self.pages.iter().map(|e| e.page.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_with(n: usize) -> Document {
        let mut d = Document::default();
        for i in 0..n {
            let id = d.new_id();
            d.pages.push(PageEntry { id, page: Page::new(0, i, PageKind::Scan) });
        }
        d
    }

    #[test]
    fn split_then_undo_restores_pages() {
        let mut d = doc_with(3);
        d.selected.insert(d.pages[1].id);
        d.split_selected();
        assert_eq!(d.pages.len(), 4);
        d.undo();
        assert_eq!(d.pages.len(), 3);
        d.redo();
        assert_eq!(d.pages.len(), 4);
    }

    #[test]
    fn move_page_places_before_target() {
        let mut d = doc_with(4);
        let ids: Vec<u64> = d.pages.iter().map(|p| p.id).collect();
        d.move_page(0, 3);
        let now: Vec<u64> = d.pages.iter().map(|p| p.id).collect();
        assert_eq!(now, vec![ids[1], ids[2], ids[0], ids[3]]);
        d.move_page(3, 0);
        assert_eq!(d.pages[0].id, ids[3]);
    }

    #[test]
    fn edits_without_change_do_not_add_history() {
        let mut d = doc_with(2);
        d.set_all_selected(true);
        d.set_color_selected(ColorMode::Gray);
        assert!(!d.can_undo());
        d.set_color_selected(ColorMode::BwFlat);
        assert!(d.can_undo());
    }
}
