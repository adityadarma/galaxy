//! IME composition (marked text) for `NSTextInputClient` (M1-10).
//!
//! While an input method is composing, its preview text (the underlined
//! "marked text") lives in the document like any other text, so layout and
//! caret math need no special cases. The composition remembers the marked
//! range and the inverse of every replacement made so far, and when it ends
//! (commit, unmark, or any other edit) the whole sequence is recorded as
//! **one** undo step. Undo therefore removes a committed word in one go
//! instead of replaying each keystroke of the preedit.
//!
//! Positions crossing this API are document-wide UTF-16 offsets, the unit
//! of `NSRange` in `NSTextInputClient`.
//!
//! Compositions act on the primary selection only; other cursors are
//! dropped for the duration (VSCode does the same).

use crate::change::{Change, ChangeSet};
use crate::history::Editor;
use crate::selection::{Selection, SelectionSet};

#[derive(Debug, Clone)]
pub(crate) struct Composition {
    /// Start of the marked text, in chars.
    start: usize,
    /// Length of the marked text, in chars. Always > 0 while active.
    len: usize,
    /// Selections before the composition began, restored by undo.
    sel_before: SelectionSet,
    /// Inverse of each replacement, in application order.
    inverses: Vec<ChangeSet>,
    /// `true` while the composition has only ever inserted text (it began
    /// at an empty range and never used a replacement range). Cancelling
    /// such a composition leaves the document untouched, so it must not
    /// leave an undo step behind.
    insert_only: bool,
}

impl Editor {
    /// `true` while marked text is present.
    pub fn has_marked_text(&self) -> bool {
        self.composition.is_some()
    }

    /// Marked range as document-wide UTF-16 `(location, length)`.
    pub fn marked_range_utf16(&self) -> Option<(usize, usize)> {
        let c = self.composition.as_ref()?;
        let start = self.doc.char_to_utf16_offset(c.start);
        let end = self.doc.char_to_utf16_offset(c.start + c.len);
        Some((start, end - start))
    }

    /// Marked range in chars, for rendering the underline.
    pub fn marked_range_chars(&self) -> Option<(usize, usize)> {
        self.composition
            .as_ref()
            .map(|c| (c.start, c.start + c.len))
    }

    /// Primary selection as document-wide UTF-16 `(location, length)`.
    pub fn selected_range_utf16(&self) -> (usize, usize) {
        let p = self.sel.primary();
        let start = self.doc.char_to_utf16_offset(p.start());
        let end = self.doc.char_to_utf16_offset(p.end());
        (start, end - start)
    }

    /// Text in a UTF-16 range (clamped), for `attributedSubstring`.
    pub fn text_utf16(&self, location: usize, length: usize) -> String {
        let a = self.doc.utf16_offset_to_char(location);
        let b = self
            .doc
            .utf16_offset_to_char(location.saturating_add(length));
        self.doc.rope().slice(a..b.max(a)).to_string()
    }

    /// `setMarkedText`: replaces the marked range (or `replacement`, or the
    /// selection when nothing is marked) with `text` and keeps it marked.
    /// `inner` is the selection inside `text` in UTF-16 units `(location,
    /// length)`, which is where the IME wants the caret. Empty `text`
    /// cancels the composition.
    pub fn set_marked_text(
        &mut self,
        text: &str,
        inner: (usize, usize),
        replacement: Option<(usize, usize)>,
    ) {
        let (a, b) = self.target_range(replacement);
        if self.composition.is_none() {
            self.composition = Some(Composition {
                start: a,
                len: 0,
                sel_before: self.sel.clone(),
                inverses: Vec::new(),
                insert_only: a == b,
            });
        } else if replacement.is_some()
            && let Some(c) = self.composition.as_mut()
        {
            c.insert_only = false;
        }
        let change = Change::new(a, b, text);
        let new_len = change.inserted_len();
        let inverse = self
            .doc
            .apply(&ChangeSet::new(vec![change], self.doc.len_chars()));
        if let Some(c) = self.composition.as_mut() {
            c.inverses.push(inverse);
            c.start = a;
            c.len = new_len;
        }
        if new_len == 0 {
            self.end_composition(SelectionSet::single(Selection::caret(a)));
            return;
        }
        let (c0, c1) = inner_to_chars(text, inner);
        let (c0, c1) = (c0.min(new_len), c1.min(new_len));
        self.sel = SelectionSet::single(Selection::new(a + c0, a + c1));
    }

    /// `insertText(_:replacementRange:)` for composed or replaced text:
    /// replaces the marked range (or `replacement`, or the selection) with
    /// `text`, ends the composition, and records one undo step.
    ///
    /// Plain typing without marked text or a replacement range should use
    /// `edit::insert_text` instead so consecutive keystrokes group.
    pub fn commit_text(&mut self, text: &str, replacement: Option<(usize, usize)>) {
        let (a, b) = self.target_range(replacement);
        let (sel_before, mut inverses) = match self.composition.take() {
            Some(c) => (c.sel_before, c.inverses),
            None => (self.sel.clone(), Vec::new()),
        };
        let change = Change::new(a, b, text);
        let inserted = change.inserted_len();
        inverses.push(
            self.doc
                .apply(&ChangeSet::new(vec![change], self.doc.len_chars())),
        );
        let after = SelectionSet::single(Selection::caret(a + inserted));
        self.sel = after.clone();
        self.history.record_group(inverses, sel_before, after);
    }

    /// `unmarkText`: accepts the marked text as it is.
    pub fn unmark_text(&mut self) {
        if self.composition.is_some() {
            let after = self.sel.clone();
            self.end_composition(after);
        }
    }

    fn end_composition(&mut self, after: SelectionSet) {
        if let Some(c) = self.composition.take() {
            self.sel = after.clone();
            // Cancelled insert-only composition: net no-op, no undo step.
            if c.len == 0 && c.insert_only {
                return;
            }
            self.history.record_group(c.inverses, c.sel_before, after);
        }
    }

    /// Char range to replace: `replacement` if given, else the marked
    /// range, else the primary selection.
    fn target_range(&self, replacement: Option<(usize, usize)>) -> (usize, usize) {
        let len = self.doc.len_chars();
        if let Some((location, length)) = replacement {
            let a = self.doc.utf16_offset_to_char(location);
            let b = self
                .doc
                .utf16_offset_to_char(location.saturating_add(length));
            return (a.min(len), b.max(a).min(len));
        }
        if let Some(c) = &self.composition {
            return (c.start.min(len), (c.start + c.len).min(len));
        }
        let p = self.sel.primary();
        (p.start(), p.end())
    }
}

/// Converts a UTF-16 `(location, length)` inside `text` to char offsets.
/// Offsets inside a surrogate pair round down.
fn inner_to_chars(text: &str, (location, length): (usize, usize)) -> (usize, usize) {
    let to_char = |target: usize| {
        let mut units = 0;
        for (i, c) in text.chars().enumerate() {
            if units + c.len_utf16() > target {
                return i;
            }
            units += c.len_utf16();
        }
        text.chars().count()
    };
    (to_char(location), to_char(location.saturating_add(length)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use crate::edit::{self, IndentConfig};
    use crate::history::EditKind;
    use std::time::Instant;

    fn editor(text: &str, caret: usize) -> Editor {
        let mut ed = Editor::new(Document::from_text(text));
        ed.sel = SelectionSet::single(Selection::caret(caret));
        ed
    }

    #[test]
    fn japanese_composition_is_one_undo_step() {
        let mut ed = editor("", 0);
        // Kotoeri: k -> ka -> か -> かん -> 漢 (converted) -> commit.
        for preedit in ["k", "ka", "か", "かん"] {
            let n = preedit.encode_utf16().count();
            ed.set_marked_text(preedit, (n, 0), None);
            assert_eq!(ed.doc.text(), preedit);
            assert!(ed.has_marked_text());
        }
        ed.set_marked_text("漢", (1, 0), None);
        ed.commit_text("漢字", None);

        assert_eq!(ed.doc.text(), "漢字");
        assert!(!ed.has_marked_text());
        assert_eq!(ed.sel.primary(), Selection::caret(2));
        assert_eq!(ed.history.undo_len(), 1);

        assert!(ed.undo());
        assert_eq!(ed.doc.text(), "");
        assert!(ed.redo());
        assert_eq!(ed.doc.text(), "漢字");
        assert_eq!(ed.sel.primary(), Selection::caret(2));
    }

    #[test]
    fn composition_replaces_selection_and_undo_restores_it() {
        let mut ed = editor("abc", 0);
        ed.sel = SelectionSet::single(Selection::new(1, 2)); // "b"
        ed.set_marked_text("x", (1, 0), None);
        assert_eq!(ed.doc.text(), "axc");
        ed.commit_text("x", None);
        assert_eq!(ed.doc.text(), "axc");
        ed.undo();
        assert_eq!(ed.doc.text(), "abc");
        assert_eq!(ed.sel.primary(), Selection::new(1, 2));
    }

    #[test]
    fn empty_marked_text_cancels_without_history() {
        let mut ed = editor("hi", 2);
        ed.set_marked_text("ab", (2, 0), None);
        assert_eq!(ed.doc.text(), "hiab");
        ed.set_marked_text("", (0, 0), None);
        assert_eq!(ed.doc.text(), "hi");
        assert!(!ed.has_marked_text());
        assert!(!ed.history.can_undo());
        assert_eq!(ed.sel.primary(), Selection::caret(2));
    }

    #[test]
    fn inner_selection_positions_the_caret_in_utf16() {
        let mut ed = editor("", 0);
        // Caret after the emoji: 2 UTF-16 units into the marked text.
        ed.set_marked_text("😀x", (2, 0), None);
        assert_eq!(ed.sel.primary(), Selection::caret(1));
        // A highlighted clause inside the preedit.
        ed.set_marked_text("あいう", (1, 2), None);
        assert_eq!(ed.sel.primary(), Selection::new(1, 3));
        assert_eq!(ed.marked_range_utf16(), Some((0, 3)));
        // Out-of-range inner selection clamps instead of panicking.
        ed.set_marked_text("あ", (9, 9), None);
        assert_eq!(ed.sel.primary(), Selection::caret(1));
    }

    #[test]
    fn dead_key_accent_replaces_the_marked_accent() {
        let mut ed = editor("caf", 3);
        ed.set_marked_text("´", (1, 0), None); // Option+E
        assert_eq!(ed.doc.text(), "caf´");
        ed.commit_text("é", None); // then "e"
        assert_eq!(ed.doc.text(), "café");
        assert_eq!(ed.history.undo_len(), 1);
        ed.undo();
        assert_eq!(ed.doc.text(), "caf");
    }

    #[test]
    fn replacement_range_without_composition() {
        // Press-and-hold accent menu / emoji picker replace a range.
        let mut ed = editor("ab", 2);
        ed.commit_text("é", Some((1, 1)));
        assert_eq!(ed.doc.text(), "aé");
        assert_eq!(ed.sel.primary(), Selection::caret(2));
        ed.undo();
        assert_eq!(ed.doc.text(), "ab");
        // Range beyond the end clamps.
        ed.commit_text("!", Some((99, 5)));
        assert_eq!(ed.doc.text(), "ab!");
    }

    #[test]
    fn other_edits_commit_the_composition_first() {
        let mut ed = editor("", 0);
        ed.set_marked_text("あ", (1, 0), None);
        let tx = edit::newline(&ed.doc, &ed.sel, &IndentConfig::default());
        ed.apply(tx, EditKind::Other, Instant::now());
        assert!(!ed.has_marked_text());
        assert_eq!(ed.doc.text(), "あ\n");
        // Composition and newline are separate undo steps.
        assert_eq!(ed.history.undo_len(), 2);
        ed.undo();
        assert_eq!(ed.doc.text(), "あ");
        ed.undo();
        assert_eq!(ed.doc.text(), "");
    }

    #[test]
    fn undo_and_cursor_moves_commit_the_composition() {
        let mut ed = editor("x", 1);
        ed.set_marked_text("あ", (1, 0), None);
        assert!(ed.undo());
        assert_eq!(ed.doc.text(), "x");
        assert!(!ed.has_marked_text());

        ed.set_marked_text("い", (1, 0), None);
        ed.set_selections(SelectionSet::single(Selection::caret(0)));
        assert!(!ed.has_marked_text());
        assert_eq!(ed.doc.text(), "xい");
    }

    #[test]
    fn cancelling_over_a_selection_keeps_the_deletion_undoable() {
        // Composing over a selection replaces it; cancelling does not bring
        // the old text back, so that change must stay in history.
        let mut ed = editor("abc", 0);
        ed.sel = SelectionSet::single(Selection::new(1, 2));
        ed.set_marked_text("x", (1, 0), None);
        ed.set_marked_text("", (0, 0), None);
        assert_eq!(ed.doc.text(), "ac");
        assert_eq!(ed.history.undo_len(), 1);
        ed.undo();
        assert_eq!(ed.doc.text(), "abc");
    }

    #[test]
    fn unmark_keeps_text_and_selection() {
        let mut ed = editor("", 0);
        ed.set_marked_text("ab", (1, 0), None);
        ed.unmark_text();
        assert_eq!(ed.doc.text(), "ab");
        assert_eq!(ed.sel.primary(), Selection::caret(1));
        assert_eq!(ed.history.undo_len(), 1);
        ed.unmark_text(); // no-op
        assert_eq!(ed.history.undo_len(), 1);
    }

    #[test]
    fn multi_cursor_collapses_to_primary_and_undo_restores_all() {
        let doc = Document::from_text("a\nb\nc");
        let mut ed = Editor::new(doc);
        ed.sel = SelectionSet::new(
            vec![
                Selection::caret(1),
                Selection::caret(3),
                Selection::caret(5),
            ],
            1,
            &ed.doc,
        );
        let before = ed.sel.clone();
        ed.set_marked_text("あ", (1, 0), None);
        assert_eq!(ed.doc.text(), "a\nbあ\nc");
        assert_eq!(ed.sel.len(), 1);
        ed.commit_text("あ", None);
        ed.undo();
        assert_eq!(ed.doc.text(), "a\nb\nc");
        assert_eq!(ed.sel, before);
    }

    #[test]
    fn queries_use_document_wide_utf16_offsets() {
        let mut ed = editor("😀\nab", 2); // caret right after the newline
        ed.set_marked_text("あい", (2, 0), None);
        // "😀\nあい": 😀 = 2 units, \n = 1, so marked text starts at 3.
        assert_eq!(ed.marked_range_utf16(), Some((3, 2)));
        assert_eq!(ed.selected_range_utf16(), (5, 0));
        assert_eq!(ed.text_utf16(3, 2), "あい");
        assert_eq!(ed.text_utf16(0, 2), "😀");
        assert_eq!(ed.text_utf16(4, 99), "いab");
        assert_eq!(ed.text_utf16(99, 5), "");
    }

    #[test]
    fn marked_text_is_reported_in_the_viewport() {
        let mut ed = editor("ab\ncd", 2);
        ed.set_marked_text("あい", (2, 0), None);
        let snap = ed.viewport(0, 2);
        let span = snap.lines[0].marked.expect("marked span on line 0");
        assert_eq!((span.start, span.end), (2, 4));
        assert!(snap.lines[1].marked.is_none());
        ed.unmark_text();
        assert!(ed.viewport(0, 2).lines[0].marked.is_none());
    }
}
