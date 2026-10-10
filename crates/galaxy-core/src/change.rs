//! Changes and change sets (M1-03).
//!
//! A `Change` replaces the char range `start..end` of the *original*
//! document with `text`. A `ChangeSet` is a sorted, non-overlapping batch of
//! changes applied atomically, e.g. one keystroke across all cursors.
//!
//! This is the single mutation primitive of the core. Applying a set
//! returns its inverse, which is what undo (M1-04) stores, and the same
//! ranges feed LSP `didChange` (M4-04) and tree-sitter `InputEdit` (M3-02).

use crate::document::{Document, normalize_line_breaks};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    start: usize,
    end: usize,
    text: String,
    text_chars: usize,
}

impl Change {
    /// Replaces `start..end` (swapped if reversed) with `text`. Line breaks
    /// in `text` are normalized to LF.
    pub fn new(start: usize, end: usize, text: impl Into<String>) -> Self {
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let mut text: String = text.into();
        if text.contains('\r') {
            text = normalize_line_breaks(&text);
        }
        let text_chars = text.chars().count();
        Self {
            start,
            end,
            text,
            text_chars,
        }
    }

    pub fn insert(at: usize, text: impl Into<String>) -> Self {
        Self::new(at, at, text)
    }

    pub fn delete(start: usize, end: usize) -> Self {
        Self::new(start, end, String::new())
    }

    pub fn start(&self) -> usize {
        self.start
    }

    pub fn end(&self) -> usize {
        self.end
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Chars inserted.
    pub fn inserted_len(&self) -> usize {
        self.text_chars
    }

    /// Chars removed.
    pub fn removed_len(&self) -> usize {
        self.end - self.start
    }

    fn is_pure_insert(&self) -> bool {
        self.start == self.end
    }

    fn is_noop(&self) -> bool {
        self.start == self.end && self.text.is_empty()
    }

    fn delta(&self) -> isize {
        self.text_chars as isize - self.removed_len() as isize
    }
}

/// Which side of an insertion a mapped position sticks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assoc {
    /// Stay before text inserted at this position (selection starts).
    Before,
    /// Move after text inserted at this position (carets, selection ends).
    After,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeSet {
    changes: Vec<Change>,
    /// `prefix[i]` = net char delta of `changes[..i]`.
    prefix: Vec<isize>,
}

impl ChangeSet {
    /// Builds a set for a document of `doc_len` chars: clamps ranges, drops
    /// no-ops, sorts, and merges overlapping changes (their texts are
    /// concatenated in order, which is exact for deletions). Two pure
    /// insertions at the same point are merged the same way.
    pub fn new(mut changes: Vec<Change>, doc_len: usize) -> Self {
        for c in &mut changes {
            c.start = c.start.min(doc_len);
            c.end = c.end.min(doc_len);
        }
        changes.retain(|c| !c.is_noop());
        changes.sort_by_key(|c| (c.start, c.end));

        let mut merged: Vec<Change> = Vec::with_capacity(changes.len());
        for c in changes {
            if let Some(last) = merged.last_mut() {
                let overlaps = c.start < last.end;
                let same_point =
                    last.is_pure_insert() && c.is_pure_insert() && c.start == last.start;
                if overlaps || same_point {
                    let mut text = std::mem::take(&mut last.text);
                    text.push_str(&c.text);
                    *last = Change::new(last.start, last.end.max(c.end), text);
                    continue;
                }
            }
            merged.push(c);
        }
        Self::from_sorted(merged)
    }

    /// `changes` must already be sorted, clamped, and non-overlapping.
    pub(crate) fn from_sorted(changes: Vec<Change>) -> Self {
        let mut prefix = Vec::with_capacity(changes.len() + 1);
        let mut acc = 0isize;
        prefix.push(0);
        for c in &changes {
            acc += c.delta();
            prefix.push(acc);
        }
        Self { changes, prefix }
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.changes.len()
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Change> {
        self.changes.iter()
    }

    /// Approximate heap bytes held by this set, for undo memory limits.
    pub fn heap_size(&self) -> usize {
        let per_change = std::mem::size_of::<Change>() + std::mem::size_of::<isize>();
        self.changes
            .iter()
            .map(|c| c.text.capacity() + per_change)
            .sum()
    }

    /// Document length after applying this set to a document of `len`.
    pub fn len_after(&self, len: usize) -> usize {
        shift(len, *self.prefix.last().unwrap_or(&0))
    }

    /// Maps a char offset in the original document to the edited one.
    /// Positions inside a replaced range collapse to its start (`Before`)
    /// or the end of its new text (`After`). O(log n).
    pub fn map(&self, pos: usize, assoc: Assoc) -> usize {
        // Ends are non-decreasing because changes are sorted and disjoint.
        let first = self.changes.partition_point(|c| c.end < pos);
        let mut delta = self.prefix[first];
        for c in &self.changes[first..] {
            if c.start > pos {
                break;
            }
            let passed = c.end == pos && (!c.is_pure_insert() || assoc == Assoc::After);
            if passed {
                delta += c.delta();
                continue;
            }
            let new_start = shift(c.start, delta);
            return if pos > c.start && assoc == Assoc::After {
                new_start + c.text_chars
            } else {
                new_start
            };
        }
        shift(pos, delta)
    }
}

fn shift(pos: usize, delta: isize) -> usize {
    pos.checked_add_signed(delta).unwrap_or(0)
}

impl Document {
    /// Applies `changes` and returns the inverse set, which restores the
    /// previous text when applied to the result.
    pub fn apply(&mut self, changes: &ChangeSet) -> ChangeSet {
        if changes.is_empty() {
            // Nothing to do; keep `revision` (and the "modified" flag) still.
            return ChangeSet::default();
        }
        let len = self.len_chars();
        let mut inverse = Vec::with_capacity(changes.len());
        let mut delta = 0isize;
        for c in changes.iter() {
            // Sets are built against this document; clamp defensively in
            // case it changed in between.
            let (start, end) = (c.start.min(len), c.end.min(len));
            let removed = self.rope().slice(start..end).to_string();
            let new_start = shift(start, delta);
            inverse.push(Change::new(new_start, new_start + c.text_chars, removed));
            delta += c.text_chars as isize - (end - start) as isize;
        }
        // Back to front so earlier offsets stay valid.
        let rope = self.rope_mut();
        for c in changes.iter().rev() {
            let (start, end) = (c.start.min(len), c.end.min(len));
            if start < end {
                rope.remove(start..end);
            }
            if !c.text.is_empty() {
                rope.insert(start, &c.text);
            }
        }
        ChangeSet::from_sorted(inverse)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(doc: &Document, changes: Vec<Change>) -> ChangeSet {
        ChangeSet::new(changes, doc.len_chars())
    }

    #[test]
    fn apply_and_inverse_round_trip() {
        let mut d = Document::from_text("hello world");
        let cs = set(
            &d,
            vec![
                Change::new(6, 11, "there"),
                Change::insert(0, ">> "),
                Change::delete(4, 5),
            ],
        );
        let inv = d.apply(&cs);
        assert_eq!(d.text(), ">> hell there");
        d.apply(&inv);
        assert_eq!(d.text(), "hello world");
    }

    #[test]
    fn overlapping_deletes_merge() {
        let d = Document::from_text("abcdef");
        let cs = set(&d, vec![Change::delete(1, 4), Change::delete(2, 5)]);
        assert_eq!(cs.len(), 1);
        let c = cs.iter().next().unwrap();
        assert_eq!((c.start(), c.end()), (1, 5));
    }

    #[test]
    fn crlf_in_inserted_text_is_normalized() {
        let mut d = Document::from_text("ab");
        let cs = set(&d, vec![Change::insert(1, "x\r\ny\rz")]);
        d.apply(&cs);
        assert_eq!(d.text(), "ax\ny\nzb");
    }

    #[test]
    fn ranges_are_clamped_and_noops_dropped() {
        let mut d = Document::from_text("abc");
        let cs = set(&d, vec![Change::delete(2, 99), Change::insert(1, "")]);
        assert_eq!(cs.len(), 1);
        d.apply(&cs);
        assert_eq!(d.text(), "ab");
    }

    #[test]
    fn map_positions() {
        let d = Document::from_text("0123456789");
        // Insert "XX" at 2, replace 5..7 with "Y".
        let cs = set(&d, vec![Change::insert(2, "XX"), Change::new(5, 7, "Y")]);
        use Assoc::*;
        assert_eq!(cs.map(1, After), 1);
        assert_eq!(cs.map(2, Before), 2);
        assert_eq!(cs.map(2, After), 4);
        assert_eq!(cs.map(4, After), 6);
        assert_eq!(cs.map(5, Before), 7); // start of replaced range
        assert_eq!(cs.map(6, Before), 7); // inside -> start
        assert_eq!(cs.map(6, After), 8); // inside -> end of new text
        assert_eq!(cs.map(7, Before), 8); // end of replaced range
        assert_eq!(cs.map(9, After), 10);
        assert_eq!(cs.len_after(10), 11);
    }

    #[test]
    fn map_with_touching_changes() {
        let d = Document::from_text("abcdef");
        // Replace 1..3 and insert at 3: a position at 3 must clear both.
        let cs = set(&d, vec![Change::new(1, 3, "Z"), Change::insert(3, "QQ")]);
        assert_eq!(cs.map(3, Assoc::After), 4);
        assert_eq!(cs.map(3, Assoc::Before), 2);
    }
}
