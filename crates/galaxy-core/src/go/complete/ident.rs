//! Identifiers at an expression, statement or type position.

use std::collections::HashSet;

use super::member::package_items;
use super::{Cx, Item};
use crate::go::actions::import_insertions;
use crate::go::diag::default_name;
use crate::go::scope::{BindingKind, Origin, scope_at};
use crate::go::types::buffer_package;
use crate::lang::ItemKind;

/// Auto-import candidates offered at most.
const MAX_AUTO_IMPORTS: usize = 20;

const UNIVERSE_TYPES: &[&str] = &[
    "bool",
    "string",
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "uintptr",
    "float32",
    "float64",
    "complex64",
    "complex128",
    "byte",
    "rune",
    "error",
    "any",
    "comparable",
];
const UNIVERSE_FUNCS: &[&str] = &[
    "len", "cap", "make", "new", "append", "copy", "delete", "panic", "recover", "print",
    "println", "close", "min", "max", "clear", "complex", "real", "imag",
];
const UNIVERSE_CONSTS: &[&str] = &["true", "false", "nil", "iota"];
const KEYWORDS: &[&str] = &[
    "func",
    "if",
    "for",
    "switch",
    "select",
    "go",
    "defer",
    "return",
    "var",
    "const",
    "type",
    "struct",
    "interface",
    "map",
    "chan",
    "range",
    "break",
    "continue",
    "fallthrough",
    "goto",
    "else",
    "case",
    "default",
    "package",
    "import",
];
const TYPE_KEYWORDS: &[&str] = &["struct", "interface", "map", "chan", "func"];

fn locals(cx: &Cx<'_>, types: bool) -> Vec<Item> {
    let scope = scope_at(cx.tree, cx.src, cx.byte);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for b in scope.bindings.iter().rev() {
        if b.kind == BindingKind::Label || !seen.insert(b.name.clone()) {
            continue;
        }
        if types != (b.kind == BindingKind::TypeParam) && types {
            continue;
        }
        let kind = match b.kind {
            BindingKind::Const => ItemKind::Constant,
            BindingKind::TypeParam => ItemKind::Class,
            _ => ItemKind::Variable,
        };
        let detail = match &b.origin {
            Origin::Declared(t) => t.clone(),
            Origin::Expr { text, count: 1, .. } if text.len() <= 40 && !text.contains('\n') => {
                text.clone()
            }
            _ => String::new(),
        };
        out.push(Item::new(&b.name, kind, 0).detail(detail));
    }
    out
}

fn universe(types: bool) -> Vec<Item> {
    let mut out: Vec<Item> = UNIVERSE_TYPES
        .iter()
        .map(|n| {
            let kind = if matches!(*n, "error" | "any" | "comparable") {
                ItemKind::Interface
            } else {
                ItemKind::Class
            };
            Item::new(n, kind, 3)
        })
        .collect();
    if !types {
        out.extend(UNIVERSE_FUNCS.iter().map(|n| {
            Item::new(n, ItemKind::Function, 3).call(!matches!(*n, "recover" | "print" | "println"))
        }));
        out.extend(
            UNIVERSE_CONSTS
                .iter()
                .map(|n| Item::new(n, ItemKind::Constant, 3)),
        );
    }
    out
}

fn is_internal(path: &str) -> bool {
    path.split('/').any(|p| p == "internal" || p == "vendor")
}

/// Packages not imported yet whose name starts with `prefix`: standard
/// library first, then the workspace modules, then dependencies, shortest
/// path first within each.
fn auto_imports(
    cx: &mut Cx<'_>,
    prefix: &str,
    taken: &HashSet<String>,
    skip: &HashSet<String>,
) -> Vec<Item> {
    let want = prefix.to_lowercase();
    let matching = |paths: &[String]| -> Vec<String> {
        let mut found: Vec<String> = paths
            .iter()
            .filter(|p| !skip.contains(p.as_str()) && !is_internal(p))
            .filter(|p| {
                let name = default_name(p);
                !taken.contains(&name) && name.to_lowercase().starts_with(&want)
            })
            .cloned()
            .collect();
        found.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
        found
    };
    let mut paths = matching(cx.engine.std_package_paths());
    paths.extend(matching(cx.engine.module_package_paths()));
    if paths.len() < MAX_AUTO_IMPORTS
        && let Some(project) = cx.engine.project()
    {
        let mut deps = Vec::new();
        for (module, _) in project.required_modules() {
            deps.extend(matching(&project.dependency_packages(&module)));
        }
        deps.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
        paths.extend(deps);
    }
    let mut seen = HashSet::new();
    paths.retain(|p| seen.insert(p.clone()));
    paths.truncate(MAX_AUTO_IMPORTS);
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let edits = import_insertions(cx.src, cx.tree, &refs);
    paths
        .iter()
        .zip(edits)
        .enumerate()
        .filter_map(|(index, (path, extra))| {
            let name = default_name(path);
            let mut item =
                Item::new(&name, ItemKind::Class, 5).detail(format!("import \"{path}\""));
            item.key = format!("{name}\0{index:03}{path}");
            item.extra = extra?;
            Some(item)
        })
        .collect()
}

pub(super) fn items(cx: &mut Cx<'_>, types: bool) -> Vec<Item> {
    let prefix = cx.prefix().to_owned();
    let mut out = locals(cx, types);
    let mut taken: HashSet<String> = out.iter().map(|i| i.label.clone()).collect();
    let mut skip = HashSet::new();
    if let Some((pkg, file)) = buffer_package(cx.engine, cx.path, cx.src, cx.tree) {
        let decls = package_items(&pkg, 1, false, types);
        taken.extend(decls.iter().map(|i| i.label.clone()));
        out.extend(decls);
        let mut names: Vec<(String, String)> = cx.engine.import_names(&file).into_iter().collect();
        names.sort();
        for (name, path) in names {
            taken.insert(name.clone());
            skip.insert(path.clone());
            out.push(Item::new(&name, ItemKind::Class, 2).detail(format!("package {path}")));
        }
        skip.insert(pkg.import_path.clone());
        // Names that stay hidden for auto-import even when not shown.
        for import in &file.imports {
            skip.insert(import.path.clone());
        }
    }
    out.extend(universe(types));
    let keywords = if types { TYPE_KEYWORDS } else { KEYWORDS };
    out.extend(keywords.iter().map(|k| Item::new(k, ItemKind::Keyword, 4)));
    if prefix.chars().count() >= 2 {
        let pkg_names: HashSet<String> = taken;
        out.extend(auto_imports(cx, &prefix, &pkg_names, &skip));
    }
    out
}
