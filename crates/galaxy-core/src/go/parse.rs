//! Declarations of one Go file, read with Tree-sitter.
//!
//! Only what navigation and completion need: package, imports, types and
//! their fields or interface methods, functions and methods, constants and
//! variables, each with its doc comment and position. Function bodies are
//! not analysed. Lines are 0-based and columns are UTF-16 code units, as
//! the editor counts them.

use std::path::{Path, PathBuf};

use tree_sitter::{Parser, Tree};

/// Deepest `ERROR` nesting searched for declarations.
const MAX_DEPTH: usize = 64;

/// Node kinds [`identifier_at`] accepts.
const IDENT_KINDS: &[&str] = &[
    "identifier",
    "field_identifier",
    "type_identifier",
    "package_identifier",
    "label_name",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoFile {
    pub path: PathBuf,
    pub package: String,
    pub imports: Vec<Import>,
    pub types: Vec<TypeDecl>,
    pub funcs: Vec<FuncDecl>,
    pub consts: Vec<ValueDecl>,
    pub vars: Vec<ValueDecl>,
    /// Text after `//go:build`.
    pub build_constraint: Option<String>,
    pub package_doc: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// `"."`, `"_"` or the name.
    pub alias: Option<String>,
    /// Unquoted.
    pub path: String,
    pub line: u32,
    pub column: u32,
    /// End of the whole spec.
    pub end_column: u32,
    /// Start of the string literal.
    pub path_column: u32,
    pub path_end_column: u32,
}

/// Position of a name identifier.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pos {
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeKind {
    Struct,
    Interface,
    /// `type A = B`
    Alias,
    /// `type A B`
    Named,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeParam {
    pub name: String,
    pub constraint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// For an embedded field, the base name of its type.
    pub name: String,
    /// Normalized type text.
    pub ty: String,
    /// Unquoted struct tag.
    pub tag: Option<String>,
    pub embedded: bool,
    pub doc: String,
    pub pos: Pos,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfaceMethod {
    pub name: String,
    /// `(a int) (string, error)`
    pub signature: String,
    pub doc: String,
    pub pos: Pos,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDecl {
    pub name: String,
    pub kind: TypeKind,
    pub type_params: Vec<TypeParam>,
    pub fields: Vec<Field>,
    pub methods: Vec<IfaceMethod>,
    /// Embedded interfaces and constraint unions, as text.
    pub embeds: Vec<String>,
    /// The type text for an alias or named type; `struct` or `interface`.
    pub underlying: String,
    pub doc: String,
    pub pos: Pos,
    pub end_line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    /// Empty if unnamed.
    pub name: String,
    /// The element type for a variadic parameter.
    pub ty: String,
    pub variadic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receiver {
    pub name: String,
    /// `*Server[T]`
    pub ty: String,
    /// `Server`
    pub base: String,
    pub pointer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncDecl {
    pub name: String,
    pub receiver: Option<Receiver>,
    pub type_params: Vec<TypeParam>,
    pub params: Vec<Param>,
    pub results: Vec<Param>,
    pub doc: String,
    pub pos: Pos,
    /// First and last line of the body block.
    pub body: Option<(u32, u32)>,
    pub end_line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueDecl {
    pub name: String,
    pub ty: Option<String>,
    /// Source text of the initializer; an `iota` group member without one
    /// inherits nothing here.
    pub value: Option<String>,
    pub doc: String,
    pub pos: Pos,
    /// Type carried over from an earlier spec of a `const ( ... )` group
    /// when this spec has neither type nor value.
    pub group_type: Option<String>,
    pub is_const: bool,
}

/// The identifier under or just before a cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentAt {
    pub name: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
}

/// Whether `name` starts with an upper-case letter, i.e. is exported.
pub fn exported(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}

/// UTF-16 column for a byte column of `line_text`. A byte column inside a
/// character, or past the end, is clamped.
pub fn utf16_col(line_text: &str, byte_col: usize) -> u32 {
    let mut end = byte_col.min(line_text.len());
    while !line_text.is_char_boundary(end) {
        end -= 1;
    }
    line_text[..end].encode_utf16().count() as u32
}

/// Reads the declarations of `src`; an empty file with `path` if the parser
/// gives nothing.
pub fn parse_file(path: &Path, src: &str) -> GoFile {
    parse(path, src)
        .map(|(file, _)| file)
        .unwrap_or_else(|| GoFile {
            path: path.to_path_buf(),
            ..GoFile::default()
        })
}

/// Like [`parse_file`], and hands back the syntax tree for further lookups.
/// Syntax errors do not stop it; whatever Tree-sitter recovers is read.
pub fn parse(path: &Path, src: &str) -> Option<(GoFile, Tree)> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_go::LANGUAGE.into()).ok()?;
    let tree = parser.parse(src, None)?;
    let file = read::read(path, src, tree.root_node());
    Some((file, tree))
}

/// The identifier under the cursor, else the one ending right at it.
/// `column_utf16` counts UTF-16 code units.
pub fn identifier_at(tree: &Tree, src: &str, line: u32, column_utf16: u32) -> Option<IdentAt> {
    let at = byte_offset(src, line, column_utf16)?;
    let root = tree.root_node();
    let mut ranges = Vec::with_capacity(2);
    if let Some(next) = src.get(at..).and_then(|rest| rest.chars().next()) {
        ranges.push((at, at + next.len_utf8()));
    }
    if let Some((prev, _)) = src
        .get(..at)
        .and_then(|head| head.char_indices().next_back())
    {
        ranges.push((prev, at));
    }
    for (start, end) in ranges {
        let Some(node) = root.descendant_for_byte_range(start, end) else {
            continue;
        };
        if !IDENT_KINDS.contains(&node.kind()) {
            continue;
        }
        let name = src.get(node.byte_range())?.to_owned();
        let begin = node.start_position();
        let finish = node.end_position();
        let line_of = |byte: usize, col: usize| {
            let line_start = byte.saturating_sub(col);
            utf16_col(src.get(line_start..).unwrap_or(""), col)
        };
        return Some(IdentAt {
            name,
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            line: begin.row as u32,
            column: line_of(node.start_byte(), begin.column),
            end_column: line_of(node.end_byte(), finish.column),
        });
    }
    None
}

/// Byte offset of a 0-based line and UTF-16 column; the column is clamped
/// to the end of the line.
fn byte_offset(src: &str, line: u32, column: u32) -> Option<usize> {
    let mut start = 0;
    for _ in 0..line {
        start += src.get(start..)?.find('\n')? + 1;
    }
    let rest = src.get(start..)?;
    let text = &rest[..rest.find('\n').unwrap_or(rest.len())];
    let mut units = 0u32;
    for (index, c) in text.char_indices() {
        if units >= column {
            return Some(start + index);
        }
        units += c.len_utf16() as u32;
    }
    Some(start + text.len())
}

mod read;

#[cfg(test)]
mod tests {
    use super::*;

    fn go(src: &str) -> GoFile {
        parse_file(Path::new("x.go"), src)
    }

    fn type_of<'a>(file: &'a GoFile, name: &str) -> &'a TypeDecl {
        file.types.iter().find(|t| t.name == name).expect(name)
    }

    fn func_of<'a>(file: &'a GoFile, name: &str) -> &'a FuncDecl {
        file.funcs.iter().find(|f| f.name == name).expect(name)
    }

    #[test]
    fn package_and_doc() {
        let file = go("// Package x does things.\n//\n// More.\npackage x\n");
        assert_eq!(file.package, "x");
        assert_eq!(file.package_doc, "Package x does things.\n\nMore.");
        assert_eq!(file.path, Path::new("x.go"));
    }

    #[test]
    fn build_constraint_is_not_package_doc() {
        let file = go("//go:build linux && amd64\n\n// Doc.\npackage x\n");
        assert_eq!(file.build_constraint.as_deref(), Some("linux && amd64"));
        assert_eq!(file.package_doc, "Doc.");
        assert_eq!(go("package x\n").build_constraint, None);
    }

    #[test]
    fn imports_with_alias_and_columns() {
        let src = "package x\n\nimport (\n\tf \"fmt\"\n\t_ \"a/b\"\n\t. \"c\"\n\t\"os\"\n)\nimport `raw/path`\n";
        let file = go(src);
        let got: Vec<(Option<&str>, &str)> = file
            .imports
            .iter()
            .map(|i| (i.alias.as_deref(), i.path.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (Some("f"), "fmt"),
                (Some("_"), "a/b"),
                (Some("."), "c"),
                (None, "os"),
                (None, "raw/path"),
            ]
        );
        let first = &file.imports[0];
        assert_eq!((first.line, first.column, first.end_column), (3, 1, 8));
        assert_eq!((first.path_column, first.path_end_column), (3, 8));
        let plain = &file.imports[3];
        assert_eq!((plain.column, plain.path_column), (1, 1));
        let single = &file.imports[4];
        assert_eq!((single.line, single.column, single.path_column), (8, 7, 7));
    }

    #[test]
    fn struct_fields_names_tags_docs() {
        let src = "package x\n\n// User is a user.\ntype User struct {\n\t// Name doc.\n\tName, Nick string `json:\"name\"`\n\tage int\n\t_ [4]byte\n\tTags map[string][]*pkg.T\n\tOn chan<- int\n\tCb func(int) error\n}\n";
        let file = go(src);
        let user = type_of(&file, "User");
        assert_eq!(user.kind, TypeKind::Struct);
        assert_eq!(user.underlying, "struct");
        assert_eq!(user.doc, "User is a user.");
        assert_eq!(
            (user.pos.line, user.pos.column, user.pos.end_column),
            (3, 5, 9)
        );
        assert_eq!(user.end_line, 11);
        let names: Vec<&str> = user.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["Name", "Nick", "age", "Tags", "On", "Cb"]);
        assert_eq!(user.fields[0].ty, "string");
        assert_eq!(user.fields[0].tag.as_deref(), Some("json:\"name\""));
        assert_eq!(user.fields[1].tag.as_deref(), Some("json:\"name\""));
        assert_eq!(user.fields[0].doc, "Name doc.");
        assert_eq!(user.fields[1].doc, "Name doc.");
        assert_eq!(user.fields[2].tag, None);
        assert_eq!(user.fields[2].doc, "");
        assert_eq!(user.fields[3].ty, "map[string][]*pkg.T");
        assert_eq!(user.fields[4].ty, "chan<- int");
        assert_eq!(user.fields[5].ty, "func(int) error");
        assert_eq!((user.fields[0].pos.line, user.fields[0].pos.column), (5, 1));
        assert_eq!(user.fields[1].pos.column, 7);
    }

    #[test]
    fn embedded_fields() {
        let src = "package x\n\ntype A struct {\n\tBase\n\t*Ptr\n\tpkg.Qual\n\t*pkg.QPtr\n\tGen[int]\n\tNamed int\n}\n";
        let file = go(src);
        let fields = &type_of(&file, "A").fields;
        let got: Vec<(&str, &str, bool)> = fields
            .iter()
            .map(|f| (f.name.as_str(), f.ty.as_str(), f.embedded))
            .collect();
        assert_eq!(
            got,
            [
                ("Base", "Base", true),
                ("Ptr", "*Ptr", true),
                ("Qual", "pkg.Qual", true),
                ("QPtr", "*pkg.QPtr", true),
                ("Gen", "Gen[int]", true),
                ("Named", "int", false),
            ]
        );
        // The position is that of the base name.
        assert_eq!((fields[2].pos.column, fields[2].pos.end_column), (5, 9));
    }

    #[test]
    fn interface_methods_and_embeds() {
        let src = "package x\n\ntype I interface {\n\tfmt.Stringer\n\t~int | string\n\t// Do does.\n\tDo(a int, b ...string) (string, error)\n\tClose() error\n\tNone()\n}\n";
        let file = go(src);
        let iface = type_of(&file, "I");
        assert_eq!(iface.kind, TypeKind::Interface);
        assert_eq!(iface.underlying, "interface");
        assert_eq!(iface.embeds, ["fmt.Stringer", "~int | string"]);
        let sigs: Vec<(&str, &str)> = iface
            .methods
            .iter()
            .map(|m| (m.name.as_str(), m.signature.as_str()))
            .collect();
        assert_eq!(
            sigs,
            [
                ("Do", "(a int, b ...string) (string, error)"),
                ("Close", "() error"),
                ("None", "()"),
            ]
        );
        assert_eq!(iface.methods[0].doc, "Do does.");
        assert_eq!(iface.methods[0].pos.line, 6);
        assert_eq!(iface.methods[0].pos.column, 1);
    }

    #[test]
    fn alias_and_named_types() {
        let src = "package x\n\ntype B = map[string]int\ntype C []string\ntype D func(int) error\ntype _ int\n";
        let file = go(src);
        let b = type_of(&file, "B");
        assert_eq!(
            (b.kind, b.underlying.as_str()),
            (TypeKind::Alias, "map[string]int")
        );
        let c = type_of(&file, "C");
        assert_eq!(
            (c.kind, c.underlying.as_str()),
            (TypeKind::Named, "[]string")
        );
        assert_eq!(type_of(&file, "D").underlying, "func(int) error");
        assert_eq!(file.types.len(), 3, "blank type skipped");
    }

    #[test]
    fn generic_type_params() {
        let src = "package x\n\ntype Set[K comparable, V any] map[K]V\ntype Num[T ~int | ~float64] struct{ v T }\ntype Pair[A, B any] struct{}\n";
        let file = go(src);
        let tp = |name: &str| -> Vec<(String, String)> {
            type_of(&file, name)
                .type_params
                .iter()
                .map(|p| (p.name.clone(), p.constraint.clone()))
                .collect()
        };
        assert_eq!(
            tp("Set"),
            [
                ("K".into(), "comparable".into()),
                ("V".into(), "any".into())
            ]
        );
        assert_eq!(tp("Num"), [("T".into(), "~int | ~float64".into())]);
        assert_eq!(
            tp("Pair"),
            [("A".into(), "any".into()), ("B".into(), "any".into())]
        );
    }

    #[test]
    fn grouped_types_use_group_doc() {
        let src = "package x\n\n// Group doc.\ntype (\n\tA int\n\t// B doc.\n\tB string\n)\n";
        let file = go(src);
        assert_eq!(type_of(&file, "A").doc, "Group doc.");
        assert_eq!(type_of(&file, "B").doc, "B doc.");
    }

    #[test]
    fn function_params_and_results() {
        let src = "package x\n\n// Add adds.\nfunc Add(a, b int, rest ...string) (n int, err error) {\n\treturn\n}\nfunc one(int, string) bool { return true }\nfunc none() {}\n";
        let file = go(src);
        let add = func_of(&file, "Add");
        assert_eq!(add.doc, "Add adds.");
        assert_eq!(add.receiver, None);
        let params: Vec<(&str, &str, bool)> = add
            .params
            .iter()
            .map(|p| (p.name.as_str(), p.ty.as_str(), p.variadic))
            .collect();
        assert_eq!(
            params,
            [
                ("a", "int", false),
                ("b", "int", false),
                ("rest", "string", true)
            ]
        );
        let results: Vec<(&str, &str)> = add
            .results
            .iter()
            .map(|p| (p.name.as_str(), p.ty.as_str()))
            .collect();
        assert_eq!(results, [("n", "int"), ("err", "error")]);
        assert_eq!(add.body, Some((3, 5)));
        assert_eq!(add.end_line, 5);
        assert_eq!(
            (add.pos.line, add.pos.column, add.pos.end_column),
            (3, 5, 8)
        );
        let one = func_of(&file, "one");
        assert_eq!(one.params.len(), 2);
        assert_eq!(one.params[0].name, "");
        assert_eq!(one.params[1].ty, "string");
        assert_eq!(one.results.len(), 1);
        assert_eq!(one.results[0].ty, "bool");
        assert!(func_of(&file, "none").results.is_empty());
    }

    #[test]
    fn generic_function_and_receiver() {
        let src = "package x\n\nfunc Map[T, U any](xs []T, f func(T) U) []U { return nil }\nfunc (s *Server[T]) Do() {}\nfunc (Plain) Val() {}\nfunc (c Conn) Close() error { return nil }\n";
        let file = go(src);
        let map = func_of(&file, "Map");
        assert_eq!(map.type_params.len(), 2);
        assert_eq!(map.type_params[1].constraint, "any");
        assert_eq!(map.params[1].ty, "func(T) U");
        assert_eq!(map.results[0].ty, "[]U");
        let recv = func_of(&file, "Do").receiver.clone().expect("receiver");
        assert_eq!(
            (
                recv.name.as_str(),
                recv.ty.as_str(),
                recv.base.as_str(),
                recv.pointer
            ),
            ("s", "*Server[T]", "Server", true)
        );
        let plain = func_of(&file, "Val").receiver.clone().expect("receiver");
        assert_eq!(
            (
                plain.name.as_str(),
                plain.ty.as_str(),
                plain.base.as_str(),
                plain.pointer
            ),
            ("", "Plain", "Plain", false)
        );
        let conn = func_of(&file, "Close").receiver.clone().expect("receiver");
        assert_eq!((conn.base.as_str(), conn.pointer), ("Conn", false));
    }

    #[test]
    fn function_without_body() {
        let file = go("package x\n\nfunc asm(x int) int\n\nfunc next() {}\n");
        let asm = func_of(&file, "asm");
        assert_eq!(asm.body, None);
        assert_eq!(asm.end_line, 2);
        assert_eq!(func_of(&file, "next").body, Some((4, 4)));
    }

    #[test]
    fn const_iota_group() {
        let src = "package x\n\nconst (\n\tA Color = iota\n\tB\n\tC\n\tD, E = 1, 2\n\tF\n)\n";
        let file = go(src);
        let by = |n: &str| file.consts.iter().find(|c| c.name == n).expect(n);
        assert_eq!(by("A").ty.as_deref(), Some("Color"));
        assert_eq!(by("A").value.as_deref(), Some("iota"));
        assert_eq!(by("A").group_type, None);
        assert!(by("A").is_const);
        assert_eq!((by("B").ty.clone(), by("B").value.clone()), (None, None));
        assert_eq!(by("B").group_type.as_deref(), Some("Color"));
        assert_eq!(by("C").group_type.as_deref(), Some("Color"));
        assert_eq!(by("D").value.as_deref(), Some("1"));
        assert_eq!(by("E").value.as_deref(), Some("2"));
        assert_eq!(by("D").group_type, None);
        // `D, E = 1, 2` has no type, so nothing is carried past it.
        assert_eq!(by("F").group_type, None);
    }

    #[test]
    fn vars_values_and_blanks() {
        let src = "package x\n\nvar v, w = 1, 2\nvar _ = f()\nvar (\n\ta, b = pair()\n\tc int\n)\nvar _ = 1\n";
        let file = go(src);
        let names: Vec<&str> = file.vars.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["v", "w", "a", "b", "c"]);
        assert_eq!(file.vars[0].value.as_deref(), Some("1"));
        assert_eq!(file.vars[1].value.as_deref(), Some("2"));
        assert_eq!(file.vars[2].value.as_deref(), Some("pair()"));
        assert_eq!(file.vars[3].value.as_deref(), Some("pair()"));
        assert_eq!(file.vars[4].ty.as_deref(), Some("int"));
        assert_eq!(file.vars[4].value, None);
        assert!(!file.vars[0].is_const);
        assert_eq!((file.vars[0].pos.line, file.vars[0].pos.column), (2, 4));
    }

    #[test]
    fn value_docs_with_group_fallback() {
        let src = "package x\n\n// Limits.\nconst (\n\tMin = 1\n\t// Max is high.\n\tMax = 9\n)\n\n// Name is a name.\nvar Name = \"n\"\n";
        let file = go(src);
        assert_eq!(file.consts[0].doc, "Limits.");
        assert_eq!(file.consts[1].doc, "Max is high.");
        assert_eq!(file.vars[0].doc, "Name is a name.");
    }

    #[test]
    fn block_comment_docs() {
        let src = "package x\n\n/*\n * Run runs.\n *\n * Slowly.\n */\nfunc Run() {}\n\n/** Stop stops. */\nfunc Stop() {}\n";
        let file = go(src);
        assert_eq!(func_of(&file, "Run").doc, "Run runs.\n\nSlowly.");
        assert_eq!(func_of(&file, "Stop").doc, "Stop stops.");
    }

    #[test]
    fn doc_needs_adjacency_and_skips_directives() {
        let src = "package x\n\n// Detached.\n\n//go:noinline\n// Kept.\nfunc A() {}\n\nvar t = 1 // trailing\nfunc B() {}\n\n// Third.\n\nfunc C() {}\n";
        let file = go(src);
        assert_eq!(func_of(&file, "A").doc, "Kept.");
        assert_eq!(func_of(&file, "B").doc, "");
        assert_eq!(func_of(&file, "C").doc, "");
    }

    #[test]
    fn utf16_columns() {
        // The emoji is two UTF-16 units and four bytes.
        let src = "package x\n\nvar s = \"\u{1F600}\"; var Ünï = 1\n";
        let file = go(src);
        let v = file.vars.iter().find(|v| v.name == "Ünï").expect("var");
        assert_eq!(v.pos.line, 2);
        // `var s = "` is 9 units, the emoji 2, `"; var ` 7 more.
        assert_eq!(v.pos.column, 18);
        assert_eq!(v.pos.end_column, 21);
    }

    #[test]
    fn utf16_helper() {
        assert_eq!(utf16_col("abc", 2), 2);
        assert_eq!(utf16_col("\u{1F600}x", 4), 2);
        assert_eq!(utf16_col("\u{e9}x", 2), 1);
        // Inside a character: clamped back to its start.
        assert_eq!(utf16_col("\u{1F600}x", 2), 0);
        assert_eq!(utf16_col("ab", 99), 2);
        assert_eq!(utf16_col("", 0), 0);
    }

    #[test]
    fn exported_names() {
        assert!(exported("Foo"));
        assert!(exported("\u{c9}tat"));
        assert!(!exported("foo"));
        assert!(!exported("_Foo"));
        assert!(!exported(""));
        assert!(!exported("\u{e9}tat"));
    }

    #[test]
    fn multiline_types_are_normalized() {
        let src = "package x\n\ntype H struct {\n\tFn func(\n\t\ta int,\n\t\tb string,\n\t) error\n\tM  map[string]\t[]int\n}\n";
        let file = go(src);
        let fields = &type_of(&file, "H").fields;
        assert_eq!(fields[0].ty, "func(a int, b string,) error");
        assert_eq!(fields[1].ty, "map[string] []int");
    }

    #[test]
    fn comments_inside_types_are_dropped() {
        let src = "package x\n\ntype T struct {\n\tIn struct { // note\n\t\ta int // trailing\n\t\t/* block */ b string\n\t}\n}\n";
        let file = go(src);
        let fields = &type_of(&file, "T").fields;
        assert_eq!(fields[0].ty, "struct { a int b string }");
        assert_eq!(
            crate::go::TypeExpr::parse(&fields[0].ty),
            crate::go::TypeExpr::Struct
        );
    }

    #[test]
    fn syntax_errors_keep_what_parses() {
        let src = "package x\n\nfunc broken() {\n\tx := \n}\n\nfunc Good() int { return 1 }\n\ntype T struct { a int }\nvar ok = 1\n@@@\n";
        let file = go(src);
        assert_eq!(file.package, "x");
        assert!(file.funcs.iter().any(|f| f.name == "Good"));
        assert!(file.types.iter().any(|t| t.name == "T"));
        assert!(file.vars.iter().any(|v| v.name == "ok"));
    }

    #[test]
    fn garbage_never_panics() {
        for src in [
            "",
            "\n\n",
            "func",
            "package",
            "type",
            "type A",
            "type A struct {",
            "const (",
            "var x, y",
            "import (",
            "func (",
            "func (s *) F()",
            "type X interface { M(",
            "\u{1F600}\u{1F600}",
            "package \u{e9}\nfunc \u{e9}() {}",
            "type A[",
            "type A[T",
            "var _ = [",
        ] {
            let file = go(src);
            let _ = file.types.len() + file.funcs.len();
        }
    }

    #[test]
    fn deep_nesting_is_safe() {
        let open = "{".repeat(5000);
        let src = format!("package x\n\nfunc F() {{ {open}\n\nfunc G() {{}}\n");
        let file = go(&src);
        assert_eq!(file.package, "x");
        let parens = "(".repeat(5000);
        let _ = go(&format!("package x\nvar a = {parens}\n"));
        let types = "[]".repeat(5000);
        let _ = go(&format!("package x\nvar a {types}int\n"));
        let errors = "func ".repeat(3000);
        let _ = go(&format!("package x\n{errors}"));
    }

    #[test]
    fn parse_returns_tree_and_default_on_empty() {
        let (file, tree) = parse(Path::new("a.go"), "package a\n").expect("parsed");
        assert_eq!(file.package, "a");
        assert_eq!(tree.root_node().kind(), "source_file");
        let empty = parse_file(Path::new("e.go"), "");
        assert_eq!(empty.package, "");
        assert_eq!(empty.path, Path::new("e.go"));
        assert!(empty.types.is_empty());
    }

    #[test]
    fn identifier_at_finds_name_under_or_before_cursor() {
        let src = "package x\n\nfunc Run(srv *Server) {\n\tsrv.Start()\n}\n";
        let (_, tree) = parse(Path::new("x.go"), src).expect("parsed");
        let at = |line, col| identifier_at(&tree, src, line, col);
        // Inside, at the start, and just after the name.
        for col in [6, 5, 8] {
            let ident = at(2, col).expect("Run");
            assert_eq!(ident.name, "Run");
            assert_eq!((ident.line, ident.column, ident.end_column), (2, 5, 8));
            assert_eq!(&src[ident.start_byte..ident.end_byte], "Run");
        }
        // The type name, and a field selector.
        assert_eq!(at(2, 16).expect("type").name, "Server");
        let method = at(3, 6).expect("method");
        assert_eq!(method.name, "Start");
        assert_eq!(method.column, 5);
        // Just after `srv` and before the dot: the receiver.
        assert_eq!(at(3, 4).expect("srv").name, "srv");
        // Package name.
        assert_eq!(at(0, 8).expect("package").name, "x");
        // Whitespace and keywords away from any name.
        assert!(at(1, 0).is_none());
        assert!(at(100, 0).is_none());
    }

    #[test]
    fn identifier_at_counts_utf16_units() {
        let src = "package x\n\nvar _ = \"\u{1F600}\" + name\n";
        let (_, tree) = parse(Path::new("x.go"), src).expect("parsed");
        // `var _ = "` is 9 units, the emoji 2, then `" + ` 4 more.
        let ident = identifier_at(&tree, src, 2, 16).expect("name");
        assert_eq!(ident.name, "name");
        assert_eq!((ident.column, ident.end_column), (15, 19));
        assert_eq!(&src[ident.start_byte..ident.end_byte], "name");
    }

    #[test]
    fn identifier_at_labels_and_fields() {
        let src = "package x\n\ntype T struct{ Field int }\n\nfunc f() {\nouter:\n\tfor {\n\t\tbreak outer\n\t}\n}\n";
        let (_, tree) = parse(Path::new("x.go"), src).expect("parsed");
        assert_eq!(
            identifier_at(&tree, src, 2, 18).expect("field").name,
            "Field"
        );
        assert_eq!(
            identifier_at(&tree, src, 5, 2).expect("label").name,
            "outer"
        );
        assert_eq!(
            identifier_at(&tree, src, 7, 9).expect("break").name,
            "outer"
        );
    }

    #[test]
    fn crlf_sources_parse() {
        let src = "package x\r\n\r\n// Hi.\r\nfunc F() {}\r\ntype T struct {\r\n\tA int\r\n}\r\n";
        let file = go(src);
        assert_eq!(func_of(&file, "F").doc, "Hi.");
        assert_eq!(func_of(&file, "F").pos.line, 3);
        assert_eq!(type_of(&file, "T").fields[0].name, "A");
        assert_eq!(type_of(&file, "T").end_line, 6);
    }
}
