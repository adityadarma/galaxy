//! Selections and multi-cursor (M1-02).
//!
//! A `Selection` is an `anchor` (where the selection started) and a `head`
//! (where the cursor is). Both are char offsets. A caret is a selection with
//! `anchor == head`. Direction matters: extending a selection moves `head`.
//!
//! `SelectionSet` holds one or more selections and keeps them normalized:
//! sorted by start, clamped to the document, and with overlapping
//! selections merged. One of them is the *primary* selection (the one the
//! viewport follows and that single-cursor commands use).

use crate::document::Document;
use crate::position::{Position, Utf16Position};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
    /// Sticky column (in chars) for vertical movement, so moving down
    /// through a short line returns to the original column. Cleared by any
    /// horizontal movement or edit.
    pub goal_col: Option<usize>,
}

impl Selection {
    pub const fn new(anchor: usize, head: usize) -> Self {
        Self {
            anchor,
            head,
            goal_col: None,
        }
    }

    pub const fn caret(pos: usize) -> Self {
        Self::new(pos, pos)
    }

    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    pub fn len(&self) -> usize {
        self.end() - self.start()
    }

    /// `true` when the head is before the anchor (selected right-to-left).
    pub fn is_backward(&self) -> bool {
        self.head < self.anchor
    }

    /// Moves the head, keeping the anchor (shift + movement).
    pub fn extend_to(self, head: usize) -> Self {
        Self::new(self.anchor, head)
    }

    /// Collapses to a caret at `pos` (movement without shift).
    pub fn collapse_to(self, pos: usize) -> Self {
        Self::caret(pos)
    }

    pub fn with_goal_col(mut self, goal_col: Option<usize>) -> Self {
        self.goal_col = goal_col;
        self
    }

    /// `true` if the two selections must become one. Overlapping ranges
    /// merge; ranges that only touch stay separate (so `Cmd+D` on adjacent
    /// words works), except that a caret merges with anything it touches.
    pub fn should_merge(&self, other: &Selection) -> bool {
        let (a, b) = if self.start() <= other.start() {
            (self, other)
        } else {
            (other, self)
        };
        b.start() < a.end() || (b.start() == a.end() && (a.is_empty() || b.is_empty()))
    }

    /// Union of two selections. Direction follows `self`.
    fn merge(&self, other: &Selection) -> Selection {
        let start = self.start().min(other.start());
        let end = self.end().max(other.end());
        if self.is_backward() {
            Selection::new(end, start)
        } else {
            Selection::new(start, end)
        }
    }

    fn clamp(self, len: usize) -> Self {
        Self {
            anchor: self.anchor.min(len),
            head: self.head.min(len),
            goal_col: self.goal_col,
        }
    }
}

/// A non-empty, normalized set of selections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionSet {
    ranges: Vec<Selection>,
    primary: usize,
}

impl Default for SelectionSet {
    fn default() -> Self {
        Self::single(Selection::caret(0))
    }
}

impl SelectionSet {
    pub fn single(sel: Selection) -> Self {
        Self {
            ranges: vec![sel],
            primary: 0,
        }
    }

    /// Builds a normalized set. `primary` indexes into `ranges` before
    /// normalization; an empty input yields a caret at 0.
    pub fn new(ranges: Vec<Selection>, primary: usize, doc: &Document) -> Self {
        if ranges.is_empty() {
            return Self::default();
        }
        let primary = primary.min(ranges.len() - 1);
        let mut set = Self { ranges, primary };
        set.normalize(doc);
        set
    }

    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    /// Always `false`; a set holds at least one selection. Provided for
    /// API symmetry with `len`.
    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn iter(&self) -> impl Iterator<Item = &Selection> {
        self.ranges.iter()
    }

    pub fn as_slice(&self) -> &[Selection] {
        &self.ranges
    }

    pub fn primary(&self) -> Selection {
        self.ranges[self.primary]
    }

    pub fn primary_index(&self) -> usize {
        self.primary
    }

    /// Adds a selection and makes it primary (`Opt+Click`, `Cmd+D`).
    pub fn push(&mut self, sel: Selection, doc: &Document) {
        self.ranges.push(sel);
        self.primary = self.ranges.len() - 1;
        self.normalize(doc);
    }

    /// Removes the selection at `index` unless it is the last one.
    pub fn remove(&mut self, index: usize) {
        if self.ranges.len() <= 1 || index >= self.ranges.len() {
            return;
        }
        self.ranges.remove(index);
        if self.primary > index || self.primary >= self.ranges.len() {
            self.primary = self.primary.saturating_sub(1);
        }
    }

    /// Keeps only the primary selection (`Esc`).
    pub fn collapse_to_primary(&mut self) {
        let primary = self.primary();
        *self = Self::single(primary);
    }

    /// Applies `f` to every selection, then re-normalizes. This is the
    /// building block for movement commands.
    pub fn transform(&mut self, doc: &Document, mut f: impl FnMut(Selection) -> Selection) {
        for sel in &mut self.ranges {
            *sel = f(*sel);
        }
        self.normalize(doc);
    }

    /// Clamps to the document, sorts by start, and merges overlapping
    /// selections. The primary follows its selection through the merge.
    pub fn normalize(&mut self, doc: &Document) {
        let len = doc.len_chars();
        let primary_sel = self.ranges[self.primary].clamp(len);

        let mut ranges: Vec<Selection> = self.ranges.iter().map(|s| s.clamp(len)).collect();
        // Stable sort keeps insertion order for identical starts, which
        // makes merges deterministic.
        ranges.sort_by_key(|s| (s.start(), s.end()));

        let mut merged: Vec<Selection> = Vec::with_capacity(ranges.len());
        let mut primary = 0;
        for sel in ranges {
            let is_primary = sel == primary_sel;
            match merged.last_mut() {
                Some(last) if last.should_merge(&sel) => {
                    // Keep the primary's direction and goal column.
                    *last = if is_primary {
                        sel.merge(last).with_goal_col(sel.goal_col)
                    } else {
                        last.merge(&sel).with_goal_col(last.goal_col)
                    };
                    if is_primary {
                        primary = merged.len() - 1;
                    }
                }
                _ => {
                    if is_primary {
                        primary = merged.len();
                    }
                    merged.push(sel);
                }
            }
        }
        self.ranges = merged;
        self.primary = primary;
    }

    /// Line/col (chars) of every head, in order. Used by the status bar.
    pub fn head_positions(&self, doc: &Document) -> Vec<Position> {
        self.ranges
            .iter()
            .map(|s| doc.char_to_position(s.head))
            .collect()
    }

    /// LSP positions of every head, in order.
    pub fn head_positions_utf16(&self, doc: &Document) -> Vec<Utf16Position> {
        self.ranges
            .iter()
            .map(|s| doc.char_to_utf16(s.head))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> Document {
        Document::from_text("hello world\nsecond line\n")
    }

    fn spans(set: &SelectionSet) -> Vec<(usize, usize)> {
        set.iter().map(|s| (s.anchor, s.head)).collect()
    }

    #[test]
    fn selection_basics() {
        let s = Selection::new(5, 2);
        assert_eq!((s.start(), s.end(), s.len()), (2, 5, 3));
        assert!(s.is_backward());
        assert!(!s.is_empty());
        assert_eq!(s.extend_to(8), Selection::new(5, 8));
        assert_eq!(s.collapse_to(3), Selection::caret(3));
    }

    #[test]
    fn new_sorts_and_clamps() {
        let d = doc();
        let set = SelectionSet::new(
            vec![
                Selection::caret(10),
                Selection::new(999, 1_000),
                Selection::caret(1),
            ],
            0,
            &d,
        );
        let end = d.len_chars();
        assert_eq!(spans(&set), vec![(1, 1), (10, 10), (end, end)]);
        assert_eq!(set.primary(), Selection::caret(10));
    }

    #[test]
    fn overlapping_ranges_merge_touching_ranges_do_not() {
        let d = doc();
        let set = SelectionSet::new(
            vec![
                Selection::new(0, 5),
                Selection::new(3, 8),
                Selection::new(8, 11),
            ],
            0,
            &d,
        );
        assert_eq!(spans(&set), vec![(0, 8), (8, 11)]);
    }

    #[test]
    fn carets_merge_with_touching_selections() {
        let d = doc();
        let set = SelectionSet::new(
            vec![
                Selection::caret(4),
                Selection::caret(4),
                Selection::new(0, 4),
                Selection::caret(11),
                Selection::new(11, 14),
            ],
            0,
            &d,
        );
        assert_eq!(spans(&set), vec![(0, 4), (11, 14)]);
    }

    #[test]
    fn primary_survives_merge_and_keeps_direction() {
        let d = doc();
        let mut set = SelectionSet::single(Selection::new(0, 5));
        set.push(Selection::new(7, 3), &d); // backward, overlaps, primary
        assert_eq!(set.len(), 1);
        assert_eq!(set.primary(), Selection::new(7, 0));
        assert_eq!(set.primary_index(), 0);
    }

    #[test]
    fn push_makes_new_selection_primary() {
        let d = doc();
        let mut set = SelectionSet::single(Selection::caret(12));
        set.push(Selection::caret(2), &d);
        assert_eq!(spans(&set), vec![(2, 2), (12, 12)]);
        assert_eq!(set.primary(), Selection::caret(2));
        assert_eq!(set.primary_index(), 0);
    }

    #[test]
    fn transform_merges_carets_that_collide() {
        let d = doc();
        let mut set = SelectionSet::new(
            vec![
                Selection::caret(1),
                Selection::caret(2),
                Selection::caret(20),
            ],
            2,
            &d,
        );
        // Move every caret to line start: first two collide.
        set.transform(&d, |s| {
            let line = d.char_to_line(s.head);
            s.collapse_to(d.line_to_char(line))
        });
        assert_eq!(spans(&set), vec![(0, 0), (12, 12)]);
        assert_eq!(set.primary(), Selection::caret(12));
    }

    #[test]
    fn remove_and_collapse() {
        let d = doc();
        let mut set = SelectionSet::new(
            vec![
                Selection::caret(1),
                Selection::caret(5),
                Selection::caret(9),
            ],
            2,
            &d,
        );
        set.remove(0);
        assert_eq!(spans(&set), vec![(5, 5), (9, 9)]);
        assert_eq!(set.primary(), Selection::caret(9));
        set.remove(1);
        assert_eq!(set.primary(), Selection::caret(5));
        set.remove(0); // last one is kept
        assert_eq!(set.len(), 1);

        let mut set = SelectionSet::new(vec![Selection::caret(1), Selection::caret(5)], 1, &d);
        set.collapse_to_primary();
        assert_eq!(spans(&set), vec![(5, 5)]);
    }

    #[test]
    fn head_positions_in_chars_and_utf16() {
        let d = Document::from_text("😀x\nab");
        let set = SelectionSet::new(vec![Selection::caret(1), Selection::new(3, 5)], 0, &d);
        assert_eq!(
            set.head_positions(&d),
            vec![Position::new(0, 1), Position::new(1, 2)]
        );
        assert_eq!(
            set.head_positions_utf16(&d),
            vec![Utf16Position::new(0, 2), Utf16Position::new(1, 2)]
        );
    }

    #[test]
    fn empty_input_defaults_to_caret_at_zero() {
        let set = SelectionSet::new(Vec::new(), 3, &doc());
        assert_eq!(spans(&set), vec![(0, 0)]);
    }
}
