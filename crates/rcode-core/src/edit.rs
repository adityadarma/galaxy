//! Multi-cursor edit operations (M1-03).
//!
//! Every operation is a pure function of `(document, selections, config)`
//! that returns a `Transaction`: one `ChangeSet` plus the selections to show
//! afterwards. Nothing is mutated until `Transaction::apply`, which returns
//! the inverse change set for undo (M1-04).
//!
//! Behavior follows VSCode defaults:
//! - Typing replaces each selection; carets end after the inserted text.
//! - Paste with N cursors and N clipboard lines puts one line per cursor
//!   (`editor.multiCursorPaste: "spread"`).
//! - Backspace deletes one grapheme, or back to the previous tab stop when
//!   the caret is inside leading spaces (`editor.useTabStops`).
//! - Enter keeps the current indentation, adds one level after an opening
//!   bracket, and splits `{|}` onto three lines.
//! - Tab inserts to the next tab stop at a caret, and indents whole lines
//!   when a selection spans lines. Shift+Tab outdents lines.

use std::collections::BTreeSet;

use crate::change::{Assoc, Change, ChangeSet};
use crate::document::Document;
use crate::selection::{Selection, SelectionSet};

/// Indentation settings (VSCode: `editor.insertSpaces`, `editor.tabSize`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndentConfig {
    pub use_tabs: bool,
    pub tab_size: usize,
}

impl Default for IndentConfig {
    fn default() -> Self {
        Self {
            use_tabs: false,
            tab_size: 4,
        }
    }
}

impl IndentConfig {
    fn tab_size(&self) -> usize {
        self.tab_size.max(1)
    }

    /// One indent level.
    pub fn unit(&self) -> String {
        if self.use_tabs {
            "\t".into()
        } else {
            " ".repeat(self.tab_size())
        }
    }

    /// Text that advances from visual column `vcol` to the next tab stop.
    fn next_stop_fill(&self, vcol: usize) -> String {
        if self.use_tabs {
            "\t".into()
        } else {
            let t = self.tab_size();
            " ".repeat(t - vcol % t)
        }
    }
}

/// A batch of changes plus the resulting selections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transaction {
    pub changes: ChangeSet,
    selections: Vec<Selection>,
    primary: usize,
}

impl Transaction {
    pub fn is_noop(&self) -> bool {
        self.changes.is_empty()
    }

    /// Applies the changes and installs the new selections. Returns the
    /// inverse change set.
    pub fn apply(self, doc: &mut Document, sel: &mut SelectionSet) -> ChangeSet {
        let inverse = doc.apply(&self.changes);
        *sel = SelectionSet::new(self.selections, self.primary, doc);
        inverse
    }
}

/// Types `text` at every selection (replacing selected text).
pub fn insert_text(doc: &Document, sel: &SelectionSet, text: &str) -> Transaction {
    let items = sel
        .iter()
        .map(|s| Item::new(s.start(), s.end(), text.to_string()))
        .collect();
    finish_with_carets(doc, sel, items)
}

/// Pastes `text`. With several cursors and exactly as many clipboard lines,
/// each cursor gets one line; otherwise every cursor gets the full text.
pub fn paste(doc: &Document, sel: &SelectionSet, text: &str) -> Transaction {
    if sel.len() > 1 {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let body = normalized.strip_suffix('\n').unwrap_or(&normalized);
        let lines: Vec<&str> = body.split('\n').collect();
        if lines.len() == sel.len() {
            let items = sel
                .iter()
                .zip(lines)
                .map(|(s, line)| Item::new(s.start(), s.end(), line.to_string()))
                .collect();
            return finish_with_carets(doc, sel, items);
        }
    }
    insert_text(doc, sel, text)
}

/// Backspace: deletes the selection, or the grapheme / tab stop before
/// each caret.
pub fn backspace(doc: &Document, sel: &SelectionSet, cfg: &IndentConfig) -> Transaction {
    let items = sel
        .iter()
        .map(|s| {
            if !s.is_empty() {
                return Item::new(s.start(), s.end(), String::new());
            }
            let pos = s.head;
            let start = tab_stop_backspace(doc, pos, cfg)
                .unwrap_or_else(|| doc.prev_grapheme_boundary(pos));
            Item::new(start, pos, String::new())
        })
        .collect();
    finish_with_carets(doc, sel, items)
}

/// Delete (forward): deletes the selection or the grapheme after each caret.
pub fn delete_forward(doc: &Document, sel: &SelectionSet) -> Transaction {
    let items = sel
        .iter()
        .map(|s| {
            if s.is_empty() {
                Item::new(s.head, doc.next_grapheme_boundary(s.head), String::new())
            } else {
                Item::new(s.start(), s.end(), String::new())
            }
        })
        .collect();
    finish_with_carets(doc, sel, items)
}

/// Enter with auto-indent.
pub fn newline(doc: &Document, sel: &SelectionSet, cfg: &IndentConfig) -> Transaction {
    let rope = doc.rope();
    let items = sel
        .iter()
        .map(|s| {
            let (start, end) = (s.start(), s.end());
            let line = doc.char_to_line(start);
            let line_start = doc.line_to_char(line);
            let line_end = doc.line_end_char(doc.char_to_line(end));

            // Indentation of the current line, but never past the caret.
            let indent: String = rope
                .slice(line_start..start)
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();

            let before = rope
                .slice(line_start..start)
                .chars()
                .filter(|c| !c.is_whitespace())
                .last();
            let after = rope
                .slice(end..line_end)
                .chars()
                .find(|c| !c.is_whitespace());

            let mut text = format!("\n{indent}");
            let mut caret = None;
            if let Some(open) = before.filter(|c| matches!(c, '{' | '[' | '(')) {
                text.push_str(&cfg.unit());
                if after == Some(closing(open)) {
                    caret = Some(text.chars().count());
                    text.push('\n');
                    text.push_str(&indent);
                }
            }
            let mut item = Item::new(start, end, text);
            if let Some(c) = caret {
                item.caret = c;
            }
            item
        })
        .collect();
    finish_with_carets(doc, sel, items)
}

/// Tab: indent lines for selections that span lines (or a whole line),
/// otherwise insert up to the next tab stop.
pub fn indent(doc: &Document, sel: &SelectionSet, cfg: &IndentConfig) -> Transaction {
    let rope = doc.rope();
    let t = cfg.tab_size();
    let mut lines = BTreeSet::new();
    let mut changes = Vec::new();
    for s in sel.iter() {
        if let Some(range) = line_range(doc, s) {
            lines.extend(range);
        } else {
            let line_start = doc.line_to_char(doc.char_to_line(s.start()));
            let vcol = visual_col(rope.slice(line_start..s.start()).chars(), t);
            changes.push(Change::new(s.start(), s.end(), cfg.next_stop_fill(vcol)));
        }
    }
    let multi_line = lines.len() > 1;
    for line in lines {
        // Like VSCode, empty lines stay empty in multi-line indents.
        if multi_line && doc.line_len_chars(line) == 0 {
            continue;
        }
        let start = doc.line_to_char(line);
        let text = if cfg.use_tabs {
            "\t".to_string()
        } else {
            // Align to the next tab stop rather than adding a fixed width.
            let width = visual_col(leading_ws(doc, line).chars(), t);
            " ".repeat(t - width % t)
        };
        changes.push(Change::insert(start, text));
    }
    finish_with_map(doc, sel, changes, |s| {
        !s.is_empty() && line_range(doc, s).is_none()
    })
}

/// Shift+Tab: removes one indent level (to the previous tab stop) from
/// every line touched by a selection.
pub fn outdent(doc: &Document, sel: &SelectionSet, cfg: &IndentConfig) -> Transaction {
    let t = cfg.tab_size();
    let mut lines = BTreeSet::new();
    for s in sel.iter() {
        let first = doc.char_to_line(s.start());
        let last = line_range(doc, s).map_or(first, |r| *r.end());
        lines.extend(first..=last);
    }
    let changes = lines
        .into_iter()
        .filter_map(|line| {
            let ws = leading_ws(doc, line);
            let remove = if ws.starts_with('\t') {
                1
            } else {
                let spaces = ws.chars().take_while(|c| *c == ' ').count();
                match spaces % t {
                    0 => spaces.min(t),
                    r => r,
                }
            };
            (remove > 0).then(|| {
                let start = doc.line_to_char(line);
                Change::delete(start, start + remove)
            })
        })
        .collect();
    finish_with_map(doc, sel, changes, |_| false)
}

// ---------------------------------------------------------------------------

/// One change produced for one selection, plus where its caret lands.
pub(crate) struct Item {
    start: usize,
    end: usize,
    text: String,
    pub(crate) text_chars: usize,
    /// Caret offset in chars from the start of the inserted text.
    pub(crate) caret: usize,
}

impl Item {
    pub(crate) fn new(start: usize, end: usize, text: String) -> Self {
        // Count after CR normalization so carets match the applied text.
        let text = if text.contains('\r') {
            crate::document::normalize_line_breaks(&text)
        } else {
            text
        };
        let text_chars = text.chars().count();
        Self {
            start,
            end,
            text,
            text_chars,
            caret: text_chars,
        }
    }
}

/// Builds a transaction where each selection becomes a caret at a known
/// offset inside its own change. `items[i]` belongs to selection `i`.
pub(crate) fn finish_with_carets(
    doc: &Document,
    sel: &SelectionSet,
    items: Vec<Item>,
) -> Transaction {
    let len = doc.len_chars();
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&i| (items[i].start.min(len), items[i].end.min(len)));

    // Merge overlapping ranges (e.g. two carets backspacing into the same
    // whitespace). A merged item's caret follows the item it merged into;
    // the duplicate carets collapse when the selection set normalizes.
    let mut groups: Vec<(Item, Vec<usize>)> = Vec::with_capacity(items.len());
    let mut items: Vec<Option<Item>> = items.into_iter().map(Some).collect();
    for i in order {
        let mut item = items[i].take().expect("each index visited once");
        item.start = item.start.min(len);
        item.end = item.end.min(len);
        match groups.last_mut() {
            Some((last, owners)) if item.start < last.end => {
                last.end = last.end.max(item.end);
                last.text.push_str(&item.text);
                last.text_chars += item.text_chars;
                owners.push(i);
            }
            _ => groups.push((item, vec![i])),
        }
    }

    let mut carets = vec![0usize; sel.len()];
    let mut changes = Vec::with_capacity(groups.len());
    let mut delta = 0isize;
    for (item, owners) in groups {
        let new_start = item.start.checked_add_signed(delta).unwrap_or(0);
        for owner in owners {
            carets[owner] = new_start + item.caret;
        }
        delta += item.text_chars as isize - (item.end - item.start) as isize;
        let change = Change::new(item.start, item.end, item.text);
        if !(change.start() == change.end() && change.text().is_empty()) {
            changes.push(change);
        }
    }

    Transaction {
        changes: ChangeSet::from_sorted(changes),
        selections: carets.into_iter().map(Selection::caret).collect(),
        primary: sel.primary_index(),
    }
}

/// Builds a transaction from arbitrary changes and maps each selection
/// through them, keeping ranges selected (used by indent/outdent).
pub(crate) fn finish_with_map(
    doc: &Document,
    sel: &SelectionSet,
    changes: Vec<Change>,
    collapse: impl Fn(&Selection) -> bool,
) -> Transaction {
    let changes = ChangeSet::new(changes, doc.len_chars());
    let selections = sel
        .iter()
        .map(|s| {
            // Selections replaced by their change become a caret after it.
            if collapse(s) {
                return Selection::caret(changes.map(s.end(), Assoc::After));
            }
            if s.is_empty() {
                return Selection::caret(changes.map(s.head, Assoc::After));
            }
            // The start sticks before text inserted at it (so a selection
            // starting at column 0 grows to include the new indent).
            let start = changes.map(s.start(), Assoc::Before);
            let end = changes.map(s.end(), Assoc::After);
            if s.is_backward() {
                Selection::new(end, start)
            } else {
                Selection::new(start, end)
            }
        })
        .collect();
    Transaction {
        changes,
        selections,
        primary: sel.primary_index(),
    }
}

/// Lines a selection indents as whole lines, or `None` when Tab should
/// insert text instead. A selection ending at column 0 does not include
/// that last line.
fn line_range(doc: &Document, s: &Selection) -> Option<std::ops::RangeInclusive<usize>> {
    if s.is_empty() {
        return None;
    }
    let first = doc.char_to_line(s.start());
    let mut last = doc.char_to_line(s.end());
    if last > first && s.end() == doc.line_to_char(last) {
        last -= 1;
    }
    let whole_line = s.start() == doc.line_to_char(first) && s.end() >= doc.line_end_char(first);
    (last > first || whole_line).then_some(first..=last)
}

fn leading_ws(doc: &Document, line: usize) -> String {
    let start = doc.line_to_char(line);
    let end = doc.line_end_char(line);
    doc.rope()
        .slice(start..end)
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Visual width with tabs expanded to the next tab stop.
fn visual_col(chars: impl Iterator<Item = char>, tab_size: usize) -> usize {
    chars.fold(0, |col, c| {
        if c == '\t' {
            col + tab_size - col % tab_size
        } else {
            col + 1
        }
    })
}

/// If the caret sits after only whitespace on its line and the char before
/// it is a space, returns the start of the range back to the previous tab
/// stop. Tabs are handled as single graphemes by the caller.
fn tab_stop_backspace(doc: &Document, pos: usize, cfg: &IndentConfig) -> Option<usize> {
    let rope = doc.rope();
    let line_start = doc.line_to_char(doc.char_to_line(pos));
    if pos == line_start || rope.char(pos - 1) != ' ' {
        return None;
    }
    let prefix = rope.slice(line_start..pos);
    if !prefix.chars().all(|c| c == ' ' || c == '\t') {
        return None;
    }
    let t = cfg.tab_size();
    let vcol = visual_col(prefix.chars(), t);
    let target = (vcol - 1) / t * t;
    let mut start = pos;
    let mut col = vcol;
    while start > line_start && col > target && rope.char(start - 1) == ' ' {
        start -= 1;
        col -= 1;
    }
    Some(start)
}

fn closing(open: char) -> char {
    match open {
        '{' => '}',
        '[' => ']',
        _ => ')',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses `|` as carets and `[...]` as forward selections.
    fn setup(marked: &str) -> (Document, SelectionSet) {
        let mut text = String::new();
        let mut sels = Vec::new();
        let mut open = None;
        let mut n = 0;
        for c in marked.chars() {
            match c {
                '|' => sels.push(Selection::caret(n)),
                '[' => open = Some(n),
                ']' => sels.push(Selection::new(open.take().unwrap(), n)),
                _ => {
                    text.push(c);
                    n += 1;
                }
            }
        }
        let doc = Document::from_text(&text);
        let sel = SelectionSet::new(sels, 0, &doc);
        (doc, sel)
    }

    /// Renders the document with selections marked the same way.
    fn render(doc: &Document, sel: &SelectionSet) -> String {
        let text: Vec<char> = doc.text().chars().collect();
        let mut out = String::new();
        for (i, c) in text
            .iter()
            .enumerate()
            .chain(std::iter::once((text.len(), &'\0')))
        {
            for s in sel.iter() {
                if s.is_empty() && s.head == i {
                    out.push('|');
                } else if !s.is_empty() && s.end() == i {
                    out.push(']');
                } else if !s.is_empty() && s.start() == i {
                    out.push('[');
                }
            }
            if *c != '\0' {
                out.push(*c);
            }
        }
        out
    }

    fn run(marked: &str, op: impl FnOnce(&Document, &SelectionSet) -> Transaction) -> String {
        let (mut doc, mut sel) = setup(marked);
        let before = doc.text();
        let tx = op(&doc, &sel);
        let inverse = tx.apply(&mut doc, &mut sel);
        let after = render(&doc, &sel);
        // Every operation must be exactly undoable.
        doc.apply(&inverse);
        assert_eq!(doc.text(), before, "inverse failed for {marked:?}");
        after
    }

    const CFG: IndentConfig = IndentConfig {
        use_tabs: false,
        tab_size: 4,
    };

    #[test]
    fn typing_at_multiple_carets_and_selections() {
        assert_eq!(run("a|b|c", |d, s| insert_text(d, s, "X")), "aX|bX|c");
        assert_eq!(run("[ab]c[de]", |d, s| insert_text(d, s, "_")), "_|c_|");
        assert_eq!(run("|", |d, s| insert_text(d, s, "日本")), "日本|");
    }

    #[test]
    fn paste_spreads_lines_across_cursors() {
        assert_eq!(
            run("|a\n|b\n|c", |d, s| paste(d, s, "1\n2\n3\n")),
            "1|a\n2|b\n3|c"
        );
        // Line count mismatch: everyone gets everything.
        assert_eq!(
            run("|a|b", |d, s| paste(d, s, "x\ny\nz")),
            "x\ny\nz|ax\ny\nz|b"
        );
        // Single cursor never spreads.
        assert_eq!(run("|", |d, s| paste(d, s, "x\r\ny")), "x\ny|");
    }

    #[test]
    fn backspace_graphemes_and_selections() {
        assert_eq!(run("ab|c", |d, s| backspace(d, s, &CFG)), "a|c");
        assert_eq!(
            run("a👨\u{200d}👩\u{200d}👧|", |d, s| backspace(
                d, s, &CFG
            )),
            "a|"
        );
        assert_eq!(run("a[bc]d", |d, s| backspace(d, s, &CFG)), "a|d");
        assert_eq!(run("|ab", |d, s| backspace(d, s, &CFG)), "|ab");
        assert_eq!(run("a\n|b", |d, s| backspace(d, s, &CFG)), "a|b");
    }

    #[test]
    fn backspace_uses_tab_stops_in_leading_whitespace() {
        assert_eq!(run("        |x", |d, s| backspace(d, s, &CFG)), "    |x");
        assert_eq!(run("      |x", |d, s| backspace(d, s, &CFG)), "    |x");
        assert_eq!(run("\t  |x", |d, s| backspace(d, s, &CFG)), "\t|x");
        // Not leading whitespace: one char.
        assert_eq!(run("a    |x", |d, s| backspace(d, s, &CFG)), "a   |x");
        // Two carets in the same run merge cleanly.
        assert_eq!(run("  |  |x", |d, s| backspace(d, s, &CFG)), "|x");
    }

    #[test]
    fn delete_forward_graphemes() {
        assert_eq!(run("a|e\u{301}b", delete_forward), "a|b");
        assert_eq!(run("ab|", delete_forward), "ab|");
        assert_eq!(run("a|\nb", delete_forward), "a|b");
    }

    #[test]
    fn newline_keeps_indent() {
        assert_eq!(
            run("    foo|", |d, s| newline(d, s, &CFG)),
            "    foo\n    |"
        );
        assert_eq!(
            run("\tfoo|bar", |d, s| newline(d, s, &CFG)),
            "\tfoo\n\t|bar"
        );
        // Caret inside leading whitespace keeps only what is before it.
        assert_eq!(run("  |  foo", |d, s| newline(d, s, &CFG)), "  \n  |  foo");
    }

    #[test]
    fn newline_after_bracket_indents_and_splits_pair() {
        assert_eq!(
            run("fn x() {|", |d, s| newline(d, s, &CFG)),
            "fn x() {\n    |"
        );
        assert_eq!(
            run("  if x {|}", |d, s| newline(d, s, &CFG)),
            "  if x {\n      |\n  }"
        );
        assert_eq!(
            run("call(|)", |d, s| newline(d, s, &CFG)),
            "call(\n    |\n)"
        );
        // Mismatched pair does not split.
        assert_eq!(run("{|)", |d, s| newline(d, s, &CFG)), "{\n    |)");
    }

    #[test]
    fn newline_multi_cursor() {
        assert_eq!(
            run("a {|}\n  b|", |d, s| newline(d, s, &CFG)),
            "a {\n    |\n}\n  b\n  |"
        );
    }

    #[test]
    fn tab_at_caret_goes_to_next_tab_stop() {
        assert_eq!(run("|x", |d, s| indent(d, s, &CFG)), "    |x");
        assert_eq!(run("ab|x", |d, s| indent(d, s, &CFG)), "ab  |x");
        assert_eq!(run("a[b]c", |d, s| indent(d, s, &CFG)), "a   |c");
        let tabs = IndentConfig {
            use_tabs: true,
            tab_size: 4,
        };
        assert_eq!(run("ab|x", |d, s| indent(d, s, &tabs)), "ab\t|x");
    }

    #[test]
    fn tab_with_multiline_selection_indents_lines() {
        assert_eq!(
            run("[a\n\n  b\nc]\nd", |d, s| indent(d, s, &CFG)),
            "[    a\n\n    b\n    c]\nd"
        );
        // Ending at column 0 excludes that line.
        assert_eq!(run("[a\n]b", |d, s| indent(d, s, &CFG)), "[    a\n]b");
        // Whole single line counts as a line selection.
        assert_eq!(run("[ab]\nc", |d, s| indent(d, s, &CFG)), "[    ab]\nc");
    }

    #[test]
    fn outdent_lines() {
        assert_eq!(run("        |x", |d, s| outdent(d, s, &CFG)), "    |x");
        assert_eq!(run("      x|", |d, s| outdent(d, s, &CFG)), "    x|");
        assert_eq!(run("\t\tx|", |d, s| outdent(d, s, &CFG)), "\tx|");
        assert_eq!(run("  |  x", |d, s| outdent(d, s, &CFG)), "|x");
        assert_eq!(run("x|", |d, s| outdent(d, s, &CFG)), "x|");
        assert_eq!(
            run("[    a\n  b\nc]", |d, s| outdent(d, s, &CFG)),
            "[a\nb\nc]"
        );
    }

    #[test]
    fn primary_selection_is_preserved() {
        let doc = Document::from_text("ab\ncd");
        let sel = SelectionSet::new(vec![Selection::caret(1), Selection::caret(4)], 1, &doc);
        let (mut doc, mut sel) = (doc, sel);
        insert_text(&doc, &sel, "X").apply(&mut doc, &mut sel);
        assert_eq!(sel.primary(), Selection::caret(6));
    }
}
