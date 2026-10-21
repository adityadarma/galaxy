//! Evaluation state: the file under the cursor, caches and limits.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use tree_sitter::{Node, Parser, Tree};

use super::Ty;
use crate::go::engine::{GoEngine, Package};
use crate::go::parse::{GoFile, parse_file};
use crate::go::scope::{Scope, ScopeIndex, scope_with};

/// Nesting of expression texts, type conversions and bindings followed.
pub(super) const MAX_DEPTH: usize = 24;
/// Nesting of syntax nodes walked, all expression texts together.
pub(super) const MAX_NODES: usize = 256;
/// Expressions evaluated for one request.
pub(super) const MAX_STEPS: usize = 40_000;

/// Source an expression text is parsed after.
const PREFIX: &str = "package p\nvar _ = ";

/// Type parameter names to their binding.
pub(super) type Env = HashMap<String, Ty>;

/// Where names are looked up: a file of a package, and the local scope when
/// that file is the one under the cursor.
#[derive(Clone)]
pub(super) struct Ctx {
    pub pkg: Arc<Package>,
    pub file: Arc<GoFile>,
    pub imports: Rc<HashMap<String, String>>,
    pub scope: Rc<Scope>,
}

pub(super) struct Eval<'e> {
    pub engine: &'e mut GoEngine,
    pub src: &'e str,
    pub tree: &'e Tree,
    pub path: &'e Path,
    /// The package of the file under the cursor, with the buffer's version
    /// of that file.
    pub pkg: Arc<Package>,
    pub file: Arc<GoFile>,
    pub parser: Parser,
    pub depth: usize,
    pub nodes: usize,
    pub steps: usize,
    /// Package-level values being resolved.
    pub visiting: HashSet<(String, String)>,
    pub scopes: HashMap<usize, Rc<Scope>>,
    pub scope_index: ScopeIndex<'e>,
    pub imports: HashMap<PathBuf, Rc<HashMap<String, String>>>,
    /// Types of local bindings, by declaration byte.
    pub bindings: HashMap<usize, Ty>,
    pub empty: Rc<Scope>,
}

impl<'e> Eval<'e> {
    pub(super) fn new(
        engine: &'e mut GoEngine,
        path: &'e Path,
        src: &'e str,
        tree: &'e Tree,
    ) -> Option<Self> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_go::LANGUAGE.into()).ok()?;
        let file = Arc::new(parse_file(path, src));
        let pkg = match engine.package_of_file(path) {
            Some(found) => {
                let mut files = found.files.clone();
                match files.iter().position(|f| f.path == path) {
                    Some(i) => files[i] = file.clone(),
                    None => {
                        files.push(file.clone());
                        files.sort_by(|a, b| a.path.cmp(&b.path));
                    }
                }
                Arc::new(Package {
                    import_path: found.import_path.clone(),
                    name: found.name.clone(),
                    dir: found.dir.clone(),
                    files,
                })
            }
            None => {
                let dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
                Arc::new(Package {
                    import_path: dir.to_string_lossy().into_owned(),
                    name: file.package.clone(),
                    dir,
                    files: vec![file.clone()],
                })
            }
        };
        Some(Self {
            engine,
            src,
            tree,
            path,
            pkg,
            file,
            parser,
            depth: 0,
            nodes: 0,
            steps: 0,
            visiting: HashSet::new(),
            scopes: HashMap::new(),
            scope_index: ScopeIndex::default(),
            imports: HashMap::new(),
            bindings: HashMap::new(),
            empty: Rc::new(Scope::default()),
        })
    }

    /// The file under the cursor with the names visible at `byte`.
    pub(super) fn site_at(&mut self, byte: usize) -> Ctx {
        let (tree, src) = (self.tree, self.src);
        let scope = self
            .scopes
            .entry(byte)
            .or_insert_with(|| Rc::new(scope_with(tree, src, byte, Some(&mut self.scope_index))))
            .clone();
        let file = self.file.clone();
        let imports = self.imports_of(&file);
        Ctx {
            pkg: self.pkg.clone(),
            file,
            imports,
            scope,
        }
    }

    fn imports_of(&mut self, file: &Arc<GoFile>) -> Rc<HashMap<String, String>> {
        if let Some(found) = self.imports.get(&file.path) {
            return found.clone();
        }
        let names = Rc::new(self.engine.import_names(file));
        self.imports.insert(file.path.clone(), names.clone());
        names
    }

    /// The context of the file `path` of `pkg`, without local names.
    pub(super) fn ctx_for(&mut self, pkg: &Arc<Package>, path: &Path) -> Option<Ctx> {
        let (pkg, file) = if path == self.path {
            (self.pkg.clone(), self.file.clone())
        } else {
            (
                pkg.clone(),
                pkg.files.iter().find(|f| f.path == path)?.clone(),
            )
        };
        let imports = self.imports_of(&file);
        Some(Ctx {
            pkg,
            file,
            imports,
            scope: self.empty.clone(),
        })
    }

    /// The package with import path `path`.
    pub(super) fn package_for(&mut self, path: &str) -> Option<Arc<Package>> {
        if path == self.pkg.import_path {
            return Some(self.pkg.clone());
        }
        self.engine.package_by_import(path)
    }

    /// Parses `text` as an expression and runs `f` on its node.
    pub(super) fn with_expr<R>(
        &mut self,
        text: &str,
        f: impl FnOnce(&mut Self, &str, Node<'_>) -> R,
    ) -> Option<R> {
        if self.depth >= MAX_DEPTH || text.trim().is_empty() {
            return None;
        }
        let src = format!("{PREFIX}{text}\n");
        let tree = self.parser.parse(&src, None)?;
        let node = expr_node(&tree)?;
        self.depth += 1;
        let out = f(self, &src, node);
        self.depth -= 1;
        Some(out)
    }
}

/// The initializer of the `var _ =` an expression text is wrapped in.
fn expr_node(tree: &Tree) -> Option<Node<'_>> {
    let root = tree.root_node();
    let mut cursor = root.walk();
    let decl = root
        .named_children(&mut cursor)
        .find(|n| n.kind() == "var_declaration")?;
    let mut c = decl.walk();
    let spec = decl
        .named_children(&mut c)
        .find(|n| n.kind() == "var_spec")?;
    let list = spec.child_by_field_name("value")?;
    let mut c2 = list.walk();
    list.named_children(&mut c2).find(|n| n.kind() != "comment")
}

/// Text of a node.
pub(super) fn text<'a>(src: &'a str, node: Node<'_>) -> &'a str {
    src.get(node.byte_range()).unwrap_or("")
}

/// Named children other than comments.
pub(super) fn kids<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|n| n.kind() != "comment")
        .collect()
}

/// A predeclared type.
pub(super) fn uni(name: &str) -> Ty {
    Ty::Named {
        pkg: String::new(),
        name: name.to_owned(),
        args: Vec::new(),
    }
}
