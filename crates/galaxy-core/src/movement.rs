//! Cursor movement and selection commands (M1-11).
//!
//! Every function here is pure: it reads the document and a `SelectionSet`
//! and returns a new `SelectionSet`. Nothing is an undo step.
//!
//! Columns are counted in chars (the canonical unit); landing spots are
//! always snapped to grapheme boundaries so the caret never sits inside an
//! emoji or a combining sequence.

use crate::document::Document;
use crate::selection::{Selection, SelectionSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    /// Smart Home: first non-blank, then column 0, toggling.
    LineStart,
    LineEnd,
    DocStart,
    DocEnd,
    /// Up by this many lines.
    PageUp(usize),
    PageDown(usize),
}

/// Moves every selection. With `extend` the anchor stays (Shift + key);
/// otherwise selections collapse to carets.
pub fn move_selections(
    doc: &Document,
    sel: &SelectionSet,
    motion: Motion,
    extend: bool,
) -> SelectionSet {
    let mut out = sel.clone();
    out.transform(doc, |s| move_one(doc, s, motion, extend));
    out
}

fn move_one(doc: &Document, s: Selection, motion: Motion, extend: bool) -> Selection {
    let len = doc.len_chars();
    let head = s.head.min(len);

    // Left/Right on a selection just collapse it to the matching edge.
    if !extend && !s.is_empty() {
        match motion {
            Motion::Left => return Selection::caret(s.start()),
            Motion::Right => return Selection::caret(s.end()),
            _ => {}
        }
    }

    let (target, goal) = match motion {
        Motion::Left => (doc.prev_grapheme_boundary(head), None),
        Motion::Right => (doc.next_grapheme_boundary(head), None),
        Motion::WordLeft => (snap(doc, word_left(doc, head)), None),
        Motion::WordRight => (snap(doc, word_right(doc, head)), None),
        Motion::LineStart => (smart_home(doc, head), None),
        Motion::LineEnd => (doc.line_end_char(doc.char_to_line(head)), None),
        Motion::DocStart => (0, None),
        Motion::DocEnd => (len, None),
        Motion::Up => vertical(doc, &s, head, -1),
        Motion::Down => vertical(doc, &s, head, 1),
        Motion::PageUp(n) => vertical(doc, &s, head, -(n.max(1) as isize)),
        Motion::PageDown(n) => vertical(doc, &s, head, n.max(1) as isize),
    };

    let moved = if extend {
        Selection::new(s.anchor.min(len), target)
    } else {
        Selection::caret(target)
    };
    moved.with_goal_col(goal)
}

/// Like `move_selections`, but with soft wrap on: up/down/page move by
/// visual row, and Home/End act on the row under the caret.
pub fn move_selections_wrapped(
    doc: &Document,
    sel: &SelectionSet,
    motion: Motion,
    extend: bool,
    columns: usize,
) -> SelectionSet {
    match motion {
        Motion::Up | Motion::Down | Motion::PageUp(_) | Motion::PageDown(_) | Motion::LineEnd => {
            let mut out = sel.clone();
            out.transform(doc, |s| move_wrapped(doc, s, motion, extend, columns));
            out
        }
        // Home keeps its smart toggle on the first row only.
        Motion::LineStart => {
            let mut out = sel.clone();
            out.transform(doc, |s| {
                let head = s.head.min(doc.len_chars());
                let (line, row) = row_of(doc, head, columns);
                if row == 0 {
                    return move_one(doc, s, motion, extend);
                }
                let target = row_start_char(doc, line, row, columns);
                let moved = if extend {
                    Selection::new(s.anchor.min(doc.len_chars()), target)
                } else {
                    Selection::caret(target)
                };
                moved.with_goal_col(None)
            });
            out
        }
        _ => move_selections(doc, sel, motion, extend),
    }
}

/// `(line, row within the line)` of the char offset `head`.
fn row_of(doc: &Document, head: usize, columns: usize) -> (usize, usize) {
    let line = doc.char_to_line(head);
    let col = head - doc.line_to_char(line);
    let breaks = crate::wrap::breaks_for_line(doc, line, columns);
    (line, breaks.partition_point(|b| *b <= col))
}

fn row_start_char(doc: &Document, line: usize, row: usize, columns: usize) -> usize {
    let start = doc.line_to_char(line);
    if row == 0 {
        return start;
    }
    start + crate::wrap::breaks_for_line(doc, line, columns)[row - 1]
}

/// Last caret slot of a row: the line end for the last row, else just
/// before the first grapheme of the next row.
fn row_end_char(doc: &Document, line: usize, row: usize, columns: usize) -> usize {
    let breaks = crate::wrap::breaks_for_line(doc, line, columns);
    match breaks.get(row) {
        Some(next) => doc.prev_grapheme_boundary(doc.line_to_char(line) + next),
        None => doc.line_end_char(line),
    }
}

/// One visual row down (`true`) or up (`false`), or `None` at the edge.
fn step_row(
    doc: &Document,
    line: usize,
    row: usize,
    down: bool,
    columns: usize,
) -> Option<(usize, usize)> {
    if down {
        if row + 1 < crate::wrap::rows_in_line(doc, line, columns) {
            Some((line, row + 1))
        } else if line + 1 < doc.line_count() {
            Some((line + 1, 0))
        } else {
            None
        }
    } else if row > 0 {
        Some((line, row - 1))
    } else if line > 0 {
        let rows = crate::wrap::rows_in_line(doc, line - 1, columns);
        Some((line - 1, rows - 1))
    } else {
        None
    }
}

fn move_wrapped(
    doc: &Document,
    s: Selection,
    motion: Motion,
    extend: bool,
    columns: usize,
) -> Selection {
    let head = s.head.min(doc.len_chars());
    let (line, row) = row_of(doc, head, columns);
    let (target, goal) = if motion == Motion::LineEnd {
        (snap(doc, row_end_char(doc, line, row, columns)), None)
    } else {
        let (down, count) = match motion {
            Motion::Up => (false, 1),
            Motion::Down => (true, 1),
            Motion::PageUp(n) => (false, n.max(1)),
            Motion::PageDown(n) => (true, n.max(1)),
            _ => unreachable!("handled by the caller"),
        };
        let col = s
            .goal_col
            .unwrap_or(head - row_start_char(doc, line, row, columns));
        let (mut l, mut r) = (line, row);
        let mut hit_edge = false;
        for _ in 0..count {
            match step_row(doc, l, r, down, columns) {
                Some(next) => (l, r) = next,
                None => {
                    hit_edge = true;
                    break;
                }
            }
        }
        if hit_edge && (l, r) == (line, row) {
            // Already on the first or last row: jump to the document edge.
            (if down { doc.len_chars() } else { 0 }, Some(col))
        } else {
            let start = row_start_char(doc, l, r, columns);
            let end = row_end_char(doc, l, r, columns);
            (snap(doc, (start + col).min(end)), Some(col))
        }
    };
    let moved = if extend {
        Selection::new(s.anchor.min(doc.len_chars()), target)
    } else {
        Selection::caret(target)
    };
    moved.with_goal_col(goal)
}

/// Vertical movement keeps a sticky column. Past the first or last line the
/// caret jumps to the document start or end, as in VSCode.
fn vertical(doc: &Document, s: &Selection, head: usize, delta: isize) -> (usize, Option<usize>) {
    let line = doc.char_to_line(head);
    let col = s.goal_col.unwrap_or(head - doc.line_to_char(line));
    let target_line = line as isize + delta;
    if target_line < 0 {
        return (0, Some(col));
    }
    let target_line = target_line as usize;
    if target_line >= doc.line_count() {
        return (doc.len_chars(), Some(col));
    }
    let start = doc.line_to_char(target_line);
    let end = doc.line_end_char(target_line);
    (snap(doc, (start + col).min(end)), Some(col))
}

fn smart_home(doc: &Document, head: usize) -> usize {
    let line = doc.char_to_line(head);
    let start = doc.line_to_char(line);
    let end = doc.line_end_char(line);
    let indent = doc
        .rope()
        .slice(start..end)
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .count();
    // Indent followed by a combining mark would otherwise split a cluster.
    let first = snap(doc, start + indent);
    if head == first { start } else { first }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Blank,
    Newline,
    Word,
    Punct,
}

fn class(c: char) -> Class {
    if c == '\n' {
        Class::Newline
    } else if c.is_whitespace() {
        Class::Blank
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

/// `true` for letters, digits and `_`: the chars that make up a word.
pub(crate) fn is_word_char(c: char) -> bool {
    class(c) == Class::Word
}

/// End of the next word. Stops at line ends and crosses them one at a time.
fn word_right(doc: &Document, pos: usize) -> usize {
    let rope = doc.rope();
    let len = rope.len_chars();
    let mut i = pos.min(len);
    if i == len {
        return len;
    }
    if class(rope.char(i)) == Class::Newline {
        return i + 1;
    }
    while i < len && class(rope.char(i)) == Class::Blank {
        i += 1;
    }
    if i == len || class(rope.char(i)) == Class::Newline {
        return i;
    }
    let c = class(rope.char(i));
    while i < len && class(rope.char(i)) == c {
        i += 1;
    }
    i
}

/// Start of the previous word (mirror of `word_right`).
fn word_left(doc: &Document, pos: usize) -> usize {
    let rope = doc.rope();
    let mut i = pos.min(rope.len_chars());
    if i == 0 {
        return 0;
    }
    if class(rope.char(i - 1)) == Class::Newline {
        return i - 1;
    }
    while i > 0 && class(rope.char(i - 1)) == Class::Blank {
        i -= 1;
    }
    if i == 0 || class(rope.char(i - 1)) == Class::Newline {
        return i;
    }
    let c = class(rope.char(i - 1));
    while i > 0 && class(rope.char(i - 1)) == c {
        i -= 1;
    }
    i
}

/// Moves `pos` back to the start of the grapheme containing it.
pub(crate) fn snap(doc: &Document, pos: usize) -> usize {
    if pos >= doc.len_chars() {
        doc.len_chars()
    } else {
        doc.prev_grapheme_boundary(doc.next_grapheme_boundary(pos))
    }
}

/// Moves an exclusive range end forward to a grapheme boundary.
pub(crate) fn snap_end(doc: &Document, pos: usize) -> usize {
    if pos == 0 || pos >= doc.len_chars() {
        pos.min(doc.len_chars())
    } else {
        doc.next_grapheme_boundary(doc.prev_grapheme_boundary(pos))
    }
}

// --- Selection commands --------------------------------------------------

pub fn select_all(doc: &Document) -> SelectionSet {
    SelectionSet::single(Selection::new(0, doc.len_chars()))
}

/// Word (or run of blanks / punctuation) under `pos`: double-click.
pub fn word_at(doc: &Document, pos: usize) -> Selection {
    let rope = doc.rope();
    let len = rope.len_chars();
    let pos = pos.min(len);
    if len == 0 {
        return Selection::caret(0);
    }
    let mut probe = if pos == len { len - 1 } else { pos };
    // A click at the end of a word selects that word (like VSCode), and a
    // click at the line break selects whatever run ends the line.
    if probe > 0 {
        let here = class(rope.char(probe));
        let before = class(rope.char(probe - 1));
        if (here != Class::Word && before == Class::Word)
            || (here == Class::Newline && before != Class::Newline)
        {
            probe -= 1;
        }
    }
    let c = class(rope.char(probe));
    if c == Class::Newline {
        return Selection::caret(pos);
    }
    let mut start = probe;
    while start > 0 && class(rope.char(start - 1)) == c {
        start -= 1;
    }
    let mut end = probe + 1;
    while end < len && class(rope.char(end)) == c {
        end += 1;
    }
    Selection::new(snap(doc, start), snap_end(doc, end))
}

/// The whole line under `pos`, including its line break: triple-click.
pub fn line_at(doc: &Document, pos: usize) -> Selection {
    let line = doc.char_to_line(pos);
    let start = doc.line_to_char(line);
    let end = if line + 1 < doc.line_count() {
        doc.line_to_char(line + 1)
    } else {
        doc.len_chars()
    };
    Selection::new(start, end)
}

// --- Mouse ---------------------------------------------------------------

/// Plain click: one caret at `pos`.
pub fn set_caret(doc: &Document, pos: usize) -> SelectionSet {
    SelectionSet::single(Selection::caret(snap(doc, pos)))
}

/// Option-click: adds a caret and makes it primary.
pub fn add_caret(doc: &Document, sel: &SelectionSet, pos: usize) -> SelectionSet {
    let mut ranges = sel.as_slice().to_vec();
    ranges.push(Selection::caret(snap(doc, pos)));
    let primary = ranges.len() - 1;
    SelectionSet::new(ranges, primary, doc)
}

/// Shift-click or drag: moves the primary head to `pos`, keeping its anchor.
pub fn extend_primary_to(doc: &Document, sel: &SelectionSet, pos: usize) -> SelectionSet {
    let mut ranges = sel.as_slice().to_vec();
    let primary = sel.primary_index();
    ranges[primary] = ranges[primary].extend_to(snap(doc, pos));
    SelectionSet::new(ranges, primary, doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn doc(text: &str) -> Document {
        Document::from_text(text)
    }

    fn caret(n: usize) -> SelectionSet {
        SelectionSet::single(Selection::caret(n))
    }

    fn step(d: &Document, sel: &SelectionSet, m: Motion) -> SelectionSet {
        move_selections(d, sel, m, false)
    }

    fn head(sel: &SelectionSet) -> usize {
        sel.primary().head
    }

    #[test]
    fn horizontal_moves_whole_graphemes() {
        // 'a', family emoji (5 chars with ZWJs), 'e' + combining acute, 'b'.
        let d = doc("a\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}e\u{301}b");
        let mut s = caret(0);
        let mut seen = vec![0];
        for _ in 0..4 {
            s = step(&d, &s, Motion::Right);
            seen.push(head(&s));
        }
        assert_eq!(seen, vec![0, 1, 6, 8, 9]);
        assert_eq!(head(&step(&d, &s, Motion::Right)), 9, "stays at the end");
        for expected in [8, 6, 1, 0, 0] {
            s = step(&d, &s, Motion::Left);
            assert_eq!(head(&s), expected);
        }
    }

    #[test]
    fn left_and_right_collapse_a_selection_to_its_edges() {
        let d = doc("hello world");
        let s = SelectionSet::single(Selection::new(2, 7));
        assert_eq!(step(&d, &s, Motion::Left).primary(), Selection::caret(2));
        assert_eq!(step(&d, &s, Motion::Right).primary(), Selection::caret(7));
        // Backward selections collapse the same way.
        let back = SelectionSet::single(Selection::new(7, 2));
        assert_eq!(step(&d, &back, Motion::Left).primary(), Selection::caret(2));
        assert_eq!(
            step(&d, &back, Motion::Right).primary(),
            Selection::caret(7)
        );
    }

    #[test]
    fn shift_extends_and_keeps_the_anchor() {
        let d = doc("hello world");
        let mut s = caret(5);
        s = move_selections(&d, &s, Motion::Right, true);
        s = move_selections(&d, &s, Motion::Right, true);
        assert_eq!(s.primary(), Selection::new(5, 7));
        s = move_selections(&d, &s, Motion::WordLeft, true);
        assert_eq!(
            s.primary(),
            Selection::new(5, 6),
            "back to the start of 'world'"
        );
        s = move_selections(&d, &s, Motion::Left, true);
        s = move_selections(&d, &s, Motion::Left, true);
        assert_eq!(s.primary(), Selection::new(5, 4));
        assert!(s.primary().is_backward());
        s = move_selections(&d, &s, Motion::DocEnd, true);
        assert_eq!(s.primary(), Selection::new(5, 11));
    }

    #[test]
    fn vertical_moves_keep_a_sticky_column() {
        let d = doc("long line\nx\nanother long");
        let mut s = caret(8);
        s = step(&d, &s, Motion::Down);
        assert_eq!(head(&s), 11, "short line clamps to its end");
        s = step(&d, &s, Motion::Down);
        assert_eq!(head(&s), 12 + 8, "column 8 is remembered");
        s = step(&d, &s, Motion::Up);
        s = step(&d, &s, Motion::Up);
        assert_eq!(head(&s), 8);
        // Horizontal movement forgets the sticky column.
        s = step(&d, &s, Motion::Down);
        s = step(&d, &s, Motion::Left);
        assert_eq!(s.primary().goal_col, None);
    }

    #[test]
    fn vertical_moves_past_the_edges_jump_to_document_ends() {
        let d = doc("ab\ncd");
        assert_eq!(head(&step(&d, &caret(1), Motion::Up)), 0);
        assert_eq!(head(&step(&d, &caret(4), Motion::Down)), 5);
        assert_eq!(head(&step(&d, &caret(0), Motion::PageDown(50))), 5);
        assert_eq!(head(&step(&d, &caret(4), Motion::PageUp(50))), 0);
        assert_eq!(head(&step(&d, &caret(0), Motion::PageDown(1))), 3);
    }

    #[test]
    fn vertical_landing_never_splits_a_grapheme() {
        // Line 1 is "e\u{301}e\u{301}" (4 chars, 2 graphemes).
        let d = doc("abc\ne\u{301}e\u{301}");
        let s = step(&d, &caret(1), Motion::Down);
        assert_eq!(head(&s), 4, "column 1 is inside the first cluster");
        let s = step(&d, &caret(3), Motion::Down);
        assert_eq!(
            head(&s),
            6,
            "column 3 is inside the second cluster, so it snaps back"
        );
        let s = step(&d, &caret(3), Motion::Down);
        let s = step(&d, &s, Motion::Right);
        assert_eq!(head(&s), 8, "and the next step reaches the line end");
    }

    #[test]
    fn word_motion_walks_words_punctuation_and_line_breaks() {
        let d = doc("foo.bar  baz\nqux");
        let mut s = caret(0);
        let mut stops = vec![];
        for _ in 0..6 {
            s = step(&d, &s, Motion::WordRight);
            stops.push(head(&s));
        }
        assert_eq!(stops, vec![3, 4, 7, 12, 13, 16]);
        let mut stops = vec![];
        for _ in 0..7 {
            s = step(&d, &s, Motion::WordLeft);
            stops.push(head(&s));
        }
        assert_eq!(stops, vec![13, 12, 9, 4, 3, 0, 0]);
    }

    #[test]
    fn smart_home_toggles_between_indent_and_column_zero() {
        let d = doc("    foo\nbar");
        let mut s = caret(6);
        s = step(&d, &s, Motion::LineStart);
        assert_eq!(head(&s), 4);
        s = step(&d, &s, Motion::LineStart);
        assert_eq!(head(&s), 0);
        s = step(&d, &s, Motion::LineStart);
        assert_eq!(head(&s), 4);
        assert_eq!(head(&step(&d, &caret(5), Motion::LineEnd)), 7);
        assert_eq!(head(&step(&d, &caret(9), Motion::DocStart)), 0);
        assert_eq!(head(&step(&d, &caret(2), Motion::DocEnd)), 11);
        // A blank line has no indent to toggle to.
        let blank = doc("\n");
        assert_eq!(head(&step(&blank, &caret(0), Motion::LineStart)), 0);
    }

    #[test]
    fn carets_that_meet_merge() {
        let d = doc("abcd");
        let s = SelectionSet::new(vec![Selection::caret(0), Selection::caret(1)], 1, &d);
        let moved = step(&d, &s, Motion::Left);
        assert_eq!(moved.len(), 1);
        assert_eq!(head(&moved), 0);
    }

    #[test]
    fn select_all_word_and_line() {
        let d = doc("foo bar\n\nbaz");
        assert_eq!(select_all(&d).primary(), Selection::new(0, 12));
        assert_eq!(word_at(&d, 1), Selection::new(0, 3));
        assert_eq!(
            word_at(&d, 3),
            Selection::new(0, 3),
            "end of word prefers the word"
        );
        assert_eq!(word_at(&d, 4), Selection::new(4, 7));
        assert_eq!(
            word_at(&d, 7),
            Selection::new(4, 7),
            "line end prefers the word"
        );
        assert_eq!(word_at(&d, 8), Selection::caret(8), "empty line");
        assert_eq!(word_at(&d, 12), Selection::new(9, 12), "document end");
        assert_eq!(word_at(&doc("a   b"), 2), Selection::new(1, 4), "blank run");
        assert_eq!(
            word_at(&doc("a+=b"), 2),
            Selection::new(1, 3),
            "punctuation run"
        );
        assert_eq!(
            word_at(&doc("a+=b"), 1),
            Selection::new(0, 1),
            "end of 'a' prefers the word"
        );
        assert_eq!(
            word_at(&doc("a.\nb"), 2),
            Selection::new(1, 2),
            "punctuation before a break"
        );
        assert_eq!(word_at(&doc(""), 0), Selection::caret(0));
        assert_eq!(line_at(&d, 2), Selection::new(0, 8));
        assert_eq!(line_at(&d, 8), Selection::new(8, 9));
        assert_eq!(
            line_at(&d, 11),
            Selection::new(9, 12),
            "last line has no break"
        );
    }

    #[test]
    fn word_selection_keeps_combining_marks_together() {
        let d = doc("cafe\u{301} ok");
        assert_eq!(word_at(&d, 1), Selection::new(0, 5));
    }

    #[test]
    fn mouse_commands() {
        let d = doc("hello\nworld");
        let s = set_caret(&d, 8);
        assert_eq!(s.primary(), Selection::caret(8));

        let dragged = extend_primary_to(&d, &s, 2);
        assert_eq!(dragged.primary(), Selection::new(8, 2));

        let two = add_caret(&d, &s, 1);
        assert_eq!(two.len(), 2);
        assert_eq!(two.primary().head, 1, "the new caret is primary");
        let two = extend_primary_to(&d, &two, 3);
        assert_eq!(two.as_slice()[0], Selection::new(1, 3));
        assert_eq!(two.as_slice()[1], Selection::caret(8));
        // Clicking past the end clamps.
        assert_eq!(set_caret(&d, 999).primary(), Selection::caret(11));
    }

    fn arb_motion() -> impl Strategy<Value = Motion> {
        prop_oneof![
            Just(Motion::Left),
            Just(Motion::Right),
            Just(Motion::Up),
            Just(Motion::Down),
            Just(Motion::WordLeft),
            Just(Motion::WordRight),
            Just(Motion::LineStart),
            Just(Motion::LineEnd),
            Just(Motion::DocStart),
            Just(Motion::DocEnd),
            (1usize..40).prop_map(Motion::PageUp),
            (1usize..40).prop_map(Motion::PageDown),
        ]
    }

    fn is_boundary(d: &Document, pos: usize) -> bool {
        pos == 0
            || pos == d.len_chars()
            || d.prev_grapheme_boundary(d.next_grapheme_boundary(pos)) == pos
    }

    proptest! {
        #[test]
        fn motions_stay_in_bounds_on_boundaries_and_normalized(
            text in proptest::collection::vec(
                prop_oneof![
                    Just("a"), Just("b"), Just(" "), Just("\n"), Just("\t"), Just("_"),
                    Just("."), Just("\u{301}"), Just("\u{1F468}\u{200D}\u{1F469}"), Just("\u{4E2D}"),
                ],
                0..60,
            ).prop_map(|v| v.concat()),
            start in 0usize..80,
            moves in proptest::collection::vec((arb_motion(), any::<bool>()), 1..40),
        ) {
            let d = doc(&text);
            let len = d.len_chars();
            let mut s = set_caret(&d, start.min(len));
            for (motion, extend) in moves {
                s = move_selections(&d, &s, motion, extend);
                prop_assert!(!s.is_empty());
                let mut prev_end = 0;
                for r in s.iter() {
                    prop_assert!(r.anchor <= len && r.head <= len);
                    prop_assert!(is_boundary(&d, r.head), "head {} mid-grapheme in {:?}", r.head, text);
                    prop_assert!(r.start() >= prev_end || prev_end == 0);
                    prev_end = r.end();
                }
                if !extend {
                    prop_assert!(s.iter().all(Selection::is_empty));
                }
            }
        }

        #[test]
        fn word_at_is_a_boundary_aligned_range_containing_the_click(
            text in "[a-z .\n\u{301}]{0,40}",
            pos in 0usize..60,
        ) {
            let d = doc(&text);
            let len = d.len_chars();
            let w = word_at(&d, pos);
            prop_assert!(w.start() <= w.end() && w.end() <= len);
            prop_assert!(is_boundary(&d, w.start()) && is_boundary(&d, w.end()));
            let l = line_at(&d, pos);
            prop_assert!(l.start() <= pos.min(len) && pos.min(len) <= l.end());
        }
    }
}

#[cfg(test)]
mod wrap_tests {
    use super::*;
    use crate::selection::SelectionSet;

    fn at(_doc: &Document, pos: usize) -> SelectionSet {
        SelectionSet::single(Selection::caret(pos))
    }
    fn head(sel: &SelectionSet) -> usize {
        sel.primary().head
    }

    // 4 columns: "abcd" | "efgh" | "ij", then "xy"
    const TEXT: &str = "abcdefghij\nxy";

    #[test]
    fn down_and_up_move_by_visual_row() {
        let d = Document::from_text(TEXT);
        let s = move_selections_wrapped(&d, &at(&d, 1), Motion::Down, false, 4);
        assert_eq!(head(&s), 5, "same column on the second row");
        let s = move_selections_wrapped(&d, &s, Motion::Down, false, 4);
        assert_eq!(head(&s), 9, "third row is shorter: clamps to its end");
        let s = move_selections_wrapped(&d, &s, Motion::Down, false, 4);
        assert_eq!(head(&s), 12, "next logical line, sticky column 1");
        let s = move_selections_wrapped(&d, &s, Motion::Up, false, 4);
        assert_eq!(head(&s), 9);
    }

    #[test]
    fn first_and_last_row_jump_to_the_document_edges() {
        let d = Document::from_text(TEXT);
        let s = move_selections_wrapped(&d, &at(&d, 2), Motion::Up, false, 4);
        assert_eq!(head(&s), 0);
        let s = move_selections_wrapped(&d, &at(&d, 12), Motion::Down, false, 4);
        assert_eq!(head(&s), d.len_chars());
    }

    #[test]
    fn page_moves_several_rows() {
        let d = Document::from_text(TEXT);
        let s = move_selections_wrapped(&d, &at(&d, 0), Motion::PageDown(3), false, 4);
        assert_eq!(head(&s), 11, "four rows total, three down is the last");
    }

    #[test]
    fn home_and_end_act_on_the_visual_row() {
        let d = Document::from_text(TEXT);
        let s = move_selections_wrapped(&d, &at(&d, 6), Motion::LineEnd, false, 4);
        assert_eq!(head(&s), 7, "end of row two stops before the next row");
        let s = move_selections_wrapped(&d, &at(&d, 6), Motion::LineStart, false, 4);
        assert_eq!(head(&s), 4);
        let s = move_selections_wrapped(&d, &at(&d, 9), Motion::LineEnd, false, 4);
        assert_eq!(head(&s), 10, "last row ends at the line end");
    }

    #[test]
    fn extend_keeps_the_anchor() {
        let d = Document::from_text(TEXT);
        let s = move_selections_wrapped(&d, &at(&d, 1), Motion::Down, true, 4);
        assert_eq!((s.primary().anchor, s.primary().head), (1, 5));
    }
}
