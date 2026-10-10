//! Find and replace in the current file (M1-13).
//!
//! Matching runs **one line at a time** on the line's text without its
//! `\n`. That keeps the UI responsive on huge files and makes `^` and `$`
//! mean line start and end. The consequences, which are deliberate:
//!
//! * A pattern cannot match across lines, so `\n` never matches.
//! * Empty matches (`x*` on `abc`, or a bare `^`) are ignored.
//! * Matches do not overlap: `aa` in `aaaa` matches twice.
//!
//! Replacement text is literal unless regex mode is on, where `$1`,
//! `${name}` and `$$` work as in the `regex` crate.

use std::borrow::Cow;
use std::fmt;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

use regex::{Regex, RegexBuilder};

use crate::change::Change;
use crate::document::Document;
use crate::edit::{Item, Transaction, finish_with_carets, finish_with_map};
use crate::movement::is_word_char;
use crate::selection::{Selection, SelectionSet};

/// Match counting and "select all matches" stop here so that a one-letter
/// pattern in a huge file stays cheap.
pub const MAX_MATCHES: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FindOptions {
    pub case_sensitive: bool,
    /// Matches must not touch a word character on either side. The check
    /// only applies on an edge where the match itself has a word character,
    /// so `+=` still matches inside `a+=b`.
    pub whole_word: bool,
    /// Treat the pattern as a regular expression.
    pub regex: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindError {
    Empty,
    /// The regex did not compile. The message is one short line.
    Invalid(String),
}

impl fmt::Display for FindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FindError::Empty => write!(f, "empty pattern"),
            FindError::Invalid(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for FindError {}

/// How many matches there are and which one is selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Summary {
    pub total: usize,
    /// 1-based index of the match equal to the primary selection.
    pub current: Option<usize>,
    /// There are more matches than `MAX_MATCHES`; `total` is a lower bound.
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct Finder {
    re: Regex,
    options: FindOptions,
}

/// A match found in one line's text: byte range plus, when asked for, the
/// expanded replacement.
type Raw = (Range<usize>, Option<String>);

impl Finder {
    pub fn new(pattern: &str, options: FindOptions) -> Result<Self, FindError> {
        if pattern.is_empty() {
            return Err(FindError::Empty);
        }
        let source = if options.regex {
            Cow::Borrowed(pattern)
        } else {
            Cow::Owned(regex::escape(pattern))
        };
        let re = RegexBuilder::new(&source)
            .case_insensitive(!options.case_sensitive)
            .build()
            .map_err(|e| FindError::Invalid(short_message(&e.to_string())))?;
        Ok(Self { re, options })
    }

    pub fn options(&self) -> FindOptions {
        self.options
    }

    // --- Matching one line ---------------------------------------------

    fn scan(&self, text: &str, replacement: Option<&str>) -> Vec<Raw> {
        let mut hits = Vec::new();
        let mut pos = 0;
        while pos <= text.len() {
            let (range, expanded) = match replacement {
                Some(template) if self.options.regex => {
                    let Some(caps) = self.re.captures_at(text, pos) else {
                        break;
                    };
                    let whole = caps.get(0).expect("group 0 always exists").range();
                    let mut out = String::new();
                    caps.expand(template, &mut out);
                    (whole, Some(out))
                }
                Some(literal) => match self.re.find_at(text, pos) {
                    Some(m) => (m.range(), Some(literal.to_string())),
                    None => break,
                },
                None => match self.re.find_at(text, pos) {
                    Some(m) => (m.range(), None),
                    None => break,
                },
            };
            let skip_to = |from: usize| text[from..].chars().next().map(|c| from + c.len_utf8());
            if range.is_empty() {
                match skip_to(range.end) {
                    Some(next) => pos = next,
                    None => break,
                }
                continue;
            }
            if self.options.whole_word && !is_bounded(text, &range) {
                match skip_to(range.start) {
                    Some(next) => pos = next,
                    None => break,
                }
                continue;
            }
            pos = range.end;
            hits.push((range, expanded));
        }
        hits
    }

    /// Match ranges in `line` as char offsets in the document.
    pub fn matches_in_line(&self, doc: &Document, line: usize) -> Vec<Range<usize>> {
        let text = line_text(doc, line);
        let base = doc.line_to_char(line);
        let hits = self.scan(&text, None);
        measure(&text, &hits, |s| s.chars().count())
            .into_iter()
            .map(|r| base + r.start..base + r.end)
            .collect()
    }

    /// Match ranges in `line` as UTF-16 columns within the line, which is
    /// what the renderer needs to draw highlights.
    pub fn columns_in_line(&self, doc: &Document, line: usize) -> Vec<Range<usize>> {
        self.columns_in_text(&line_text(doc, line))
    }

    /// Match ranges in `text` as UTF-16 columns. The viewport uses this on
    /// the displayed (possibly cut) line, so a minified 10 MB line costs
    /// only as much as the part on screen.
    pub fn columns_in_text(&self, text: &str) -> Vec<Range<usize>> {
        let hits = self.scan(text, None);
        measure(text, &hits, |s| s.encode_utf16().count())
    }

    // --- Whole document ------------------------------------------------

    /// Up to `limit` matches in document order.
    pub fn all_matches(&self, doc: &Document, limit: usize) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        for line in 0..doc.line_count() {
            if out.len() >= limit {
                break;
            }
            out.extend(self.matches_in_line(doc, line));
        }
        out.truncate(limit);
        out
    }

    /// Like `all_matches`, but gives up (returns `None`) as soon as
    /// `cancel` is set. Checked every 256 lines, so a cancelled count on a
    /// huge file stops within a fraction of a millisecond.
    pub fn all_matches_cancellable(
        &self,
        doc: &Document,
        limit: usize,
        cancel: &AtomicBool,
    ) -> Option<Vec<Range<usize>>> {
        let mut out = Vec::new();
        for line in 0..doc.line_count() {
            if line % 256 == 0 && cancel.load(Ordering::Relaxed) {
                return None;
            }
            if out.len() >= limit {
                break;
            }
            out.extend(self.matches_in_line(doc, line));
        }
        out.truncate(limit);
        Some(out)
    }

    pub fn summary(&self, doc: &Document, sel: &SelectionSet) -> Summary {
        summarize(self.all_matches(doc, MAX_MATCHES + 1), sel)
    }

    /// `summary` that stops early (returning `None`) once `cancel` is set,
    /// for counting on a background thread.
    pub fn summary_cancellable(
        &self,
        doc: &Document,
        sel: &SelectionSet,
        cancel: &AtomicBool,
    ) -> Option<Summary> {
        self.all_matches_cancellable(doc, MAX_MATCHES + 1, cancel)
            .map(|all| summarize(all, sel))
    }

    /// First match starting at or after `from`. With `wrap` the search
    /// continues from the top of the document.
    pub fn find_next(&self, doc: &Document, from: usize, wrap: bool) -> Option<Range<usize>> {
        let from = from.min(doc.len_chars());
        let first = doc.char_to_line(from);
        for line in first..doc.line_count() {
            let hit = self
                .matches_in_line(doc, line)
                .into_iter()
                .find(|m| line != first || m.start >= from);
            if hit.is_some() {
                return hit;
            }
        }
        if wrap {
            for line in 0..=first {
                if let Some(m) = self.matches_in_line(doc, line).into_iter().next() {
                    return Some(m);
                }
            }
        }
        None
    }

    /// Last match starting before `before`. With `wrap` the search
    /// continues from the bottom of the document.
    pub fn find_prev(&self, doc: &Document, before: usize, wrap: bool) -> Option<Range<usize>> {
        let before = before.min(doc.len_chars());
        let first = doc.char_to_line(before);
        for line in (0..=first).rev() {
            let hit = self
                .matches_in_line(doc, line)
                .into_iter()
                .rev()
                .find(|m| line != first || m.start < before);
            if hit.is_some() {
                return hit;
            }
        }
        if wrap {
            for line in (first..doc.line_count()).rev() {
                if let Some(m) = self.matches_in_line(doc, line).pop() {
                    return Some(m);
                }
            }
        }
        None
    }

    /// Selects every match (Alt+Enter in the find bar). `near` picks the
    /// primary: the first match at or after it.
    pub fn select_all(&self, doc: &Document, near: usize) -> Option<SelectionSet> {
        let all = self.all_matches(doc, MAX_MATCHES);
        if all.is_empty() {
            return None;
        }
        let primary = all.iter().position(|m| m.start >= near).unwrap_or(0);
        let ranges = all.iter().map(|m| Selection::new(m.start, m.end)).collect();
        Some(SelectionSet::new(ranges, primary, doc))
    }

    // --- Replace ---------------------------------------------------------

    /// Replaces the primary selection if, and only if, it is exactly one
    /// match. The caret lands after the inserted text.
    pub fn replace_current(
        &self,
        doc: &Document,
        sel: &SelectionSet,
        replacement: &str,
    ) -> Option<Transaction> {
        let p = sel.primary();
        if p.is_empty() {
            return None;
        }
        let line = doc.char_to_line(p.start());
        if doc.char_to_line(p.end()) != line {
            return None;
        }
        let text = line_text(doc, line);
        let base = doc.line_to_char(line);
        let hits = self.scan(&text, Some(replacement));
        let ranges = measure(&text, &hits, |s| s.chars().count());
        let i = ranges
            .iter()
            .position(|r| base + r.start == p.start() && base + r.end == p.end())?;
        let new_text = hits[i].1.clone().unwrap_or_default();
        let item = Item::new(p.start(), p.end(), new_text);
        Some(finish_with_carets(
            doc,
            &SelectionSet::single(p),
            vec![item],
        ))
    }

    /// Replaces every match. Returns the edit and how many were replaced.
    pub fn replace_all(
        &self,
        doc: &Document,
        sel: &SelectionSet,
        replacement: &str,
    ) -> Option<(Transaction, usize)> {
        let mut changes = Vec::new();
        for line in 0..doc.line_count() {
            let text = line_text(doc, line);
            let base = doc.line_to_char(line);
            let hits = self.scan(&text, Some(replacement));
            if hits.is_empty() {
                continue;
            }
            let ranges = measure(&text, &hits, |s| s.chars().count());
            for (range, (_, new_text)) in ranges.into_iter().zip(hits) {
                changes.push(Change::new(
                    base + range.start,
                    base + range.end,
                    new_text.unwrap_or_default(),
                ));
            }
        }
        if changes.is_empty() {
            return None;
        }
        let count = changes.len();
        Some((finish_with_map(doc, sel, changes, |_| false), count))
    }
}

/// Total and current index from up to `MAX_MATCHES + 1` matches.
fn summarize(mut all: Vec<Range<usize>>, sel: &SelectionSet) -> Summary {
    let truncated = all.len() > MAX_MATCHES;
    all.truncate(MAX_MATCHES);
    let p = sel.primary();
    let current = (!p.is_empty())
        .then(|| {
            all.iter()
                .position(|m| m.start == p.start() && m.end == p.end())
        })
        .flatten()
        .map(|i| i + 1);
    Summary {
        total: all.len(),
        current,
        truncated,
    }
}

/// Text of `line` without its line break; borrowed when it sits in one
/// rope chunk.
fn line_text(doc: &Document, line: usize) -> Cow<'_, str> {
    let start = doc.line_to_char(line);
    let end = doc.line_end_char(line);
    let slice = doc.rope().slice(start..end);
    match slice.as_str() {
        Some(s) => Cow::Borrowed(s),
        None => Cow::Owned(slice.to_string()),
    }
}

/// Converts byte ranges into another unit (chars or UTF-16) in one pass.
fn measure(text: &str, hits: &[Raw], unit: impl Fn(&str) -> usize) -> Vec<Range<usize>> {
    let mut out = Vec::with_capacity(hits.len());
    let (mut byte, mut acc) = (0, 0);
    for (r, _) in hits {
        acc += unit(&text[byte..r.start]);
        let start = acc;
        acc += unit(&text[r.start..r.end]);
        out.push(start..acc);
        byte = r.end;
    }
    out
}

fn is_bounded(text: &str, range: &Range<usize>) -> bool {
    let inner = &text[range.clone()];
    let starts_word = inner.chars().next().is_some_and(is_word_char);
    let ends_word = inner.chars().next_back().is_some_and(is_word_char);
    let before_ok = !starts_word
        || !text[..range.start]
            .chars()
            .next_back()
            .is_some_and(is_word_char);
    let after_ok = !ends_word || !text[range.end..].chars().next().is_some_and(is_word_char);
    before_ok && after_ok
}

/// The regex crate's errors are several lines with the pattern echoed back;
/// the last line is the actual complaint.
fn short_message(full: &str) -> String {
    full.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().trim_start_matches("error:").trim().to_string())
        .unwrap_or_else(|| full.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn doc(text: &str) -> Document {
        Document::from_text(text)
    }

    fn opts(case: bool, word: bool, regex: bool) -> FindOptions {
        FindOptions {
            case_sensitive: case,
            whole_word: word,
            regex,
        }
    }

    fn finder(pattern: &str, o: FindOptions) -> Finder {
        Finder::new(pattern, o).unwrap()
    }

    fn all(f: &Finder, d: &Document) -> Vec<(usize, usize)> {
        f.all_matches(d, usize::MAX)
            .iter()
            .map(|r| (r.start, r.end))
            .collect()
    }

    fn caret(n: usize) -> SelectionSet {
        SelectionSet::single(Selection::caret(n))
    }

    #[test]
    fn literal_search_honors_case() {
        let d = doc("Foo foo FOO");
        assert_eq!(
            all(&finder("foo", opts(true, false, false)), &d),
            vec![(4, 7)]
        );
        assert_eq!(
            all(&finder("foo", opts(false, false, false)), &d),
            vec![(0, 3), (4, 7), (8, 11)]
        );
        // Unicode case folding.
        let d = doc("\u{c9}cole \u{e9}cole");
        assert_eq!(
            all(&finder("\u{e9}cole", opts(false, false, false)), &d).len(),
            2
        );
        assert_eq!(
            all(&finder("\u{e9}cole", opts(true, false, false)), &d).len(),
            1
        );
    }

    #[test]
    fn literal_mode_escapes_regex_syntax() {
        let d = doc("a.b a+b (x) a*b");
        let f = finder(".", opts(true, false, false));
        assert_eq!(all(&f, &d), vec![(1, 2)]);
        assert_eq!(
            all(&finder("(x)", opts(true, false, false)), &d),
            vec![(8, 11)]
        );
        assert_eq!(
            all(&finder("a*b", opts(true, false, false)), &d),
            vec![(12, 15)]
        );
    }

    #[test]
    fn whole_word_skips_longer_words_but_not_symbol_matches() {
        let d = doc("cat concat cat_x cat. cat");
        let f = finder("cat", opts(true, true, false));
        assert_eq!(all(&f, &d), vec![(0, 3), (17, 20), (22, 25)]);
        // A match that starts and ends with a symbol has no word edge to check.
        let d = doc("a+=b");
        assert_eq!(
            all(&finder("+=", opts(true, true, false)), &d),
            vec![(1, 3)]
        );
        // Retry after a rejected match: "ab" is rejected inside "cab", found later.
        let d = doc("cab ab");
        assert_eq!(
            all(&finder("ab", opts(true, true, false)), &d),
            vec![(4, 6)]
        );
    }

    #[test]
    fn regex_mode_matches_per_line_with_anchors() {
        let d = doc("foo1 bar22\nbaz3\nfoo");
        let f = finder(r"[a-z]+\d+", opts(true, false, true));
        assert_eq!(all(&f, &d), vec![(0, 4), (5, 10), (11, 15)]);
        let f = finder("^foo$", opts(true, false, true));
        assert_eq!(all(&f, &d), vec![(16, 19)]);
        let f = finder(r"\bbar", opts(true, false, true));
        assert_eq!(all(&f, &d), vec![(5, 8)]);
        // Newlines are not part of the searched text.
        assert!(all(&finder(r"1\n", opts(true, false, true)), &d).is_empty());
    }

    #[test]
    fn bad_patterns_report_short_errors() {
        assert_eq!(
            Finder::new("", FindOptions::default()).unwrap_err(),
            FindError::Empty
        );
        let err = Finder::new("(", opts(true, false, true)).unwrap_err();
        let FindError::Invalid(message) = err else {
            panic!("expected Invalid")
        };
        assert!(!message.contains('\n'), "{message:?}");
        assert!(message.contains("unclosed group"), "{message:?}");
        // The same text is fine as a literal.
        assert!(Finder::new("(", opts(true, false, false)).is_ok());
    }

    #[test]
    fn matches_never_overlap_and_empty_matches_are_ignored() {
        let d = doc("aaaa");
        assert_eq!(
            all(&finder("aa", opts(true, false, false)), &d),
            vec![(0, 2), (2, 4)]
        );
        let d = doc("abc\n\nxyz");
        assert!(all(&finder("x*", opts(true, false, true)), &d).len() == 1);
        assert!(all(&finder("^", opts(true, false, true)), &d).is_empty());
        assert!(all(&finder("q*", opts(true, false, true)), &d).is_empty());
    }

    #[test]
    fn columns_are_utf16_and_offsets_are_chars() {
        // The emoji is 1 char and 2 UTF-16 units.
        let d = doc("x\u{1F600}ab\nab");
        let f = finder("ab", opts(true, false, false));
        assert_eq!(f.matches_in_line(&d, 0), vec![2..4]);
        assert_eq!(f.columns_in_line(&d, 0), vec![3..5]);
        assert_eq!(f.matches_in_line(&d, 1), vec![5..7]);
        assert_eq!(f.columns_in_line(&d, 1), vec![0..2]);
    }

    #[test]
    fn next_and_previous_wrap_around() {
        let d = doc("ab x ab\nab");
        let f = finder("ab", opts(true, false, false));
        assert_eq!(f.find_next(&d, 0, false), Some(0..2));
        assert_eq!(f.find_next(&d, 1, false), Some(5..7));
        assert_eq!(f.find_next(&d, 7, false), Some(8..10));
        assert_eq!(f.find_next(&d, 9, false), None);
        assert_eq!(f.find_next(&d, 9, true), Some(0..2), "wraps to the top");
        assert_eq!(f.find_prev(&d, 10, false), Some(8..10));
        assert_eq!(f.find_prev(&d, 8, false), Some(5..7));
        assert_eq!(f.find_prev(&d, 0, false), None);
        assert_eq!(f.find_prev(&d, 0, true), Some(8..10), "wraps to the bottom");

        // A single match finds itself after wrapping.
        let one = doc("only ab here");
        let f = finder("ab", opts(true, false, false));
        assert_eq!(f.find_next(&one, 7, true), Some(5..7));
        assert_eq!(f.find_prev(&one, 5, true), Some(5..7));
        assert_eq!(
            finder("zz", opts(true, false, false)).find_next(&one, 0, true),
            None
        );
    }

    #[test]
    fn summary_reports_total_and_current_index() {
        let d = doc("ab ab ab");
        let f = finder("ab", opts(true, false, false));
        let none = f.summary(&d, &caret(0));
        assert_eq!((none.total, none.current, none.truncated), (3, None, false));
        let on_second = f.summary(&d, &SelectionSet::single(Selection::new(3, 5)));
        assert_eq!(on_second.current, Some(2));
        let partial = f.summary(&d, &SelectionSet::single(Selection::new(3, 4)));
        assert_eq!(partial.current, None, "a partial selection is not a match");
    }

    #[test]
    fn summary_truncates_huge_counts() {
        let d = doc(&"a\n".repeat(MAX_MATCHES + 50));
        let s = finder("a", opts(true, false, false)).summary(&d, &caret(0));
        assert_eq!(s.total, MAX_MATCHES);
        assert!(s.truncated);
    }

    #[test]
    fn cancellable_count_matches_the_plain_one_and_stops_when_cancelled() {
        let d = doc(&"ab x\n".repeat(2_000));
        let f = finder("ab", opts(true, false, false));
        let go = AtomicBool::new(false);
        assert_eq!(
            f.all_matches_cancellable(&d, usize::MAX, &go),
            Some(f.all_matches(&d, usize::MAX))
        );
        let stop = AtomicBool::new(true);
        assert_eq!(f.all_matches_cancellable(&d, usize::MAX, &stop), None);
    }

    #[test]
    fn select_all_builds_one_selection_per_match() {
        let d = doc("ab x ab\nab");
        let f = finder("ab", opts(true, false, false));
        let s = f.select_all(&d, 6).unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(
            s.primary(),
            Selection::new(8, 10),
            "the first match starting at or after 6 (5..7 starts earlier)"
        );
        // Past the last match the primary falls back to the first one.
        assert_eq!(
            f.select_all(&d, 11).unwrap().primary(),
            Selection::new(0, 2)
        );
        assert!(
            finder("zz", opts(true, false, false))
                .select_all(&d, 0)
                .is_none()
        );
    }

    fn apply(d: &mut Document, tx: Transaction, sel: &mut SelectionSet) {
        tx.apply(d, sel);
    }

    #[test]
    fn replace_current_only_replaces_an_exact_match() {
        let mut d = doc("cat hat cat");
        let f = finder("cat", opts(true, false, false));
        let mut sel = SelectionSet::single(Selection::new(0, 3));
        let tx = f.replace_current(&d, &sel, "dog").unwrap();
        apply(&mut d, tx, &mut sel);
        assert_eq!(d.text(), "dog hat cat");
        assert_eq!(
            sel.primary(),
            Selection::caret(3),
            "caret after the replacement"
        );

        // A caret, a partial selection or a non-match is left alone.
        assert!(f.replace_current(&d, &caret(1), "x").is_none());
        let partial = SelectionSet::single(Selection::new(8, 10));
        assert!(f.replace_current(&d, &partial, "x").is_none());
        let other = SelectionSet::single(Selection::new(4, 7));
        assert!(f.replace_current(&d, &other, "x").is_none());
    }

    #[test]
    fn replace_all_literal_keeps_dollars_literal() {
        let mut d = doc("a.b a.b\na.b");
        let f = finder("a.b", opts(true, false, false));
        let mut sel = caret(0);
        let (tx, n) = f.replace_all(&d, &sel, "$1").unwrap();
        apply(&mut d, tx, &mut sel);
        assert_eq!(n, 3);
        assert_eq!(d.text(), "$1 $1\n$1");
    }

    #[test]
    fn replace_all_regex_expands_capture_groups() {
        let mut d = doc("john smith\nada lovelace");
        let f = finder(r"(\w+) (\w+)", opts(true, false, true));
        let mut sel = caret(0);
        let (tx, n) = f.replace_all(&d, &sel, "$2, $1").unwrap();
        apply(&mut d, tx, &mut sel);
        assert_eq!(n, 2);
        assert_eq!(d.text(), "smith, john\nlovelace, ada");

        // Named groups and a literal dollar.
        let mut d = doc("id=7");
        let f = finder(r"id=(?<n>\d+)", opts(true, false, true));
        let mut sel = caret(0);
        let (tx, _) = f.replace_all(&d, &sel, "${n}$$").unwrap();
        apply(&mut d, tx, &mut sel);
        assert_eq!(d.text(), "7$");
    }

    #[test]
    fn replace_all_handles_growth_shrinkage_and_undo_inverse() {
        let mut d = doc("aXa\nXX");
        let f = finder("X", opts(true, false, false));
        let mut sel = caret(0);
        let (tx, n) = f.replace_all(&d, &sel, "LONGER").unwrap();
        let inverse = d.apply(&tx.changes);
        assert_eq!(n, 3);
        assert_eq!(d.text(), "aLONGERa\nLONGERLONGER");
        d.apply(&inverse);
        assert_eq!(d.text(), "aXa\nXX");

        let (tx, _) = f.replace_all(&d, &sel, "").unwrap();
        tx.apply(&mut d, &mut sel);
        assert_eq!(d.text(), "aa\n");
        assert!(
            f.replace_all(&d, &sel, "y").is_none(),
            "nothing left to replace"
        );
    }

    #[test]
    fn replace_counts_only_whole_words_when_asked() {
        let mut d = doc("cat concat cat");
        let f = finder("cat", opts(true, true, false));
        let mut sel = caret(0);
        let (tx, n) = f.replace_all(&d, &sel, "dog").unwrap();
        apply(&mut d, tx, &mut sel);
        assert_eq!(n, 2);
        assert_eq!(d.text(), "dog concat dog");
    }

    fn arb_text() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            prop_oneof![
                Just("a"),
                Just("b"),
                Just("ab"),
                Just("aa"),
                Just(" "),
                Just("\n"),
                Just("\u{e9}"),
                Just("\u{c9}"),
                Just("\u{1F600}"),
            ],
            0..60,
        )
        .prop_map(|v| v.concat())
    }

    proptest! {
        #[test]
        fn literal_matches_agree_with_str_matches(
            text in arb_text(),
            needle in prop_oneof![Just("a"), Just("ab"), Just("aa"), Just("\u{e9}"), Just(" a")],
        ) {
            let d = doc(&text);
            let f = finder(needle, opts(true, false, false));
            let found = f.all_matches(&d, usize::MAX);
            let expected: usize = text.split('\n').map(|l| l.matches(needle).count()).sum();
            prop_assert_eq!(found.len(), expected);
            let chars: Vec<char> = text.chars().collect();
            let want: Vec<char> = needle.chars().collect();
            let mut prev_end = 0;
            for m in &found {
                prop_assert!(m.start >= prev_end && m.end <= chars.len());
                prop_assert_eq!(&chars[m.start..m.end], &want[..]);
                prev_end = m.end;
            }
        }

        #[test]
        fn next_and_prev_agree_with_the_full_match_list(
            text in arb_text(),
            at in 0usize..70,
        ) {
            let d = doc(&text);
            let f = finder("a", opts(true, false, false));
            let all = f.all_matches(&d, usize::MAX);
            let at = at.min(d.len_chars());
            prop_assert_eq!(f.find_next(&d, at, false), all.iter().find(|m| m.start >= at).cloned());
            prop_assert_eq!(f.find_prev(&d, at, false), all.iter().rev().find(|m| m.start < at).cloned());
            prop_assert_eq!(f.find_next(&d, at, true).is_some(), !all.is_empty());
            prop_assert_eq!(f.find_prev(&d, at, true).is_some(), !all.is_empty());
        }

        #[test]
        fn utf16_columns_match_a_direct_computation(text in arb_text()) {
            let d = doc(&text);
            let f = finder("a", opts(false, false, false));
            for (line, src) in text.split('\n').enumerate() {
                let expected: Vec<Range<usize>> = {
                    let mut out = vec![];
                    let mut col = 0;
                    for c in src.chars() {
                        let w = c.len_utf16();
                        if c.to_lowercase().eq("a".chars()) { out.push(col..col + w); }
                        col += w;
                    }
                    out
                };
                prop_assert_eq!(f.columns_in_line(&d, line), expected);
            }
        }

        #[test]
        fn replacing_a_literal_with_itself_changes_nothing_and_counts_match(text in arb_text()) {
            let mut d = doc(&text);
            let f = finder("ab", opts(true, false, false));
            let total = f.all_matches(&d, usize::MAX).len();
            let mut sel = caret(0);
            match f.replace_all(&d, &sel, "ab") {
                Some((tx, n)) => {
                    prop_assert_eq!(n, total);
                    tx.apply(&mut d, &mut sel);
                    prop_assert_eq!(d.text(), text);
                }
                None => prop_assert_eq!(total, 0),
            }
        }
    }
}
