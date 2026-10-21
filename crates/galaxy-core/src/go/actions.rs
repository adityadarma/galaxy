//! Quick fixes for Go imports: add the import a bare `pkg.` qualifier needs,
//! and remove imports that nothing uses.

use std::collections::BTreeSet;
use std::path::Path;

use tree_sitter::{Node, Tree};

use crate::lang::{Action, Edit, Insertion};

use super::diag::{default_name, point, unused_imports};
use super::engine::GoEngine;
use super::engine::Package;
use super::parse::{GoFile, identifier_at, parse, utf16_col};
use super::scope::scope_at;

#[cfg(test)]
mod tests;

/// Add-import actions offered at one position.
const MAX_IMPORT_ACTIONS: usize = 8;

struct Spec {
    path: String,
    /// Source text of the spec, alias included.
    text: String,
    /// First row, leading own-line comments included.
    first_row: usize,
    end_row: usize,
    /// Nothing but whitespace (or a trailing line comment) shares its lines.
    own_line: bool,
}

struct ImportDecl {
    start_byte: usize,
    end_byte: usize,
    start: tree_sitter::Point,
    end: tree_sitter::Point,
    list: bool,
    open_row: usize,
    close_row: usize,
    own_line: bool,
    specs: Vec<Spec>,
}

fn is_std(path: &str) -> bool {
    !path.split('/').next().unwrap_or("").contains('.')
}

fn blank(lines: &[&str], row: usize) -> bool {
    lines.get(row).is_some_and(|l| l.trim().is_empty())
}

fn ins(row: usize, column: u32, text: String) -> Edit {
    Edit {
        start_line: row as u32,
        start_column: column,
        end_line: row as u32,
        end_column: column,
        text,
    }
}

/// Whether only whitespace precedes `node` on its first line and only
/// whitespace or a line comment follows it on its last.
fn alone(lines: &[&str], node: Node<'_>) -> bool {
    let (start, end) = (node.start_position(), node.end_position());
    let before = lines.get(start.row).and_then(|l| l.get(..start.column));
    let after = lines.get(end.row).and_then(|l| l.get(end.column..));
    match (before, after) {
        (Some(before), Some(after)) => {
            let after = after.trim();
            before.trim().is_empty() && (after.is_empty() || after.starts_with("//"))
        }
        _ => false,
    }
}

fn spec_of(src: &str, lines: &[&str], node: Node<'_>) -> Spec {
    let path = node
        .child_by_field_name("path")
        .and_then(|p| src.get(p.byte_range()))
        .map(|t| t.trim_matches(['"', '`']).to_owned())
        .unwrap_or_default();
    let mut first_row = node.start_position().row;
    let mut prev = node.prev_sibling();
    while let Some(comment) = prev {
        if comment.kind() != "comment"
            || comment.end_position().row + 1 != first_row
            || !alone(lines, comment)
        {
            break;
        }
        first_row = comment.start_position().row;
        prev = comment.prev_sibling();
    }
    Spec {
        path,
        text: src.get(node.byte_range()).unwrap_or("").to_owned(),
        first_row,
        end_row: node.end_position().row,
        own_line: alone(lines, node),
    }
}

fn read_decls(src: &str, lines: &[&str], root: Node<'_>) -> Vec<ImportDecl> {
    let mut out = Vec::new();
    let mut cursor = root.walk();
    for decl in root.children(&mut cursor) {
        if decl.kind() != "import_declaration" {
            continue;
        }
        let mut specs = Vec::new();
        let mut list = None;
        let mut inner = decl.walk();
        for child in decl.named_children(&mut inner) {
            match child.kind() {
                "import_spec" => specs.push(spec_of(src, lines, child)),
                "import_spec_list" => {
                    list = Some(child);
                    let mut c = child.walk();
                    for spec in child.named_children(&mut c) {
                        if spec.kind() == "import_spec" {
                            specs.push(spec_of(src, lines, spec));
                        }
                    }
                }
                _ => {}
            }
        }
        out.push(ImportDecl {
            start_byte: decl.start_byte(),
            end_byte: decl.end_byte(),
            start: decl.start_position(),
            end: decl.end_position(),
            list: list.is_some(),
            open_row: list.map_or(0, |l| l.start_position().row),
            close_row: list.map_or(0, |l| l.end_position().row),
            own_line: alone(lines, decl),
            specs,
        });
    }
    out
}

impl ImportDecl {
    /// Indexes of `specs` split at blank lines.
    fn groups(&self, lines: &[&str]) -> Vec<Vec<usize>> {
        let mut out: Vec<Vec<usize>> = Vec::new();
        for (i, spec) in self.specs.iter().enumerate() {
            let split = i > 0 && {
                let prev = &self.specs[i - 1];
                (prev.end_row + 1..spec.first_row).any(|r| blank(lines, r))
            };
            match out.last_mut() {
                Some(group) if !split => group.push(i),
                _ => out.push(vec![i]),
            }
        }
        out
    }
}

// ---- add import -------------------------------------------------------

/// The edit that imports `path`, or `None` when the layout is too unusual.
fn add_import(
    src: &str,
    lines: &[&str],
    root: Node<'_>,
    decls: &[ImportDecl],
    path: &str,
    nl: &str,
) -> Option<Edit> {
    let std = is_std(path);
    let item = format!("\t\"{path}\"{nl}");
    let lists: Vec<&ImportDecl> = decls.iter().filter(|d| d.list).collect();
    if !lists.is_empty() {
        let mut target: Option<(&ImportDecl, Vec<usize>)> = None;
        for decl in &lists {
            for group in decl.groups(lines) {
                if !group.iter().any(|&i| is_std(&decl.specs[i].path) == std) {
                    continue;
                }
                if !std || target.is_none() {
                    target = Some((decl, group));
                }
            }
        }
        if let Some((decl, group)) = target {
            let next = group
                .iter()
                .map(|&i| &decl.specs[i])
                .find(|s| s.path.as_str() > path);
            return match next {
                Some(spec) => spec.own_line.then(|| ins(spec.first_row, 0, item)),
                None => {
                    let last = &decl.specs[*group.last()?];
                    last.own_line.then(|| ins(last.end_row + 1, 0, item))
                }
            };
        }
        // No group of the right kind: start a new one.
        let decl = if std {
            lists[0]
        } else {
            lists[lists.len() - 1]
        };
        let (Some(first), Some(last)) = (decl.specs.first(), decl.specs.last()) else {
            let closing = lines.get(decl.close_row)?.trim() == ")";
            return (decl.close_row > decl.open_row && closing)
                .then(|| ins(decl.close_row, 0, item));
        };
        return if std {
            first
                .own_line
                .then(|| ins(first.first_row, 0, format!("{item}{nl}")))
        } else {
            last.own_line
                .then(|| ins(last.end_row + 1, 0, format!("{nl}{item}")))
        };
    }
    let singles: Vec<&ImportDecl> = decls.iter().filter(|d| d.specs.len() == 1).collect();
    let single = if std { singles.first() } else { singles.last() };
    if let Some(decl) = single {
        let old = &decl.specs[0];
        let same = is_std(&old.path) == std;
        let mut items = [
            (old.path.as_str(), old.text.clone()),
            (path, format!("\"{path}\"")),
        ];
        if same {
            items.sort_by(|a, b| a.0.cmp(b.0));
        } else {
            items.sort_by_key(|(p, _)| !is_std(p));
        }
        let gap = if same { "" } else { nl };
        let text = format!("import ({nl}\t{}{nl}{gap}\t{}{nl})", items[0].1, items[1].1);
        let (start_line, start_column) = point(src, decl.start_byte, decl.start);
        let (end_line, end_column) = point(src, decl.end_byte, decl.end);
        return Some(Edit {
            start_line,
            start_column,
            end_line,
            end_column,
            text,
        });
    }
    let mut cursor = root.walk();
    let clause = root
        .children(&mut cursor)
        .find(|n| n.kind() == "package_clause")?;
    let row = clause.end_position().row;
    let line = lines.get(row)?.trim_end_matches('\r');
    Some(ins(
        row,
        utf16_col(line, line.len()),
        format!("{nl}{nl}import \"{path}\""),
    ))
}

/// Insertions that import each of `imports` (one entry per path, `None`
/// when the layout of the imports is too unusual). Used by completion for
/// auto-import. A single `import "x"` that would be rewritten into a block
/// gets a second `import` declaration after it instead, since insertions
/// cannot replace text.
pub(super) fn import_insertions(
    src: &str,
    tree: &Tree,
    imports: &[&str],
) -> Vec<Option<Vec<Insertion>>> {
    let lines: Vec<&str> = src.split('\n').collect();
    let root = tree.root_node();
    let decls = read_decls(src, &lines, root);
    let nl = if src.contains("\r\n") { "\r\n" } else { "\n" };
    imports
        .iter()
        .map(|path| {
            let edit = add_import(src, &lines, root, &decls, path, nl)?;
            let empty = edit.start_line == edit.end_line && edit.start_column == edit.end_column;
            Some(vec![if empty {
                Insertion {
                    line: edit.start_line,
                    column: edit.start_column,
                    text: edit.text,
                }
            } else {
                Insertion {
                    line: edit.end_line,
                    column: edit.end_column,
                    text: format!("{nl}import \"{path}\""),
                }
            }])
        })
        .collect()
}

/// The operand identifier of the selector (or package of the qualified
/// type) `node` belongs to.
fn qualifier_of(node: Node<'_>) -> Option<Node<'_>> {
    let parent = node.parent()?;
    match parent.kind() {
        "selector_expression" => {
            let operand = parent.child_by_field_name("operand")?;
            (operand.kind() == "identifier").then_some(operand)
        }
        "qualified_type" => parent.child_by_field_name("package"),
        _ => None,
    }
}

fn declared_in_file(file: &GoFile, name: &str) -> bool {
    file.types.iter().any(|t| t.name == name)
        || file
            .funcs
            .iter()
            .any(|f| f.receiver.is_none() && f.name == name)
        || file
            .consts
            .iter()
            .chain(file.vars.iter())
            .any(|v| v.name == name)
}

/// Import paths of packages named `name`: standard library first, then the
/// module's own, then dependencies in the cache; shortest path first within
/// each, at most [`MAX_IMPORT_ACTIONS`].
fn candidates(engine: &GoEngine, name: &str, skip: &BTreeSet<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let push_group = |mut paths: Vec<String>, out: &mut Vec<String>| {
        paths.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
        for path in paths {
            if out.len() < MAX_IMPORT_ACTIONS && !out.contains(&path) {
                out.push(path);
            }
        }
    };
    let matching = |paths: &[String]| -> Vec<String> {
        paths
            .iter()
            .filter(|p| !skip.contains(p.as_str()) && default_name(p) == name)
            .cloned()
            .collect()
    };
    push_group(matching(engine.std_package_paths()), &mut out);
    push_group(matching(engine.module_package_paths()), &mut out);
    if out.len() < MAX_IMPORT_ACTIONS
        && let Some(project) = engine.project()
    {
        let mut deps = Vec::new();
        for (module, _) in project.required_modules() {
            deps.extend(matching(&project.dependency_packages(&module)));
        }
        push_group(deps, &mut out);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn import_actions(
    engine: &mut GoEngine,
    path: &Path,
    src: &str,
    tree: &Tree,
    file: &GoFile,
    lines: &[&str],
    decls: &[ImportDecl],
    (line, column): (u32, u32),
) -> Vec<Action> {
    let root = tree.root_node();
    let Some(ident) = identifier_at(tree, src, line, column) else {
        return Vec::new();
    };
    let Some(node) = root.descendant_for_byte_range(ident.start_byte, ident.end_byte) else {
        return Vec::new();
    };
    let Some(qualifier) = qualifier_of(node) else {
        return Vec::new();
    };
    let Some(name) = src.get(qualifier.byte_range()) else {
        return Vec::new();
    };
    if name == "_"
        || scope_at(tree, src, qualifier.start_byte())
            .get(name)
            .is_some()
    {
        return Vec::new();
    }
    if declared_in_file(file, name) {
        return Vec::new();
    }
    let package = engine.package_of_file(path);
    let fallback;
    let pkg: &Package = match &package {
        Some(pkg) => pkg,
        None => {
            fallback = Package {
                import_path: String::new(),
                name: file.package.clone(),
                dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
                files: Vec::new(),
            };
            &fallback
        }
    };
    if engine.lookup_in_file_scope(file, pkg, name).is_some() {
        return Vec::new();
    }
    let mut skip: BTreeSet<&str> = file.imports.iter().map(|i| i.path.as_str()).collect();
    if !pkg.import_path.is_empty() {
        skip.insert(pkg.import_path.as_str());
    }
    let nl = if src.contains("\r\n") { "\r\n" } else { "\n" };
    candidates(engine, name, &skip)
        .into_iter()
        .filter_map(|import| {
            let edit = add_import(src, lines, root, decls, &import, nl)?;
            Some(Action {
                title: format!("Import \"{import}\""),
                edits: vec![edit],
                files: Vec::new(),
            })
        })
        .collect()
}

// ---- remove imports ---------------------------------------------------

/// Edits deleting the specs in `remove` (declaration index, spec index)
/// line by line; a declaration left empty goes whole, as does a blank line
/// left between groups. `None` when a removed import shares its line.
fn removal_edits(
    decls: &[ImportDecl],
    lines: &[&str],
    remove: &BTreeSet<(usize, usize)>,
) -> Option<Vec<Edit>> {
    let mut rows: BTreeSet<usize> = BTreeSet::new();
    for (di, decl) in decls.iter().enumerate() {
        let gone: Vec<usize> = remove
            .iter()
            .filter(|(d, _)| *d == di)
            .map(|(_, s)| *s)
            .collect();
        if gone.is_empty() {
            continue;
        }
        if gone.len() == decl.specs.len() {
            if !decl.own_line {
                return None;
            }
            rows.extend(decl.start.row..=decl.end.row);
            // Do not leave two blank lines behind.
            if blank(lines, decl.end.row + 1)
                && (decl.start.row == 0 || blank(lines, decl.start.row - 1))
            {
                rows.insert(decl.end.row + 1);
            }
            continue;
        }
        for &i in &gone {
            let spec = &decl.specs[i];
            if !spec.own_line {
                return None;
            }
            rows.extend(spec.first_row..=spec.end_row);
        }
        let groups = decl.groups(lines);
        for (gi, group) in groups.iter().enumerate() {
            if !group.iter().all(|i| gone.contains(i)) {
                continue;
            }
            let first = &decl.specs[group[0]];
            let last = &decl.specs[group[group.len() - 1]];
            if gi + 1 < groups.len() && blank(lines, last.end_row + 1) {
                rows.insert(last.end_row + 1);
            } else if gi > 0 && first.first_row > 0 && blank(lines, first.first_row - 1) {
                rows.insert(first.first_row - 1);
            }
        }
    }
    let mut edits: Vec<Edit> = Vec::new();
    let mut run: Option<(usize, usize)> = None;
    let flush = |run: Option<(usize, usize)>, edits: &mut Vec<Edit>| {
        if let Some((from, to)) = run {
            edits.push(Edit {
                start_line: from as u32,
                start_column: 0,
                end_line: to as u32 + 1,
                end_column: 0,
                text: String::new(),
            });
        }
    };
    for row in rows {
        run = match run {
            Some((from, to)) if to + 1 == row => Some((from, row)),
            other => {
                flush(other, &mut edits);
                Some((row, row))
            }
        };
    }
    flush(run, &mut edits);
    Some(edits)
}

fn remove_actions(
    engine: &mut GoEngine,
    src: &str,
    tree: &Tree,
    file: &GoFile,
    lines: &[&str],
    decls: &[ImportDecl],
    cursor_line: u32,
) -> Vec<Action> {
    let unused = unused_imports(engine, file, tree, src);
    let mut found: Vec<((usize, usize), String)> = Vec::new();
    for import in &unused {
        let row = import.line as usize;
        for (di, decl) in decls.iter().enumerate() {
            if let Some(si) = decl
                .specs
                .iter()
                .position(|s| s.path == import.path && s.first_row <= row && row <= s.end_row)
            {
                found.push(((di, si), import.path.clone()));
                break;
            }
        }
    }
    let mut out = Vec::new();
    let on_cursor = |(di, si): (usize, usize)| {
        let spec = &decls[di].specs[si];
        spec.first_row <= cursor_line as usize && cursor_line as usize <= spec.end_row
    };
    let mut single_at_cursor = false;
    for (key, path) in &found {
        if !on_cursor(*key) {
            continue;
        }
        single_at_cursor = true;
        if let Some(edits) = removal_edits(decls, lines, &BTreeSet::from([*key])) {
            out.push(Action {
                title: format!("Remove unused import \"{path}\""),
                edits,
                files: Vec::new(),
            });
        }
    }
    let all: BTreeSet<(usize, usize)> = found.iter().map(|(k, _)| *k).collect();
    if (all.len() > 1 || (all.len() == 1 && !single_at_cursor))
        && let Some(edits) = removal_edits(decls, lines, &all)
    {
        out.push(Action {
            title: "Remove unused imports".to_owned(),
            edits,
            files: Vec::new(),
        });
    }
    out
}

/// Quick fixes at `line`/`column` (0-based, UTF-16): import candidates for
/// an unresolved `pkg.` qualifier under the cursor, and removal of unused
/// imports on the cursor line or all at once.
pub fn actions(
    engine: &mut GoEngine,
    path: &Path,
    src: &str,
    line: u32,
    column: u32,
) -> Vec<Action> {
    let Some((file, tree)) = parse(path, src) else {
        return Vec::new();
    };
    let lines: Vec<&str> = src.split('\n').collect();
    let decls = read_decls(src, &lines, tree.root_node());
    let mut out = import_actions(
        engine,
        path,
        src,
        &tree,
        &file,
        &lines,
        &decls,
        (line, column),
    );
    out.extend(remove_actions(
        engine, src, &tree, &file, &lines, &decls, line,
    ));
    out
}
