//! Copy, cut and paste (M1-11), with VSCode's line semantics.
//!
//! * Copy or cut with only carets takes the whole line (`linewise`).
//! * Pasting linewise text at a caret inserts it above the caret's line and
//!   keeps the caret in the same column of its (now lower) line.
//!
//! The `linewise` flag has to survive the round trip through the system
//! pasteboard, so the UI stores it next to the text.

use std::collections::BTreeSet;

use crate::document::{Document, normalize_line_breaks};
use crate::edit::{Item, Transaction, finish_with_carets};
use crate::selection::SelectionSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copied {
    pub text: String,
    /// `true` when the text is whole lines taken from carets.
    pub linewise: bool,
}

fn all_carets(sel: &SelectionSet) -> bool {
    sel.iter().all(|s| s.is_empty())
}

/// Lines touched by the carets, each once, in order.
fn caret_lines(doc: &Document, sel: &SelectionSet) -> BTreeSet<usize> {
    sel.iter().map(|s| doc.char_to_line(s.head)).collect()
}

pub fn copy(doc: &Document, sel: &SelectionSet) -> Copied {
    if all_carets(sel) {
        let mut text = String::new();
        for line in caret_lines(doc, sel) {
            let (start, end) = (doc.line_to_char(line), doc.line_end_char(line));
            text.extend(doc.rope().slice(start..end).chunks());
            text.push('\n');
        }
        return Copied {
            text,
            linewise: true,
        };
    }
    let parts: Vec<String> = sel
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| doc.rope().slice(s.start()..s.end()).to_string())
        .collect();
    Copied {
        text: parts.join("\n"),
        linewise: false,
    }
}

/// Copies, then removes what was copied.
pub fn cut(doc: &Document, sel: &SelectionSet) -> (Copied, Transaction) {
    let copied = copy(doc, sel);
    let len = doc.len_chars();
    let items = sel
        .iter()
        .map(|s| {
            if !copied.linewise {
                return Item::new(s.start(), s.end(), String::new());
            }
            let line = doc.char_to_line(s.head);
            let mut start = doc.line_to_char(line);
            let end = if line + 1 < doc.line_count() {
                doc.line_to_char(line + 1)
            } else {
                // Removing the last line also removes the break before it.
                start = start.saturating_sub(1);
                len
            };
            Item::new(start, end, String::new())
        })
        .collect();
    (copied, finish_with_carets(doc, sel, items))
}

/// Pastes text that was copied linewise. Carets insert above their line;
/// selections are replaced as usual.
pub fn paste_linewise(doc: &Document, sel: &SelectionSet, text: &str) -> Transaction {
    let mut text = normalize_line_breaks(text).to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    let mut seen = BTreeSet::new();
    let items = sel
        .iter()
        .map(|s| {
            if !s.is_empty() {
                return Item::new(s.start(), s.end(), text.clone());
            }
            let line = doc.char_to_line(s.head);
            let start = doc.line_to_char(line);
            let col = s.head - start;
            // Several carets on one line insert the text only once.
            let mut item = if seen.insert(line) {
                Item::new(start, start, text.clone())
            } else {
                Item::new(start, start, String::new())
            };
            item.caret = item.text_chars + col;
            if item.text_chars == 0 {
                item.caret = col;
            }
            item
        })
        .collect();
    finish_with_carets(doc, sel, items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::Selection;

    fn run(
        text: &str,
        sel: SelectionSet,
        f: impl FnOnce(&Document, &SelectionSet) -> Transaction,
    ) -> (String, SelectionSet) {
        let mut doc = Document::from_text(text);
        let mut sel = sel;
        let tx = f(&doc, &sel);
        tx.apply(&mut doc, &mut sel);
        (doc.text(), sel)
    }

    fn caret(n: usize) -> SelectionSet {
        SelectionSet::single(Selection::caret(n))
    }

    #[test]
    fn copy_selection_and_multiple_selections() {
        let doc = Document::from_text("hello world");
        let one = SelectionSet::single(Selection::new(6, 11));
        assert_eq!(
            copy(&doc, &one),
            Copied {
                text: "world".into(),
                linewise: false
            }
        );

        let two = SelectionSet::new(vec![Selection::new(0, 5), Selection::new(6, 11)], 0, &doc);
        assert_eq!(copy(&doc, &two).text, "hello\nworld");
    }

    #[test]
    fn copy_without_selection_takes_the_whole_line() {
        let doc = Document::from_text("a\nbb\nc");
        assert_eq!(
            copy(&doc, &caret(3)),
            Copied {
                text: "bb\n".into(),
                linewise: true
            }
        );
        assert_eq!(
            copy(&doc, &caret(5)).text,
            "c\n",
            "last line gets a break too"
        );
        // Two carets on one line copy it once; different lines copy both.
        let same = SelectionSet::new(vec![Selection::caret(2), Selection::caret(3)], 0, &doc);
        assert_eq!(copy(&doc, &same).text, "bb\n");
        let diff = SelectionSet::new(vec![Selection::caret(0), Selection::caret(5)], 0, &doc);
        assert_eq!(copy(&doc, &diff).text, "a\nc\n");
    }

    #[test]
    fn cut_selection_removes_it() {
        let mut doc = Document::from_text("hello world");
        let mut sel = SelectionSet::single(Selection::new(5, 11));
        let (copied, tx) = cut(&doc, &sel);
        tx.apply(&mut doc, &mut sel);
        assert_eq!(copied.text, " world");
        assert_eq!(doc.text(), "hello");
        assert_eq!(sel.primary(), Selection::caret(5));
    }

    #[test]
    fn cut_line_removes_the_line() {
        for (text, at, want, caret_at) in [
            ("a\nb\nc", 2, "a\nc", 2),
            ("a\nb\nc", 0, "b\nc", 0),
            ("a\nb", 3, "a", 1),
            ("only", 2, "", 0),
        ] {
            let mut doc = Document::from_text(text);
            let mut sel = caret(at);
            let (copied, tx) = cut(&doc, &sel);
            tx.apply(&mut doc, &mut sel);
            assert!(copied.linewise);
            assert_eq!(doc.text(), want, "cut {text:?} at {at}");
            assert_eq!(
                sel.primary(),
                Selection::caret(caret_at),
                "caret for {text:?}"
            );
        }
    }

    #[test]
    fn cut_several_lines_at_once() {
        let mut doc = Document::from_text("a\nb\nc\nd");
        let mut sel = SelectionSet::new(
            vec![
                Selection::caret(0),
                Selection::caret(2),
                Selection::caret(2),
            ],
            0,
            &doc,
        );
        let (copied, tx) = cut(&doc, &sel);
        tx.apply(&mut doc, &mut sel);
        assert_eq!(copied.text, "a\nb\n");
        assert_eq!(doc.text(), "c\nd");
    }

    #[test]
    fn paste_linewise_goes_above_and_keeps_the_column() {
        let (text, sel) = run("a\nbc", caret(3), |d, s| paste_linewise(d, s, "X\n"));
        assert_eq!(text, "a\nX\nbc");
        assert_eq!(sel.primary(), Selection::caret(5), "still after the 'b'");
    }

    #[test]
    fn paste_linewise_adds_a_missing_break_and_replaces_selections() {
        let (text, _) = run("a\nb", caret(0), |d, s| paste_linewise(d, s, "X"));
        assert_eq!(text, "X\na\nb");
        let (text, sel) = run("abc", SelectionSet::single(Selection::new(1, 2)), |d, s| {
            paste_linewise(d, s, "X\n")
        });
        assert_eq!(text, "aX\nc");
        assert_eq!(sel.primary(), Selection::caret(3));
        let (text, _) = run("a\r\nb", caret(0), |d, s| {
            paste_linewise(d, s, "X\r\nY\r\n")
        });
        assert_eq!(text, "X\nY\na\nb", "CRLF is normalized");
    }

    #[test]
    fn paste_linewise_inserts_once_for_carets_on_the_same_line() {
        let doc = Document::from_text("abcd");
        let sel = SelectionSet::new(vec![Selection::caret(1), Selection::caret(3)], 0, &doc);
        let (text, sel) = run("abcd", sel, |d, s| paste_linewise(d, s, "X\n"));
        assert_eq!(text, "X\nabcd");
        let heads: Vec<_> = sel.iter().map(|s| s.head).collect();
        assert_eq!(heads, vec![3, 5]);
    }

    #[test]
    fn copy_then_paste_linewise_duplicates_a_line() {
        let mut doc = Document::from_text("one\ntwo");
        let mut sel = caret(5);
        let copied = copy(&doc, &sel);
        let tx = if copied.linewise {
            paste_linewise(&doc, &sel, &copied.text)
        } else {
            crate::edit::paste(&doc, &sel, &copied.text)
        };
        tx.apply(&mut doc, &mut sel);
        assert_eq!(doc.text(), "one\ntwo\ntwo");
    }
}
