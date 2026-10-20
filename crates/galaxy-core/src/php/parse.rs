//! Declarations of one PHP file, read with Tree-sitter.
//!
//! Only what navigation and completion need: namespace, `use` imports,
//! classes/interfaces/traits/enums and their members. Method bodies are
//! not analysed here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use regex::Regex;
use tree_sitter::{Node, Parser};

use super::generics;

/// Type name stored for `static` / `$this` returns; the caller replaces it
/// with the class the call was made on.
pub const STATIC_TYPE: &str = "static";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassKind {
    Class,
    Interface,
    Trait,
    Enum,
}

impl ClassKind {
    pub fn keyword(self) -> &'static str {
        match self {
            Self::Class => "class",
            Self::Interface => "interface",
            Self::Trait => "trait",
            Self::Enum => "enum",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKind {
    Method,
    Property,
    Constant,
    Case,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Protected,
    Private,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub kind: MemberKind,
    pub visibility: Visibility,
    pub is_static: bool,
    /// `(params): return` for methods, the type or value otherwise.
    pub detail: String,
    /// Fully qualified type of the value or return, or [`STATIC_TYPE`].
    pub ty: Option<String>,
    pub line: u32,
    /// UTF-16 column of the name.
    pub column: u32,
    pub doc: Option<String>,
    /// A `@template` method/function that returns the class named by the
    /// `X::class` argument at this index (`app(X::class)`, `make(X::class)`).
    /// `ty` is then what a call without arguments returns.
    pub class_arg: Option<usize>,
    /// `@param class-string<T> $x` of a `@template T` method: the template
    /// and the index of the argument whose `X::class` binds it.
    pub class_params: Vec<(String, usize)>,
    /// The expression of the first top-level `return` in the body, used to
    /// work out a return type the declaration leaves without arguments.
    pub body_return: Option<String>,
    /// Declared by an `@method` or `@property` tag, not by code. Its
    /// position is that of the tag in the docblock.
    pub in_docblock: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassInfo {
    pub fqn: String,
    pub name: String,
    pub kind: ClassKind,
    pub path: PathBuf,
    pub line: u32,
    pub column: u32,
    pub start_line: u32,
    pub end_line: u32,
    pub is_abstract: bool,
    pub extends: Vec<String>,
    pub implements: Vec<String>,
    pub traits: Vec<String>,
    /// `@mixin` classes: their members are reachable through magic methods.
    pub mixins: Vec<String>,
    pub members: Vec<Member>,
    pub doc: Option<String>,
    /// Names declared with `@template`.
    pub templates: Vec<String>,
    /// `@extends`/`@implements` types with arguments, e.g.
    /// `Base<%T,Coll<,%T>>`; `%T` is a template of this class.
    pub generic_parents: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionInfo {
    pub fqn: String,
    pub name: String,
    pub path: PathBuf,
    pub line: u32,
    pub column: u32,
    pub detail: String,
    pub ty: Option<String>,
    pub doc: Option<String>,
    pub class_arg: Option<usize>,
    pub class_params: Vec<(String, usize)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileSymbols {
    pub classes: Vec<Arc<ClassInfo>>,
    pub functions: Vec<Arc<FunctionInfo>>,
    pub namespace: String,
    /// Lowercase alias to fully qualified name.
    pub uses: HashMap<String, String>,
    pub last_use_line: Option<u32>,
    pub namespace_line: Option<u32>,
    pub open_tag_line: Option<u32>,
}

/// Fully qualified form of `raw` as PHP resolves a class name.
pub fn resolve_name(raw: &str, namespace: &str, uses: &HashMap<String, String>) -> String {
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix('\\') {
        return rest.to_owned();
    }
    let (first, rest) = match raw.split_once('\\') {
        Some((first, rest)) => (first, Some(rest)),
        None => (raw, None),
    };
    if first.eq_ignore_ascii_case("namespace") {
        let rest = rest.unwrap_or("");
        return join_ns(namespace, rest);
    }
    if let Some(full) = uses.get(&first.to_lowercase()) {
        return match rest {
            Some(rest) => format!("{full}\\{rest}"),
            None => full.clone(),
        };
    }
    join_ns(namespace, raw)
}

fn join_ns(namespace: &str, name: &str) -> String {
    if namespace.is_empty() {
        name.to_owned()
    } else if name.is_empty() {
        namespace.to_owned()
    } else {
        format!("{namespace}\\{name}")
    }
}

const SCALARS: &[&str] = &[
    "int",
    "integer",
    "float",
    "double",
    "string",
    "bool",
    "boolean",
    "array",
    "mixed",
    "void",
    "null",
    "callable",
    "iterable",
    "object",
    "never",
    "false",
    "true",
    "resource",
    "numeric",
    "class-string",
    "list",
    "array-key",
    "non-empty-array",
    "non-empty-list",
    "non-empty-string",
    "scalar",
    "positive-int",
    "negative-int",
    "callable-string",
    "literal-string",
];

/// Class a type declaration or docblock type points at. Unions use their
/// first class; scalars, arrays and `Foo[]` give `None`.
pub fn resolve_type(
    raw: &str,
    namespace: &str,
    uses: &HashMap<String, String>,
    self_fqn: &str,
) -> Option<String> {
    let raw = raw.trim().trim_start_matches(':').trim();
    let no_generics = match raw.find('<') {
        Some(at) => &raw[..at],
        None => raw,
    };
    for part in no_generics.split(['|', '&']) {
        let part = part.trim().trim_start_matches('?').trim_matches(['(', ')']);
        if part.is_empty() || part.ends_with("[]") || part.ends_with('}') {
            continue;
        }
        let lower = part.to_ascii_lowercase();
        if lower == "static" || lower == "$this" {
            return Some(STATIC_TYPE.to_owned());
        }
        if lower == "self" {
            return Some(self_fqn.to_owned());
        }
        if SCALARS.contains(&lower.as_str()) {
            continue;
        }
        return Some(resolve_name(part, namespace, uses));
    }
    None
}

/// Like [`resolve_type`] but keeps generic arguments: `Base<A,B>`, with
/// `%T` for the template names in `templates`, [`STATIC_TYPE`] for
/// `static`/`$this`, and an empty argument where the type is not a class.
pub fn resolve_generic(
    raw: &str,
    namespace: &str,
    uses: &HashMap<String, String>,
    self_fqn: &str,
    templates: &[String],
) -> Option<String> {
    let raw = raw.trim().trim_start_matches(':').trim();
    if raw.starts_with('(') {
        // `($id is array ? Collection<TModel> : TModel|null)`: the else
        // branch is the common case (one id, one model).
        let inner = raw.strip_prefix('(')?.strip_suffix(')')?;
        let otherwise = conditional_else(inner)?;
        return resolve_generic(otherwise, namespace, uses, self_fqn, templates);
    }
    for part in generics::split_union(raw) {
        let part = part.trim().trim_start_matches('?').trim();
        if part.is_empty() || part.ends_with("[]") || part.ends_with('}') {
            continue;
        }
        let (base, args) = generics::split(part);
        let lower = base.to_ascii_lowercase();
        if lower == "static" || lower == "$this" {
            return Some(STATIC_TYPE.to_owned());
        }
        if lower == "self" {
            return Some(self_fqn.to_owned());
        }
        if templates.iter().any(|name| name == base) {
            return Some(format!("%{base}"));
        }
        if SCALARS.contains(&lower.as_str()) {
            continue;
        }
        let resolved = resolve_name(base, namespace, uses);
        let args: Vec<String> = args
            .iter()
            .map(|arg| {
                resolve_generic(arg, namespace, uses, self_fqn, templates).unwrap_or_default()
            })
            .collect();
        return Some(generics::join(&resolved, &args));
    }
    None
}

/// The branch after the top-level ` : ` of a conditional type body
/// (`$x is T ? A : B` gives `B`).
fn conditional_else(inner: &str) -> Option<&str> {
    let mut depth = 0i32;
    let mut seen_question = false;
    for (index, c) in inner.char_indices() {
        match c {
            '(' | '<' | '[' | '{' => depth += 1,
            ')' | '>' | ']' | '}' => depth -= 1,
            '?' if depth == 0 && inner[index..].starts_with("? ") => seen_question = true,
            ':' if depth == 0 && seen_question && inner[..index].ends_with(' ') => {
                return Some(inner[index + 1..].trim());
            }
            _ => {}
        }
    }
    None
}

/// Names declared by `@template` tags.
fn doc_templates(doc: Option<&str>) -> Vec<String> {
    static TEMPLATE: OnceLock<Regex> = OnceLock::new();
    let Some(doc) = doc else { return Vec::new() };
    let re = TEMPLATE.get_or_init(|| {
        Regex::new(r"@template(?:-covariant|-contravariant)?\s+(\w+)").expect("valid")
    });
    re.captures_iter(doc)
        .map(|caps| caps[1].to_owned())
        .collect()
}

/// Position of parameter `$name` in a parameter list.
fn param_position(params: &str, name: &str) -> Option<usize> {
    let wanted = format!("${name}");
    let inner = params.trim().trim_start_matches('(').trim_end_matches(')');
    generics::split_top(inner).iter().position(|part| {
        part.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '$')
            .any(|word| word == wanted)
    })
}

/// Templates bound by `class-string<T>` parameters.
fn class_params(doc: Option<&str>, params: Option<&str>) -> Vec<(String, usize)> {
    static PARAM: OnceLock<Regex> = OnceLock::new();
    let (Some(doc), Some(params)) = (doc, params) else {
        return Vec::new();
    };
    let re = PARAM.get_or_init(|| {
        Regex::new(r"@param\s+\S*class-string<(\w+)>\S*\s+\$(\w+)").expect("valid")
    });
    re.captures_iter(doc)
        .filter_map(|caps| Some((caps[1].to_owned(), param_position(params, &caps[2])?)))
        .collect()
}

/// The first word after `@tag` in a docblock.
pub fn doc_tag_type(doc: &str, tag: &str) -> Option<String> {
    let needle = format!("@{tag}");
    doc.lines().find_map(|line| {
        let rest = line.trim().trim_start_matches(['/', '*', ' ']);
        let rest = rest.strip_prefix(&needle)?;
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        Some(generics::take_type(rest.trim_start()).to_owned()).filter(|ty| !ty.is_empty())
    })
}

/// The first word after every `@tag` in a docblock.
pub fn doc_tag_types(doc: &str, tag: &str) -> Vec<String> {
    let needle = format!("@{tag}");
    doc.lines()
        .filter_map(|line| {
            let rest = line.trim().trim_start_matches(['/', '*', ' ']);
            let rest = rest.strip_prefix(&needle)?;
            if !rest.starts_with(char::is_whitespace) {
                return None;
            }
            Some(generics::take_type(rest.trim_start()).to_owned()).filter(|ty| !ty.is_empty())
        })
        .collect()
}

/// Index of the parameter documented as `class-string<T>` when the function
/// returns that `T` (`@template T`): the call returns the named class.
fn class_arg_index(doc: Option<&str>, params: Option<&str>) -> Option<usize> {
    static PARAM: OnceLock<Regex> = OnceLock::new();
    let doc = doc?;
    let params = params?;
    if !doc.contains("@template") {
        return None;
    }
    let re = PARAM.get_or_init(|| {
        Regex::new(r"@param\s+\S*class-string<(\w+)>\S*\s+\$(\w+)").expect("valid")
    });
    let caps = re.captures(doc)?;
    let template = caps.get(1)?.as_str();
    let returns_template = doc_tag_types(doc, "return").iter().any(|ty| {
        generics::split_union(ty)
            .iter()
            .any(|part| part.trim().trim_start_matches('?') == template)
    });
    if !returns_template {
        return None;
    }
    let wanted = format!("${}", &caps[2]);
    let inner = params.trim().trim_start_matches('(').trim_end_matches(')');
    let mut depth = 0i32;
    let mut index = 0usize;
    let mut current = String::new();
    let mut found = None;
    for ch in inner.chars().chain(std::iter::once(',')) {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                let has = current
                    .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '$')
                    .any(|word| word == wanted);
                if has && found.is_none() {
                    found = Some(index);
                }
                index += 1;
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(ch);
    }
    found
}

fn collapse(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("( ", "(")
        .replace(" )", ")")
}

/// Reads the declarations of `source`.
pub fn parse_file(path: &Path, source: &str) -> FileSymbols {
    parse_with_tree(path, source)
        .map(|(file, _)| file)
        .unwrap_or_default()
}

/// Like [`parse_file`], and hands back the syntax tree for further lookups.
pub fn parse_with_tree(path: &Path, source: &str) -> Option<(FileSymbols, tree_sitter::Tree)> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
        .ok()?;
    let tree = parser.parse(source, None)?;
    let mut reader = Reader {
        src: source,
        path,
        namespace: String::new(),
        uses: HashMap::new(),
        out: FileSymbols::default(),
        templates: Vec::new(),
        attrs: Vec::new(),
    };
    reader.statements(tree.root_node());
    reader.out.namespace = reader.namespace;
    reader.out.uses = reader.uses;
    Some((reader.out, tree))
}

struct Reader<'a> {
    src: &'a str,
    path: &'a Path,
    namespace: String,
    uses: HashMap<String, String>,
    out: FileSymbols,
    /// Template names in scope: the class's and the member's.
    templates: Vec<String>,
    /// Attribute names found in the class being read.
    attrs: Vec<Attr>,
}

/// A name listed in a property array such as `$fillable`, which stands for
/// a property the class reaches through magic access.
#[derive(Debug)]
struct Attr {
    source: &'static str,
    name: String,
    line: u32,
    column: u32,
    cast: Option<String>,
}

/// Property arrays whose strings name attributes, most authoritative first.
const ATTRIBUTE_SOURCES: &[&str] = &["fillable", "casts", "hidden", "visible", "dates"];

impl<'a> Reader<'a> {
    fn text(&self, node: Node<'_>) -> &'a str {
        &self.src[node.byte_range()]
    }

    fn column(&self, node: Node<'_>) -> u32 {
        let start = node.start_byte();
        let line_start = start - node.start_position().column;
        self.src[line_start..start].encode_utf16().count() as u32
    }

    fn statements(&mut self, parent: Node<'_>) {
        let mut cursor = parent.walk();
        let kids: Vec<Node<'_>> = parent.named_children(&mut cursor).collect();
        for node in kids {
            match node.kind() {
                "php_tag" => {
                    self.out
                        .open_tag_line
                        .get_or_insert(node.start_position().row as u32);
                }
                "namespace_definition" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        self.namespace = self.text(name).trim_matches('\\').to_owned();
                    } else {
                        self.namespace.clear();
                    }
                    self.out.namespace_line = Some(node.start_position().row as u32);
                    if let Some(body) = node.child_by_field_name("body") {
                        self.statements(body);
                    }
                }
                "namespace_use_declaration" => {
                    self.read_use(node);
                    self.out.last_use_line = Some(node.end_position().row as u32);
                }
                "function_definition" => self.read_function(node),
                // `if (! function_exists('now')) { function now() {} }`: how
                // libraries declare helpers a project may override.
                "if_statement" => {
                    let guarded = node
                        .child_by_field_name("condition")
                        .is_some_and(|c| self.text(c).contains("_exists("));
                    if guarded && let Some(body) = node.child_by_field_name("body") {
                        self.statements(body);
                    }
                }
                "class_declaration" => self.read_class(node, ClassKind::Class),
                "interface_declaration" => self.read_class(node, ClassKind::Interface),
                "trait_declaration" => self.read_class(node, ClassKind::Trait),
                "enum_declaration" => self.read_class(node, ClassKind::Enum),
                _ => {}
            }
        }
    }

    fn read_function(&mut self, node: Node<'_>) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name_node).to_owned();
        let doc = self.doc_before(node);
        let params = node
            .child_by_field_name("parameters")
            .map(|p| collapse(self.text(p)))
            .unwrap_or_default();
        let ret = node.child_by_field_name("return_type").map(|n| {
            collapse(self.text(n))
                .trim_start_matches(':')
                .trim()
                .to_owned()
        });
        let params_node = node.child_by_field_name("parameters").map(|p| self.text(p));
        let class_arg = class_arg_index(doc.as_deref(), params_node);
        let class_params = class_params(doc.as_deref(), params_node);
        self.templates = doc_templates(doc.as_deref());
        let ty = if class_arg.is_some() {
            None
        } else {
            self.type_of(ret.as_deref().unwrap_or(""), doc.as_deref(), "return", "")
        };
        self.templates.clear();
        let detail = match &ret {
            Some(ret) => format!("{params}: {ret}"),
            None => params,
        };
        self.out.functions.push(Arc::new(FunctionInfo {
            class_arg,
            class_params,
            fqn: join_ns(&self.namespace, &name),
            name,
            path: self.path.to_path_buf(),
            line: name_node.start_position().row as u32,
            column: self.column(name_node),
            detail,
            ty,
            doc,
        }));
    }

    fn read_use(&mut self, node: Node<'_>) {
        if node.child_by_field_name("type").is_some() {
            return;
        }
        let mut prefix = String::new();
        let mut cursor = node.walk();
        let kids: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        for child in kids {
            match child.kind() {
                "namespace_name" => prefix = self.text(child).trim_matches('\\').to_owned(),
                "namespace_use_clause" => self.add_use(child, ""),
                "namespace_use_group" => {
                    let mut inner = child.walk();
                    let clauses: Vec<Node<'_>> = child.named_children(&mut inner).collect();
                    for clause in clauses {
                        if clause.kind() == "namespace_use_clause" {
                            self.add_use(clause, &prefix);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn add_use(&mut self, clause: Node<'_>, prefix: &str) {
        if clause.child_by_field_name("type").is_some() {
            return;
        }
        let mut cursor = clause.walk();
        let name = clause
            .named_children(&mut cursor)
            .find(|n| matches!(n.kind(), "name" | "qualified_name"));
        let Some(name) = name else { return };
        let raw = self.text(name).trim_start_matches('\\');
        let full = join_ns(prefix, raw);
        let alias = clause
            .child_by_field_name("alias")
            .map(|n| self.text(n).to_owned())
            .unwrap_or_else(|| full.rsplit('\\').next().unwrap_or(&full).to_owned());
        self.uses.insert(alias.to_lowercase(), full);
    }

    fn names_in(&self, node: Node<'_>) -> Vec<String> {
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .filter(|n| matches!(n.kind(), "name" | "qualified_name" | "relative_name"))
            .map(|n| resolve_name(self.text(n), &self.namespace, &self.uses))
            .collect()
    }

    fn doc_before(&self, node: Node<'_>) -> Option<String> {
        let prev = node.prev_named_sibling()?;
        let text = self.text(prev);
        (prev.kind() == "comment" && text.starts_with("/**")).then(|| text.to_owned())
    }

    /// Row and UTF-16 column where the docblock before `node` starts.
    fn doc_start(&self, node: Node<'_>) -> Option<(u32, u32)> {
        let prev = node.prev_named_sibling()?;
        (prev.kind() == "comment" && self.text(prev).starts_with("/**"))
            .then(|| (prev.start_position().row as u32, self.column(prev)))
    }

    fn modifiers(&self, node: Node<'_>) -> (Visibility, bool) {
        let mut visibility = Visibility::Public;
        let mut is_static = false;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "visibility_modifier" => {
                    visibility = match self.text(child).trim() {
                        "private" => Visibility::Private,
                        "protected" => Visibility::Protected,
                        _ => Visibility::Public,
                    }
                }
                "static_modifier" => is_static = true,
                _ => {}
            }
        }
        (visibility, is_static)
    }

    fn type_of(&self, raw: &str, doc: Option<&str>, tag: &str, self_fqn: &str) -> Option<String> {
        let resolve = |text: &str| {
            resolve_generic(text, &self.namespace, &self.uses, self_fqn, &self.templates)
        };
        let native = resolve(raw);
        let documented = doc
            .and_then(|doc| doc_tag_type(doc, tag))
            .and_then(|ty| resolve(&ty));
        match (native, documented) {
            // `: HasMany` plus `@return HasMany<Model, $this>`: the docblock adds arguments.
            (Some(native), Some(documented))
                if generics::base(&native).eq_ignore_ascii_case(generics::base(&documented)) =>
            {
                Some(documented)
            }
            (Some(native), _) => Some(native),
            (None, documented) => documented,
        }
    }

    /// `@extends`, `@implements`, `@use` (and `@template-` forms) and `@mixin`
    /// of a class docblock or of a trait `use` statement, with the generic arguments they were written with.
    fn generic_parents(&self, doc: Option<&str>, self_fqn: &str) -> Vec<String> {
        let Some(doc) = doc else { return Vec::new() };
        let mut out = Vec::new();
        for line in doc.lines() {
            let rest = line.trim().trim_start_matches(['/', '*', ' ']);
            for tag in [
                "template-extends",
                "extends",
                "template-implements",
                "implements",
                "template-use",
                "use",
                "mixin",
            ] {
                let Some(after) = rest.strip_prefix('@').and_then(|r| r.strip_prefix(tag)) else {
                    continue;
                };
                if !after.starts_with(char::is_whitespace) {
                    continue;
                }
                let ty = generics::take_type(after.trim_start());
                if let Some(resolved) =
                    resolve_generic(ty, &self.namespace, &self.uses, self_fqn, &self.templates)
                {
                    out.push(resolved);
                }
            }
        }
        out
    }

    /// Expression of the first `return` directly in a method body.
    fn body_return(&self, method: Node<'_>) -> Option<String> {
        let body = method.child_by_field_name("body")?;
        let mut cursor = body.walk();
        let ret = body
            .named_children(&mut cursor)
            .find(|n| n.kind() == "return_statement")?;
        let expr = ret.named_child(0)?;
        let text = collapse(self.text(expr));
        (text.len() <= 400).then_some(text)
    }

    fn read_class(&mut self, node: Node<'_>, kind: ClassKind) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name_node).to_owned();
        let fqn = join_ns(&self.namespace, &name);
        let mut info = ClassInfo {
            fqn: fqn.clone(),
            name,
            kind,
            path: self.path.to_path_buf(),
            line: name_node.start_position().row as u32,
            column: self.column(name_node),
            start_line: node.start_position().row as u32,
            end_line: node.end_position().row as u32,
            is_abstract: false,
            extends: Vec::new(),
            implements: Vec::new(),
            traits: Vec::new(),
            mixins: Vec::new(),
            members: Vec::new(),
            doc: self.doc_before(node),
            templates: Vec::new(),
            generic_parents: Vec::new(),
        };
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "abstract_modifier" => info.is_abstract = true,
                "base_clause" => info.extends = self.names_in(child),
                "class_interface_clause" => info.implements = self.names_in(child),
                _ => {}
            }
        }
        self.attrs.clear();
        info.templates = doc_templates(info.doc.as_deref());
        self.templates = info.templates.clone();
        info.generic_parents = self.generic_parents(info.doc.as_deref(), &info.fqn);
        if let Some(body) = node.child_by_field_name("body") {
            self.read_members(body, &mut info);
        }
        if let Some(doc) = info.doc.clone() {
            let start = self.doc_start(node).unwrap_or((info.line, info.column));
            self.read_doc_members(&doc, start, &mut info);
            info.mixins = doc_tag_types(&doc, "mixin")
                .into_iter()
                .filter_map(|raw| resolve_type(&raw, &self.namespace, &self.uses, &info.fqn))
                .collect();
        }
        self.merge_attributes(&mut info);
        self.templates.clear();
        self.out.classes.push(Arc::new(info));
    }

    /// `@method` and `@property` tags of a class docblock, which is how
    /// facades and ide-helper models describe their magic members.
    fn read_doc_members(&self, doc: &str, start: (u32, u32), info: &mut ClassInfo) {
        // Row and UTF-16 column of the byte `at` of the docblock.
        let position = |at: usize| {
            let before = &doc[..at];
            let rows = before.matches('\n').count() as u32;
            let line_start = before.rfind('\n').map_or(0, |i| i + 1);
            let column = before[line_start..].encode_utf16().count() as u32;
            (
                start.0 + rows,
                if rows == 0 { start.1 + column } else { column },
            )
        };
        static METHOD: OnceLock<Regex> = OnceLock::new();
        static PROPERTY: OnceLock<Regex> = OnceLock::new();
        let method = METHOD.get_or_init(|| {
            Regex::new(r"@method\s+(static\s+)?(?:(\S+)\s+)?(\w+)\s*\(([^)]*)\)").expect("valid")
        });
        let property = PROPERTY.get_or_init(|| {
            Regex::new(r"@property(?:-read|-write)?\s+(?:(\S+)\s+)?\$(\w+)").expect("valid")
        });
        for caps in method.captures_iter(doc) {
            let raw_type = caps.get(2).map_or("", |m| m.as_str());
            let params = collapse(caps.get(4).map_or("", |m| m.as_str()));
            let detail = if raw_type.is_empty() {
                format!("({params})")
            } else {
                format!("({params}): {raw_type}")
            };
            info.members.push(Member {
                in_docblock: true,
                class_arg: None,
                class_params: Vec::new(),
                body_return: None,
                name: caps[3].to_owned(),
                kind: MemberKind::Method,
                visibility: Visibility::Public,
                is_static: caps.get(1).is_some(),
                detail,
                ty: resolve_type(raw_type, &self.namespace, &self.uses, &info.fqn),
                line: position(caps.get(3).map_or(0, |m| m.start())).0,
                column: position(caps.get(3).map_or(0, |m| m.start())).1,
                doc: None,
            });
        }
        for caps in property.captures_iter(doc) {
            let raw_type = caps.get(1).map_or("", |m| m.as_str());
            info.members.push(Member {
                in_docblock: true,
                class_arg: None,
                class_params: Vec::new(),
                body_return: None,
                name: caps[2].to_owned(),
                kind: MemberKind::Property,
                visibility: Visibility::Public,
                is_static: false,
                detail: raw_type.to_owned(),
                ty: resolve_type(raw_type, &self.namespace, &self.uses, &info.fqn),
                line: position(caps.get(2).map_or(0, |m| m.start())).0,
                column: position(caps.get(2).map_or(0, |m| m.start())).1,
                doc: None,
            });
        }
    }

    /// Text and node of a plain string literal's contents.
    fn string_value(&self, node: Node<'_>) -> Option<(String, u32, u32)> {
        if node.kind() != "string" {
            return None;
        }
        let mut cursor = node.walk();
        let content = node
            .named_children(&mut cursor)
            .find(|n| n.kind() == "string_content")?;
        Some((
            self.text(content).to_owned(),
            content.start_position().row as u32,
            self.column(content),
        ))
    }

    /// Strings of an array literal, as `['a', 'b' => 'cast']`: the value for
    /// a list entry, the key (with its value as the cast) for a pair.
    fn read_attribute_array(&mut self, array: Node<'_>, source: &'static str) {
        if array.kind() != "array_creation_expression" {
            return;
        }
        let mut cursor = array.walk();
        let elements: Vec<Node<'_>> = array
            .named_children(&mut cursor)
            .filter(|n| n.kind() == "array_element_initializer")
            .collect();
        for element in elements {
            let mut inner = element.walk();
            let parts: Vec<Node<'_>> = element.named_children(&mut inner).collect();
            let (key, cast) = match parts.as_slice() {
                [value] => (*value, None),
                [key, value] => (
                    *key,
                    Some(
                        self.string_value(*value)
                            .map(|(text, ..)| text)
                            .unwrap_or_else(|| collapse(self.text(*value))),
                    ),
                ),
                _ => continue,
            };
            if let Some((name, line, column)) = self.string_value(key) {
                self.attrs.push(Attr {
                    source,
                    name,
                    line,
                    column,
                    cast,
                });
            }
        }
    }

    /// Adds the collected attribute names as properties, unless the class
    /// already declares or documents one of that name.
    fn merge_attributes(&mut self, info: &mut ClassInfo) {
        let attrs = std::mem::take(&mut self.attrs);
        if attrs.is_empty() {
            return;
        }
        let casts: HashMap<&str, &str> = attrs
            .iter()
            .filter(|a| a.source == "casts")
            .filter_map(|a| Some((a.name.as_str(), a.cast.as_deref()?)))
            .collect();
        let mut seen: std::collections::HashSet<String> =
            info.members.iter().map(|m| m.name.clone()).collect();
        for source in ATTRIBUTE_SOURCES {
            for attr in attrs.iter().filter(|a| a.source == *source) {
                if !seen.insert(attr.name.clone()) {
                    continue;
                }
                info.members.push(Member {
                    in_docblock: false,
                    class_arg: None,
                    class_params: Vec::new(),
                    body_return: None,
                    name: attr.name.clone(),
                    kind: MemberKind::Property,
                    visibility: Visibility::Public,
                    is_static: false,
                    detail: casts
                        .get(attr.name.as_str())
                        .copied()
                        .unwrap_or("")
                        .to_owned(),
                    ty: None,
                    line: attr.line,
                    column: attr.column,
                    doc: Some(format!("Attribute listed in `${source}`.")),
                });
            }
        }
    }

    fn read_members(&mut self, body: Node<'_>, info: &mut ClassInfo) {
        let mut cursor = body.walk();
        let kids: Vec<Node<'_>> = body.named_children(&mut cursor).collect();
        for child in kids {
            match child.kind() {
                "method_declaration" => self.read_method(child, info),
                "property_declaration" => self.read_property(child, info),
                "const_declaration" => self.read_const(child, info),
                "enum_case" => {
                    if let Some(name) = child.child_by_field_name("name") {
                        info.members.push(Member {
                            in_docblock: false,
                            class_arg: None,
                            class_params: Vec::new(),
                            body_return: None,
                            name: self.text(name).to_owned(),
                            kind: MemberKind::Case,
                            visibility: Visibility::Public,
                            is_static: true,
                            detail: child
                                .child_by_field_name("value")
                                .map(|v| collapse(self.text(v)))
                                .unwrap_or_default(),
                            ty: Some(info.fqn.clone()),
                            line: name.start_position().row as u32,
                            column: self.column(name),
                            doc: self.doc_before(child),
                        });
                    }
                }
                "use_declaration" => {
                    info.traits.extend(self.names_in(child));
                    // `/** @use BuildsQueries<TModel> */` binds the trait's templates.
                    let doc = self.doc_before(child);
                    let bound = self.generic_parents(doc.as_deref(), &info.fqn);
                    info.generic_parents.extend(bound);
                }
                _ => {}
            }
        }
    }

    fn read_method(&mut self, node: Node<'_>, info: &mut ClassInfo) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name_node).to_owned();
        let (visibility, is_static) = self.modifiers(node);
        let doc = self.doc_before(node);
        let params = node.child_by_field_name("parameters");
        let ret = node.child_by_field_name("return_type").map(|n| {
            collapse(self.text(n))
                .trim_start_matches(':')
                .trim()
                .to_owned()
        });
        let class_arg = class_arg_index(doc.as_deref(), params.map(|p| self.text(p)));
        let class_params = class_params(doc.as_deref(), params.map(|p| self.text(p)));
        let class_templates = self.templates.len();
        self.templates.extend(doc_templates(doc.as_deref()));
        let ty = if class_arg.is_some() {
            None
        } else {
            match &ret {
                Some(ret) => self.type_of(ret, doc.as_deref(), "return", &info.fqn),
                None => self.type_of("", doc.as_deref(), "return", &info.fqn),
            }
        };
        self.templates.truncate(class_templates);
        let body_return = self.body_return(node);
        if name.eq_ignore_ascii_case("casts")
            && let Some(body) = node.child_by_field_name("body")
        {
            let mut cursor = body.walk();
            let returned = body
                .named_children(&mut cursor)
                .find(|n| n.kind() == "return_statement")
                .and_then(|ret| ret.named_child(0));
            if let Some(array) = returned {
                self.read_attribute_array(array, "casts");
            }
        }
        let params_text = params.map(|p| collapse(self.text(p))).unwrap_or_default();
        let detail = match &ret {
            Some(ret) => format!("{params_text}: {ret}"),
            None => params_text,
        };
        if name.eq_ignore_ascii_case("__construct")
            && let Some(params) = params
        {
            self.read_promoted(params, info);
        }
        info.members.push(Member {
            in_docblock: false,
            class_arg,
            class_params,
            body_return,
            name,
            kind: MemberKind::Method,
            visibility,
            is_static,
            detail,
            ty,
            line: name_node.start_position().row as u32,
            column: self.column(name_node),
            doc,
        });
    }

    fn read_promoted(&mut self, params: Node<'_>, info: &mut ClassInfo) {
        let mut cursor = params.walk();
        let kids: Vec<Node<'_>> = params.named_children(&mut cursor).collect();
        for param in kids {
            if param.kind() != "property_promotion_parameter" {
                continue;
            }
            let Some(name) = param.child_by_field_name("name") else {
                continue;
            };
            let visibility = match param
                .child_by_field_name("visibility")
                .map(|v| self.text(v).trim())
            {
                Some("private") => Visibility::Private,
                Some("protected") => Visibility::Protected,
                _ => Visibility::Public,
            };
            let raw = param
                .child_by_field_name("type")
                .map(|t| collapse(self.text(t)))
                .unwrap_or_default();
            info.members.push(Member {
                in_docblock: false,
                class_arg: None,
                class_params: Vec::new(),
                body_return: None,
                name: self.text(name).trim_start_matches('$').to_owned(),
                kind: MemberKind::Property,
                visibility,
                is_static: false,
                ty: self.type_of(&raw, None, "var", &info.fqn),
                detail: raw,
                line: name.start_position().row as u32,
                column: self.column(name),
                doc: None,
            });
        }
    }

    fn read_property(&mut self, node: Node<'_>, info: &mut ClassInfo) {
        let (visibility, is_static) = self.modifiers(node);
        let doc = self.doc_before(node);
        let raw = node
            .child_by_field_name("type")
            .map(|t| collapse(self.text(t)))
            .unwrap_or_default();
        let ty = self.type_of(&raw, doc.as_deref(), "var", &info.fqn);
        let detail = if raw.is_empty() {
            doc.as_deref()
                .and_then(|d| doc_tag_type(d, "var"))
                .unwrap_or_default()
        } else {
            raw
        };
        let mut cursor = node.walk();
        let elements: Vec<Node<'_>> = node
            .named_children(&mut cursor)
            .filter(|n| n.kind() == "property_element")
            .collect();
        for element in elements {
            let Some(name) = element.child_by_field_name("name") else {
                continue;
            };
            let property = self.text(name).trim_start_matches('$');
            if !is_static
                && let Some(source) = ATTRIBUTE_SOURCES.iter().find(|s| **s == property)
                && let Some(default) = element.child_by_field_name("default_value")
            {
                self.read_attribute_array(default, source);
            }
            info.members.push(Member {
                in_docblock: false,
                class_arg: None,
                class_params: Vec::new(),
                body_return: None,
                name: self.text(name).trim_start_matches('$').to_owned(),
                kind: MemberKind::Property,
                visibility,
                is_static,
                detail: detail.clone(),
                ty: ty.clone(),
                line: name.start_position().row as u32,
                column: self.column(name),
                doc: doc.clone(),
            });
        }
    }

    fn read_const(&mut self, node: Node<'_>, info: &mut ClassInfo) {
        let (visibility, _) = self.modifiers(node);
        let doc = self.doc_before(node);
        let mut cursor = node.walk();
        let elements: Vec<Node<'_>> = node
            .named_children(&mut cursor)
            .filter(|n| n.kind() == "const_element")
            .collect();
        for element in elements {
            let mut inner = element.walk();
            let mut parts = element.named_children(&mut inner);
            let Some(name) = parts.next() else { continue };
            let value = parts.next().map(|v| collapse(self.text(v)));
            let mut detail = value.unwrap_or_default();
            if detail.chars().count() > 40 {
                detail = detail.chars().take(40).collect::<String>() + "…";
            }
            info.members.push(Member {
                in_docblock: false,
                class_arg: None,
                class_params: Vec::new(),
                body_return: None,
                name: self.text(name).to_owned(),
                kind: MemberKind::Constant,
                visibility,
                is_static: true,
                detail,
                ty: None,
                line: name.start_position().row as u32,
                column: self.column(name),
                doc: doc.clone(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"<?php

namespace App\Models;

use Illuminate\Database\Eloquent\{Model, Builder as Q};
use App\Support\Helper;
use function strlen;

/** A user. */
abstract class User extends Model implements \JsonSerializable
{
    use Concerns\HasThing;

    public const ROLE = 'admin';
    protected static int $count = 0;
    public ?Profile $profile = null;

    public function __construct(private readonly Helper $helper, public string $name) {}

    /** @return Profile */
    public function profile() { return $this->profile; }

    public static function make(int $a,
        string $b): static { return new static; }
}

enum Status: string { case Active = 'a'; }
"#;

    fn parsed() -> FileSymbols {
        parse_file(Path::new("/p/app/Models/User.php"), SRC)
    }

    #[test]
    fn reads_namespace_and_uses() {
        let file = parsed();
        assert_eq!(file.namespace, "App\\Models");
        assert_eq!(file.uses["model"], "Illuminate\\Database\\Eloquent\\Model");
        assert_eq!(file.uses["q"], "Illuminate\\Database\\Eloquent\\Builder");
        assert_eq!(file.uses["helper"], "App\\Support\\Helper");
        assert!(!file.uses.contains_key("strlen"));
        assert_eq!(file.last_use_line, Some(6));
        assert_eq!(file.namespace_line, Some(2));
        assert_eq!(file.open_tag_line, Some(0));
    }

    #[test]
    fn reads_classes_and_inheritance() {
        let file = parsed();
        let user = &file.classes[0];
        assert_eq!(user.fqn, "App\\Models\\User");
        assert!(user.is_abstract);
        assert_eq!(user.extends, ["Illuminate\\Database\\Eloquent\\Model"]);
        assert_eq!(user.implements, ["JsonSerializable"]);
        assert_eq!(user.traits, ["App\\Models\\Concerns\\HasThing"]);
        assert_eq!((user.line, user.column), (9, 15));
        assert_eq!(file.classes[1].kind, ClassKind::Enum);
    }

    #[test]
    fn reads_members() {
        let file = parsed();
        let user = &file.classes[0];
        let find = |name: &str| user.members.iter().find(|m| m.name == name).unwrap();

        let role = find("ROLE");
        assert_eq!(
            (role.kind, role.detail.as_str()),
            (MemberKind::Constant, "'admin'")
        );

        let count = find("count");
        assert!(count.is_static);
        assert_eq!(count.visibility, Visibility::Protected);

        assert_eq!(find("profile").kind, MemberKind::Property);
        assert_eq!(
            user.members
                .iter()
                .find(|m| m.name == "profile" && m.kind == MemberKind::Method)
                .unwrap()
                .ty
                .as_deref(),
            Some("App\\Models\\Profile")
        );

        let helper = find("helper");
        assert_eq!(helper.visibility, Visibility::Private);
        assert_eq!(helper.ty.as_deref(), Some("App\\Support\\Helper"));
        assert_eq!(find("name").visibility, Visibility::Public);

        let make = find("make");
        assert!(make.is_static);
        assert_eq!(make.detail, "(int $a, string $b): static");
        assert_eq!(make.ty.as_deref(), Some(STATIC_TYPE));

        assert_eq!(file.classes[1].members[0].name, "Active");
    }

    #[test]
    fn reads_magic_members_from_docblocks() {
        let src = "<?php\nnamespace Illuminate\\Support\\Facades;\nuse Illuminate\\Events\\Dispatcher;\n/**\n * @method static void listen(\\Closure|string $events, $l = null)\n * @method Dispatcher fake()\n * @property-read string $label\n */\nclass Event {}\n";
        let file = parse_file(Path::new("/f/Event.php"), src);
        let event = &file.classes[0];
        let listen = event.members.iter().find(|m| m.name == "listen").unwrap();
        assert!(listen.is_static);
        assert_eq!(listen.detail, "(\\Closure|string $events, $l = null): void");
        let fake = event.members.iter().find(|m| m.name == "fake").unwrap();
        assert!(!fake.is_static);
        assert_eq!(fake.ty.as_deref(), Some("Illuminate\\Events\\Dispatcher"));
        let label = event.members.iter().find(|m| m.name == "label").unwrap();
        assert_eq!(label.kind, MemberKind::Property);
        // Each one sits on its own tag, not on the class.
        assert!(listen.in_docblock && fake.in_docblock && label.in_docblock);
        assert_eq!((listen.line, listen.column), (4, 23));
        assert_eq!((fake.line, fake.column), (5, 22));
        assert_eq!((label.line, label.column), (6, 26));
    }

    #[test]
    fn resolves_names_like_php() {
        let uses = HashMap::from([("model".to_owned(), "A\\Model".to_owned())]);
        assert_eq!(resolve_name("Model", "App", &uses), "A\\Model");
        assert_eq!(resolve_name("Model\\Sub", "App", &uses), "A\\Model\\Sub");
        assert_eq!(resolve_name("\\Exception", "App", &uses), "Exception");
        assert_eq!(resolve_name("Other", "App", &uses), "App\\Other");
        assert_eq!(resolve_name("namespace\\X", "App", &uses), "App\\X");
        assert_eq!(resolve_name("Other", "", &uses), "Other");
    }

    #[test]
    fn types_skip_scalars_and_arrays() {
        let uses = HashMap::new();
        let ty = |raw| resolve_type(raw, "App", &uses, "App\\Me");
        assert_eq!(ty("?Foo").as_deref(), Some("App\\Foo"));
        assert_eq!(ty("null|Foo").as_deref(), Some("App\\Foo"));
        assert_eq!(ty("self").as_deref(), Some("App\\Me"));
        assert_eq!(ty("$this").as_deref(), Some(STATIC_TYPE));
        assert_eq!(ty("string"), None);
        assert_eq!(ty("Foo[]"), None);
        assert_eq!(
            ty("Collection<int, Foo>").as_deref(),
            Some("App\\Collection")
        );
    }

    #[test]
    fn lists_become_properties_at_their_strings() {
        let src = "<?php\nclass Item\n{\n    protected $fillable = [\n        'name',\n        'price',\n    ];\n\n    protected function casts(): array\n    {\n        return [\n            'price' => 'integer',\n            'qty' => 'integer',\n        ];\n    }\n}\n";
        let file = parse_file(Path::new("Item.php"), src);
        let item = &file.classes[0];
        let find = |name: &str| {
            item.members
                .iter()
                .find(|m| m.name == name && m.kind == MemberKind::Property)
        };
        let name = find("name").expect("fillable entry");
        assert_eq!((name.line, name.column), (4, 9));
        // Listed twice: the first source wins, the cast supplies the type.
        let price = find("price").expect("price");
        assert_eq!((price.line, price.detail.as_str()), (5, "integer"));
        // Only in `casts()`.
        let qty = find("qty").expect("cast only");
        assert_eq!(qty.line, 12);
    }
}
