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
use crate::tags::{self, Flavor};

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

    pub(crate) fn apply_selections(self, doc: &Document, sel: &mut SelectionSet) {
        *sel = SelectionSet::new(self.selections, self.primary, doc);
    }
}

pub fn from_changes(changes: ChangeSet, doc: &Document) -> Transaction {
    let end = changes.len_after(doc.len_chars());
    Transaction {
        changes,
        selections: vec![Selection::caret(end)],
        primary: 0,
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

/// Types an opening delimiter and its closing partner. Selections are wrapped;
/// empty carets land between the pair. Quotes are only paired at a boundary so
/// apostrophes in words keep ordinary typing behavior.
pub fn insert_pair(doc: &Document, sel: &SelectionSet, open: char) -> Transaction {
    let close = pair_for(open).expect("only paired delimiters call insert_pair");
    let items = sel
        .iter()
        .map(|s| {
            if s.is_empty() {
                let before = (s.head > 0).then(|| doc.rope().char(s.head - 1));
                let after = (s.head < doc.len_chars()).then(|| doc.rope().char(s.head));
                if matches!(open, '\'' | '"')
                    && before.is_some_and(is_word)
                    && after.is_some_and(is_word)
                {
                    return Item::new(s.head, s.head, open.to_string());
                }
                let mut item = Item::new(s.head, s.head, format!("{open}{close}"));
                item.caret = 1;
                item
            } else {
                let body = doc.rope().slice(s.start()..s.end()).to_string();
                let mut item = Item::new(s.start(), s.end(), format!("{open}{body}{close}"));
                item.caret = body.chars().count() + 2;
                item
            }
        })
        .collect();
    finish_with_carets(doc, sel, items)
}

/// Skips a closing delimiter already at every empty caret.
pub fn skip_closing(doc: &Document, sel: &SelectionSet, close: char) -> Option<Transaction> {
    if !sel
        .iter()
        .all(|s| s.is_empty() && s.head < doc.len_chars() && doc.rope().char(s.head) == close)
    {
        return None;
    }
    let items = sel
        .iter()
        .map(|s| {
            let mut item = Item::new(s.head, s.head, String::new());
            item.caret = 1;
            item
        })
        .collect();
    Some(finish_with_carets(doc, sel, items))
}

/// Typing `>` at the end of an unfinished tag also adds its closing tag,
/// with the caret between: `<div|` + `>` gives `<div>|</div>`. `None` when no
/// caret is at such a tag (the `>` is then typed normally).
pub fn close_tag_gt(doc: &Document, sel: &SelectionSet, flavor: Flavor) -> Option<Transaction> {
    close_tag_with(doc, sel, ">", |caret| {
        tags::closing_after_gt(doc, caret, flavor).map(|closing| (format!(">{closing}"), 1))
    })
}

/// Typing `/` right after `<` completes the nearest unclosed tag:
/// `<div>text<|` + `/` gives `<div>text</div>|`.
pub fn close_tag_slash(doc: &Document, sel: &SelectionSet, flavor: Flavor) -> Option<Transaction> {
    close_tag_with(doc, sel, "/", |caret| {
        tags::closing_after_slash(doc, caret, flavor).map(|rest| {
            let chars = rest.chars().count();
            (rest, chars)
        })
    })
}

/// Shared by the two above: every caret gets its own completion, or plain
/// `typed` when it has none. `None` when no caret has one, or any selection
/// is non-empty.
fn close_tag_with(
    doc: &Document,
    sel: &SelectionSet,
    typed: &str,
    completion: impl Fn(usize) -> Option<(String, usize)>,
) -> Option<Transaction> {
    if sel.iter().any(|s| !s.is_empty()) || sel.len() > MAX_TAG_CARETS {
        return None;
    }
    let mut any = false;
    let items = sel
        .iter()
        .map(|s| match completion(s.head) {
            Some((text, caret)) => {
                any = true;
                let mut item = Item::new(s.head, s.head, text);
                item.caret = caret;
                item
            }
            None => Item::new(s.head, s.head, typed.to_string()),
        })
        .collect();
    any.then(|| finish_with_carets(doc, sel, items))
}

const MAX_TAG_CARETS: usize = 8;

/// Adds to `tx` the rename of each tag name's partner: when an edit changes
/// `<div>`'s name, `</div>` follows in the same transaction, so one undo
/// reverts both. Carets and the rest of `tx` are unchanged. Edits that
/// split a name, empty it, or touch the partner itself are left alone.
pub fn with_linked_rename(
    doc: &Document,
    sel: &SelectionSet,
    tx: Transaction,
    flavor: Flavor,
) -> Transaction {
    if tx.is_noop() || sel.len() > MAX_TAG_CARETS {
        return tx;
    }
    let mut extra: Vec<Change> = Vec::new();
    let mut after: Option<Document> = None;
    for s in sel.iter() {
        let Some((own, partner)) = tags::linked_names(doc, s.head, flavor) else {
            continue;
        };
        if s.start() < own.0 || s.end() > own.1 {
            continue;
        }
        let touches = |c: &Change| c.start() <= partner.1 && c.end() >= partner.0;
        if tx.changes.iter().any(touches) || extra.iter().any(touches) {
            continue;
        }
        let start = tx.changes.map(own.0, Assoc::Before);
        let end = tx.changes.map(own.1, Assoc::After);
        let edited = after.get_or_insert_with(|| {
            let (mut d, mut selections) = (doc.clone(), sel.clone());
            tx.clone().apply(&mut d, &mut selections);
            d
        });
        let rope = edited.rope();
        let mut stop = start;
        while stop < edited.len_chars() && tags::is_name_char(rope.char(stop)) {
            stop += 1;
        }
        if stop != end || stop == start || !rope.char(start).is_alphabetic() {
            continue;
        }
        let renamed: String = rope.slice(start..stop).chars().collect();
        let old = |r: (usize, usize)| doc.rope().slice(r.0..r.1).chars().collect::<String>();
        if renamed == old(own) || renamed == old(partner) {
            continue;
        }
        extra.push(Change::new(partner.0, partner.1, renamed));
    }
    if extra.is_empty() {
        return tx;
    }

    let mut all: Vec<Change> = tx.changes.iter().cloned().collect();
    all.extend(extra.iter().cloned());
    let combined = ChangeSet::new(all, doc.len_chars());
    // The partner edits expressed after `tx`, to shift the carets past them.
    let shifted = extra
        .iter()
        .map(|c| {
            Change::new(
                tx.changes.map(c.start(), Assoc::Before),
                tx.changes.map(c.end(), Assoc::After),
                c.text(),
            )
        })
        .collect();
    let follow = ChangeSet::new(shifted, tx.changes.len_after(doc.len_chars()));
    let selections = tx
        .selections
        .iter()
        .map(|s| {
            Selection::new(
                follow.map(s.anchor, Assoc::After),
                follow.map(s.head, Assoc::After),
            )
        })
        .collect();
    Transaction {
        changes: combined,
        selections,
        primary: tx.primary,
    }
}

pub fn pair_for(open: char) -> Option<char> {
    match open {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '\'' => Some('\''),
        '"' => Some('"'),
        _ => None,
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
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

/// Toggles a language's line comment marker after each affected line's
/// indentation. A selection across lines is one transaction and one undo step.
pub fn toggle_comment(doc: &Document, sel: &SelectionSet, marker: &str) -> Transaction {
    let mut lines = BTreeSet::new();
    for selection in sel.iter() {
        let first = doc.char_to_line(selection.start());
        let mut last = doc.char_to_line(selection.end());
        if last > first && selection.end() == doc.line_to_char(last) {
            last -= 1;
        }
        lines.extend(first..=last);
    }
    let all_commented = lines.iter().all(|&line| {
        let text = doc
            .rope()
            .slice(doc.line_to_char(line)..doc.line_end_char(line))
            .to_string();
        text.trim_start_matches([' ', '\t']).starts_with(marker)
    });
    let changes = lines
        .into_iter()
        .map(|line| {
            let start = doc.line_to_char(line);
            let text = doc.rope().slice(start..doc.line_end_char(line)).to_string();
            let indent = text.chars().take_while(|c| matches!(c, ' ' | '\t')).count();
            let at = start + indent;
            if all_commented {
                let rest = &text[indent..];
                let remove =
                    marker.chars().count() + usize::from(rest.starts_with(&format!("{marker} ")));
                Change::delete(at, at + remove)
            } else {
                Change::insert(at, format!("{marker} "))
            }
        })
        .collect();
    finish_with_map(doc, sel, changes, |_| false)
}

// --- Line operations (Selection and Edit menus) ---------------------------

/// Inclusive line ranges touched by the selections, merged when they
/// overlap or touch. A selection that ends at the start of a line does not
/// include that line.
fn line_blocks(doc: &Document, sel: &SelectionSet) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = sel
        .iter()
        .map(|s| {
            let first = doc.char_to_line(s.start());
            let mut last = doc.char_to_line(s.end());
            if last > first && s.end() == doc.line_to_char(last) {
                last -= 1;
            }
            (first, last)
        })
        .collect();
    ranges.sort_unstable();
    let mut blocks: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (first, last) in ranges {
        match blocks.last_mut() {
            Some(prev) if first <= prev.1 + 1 => prev.1 = prev.1.max(last),
            _ => blocks.push((first, last)),
        }
    }
    blocks
}

fn text_between(doc: &Document, start: usize, end: usize) -> String {
    doc.rope().slice(start..end).to_string()
}

/// Moves every selection by the offset of the block its first line is in.
fn shift_selections(
    doc: &Document,
    sel: &SelectionSet,
    blocks: &[(usize, usize)],
    delta: impl Fn(usize) -> isize,
) -> Vec<Selection> {
    sel.iter()
        .map(|s| {
            let line = doc.char_to_line(s.start());
            let index = blocks.iter().position(|&(a, b)| (a..=b).contains(&line));
            let d = index.map_or(0, &delta);
            let shift = |p: usize| p.checked_add_signed(d).unwrap_or(0);
            Selection::new(shift(s.anchor), shift(s.head))
        })
        .collect()
}

/// Alt+Up / Alt+Down: swaps each block of selected lines with the line
/// above or below it. The selections travel with their lines.
pub fn move_lines(doc: &Document, sel: &SelectionSet, up: bool) -> Transaction {
    let blocks = line_blocks(doc, sel);
    let last_line = doc.line_count() - 1;
    let mut changes = Vec::new();
    let mut deltas = vec![0isize; blocks.len()];
    for (i, &(a, b)) in blocks.iter().enumerate() {
        if up {
            if a == 0 {
                continue;
            }
            let prev = a - 1;
            let start = doc.line_to_char(prev);
            let end = doc.line_end_char(b);
            let prev_text = text_between(doc, start, doc.line_end_char(prev));
            let block = text_between(doc, doc.line_to_char(a), end);
            changes.push(Change::new(start, end, format!("{block}\n{prev_text}")));
            deltas[i] = -(doc.line_len_chars(prev) as isize + 1);
        } else {
            if b >= last_line {
                continue;
            }
            let next = b + 1;
            let start = doc.line_to_char(a);
            let end = doc.line_end_char(next);
            let block = text_between(doc, start, doc.line_end_char(b));
            let next_text = text_between(doc, doc.line_to_char(next), end);
            changes.push(Change::new(start, end, format!("{next_text}\n{block}")));
            deltas[i] = doc.line_len_chars(next) as isize + 1;
        }
    }
    Transaction {
        changes: ChangeSet::new(changes, doc.len_chars()),
        selections: shift_selections(doc, sel, &blocks, |i| deltas[i]),
        primary: sel.primary_index(),
    }
}

/// Shift+Alt+Up / Shift+Alt+Down: duplicates each block of selected lines
/// above or below. The selections end up on the copy.
pub fn copy_lines(doc: &Document, sel: &SelectionSet, up: bool) -> Transaction {
    let blocks = line_blocks(doc, sel);
    let mut changes = Vec::new();
    let mut deltas = vec![0isize; blocks.len()];
    // Chars inserted by the blocks before this one.
    let mut inserted = 0isize;
    for (i, &(a, b)) in blocks.iter().enumerate() {
        let start = doc.line_to_char(a);
        let end = doc.line_end_char(b);
        let block = text_between(doc, start, end);
        let added = block.chars().count() as isize + 1;
        if up {
            changes.push(Change::insert(start, format!("{block}\n")));
            deltas[i] = inserted;
        } else {
            changes.push(Change::insert(end, format!("\n{block}")));
            deltas[i] = inserted + added;
        }
        inserted += added;
    }
    Transaction {
        changes: ChangeSet::new(changes, doc.len_chars()),
        selections: shift_selections(doc, sel, &blocks, |i| deltas[i]),
        primary: sel.primary_index(),
    }
}

/// Cmd+Shift+K: deletes every line a selection touches, line break included.
pub fn delete_lines(doc: &Document, sel: &SelectionSet) -> Transaction {
    let last_line = doc.line_count() - 1;
    let changes = line_blocks(doc, sel)
        .into_iter()
        .map(|(a, b)| {
            if b < last_line {
                Change::delete(doc.line_to_char(a), doc.line_to_char(b + 1))
            } else if a > 0 {
                // The last line has no break after it: take the one before.
                Change::delete(doc.line_end_char(a - 1), doc.line_end_char(b))
            } else {
                Change::delete(0, doc.line_end_char(b))
            }
        })
        .collect();
    finish_with_map(doc, sel, changes, |_| true)
}

/// Ctrl+J: joins each caret's line with the next one, or all the lines of
/// a multi-line selection. The next line's indentation becomes one space.
pub fn join_lines(doc: &Document, sel: &SelectionSet) -> Transaction {
    let last_line = doc.line_count() - 1;
    let mut joins = BTreeSet::new();
    for s in sel.iter() {
        let first = doc.char_to_line(s.start());
        let mut last = doc.char_to_line(s.end());
        if last > first && s.end() == doc.line_to_char(last) {
            last -= 1;
        }
        if last == first {
            joins.insert(first);
        } else {
            joins.extend(first..last);
        }
    }
    let changes = joins
        .into_iter()
        .filter(|&line| line < last_line)
        .map(|line| {
            let end = doc.line_end_char(line);
            let next_start = doc.line_to_char(line + 1);
            let next_text = text_between(doc, next_start, doc.line_end_char(line + 1));
            let trimmed = next_text.trim_start_matches([' ', '\t']);
            let skipped = next_text.chars().count() - trimmed.chars().count();
            let separator = if doc.line_len_chars(line) == 0 || trimmed.is_empty() {
                ""
            } else {
                " "
            };
            Change::new(end, next_start + skipped, separator)
        })
        .collect();
    finish_with_map(doc, sel, changes, |_| false)
}

/// Upper or lower cases each selection, or the word at an empty caret.
pub fn transform_case(doc: &Document, sel: &SelectionSet, upper: bool) -> Transaction {
    let mut ranges: Vec<(usize, usize)> = sel
        .iter()
        .map(|s| {
            if s.is_empty() {
                word_range(doc, s.head)
            } else {
                (s.start(), s.end())
            }
        })
        .filter(|(start, end)| start < end)
        .collect();
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(prev) if start < prev.1 => prev.1 = prev.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    let changes = merged
        .into_iter()
        .filter_map(|(start, end)| {
            let text = text_between(doc, start, end);
            let changed = if upper {
                text.to_uppercase()
            } else {
                text.to_lowercase()
            };
            (changed != text).then(|| Change::new(start, end, changed))
        })
        .collect();
    finish_keeping(doc, sel, changes)
}

/// Removes spaces and tabs at the end of every line in the document.
pub fn trim_trailing_whitespace(doc: &Document, sel: &SelectionSet) -> Transaction {
    let changes = (0..doc.line_count())
        .filter_map(|line| {
            let start = doc.line_to_char(line);
            let end = doc.line_end_char(line);
            let text = text_between(doc, start, end);
            let trailing = text.len() - text.trim_end_matches([' ', '\t']).len();
            (trailing > 0).then(|| Change::delete(end - trailing, end))
        })
        .collect();
    finish_with_map(doc, sel, changes, |_| false)
}

/// Sorts the lines of each multi-line selection.
pub fn sort_lines(doc: &Document, sel: &SelectionSet, descending: bool) -> Transaction {
    let changes = line_blocks(doc, sel)
        .into_iter()
        .filter(|&(a, b)| b > a)
        .filter_map(|(a, b)| {
            let start = doc.line_to_char(a);
            let end = doc.line_end_char(b);
            let mut lines: Vec<String> = (a..=b)
                .map(|line| text_between(doc, doc.line_to_char(line), doc.line_end_char(line)))
                .collect();
            lines.sort();
            if descending {
                lines.reverse();
            }
            let text = lines.join("\n");
            (text != text_between(doc, start, end)).then(|| Change::new(start, end, text))
        })
        .collect();
    finish_keeping(doc, sel, changes)
}

fn word_range(doc: &Document, pos: usize) -> (usize, usize) {
    let rope = doc.rope();
    let mut start = pos;
    while start > 0 && is_word(rope.char(start - 1)) {
        start -= 1;
    }
    let mut end = pos;
    while end < rope.len_chars() && is_word(rope.char(end)) {
        end += 1;
    }
    (start, end)
}

/// Keeps the selections as they are when no change alters the text length
/// (case changes, sorting); otherwise maps them through the changes.
fn finish_keeping(doc: &Document, sel: &SelectionSet, changes: Vec<Change>) -> Transaction {
    if changes.iter().all(|c| c.inserted_len() == c.removed_len()) {
        Transaction {
            changes: ChangeSet::new(changes, doc.len_chars()),
            selections: sel.iter().copied().collect(),
            primary: sel.primary_index(),
        }
    } else {
        finish_with_map(doc, sel, changes, |_| false)
    }
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

/// Turns the document into `new` with one change covering only the part
/// that differs (common prefix and suffix kept), so carets and the scroll
/// position outside it stay put. Used to reload a file changed on disk
/// (M2-03); the result is one undo step.
pub fn replace_by_diff(doc: &Document, sel: &SelectionSet, new: &str) -> Transaction {
    let old = doc.text();
    let (a, b) = (old.as_bytes(), new.as_bytes());
    let mut prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let max_suffix = a.len().min(b.len()) - prefix;
    let mut suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(max_suffix)
        .take_while(|(x, y)| x == y)
        .count();
    while !old.is_char_boundary(a.len() - suffix) || !new.is_char_boundary(b.len() - suffix) {
        suffix -= 1;
    }
    let start = old[..prefix].chars().count();
    let end = start + old[prefix..a.len() - suffix].chars().count();
    let change = Change::new(start, end, &new[prefix..b.len() - suffix]);
    finish_with_map(doc, sel, vec![change], |_| false)
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

    #[test]
    fn pairs_delimiters_wraps_selections_and_skips_closing() {
        let (mut doc, mut sel) = setup("|");
        insert_pair(&doc, &sel, '(').apply(&mut doc, &mut sel);
        assert_eq!(doc.text(), "()");
        assert_eq!(sel.primary().head, 1);
        skip_closing(&doc, &sel, ')')
            .unwrap()
            .apply(&mut doc, &mut sel);
        assert_eq!(doc.text(), "()");
        assert_eq!(sel.primary().head, 2);

        let (mut doc, mut sel) = setup("[word]");
        insert_pair(&doc, &sel, '"').apply(&mut doc, &mut sel);
        assert_eq!(doc.text(), "\"word\"");
        assert_eq!(sel.primary().head, 6);
    }

    #[test]
    fn apostrophe_inside_a_word_is_not_paired() {
        let (mut doc, mut sel) = setup("don|t");
        insert_pair(&doc, &sel, '\'').apply(&mut doc, &mut sel);
        assert_eq!(doc.text(), "don't");
        assert_eq!(sel.primary().head, 4);
    }

    // --- Tags ---

    const HTML: Flavor = Flavor::HTML;

    fn type_gt(marked: &str, flavor: Flavor) -> Option<String> {
        let (mut doc, mut sel) = setup(marked);
        let tx = close_tag_gt(&doc, &sel, flavor)?;
        tx.apply(&mut doc, &mut sel);
        Some(show(&doc, &sel))
    }

    fn type_slash(marked: &str, flavor: Flavor) -> Option<String> {
        let (mut doc, mut sel) = setup(marked);
        let tx = close_tag_slash(&doc, &sel, flavor)?;
        tx.apply(&mut doc, &mut sel);
        Some(show(&doc, &sel))
    }

    /// The document with `|` at every caret.
    fn show(doc: &Document, sel: &SelectionSet) -> String {
        let mut text: Vec<char> = doc.text().chars().collect();
        let mut carets: Vec<usize> = sel.iter().map(|s| s.head).collect();
        carets.sort_unstable_by(|a, b| b.cmp(a));
        for c in carets {
            text.insert(c, '|');
        }
        text.into_iter().collect()
    }

    #[test]
    fn typing_gt_adds_the_closing_tag_between_the_carets() {
        assert_eq!(type_gt("<div|", HTML).as_deref(), Some("<div>|</div>"));
        assert_eq!(type_gt("<ul><li|", HTML).as_deref(), Some("<ul><li>|</li>"));
        assert_eq!(type_gt("<br|", HTML), None);
        assert_eq!(type_gt("a |", HTML), None);
        assert_eq!(
            type_gt("<[di]v", HTML),
            None,
            "a selection is replaced, not completed"
        );
    }

    #[test]
    fn typing_gt_with_several_carets_completes_each_and_types_the_rest() {
        assert_eq!(
            type_gt("x |\n<a|\n<b|", HTML).as_deref(),
            Some("x >|\n<a>|</a>\n<b>|</b>")
        );
    }

    #[test]
    fn typing_slash_completes_the_nearest_open_tag() {
        assert_eq!(
            type_slash("<div>hi<|", HTML).as_deref(),
            Some("<div>hi</div>|")
        );
        assert_eq!(type_slash("<p>a</p><|", HTML), None);
    }

    /// Replaces the document with `typed` at the caret(s) the way typing
    /// does, then renames linked tags; returns the document with carets.
    fn edit_then_rename(
        marked: &str,
        flavor: Flavor,
        edit: impl Fn(&Document, &SelectionSet) -> Transaction,
    ) -> String {
        let (mut doc, mut sel) = setup(marked);
        let tx = with_linked_rename(&doc, &sel, edit(&doc, &sel), flavor);
        tx.apply(&mut doc, &mut sel);
        show(&doc, &sel)
    }

    #[test]
    fn renaming_an_opening_tag_renames_the_closing_one() {
        let typed = |t: &'static str| move |d: &Document, s: &SelectionSet| insert_text(d, s, t);
        assert_eq!(
            edit_then_rename("<div|>x</div>", HTML, typed("s")),
            "<divs|>x</divs>"
        );
        assert_eq!(
            edit_then_rename("<|div>x</div>", HTML, typed("a")),
            "<a|div>x</adiv>"
        );
        // Typing inside the closing name renames the opening one too.
        assert_eq!(
            edit_then_rename("<div>x</di|v>", HTML, typed("s")),
            "<disv>x</dis|v>"
        );
    }

    #[test]
    fn deleting_name_characters_renames_the_partner_and_keeps_the_caret() {
        let back = |d: &Document, s: &SelectionSet| backspace(d, s, &IndentConfig::default());
        assert_eq!(
            edit_then_rename("<span|>x</span>", HTML, back),
            "<spa|>x</spa>"
        );
        let fwd = |d: &Document, s: &SelectionSet| delete_forward(d, s);
        assert_eq!(
            edit_then_rename("<|span>x</span>", HTML, fwd),
            "<|pan>x</pan>"
        );
    }

    #[test]
    fn rename_is_one_undo_step_and_skips_unsafe_edits() {
        let (mut doc, mut sel) = setup("<div|>x</div>");
        let tx = with_linked_rename(&doc, &sel, insert_text(&doc, &sel, "s"), HTML);
        let inverse = tx.apply(&mut doc, &mut sel);
        assert_eq!(doc.text(), "<divs>x</divs>");
        doc.apply(&inverse);
        assert_eq!(doc.text(), "<div>x</div>", "both names restored together");

        let typed = |t: &'static str| move |d: &Document, s: &SelectionSet| insert_text(d, s, t);
        // A space ends the name: the partner must not become `di`.
        assert_eq!(
            edit_then_rename("<di| v>x</div>", HTML, typed(" ")),
            "<di | v>x</div>"
        );
        // Typing `>` or deleting the whole name leaves the partner alone.
        assert_eq!(
            edit_then_rename("<div|>x</div>", HTML, typed("a")),
            "<diva|>x</diva>"
        );
        let back = |d: &Document, s: &SelectionSet| backspace(d, s, &IndentConfig::default());
        assert_eq!(edit_then_rename("<a|>x</a>", HTML, back), "<|>x</a>");
        // Text, attributes and void tags are not tag names.
        assert_eq!(
            edit_then_rename("<p>te|xt</p>", HTML, typed("!")),
            "<p>te!|xt</p>"
        );
        assert_eq!(
            edit_then_rename("<div cl|ass>x</div>", HTML, typed("!")),
            "<div cl!|ass>x</div>"
        );
    }

    #[test]
    fn rename_follows_nesting_and_every_caret() {
        let typed = |d: &Document, s: &SelectionSet| insert_text(d, s, "x");
        assert_eq!(
            edit_then_rename("<div|><div>a</div></div>", HTML, typed),
            "<divx|><div>a</div></divx>"
        );
        assert_eq!(
            edit_then_rename("<a|>1</a><b|>2</b>", HTML, typed),
            "<ax|>1</ax><bx|>2</bx>"
        );
    }

    #[test]
    fn toggle_comment_preserves_indent_and_round_trips() {
        let (mut doc, mut sel) = setup("[  one\n    two\n  three]");
        let tx = toggle_comment(&doc, &sel, "//");
        tx.apply(&mut doc, &mut sel);
        assert_eq!(doc.text(), "  // one\n    // two\n  // three");
        toggle_comment(&doc, &sel, "//").apply(&mut doc, &mut sel);
        assert_eq!(doc.text(), "  one\n    two\n  three");
    }

    fn lines_op(marked: &str, f: impl Fn(&Document, &SelectionSet) -> Transaction) -> String {
        let (mut doc, mut sel) = setup(marked);
        f(&doc, &sel).apply(&mut doc, &mut sel);
        render(&doc, &sel)
    }

    #[test]
    fn move_lines_swaps_with_neighbor_and_keeps_selection_on_the_lines() {
        assert_eq!(
            lines_op("a\nb|\nc", |d, s| move_lines(d, s, true)),
            "b|\na\nc"
        );
        assert_eq!(
            lines_op("a|\nb\nc", |d, s| move_lines(d, s, false)),
            "b\na|\nc"
        );
        assert_eq!(
            lines_op("a\n[b\nc]\nd", |d, s| move_lines(d, s, true)),
            "[b\nc]\na\nd"
        );
        // Edges do nothing.
        assert_eq!(lines_op("a|\nb", |d, s| move_lines(d, s, true)), "a|\nb");
        assert_eq!(lines_op("a\nb|", |d, s| move_lines(d, s, false)), "a\nb|");
        // Moving into the unterminated last line.
        assert_eq!(lines_op("a|\nb", |d, s| move_lines(d, s, false)), "b\na|");
    }

    #[test]
    fn copy_lines_duplicates_and_selection_follows_the_copy() {
        assert_eq!(
            lines_op("a|b\nc", |d, s| copy_lines(d, s, false)),
            "ab\na|b\nc"
        );
        assert_eq!(
            lines_op("a|b\nc", |d, s| copy_lines(d, s, true)),
            "a|b\nab\nc"
        );
        assert_eq!(
            lines_op("a\nb|", |d, s| copy_lines(d, s, false)),
            "a\nb\nb|"
        );
    }

    #[test]
    fn delete_lines_removes_whole_lines_including_the_last() {
        assert_eq!(lines_op("a\nb|\nc", delete_lines), "a\n|c");
        assert_eq!(lines_op("a\nb\nc|", delete_lines), "a\nb|");
        assert_eq!(lines_op("a|", delete_lines), "|");
        assert_eq!(lines_op("[a\nb]\nc", delete_lines), "|c");
    }

    #[test]
    fn join_lines_replaces_break_and_indent_with_one_space() {
        assert_eq!(lines_op("a|\n    b\nc", join_lines), "a| b\nc");
        assert_eq!(lines_op("[a\nb\nc]", join_lines), "[a b c]");
        assert_eq!(lines_op("a\nb|", join_lines), "a\nb|", "last line");
        assert_eq!(
            lines_op("|\n  b", join_lines),
            "|b",
            "no space after empty line"
        );
    }

    #[test]
    fn case_transforms_selection_or_word_at_caret() {
        assert_eq!(
            lines_op("[abc] def", |d, s| transform_case(d, s, true)),
            "[ABC] def"
        );
        assert_eq!(
            lines_op("ab|c def", |d, s| transform_case(d, s, true)),
            "AB|C def"
        );
        assert_eq!(
            lines_op("[ABC]", |d, s| transform_case(d, s, false)),
            "[abc]"
        );
        assert_eq!(
            lines_op("| x", |d, s| transform_case(d, s, true)),
            "| x",
            "no word, no change"
        );
    }

    #[test]
    fn trim_trailing_whitespace_and_sort_lines() {
        assert_eq!(
            lines_op("a  \nb\t|\nc", trim_trailing_whitespace),
            "a\nb|\nc"
        );
        assert_eq!(
            lines_op("[c\na\nb]", |d, s| sort_lines(d, s, false)),
            "[a\nb\nc]"
        );
        assert_eq!(
            lines_op("[a\nc\nb]", |d, s| sort_lines(d, s, true)),
            "[c\nb\na]"
        );
        assert_eq!(
            lines_op("c|\na", |d, s| sort_lines(d, s, false)),
            "c|\na",
            "needs a multi-line selection"
        );
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
