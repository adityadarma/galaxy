//! Find usages and rename for Go.
//!
//! The symbol under the cursor is resolved once with [`resolve_at`]; its
//! declaration (file and position) is its identity. Every identifier of the
//! same spelling in the files that could mention it is then resolved the
//! same way, and kept when it ends at the same declaration. So two methods
//! of one name on different types, or a local that shadows a package-level
//! name, are told apart by the type checker-lite of [`crate::go::types`].
//!
//! Limits: names brought in by a dot import are not followed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tree_sitter::{Node, Tree};

use crate::go::engine::GoEngine;
use crate::go::engine::MemberKind;
use crate::go::parse::{FuncDecl, TypeKind, exported, identifier_at, parse_file};
use crate::go::texpr::TypeExpr;
use crate::go::types::{Symbol, resolve_at};
use crate::lang::Occurrence;
use scan::{candidates, scan_file};

mod scan;

const MAX_OCCURRENCES: usize = 2000;

const KEYWORDS: &[&str] = &[
    "break",
    "default",
    "func",
    "interface",
    "select",
    "case",
    "defer",
    "go",
    "map",
    "struct",
    "chan",
    "else",
    "goto",
    "package",
    "switch",
    "const",
    "fallthrough",
    "if",
    "range",
    "type",
    "continue",
    "for",
    "import",
    "return",
    "var",
];

/// What an identifier has to resolve to.
#[derive(Debug, Clone)]
enum Key {
    /// A declaration, by file and position of its name.
    Decl {
        path: PathBuf,
        line: u32,
        column: u32,
    },
    /// An import name of one file.
    Import {
        file: PathBuf,
        import_path: String,
    },
    TypeParam,
}

/// Where uses can be.
#[derive(Debug, Clone)]
enum Reach {
    /// One byte span of one file.
    Within {
        path: PathBuf,
        from: usize,
        to: usize,
    },
    /// A package-level name: its directory, plus qualified uses elsewhere.
    Package {
        dir: PathBuf,
        import_path: String,
        exported: bool,
    },
    /// A field or method: selectors anywhere when exported.
    Member { dir: PathBuf, exported: bool },
}

/// A symbol to look for, built by [`reference_query`].
#[derive(Debug, Clone)]
pub struct GoRefQuery {
    key: Key,
    /// Interface and concrete method declarations connected to `key`.
    related_methods: Vec<(PathBuf, u32, u32)>,
    reach: Reach,
    /// Declared in GOROOT, the module cache or `vendor/`: found, but not
    /// ours to rename.
    pub external: bool,
    /// The plain name searched for in sources.
    pub name: String,
    /// An import without an alias: its name comes from the package.
    unaliased: bool,
}

/// Whether `name` is a Go identifier that is not a keyword and not `_`.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
        && name != "_"
        && !KEYWORDS.contains(&name)
}

/// Whether the symbol of `query` can be renamed to `new_name`.
pub fn can_rename(query: &GoRefQuery, new_name: &str) -> Result<(), String> {
    if query.external {
        return Err("cannot rename external symbol".to_owned());
    }
    if !valid_name(new_name) {
        return Err("invalid name".to_owned());
    }
    if matches!(query.name.as_str(), "init" | "main") || matches!(new_name, "init" | "main") {
        return Err("cannot rename init/main".to_owned());
    }
    if query.unaliased {
        return Err("cannot rename an import without an alias".to_owned());
    }
    Ok(())
}

/// Edits that rename every occurrence: path, line, start and end column
/// (UTF-16) and the new text. Sorted by path and line, later columns first,
/// so applying them in order keeps earlier columns valid.
pub fn rename_edits(occ: &[Occurrence], new_name: &str) -> Vec<(PathBuf, u32, u32, u32, String)> {
    let mut edits: Vec<_> = occ
        .iter()
        .map(|o| {
            (
                o.path.clone(),
                o.line,
                o.start_column,
                o.end_column,
                new_name.to_owned(),
            )
        })
        .collect();
    edits.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(b.2.cmp(&a.2)));
    edits
}

/// Every use of the symbol at `(line, column)`, project-wide. `src` is the
/// text of `path`; open buffers of the engine win over disk for the rest.
pub fn references(
    engine: &mut GoEngine,
    path: &Path,
    src: &str,
    tree: &Tree,
    line: u32,
    column: u32,
) -> Vec<Occurrence> {
    let token = enter(engine, path, src);
    let out = references_inner(engine, path, src, tree, line, column);
    leave(engine, path, token);
    out
}

fn references_inner(
    engine: &mut GoEngine,
    path: &Path,
    src: &str,
    tree: &Tree,
    line: u32,
    column: u32,
) -> Vec<Occurrence> {
    let Some(query) = query_inner(engine, path, src, tree, line, column) else {
        return Vec::new();
    };
    let mut out = scan_file(engine, &query, path, src, Some(tree));
    if !matches!(query.reach, Reach::Within { .. }) {
        for (other, text) in candidates(engine, &query, path) {
            out.extend(scan_file(engine, &query, &other, &text, None));
        }
    }
    finish(out)
}

/// Identifies the symbol at `(line, column)`; `None` when there is none, it
/// is predeclared, or it cannot be resolved.
pub fn reference_query(
    engine: &mut GoEngine,
    path: &Path,
    src: &str,
    tree: &Tree,
    line: u32,
    column_utf16: u32,
) -> Option<GoRefQuery> {
    let token = enter(engine, path, src);
    let out = query_inner(engine, path, src, tree, line, column_utf16);
    leave(engine, path, token);
    out
}

/// Uses of the query's symbol in `src`, the content of `path`.
pub fn scan_references(
    engine: &mut GoEngine,
    query: &GoRefQuery,
    path: &Path,
    src: &str,
) -> Vec<Occurrence> {
    let token = enter(engine, path, src);
    let out = scan_file(engine, query, path, src, None);
    leave(engine, path, token);
    finish(out)
}

/// Sorted, without repeats and capped.
fn finish(mut found: Vec<Occurrence>) -> Vec<Occurrence> {
    found.sort();
    found.dedup();
    found.truncate(MAX_OCCURRENCES);
    found
}

// ---- the buffer being searched --------------------------------------------

/// Makes the engine see `src` as the text of `path`. Gives back what to
/// restore, or `None` when nothing changed.
fn enter(engine: &mut GoEngine, path: &Path, src: &str) -> Option<Option<Arc<str>>> {
    let previous = engine.open_text(path);
    if previous.as_deref() == Some(src) {
        return None;
    }
    if previous.is_none() && std::fs::read_to_string(path).is_ok_and(|disk| disk == src) {
        return None;
    }
    engine.open_document(path, src);
    Some(previous)
}

fn leave(engine: &mut GoEngine, path: &Path, token: Option<Option<Arc<str>>>) {
    match token {
        None => {}
        Some(Some(previous)) => engine.open_document(path, &previous),
        Some(None) => engine.close_document(path),
    }
}

// ---- identifying the symbol -----------------------------------------------

fn query_inner(
    engine: &mut GoEngine,
    path: &Path,
    src: &str,
    tree: &Tree,
    line: u32,
    column: u32,
) -> Option<GoRefQuery> {
    let id = identifier_at(tree, src, line, column)?;
    if id.name == "_" {
        return None;
    }
    let resolved = resolve_at(engine, path, src, tree, id.start_byte)?;
    let related_methods = related_methods(engine, &resolved.symbol);
    let node = tree
        .root_node()
        .descendant_for_byte_range(id.start_byte, id.end_byte)?;
    let name = id.name.clone();
    let mut external = false;
    let mut unaliased = false;
    let (key, reach) = match resolved.symbol {
        Symbol::Builtin { .. } => return None,
        Symbol::Local {
            path: decl,
            line,
            column,
            kind,
            ..
        } => {
            let key = Key::Decl {
                path: decl,
                line,
                column,
            };
            if matches!(kind.as_str(), "var" | "const") && !in_function(node) {
                // A package-level `var x` is seen as a local at its own name.
                let import_path = engine
                    .package_of_file(path)
                    .map(|p| p.import_path.clone())
                    .unwrap_or_default();
                let reach = Reach::Package {
                    dir: path.parent()?.to_path_buf(),
                    import_path,
                    exported: exported(&name),
                };
                (key, reach)
            } else {
                let top = top_level(node);
                let reach = Reach::Within {
                    path: path.to_path_buf(),
                    from: top.start_byte(),
                    to: top.end_byte(),
                };
                (key, reach)
            }
        }
        Symbol::Package { import_path } => {
            let file = parse_file(path, src);
            unaliased = !file
                .imports
                .iter()
                .any(|i| i.path == import_path && i.alias.as_deref() == Some(name.as_str()));
            let reach = Reach::Within {
                path: path.to_path_buf(),
                from: 0,
                to: src.len(),
            };
            (
                Key::Import {
                    file: path.to_path_buf(),
                    import_path,
                },
                reach,
            )
        }
        Symbol::TypeParam { .. } => {
            let owner = type_param_owner(node)?;
            let reach = Reach::Within {
                path: path.to_path_buf(),
                from: owner.start_byte(),
                to: owner.end_byte(),
            };
            (Key::TypeParam, reach)
        }
        Symbol::Type {
            pkg,
            path: decl,
            pos,
            ..
        }
        | Symbol::Func {
            pkg,
            path: decl,
            pos,
            ..
        }
        | Symbol::Value {
            pkg,
            path: decl,
            pos,
            ..
        } => {
            external = is_external(engine, &decl);
            let reach = Reach::Package {
                dir: decl.parent()?.to_path_buf(),
                import_path: pkg,
                exported: exported(&name),
            };
            (
                Key::Decl {
                    path: decl,
                    line: pos.line,
                    column: pos.column,
                },
                reach,
            )
        }
        Symbol::Field {
            path: decl, pos, ..
        }
        | Symbol::Method {
            path: decl, pos, ..
        } => {
            external = is_external(engine, &decl);
            let reach = Reach::Member {
                dir: decl.parent()?.to_path_buf(),
                exported: exported(&name),
            };
            (
                Key::Decl {
                    path: decl,
                    line: pos.line,
                    column: pos.column,
                },
                reach,
            )
        }
    };
    Some(GoRefQuery {
        key,
        related_methods,
        reach,
        external,
        name,
        unaliased,
    })
}

/// Interface method declarations and concrete methods of types that satisfy
/// the complete interface. This intentionally considers workspace packages
/// only: resolving dependencies here would make external methods renamable.
fn related_methods(engine: &mut GoEngine, symbol: &Symbol) -> Vec<(PathBuf, u32, u32)> {
    let Symbol::Method {
        owner,
        name,
        path,
        pos,
    } = symbol
    else {
        return Vec::new();
    };
    let Some(source_pkg) = engine.package_of_file(path) else {
        return Vec::new();
    };
    let source_is_interface = source_pkg
        .type_decl(owner)
        .is_some_and(|(_, decl)| decl.kind == TypeKind::Interface);
    let Some(signature) = source_signature(&source_pkg, owner, name, path, pos.line, pos.column)
    else {
        return Vec::new();
    };

    let mut packages = vec![source_pkg.clone()];
    for import_path in engine.module_package_paths().to_vec() {
        if let Some(pkg) = engine.package_by_import(&import_path)
            && !packages.iter().any(|known| known.dir == pkg.dir)
        {
            packages.push(pkg);
        }
    }

    let mut interfaces = Vec::new();
    for pkg in &packages {
        for file in &pkg.files {
            for decl in &file.types {
                if decl.kind != TypeKind::Interface {
                    continue;
                }
                if source_is_interface && (pkg.dir != source_pkg.dir || decl.name != *owner) {
                    continue;
                }
                let members = engine.members_of(pkg, &decl.name).items;
                if members.iter().any(|m| {
                    m.kind == MemberKind::Method
                        && m.name == *name
                        && method_signature(&m.ty).as_ref() == Some(&signature)
                }) {
                    interfaces.push(members);
                }
            }
        }
    }

    let mut related = Vec::new();
    for interface_members in &interfaces {
        for member in interface_members {
            if member.kind == MemberKind::Method
                && member.name == *name
                && method_signature(&member.ty).as_ref() == Some(&signature)
            {
                related.push((member.path.clone(), member.pos.line, member.pos.column));
            }
        }
        for concrete_pkg in &packages {
            for file in &concrete_pkg.files {
                for decl in &file.types {
                    if decl.kind == TypeKind::Interface {
                        continue;
                    }
                    let members = engine.members_of(concrete_pkg, &decl.name).items;
                    if implements(&members, interface_members) {
                        for member in members {
                            if member.kind == MemberKind::Method
                                && member.name == *name
                                && method_signature(&member.ty).as_ref() == Some(&signature)
                            {
                                related.push((member.path, member.pos.line, member.pos.column));
                            }
                        }
                    }
                }
            }
        }
    }
    related.sort();
    related.dedup();
    related
}

fn source_signature(
    pkg: &crate::go::engine::Package,
    owner: &str,
    name: &str,
    path: &Path,
    line: u32,
    column: u32,
) -> Option<TypeExpr> {
    let (file, decl) = pkg.type_decl(owner)?;
    if decl.kind == TypeKind::Interface {
        return decl
            .methods
            .iter()
            .find(|method| {
                file.path == path
                    && method.name == name
                    && method.pos.line == line
                    && method.pos.column == column
            })
            .and_then(|method| method_signature(&method.signature));
    }
    pkg.files
        .iter()
        .find(|file| file.path == path)
        .and_then(|file| {
            file.funcs.iter().find(|func| {
                func.receiver
                    .as_ref()
                    .is_some_and(|receiver| receiver.base == owner)
                    && func.name == name
                    && func.pos.line == line
                    && func.pos.column == column
            })
        })
        .and_then(func_signature)
}

fn func_signature(func: &FuncDecl) -> Option<TypeExpr> {
    let param = |param: &crate::go::parse::Param| {
        if param.variadic {
            format!("...{}", param.ty)
        } else {
            param.ty.clone()
        }
    };
    let mut text = format!(
        "({})",
        func.params.iter().map(param).collect::<Vec<_>>().join(", ")
    );
    match func.results.as_slice() {
        [] => {}
        [result] => text.push_str(&format!(" {}", param(result))),
        results => text.push_str(&format!(
            " ({})",
            results.iter().map(param).collect::<Vec<_>>().join(", ")
        )),
    }
    method_signature(&text)
}

fn method_signature(text: &str) -> Option<TypeExpr> {
    let signature = TypeExpr::parse(&format!("func{text}"));
    matches!(signature, TypeExpr::Func { .. }).then_some(signature)
}

fn implements(
    concrete: &[crate::go::engine::Member],
    interface: &[crate::go::engine::Member],
) -> bool {
    interface
        .iter()
        .filter(|required| required.kind == MemberKind::Method)
        .all(|required| {
            let Some(signature) = method_signature(&required.ty) else {
                return false;
            };
            concrete.iter().any(|candidate| {
                candidate.kind == MemberKind::Method
                    && candidate.name == required.name
                    && method_signature(&candidate.ty).as_ref() == Some(&signature)
            })
        })
}

fn in_function(node: Node<'_>) -> bool {
    let mut cur = node;
    while let Some(p) = cur.parent() {
        if matches!(
            p.kind(),
            "function_declaration" | "method_declaration" | "func_literal"
        ) {
            return true;
        }
        cur = p;
    }
    false
}

/// The declaration directly below the source file that holds `node`.
fn top_level(node: Node<'_>) -> Node<'_> {
    let mut cur = node;
    while let Some(p) = cur.parent() {
        if p.kind() == "source_file" {
            break;
        }
        cur = p;
    }
    cur
}

/// The type, function or method whose type parameter list `node` belongs to.
fn type_param_owner(node: Node<'_>) -> Option<Node<'_>> {
    let mut cur = node;
    while let Some(p) = cur.parent() {
        if matches!(
            p.kind(),
            "type_spec" | "type_alias" | "function_declaration" | "method_declaration"
        ) {
            return Some(p);
        }
        cur = p;
    }
    None
}

/// Declared outside the project: GOROOT, the module cache or `vendor/`.
fn is_external(engine: &GoEngine, decl: &Path) -> bool {
    let root = engine.root();
    if root.as_os_str().is_empty() {
        return false;
    }
    let mut bases: Vec<&Path> = vec![root];
    if let Some(project) = engine.project() {
        bases.extend(project.modules().iter().map(|m| m.dir.as_path()));
    }
    bases
        .into_iter()
        .find_map(|base| decl.strip_prefix(base).ok())
        .is_none_or(|rel| rel.components().any(|c| c.as_os_str() == "vendor"))
}

#[cfg(test)]
mod tests;
