//! Code folding by indentation.
//!
//! A line can fold when the next non-blank line is indented deeper. Folding
//! hides every following line up to the last one that is still deeper than
//! the header, so a closing `}` at the header's indent stays visible. For
//! Allman style (`{` alone on the line after the header) the header folds
//! the brace line and its body.
//!
//! Folds are kept as line numbers. They follow edits above them and drop
//! when an edit touches the hidden lines.

use ropey::Rope;

use crate::document::Document;
use crate::rope_diff::changed_bytes;

const TAB_COLUMNS: usize = 4;
/// How far to look for the neighbouring non-blank line.
const SCAN_LIMIT: usize = 1000;

/// Indent columns of `line`, or `None` when it holds only whitespace.
fn indent_of(doc: &Document, line: usize) -> Option<usize> {
    let start = doc.line_to_char(line);
    let end = doc.line_end_char(line);
    let mut columns = 0;
    for c in doc.rope().slice(start..end).chars() {
        match c {
            ' ' => columns += 1,
            '\t' => columns += TAB_COLUMNS,
            c if c.is_whitespace() => {}
            _ => return Some(columns),
        }
    }
    None
}

fn next_nonblank(doc: &Document, line: usize) -> Option<(usize, usize)> {
    let mut at = line + 1;
    while at < doc.line_count() && at - line <= SCAN_LIMIT {
        if let Some(indent) = indent_of(doc, at) {
            return Some((at, indent));
        }
        at += 1;
    }
    None
}

fn prev_nonblank(doc: &Document, line: usize) -> Option<(usize, usize)> {
    let mut at = line;
    let mut steps = 0;
    while at > 0 && steps < SCAN_LIMIT {
        at -= 1;
        steps += 1;
        if let Some(indent) = indent_of(doc, at) {
            return Some((at, indent));
        }
    }
    None
}

fn is_lone_open_brace(doc: &Document, line: usize) -> bool {
    let start = doc.line_to_char(line);
    let end = doc.line_end_char(line);
    doc.rope()
        .slice(start..end)
        .chars()
        .filter(|c| !c.is_whitespace())
        .eq(std::iter::once('{'))
}

enum Shape {
    /// The next non-blank line is deeper.
    Indented,
    /// The next non-blank line is a lone `{` opening a deeper block.
    Allman(usize),
}

/// Cheap check: decides from the next non-blank line only.
fn shape(doc: &Document, line: usize) -> Option<(Shape, usize)> {
    if line >= doc.line_count() {
        return None;
    }
    let base = indent_of(doc, line)?;
    let (next, next_indent) = next_nonblank(doc, line)?;
    if next_indent > base {
        return Some((Shape::Indented, base));
    }
    if next_indent == base
        && is_lone_open_brace(doc, next)
        && next_nonblank(doc, next).is_some_and(|(_, inner)| inner > base)
    {
        return Some((Shape::Allman(next), base));
    }
    None
}

/// A `{` alone under its header: the header folds, not the brace.
fn is_allman_brace(doc: &Document, line: usize) -> bool {
    let Some(base) = indent_of(doc, line) else {
        return false;
    };
    is_lone_open_brace(doc, line) && prev_nonblank(doc, line).is_some_and(|(_, indent)| indent == base)
}

/// `true` when a fold marker belongs on `line`.
pub fn is_foldable(doc: &Document, line: usize) -> bool {
    shape(doc, line).is_some() && !is_allman_brace(doc, line)
}

/// Last line hidden when `line` folds.
pub fn fold_end(doc: &Document, line: usize) -> Option<usize> {
    let (shape, base) = shape(doc, line)?;
    let from = match shape {
        Shape::Indented => line,
        Shape::Allman(brace) => brace,
    };
    let mut end = from;
    for at in from + 1..doc.line_count() {
        match indent_of(doc, at) {
            None => {}
            Some(indent) if indent > base => end = at,
            Some(_) => break,
        }
    }
    Some(end)
}

/// The folds of one document.
#[derive(Debug, Clone, Default)]
pub struct FoldSet {
    /// `(header line, last hidden line)`, sorted by header.
    folds: Vec<(usize, usize)>,
    /// `folds` merged into disjoint `(first, last)` hidden line ranges.
    hidden: Vec<(usize, usize)>,
    /// Text the line numbers above refer to.
    base: Option<Rope>,
    revision: u64,
}

impl FoldSet {
    pub fn is_empty(&self) -> bool {
        self.folds.is_empty()
    }

    /// Moves folds along with edits made since the last call; folds that an
    /// edit touches are dropped.
    pub fn sync(&mut self, doc: &Document) {
        if self.folds.is_empty() {
            self.base = None;
            return;
        }
        if doc.revision() == self.revision {
            return;
        }
        let Some(old) = self.base.take() else {
            self.folds.clear();
            self.changed();
            return;
        };
        let before = self.folds.clone();
        let new = doc.rope();
        if let Some(change) = changed_bytes(&old, new) {
            let first = old.byte_to_line(change.start);
            let old_last = old.byte_to_line(change.old_end);
            let delta = new.len_lines() as isize - old.len_lines() as isize;
            let within_one_line = first == old_last && delta == 0;
            let lines = new.len_lines();
            self.folds.retain_mut(|(header, end)| {
                if old_last < *header {
                    let shift = |line: usize| line.checked_add_signed(delta);
                    match (shift(*header), shift(*end)) {
                        (Some(h), Some(e)) if e < lines => {
                            *header = h;
                            *end = e;
                            true
                        }
                        _ => false,
                    }
                } else if first > *end {
                    true
                } else {
                    // Typing on the header line keeps the fold.
                    within_one_line && first == *header
                }
            });
        }
        self.revision = doc.revision();
        self.base = if self.folds.is_empty() {
            None
        } else {
            Some(new.clone())
        };
        if self.folds != before {
            self.changed();
        }
    }

    fn changed(&mut self) {
        self.hidden.clear();
        for &(header, end) in &self.folds {
            let (first, last) = (header + 1, end);
            match self.hidden.last_mut() {
                Some(prev) if first <= prev.1 + 1 => prev.1 = prev.1.max(last),
                _ => self.hidden.push((first, last)),
            }
        }
    }


    /// Folds `line`, or unfolds it when it is already folded. `false` when
    /// the line has nothing to fold.
    pub fn toggle(&mut self, doc: &Document, line: usize) -> bool {
        self.sync(doc);
        if let Some(at) = self.folds.iter().position(|f| f.0 == line) {
            self.folds.remove(at);
            if self.folds.is_empty() {
                self.base = None;
            }
            self.changed();
            return true;
        }
        if !is_foldable(doc, line) {
            return false;
        }
        let Some(end) = fold_end(doc, line) else {
            return false;
        };
        self.folds.push((line, end));
        self.folds.sort_unstable();
        self.base = Some(doc.rope().clone());
        self.revision = doc.revision();
        self.changed();
        true
    }

    pub fn is_folded(&self, line: usize) -> bool {
        self.folds.iter().any(|f| f.0 == line)
    }

    /// Merged `(first, last)` hidden line ranges, sorted.
    pub fn hidden_ranges(&self) -> &[(usize, usize)] {
        &self.hidden
    }

    /// The merged hidden range holding `line`. The line above its start is
    /// the header that stays visible.
    pub fn hidden_at(&self, line: usize) -> Option<(usize, usize)> {
        let at = self.hidden.partition_point(|&(_, last)| last < line);
        self.hidden
            .get(at)
            .copied()
            .filter(|&(first, _)| first <= line)
    }

    pub fn is_hidden(&self, line: usize) -> bool {
        self.hidden_at(line).is_some()
    }

    pub fn clear(&mut self) {
        if !self.folds.is_empty() {
            self.folds.clear();
            self.base = None;
            self.changed();
        }
    }

    /// Unfolds everything that hides `line`. `true` if anything changed.
    pub fn reveal(&mut self, line: usize) -> bool {
        let before = self.folds.len();
        self.folds.retain(|&(header, end)| !(header < line && line <= end));
        if self.folds.is_empty() {
            self.base = None;
        }
        let changed = self.folds.len() != before;
        if changed {
            self.changed();
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "fn a() {\n    one;\n\n    two;\n}\n\nfn b() {\n    if x {\n        y;\n    }\n}\n";

    fn doc(text: &str) -> Document {
        Document::from_text(text)
    }

    #[test]
    fn deeper_next_line_makes_a_fold_that_leaves_the_closing_line() {
        let d = doc(SRC);
        assert!(is_foldable(&d, 0));
        assert_eq!(fold_end(&d, 0), Some(3), "trailing blank and `}}` stay");
        assert!(!is_foldable(&d, 1), "`one;` has no deeper child");
        assert!(!is_foldable(&d, 4));
        assert!(!is_foldable(&d, 5), "blank line");
        assert!(is_foldable(&d, 7));
        assert_eq!(fold_end(&d, 6), Some(9));
        assert_eq!(fold_end(&d, 7), Some(8));
    }

    #[test]
    fn allman_header_folds_the_brace_and_body() {
        let d = doc("class A\n{\n    x;\n    y;\n}\n");
        assert!(is_foldable(&d, 0));
        assert!(!is_foldable(&d, 1), "the brace line has no marker of its own");
        assert_eq!(fold_end(&d, 0), Some(3));
    }

    #[test]
    fn tabs_count_as_four_columns() {
        let d = doc("a\n\tb\n    c\n");
        assert_eq!(fold_end(&d, 0), Some(2));
    }

    #[test]
    fn toggle_folds_and_unfolds() {
        let d = doc(SRC);
        let mut folds = FoldSet::default();
        assert!(!folds.toggle(&d, 1), "nothing to fold");
        assert!(folds.toggle(&d, 0));
        assert!(folds.is_folded(0));
        assert_eq!(folds.hidden_ranges(), [(1, 3)]);
        assert!(folds.is_hidden(2) && !folds.is_hidden(0) && !folds.is_hidden(4));
        assert!(folds.toggle(&d, 0));
        assert!(folds.is_empty());
    }

    #[test]
    fn nested_folds_merge_into_one_hidden_range() {
        let d = doc(SRC);
        let mut folds = FoldSet::default();
        assert!(folds.toggle(&d, 7));
        assert!(folds.toggle(&d, 6));
        assert_eq!(folds.hidden_ranges(), [(7, 9)]);
        assert_eq!(folds.hidden_at(8), Some((7, 9)));
        assert_eq!(folds.hidden_at(0), None);
    }

    #[test]
    fn reveal_unfolds_what_hides_a_line() {
        let d = doc(SRC);
        let mut folds = FoldSet::default();
        folds.toggle(&d, 0);
        assert!(!folds.reveal(0), "the header is visible");
        assert!(folds.reveal(2));
        assert!(folds.is_empty());
    }

    #[test]
    fn folds_follow_edits_above_and_drop_when_touched() {
        let mut d = doc(SRC);
        let mut folds = FoldSet::default();
        folds.toggle(&d, 6);
        let before = folds.hidden_ranges().to_vec();
        // A new line above shifts the fold down.
        let at = d.line_to_char(0);
        d.apply(&crate::change::ChangeSet::new(
            vec![crate::change::Change::insert(at, "// top\n")],
            d.len_chars(),
        ));
        folds.sync(&d);
        assert_eq!(folds.hidden_ranges(), [(before[0].0 + 1, before[0].1 + 1)]);

        // Typing on the header line keeps it.
        let header_end = d.line_end_char(7);
        d.apply(&crate::change::ChangeSet::new(
            vec![crate::change::Change::insert(header_end, " // note")],
            d.len_chars(),
        ));
        folds.sync(&d);
        assert!(folds.is_folded(7));

        // An edit inside the hidden lines unfolds.
        let inside = d.line_to_char(9);
        d.apply(&crate::change::ChangeSet::new(
            vec![crate::change::Change::insert(inside, "z")],
            d.len_chars(),
        ));
        folds.sync(&d);
        assert!(folds.is_empty());
    }
}
