//! Go to Line input and back/forward navigation history (M2-08).
//!
//! The history works like a browser's, keyed by an opaque document key
//! (a resolved file path, or an id for untitled buffers):
//!
//! - before a jump, the position being left is recorded;
//! - positions within [`NEAR_LINES`] of the last entry in the same
//!   document replace it, so small moves do not fill the history;
//! - going back pushes the current position on the forward stack, and
//!   any new jump clears that stack.
//!
//! The UI decides what counts as a jump (switching tabs, Go to Line,
//! find next, a click far away); [`NavHistory::record_jump`] drops the
//! ones that stayed close.

use std::collections::VecDeque;

use crate::document::Document;

/// Entries kept in each direction.
pub const MAX_ENTRIES: usize = 50;
/// Moves of at most this many lines within one document are not jumps.
pub const NEAR_LINES: usize = 10;

/// A cursor position in a document. `col` counts chars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub key: String,
    pub line: usize,
    pub col: usize,
}

impl Location {
    pub fn new(key: impl Into<String>, line: usize, col: usize) -> Self {
        Self {
            key: key.into(),
            line,
            col,
        }
    }

    fn is_near(&self, other: &Location) -> bool {
        self.key == other.key && self.line.abs_diff(other.line) <= NEAR_LINES
    }
}

#[derive(Debug, Default)]
pub struct NavHistory {
    back: VecDeque<Location>,
    forward: VecDeque<Location>,
}

impl NavHistory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `from` as a place to come back to, and forgets the
    /// forward history.
    pub fn record(&mut self, from: Location) {
        self.forward.clear();
        push(&mut self.back, from);
    }

    /// Records `from` if moving to `to` was a real jump: another document,
    /// or more than [`NEAR_LINES`] lines away. Returns whether it did.
    pub fn record_jump(&mut self, from: Location, to: &Location) -> bool {
        if from.is_near(to) {
            return false;
        }
        self.record(from);
        true
    }

    /// The previous location, skipping entries next to `current`.
    /// `current` (if any) becomes the first forward entry.
    pub fn back(&mut self, current: Option<Location>) -> Option<Location> {
        step(&mut self.back, &mut self.forward, current)
    }

    /// The opposite of [`back`](Self::back).
    pub fn forward(&mut self, current: Option<Location>) -> Option<Location> {
        step(&mut self.forward, &mut self.back, current)
    }

    /// Drops every entry for `key`, for example a closed untitled buffer
    /// that can no longer be shown.
    pub fn forget(&mut self, key: &str) {
        self.back.retain(|l| l.key != key);
        self.forward.retain(|l| l.key != key);
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }
}

/// Pushes onto a stack, merging with a nearby top entry and capping size.
fn push(stack: &mut VecDeque<Location>, loc: Location) {
    if let Some(top) = stack.back_mut()
        && top.is_near(&loc)
    {
        *top = loc;
        return;
    }
    stack.push_back(loc);
    if stack.len() > MAX_ENTRIES {
        stack.pop_front();
    }
}

fn step(
    from: &mut VecDeque<Location>,
    to: &mut VecDeque<Location>,
    current: Option<Location>,
) -> Option<Location> {
    while let Some(loc) = from.pop_back() {
        // Going "back" to where the cursor already is would do nothing.
        if current.as_ref().is_some_and(|c| c.is_near(&loc)) {
            continue;
        }
        if let Some(c) = current {
            push(to, c);
        }
        return Some(loc);
    }
    None
}

/// Char column of the first non-blank char of `line` (clamped), or the
/// line length if it is all blank. Reads only the leading whitespace.
pub fn first_non_blank_col(doc: &Document, line: usize) -> usize {
    let line = line.min(doc.line_count() - 1);
    let start = doc.line_to_char(line);
    let len = doc.line_len_chars(line);
    doc.rope()
        .chars_at(start)
        .take(len)
        .take_while(|c| *c == ' ' || *c == '\t')
        .count()
}

/// Where Go to Line goes. Zero-based; `col` counts chars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineTarget {
    pub line: usize,
    pub col: Option<usize>,
}

/// Parses Go to Line input, as VSCode does: `42`, `42:7`, `42,7`, an
/// optional leading `:`, and negative lines counted from the end (`-1`
/// is the last line). Lines past the end go to the last line. `None` for
/// empty or malformed input, line 0, or column 0.
pub fn parse_line_target(input: &str, line_count: usize) -> Option<LineTarget> {
    let text = input.trim();
    let text = text.strip_prefix(':').unwrap_or(text).trim();
    let (line_part, col_part) = match text.find([':', ',']) {
        Some(i) => (&text[..i], Some(text[i + 1..].trim())),
        None => (text, None),
    };
    let n: i64 = line_part.trim().parse().ok()?;
    let last = line_count.max(1) - 1;
    let line = match n {
        0 => return None,
        n if n > 0 => usize::try_from(n - 1).unwrap_or(usize::MAX).min(last),
        n => {
            let back = usize::try_from(n.unsigned_abs()).unwrap_or(usize::MAX);
            line_count.max(1).saturating_sub(back)
        }
    };
    let col = match col_part {
        None | Some("") => None,
        Some(c) => match c.parse::<u64>().ok()? {
            0 => return None,
            c => Some(usize::try_from(c - 1).unwrap_or(usize::MAX)),
        },
    };
    Some(LineTarget { line, col })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(key: &str, line: usize) -> Location {
        Location::new(key, line, 0)
    }

    fn target(line: usize, col: Option<usize>) -> Option<LineTarget> {
        Some(LineTarget { line, col })
    }

    #[test]
    fn first_non_blank_column() {
        let doc = Document::from_text("x\n  \t y\n   \n");
        assert_eq!(first_non_blank_col(&doc, 0), 0);
        assert_eq!(first_non_blank_col(&doc, 1), 4);
        assert_eq!(first_non_blank_col(&doc, 2), 3, "blank line: its end");
        assert_eq!(
            first_non_blank_col(&doc, 99),
            0,
            "clamped to the last, empty line"
        );
    }

    #[test]
    fn parses_lines_and_columns() {
        assert_eq!(parse_line_target("42", 100), target(41, None));
        assert_eq!(parse_line_target(" :42 ", 100), target(41, None));
        assert_eq!(parse_line_target("42:7", 100), target(41, Some(6)));
        assert_eq!(parse_line_target("42,7", 100), target(41, Some(6)));
        assert_eq!(parse_line_target("42:", 100), target(41, None));
        assert_eq!(parse_line_target("1", 1), target(0, None));
    }

    #[test]
    fn clamps_and_counts_negative_lines_from_the_end() {
        assert_eq!(parse_line_target("500", 100), target(99, None));
        assert_eq!(parse_line_target("-1", 100), target(99, None));
        assert_eq!(parse_line_target("-100", 100), target(0, None));
        assert_eq!(parse_line_target("-500", 100), target(0, None));
        assert_eq!(
            parse_line_target("99999999999999999999", 10),
            None,
            "overflow"
        );
        assert_eq!(parse_line_target("3", 0), target(0, None), "empty document");
    }

    #[test]
    fn rejects_malformed_input() {
        for bad in ["", " ", ":", "abc", "0", "4x", "4:x", "4:0", "4:-2", "1.5"] {
            assert_eq!(parse_line_target(bad, 100), None, "{bad:?}");
        }
    }

    #[test]
    fn back_and_forward_walk_the_history() {
        let mut h = NavHistory::new();
        h.record(at("a", 0));
        h.record(at("a", 50));
        assert!(h.can_go_back());
        assert!(!h.can_go_forward());

        assert_eq!(h.back(Some(at("b", 3))), Some(at("a", 50)));
        assert_eq!(h.back(Some(at("a", 50))), Some(at("a", 0)));
        assert_eq!(h.back(Some(at("a", 0))), None);
        assert_eq!(h.forward(Some(at("a", 0))), Some(at("a", 50)));
        assert_eq!(h.forward(Some(at("a", 50))), Some(at("b", 3)));
        assert_eq!(h.forward(Some(at("b", 3))), None);
        assert_eq!(h.back(Some(at("b", 3))), Some(at("a", 50)));
    }

    #[test]
    fn a_new_jump_clears_forward() {
        let mut h = NavHistory::new();
        h.record(at("a", 0));
        h.back(Some(at("a", 80)));
        assert!(h.can_go_forward());
        h.record(at("a", 40));
        assert!(!h.can_go_forward());
    }

    #[test]
    fn nearby_positions_merge_and_small_moves_are_not_jumps() {
        let mut h = NavHistory::new();
        h.record(at("a", 20));
        h.record(at("a", 25));
        assert_eq!(h.back(None), Some(at("a", 25)));
        assert_eq!(h.back(None), None, "merged into one entry");

        assert!(!h.record_jump(at("a", 5), &at("a", 15)));
        assert!(!h.can_go_back());
        assert!(h.record_jump(at("a", 5), &at("a", 16)));
        assert!(h.record_jump(at("a", 30), &at("b", 30)), "other document");
    }

    #[test]
    fn going_back_skips_where_the_cursor_already_is() {
        let mut h = NavHistory::new();
        h.record(at("a", 0));
        h.record(at("a", 100));
        // The user walked back to line 98 by hand.
        assert_eq!(h.back(Some(at("a", 98))), Some(at("a", 0)));
    }

    #[test]
    fn forget_drops_a_document_and_size_is_capped() {
        let mut h = NavHistory::new();
        for i in 0..(MAX_ENTRIES + 10) {
            h.record(at("a", i * 100));
        }
        h.record(at("u", 0));
        h.forget("u");
        let mut count = 0;
        let mut last = None;
        while let Some(l) = h.back(None) {
            count += 1;
            last = Some(l);
        }
        assert_eq!(
            count,
            MAX_ENTRIES - 1,
            "one slot went to the forgotten entry"
        );
        assert_eq!(last, Some(at("a", 11 * 100)), "oldest entries dropped");
    }
}
