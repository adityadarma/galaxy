//! Searching files for the uses of a [`GoRefQuery`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tree_sitter::Tree;

use super::{GoRefQuery, Key, Reach};
use crate::go::engine::GoEngine;
use crate::go::parse::{IdentAt, parse, utf16_col};
use crate::go::types::{Symbol, resolve_ids};
use crate::lang::Occurrence;

const MAX_FILES: usize = 5000;
const MAX_CANDIDATES: usize = 3000;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_DEPTH: usize = 40;

const IDENT_KINDS: &[&str] = &[
    "identifier",
    "field_identifier",
    "type_identifier",
    "package_identifier",
    "label_name",
];

// ---- finding uses -----------------------------------------------------------

impl GoRefQuery {
    pub(super) fn accepts(&self, symbol: &Symbol, file: &Path) -> bool {
        let related = |path: &Path, line: u32, column: u32| {
            self.related_methods
                .iter()
                .any(|(p, l, c)| p == path && *l == line && *c == column)
        };
        match (&self.key, symbol) {
            (
                Key::Decl { path, line, column },
                Symbol::Local {
                    path: p,
                    line: l,
                    column: c,
                    ..
                },
            ) => p == path && l == line && c == column,
            (
                Key::Decl { path, line, column },
                Symbol::Type { path: p, pos, .. }
                | Symbol::Func { path: p, pos, .. }
                | Symbol::Value { path: p, pos, .. }
                | Symbol::Field { path: p, pos, .. },
            ) => p == path && pos.line == *line && pos.column == *column,
            (Key::Decl { path, line, column }, Symbol::Method { path: p, pos, .. }) => {
                (p == path && pos.line == *line && pos.column == *column)
                    || related(p, pos.line, pos.column)
            }
            (
                Key::Import {
                    file: f,
                    import_path,
                },
                Symbol::Package { import_path: found },
            ) => f == file && found == import_path,
            (Key::TypeParam, Symbol::TypeParam { name }) => *name == self.name,
            _ => false,
        }
    }

    /// The directory of a package-level or member declaration.
    pub(super) fn dir(&self) -> Option<&Path> {
        match &self.reach {
            Reach::Package { dir, .. } | Reach::Member { dir, .. } => Some(dir),
            Reach::Within { .. } => None,
        }
    }
}

/// Identifiers of the span named `name`, with whether they could be a use
/// from another package (a selector, a qualified type or a literal key).
fn identifiers(src: &str, tree: &Tree, name: &str, from: usize, to: usize) -> Vec<(IdentAt, bool)> {
    let root = tree.root_node();
    let mut out = Vec::new();
    for (at, _) in src.match_indices(name) {
        let end = at + name.len();
        if at < from || end > to {
            continue;
        }
        let Some(node) = root.descendant_for_byte_range(at, end) else {
            continue;
        };
        if node.start_byte() != at || node.end_byte() != end || !IDENT_KINDS.contains(&node.kind())
        {
            continue;
        }
        let parent = node.parent().map(|p| p.kind());
        let outside = node.kind() == "field_identifier"
            || (node.kind() == "type_identifier" && parent == Some("qualified_type"))
            || parent == Some("literal_element");
        let line_start = src[..at].rfind('\n').map_or(0, |i| i + 1);
        let column = utf16_col(&src[line_start..], at - line_start);
        out.push((
            IdentAt {
                name: name.to_owned(),
                start_byte: at,
                end_byte: end,
                line: node.start_position().row as u32,
                column,
                end_column: column + name.encode_utf16().count() as u32,
            },
            outside,
        ));
    }
    out
}

pub(super) fn scan_file(
    engine: &mut GoEngine,
    query: &GoRefQuery,
    path: &Path,
    src: &str,
    tree: Option<&Tree>,
) -> Vec<Occurrence> {
    let (from, to) = match &query.reach {
        Reach::Within {
            path: own,
            from,
            to,
        } => {
            if own != path {
                return Vec::new();
            }
            (*from, (*to).min(src.len()))
        }
        _ => (0, src.len()),
    };
    let parsed;
    let tree = match tree {
        Some(tree) => tree,
        None => {
            let Some((_, tree)) = parse(path, src) else {
                return Vec::new();
            };
            parsed = tree;
            &parsed
        }
    };
    let in_dir = query.dir().is_some_and(|dir| path.parent() == Some(dir));
    let within = matches!(query.reach, Reach::Within { .. });
    let ids: Vec<IdentAt> = identifiers(src, tree, &query.name, from, to)
        .into_iter()
        .filter(|(_, outside)| within || in_dir || *outside)
        .map(|(id, _)| id)
        .collect();
    if ids.is_empty() {
        return Vec::new();
    }
    let resolved = resolve_ids(engine, path, src, tree, &ids);
    ids.into_iter()
        .zip(resolved)
        .filter(|(_, found)| {
            found
                .as_ref()
                .is_some_and(|r| query.accepts(&r.symbol, path))
        })
        .map(|(id, _)| Occurrence {
            path: path.to_path_buf(),
            line: id.line,
            start_column: id.column,
            end_column: id.end_column,
            dollar: false,
        })
        .collect()
}

/// Whether a file of the project can hold a use of the symbol.
fn wants(query: &GoRefQuery, path: &Path, text: &str) -> bool {
    if !text.contains(&query.name) {
        return false;
    }
    if query.dir().is_some_and(|dir| path.parent() == Some(dir)) {
        return true;
    }
    match &query.reach {
        Reach::Package {
            exported: true,
            import_path,
            ..
        } => {
            text.contains(&format!("\"{import_path}\""))
                || text.contains(&format!("`{import_path}`"))
        }
        Reach::Member { exported: true, .. } => true,
        _ => false,
    }
}

/// Go files of the project that can mention the symbol, with their text
/// (open buffers first, else disk). Skips `vendor`, `testdata`,
/// `node_modules`, hidden and `_` directories and files over 2 MB.
pub(super) fn candidates(
    engine: &GoEngine,
    query: &GoRefQuery,
    current: &Path,
) -> Vec<(PathBuf, String)> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let root = engine.root();
    if root.as_os_str().is_empty() {
        if let Some(dir) = current.parent() {
            roots.push(dir.to_path_buf());
        }
    } else {
        roots.push(root.to_path_buf());
        if let Some(project) = engine.project() {
            for module in project.modules() {
                if !module.dir.starts_with(root) {
                    roots.push(module.dir.clone());
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut seen = 0usize;
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut stack: Vec<(PathBuf, usize)> = roots.into_iter().rev().map(|r| (r, 0)).collect();
    while let Some((dir, depth)) = stack.pop() {
        if !visited.insert(dir.clone()) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut items: Vec<(String, PathBuf, bool)> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let is_dir = entry.file_type().ok()?.is_dir();
                Some((name, entry.path(), is_dir))
            })
            .collect();
        items.sort();
        let mut subdirs = Vec::new();
        for (name, full, is_dir) in items {
            if is_dir {
                let skip = name.starts_with(['.', '_'])
                    || matches!(name.as_str(), "vendor" | "testdata" | "node_modules");
                if !skip && depth < MAX_DEPTH {
                    subdirs.push((full, depth + 1));
                }
                continue;
            }
            if !name.ends_with(".go") || full == current {
                continue;
            }
            seen += 1;
            if seen > MAX_FILES || out.len() >= MAX_CANDIDATES {
                return out;
            }
            let text = match engine.open_text(&full) {
                Some(text) => text.to_string(),
                None => {
                    if !std::fs::metadata(&full).is_ok_and(|m| m.len() <= MAX_FILE_BYTES) {
                        continue;
                    }
                    match std::fs::read_to_string(&full) {
                        Ok(text) => text,
                        Err(_) => continue,
                    }
                }
            };
            if text.len() as u64 <= MAX_FILE_BYTES && wants(query, &full, &text) {
                out.push((full, text));
            }
        }
        stack.extend(subdirs.into_iter().rev());
    }
    // Buffers that are not under the walked directories.
    for open in engine.open_paths() {
        let is_go = open.extension().is_some_and(|e| e == "go");
        if !is_go || open == current || out.iter().any(|(p, _)| *p == open) {
            continue;
        }
        if let Some(text) = engine.open_text(&open)
            && wants(query, &open, &text)
            && out.len() < MAX_CANDIDATES
        {
            out.push((open, text.to_string()));
        }
    }
    out
}
