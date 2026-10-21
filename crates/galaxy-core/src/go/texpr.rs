//! Go type expressions as a tree.
//!
//! [`TypeExpr::parse`] reads the type text kept in `go::parse` declarations
//! (`map[string][]*pkg.T`, `func(int, ...string) error`) with Tree-sitter.
//! Union terms `~int | string` are handled on the text, since the grammar
//! only accepts them inside constraints. A tilde term over a plain name is
//! kept as the name `~int`; over any other type the tilde is dropped.

use std::collections::HashMap;

use tree_sitter::{Node, Parser};

/// Deepest nesting read; anything below becomes [`TypeExpr::Unknown`].
const MAX_DEPTH: usize = 64;

/// Source prefix the type text is parsed after.
const PREFIX: &str = "package p\nvar _ ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChanDir {
    /// `chan T`
    Both,
    /// `chan<- T`
    Send,
    /// `<-chan T`
    Recv,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeExpr {
    /// `T`, `pkg.T`, `T[A, B]`.
    Name {
        pkg: Option<String>,
        name: String,
        args: Vec<TypeExpr>,
    },
    Pointer(Box<TypeExpr>),
    Slice(Box<TypeExpr>),
    /// `[N]T`; `len` is the source text (`...` for `[...]T`).
    Array {
        len: String,
        elem: Box<TypeExpr>,
    },
    Map {
        key: Box<TypeExpr>,
        value: Box<TypeExpr>,
    },
    Chan {
        dir: ChanDir,
        elem: Box<TypeExpr>,
    },
    /// Parameter names are dropped. For a variadic function the last entry
    /// of `params` is the element type.
    Func {
        params: Vec<TypeExpr>,
        results: Vec<TypeExpr>,
        variadic: bool,
    },
    /// `struct{...}`; the members are not kept.
    Struct,
    /// `interface{...}`; the members are not kept.
    Interface,
    Union(Vec<TypeExpr>),
    /// `...T`, as written for a variadic parameter.
    Ellipsis(Box<TypeExpr>),
    Unknown,
}

const BUILTINS: &[&str] = &[
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
    "error",
    "any",
    "comparable",
    "rune",
    "byte",
];

impl TypeExpr {
    /// Reads `text` as a Go type; [`TypeExpr::Unknown`] if it is not one.
    pub fn parse(text: &str) -> TypeExpr {
        parse_at(text, 0)
    }

    /// The name at the end of any number of pointers. `None` for anything
    /// but a [`TypeExpr::Name`].
    pub fn base_name(&self) -> Option<&str> {
        match self.deref() {
            TypeExpr::Name { name, .. } => Some(name),
            _ => None,
        }
    }

    /// Canonical Go text. Structs and interfaces print as `struct` and
    /// `interface`, their members are not kept.
    pub fn display(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    /// The type behind any number of pointers.
    #[allow(clippy::should_implement_trait)]
    pub fn deref(&self) -> &TypeExpr {
        let mut current = self;
        while let TypeExpr::Pointer(inner) = current {
            current = inner;
        }
        current
    }

    /// Whether `name` is a predeclared type of Go.
    pub fn is_builtin(name: &str) -> bool {
        BUILTINS.contains(&name)
    }

    /// Replaces unqualified, argument-less names found in `map` (type
    /// parameters) with their binding.
    pub fn substitute(&self, map: &HashMap<String, TypeExpr>) -> TypeExpr {
        let sub = |t: &TypeExpr| Box::new(t.substitute(map));
        match self {
            TypeExpr::Name { pkg, name, args } => {
                if pkg.is_none()
                    && args.is_empty()
                    && let Some(bound) = map.get(name)
                {
                    return bound.clone();
                }
                TypeExpr::Name {
                    pkg: pkg.clone(),
                    name: name.clone(),
                    args: args.iter().map(|a| a.substitute(map)).collect(),
                }
            }
            TypeExpr::Pointer(t) => TypeExpr::Pointer(sub(t)),
            TypeExpr::Slice(t) => TypeExpr::Slice(sub(t)),
            TypeExpr::Array { len, elem } => TypeExpr::Array {
                len: len.clone(),
                elem: sub(elem),
            },
            TypeExpr::Map { key, value } => TypeExpr::Map {
                key: sub(key),
                value: sub(value),
            },
            TypeExpr::Chan { dir, elem } => TypeExpr::Chan {
                dir: *dir,
                elem: sub(elem),
            },
            TypeExpr::Func {
                params,
                results,
                variadic,
            } => TypeExpr::Func {
                params: params.iter().map(|t| t.substitute(map)).collect(),
                results: results.iter().map(|t| t.substitute(map)).collect(),
                variadic: *variadic,
            },
            TypeExpr::Union(items) => {
                TypeExpr::Union(items.iter().map(|t| t.substitute(map)).collect())
            }
            TypeExpr::Ellipsis(t) => TypeExpr::Ellipsis(sub(t)),
            TypeExpr::Struct | TypeExpr::Interface | TypeExpr::Unknown => self.clone(),
        }
    }

    fn write(&self, out: &mut String) {
        match self {
            TypeExpr::Name { pkg, name, args } => {
                if let Some(pkg) = pkg {
                    out.push_str(pkg);
                    out.push('.');
                }
                out.push_str(name);
                if !args.is_empty() {
                    out.push('[');
                    write_list(args, out);
                    out.push(']');
                }
            }
            TypeExpr::Pointer(t) => {
                out.push('*');
                t.write(out);
            }
            TypeExpr::Slice(t) => {
                out.push_str("[]");
                t.write(out);
            }
            TypeExpr::Array { len, elem } => {
                out.push('[');
                out.push_str(len);
                out.push(']');
                elem.write(out);
            }
            TypeExpr::Map { key, value } => {
                out.push_str("map[");
                key.write(out);
                out.push(']');
                value.write(out);
            }
            TypeExpr::Chan { dir, elem } => {
                out.push_str(match dir {
                    ChanDir::Both => "chan ",
                    ChanDir::Send => "chan<- ",
                    ChanDir::Recv => "<-chan ",
                });
                // `chan (<-chan T)` needs the parentheses to stay a `chan`.
                let wrap = *dir == ChanDir::Both
                    && matches!(
                        **elem,
                        TypeExpr::Chan {
                            dir: ChanDir::Recv,
                            ..
                        }
                    );
                if wrap {
                    out.push('(');
                }
                elem.write(out);
                if wrap {
                    out.push(')');
                }
            }
            TypeExpr::Func {
                params,
                results,
                variadic,
            } => {
                out.push_str("func(");
                for (i, param) in params.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    if *variadic && i + 1 == params.len() {
                        out.push_str("...");
                    }
                    param.write(out);
                }
                out.push(')');
                match results.as_slice() {
                    [] => {}
                    [one] => {
                        out.push(' ');
                        one.write(out);
                    }
                    many => {
                        out.push_str(" (");
                        write_list(many, out);
                        out.push(')');
                    }
                }
            }
            TypeExpr::Struct => out.push_str("struct"),
            TypeExpr::Interface => out.push_str("interface"),
            TypeExpr::Union(items) => {
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(" | ");
                    }
                    item.write(out);
                }
            }
            TypeExpr::Ellipsis(t) => {
                out.push_str("...");
                t.write(out);
            }
            TypeExpr::Unknown => out.push('?'),
        }
    }
}

fn write_list(items: &[TypeExpr], out: &mut String) {
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        item.write(out);
    }
}

fn parse_at(text: &str, depth: usize) -> TypeExpr {
    let text = text.trim();
    if text.is_empty() || depth > MAX_DEPTH {
        return TypeExpr::Unknown;
    }
    if let Some(rest) = text.strip_prefix("...") {
        return TypeExpr::Ellipsis(Box::new(parse_at(rest, depth + 1)));
    }
    if let Some(rest) = text.strip_prefix("[...]") {
        return TypeExpr::Array {
            len: "...".to_owned(),
            elem: Box::new(parse_at(rest, depth + 1)),
        };
    }
    let terms = split_top(text, '|');
    if terms.len() > 1 || text.starts_with('~') {
        let mut items: Vec<TypeExpr> = terms.iter().map(|t| parse_term(t, depth + 1)).collect();
        return if items.len() == 1 {
            items.remove(0)
        } else {
            TypeExpr::Union(items)
        };
    }
    parse_single(text, depth)
}

/// One term of a union: `T` or `~T`.
fn parse_term(term: &str, depth: usize) -> TypeExpr {
    let term = term.trim();
    match term.strip_prefix('~') {
        Some(rest) => match parse_single(rest.trim(), depth) {
            TypeExpr::Name { pkg, name, args } => TypeExpr::Name {
                pkg,
                name: format!("~{name}"),
                args,
            },
            other => other,
        },
        None => parse_at(term, depth),
    }
}

/// Splits at `sep` outside brackets, parentheses and braces.
fn split_top(text: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in text.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            c if c == sep && depth == 0 => {
                parts.push(&text[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

fn parse_single(text: &str, depth: usize) -> TypeExpr {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .is_err()
    {
        return TypeExpr::Unknown;
    }
    let src = format!("{PREFIX}{text}\n");
    let Some(tree) = parser.parse(&src, None) else {
        return TypeExpr::Unknown;
    };
    let root = tree.root_node();
    let mut cursor = root.walk();
    let spec = root
        .named_children(&mut cursor)
        .find(|n| n.kind() == "var_declaration")
        .and_then(|decl| {
            let mut c = decl.walk();
            decl.named_children(&mut c).find(|n| n.kind() == "var_spec")
        });
    let Some(node) = spec.and_then(|s| s.child_by_field_name("type")) else {
        return TypeExpr::Unknown;
    };
    // The whole text must be the type; trailing garbage is not.
    if node.start_byte() != PREFIX.len() || node.end_byte() != PREFIX.len() + text.len() {
        return TypeExpr::Unknown;
    }
    convert(node, &src, depth)
}

fn text<'a>(node: Node<'_>, src: &'a str) -> &'a str {
    src.get(node.byte_range()).unwrap_or("")
}

fn field(node: Node<'_>, name: &str, src: &str, depth: usize) -> Box<TypeExpr> {
    Box::new(
        node.child_by_field_name(name)
            .map_or(TypeExpr::Unknown, |n| convert(n, src, depth + 1)),
    )
}

fn convert(node: Node<'_>, src: &str, depth: usize) -> TypeExpr {
    if depth > MAX_DEPTH || node.is_error() || node.is_missing() {
        return TypeExpr::Unknown;
    }
    match node.kind() {
        "type_identifier" => TypeExpr::Name {
            pkg: None,
            name: text(node, src).to_owned(),
            args: Vec::new(),
        },
        "qualified_type" => {
            let (Some(pkg), Some(name)) = (
                node.child_by_field_name("package"),
                node.child_by_field_name("name"),
            ) else {
                return TypeExpr::Unknown;
            };
            TypeExpr::Name {
                pkg: Some(text(pkg, src).to_owned()),
                name: text(name, src).to_owned(),
                args: Vec::new(),
            }
        }
        "generic_type" => {
            let base = field(node, "type", src, depth);
            let Some(arguments) = node.child_by_field_name("type_arguments") else {
                return TypeExpr::Unknown;
            };
            let mut cursor = arguments.walk();
            let args: Vec<TypeExpr> = arguments
                .named_children(&mut cursor)
                .filter(|n| n.kind() == "type_elem")
                .map(|n| convert_elem(n, src, depth + 1))
                .collect();
            match *base {
                TypeExpr::Name { pkg, name, .. } => TypeExpr::Name { pkg, name, args },
                _ => TypeExpr::Unknown,
            }
        }
        "pointer_type" => match node.named_child(0) {
            Some(inner) => TypeExpr::Pointer(Box::new(convert(inner, src, depth + 1))),
            None => TypeExpr::Unknown,
        },
        "parenthesized_type" => node
            .named_child(0)
            .map_or(TypeExpr::Unknown, |n| convert(n, src, depth + 1)),
        "slice_type" => TypeExpr::Slice(field(node, "element", src, depth)),
        "array_type" => TypeExpr::Array {
            len: node
                .child_by_field_name("length")
                .map_or_else(String::new, |n| text(n, src).to_owned()),
            elem: field(node, "element", src, depth),
        },
        "implicit_length_array_type" => TypeExpr::Array {
            len: "...".to_owned(),
            elem: field(node, "element", src, depth),
        },
        "map_type" => TypeExpr::Map {
            key: field(node, "key", src, depth),
            value: field(node, "value", src, depth),
        },
        "channel_type" => {
            let mut chan_seen = false;
            let mut dir = ChanDir::Both;
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.kind() {
                    "chan" => chan_seen = true,
                    "<-" => {
                        dir = if chan_seen {
                            ChanDir::Send
                        } else {
                            ChanDir::Recv
                        }
                    }
                    _ => {}
                }
            }
            TypeExpr::Chan {
                dir,
                elem: field(node, "value", src, depth),
            }
        }
        "function_type" => {
            let (params, variadic) = node
                .child_by_field_name("parameters")
                .map_or((Vec::new(), false), |n| param_types(n, src, depth + 1));
            let results = match node.child_by_field_name("result") {
                Some(n) if n.kind() == "parameter_list" => param_types(n, src, depth + 1).0,
                Some(n) => vec![convert(n, src, depth + 1)],
                None => Vec::new(),
            };
            TypeExpr::Func {
                params,
                results,
                variadic,
            }
        }
        "struct_type" => TypeExpr::Struct,
        "interface_type" => TypeExpr::Interface,
        "negated_type" => match node.named_child(0).map(|n| convert(n, src, depth + 1)) {
            Some(TypeExpr::Name { pkg, name, args }) => TypeExpr::Name {
                pkg,
                name: format!("~{name}"),
                args,
            },
            Some(other) => other,
            None => TypeExpr::Unknown,
        },
        _ => TypeExpr::Unknown,
    }
}

/// A `type_elem`: one type, or a union of terms.
fn convert_elem(node: Node<'_>, src: &str, depth: usize) -> TypeExpr {
    let mut cursor = node.walk();
    let mut items: Vec<TypeExpr> = node
        .named_children(&mut cursor)
        .filter(|n| n.kind() != "comment")
        .map(|n| convert(n, src, depth + 1))
        .collect();
    match items.len() {
        0 => TypeExpr::Unknown,
        1 => items.remove(0),
        _ => TypeExpr::Union(items),
    }
}

/// Types of a parameter list, one per name (`a, b int` gives two), and
/// whether the last one is variadic.
fn param_types(list: Node<'_>, src: &str, depth: usize) -> (Vec<TypeExpr>, bool) {
    let mut out = Vec::new();
    let mut variadic = false;
    let mut cursor = list.walk();
    for child in list.named_children(&mut cursor) {
        let kind = child.kind();
        if kind != "parameter_declaration" && kind != "variadic_parameter_declaration" {
            continue;
        }
        let ty = child
            .child_by_field_name("type")
            .map_or(TypeExpr::Unknown, |n| convert(n, src, depth + 1));
        let mut names = child.walk();
        let count = child
            .children_by_field_name("name", &mut names)
            .filter(|n| n.kind() == "identifier")
            .count()
            .max(1);
        variadic = kind == "variadic_parameter_declaration";
        for _ in 0..count {
            out.push(ty.clone());
        }
    }
    (out, variadic)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(n: &str) -> TypeExpr {
        TypeExpr::Name {
            pkg: None,
            name: n.to_owned(),
            args: Vec::new(),
        }
    }

    #[test]
    fn plain_and_qualified_names() {
        assert_eq!(TypeExpr::parse("int"), name("int"));
        assert_eq!(
            TypeExpr::parse(" http.Request "),
            TypeExpr::Name {
                pkg: Some("http".into()),
                name: "Request".into(),
                args: vec![]
            }
        );
    }

    #[test]
    fn generic_arguments() {
        let t = TypeExpr::parse("pkg.T[int, []string]");
        assert_eq!(
            t,
            TypeExpr::Name {
                pkg: Some("pkg".into()),
                name: "T".into(),
                args: vec![name("int"), TypeExpr::Slice(Box::new(name("string")))],
            }
        );
        assert_eq!(t.display(), "pkg.T[int, []string]");
    }

    #[test]
    fn pointer_slice_map_nesting() {
        let t = TypeExpr::parse("map[string][]*pkg.T");
        let TypeExpr::Map { key, value } = &t else {
            panic!("not a map: {t:?}");
        };
        assert_eq!(**key, name("string"));
        let TypeExpr::Slice(elem) = &**value else {
            panic!("not a slice");
        };
        assert_eq!(elem.base_name(), Some("T"));
        assert_eq!(t.display(), "map[string][]*pkg.T");
    }

    #[test]
    fn arrays() {
        assert_eq!(
            TypeExpr::parse("[3]int"),
            TypeExpr::Array {
                len: "3".into(),
                elem: Box::new(name("int"))
            }
        );
        let implicit = TypeExpr::parse("[...]int");
        assert_eq!(implicit.display(), "[...]int");
        assert_eq!(TypeExpr::parse("[N*2]byte").display(), "[N*2]byte");
    }

    #[test]
    fn channel_directions() {
        for (text, dir) in [
            ("chan int", ChanDir::Both),
            ("chan<- int", ChanDir::Send),
            ("<-chan int", ChanDir::Recv),
        ] {
            let t = TypeExpr::parse(text);
            assert!(
                matches!(&t, TypeExpr::Chan { dir: d, .. } if *d == dir),
                "{text}"
            );
            assert_eq!(t.display(), text);
        }
        assert_eq!(
            TypeExpr::parse("chan (<-chan int)").display(),
            "chan (<-chan int)"
        );
    }

    #[test]
    fn func_types() {
        let t = TypeExpr::parse("func(a, b int, rest ...string) (int, error)");
        assert_eq!(
            t,
            TypeExpr::Func {
                params: vec![name("int"), name("int"), name("string")],
                results: vec![name("int"), name("error")],
                variadic: true,
            }
        );
        assert_eq!(t.display(), "func(int, int, ...string) (int, error)");
        assert_eq!(TypeExpr::parse("func()").display(), "func()");
        assert_eq!(
            TypeExpr::parse("func(int) error").display(),
            "func(int) error"
        );
    }

    #[test]
    fn struct_and_interface() {
        assert_eq!(TypeExpr::parse("struct{ a int }"), TypeExpr::Struct);
        assert_eq!(TypeExpr::parse("interface{}"), TypeExpr::Interface);
        assert_eq!(TypeExpr::parse("*struct{}").display(), "*struct");
    }

    #[test]
    fn unions_and_tilde() {
        let t = TypeExpr::parse("~int | string");
        assert_eq!(t, TypeExpr::Union(vec![name("~int"), name("string")]));
        assert_eq!(t.display(), "~int | string");
        assert_eq!(TypeExpr::parse("~float64"), name("~float64"));
        let nested = TypeExpr::parse("Set[~int | string]");
        assert_eq!(nested.display(), "Set[~int | string]");
    }

    #[test]
    fn ellipsis() {
        let t = TypeExpr::parse("...int");
        assert_eq!(t, TypeExpr::Ellipsis(Box::new(name("int"))));
        assert_eq!(t.display(), "...int");
    }

    #[test]
    fn invalid_text_is_unknown() {
        for text in ["", "[]", "int string", "map[", "1 +", "func(", "int\nvar x"] {
            assert_eq!(TypeExpr::parse(text), TypeExpr::Unknown, "{text:?}");
        }
        assert_eq!(TypeExpr::Unknown.display(), "?");
    }

    #[test]
    fn deref_and_base_name() {
        let t = TypeExpr::parse("**pkg.Server");
        assert_eq!(t.base_name(), Some("Server"));
        assert_eq!(t.deref(), &TypeExpr::parse("pkg.Server"));
        assert_eq!(TypeExpr::parse("[]int").base_name(), None);
        assert_eq!(TypeExpr::parse("*[]int").base_name(), None);
    }

    #[test]
    fn builtin_names() {
        for n in [
            "bool",
            "string",
            "int",
            "int8",
            "uint64",
            "uintptr",
            "float32",
            "float64",
            "complex64",
            "complex128",
            "error",
            "any",
            "comparable",
            "rune",
            "byte",
        ] {
            assert!(TypeExpr::is_builtin(n), "{n}");
        }
        assert!(!TypeExpr::is_builtin("Server"));
        assert!(!TypeExpr::is_builtin("String"));
    }

    #[test]
    fn substitution_replaces_type_parameters() {
        let t = TypeExpr::parse("map[K][]*List[V]");
        let mut map = HashMap::new();
        map.insert("K".to_owned(), TypeExpr::parse("string"));
        map.insert("V".to_owned(), TypeExpr::parse("pkg.Item"));
        assert_eq!(t.substitute(&map).display(), "map[string][]*List[pkg.Item]");
        // A qualified name is never a type parameter.
        let q = TypeExpr::parse("pkg.K");
        assert_eq!(q.substitute(&map), q);
        let f = TypeExpr::parse("func(K) V");
        assert_eq!(f.substitute(&map).display(), "func(string) pkg.Item");
    }

    #[test]
    fn deep_nesting_does_not_overflow() {
        let text = format!("{}int", "[]".repeat(500));
        let _ = TypeExpr::parse(&text);
        let text = format!("{}int", "...".repeat(500));
        let _ = TypeExpr::parse(&text);
        let text = format!("{}int{}", "(".repeat(500), ")".repeat(500));
        let _ = TypeExpr::parse(&text);
    }
}
