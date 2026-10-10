//! Viewport snapshots (M1-05).
//!
//! The UI renders from one immutable snapshot per frame: the visible lines,
//! the selection spans and carets on those lines, and the primary cursor
//! for the status bar. One call per frame keeps FFI traffic constant no
//! matter how many cursors exist.
//!
//! Very long lines (minified files) are cut to `MAX_LINE_DISPLAY_CHARS` so
//! one frame never copies or lays out megabytes of text. Anything past the
//! cut is not drawn; the line is flagged `truncated`.
//!
//! Columns in the snapshot are **UTF-16 code units** within the line,
//! because that is what `NSString`, Core Text (`CTLine`), and
//! `NSTextInputClient` index by. The core keeps char offsets internally.

use crate::document::Document;
use crate::history::Editor;
use crate::selection::SelectionSet;

/// Lines longer than this many chars are cut off for display.
pub const MAX_LINE_DISPLAY_CHARS: usize = 10_000;

/// The part of `line` that is displayed (and hit-tested): at most
/// `MAX_LINE_DISPLAY_CHARS` chars, without the line break.
pub fn display_text(doc: &Document, line: usize) -> String {
    let start = doc.line_to_char(line);
    let end = doc.line_end_char(line).min(start + MAX_LINE_DISPLAY_CHARS);
    doc.rope().slice(start..end).to_string()
}

/// A selected range on one line, in UTF-16 columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineSpan {
    pub start: usize,
    pub end: usize,
    /// The selection continues past the end of this line (its newline is
    /// selected), so the renderer should extend the highlight to the edge.
    pub includes_eol: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewportLine {
    /// Zero-based document line number.
    pub line: usize,
    /// Line text without its `\n`.
    pub text: String,
    /// Selected spans on this line, sorted and non-overlapping.
    pub selections: Vec<LineSpan>,
    /// Caret columns on this line, sorted. Every selection has a caret at
    /// its head, as in VSCode.
    pub carets: Vec<usize>,
    /// IME marked text on this line (drawn underlined), if any.
    pub marked: Option<LineSpan>,
    /// `text` is only the first `MAX_LINE_DISPLAY_CHARS` chars of the line.
    pub truncated: bool,
}

/// Primary cursor position for the status bar and scroll-into-view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CursorInfo {
    pub line: usize,
    /// Column in chars (what the status bar shows, 0-based).
    pub col: usize,
    /// Column in UTF-16 code units (for locating it in a `CTLine`).
    pub col_utf16: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewportSnapshot {
    /// `Document::revision` the snapshot was taken at.
    pub revision: u64,
    /// Total lines in the document (for the virtual scroll height).
    pub line_count: usize,
    pub lines: Vec<ViewportLine>,
    pub primary: CursorInfo,
    pub selection_count: usize,
}

impl Editor {
    /// Snapshot of `count` lines starting at `first_line` (clamped).
    pub fn viewport(&self, first_line: usize, count: usize) -> ViewportSnapshot {
        let mut snap = snapshot(&self.doc, &self.sel, first_line, count);
        if let Some((start, end)) = self.marked_range_chars() {
            for l in &mut snap.lines {
                let ls = self.doc.line_to_char(l.line);
                let le = self
                    .doc
                    .line_end_char(l.line)
                    .min(ls + MAX_LINE_DISPLAY_CHARS);
                if start < le && end > ls {
                    let col = |pos: usize| self.doc.char_to_utf16(pos).col;
                    l.marked = Some(LineSpan {
                        start: col(start.max(ls)),
                        end: col(end.min(le)),
                        includes_eol: false,
                    });
                }
            }
        }
        snap
    }
}

pub fn snapshot(
    doc: &Document,
    sel: &SelectionSet,
    first_line: usize,
    count: usize,
) -> ViewportSnapshot {
    let rope = doc.rope();
    let line_count = doc.line_count();
    let first = first_line.min(line_count);
    let last = first.saturating_add(count).min(line_count);
    let ranges = sel.as_slice();

    let mut lines = Vec::with_capacity(last - first);
    for line in first..last {
        let start = rope.line_to_char(line);
        let end = doc.line_end_char(line);
        let truncated = end - start > MAX_LINE_DISPLAY_CHARS;
        // Last char that is displayed; equals `end` for normal lines.
        let shown = if truncated {
            start + MAX_LINE_DISPLAY_CHARS
        } else {
            end
        };
        let start16 = rope.char_to_utf16_cu(start);
        let col16 = |pos: usize| rope.char_to_utf16_cu(pos) - start16;
        let slice = rope.slice(start..shown);
        let len16 = slice.len_utf16_cu();

        // Selections are sorted and disjoint, so both their ends and their
        // heads are sorted; binary search to the first relevant one.
        let mut spans = Vec::new();
        let from = ranges.partition_point(|s| s.end() <= start);
        for s in ranges[from..].iter().take_while(|s| s.start() <= end) {
            if s.is_empty() || (truncated && s.start() >= shown) {
                continue;
            }
            // The line break is only visible when the whole line is shown.
            let includes_eol = !truncated && s.end() > end;
            spans.push(LineSpan {
                start: if s.start() > start {
                    col16(s.start())
                } else {
                    0
                },
                end: if s.end() > shown {
                    len16
                } else {
                    col16(s.end())
                },
                includes_eol,
            });
        }

        let from = ranges.partition_point(|s| s.head < start);
        let carets = ranges[from..]
            .iter()
            .take_while(|s| s.head <= end)
            .filter(|s| s.head <= shown)
            .map(|s| col16(s.head))
            .collect();

        lines.push(ViewportLine {
            line,
            text: slice.to_string(),
            selections: spans,
            carets,
            marked: None,
            truncated,
        });
    }

    let head = sel.primary().head;
    let pos = doc.char_to_position(head);
    ViewportSnapshot {
        revision: doc.revision(),
        line_count,
        lines,
        primary: CursorInfo {
            line: pos.line,
            col: pos.col,
            col_utf16: doc.char_to_utf16(head).col,
        },
        selection_count: sel.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::Selection;

    fn snap(text: &str, sels: Vec<Selection>, first: usize, count: usize) -> ViewportSnapshot {
        let doc = Document::from_text(text);
        let sel = SelectionSet::new(sels, 0, &doc);
        snapshot(&doc, &sel, first, count)
    }

    fn span(start: usize, end: usize, includes_eol: bool) -> LineSpan {
        LineSpan {
            start,
            end,
            includes_eol,
        }
    }

    #[test]
    fn lines_are_clamped_and_numbered() {
        let s = snap("a\nb\nc", vec![Selection::caret(0)], 1, 10);
        assert_eq!(s.line_count, 3);
        let texts: Vec<_> = s.lines.iter().map(|l| (l.line, l.text.as_str())).collect();
        assert_eq!(texts, vec![(1, "b"), (2, "c")]);
        assert!(snap("a", vec![], 5, 10).lines.is_empty());
    }

    #[test]
    fn multi_line_selection_spans() {
        // Select from "b|c" on line 0 to "d|e" on line 2.
        let s = snap("abc\n\ndef", vec![Selection::new(2, 6)], 0, 3);
        assert_eq!(s.lines[0].selections, vec![span(2, 3, true)]);
        assert_eq!(s.lines[1].selections, vec![span(0, 0, true)]);
        assert_eq!(s.lines[2].selections, vec![span(0, 1, false)]);
        // Caret at the head only.
        assert_eq!(s.lines[2].carets, vec![1]);
        assert!(s.lines[0].carets.is_empty());
    }

    #[test]
    fn selection_ending_at_line_start_does_not_touch_that_line() {
        let s = snap("ab\ncd", vec![Selection::new(0, 3)], 0, 2);
        assert_eq!(s.lines[0].selections, vec![span(0, 2, true)]);
        assert!(s.lines[1].selections.is_empty());
        assert_eq!(s.lines[1].carets, vec![0]);
    }

    #[test]
    fn columns_are_utf16() {
        // 😀 is 2 UTF-16 units.
        let s = snap("😀ab", vec![Selection::new(1, 3)], 0, 1);
        assert_eq!(s.lines[0].selections, vec![span(2, 4, false)]);
        assert_eq!(s.lines[0].carets, vec![4]);
        assert_eq!(
            s.primary,
            CursorInfo {
                line: 0,
                col: 3,
                col_utf16: 4
            }
        );
    }

    #[test]
    fn many_cursors_only_visible_ones_reported() {
        let text: String = (0..1_000).map(|i| format!("line {i}\n")).collect();
        let doc = Document::from_text(&text);
        let carets = (0..1_000)
            .map(|i| Selection::caret(doc.line_to_char(i)))
            .collect();
        let sel = SelectionSet::new(carets, 500, &doc);
        let s = snapshot(&doc, &sel, 100, 10);
        assert_eq!(s.lines.len(), 10);
        assert!(s.lines.iter().all(|l| l.carets == vec![0]));
        assert_eq!(s.selection_count, 1_000);
        assert_eq!(s.primary.line, 500);
    }

    #[test]
    fn caret_at_end_of_line_and_document() {
        let s = snap("ab\n", vec![Selection::caret(2), Selection::caret(3)], 0, 2);
        assert_eq!(s.lines[0].carets, vec![2]);
        assert_eq!(s.lines[1].carets, vec![0]);
        assert_eq!(s.lines[1].text, "");
    }

    #[test]
    fn revision_tracks_edits() {
        let mut doc = Document::from_text("x");
        let r0 = doc.revision();
        doc.insert(0, "y");
        assert!(doc.revision() > r0);
        let sel = SelectionSet::default();
        assert_eq!(snapshot(&doc, &sel, 0, 1).revision, doc.revision());
    }

    #[test]
    fn long_lines_are_cut_for_display() {
        let long = "x".repeat(MAX_LINE_DISPLAY_CHARS + 500);
        let text = format!("short\n{long}\nend");
        let s = snap(&text, vec![Selection::caret(0)], 0, 3);
        assert!(!s.lines[0].truncated);
        assert!(s.lines[1].truncated);
        assert_eq!(s.lines[1].text.chars().count(), MAX_LINE_DISPLAY_CHARS);
        assert!(!s.lines[2].truncated);
    }

    #[test]
    fn selections_and_carets_past_the_cut_are_clamped_or_dropped() {
        let long = "x".repeat(MAX_LINE_DISPLAY_CHARS + 500);
        let cut = MAX_LINE_DISPLAY_CHARS;
        // Disjoint on purpose: overlapping selections would be merged.
        let sels = vec![
            Selection::caret(5),                  // primary, visible
            Selection::new(10, cut + 100),        // crosses the cut
            Selection::caret(cut + 200),          // hidden
            Selection::new(cut + 300, cut + 400), // hidden
        ];
        let s = snap(&long, sels, 0, 1);
        let l = &s.lines[0];
        assert_eq!(
            l.selections,
            vec![span(10, cut, false)],
            "clamped to the cut, no eol"
        );
        assert!(l.carets.contains(&5));
        assert!(l.carets.iter().all(|&c| c <= cut), "{:?}", l.carets);
        assert!(!l.carets.contains(&(cut + 200)));
        assert_eq!(s.primary.col, 5);
        assert_eq!(s.selection_count, 4, "hidden selections still exist");
    }

    #[test]
    fn the_cut_never_splits_a_surrogate_pair() {
        let long = "\u{1F600}".repeat(MAX_LINE_DISPLAY_CHARS + 5);
        let s = snap(&long, vec![Selection::caret(0)], 0, 1);
        assert!(s.lines[0].truncated);
        assert_eq!(s.lines[0].text.chars().count(), MAX_LINE_DISPLAY_CHARS);
        assert_eq!(
            s.lines[0].text.encode_utf16().count(),
            2 * MAX_LINE_DISPLAY_CHARS
        );
        let doc = Document::from_text(&long);
        assert_eq!(display_text(&doc, 0), s.lines[0].text);
        assert_eq!(display_text(&Document::from_text("ab\ncd"), 1), "cd");
    }

    #[test]
    fn large_file_threshold() {
        assert!(!Document::from_text("small").is_large());
        let big = Document::from_text(&"a".repeat(crate::document::LARGE_FILE_BYTES + 1));
        assert!(big.is_large());
    }
}
