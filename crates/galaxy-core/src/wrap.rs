//! Soft word wrap.
//!
//! A logical line longer than the editor is wide is shown on several visual
//! rows. The core owns the row breaks so drawing, hit-testing and vertical
//! cursor movement all agree on them. Widths are counted in monospace
//! columns: one per grapheme, two for wide (CJK, emoji) graphemes and four
//! for a tab. Only displayed text is wrapped, so a line cut at
//! `MAX_LINE_DISPLAY_CHARS` is wrapped up to that cut.

use unicode_segmentation::UnicodeSegmentation;

use crate::document::Document;
use crate::viewport::display_text;

/// Narrowest usable wrap width. Anything smaller is raised to it so every
/// row holds at least one wide grapheme.
pub const MIN_COLUMNS: usize = 2;

fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x3FFFD)
}

fn char_width(c: char) -> usize {
    match c {
        '\t' => 4,
        c if is_wide(c) => 2,
        _ => 1,
    }
}

fn grapheme_width(g: &str) -> usize {
    g.chars().next().map_or(0, char_width)
}

/// Char offsets (within `text`) where every row after the first starts.
/// Rows break after the last blank that fits, or inside a word that is
/// wider than a row.
pub fn line_breaks(text: &str, columns: usize) -> Vec<usize> {
    let cols = columns.max(MIN_COLUMNS);
    let mut breaks = Vec::new();
    let mut pos = 0; // char offset of the next grapheme
    let mut row_width = 0;
    // Char offset just after the latest blank on this row, and the width
    // of what follows it; that is what moves down if we break there.
    let mut last_blank: Option<usize> = None;
    let mut width_after_blank = 0;

    for g in text.graphemes(true) {
        let w = grapheme_width(g);
        while row_width + w > cols && row_width > 0 {
            if let Some(at) = last_blank.take() {
                breaks.push(at);
                row_width = width_after_blank;
            } else {
                breaks.push(pos);
                row_width = 0;
            }
            width_after_blank = 0;
        }
        row_width += w;
        pos += g.chars().count();
        if g == " " || g == "\t" {
            last_blank = Some(pos);
            width_after_blank = 0;
        } else if last_blank.is_some() {
            width_after_blank += w;
        }
    }
    breaks
}

/// Rows `line` takes at `columns`. Lines that fit are decided from a
/// width sum alone, with no grapheme segmentation.
pub fn rows_in_line(doc: &Document, line: usize, columns: usize) -> usize {
    let cols = columns.max(MIN_COLUMNS);
    let start = doc.line_to_char(line);
    let end = doc
        .line_end_char(line)
        .min(start + crate::viewport::MAX_LINE_DISPLAY_CHARS);
    let mut width = 0;
    for c in doc.rope().slice(start..end).chars() {
        width += char_width(c);
        if width > cols {
            return line_breaks(&display_text(doc, line), cols).len() + 1;
        }
    }
    1
}

/// Char offsets, within the line, where rows after the first start.
pub fn breaks_for_line(doc: &Document, line: usize, columns: usize) -> Vec<usize> {
    if rows_in_line(doc, line, columns) == 1 {
        return Vec::new();
    }
    line_breaks(&display_text(doc, line), columns)
}

/// First visual row of every line, for one document revision, width and set
/// of folded ranges. Folded lines take no rows; with `columns == None` every
/// other line takes exactly one.
#[derive(Debug, Clone)]
pub struct WrapMap {
    columns: Option<usize>,
    revision: u64,
    hidden: Vec<(usize, usize)>,
    /// `first_row[line]`; one extra entry holds the total.
    first_row: Vec<u64>,
}

impl WrapMap {
    pub fn build(doc: &Document, columns: usize) -> Self {
        Self::build_with(doc, Some(columns), Vec::new())
    }

    /// `hidden` holds sorted, disjoint `(first, last)` folded line ranges.
    pub fn build_with(doc: &Document, columns: Option<usize>, hidden: Vec<(usize, usize)>) -> Self {
        let lines = doc.line_count();
        let mut first_row = Vec::with_capacity(lines + 1);
        let mut total = 0u64;
        let mut next = 0;
        for line in 0..lines {
            first_row.push(total);
            while next < hidden.len() && hidden[next].1 < line {
                next += 1;
            }
            if hidden
                .get(next)
                .is_some_and(|&(a, b)| a <= line && line <= b)
            {
                continue;
            }
            total += match columns {
                Some(columns) => rows_in_line(doc, line, columns) as u64,
                None => 1,
            };
        }
        first_row.push(total);
        Self {
            columns,
            revision: doc.revision(),
            hidden,
            first_row,
        }
    }

    pub fn is_current(
        &self,
        doc: &Document,
        columns: Option<usize>,
        hidden: &[(usize, usize)],
    ) -> bool {
        self.columns == columns && self.revision == doc.revision() && self.hidden == hidden
    }

    pub fn total_rows(&self) -> u64 {
        self.first_row.last().copied().unwrap_or(0)
    }

    pub fn line_count(&self) -> usize {
        self.first_row.len().saturating_sub(1)
    }

    /// First row of `line`; lines past the end clamp to the last one.
    pub fn row_of_line(&self, line: usize) -> u64 {
        match self.line_count() {
            0 => 0,
            n => self.first_row[line.min(n - 1)],
        }
    }

    /// Rows `line` takes.
    pub fn rows_of_line(&self, line: usize) -> u64 {
        match self.line_count() {
            0 => 1,
            n => {
                let l = line.min(n - 1);
                self.first_row[l + 1] - self.first_row[l]
            }
        }
    }

    /// `(line, row within that line)` for a global row. Rows past the end
    /// clamp to the last row of the document.
    pub fn line_at_row(&self, row: u64) -> (usize, usize) {
        let n = self.line_count();
        if n == 0 {
            return (0, 0);
        }
        let row = row.min(self.total_rows().saturating_sub(1));
        let line = self.first_row[..n].partition_point(|&first| first <= row) - 1;
        (line, (row - self.first_row[line]) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> Document {
        Document::from_text(text)
    }

    #[test]
    fn short_text_has_no_breaks() {
        assert!(line_breaks("hello", 10).is_empty());
        assert!(line_breaks("", 10).is_empty());
        assert!(line_breaks("exactly10!", 10).is_empty());
    }

    #[test]
    fn breaks_after_the_last_blank_that_fits() {
        // "aaa bbb ccc" at 8 columns: "aaa bbb " | "ccc"
        assert_eq!(line_breaks("aaa bbb ccc", 8), vec![8]);
        // "hello big world" at 8: "hello " | "big " | "world"
        assert_eq!(line_breaks("hello big world", 8), vec![6, 10]);
        // At 9 "big world" fits on the second row exactly.
        assert_eq!(line_breaks("hello big world", 9), vec![6]);
    }

    #[test]
    fn a_word_wider_than_a_row_breaks_inside() {
        assert_eq!(line_breaks("abcdefghij", 4), vec![4, 8]);
        // The blank is used only when it is on the current row.
        assert_eq!(line_breaks("ab cdefghijkl", 4), vec![3, 7, 11]);
    }

    #[test]
    fn wide_graphemes_count_double_and_never_split() {
        // Each CJK char is two columns: two fit in 4 columns.
        assert_eq!(line_breaks("漢字漢字漢", 4), vec![2, 4]);
        // A combining sequence is one grapheme, one column.
        let e = "e\u{301}e\u{301}e\u{301}";
        // Two graphemes per row; the third starts at char offset 4.
        assert_eq!(line_breaks(e, 2), vec![4]);
        // Never an offset inside "e" + accent (offset 1 or 3).
        assert!(line_breaks(e, 2).iter().all(|b| b % 2 == 0));
    }

    #[test]
    fn tabs_are_four_columns() {
        assert_eq!(line_breaks("\t\tx", 8), vec![2]);
    }

    #[test]
    fn narrow_widths_are_raised_to_the_minimum() {
        // Would loop forever or panic with a width of 0 or 1.
        assert_eq!(line_breaks("abcd", 0), vec![2]);
        assert_eq!(line_breaks("abcd", 1), vec![2]);
    }

    #[test]
    fn map_counts_rows_and_finds_lines() {
        let d = doc("short\nabcdefghij\n\nxy");
        let map = WrapMap::build(&d, 4);
        // 5 chars at 4 columns: 2 rows; 10 chars: 3 rows; empty: 1; "xy": 1
        assert_eq!(map.total_rows(), 2 + 3 + 1 + 1);
        assert_eq!(map.row_of_line(0), 0);
        assert_eq!(map.row_of_line(1), 2);
        assert_eq!(map.row_of_line(2), 5);
        assert_eq!(map.row_of_line(3), 6);
        assert_eq!(map.rows_of_line(1), 3);
        assert_eq!(map.line_at_row(0), (0, 0));
        assert_eq!(map.line_at_row(1), (0, 1));
        assert_eq!(map.line_at_row(4), (1, 2));
        assert_eq!(map.line_at_row(5), (2, 0));
        assert_eq!(map.line_at_row(6), (3, 0));
        assert_eq!(map.line_at_row(99), (3, 0), "past the end clamps");
    }

    #[test]
    fn map_follows_the_document_and_the_width() {
        let d = doc("abcdefgh");
        let map = WrapMap::build(&d, 4);
        assert!(map.is_current(&d, Some(4), &[]));
        assert!(!map.is_current(&d, Some(5), &[]));
        assert!(!map.is_current(&d, Some(4), &[(0, 0)]));
        assert_eq!(map.total_rows(), 2);
        assert_eq!(WrapMap::build(&d, 8).total_rows(), 1);
    }

    #[test]
    fn folded_lines_take_no_rows() {
        let d = doc("a\nb\nc\nd\ne");
        let map = WrapMap::build_with(&d, None, vec![(1, 2)]);
        assert_eq!(map.total_rows(), 3);
        assert_eq!(map.row_of_line(0), 0);
        assert_eq!(map.rows_of_line(1), 0);
        assert_eq!(map.rows_of_line(2), 0);
        assert_eq!(
            map.row_of_line(3),
            1,
            "the line after a fold follows its header"
        );
        assert_eq!(map.row_of_line(4), 2);
        assert_eq!(map.line_at_row(0), (0, 0));
        assert_eq!(
            map.line_at_row(1),
            (3, 0),
            "a row never lands on a hidden line"
        );
        assert_eq!(map.line_at_row(2), (4, 0));
    }

    #[test]
    fn folds_and_wrap_combine() {
        let d = doc("abcdefgh\nhidden\nxy");
        let map = WrapMap::build_with(&d, Some(4), vec![(1, 1)]);
        assert_eq!(map.total_rows(), 2 + 0 + 1);
        assert_eq!(map.row_of_line(2), 2);
    }

    #[test]
    fn empty_document_is_one_row() {
        let map = WrapMap::build(&doc(""), 10);
        assert_eq!(map.total_rows(), 1);
        assert_eq!(map.line_at_row(0), (0, 0));
    }

    #[test]
    fn breaks_for_line_matches_row_count() {
        let d = doc("one two three four five six");
        for cols in [4, 7, 10, 20, 40] {
            let breaks = breaks_for_line(&d, 0, cols);
            assert_eq!(breaks.len() + 1, rows_in_line(&d, 0, cols), "cols {cols}");
            assert!(breaks.windows(2).all(|w| w[0] < w[1]), "ascending");
        }
    }
}
