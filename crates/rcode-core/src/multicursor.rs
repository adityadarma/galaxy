//! Multi-cursor commands (M1-12).
//!
//! Like `movement`, every function is pure: it takes the document and a
//! `SelectionSet` and returns a new one.
//!
//! * `select_next_occurrence` (Cmd+D): on a caret, selects the word; on a
//!   selection, adds the next match of its text, wrapping around the file.
//! * `select_all_occurrences` (Shift+Cmd+L).
//! * `add_cursor_vertical` (Opt+Cmd+Up/Down).
//! * `column_selection` (Opt+Shift+drag): one selection per line.
//!
//! Matching is case-sensitive. It is whole-word only when the text being
//! searched for is itself a whole word, so a double-clicked `foo` skips
//! `foobar` while a hand-picked `foo` inside `foobar` matches substrings.

use crate::document::Document;
use crate::movement::{self, is_word_char, snap, snap_end};
use crate::selection::{Selection, SelectionSet};

/// Upper bound for "select all occurrences", so a one-letter needle in a
/// huge file cannot create millions of cursors.
pub const MAX_OCCURRENCES: usize = 100_000;

/// What Cmd+D searches for.
struct Needle {
    chars: Vec<char>,
    whole_word: bool,
    /// The range the needle was read from.
    origin_start: usize,
    origin_end: usize,
}

/// The word under `pos`, or `None` when `pos` is not on a word.
fn word_range(doc: &Document, pos: usize) -> Option<Selection> {
    let w = movement::word_at(doc, pos);
    (!w.is_empty() && is_word_char(doc.rope().char(w.start()))).then_some(w)
}

/// `true` if neither neighbor of `start..end` is a word char.
fn at_word_bounds(doc: &Document, start: usize, end: usize) -> bool {
    let rope = doc.rope();
    (start == 0 || !is_word_char(rope.char(start - 1)))
        && (end == rope.len_chars() || !is_word_char(rope.char(end)))
}

/// Builds the search from the primary selection. Whole-word mode needs
/// *every* selection to be a bounded word, so a hand-picked fragment keeps
/// matching fragments even after a whole-word match becomes primary.
fn needle(doc: &Document, sel: &SelectionSet) -> Option<Needle> {
    let primary = sel.primary();
    let range = if primary.is_empty() {
        word_range(doc, primary.head)?
    } else {
        primary
    };
    let chars: Vec<char> = doc
        .rope()
        .slice(range.start()..range.end())
        .chars()
        .collect();
    let whole_word = chars.iter().all(|&c| is_word_char(c))
        && at_word_bounds(doc, range.start(), range.end())
        && sel
            .iter()
            .filter(|r| !r.is_empty())
            .all(|r| at_word_bounds(doc, r.start(), r.end()));
    Some(Needle {
        chars,
        whole_word,
        origin_start: range.start(),
        origin_end: range.end(),
    })
}

fn is_match(doc: &Document, start: usize, n: &Needle) -> bool {
    let rope = doc.rope();
    let end = start + n.chars.len();
    if end > rope.len_chars() || !rope.slice(start..end).chars().eq(n.chars.iter().copied()) {
        return false;
    }
    !n.whole_word || at_word_bounds(doc, start, end)
}

/// First match starting in `from..to` that `accept(start, end)` allows.
fn find_in(
    doc: &Document,
    n: &Needle,
    from: usize,
    to: usize,
    accept: impl Fn(usize, usize) -> bool,
) -> Option<usize> {
    let rope = doc.rope();
    let to = to.min(rope.len_chars());
    if from >= to {
        return None;
    }
    let first = n.chars[0];
    for (offset, c) in rope.chars_at(from).take(to - from).enumerate() {
        if c != first {
            continue;
        }
        let start = from + offset;
        if is_match(doc, start, n) && accept(start, start + n.chars.len()) {
            return Some(start);
        }
    }
    None
}

/// `true` if `start..end` collides with an existing selection. A caret
/// collides only when it sits strictly inside.
fn overlaps(sel: &SelectionSet, start: usize, end: usize) -> bool {
    sel.iter().any(|r| {
        if r.is_empty() {
            start < r.head && r.head < end
        } else {
            r.start() < end && start < r.end()
        }
    })
}

/// Cmd+D.
pub fn select_next_occurrence(doc: &Document, sel: &SelectionSet) -> SelectionSet {
    let primary = sel.primary();
    if primary.is_empty() {
        // First press: every caret that is on a word selects that word.
        if word_range(doc, primary.head).is_none() {
            return sel.clone();
        }
        let mut out = sel.clone();
        out.transform(doc, |s| {
            if !s.is_empty() {
                return s;
            }
            word_range(doc, s.head).unwrap_or(s)
        });
        return out;
    }

    let Some(n) = needle(doc, sel) else {
        return sel.clone();
    };
    let accept = |start, end| !overlaps(sel, start, end);
    let found = find_in(doc, &n, primary.end(), doc.len_chars(), accept)
        .or_else(|| find_in(doc, &n, 0, primary.start(), accept));
    let Some(start) = found else {
        return sel.clone();
    };
    let end = start + n.chars.len();
    let added = if primary.is_backward() {
        Selection::new(end, start)
    } else {
        Selection::new(start, end)
    };
    let mut ranges = sel.as_slice().to_vec();
    ranges.push(added);
    let primary_index = ranges.len() - 1;
    SelectionSet::new(ranges, primary_index, doc)
}

/// Shift+Cmd+L: selects every occurrence of the primary selection's text
/// (or of the word under the caret). Stops at `MAX_OCCURRENCES`.
pub fn select_all_occurrences(doc: &Document, sel: &SelectionSet) -> SelectionSet {
    let Some(n) = needle(doc, sel) else {
        return sel.clone();
    };
    let (len, width) = (doc.len_chars(), n.chars.len());
    let mut ranges = Vec::new();

    // The origin is always part of the result. Matches to its left must end
    // before it and matches to its right start after it, so needles that
    // overlap themselves (like "aa" in "aaa") cannot skip or collide with it.
    let mut at = 0;
    while ranges.len() < MAX_OCCURRENCES {
        let found = find_in(doc, &n, at, n.origin_start, |_, end| end <= n.origin_start);
        let Some(start) = found else { break };
        ranges.push(Selection::new(start, start + width));
        at = start + width;
    }
    let primary = ranges.len();
    ranges.push(Selection::new(n.origin_start, n.origin_end));
    at = n.origin_end;
    while ranges.len() < MAX_OCCURRENCES {
        let Some(start) = find_in(doc, &n, at, len, |_, _| true) else {
            break;
        };
        ranges.push(Selection::new(start, start + width));
        at = start + width;
    }
    SelectionSet::new(ranges, primary, doc)
}

/// Opt+Cmd+Up (`delta = -1`) and Opt+Cmd+Down (`delta = 1`): adds a caret
/// on the neighboring line of every selection, in the same column. Lines
/// that do not exist are skipped. The new caret for the primary becomes
/// the primary.
pub fn add_cursor_vertical(doc: &Document, sel: &SelectionSet, delta: isize) -> SelectionSet {
    let cols: Vec<(usize, usize)> = sel
        .iter()
        .map(|s| {
            let line = doc.char_to_line(s.head);
            (line, s.goal_col.unwrap_or(s.head - doc.line_to_char(line)))
        })
        .collect();

    // Existing selections remember the column so repeated presses line up.
    let mut ranges: Vec<Selection> = sel
        .iter()
        .zip(&cols)
        .map(|(s, &(_, col))| s.with_goal_col(Some(col)))
        .collect();
    let mut primary = sel.primary_index();
    for (i, &(line, col)) in cols.iter().enumerate() {
        let target = line as isize + delta;
        if target < 0 || target as usize >= doc.line_count() {
            continue;
        }
        let target = target as usize;
        let pos = snap(
            doc,
            (doc.line_to_char(target) + col).min(doc.line_end_char(target)),
        );
        ranges.push(Selection::caret(pos).with_goal_col(Some(col)));
        if i == sel.primary_index() {
            primary = ranges.len() - 1;
        }
    }
    SelectionSet::new(ranges, primary, doc)
}

/// Opt+Shift+drag. `anchor` and `head` are `(line, column)` with columns
/// counted in chars. They may lie past the end of short lines; those lines
/// get a caret (or a shorter selection) at their end. Tabs and wide glyphs
/// count as one column.
pub fn column_selection(
    doc: &Document,
    anchor: (usize, usize),
    head: (usize, usize),
) -> SelectionSet {
    let last = doc.line_count() - 1;
    let (anchor_line, head_line) = (anchor.0.min(last), head.0.min(last));
    let (lo, hi) = (anchor.1.min(head.1), anchor.1.max(head.1));
    let backward = head.1 < anchor.1;
    let (first, final_line) = (anchor_line.min(head_line), anchor_line.max(head_line));

    let ranges = (first..=final_line)
        .map(|line| {
            let line_start = doc.line_to_char(line);
            let len = doc.line_len_chars(line);
            let start = snap(doc, line_start + lo.min(len));
            let end = snap_end(doc, line_start + hi.min(len));
            if backward {
                Selection::new(end, start)
            } else {
                Selection::new(start, end)
            }
        })
        .collect();
    SelectionSet::new(ranges, head_line - first, doc)
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

    fn ranges(sel: &SelectionSet) -> Vec<(usize, usize)> {
        sel.iter().map(|r| (r.anchor, r.head)).collect()
    }

    fn text_of(d: &Document, r: &Selection) -> String {
        d.rope().slice(r.start()..r.end()).to_string()
    }

    #[test]
    fn first_press_selects_the_word_then_each_press_adds_the_next_match() {
        let d = doc("foo bar foo baz foo");
        let mut s = select_next_occurrence(&d, &caret(1));
        assert_eq!(ranges(&s), vec![(0, 3)]);
        s = select_next_occurrence(&d, &s);
        assert_eq!(ranges(&s), vec![(0, 3), (8, 11)]);
        assert_eq!(
            s.primary(),
            Selection::new(8, 11),
            "the new match is primary"
        );
        s = select_next_occurrence(&d, &s);
        assert_eq!(ranges(&s), vec![(0, 3), (8, 11), (16, 19)]);
        // Every match is selected; another press changes nothing.
        assert_eq!(select_next_occurrence(&d, &s), s);
    }

    #[test]
    fn search_wraps_around_the_file() {
        let d = doc("foo x foo");
        let s = SelectionSet::single(Selection::new(6, 9));
        let s = select_next_occurrence(&d, &s);
        assert_eq!(ranges(&s), vec![(0, 3), (6, 9)]);
        assert_eq!(s.primary(), Selection::new(0, 3));
    }

    #[test]
    fn a_whole_word_skips_longer_words_but_a_fragment_does_not() {
        let d = doc("foo foobar foo");
        let s = select_next_occurrence(&d, &caret(1));
        let s = select_next_occurrence(&d, &s);
        assert_eq!(ranges(&s), vec![(0, 3), (11, 14)], "foobar is skipped");

        let d = doc("foobar foo foobar");
        let s = SelectionSet::single(Selection::new(0, 3));
        let s = select_next_occurrence(&d, &s);
        assert_eq!(s.primary(), Selection::new(7, 10));
        let s = select_next_occurrence(&d, &s);
        assert_eq!(
            s.primary(),
            Selection::new(11, 14),
            "a fragment matches inside words"
        );
    }

    #[test]
    fn matching_is_case_sensitive_and_handles_multiline_text() {
        let d = doc("Foo foo FOO");
        let s = select_next_occurrence(&d, &caret(5));
        assert_eq!(select_next_occurrence(&d, &s), s, "no other 'foo'");

        let d = doc("a\nb a\nb");
        let s = SelectionSet::single(Selection::new(0, 3));
        let s = select_next_occurrence(&d, &s);
        assert_eq!(ranges(&s), vec![(0, 3), (4, 7)]);
    }

    #[test]
    fn backward_selections_keep_their_direction() {
        let d = doc("ab ab");
        let s = SelectionSet::single(Selection::new(2, 0));
        let s = select_next_occurrence(&d, &s);
        assert_eq!(s.primary(), Selection::new(5, 3));
    }

    #[test]
    fn carets_expand_to_words_and_non_words_do_nothing() {
        let d = doc("foo bar");
        let two = SelectionSet::new(vec![Selection::caret(1), Selection::caret(5)], 0, &d);
        assert_eq!(
            ranges(&select_next_occurrence(&d, &two)),
            vec![(0, 3), (4, 7)]
        );

        let d = doc("a  b");
        assert_eq!(select_next_occurrence(&d, &caret(2)), caret(2));
        let d = doc("");
        assert_eq!(select_next_occurrence(&d, &caret(0)), caret(0));
    }

    #[test]
    fn existing_selections_are_never_selected_twice() {
        let d = doc("ab ab ab");
        let s = SelectionSet::new(vec![Selection::new(0, 2), Selection::new(3, 5)], 0, &d);
        let s = select_next_occurrence(&d, &s);
        assert_eq!(ranges(&s), vec![(0, 2), (3, 5), (6, 8)]);
    }

    #[test]
    fn select_all_occurrences_takes_every_match() {
        let d = doc("foo bar foo\nfoo foobar");
        let s = select_all_occurrences(&d, &caret(9));
        assert_eq!(ranges(&s), vec![(0, 3), (8, 11), (12, 15)]);
        assert_eq!(
            s.primary(),
            Selection::new(8, 11),
            "primary is the match at the caret"
        );

        // A fragment selection matches inside longer words too.
        let d = doc("foobar foo");
        let s = select_all_occurrences(&d, &SelectionSet::single(Selection::new(0, 3)));
        assert_eq!(ranges(&s), vec![(0, 3), (7, 10)]);

        // Matches do not overlap.
        let d = doc("aaaa");
        let s = select_all_occurrences(&d, &SelectionSet::single(Selection::new(0, 2)));
        assert_eq!(ranges(&s), vec![(0, 2), (2, 4)]);

        let d = doc("  ");
        assert_eq!(select_all_occurrences(&d, &caret(1)), caret(1));
    }

    #[test]
    fn cursors_are_added_above_and_below_in_the_same_column() {
        let d = doc("abcd\nef\nghij");
        let s = add_cursor_vertical(&d, &caret(3), 1);
        assert_eq!(ranges(&s), vec![(3, 3), (7, 7)]);
        assert_eq!(s.primary().head, 7, "the new caret is primary");
        assert!(s.primary().is_empty());
        let s = add_cursor_vertical(&d, &s, 1);
        let heads: Vec<_> = s.iter().map(|r| r.head).collect();
        assert_eq!(
            heads,
            vec![3, 7, 11],
            "the short line does not lose the column"
        );

        let up = add_cursor_vertical(&d, &caret(10), -1);
        assert_eq!(up.iter().map(|r| r.head).collect::<Vec<_>>(), vec![7, 10]);
        // Nothing above the first line or below the last.
        assert_eq!(add_cursor_vertical(&d, &caret(2), -1).len(), 1);
        assert_eq!(add_cursor_vertical(&d, &caret(10), 1).len(), 1);
    }

    #[test]
    fn column_selection_follows_the_box_and_clamps_short_lines() {
        let d = doc("abcd\nef\nghij");
        let s = column_selection(&d, (0, 1), (2, 3));
        assert_eq!(ranges(&s), vec![(1, 3), (6, 7), (9, 11)]);
        assert_eq!(
            s.primary(),
            Selection::new(9, 11),
            "primary is the head line"
        );

        let back = column_selection(&d, (2, 3), (0, 1));
        assert_eq!(ranges(&back), vec![(3, 1), (7, 6), (11, 9)]);
        assert_eq!(back.primary_index(), 0);

        // Zero width: a caret per line, clamped to each line's end.
        let carets = column_selection(&d, (0, 2), (2, 2));
        let heads: Vec<_> = carets.iter().map(|r| r.head).collect();
        assert_eq!(heads, vec![2, 7, 10]);
        assert!(carets.iter().all(Selection::is_empty));

        // Lines and columns past the end clamp.
        let far = column_selection(&d, (1, 50), (99, 60));
        assert_eq!(ranges(&far), vec![(7, 7), (12, 12)]);
    }

    #[test]
    fn column_selection_keeps_graphemes_whole() {
        // Line 0 is "e\u{301}x": column 1 falls inside the first cluster.
        let d = doc("e\u{301}x\nabc");
        let s = column_selection(&d, (0, 1), (1, 2));
        assert_eq!(ranges(&s), vec![(0, 2), (5, 6)]);
    }

    fn arb_text() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            prop_oneof![
                Just("a"),
                Just("b"),
                Just("ab"),
                Just(" "),
                Just("\n"),
                Just("\u{e9}"),
                Just("\u{1F468}\u{200D}\u{1F469}"),
            ],
            1..60,
        )
        .prop_map(|v| v.concat())
    }

    fn assert_ordered_in_bounds(d: &Document, s: &SelectionSet) -> Result<(), TestCaseError> {
        let mut prev_end = 0;
        for r in s.iter() {
            prop_assert!(r.end() <= d.len_chars());
            prop_assert!(r.start() >= prev_end, "selections overlap or are unsorted");
            prev_end = r.end();
        }
        Ok(())
    }

    proptest! {
        #[test]
        fn all_occurrences_are_identical_ordered_and_include_the_origin(
            text in arb_text(),
            start in 0usize..60,
            width in 1usize..5,
        ) {
            let d = doc(&text);
            let len = d.len_chars();
            let start = start % len;
            let end = (start + width).min(len);
            let origin = Selection::new(start, end);
            let s = select_all_occurrences(&d, &SelectionSet::single(origin));
            let needle = text_of(&d, &origin);
            assert_ordered_in_bounds(&d, &s)?;
            prop_assert!(s.iter().all(|r| text_of(&d, r) == needle));
            prop_assert!(s.iter().any(|r| r.start() == start));
        }

        #[test]
        fn repeated_next_occurrence_grows_without_overlap_and_terminates(
            text in arb_text(),
            start in 0usize..60,
            width in 1usize..4,
        ) {
            let d = doc(&text);
            let len = d.len_chars();
            let start = start % len;
            let origin = Selection::new(start, (start + width).min(len));
            let needle = text_of(&d, &origin);
            let mut s = SelectionSet::single(origin);
            let mut last = 1;
            for _ in 0..(len + 2) {
                s = select_next_occurrence(&d, &s);
                assert_ordered_in_bounds(&d, &s)?;
                prop_assert!(s.len() == last || s.len() == last + 1);
                prop_assert!(s.iter().all(|r| text_of(&d, r) == needle));
                last = s.len();
            }
            // Once it stops growing it stays stable.
            prop_assert_eq!(select_next_occurrence(&d, &s), s);
        }

        #[test]
        fn column_selection_has_one_bounded_selection_per_line(
            text in arb_text(),
            a in (0usize..10, 0usize..12),
            h in (0usize..10, 0usize..12),
        ) {
            let d = doc(&text);
            let s = column_selection(&d, a, h);
            let last = d.line_count() - 1;
            let (first, final_line) = (a.0.min(last).min(h.0.min(last)), a.0.min(last).max(h.0.min(last)));
            prop_assert_eq!(s.len(), final_line - first + 1);
            for (i, r) in s.iter().enumerate() {
                let line = first + i;
                prop_assert!(r.start() >= d.line_to_char(line));
                prop_assert!(r.end() <= d.line_end_char(line));
            }
        }

        #[test]
        fn adding_cursors_never_drops_or_duplicates_carets(
            text in arb_text(),
            at in 0usize..60,
            presses in proptest::collection::vec(any::<bool>(), 1..8),
        ) {
            let d = doc(&text);
            let mut s = caret(at % (d.len_chars() + 1));
            for down in presses {
                let before = s.len();
                s = add_cursor_vertical(&d, &s, if down { 1 } else { -1 });
                prop_assert!(s.len() >= before);
                assert_ordered_in_bounds(&d, &s)?;
            }
        }
    }
}
