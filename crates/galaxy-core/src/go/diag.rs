//! Conservative diagnostics for one Go file, from the syntax tree and the
//! package index. No type checking: a wrong squiggle is worse than a missing
//! one, so every check gives up when it cannot be sure.
//!
//! - syntax errors (`ERROR` and `MISSING` nodes);
//! - imports that are never used as `name.` and std-like import paths that
//!   do not exist;
//! - local `:=`/`var` names mentioned nowhere else in their function;
//! - top-level names declared twice in the file.
//!
//! Missing returns, type mismatches and undefined names need the full type
//! checker and are not reported.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use tree_sitter::{Node, Point, Tree};

use crate::lang::{Problem, Severity};

use super::engine::GoEngine;
use super::parse::{GoFile, Import, Pos, parse, utf16_col};

#[cfg(test)]
pub(super) mod fixture;
#[cfg(test)]
mod tests;

const MAX_PROBLEMS: usize = 200;
/// More error nodes than this give one summary instead of a list.
const MAX_ERROR_NODES: usize = 50;
/// Error nodes collected before the search stops.
const ERROR_SEARCH_CAP: usize = 1000;
const MAX_DEPTH: usize = 300;

/// Line and UTF-16 column of a point of `src`, which lies at `byte`.
pub(super) fn point(src: &str, byte: usize, at: Point) -> (u32, u32) {
    let line_start = byte.saturating_sub(at.column);
    (
        at.row as u32,
        utf16_col(src.get(line_start..).unwrap_or(""), at.column),
    )
}

fn problem(
    (start_line, start_column): (u32, u32),
    (end_line, end_column): (u32, u32),
    severity: Severity,
    message: String,
    unnecessary: bool,
) -> Problem {
    Problem {
        start_line,
        start_column,
        end_line,
        end_column,
        severity,
        message,
        unnecessary,
    }
}

/// Problems of `src`, sorted by position, at most 200.
pub fn diagnostics(engine: &mut GoEngine, path: &Path, src: &str) -> Vec<Problem> {
    let Some((file, tree)) = parse(path, src) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    syntax_errors(&tree, src, &mut out);
    for import in unused_imports(engine, &file, &tree, src) {
        out.push(problem(
            (import.line, import.column),
            (import.line, import.end_column),
            Severity::Error,
            unused_message(&import),
            true,
        ));
    }
    missing_std_imports(engine, &file, &mut out);
    unused_locals(&tree, src, &mut out);
    duplicates(&file, &tree, &mut out);
    out.sort_by_key(|p| (p.start_line, p.start_column));
    out.truncate(MAX_PROBLEMS);
    out
}

fn unused_message(import: &Import) -> String {
    match import.alias.as_deref() {
        Some(alias) if !alias.is_empty() => {
            format!("\"{}\" imported as {alias} and not used", import.path)
        }
        _ => format!("\"{}\" imported and not used", import.path),
    }
}

// ---- syntax errors ----------------------------------------------------

fn collect_errors<'t>(node: Node<'t>, out: &mut Vec<Node<'t>>, depth: usize) {
    if depth > MAX_DEPTH || out.len() >= ERROR_SEARCH_CAP {
        return;
    }
    if node.is_missing() {
        out.push(node);
        return;
    }
    if !node.has_error() {
        return;
    }
    let before = out.len();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_errors(child, out, depth + 1);
    }
    if node.is_error() && out.len() == before {
        out.push(node);
    }
}

/// The range to underline for an error node: its first line, and at least
/// one character.
fn error_range(src: &str, node: Node<'_>) -> ((u32, u32), (u32, u32)) {
    let start = node.start_position();
    let line_start = node.start_byte().saturating_sub(start.column);
    let rest = src.get(line_start..).unwrap_or("");
    let line = rest.split('\n').next().unwrap_or("");
    let line = line.strip_suffix('\r').unwrap_or(line);
    let mut from = start.column.min(line.len());
    while !line.is_char_boundary(from) {
        from -= 1;
    }
    let single_line = node.end_position().row == start.row;
    let mut to = if single_line {
        node.end_position().column.clamp(from, line.len())
    } else {
        line.len()
    };
    while !line.is_char_boundary(to) {
        to -= 1;
    }
    if to == from {
        if let Some(c) = line[from..].chars().next() {
            to = from + c.len_utf8();
        } else if let Some((i, _)) = line[..from].char_indices().next_back() {
            from = i;
        }
    }
    let row = start.row as u32;
    ((row, utf16_col(line, from)), (row, utf16_col(line, to)))
}

fn syntax_errors(tree: &Tree, src: &str, out: &mut Vec<Problem>) {
    let root = tree.root_node();
    if !root.has_error() {
        return;
    }
    let mut found = Vec::new();
    collect_errors(root, &mut found, 0);
    if found.len() > MAX_ERROR_NODES {
        let (start, end) = error_range(src, found[0]);
        out.push(problem(
            start,
            end,
            Severity::Error,
            format!("syntax error (more than {MAX_ERROR_NODES} errors in file)"),
            false,
        ));
        return;
    }
    let mut lines = HashSet::new();
    for node in found {
        let (start, end) = error_range(src, node);
        if !lines.insert(start.0) {
            continue;
        }
        let message = if node.is_missing() {
            format!("missing {}", node.kind())
        } else {
            "syntax error".to_owned()
        };
        out.push(problem(start, end, Severity::Error, message, false));
    }
}

// ---- imports ----------------------------------------------------------

/// The package name an import path suggests: its last element, or the one
/// before a `/vN` suffix, without a `.vN` suffix (`gopkg.in/yaml.v3`).
pub(super) fn default_name(path: &str) -> String {
    let is_major = |s: &str| {
        s.strip_prefix('v')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    let mut parts = path.rsplit('/');
    let last = parts.next().unwrap_or("");
    let element = match parts.next() {
        Some(before) if is_major(last) && !before.is_empty() => before,
        _ => last,
    };
    match element.rsplit_once('.') {
        Some((head, tail)) if !head.is_empty() && is_major(tail) => head.to_owned(),
        _ => element.to_owned(),
    }
}

/// Local names a package may have, used to accept an import as used without
/// reading the package.
fn name_guesses(path: &str) -> Vec<String> {
    let base = default_name(path);
    let trimmed = base.trim_start_matches("go-").trim_end_matches("-go");
    vec![
        base.replace('-', "_"),
        trimmed.replace('-', ""),
        trimmed.to_owned(),
        base,
    ]
}

/// Calls `f` on `root` and every node below it, in source order.
fn for_each_node<'t>(root: Node<'t>, mut f: impl FnMut(Node<'t>)) {
    let mut cursor = root.walk();
    loop {
        f(cursor.node());
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

/// Every identifier that is directly followed by `.`: a package qualifier
/// (`http.Get`, `http.Handler`) or, conservatively, a variable.
pub(super) fn qualifiers(tree: &Tree, src: &str) -> HashSet<String> {
    let bytes = src.as_bytes();
    let mut set = HashSet::new();
    for_each_node(tree.root_node(), |node| {
        if node.child_count() != 0
            || !matches!(
                node.kind(),
                "identifier" | "package_identifier" | "type_identifier"
            )
        {
            return;
        }
        let mut i = node.end_byte();
        while matches!(bytes.get(i), Some(b' ' | b'\t')) {
            i += 1;
        }
        if bytes.get(i) == Some(&b'.')
            && let Some(text) = src.get(node.byte_range())
        {
            set.insert(text.to_owned());
        }
    });
    set
}

/// Imports whose package resolves and whose name is never used as a
/// qualifier. `_` and `.` imports, `C`, unresolvable packages and files with
/// syntax errors give nothing.
pub(super) fn unused_imports(
    engine: &mut GoEngine,
    file: &GoFile,
    tree: &Tree,
    src: &str,
) -> Vec<Import> {
    if file.imports.is_empty() || tree.root_node().has_error() {
        return Vec::new();
    }
    let used = qualifiers(tree, src);
    let mut out = Vec::new();
    for import in &file.imports {
        let alias = import.alias.as_deref().filter(|a| !a.is_empty());
        if matches!(alias, Some("_" | ".")) || import.path == "C" {
            continue;
        }
        match alias {
            Some(alias) if used.contains(alias) => continue,
            None if name_guesses(&import.path).iter().any(|g| used.contains(g)) => continue,
            _ => {}
        }
        let Some(pkg) = engine.package_by_import(&import.path) else {
            continue;
        };
        if alias.is_none() && (pkg.name.is_empty() || used.contains(&pkg.name)) {
            continue;
        }
        out.push(import.clone());
    }
    out
}

fn module_owns(engine: &GoEngine, path: &str) -> bool {
    let Some(project) = engine.project() else {
        return false;
    };
    let owns = |module: &str| {
        path == module
            || path
                .strip_prefix(module)
                .is_some_and(|rest| rest.starts_with('/'))
    };
    project.modules().iter().any(|m| owns(&m.path))
        || project.required_modules().iter().any(|(m, _)| owns(m))
}

fn missing_std_imports(engine: &GoEngine, file: &GoFile, out: &mut Vec<Problem>) {
    let Some(project) = engine.project() else {
        return;
    };
    let Some(goroot) = project.goroot() else {
        return;
    };
    if project.modules().is_empty() || !goroot.join("src").is_dir() {
        return;
    }
    for import in &file.imports {
        let path = import.path.as_str();
        let plain = path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-'));
        let clean = !path.is_empty() && path.split('/').all(|part| !part.is_empty());
        if !plain || !clean || matches!(path, "C" | "unsafe") {
            continue;
        }
        if project.import_dir(path).is_some() || module_owns(engine, path) {
            continue;
        }
        out.push(problem(
            (import.line, import.path_column),
            (import.line, import.path_end_column),
            Severity::Error,
            format!("package {path} is not in std"),
            false,
        ));
    }
}

// ---- locals -----------------------------------------------------------

fn find_units<'t>(node: Node<'t>, out: &mut Vec<Node<'t>>, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    if matches!(
        node.kind(),
        "function_declaration" | "method_declaration" | "func_literal"
    ) {
        out.push(node);
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        find_units(child, out, depth + 1);
    }
}

fn push_identifiers<'t>(list: Option<Node<'t>>, out: &mut Vec<Node<'t>>) {
    let Some(list) = list else { return };
    if list.kind() == "identifier" {
        out.push(list);
        return;
    }
    let mut cursor = list.walk();
    for child in list.named_children(&mut cursor) {
        if child.kind() == "identifier" {
            out.push(child);
        }
    }
}

/// Reports names declared by `:=` or `var` (and unused type switch guards)
/// that occur once in their function: any other mention counts as a use.
fn check_unit(unit: Node<'_>, src: &str, out: &mut Vec<Problem>) {
    if unit.has_error() || unit.child_by_field_name("body").is_none() {
        return;
    }
    let mut counts: HashMap<&str, u32> = HashMap::new();
    let mut decls: Vec<Node<'_>> = Vec::new();
    for_each_node(unit, |node| match node.kind() {
        "identifier" | "type_identifier" | "package_identifier" if node.child_count() == 0 => {
            if let Some(text) = src.get(node.byte_range()) {
                *counts.entry(text).or_insert(0) += 1;
            }
        }
        "short_var_declaration" => {
            push_identifiers(node.child_by_field_name("left"), &mut decls);
        }
        "type_switch_statement" => {
            push_identifiers(node.child_by_field_name("alias"), &mut decls);
        }
        "var_spec" => {
            let mut names = node.walk();
            for name in node.children_by_field_name("name", &mut names) {
                push_identifiers(Some(name), &mut decls);
            }
        }
        _ => {}
    });
    for ident in decls {
        let Some(name) = src.get(ident.byte_range()) else {
            continue;
        };
        if name == "_" || counts.get(name).copied().unwrap_or(0) != 1 {
            continue;
        }
        out.push(problem(
            point(src, ident.start_byte(), ident.start_position()),
            point(src, ident.end_byte(), ident.end_position()),
            Severity::Error,
            format!("declared and not used: {name}"),
            true,
        ));
    }
}

fn unused_locals(tree: &Tree, src: &str, out: &mut Vec<Problem>) {
    let mut units = Vec::new();
    find_units(tree.root_node(), &mut units, 0);
    for unit in units {
        check_unit(unit, src, out);
    }
}

// ---- duplicates -------------------------------------------------------

struct Decl {
    key: String,
    message: String,
    line: u32,
    column: u32,
    end_column: u32,
}

fn duplicates(file: &GoFile, tree: &Tree, out: &mut Vec<Problem>) {
    if tree.root_node().has_error() {
        return;
    }
    let mut decls: Vec<Decl> = Vec::new();
    let mut add = |name: &str, key: String, message: String, pos: &Pos| {
        if name != "_" && name != "init" {
            decls.push(Decl {
                key,
                message,
                line: pos.line,
                column: pos.column,
                end_column: pos.end_column,
            });
        }
    };
    for ty in &file.types {
        add(&ty.name, ty.name.clone(), redeclared(&ty.name), &ty.pos);
    }
    for value in file.consts.iter().chain(file.vars.iter()) {
        add(
            &value.name,
            value.name.clone(),
            redeclared(&value.name),
            &value.pos,
        );
    }
    for func in &file.funcs {
        match &func.receiver {
            None => add(
                &func.name,
                func.name.clone(),
                redeclared(&func.name),
                &func.pos,
            ),
            Some(recv) => {
                let name = format!("{}.{}", recv.base, func.name);
                add(
                    &func.name,
                    format!("method {name}"),
                    format!("method {name} already declared"),
                    &func.pos,
                );
            }
        }
    }
    decls.sort_by_key(|d| (d.line, d.column));
    let mut seen = HashSet::new();
    for d in decls {
        if !seen.insert(d.key) {
            out.push(problem(
                (d.line, d.column),
                (d.line, d.end_column),
                Severity::Error,
                d.message,
                false,
            ));
        }
    }
}

fn redeclared(name: &str) -> String {
    format!("{name} redeclared in this block")
}
