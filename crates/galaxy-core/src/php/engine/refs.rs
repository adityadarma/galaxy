//! Find usages and rename.
//!
//! The symbol under the cursor is identified once. Every candidate file is
//! then searched for names that resolve to the same declaration, so two
//! classes with a method of the same name are told apart by the type of
//! the receiver, as in a go-to-definition.

use std::path::{Path, PathBuf};

use tree_sitter::Node;

use super::{Ctx, PhpEngine, Symbol, byte_offset, is_scope, is_stub_path, position_of};
use crate::php::framework;
use crate::php::parse::{MemberKind, parse_with_tree};

const MAX_FILES: usize = 20_000;
const MAX_CANDIDATES: usize = 3_000;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_OCCURRENCES: usize = 2_000;

/// What a name stands for. Names are lowercase where PHP ignores case.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ident {
    Class(String),
    Member {
        owner: String,
        name: String,
        method: bool,
    },
    Function(String),
    Variable(String),
}

/// A symbol to look for, built by [`PhpEngine::reference_query`].
#[derive(Debug, Clone)]
pub struct RefQuery {
    ident: Ident,
    /// The plain name searched for in sources.
    needle: String,
    root: PathBuf,
    /// Declared in `vendor/`, a built-in stub, or by a list such as
    /// `$fillable`: found, but not ours to rename.
    pub external: bool,
    /// A variable lives in one scope: file and byte span.
    scope: Option<(PathBuf, usize, usize)>,
}

/// One use of a symbol. Columns are UTF-16 on a single line.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Occurrence {
    pub path: PathBuf,
    pub line: u32,
    pub start_column: u32,
    pub end_column: u32,
    /// The range includes the `$` of a variable.
    pub dollar: bool,
}

/// Whether `name` can replace a symbol's name (a `$` is allowed in front).
pub fn valid_name(name: &str) -> bool {
    let bare = name.strip_prefix('$').unwrap_or(name);
    let mut chars = bare.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic() || !c.is_ascii())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric() || !c.is_ascii())
}

impl RefQuery {
    fn folds(&self) -> bool {
        !matches!(
            self.ident,
            Ident::Variable(_) | Ident::Member { method: false, .. }
        )
    }

    /// Whether a rename writes `$` in front of the new name.
    pub fn is_variable(&self) -> bool {
        matches!(self.ident, Ident::Variable(_))
    }

    /// Files of the project that mention the name, with their text. Run it
    /// without holding the engine, as it reads many files.
    pub fn candidates(&self) -> Vec<(PathBuf, String)> {
        let mut out = Vec::new();
        if self.scope.is_some() {
            return out;
        }
        let needle = if self.folds() {
            self.needle.to_ascii_lowercase()
        } else {
            self.needle.clone()
        };
        let mut stack = vec![(self.root.clone(), 0usize)];
        let mut seen = 0usize;
        while let Some((dir, depth)) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    if depth < 12
                        && !name.starts_with('.')
                        && !matches!(name, "vendor" | "node_modules" | "storage" | "cache")
                    {
                        stack.push((entry.path(), depth + 1));
                    }
                } else if name.ends_with(".php") && !framework::is_template_name(name) {
                    seen += 1;
                    if seen > MAX_FILES || out.len() >= MAX_CANDIDATES {
                        return out;
                    }
                    if !entry.metadata().is_ok_and(|m| m.len() <= MAX_FILE_BYTES) {
                        continue;
                    }
                    let Ok(text) = std::fs::read_to_string(entry.path()) else {
                        continue;
                    };
                    let hit = if self.folds() {
                        text.to_ascii_lowercase().contains(&needle)
                    } else {
                        text.contains(&needle)
                    };
                    if hit {
                        out.push((entry.path(), text));
                    }
                }
            }
        }
        out
    }
}

/// Sorted, without repeats and capped.
pub fn finish(mut found: Vec<Occurrence>) -> Vec<Occurrence> {
    found.sort();
    found.dedup();
    found.truncate(MAX_OCCURRENCES);
    found
}

impl PhpEngine {
    /// Every use of the symbol at `(line, column)`, project-wide.
    pub fn references(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Vec<Occurrence> {
        let Some(query) = self.reference_query(path, text, line, column) else {
            return Vec::new();
        };
        let mut out = self.scan_references(&query, path, text);
        for (other, body) in query.candidates() {
            if other != path {
                out.extend(self.scan_references(&query, &other, &body));
            }
        }
        finish(out)
    }

    /// Identifies the symbol at `(line, column)`; `None` when there is none
    /// or it cannot be resolved.
    pub fn reference_query(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<RefQuery> {
        let previous = self.register_current(path, text);
        let out = self.reference_query_inner(path, text, line, column);
        self.restore_current(path, previous);
        out
    }

    fn reference_query_inner(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<RefQuery> {
        let byte = byte_offset(text, line, column)?;
        let (file, tree) = parse_with_tree(path, text)?;
        let class = file
            .classes
            .iter()
            .find(|c| c.start_line <= line && line <= c.end_line)
            .cloned();
        let ctx = Ctx {
            src: text,
            tree: &tree,
            file: &file,
            byte,
            class,
        };
        let root = tree.root_node();
        let mut node = root.descendant_for_byte_range(byte, byte)?;
        if !matches!(node.kind(), "name" | "variable_name") && byte > 0 {
            node = root.descendant_for_byte_range(byte - 1, byte - 1)?;
        }
        let node = target_node(node)?;
        let symbol = self.symbol_for(&ctx, node)?;
        let ident = self.ident_of(&symbol)?;
        let external = self.declared_outside(&symbol);
        let (needle, scope) = match &ident {
            Ident::Class(fqn) | Ident::Function(fqn) => {
                // Keep the declared spelling for the search text.
                let short = match &symbol {
                    Symbol::Class(name) | Symbol::Function(name) => name.as_str(),
                    _ => fqn.as_str(),
                };
                (last_segment(short).to_owned(), None)
            }
            Ident::Member { name, .. } => (name.clone(), None),
            Ident::Variable(name) => {
                if name == "this" {
                    return None;
                }
                let mut scope = node;
                while let Some(up) = scope.parent() {
                    scope = up;
                    if is_scope(scope.kind()) && scope.kind() != "arrow_function" {
                        break;
                    }
                }
                (
                    name.clone(),
                    Some((path.to_path_buf(), scope.start_byte(), scope.end_byte())),
                )
            }
        };
        Some(RefQuery {
            ident,
            needle,
            root: self.root.clone(),
            external,
            scope,
        })
    }

    /// Uses of the query's symbol in `text`, the content of `path`.
    pub fn scan_references(
        &mut self,
        query: &RefQuery,
        path: &Path,
        text: &str,
    ) -> Vec<Occurrence> {
        let previous = self.register_current(path, text);
        let out = self.scan_inner(query, path, text);
        self.restore_current(path, previous);
        out
    }

    fn scan_inner(&mut self, query: &RefQuery, path: &Path, text: &str) -> Vec<Occurrence> {
        let mut out = Vec::new();
        if let Some((scope_path, ..)) = &query.scope
            && scope_path != path
        {
            return out;
        }
        let Some((file, tree)) = parse_with_tree(path, text) else {
            return out;
        };
        let variable = query.is_variable();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
            let is_variable_node = node.kind() == "variable_name";
            if !(is_variable_node || node.kind() == "name") {
                continue;
            }
            if let Some(parent) = node.parent()
                && node.kind() == "name"
                && matches!(parent.kind(), "variable_name" | "namespace_name")
            {
                continue;
            }
            if variable && !is_variable_node {
                continue;
            }
            let bare = text[node.byte_range()].trim_start_matches('$');
            let hit = if query.folds() {
                bare.eq_ignore_ascii_case(&query.needle)
            } else {
                bare == query.needle
            };
            if !hit {
                continue;
            }
            if let Some((_, from, to)) = &query.scope {
                if node.start_byte() < *from || node.end_byte() > *to {
                    continue;
                }
                if nearest_scope(node) != (*from, *to) {
                    continue;
                }
            }
            let row = node.start_position().row as u32;
            let class = file
                .classes
                .iter()
                .find(|c| c.start_line <= row && row <= c.end_line)
                .cloned();
            let ctx = Ctx {
                src: text,
                tree: &tree,
                file: &file,
                byte: node.start_byte(),
                class,
            };
            let Some(symbol) = self.symbol_for(&ctx, node) else {
                continue;
            };
            let Some(found) = self.ident_of(&symbol) else {
                continue;
            };
            if found != query.ident {
                continue;
            }
            let start_byte = if is_variable_node && !variable {
                node.start_byte() + 1
            } else {
                node.start_byte()
            };
            let (line, start_column, _) = position_of(text, start_byte);
            let (_, end_column, _) = position_of(text, node.end_byte());
            out.push(Occurrence {
                path: path.to_path_buf(),
                line,
                start_column,
                end_column,
                dollar: is_variable_node && variable,
            });
        }
        out
    }

    /// Declarations count as the symbol they declare; anything else is read
    /// as a use.
    fn symbol_for(&mut self, ctx: &Ctx<'_>, node: Node<'_>) -> Option<Symbol> {
        if let Some(symbol) = declaration_symbol(ctx, node) {
            return Some(symbol);
        }
        self.classify(ctx, node)
    }

    fn ident_of(&mut self, symbol: &Symbol) -> Option<Ident> {
        match symbol {
            Symbol::Class(fqn) => Some(Ident::Class(fqn.trim_start_matches('\\').to_lowercase())),
            Symbol::Member { class, name, call } => {
                let class = self.class(class)?;
                let bare = name.trim_start_matches('$');
                Some(match self.find_member_or_magic(&class, bare, *call) {
                    Some((owner, member)) => {
                        member_ident(&owner.fqn, &member.name, member.kind == MemberKind::Method)
                    }
                    None => member_ident(&class.fqn, bare, *call),
                })
            }
            Symbol::Function(name) => {
                let fqn = self
                    .function(name)
                    .map_or_else(|| name.clone(), |f| f.fqn.clone());
                Some(Ident::Function(fqn.trim_start_matches('\\').to_lowercase()))
            }
            Symbol::Variable(name) => {
                Some(Ident::Variable(name.trim_start_matches('$').to_owned()))
            }
        }
    }

    /// `true` when the declaration is not in the project or not a plain
    /// declaration in source, which a rename would have to touch.
    fn declared_outside(&mut self, symbol: &Symbol) -> bool {
        let path = match symbol {
            Symbol::Class(fqn) => self.class(fqn).map(|c| c.path.clone()),
            Symbol::Member { class, name, call } => {
                let Some(class) = self.class(class) else {
                    return true;
                };
                match self.find_member_or_magic(&class, name, *call) {
                    Some((owner, member)) => {
                        if member
                            .doc
                            .as_deref()
                            .is_some_and(|d| d.starts_with("Attribute listed in"))
                        {
                            return true;
                        }
                        Some(owner.path.clone())
                    }
                    None => None,
                }
            }
            Symbol::Function(name) => self.function(name).map(|f| f.path.clone()),
            Symbol::Variable(_) => return false,
        };
        path.is_none_or(|p| is_stub_path(&p) || p.components().any(|c| c.as_os_str() == "vendor"))
    }
}

fn member_ident(owner: &str, name: &str, method: bool) -> Ident {
    Ident::Member {
        owner: owner.trim_start_matches('\\').to_lowercase(),
        name: if method {
            name.to_lowercase()
        } else {
            name.to_owned()
        },
        method,
    }
}

fn last_segment(name: &str) -> &str {
    name.rsplit('\\').next().unwrap_or(name)
}

fn qualify(namespace: &str, name: &str) -> String {
    if namespace.is_empty() {
        name.to_owned()
    } else {
        format!("{namespace}\\{name}")
    }
}

/// Byte span of the nearest scope around `node`; an arrow function shares
/// the variables of the code around it.
fn nearest_scope(node: Node<'_>) -> (usize, usize) {
    let mut current = node;
    while let Some(parent) = current.parent() {
        current = parent;
        if is_scope(current.kind()) && current.kind() != "arrow_function" {
            break;
        }
    }
    (current.start_byte(), current.end_byte())
}

/// The name or variable a click on `node` means.
fn target_node(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "variable_name" => Some(node),
        "name" => {
            let parent = node.parent()?;
            match parent.kind() {
                "variable_name" => Some(parent),
                "namespace_name" => {
                    let mut top = parent;
                    while let Some(up) = top.parent() {
                        if up.kind() == "namespace_name" {
                            top = up;
                        } else {
                            break;
                        }
                    }
                    let qualified = top.parent().filter(|p| p.kind() == "qualified_name")?;
                    let mut cursor = qualified.walk();
                    let kids: Vec<Node<'_>> = qualified.named_children(&mut cursor).collect();
                    kids.into_iter().rev().find(|n| n.kind() == "name")
                }
                _ => Some(node),
            }
        }
        _ => None,
    }
}

/// Names that declare something, which `classify` reads as uses.
fn declaration_symbol(ctx: &Ctx<'_>, node: Node<'_>) -> Option<Symbol> {
    let parent = node.parent()?;
    let text = |n: Node<'_>| ctx.src[n.byte_range()].to_owned();
    let is_name = parent
        .child_by_field_name("name")
        .is_some_and(|n| n.id() == node.id());
    let class = || ctx.class.as_ref().map(|c| c.fqn.clone());
    match (node.kind(), parent.kind()) {
        (
            "name",
            "class_declaration"
            | "interface_declaration"
            | "trait_declaration"
            | "enum_declaration",
        ) if is_name => Some(Symbol::Class(qualify(&ctx.file.namespace, &text(node)))),
        ("name", "function_definition") if is_name => {
            Some(Symbol::Function(qualify(&ctx.file.namespace, &text(node))))
        }
        ("name", "method_declaration") if is_name => Some(Symbol::Member {
            class: class()?,
            name: text(node),
            call: true,
        }),
        ("name", "enum_case") if is_name => Some(Symbol::Member {
            class: class()?,
            name: text(node),
            call: false,
        }),
        ("name", "const_element") if parent.named_child(0).is_some_and(|n| n.id() == node.id()) => {
            Some(Symbol::Member {
                class: class()?,
                name: text(node),
                call: false,
            })
        }
        ("variable_name", "property_element" | "property_promotion_parameter") if is_name => {
            Some(Symbol::Member {
                class: class()?,
                name: text(node).trim_start_matches('$').to_owned(),
                call: false,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(
            root.join("composer.json"),
            r#"{"autoload":{"psr-4":{"App\\":"app/"}}}"#,
        )
        .unwrap();
        for (name, body) in [
            (
                "Order.php",
                "<?php\nnamespace App;\n\nclass Order\n{\n    public int $count = 0;\n    const MAX = 3;\n\n    public function total(): int\n    {\n        return 1;\n    }\n}\n",
            ),
            (
                "Report.php",
                "<?php\nnamespace App;\n\nclass Report\n{\n    public function run(Order $order): int\n    {\n        $x = $order->total();\n        return $order->count + Order::MAX + $x;\n    }\n\n    public function again(Order $o)\n    {\n        $x = 1;\n        return $o->total() + $x;\n    }\n}\n",
            ),
            (
                "Other.php",
                "<?php\nnamespace App;\n\nclass Other\n{\n    public function total(): int\n    {\n        return 2;\n    }\n\n    public function again(Other $o)\n    {\n        return $o->total();\n    }\n}\n",
            ),
        ] {
            std::fs::write(root.join("app").join(name), body).unwrap();
        }
        dir
    }

    fn at(
        engine: &mut PhpEngine,
        dir: &Path,
        file: &str,
        line: u32,
        column: u32,
    ) -> Vec<(String, u32, u32)> {
        let path = dir.join("app").join(file);
        let text = std::fs::read_to_string(&path).unwrap();
        engine
            .references(&path, &text, line, column)
            .into_iter()
            .map(|o| {
                (
                    o.path.file_name().unwrap().to_string_lossy().into_owned(),
                    o.line,
                    o.start_column,
                )
            })
            .collect()
    }

    #[test]
    fn methods_are_told_apart_by_their_class() {
        let dir = project();
        let mut engine = PhpEngine::new(dir.path());
        // `$order->total()` on line 7 of Report.php.
        let found = at(&mut engine, dir.path(), "Report.php", 7, 25);
        assert_eq!(
            found,
            vec![
                ("Order.php".into(), 8, 20),
                ("Report.php".into(), 7, 21),
                ("Report.php".into(), 14, 19),
            ],
            "Other::total is a different method"
        );
        // From the declaration itself.
        assert_eq!(at(&mut engine, dir.path(), "Order.php", 8, 22), found);
    }

    #[test]
    fn classes_are_found_in_hints_and_statics() {
        let dir = project();
        let mut engine = PhpEngine::new(dir.path());
        let found = at(&mut engine, dir.path(), "Report.php", 5, 26);
        assert_eq!(
            found,
            vec![
                ("Order.php".into(), 3, 6),
                ("Report.php".into(), 5, 24),
                ("Report.php".into(), 8, 31),
                ("Report.php".into(), 11, 26),
            ]
        );
    }

    #[test]
    fn properties_and_constants_exclude_the_dollar() {
        let dir = project();
        let mut engine = PhpEngine::new(dir.path());
        let found = at(&mut engine, dir.path(), "Report.php", 8, 25);
        assert_eq!(
            found,
            vec![("Order.php".into(), 5, 16), ("Report.php".into(), 8, 23)]
        );
        let found = at(&mut engine, dir.path(), "Report.php", 8, 39);
        assert_eq!(
            found,
            vec![("Order.php".into(), 6, 10), ("Report.php".into(), 8, 38)]
        );
    }

    #[test]
    fn variables_stay_in_their_method() {
        let dir = project();
        let mut engine = PhpEngine::new(dir.path());
        let found = at(&mut engine, dir.path(), "Report.php", 7, 9);
        assert_eq!(
            found,
            vec![("Report.php".into(), 7, 8), ("Report.php".into(), 8, 44),],
            "`$x` of `again` is another variable"
        );
    }

    #[test]
    fn vendor_and_stub_symbols_are_external() {
        let dir = project();
        let mut engine = PhpEngine::new(dir.path());
        let path = dir.path().join("app/Report.php");
        let text = "<?php\nnamespace App;\nfunction f(\\DateTime $d) {}\n";
        let query = engine.reference_query(&path, text, 2, 17).unwrap();
        assert!(query.external);
        let text = std::fs::read_to_string(&path).unwrap();
        let query = engine.reference_query(&path, &text, 7, 25).unwrap();
        assert!(!query.external);
    }

    #[test]
    fn names_are_checked() {
        assert!(valid_name("total"));
        assert!(valid_name("$total"));
        assert!(valid_name("_x1"));
        assert!(!valid_name(""));
        assert!(!valid_name("1x"));
        assert!(!valid_name("a-b"));
        assert!(!valid_name("a b"));
    }
}
