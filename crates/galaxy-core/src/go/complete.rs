//! Completion for Go: members after a `.`, identifiers, import paths,
//! struct literal keys and types.
//!
//! The context is read from the text before the cursor (lexical state,
//! open brackets), so incomplete code works; the syntax tree is only used
//! for the names in scope and for types ([`super::types`]). Lines are
//! 0-based and columns UTF-16, as in [`super::parse`].

use std::collections::HashSet;
use std::path::Path;

use tree_sitter::Tree;

use super::engine::GoEngine;
use crate::lang::{Completion, Insertion, ItemKind};

mod ident;
mod imports;
mod literal;
mod member;
mod scan;

#[cfg(test)]
mod tests;

use scan::{Lex, Pos, Scan};

/// Items returned at most.
const MAX_ITEMS: usize = 200;

/// A candidate before filtering and sorting.
struct Item {
    pub label: String,
    pub detail: Option<String>,
    pub insert: String,
    pub snippet: bool,
    pub kind: ItemKind,
    /// 0 members and locals, 1 package level, 2 imports, 3 universe,
    /// 4 keywords, 5 auto-imports.
    pub tier: u8,
    /// Deduplication key; the label unless several items share it.
    pub key: String,
    pub extra: Vec<Insertion>,
    pub replace_from: Option<u32>,
}

impl Item {
    pub fn new(label: &str, kind: ItemKind, tier: u8) -> Self {
        Self {
            label: label.to_owned(),
            detail: None,
            insert: label.to_owned(),
            snippet: false,
            kind,
            tier,
            key: label.to_owned(),
            extra: Vec::new(),
            replace_from: None,
        }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        self.detail = (!detail.is_empty()).then_some(detail);
        self
    }

    /// Inserts `Name()` for no parameters, else the snippet `Name($1)`.
    pub fn call(mut self, takes_params: bool) -> Self {
        if takes_params {
            self.insert = format!("{}($1)", self.label);
            self.snippet = true;
        } else {
            self.insert = format!("{}()", self.label);
        }
        self
    }
}

/// Everything the contexts need about the cursor.
struct Cx<'a> {
    pub engine: &'a mut GoEngine,
    pub path: &'a Path,
    pub src: &'a str,
    pub tree: &'a Tree,
    pub byte: usize,
    /// Start of the identifier prefix before the cursor.
    pub start: usize,
    pub scan: Scan,
}

impl Cx<'_> {
    pub fn prefix(&self) -> &str {
        &self.src[self.start..self.byte]
    }
}

pub(super) fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Byte offset of a 0-based line and UTF-16 column, clamped to the line.
fn offset(src: &str, line: u32, column: u32) -> usize {
    let mut start = 0;
    for _ in 0..line {
        match src[start..].find('\n') {
            Some(i) => start += i + 1,
            None => return src.len(),
        }
    }
    let rest = &src[start..];
    let text = &rest[..rest.find('\n').unwrap_or(rest.len())];
    let mut units = 0u32;
    for (index, c) in text.char_indices() {
        if units >= column {
            return start + index;
        }
        units += c.len_utf16() as u32;
    }
    start + text.len()
}

/// UTF-16 column of `byte`.
pub(super) fn column_of(src: &str, byte: usize) -> u32 {
    let line_start = src[..byte].rfind('\n').map_or(0, |i| i + 1);
    src[line_start..byte].encode_utf16().count() as u32
}

/// Matches the typed text at the start, preserving case.
fn match_rank(text: &str, typed: &str) -> Option<u8> {
    text.starts_with(typed).then_some(0)
}

/// Filters `items` by `typed`, ranks, deduplicates and caps them.
fn finish(items: Vec<Item>, typed: &str) -> Vec<Completion> {
    let mut seen = HashSet::new();
    let mut out: Vec<Completion> = Vec::new();
    for item in items {
        let Some(rank) = match_rank(&item.label, typed) else {
            continue;
        };
        if !seen.insert(item.key.clone()) {
            continue;
        }
        let sort = format!("{rank}{}{}{}", item.tier, item.label, item.key);
        out.push(Completion {
            label: item.label,
            detail: item.detail,
            insert_text: item.insert,
            is_snippet: item.snippet,
            kind: item.kind,
            sort,
            extra: item.extra,
            replace_from: item.replace_from,
        });
    }
    out.sort_by(|a, b| a.sort.cmp(&b.sort));
    out.truncate(MAX_ITEMS);
    out
}

/// Completions at `line`/`column_utf16` of `src` (the buffer of `path`,
/// parsed as `tree`), best first, at most 200.
pub fn complete(
    engine: &mut GoEngine,
    path: &Path,
    src: &str,
    tree: &Tree,
    line: u32,
    column_utf16: u32,
) -> Vec<Completion> {
    let byte = offset(src, line, column_utf16);
    let scan = scan::scan(src, byte);
    let mut start = byte;
    for (i, c) in src[..byte].char_indices().rev() {
        if !is_ident(c) {
            break;
        }
        start = i;
    }
    let mut cx = Cx {
        engine,
        path,
        src,
        tree,
        byte,
        start,
        scan,
    };
    match cx.scan.lex {
        Lex::Code => {}
        Lex::Str(quote) if imports::in_import(src, quote) => {
            let from = quote + 1;
            let typed = src.get(from..byte).unwrap_or("").to_owned();
            return finish(imports::items(&mut cx, from), &typed);
        }
        _ => return Vec::new(),
    }
    let prefix = cx.prefix().to_owned();
    if prefix.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return Vec::new();
    }
    let before = &src[..start];
    if before.ends_with('.') && !before.ends_with("..") {
        let dot = start - 1;
        if member::after_number(src, dot) {
            return Vec::new();
        }
        return finish(member::items(&mut cx, dot), &prefix);
    }
    let pos = scan::type_position(src, start, &cx.scan.stack);
    if pos == Pos::Name {
        return Vec::new();
    }
    if pos == Pos::No
        && let Some(items) = literal::key_items(&mut cx)
    {
        return finish(items, &prefix);
    }
    finish(ident::items(&mut cx, pos == Pos::Type), &prefix)
}
