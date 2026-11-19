//! Import paths inside the string of an import spec.

use std::collections::HashSet;

use super::scan::{scan, word_before};
use super::{Cx, Item, column_of, match_rank};
use crate::lang::ItemKind;

/// Paths offered at most.
const MAX_PATHS: usize = 100;

/// Whether the string opened at `quote` is the path of an import spec,
/// alone (`import "x"`, `import alias "x"`) or inside `import ( ... )`.
pub(super) fn in_import(src: &str, quote: usize) -> bool {
    let line_start = src[..quote].rfind('\n').map_or(0, |i| i + 1);
    let line = src[line_start..quote].trim();
    let alias_ok = |s: &str| {
        s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    };
    if let Some(rest) = line.strip_prefix("import")
        && (rest.is_empty() || rest.starts_with(char::is_whitespace))
        && alias_ok(rest.trim())
    {
        return true;
    }
    if !alias_ok(line) {
        return false;
    }
    let stack = scan(src, quote).stack;
    matches!(stack.last(), Some(&(b'(', open)) if word_before(src[..open].trim_end()) == "import")
}

pub(super) fn items(cx: &mut Cx<'_>, from: usize) -> Vec<Item> {
    let typed = cx.src.get(from..cx.byte).unwrap_or("").to_owned();
    let replace = column_of(cx.src, from);
    let imported: HashSet<String> = crate::go::parse::parse_file(cx.path, cx.src)
        .imports
        .into_iter()
        .map(|i| i.path)
        .collect();
    let mut all: Vec<String> = cx.engine.std_package_paths().to_vec();
    all.extend(cx.engine.module_package_paths().iter().cloned());
    let mut found: Vec<(u8, usize, String)> = Vec::new();
    let take = |paths: &[String], found: &mut Vec<(u8, usize, String)>| {
        for p in paths {
            if imported.contains(p) || p.split('/').any(|s| s == "internal" || s == "vendor") {
                continue;
            }
            if let Some(rank) = match_rank(p, &typed) {
                found.push((rank, p.len(), p.clone()));
            }
        }
    };
    take(&all, &mut found);
    if let Some(project) = cx.engine.project() {
        for (module, _) in project.required_modules() {
            if module.to_lowercase().starts_with(&typed.to_lowercase())
                || typed.to_lowercase().starts_with(&module.to_lowercase())
            {
                take(&project.dependency_packages(&module), &mut found);
            }
        }
    }
    found.sort();
    found.dedup_by(|a, b| a.2 == b.2);
    found
        .into_iter()
        .take(MAX_PATHS)
        .map(|(_, _, path)| {
            let mut item = Item::new(&path, ItemKind::Class, 0);
            item.replace_from = Some(replace);
            item
        })
        .collect()
}
