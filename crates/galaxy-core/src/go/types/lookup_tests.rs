use std::path::Path;

use super::{Callee, Resolved, Symbol, callee_at, resolve_at};
use crate::go::engine::GoEngine;
use crate::go::modules::GoProject;

const MARK: &str = "/*|*/";

fn put(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn fake_goroot(root: &Path) -> std::path::PathBuf {
    let g = root.join("goroot");
    put(
        &g,
        "src/net/http/http.go",
        "package http\n\nconst StatusOK = 200\n\nvar DefaultClient *Client\n\ntype Header map[string][]string\n\nfunc (h Header) Get(key string) string { return \"\" }\n\ntype Request struct {\n\tMethod string\n\tHeader Header\n}\n\ntype Response struct {\n\tStatus string\n\tBody   Body\n}\n\ntype Body interface{ Close() error }\n\ntype Client struct{ Timeout int }\n\nfunc (c *Client) Do(req *Request) (*Response, error) { return nil, nil }\n\nfunc Get(url string) (*Response, error) { return nil, nil }\n\ntype HandlerFunc func(w string, r *Request) *Response\n",
    );
    g
}

const HELPERS: &str = "package main\n\n// Helper helps.\nfunc Helper(a int, b string) (int, error) { return 0, nil }\n\nconst Limit = 10\n\nvar ErrX error\n\n// Strs is a list.\ntype Strs []string\n\ntype Reader interface {\n\tRead(p []byte) (int, error)\n}\n\ntype Alias = Strs\n\ntype Base struct{ ID int }\n\n// Hello says hi.\nfunc (b *Base) Hello(name string) string { return \"\" }\n\ntype Derived struct {\n\tBase\n\t// Name is the name.\n\tName string\n}\n\ntype Server struct {\n\tAddr string\n\tPort int\n}\n\ntype Hooks struct {\n\tOn func(a int, rest ...string) bool\n}\n";

const UTIL: &str = "package util\n\n// Join joins parts.\nfunc Join(sep string, parts ...string) string { return \"\" }\n\n// Map maps.\nfunc Map[T, U any](xs []T, f func(T) U) []U { return nil }\n\ntype Box[T any] struct{ V T }\n\nfunc (b *Box[T]) Put(v T) {}\n";

/// Builds the module and runs `f` with the engine, the buffer, its tree and
/// the marker offset.
fn run<R>(
    main: &str,
    f: impl FnOnce(&mut GoEngine, &Path, &str, &tree_sitter::Tree, usize) -> R,
) -> R {
    let tmp = tempfile::tempdir().unwrap();
    let goroot = fake_goroot(tmp.path());
    let app = tmp.path().join("app");
    put(&app, "go.mod", "module example.com/app\n\ngo 1.22\n");
    put(&app, "helpers.go", HELPERS);
    put(&app, "util/util.go", UTIL);
    let byte = main.find(MARK).expect("marker");
    let src = main.replace(MARK, "");
    put(&app, "main.go", &src);
    let project = GoProject::detect_with(&app, Some(goroot), None);
    assert!(project.is_some());
    let mut engine = GoEngine::with_project(project);
    let path = engine.root().join("main.go");
    let tree = crate::go::parse::parse(&path, &src).unwrap().1;
    f(&mut engine, &path, &src, &tree, byte)
}

fn res(main: &str) -> Option<Resolved> {
    run(main, resolve_at)
}

fn call(main: &str) -> Option<Callee> {
    run(main, callee_at)
}

fn body(imports: &str, decls: &str, stmts: &str) -> String {
    format!("package main\n\n{imports}\n{decls}\nfunc main() {{\n{stmts}\n}}\n")
}

const HTTP: &str = "import \"net/http\"";

fn line_of(main: &str, needle: &str) -> u32 {
    let clean = main.replace(MARK, "");
    let at = clean.find(needle).expect(needle);
    clean[..at].matches('\n').count() as u32
}

fn file_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

fn must(main: &str) -> Resolved {
    res(main).unwrap_or_else(|| panic!("no symbol in:\n{main}"))
}

// ---- resolve_at: locals ---------------------------------------------------

#[test]
fn local_short_var() {
    let main = body("", "", "\tx := 1\n\t_ = x/*|*/");
    let r = must(&main);
    let Symbol::Local {
        name,
        path,
        line,
        kind,
        ..
    } = &r.symbol
    else {
        panic!("{:?}", r.symbol)
    };
    assert_eq!((name.as_str(), kind.as_str()), ("x", "var"));
    assert_eq!(file_name(path), "main.go");
    assert_eq!(*line, line_of(&main, "x := 1"));
    assert_eq!(r.signature, "x int");
    assert_eq!(r.ty.display(), "int");
}

#[test]
fn local_param_and_receiver() {
    let main = body(
        "",
        "type T struct{}\n\nfunc (t *T) Run(count int) {\n\t_ = cou/*|*/nt\n}\n",
        "",
    );
    let r = must(&main);
    assert!(matches!(&r.symbol, Symbol::Local { kind, .. } if kind == "param"));
    assert_eq!(r.signature, "count int");
    let main = main
        .replace("cou/*|*/nt", "count")
        .replace("_ = count", "_ = t/*|*/");
    let r = must(&main);
    assert!(matches!(&r.symbol, Symbol::Local { kind, .. } if kind == "receiver"));
    assert_eq!(r.signature, "t *app.T");
}

#[test]
fn local_result_const_and_var() {
    let main = body(
        "",
        "func f() (n int, err error) {\n\tconst k = 3\n\tvar v string\n\t_ = n/*|*/\n\t_, _ = v, k\n\treturn\n}\n",
        "",
    );
    let r = must(&main);
    assert!(matches!(&r.symbol, Symbol::Local { kind, .. } if kind == "result"));
    assert_eq!(r.signature, "n int");
    let r = must(
        &main
            .replace("_ = n/*|*/", "_ = n")
            .replace("v, k", "v/*|*/, k"),
    );
    assert!(matches!(&r.symbol, Symbol::Local { kind, .. } if kind == "var"));
    assert_eq!(r.signature, "v string");
    let r = must(
        &main
            .replace("_ = n/*|*/", "_ = n")
            .replace("v, k", "v, k/*|*/"),
    );
    assert!(matches!(&r.symbol, Symbol::Local { kind, .. } if kind == "const"));
}

#[test]
fn local_range_and_type_switch() {
    let main = body(
        "",
        "",
        "\tfor i, s := range []string{\"a\"} {\n\t\t_, _ = i, s/*|*/\n\t}",
    );
    let r = must(&main);
    assert!(matches!(&r.symbol, Symbol::Local { kind, .. } if kind == "range var"));
    assert_eq!(r.signature, "s string");
    let main = body(
        "",
        "",
        "\tvar x any\n\tswitch v := x.(type) {\n\tcase int:\n\t\t_ = v/*|*/\n\t}",
    );
    let r = must(&main);
    assert!(matches!(&r.symbol, Symbol::Local { kind, .. } if kind == "type switch var"));
    assert_eq!(r.signature, "v int");
}

#[test]
fn local_label() {
    let main = body("", "", "outer:\n\tfor {\n\t\tbreak outer/*|*/\n\t}");
    let r = must(&main);
    assert!(matches!(&r.symbol, Symbol::Local { kind, .. } if kind == "label"));
    assert_eq!(r.signature, "label outer");
}

// ---- resolve_at: package level -------------------------------------------

#[test]
fn package_func_in_other_file() {
    let main = body("", "", "\tHelp/*|*/er(1, \"a\")");
    let r = must(&main);
    let Symbol::Func {
        name, path, pos, ..
    } = &r.symbol
    else {
        panic!("{:?}", r.symbol)
    };
    assert_eq!(name, "Helper");
    assert_eq!(file_name(path), "helpers.go");
    assert_eq!(pos.line, 3);
    assert_eq!(r.signature, "func Helper(a int, b string) (int, error)");
    assert_eq!(r.doc, "Helper helps.");
}

#[test]
fn package_type_const_var() {
    let main = body("", "", "\tvar s Str/*|*/s");
    let r = must(&main);
    assert!(matches!(&r.symbol, Symbol::Type { name, .. } if name == "Strs"));
    assert_eq!(r.signature, "type Strs []string");
    assert_eq!(r.doc, "Strs is a list.");
    let r = must(&body("", "", "\t_ = Lim/*|*/it"));
    assert!(matches!(&r.symbol, Symbol::Value { is_const: true, .. }));
    assert_eq!(r.signature, "const Limit = 10");
    let r = must(&body("", "", "\t_ = ErrX/*|*/"));
    assert!(matches!(
        &r.symbol,
        Symbol::Value {
            is_const: false,
            ..
        }
    ));
    assert_eq!(r.signature, "var ErrX error");
    assert_eq!(r.ty.display(), "error");
}

#[test]
fn type_signatures_interface_and_alias() {
    let r = must(&body("", "", "\tvar r Rea/*|*/der"));
    assert_eq!(r.signature, "type Reader interface");
    let r = must(&body("", "", "\tvar r Ali/*|*/as"));
    assert_eq!(r.signature, "type Alias = Strs");
    let r = must(&body("", "", "\tvar r Ser/*|*/ver"));
    assert_eq!(r.signature, "type Server struct");
}

#[test]
fn import_name_is_package() {
    let main = body(HTTP, "", "\t_ = http/*|*/.Get");
    let r = must(&main);
    assert_eq!(
        r.symbol,
        Symbol::Package {
            import_path: "net/http".into()
        }
    );
    assert_eq!(r.signature, "package http");
}

#[test]
fn builtins_and_universe() {
    let r = must(&body("", "", "\t_ = le/*|*/n(\"a\")"));
    assert_eq!(r.symbol, Symbol::Builtin { name: "len".into() });
    assert!(r.signature.starts_with("func len"));
    let r = must(&body("", "", "\tvar s stri/*|*/ng"));
    assert_eq!(
        r.symbol,
        Symbol::Builtin {
            name: "string".into()
        }
    );
    assert_eq!(r.signature, "type string");
    let r = must(&body("", "", "\tvar e err/*|*/or"));
    assert!(matches!(r.symbol, Symbol::Builtin { .. }));
}

#[test]
fn type_parameters() {
    let main = body(
        "",
        "func Id[T any](v T) T {\n\tvar z T/*|*/\n\treturn z\n}\n",
        "",
    );
    let r = must(&main);
    assert_eq!(r.symbol, Symbol::TypeParam { name: "T".into() });
    assert_eq!(r.signature, "T any");
    let main = body("", "type List[E comparable] struct{ items []E/*|*/ }\n", "");
    let r = must(&main);
    assert_eq!(r.symbol, Symbol::TypeParam { name: "E".into() });
    assert_eq!(r.signature, "E comparable");
}

// ---- resolve_at: selectors -----------------------------------------------

#[test]
fn package_member_func_lands_in_goroot() {
    let main = body(HTTP, "", "\t_, _ = http.Ge/*|*/t(\"u\")");
    let r = must(&main);
    let Symbol::Func { pkg, path, pos, .. } = &r.symbol else {
        panic!("{:?}", r.symbol)
    };
    assert_eq!(pkg, "net/http");
    assert!(path.ends_with("src/net/http/http.go"), "{path:?}");
    assert_eq!(pos.line, 26);
    assert_eq!(r.signature, "func Get(url string) (*Response, error)");
}

#[test]
fn package_member_type_const_var() {
    let r = must(&body(HTTP, "", "\t_ = http.Cli/*|*/ent{}"));
    assert!(
        matches!(&r.symbol, Symbol::Type { name, pos, .. } if name == "Client" && pos.line == 22)
    );
    assert_eq!(r.signature, "type Client struct");
    let r = must(&body(HTTP, "", "\t_ = http.StatusO/*|*/K"));
    assert!(matches!(&r.symbol, Symbol::Value { is_const: true, pos, .. } if pos.line == 2));
    assert_eq!(r.signature, "const StatusOK = 200");
    let r = must(&body(HTTP, "", "\t_ = http.DefaultClient/*|*/"));
    assert!(matches!(
        &r.symbol,
        Symbol::Value {
            is_const: false,
            ..
        }
    ));
    assert_eq!(r.signature, "var DefaultClient *Client");
}

#[test]
fn unknown_package_member_is_none() {
    assert!(res(&body(HTTP, "", "\t_ = http.Nope/*|*/")).is_none());
}

#[test]
fn field_of_goroot_struct() {
    let main = body(HTTP, "", "\treq := &http.Request{}\n\t_ = req.Meth/*|*/od");
    let r = must(&main);
    let Symbol::Field {
        owner, path, pos, ..
    } = &r.symbol
    else {
        panic!("{:?}", r.symbol)
    };
    assert_eq!(owner, "Request");
    assert!(path.ends_with("http.go"));
    assert_eq!(pos.line, 11);
    assert_eq!(r.signature, "field Method string");
    assert_eq!(r.ty.display(), "string");
}

#[test]
fn method_of_goroot_type() {
    let main = body(HTTP, "", "\tc := &http.Client{}\n\t_, _ = c.D/*|*/o(nil)");
    let r = must(&main);
    let Symbol::Method { owner, pos, .. } = &r.symbol else {
        panic!("{:?}", r.symbol)
    };
    assert_eq!(owner, "Client");
    assert_eq!(pos.line, 24);
    assert_eq!(
        r.signature,
        "func (c *Client) Do(req *Request) (*Response, error)"
    );
}

#[test]
fn promoted_method_keeps_declaring_owner() {
    let main = body("", "", "\td := Derived{}\n\t_ = d.Hel/*|*/lo(\"x\")");
    let r = must(&main);
    let Symbol::Method {
        owner, path, pos, ..
    } = &r.symbol
    else {
        panic!("{:?}", r.symbol)
    };
    assert_eq!(owner, "Base");
    assert_eq!(file_name(path), "helpers.go");
    assert_eq!(pos.line, 21);
    assert_eq!(r.doc, "Hello says hi.");
    assert_eq!(r.signature, "func (b *Base) Hello(name string) string");
}

#[test]
fn promoted_field_keeps_declaring_owner() {
    let main = body("", "", "\td := Derived{}\n\t_ = d.I/*|*/D");
    let r = must(&main);
    assert!(matches!(&r.symbol, Symbol::Field { owner, .. } if owner == "Base"));
    assert_eq!(r.signature, "field ID int");
}

#[test]
fn field_doc_comment() {
    let main = body("", "", "\td := Derived{}\n\t_ = d.Na/*|*/me");
    let r = must(&main);
    assert!(matches!(&r.symbol, Symbol::Field { owner, .. } if owner == "Derived"));
    assert_eq!(r.doc, "Name is the name.");
}

#[test]
fn selector_on_call_result_and_chain() {
    let main = body(
        HTTP,
        "",
        "\tresp, _ := http.Get(\"u\")\n\t_ = resp.Body.Clo/*|*/se()",
    );
    let r = must(&main);
    assert!(matches!(&r.symbol, Symbol::Method { owner, .. } if owner == "Body"));
    assert_eq!(r.signature, "func (Body) Close() error");
}

// ---- resolve_at: literals and type positions -----------------------------

#[test]
fn keyed_struct_literal_field() {
    let main = body("", "", "\t_ = Server{Add/*|*/r: \"x\", Port: 1}");
    let r = must(&main);
    let Symbol::Field { owner, path, .. } = &r.symbol else {
        panic!("{:?}", r.symbol)
    };
    assert_eq!(owner, "Server");
    assert_eq!(file_name(path), "helpers.go");
    assert_eq!(r.signature, "field Addr string");
}

#[test]
fn keyed_field_in_pointer_and_nested_literals() {
    let r = must(&body("", "", "\t_ = &Server{Port/*|*/: 1}"));
    assert!(matches!(&r.symbol, Symbol::Field { name, .. } if name == "Port"));
    let r = must(&body("", "", "\t_ = []Server{{Port/*|*/: 1}}"));
    assert!(matches!(&r.symbol, Symbol::Field { name, .. } if name == "Port"));
    let r = must(&body(
        "",
        "",
        "\t_ = map[string]*Server{\"a\": {Addr/*|*/: \"\"}}",
    ));
    assert!(matches!(&r.symbol, Symbol::Field { name, .. } if name == "Addr"));
}

#[test]
fn keyed_goroot_struct_field() {
    let r = must(&body(HTTP, "", "\t_ = http.Request{Meth/*|*/od: \"GET\"}"));
    let Symbol::Field { path, pos, .. } = &r.symbol else {
        panic!("{:?}", r.symbol)
    };
    assert!(path.ends_with("http.go"));
    assert_eq!(pos.line, 11);
}

#[test]
fn map_key_is_not_a_field() {
    let main = body("", "", "\tk := \"a\"\n\t_ = map[string]int{k/*|*/: 1}");
    let r = must(&main);
    assert!(matches!(&r.symbol, Symbol::Local { .. }));
}

#[test]
fn type_positions() {
    let r = must(&body(HTTP, "", "\tvar c *http.Cli/*|*/ent\n\t_ = c"));
    assert!(matches!(&r.symbol, Symbol::Type { name, .. } if name == "Client"));
    let r = must(&body(HTTP, "", "\tvar c *htt/*|*/p.Client\n\t_ = c"));
    assert!(matches!(&r.symbol, Symbol::Package { .. }));
    let r = must(&body("", "func f(u *Ser/*|*/ver) {}\n", ""));
    assert!(matches!(&r.symbol, Symbol::Type { name, .. } if name == "Server"));
    let r = must(&body("", "", "\t_ = Str/*|*/s(nil)"));
    assert!(matches!(&r.symbol, Symbol::Type { name, .. } if name == "Strs"));
    let r = must(&body("", "", "\t_ = []Ser/*|*/ver{}"));
    assert!(matches!(&r.symbol, Symbol::Type { name, .. } if name == "Server"));
}

// ---- resolve_at: declarations --------------------------------------------

#[test]
fn declaration_sites_resolve_to_themselves() {
    let main = body("", "func Fo/*|*/o(a int) string { return \"\" }\n", "");
    let r = must(&main);
    let Symbol::Func {
        name, path, pos, ..
    } = &r.symbol
    else {
        panic!("{:?}", r.symbol)
    };
    assert_eq!(name, "Foo");
    assert_eq!(file_name(path), "main.go");
    assert_eq!(pos.line, line_of(&main, "func Foo"));
    assert_eq!(r.signature, "func Foo(a int) string");

    let r = must(&body("", "type Wid/*|*/get struct{ N int }\n", ""));
    assert!(matches!(&r.symbol, Symbol::Type { name, .. } if name == "Widget"));
    assert_eq!(r.signature, "type Widget struct");

    let r = must(&body("", "", "\tx/*|*/ := 5\n\t_ = x"));
    assert!(matches!(&r.symbol, Symbol::Local { kind, .. } if kind == "var"));
    assert_eq!(r.signature, "x int");

    let r = must(&body("", "type W struct {\n\tCou/*|*/nt int\n}\n", ""));
    assert!(matches!(&r.symbol, Symbol::Field { owner, .. } if owner == "W"));
    assert_eq!(r.signature, "field Count int");

    let r = must(&body("", "func g(na/*|*/me string) {}\n", ""));
    assert!(matches!(&r.symbol, Symbol::Local { kind, .. } if kind == "param"));
    assert_eq!(r.signature, "name string");
}

#[test]
fn method_declaration_site() {
    let main = body(
        "",
        "type W struct{}\n\n// Run runs.\nfunc (w *W) Ru/*|*/n(n int) error { return nil }\n",
        "",
    );
    let r = must(&main);
    assert!(
        matches!(&r.symbol, Symbol::Method { owner, name, .. } if owner == "W" && name == "Run")
    );
    assert_eq!(r.signature, "func (w *W) Run(n int) error");
    assert_eq!(r.doc, "Run runs.");
}

#[test]
fn interface_method_declaration_site() {
    let r = must(&body(
        "",
        "type I interface {\n\t// Do does.\n\tD/*|*/o(a int) error\n}\n",
        "",
    ));
    assert!(matches!(&r.symbol, Symbol::Method { owner, .. } if owner == "I"));
    assert_eq!(r.signature, "func (I) Do(a int) error");
    assert_eq!(r.doc, "Do does.");
}

#[test]
fn variadic_signature_text() {
    let r = must(&body(
        "import \"example.com/app/util\"",
        "",
        "\t_ = util.Jo/*|*/in(\",\", \"a\")",
    ));
    assert_eq!(r.signature, "func Join(sep string, parts ...string) string");
    assert_eq!(r.doc, "Join joins parts.");
}

#[test]
fn generic_func_signature() {
    let r = must(&body(
        "import \"example.com/app/util\"",
        "",
        "\t_ = util.Ma/*|*/p[int, string]",
    ));
    assert_eq!(r.signature, "func Map[T, U any](xs []T, f func(T) U) []U");
}

#[test]
fn range_covers_identifier() {
    let main = body("", "", "\tvalue := 1\n\t_ = value/*|*/");
    let r = must(&main);
    let at = main.replace(MARK, "").find("_ = value").unwrap() + 4;
    let line = line_of(&main, "_ = value");
    assert_eq!(r.start_line, line);
    let col = (at - main.replace(MARK, "")[..at].rfind('\n').unwrap() - 1) as u32;
    assert_eq!((r.start_column, r.end_column), (col, col + 5));
}

#[test]
fn cursor_at_start_and_end_of_identifier() {
    for variant in [
        "/*|*/Helper(1, \"\")",
        "Helper/*|*/(1, \"\")",
        "Hel/*|*/per(1, \"\")",
    ] {
        let r = must(&body("", "", &format!("\t{variant}")));
        assert!(
            matches!(&r.symbol, Symbol::Func { name, .. } if name == "Helper"),
            "{variant}"
        );
    }
}

#[test]
fn nothing_to_resolve() {
    assert!(
        res(&body(
            "",
            "type T struct {\n\tA int `js/*|*/on:\"a\"`\n}\n",
            ""
        ))
        .is_none()
    );
    assert!(res(&body("", "", "\t_ = \"hel/*|*/lo\"")).is_none());
    assert!(res(&body("", "", "\t// com/*|*/ment here")).is_none());
    assert!(res(&body("", "", "\t_ = 1 +/*|*/ 2")).is_none());
    assert!(res(&body("", "", "\t_ = zzz/*|*/")).is_none());
}

#[test]
fn never_panics_at_any_byte() {
    let src = body(
        HTTP,
        "type W[T any] struct{ A T }\n\nfunc (w *W[T]) M(x ...T) {}\n",
        "\tc := &http.Client{}\n\t_, err := c.Do(nil)\n\tfor i := range 3 {\n\t\t_ = i + undefined(\n\t}\n\t_ = err",
    );
    let clean = src.replace(MARK, "");
    let mut offsets: Vec<usize> = clean.char_indices().map(|(i, _)| i).collect();
    offsets.push(clean.len());
    offsets.push(clean.len() + 10);
    run(&format!("{clean}{MARK}"), |engine, path, text, tree, _| {
        for byte in offsets {
            let _ = resolve_at(engine, path, text, tree, byte);
            let _ = callee_at(engine, path, text, tree, byte);
        }
    });
}

// ---- callee_at ------------------------------------------------------------

#[test]
fn callee_same_package_function() {
    let c = call(&body("", "", "\tHelper(/*|*/")).expect("callee");
    assert_eq!(c.name, "Helper");
    assert_eq!(c.label, "func Helper(a int, b string) (int, error)");
    assert_eq!(c.params, ["a int", "b string"]);
    assert_eq!(c.results, ["int", "error"]);
    assert_eq!(c.doc, "Helper helps.");
    assert_eq!(c.active, 0);
}

#[test]
fn callee_active_index_after_commas() {
    let c = call(&body("", "", "\tHelper(1, /*|*/\"x\")")).unwrap();
    assert_eq!(c.active, 1);
    let c = call(&body("", "", "\tHelper(1/*|*/, \"x\")")).unwrap();
    assert_eq!(c.active, 0);
    let c = call(&body("", "", "\tHelper(1, \"x\"/*|*/)")).unwrap();
    assert_eq!(c.active, 1);
}

#[test]
fn callee_nested_calls() {
    let outer = call(&body("", "", "\tHelper(Helper(1, \"a\"), /*|*/)")).unwrap();
    assert_eq!((outer.name.as_str(), outer.active), ("Helper", 1));
    let inner = call(&body("", "", "\tHelper(Helper(1, /*|*/\"a\"), 2)")).unwrap();
    assert_eq!((inner.name.as_str(), inner.active), ("Helper", 1));
    let inner = call(&body("", "", "\tHelper([]int{1, 2, 3}[0], /*|*/")).unwrap();
    assert_eq!(inner.active, 1);
}

#[test]
fn callee_ignores_commas_in_strings_and_literals() {
    let c = call(&body("", "", "\tHelper(1, \"a, b, c\", /*|*/")).unwrap();
    assert_eq!(c.active, 2);
    let c = call(&body(
        "",
        "",
        "\tHelper(map[string]int{\"a\": 1, \"b\": 2}, /*|*/",
    ))
    .unwrap();
    assert_eq!(c.active, 1);
    let c = call(&body("", "", "\tHelper(',', `x,y`, /*|*/")).unwrap();
    assert_eq!(c.active, 2);
}

#[test]
fn callee_variadic_clamps_to_last_param() {
    let main = body(
        "import \"example.com/app/util\"",
        "",
        "\tutil.Join(\",\", \"a\", \"b\", /*|*/",
    );
    let c = call(&main).unwrap();
    assert_eq!(c.label, "func Join(sep string, parts ...string) string");
    assert_eq!(c.params, ["sep string", "parts ...string"]);
    assert_eq!(c.active, 1);
}

#[test]
fn callee_method_call() {
    let main = body(HTTP, "", "\tc := &http.Client{}\n\tc.Do(/*|*/");
    let c = call(&main).unwrap();
    assert_eq!(c.name, "Do");
    assert_eq!(
        c.label,
        "func (c *Client) Do(req *Request) (*Response, error)"
    );
    assert_eq!(c.params, ["req *Request"]);
    assert_eq!(c.results, ["*Response", "error"]);
}

#[test]
fn callee_promoted_method() {
    let main = body("", "", "\td := Derived{}\n\td.Hello(/*|*/)");
    let c = call(&main).unwrap();
    assert_eq!(c.label, "func (b *Base) Hello(name string) string");
    assert_eq!(c.doc, "Hello says hi.");
}

#[test]
fn callee_package_function() {
    let c = call(&body(HTTP, "", "\thttp.Get(/*|*/")).unwrap();
    assert_eq!(c.label, "func Get(url string) (*Response, error)");
    assert_eq!(c.params, ["url string"]);
}

#[test]
fn callee_generic_explicit_instantiation() {
    let main = body(
        "import \"example.com/app/util\"",
        "",
        "\tutil.Map[int, string](nil, /*|*/",
    );
    let c = call(&main).unwrap();
    assert_eq!(c.label, "func Map(xs []int, f func(int) string) []string");
    assert_eq!(c.active, 1);
}

#[test]
fn callee_generic_inferred() {
    let main = body(
        "import \"example.com/app/util\"",
        "",
        "\txs := []int{1}\n\tutil.Map(xs, /*|*/",
    );
    let c = call(&main).unwrap();
    assert_eq!(c.params[0], "xs []int");
    assert_eq!(c.active, 1);
}

#[test]
fn callee_generic_uninferred_keeps_type_params() {
    let main = body("import \"example.com/app/util\"", "", "\tutil.Map(/*|*/");
    let c = call(&main).unwrap();
    assert_eq!(c.label, "func Map[T, U any](xs []T, f func(T) U) []U");
}

#[test]
fn callee_generic_receiver_substituted() {
    let main = body(
        "import \"example.com/app/util\"",
        "",
        "\tb := &util.Box[int]{}\n\tb.Put(/*|*/",
    );
    let c = call(&main).unwrap();
    assert_eq!(c.params, ["v int"]);
}

#[test]
fn callee_func_typed_local() {
    let main = body(
        "",
        "",
        "\tcb := func(a int, b string) error { return nil }\n\tcb(1, /*|*/",
    );
    let c = call(&main).unwrap();
    assert_eq!(c.name, "cb");
    assert_eq!(c.params, ["int", "string"]);
    assert_eq!(c.results, ["error"]);
    assert_eq!(c.active, 1);
    let main = body(
        "",
        "",
        "\tvar cb func(a int, rest ...string) bool\n\tcb(1, \"x\", /*|*/",
    );
    let c = call(&main).unwrap();
    assert_eq!(c.label, "func cb(a int, rest ...string) bool");
    assert_eq!(c.active, 1);
}

#[test]
fn callee_func_typed_field() {
    let main = body("", "", "\th := Hooks{}\n\th.On(1, /*|*/");
    let c = call(&main).unwrap();
    assert_eq!(c.name, "On");
    assert_eq!(c.params, ["a int", "rest ...string"]);
    assert_eq!(c.active, 1);
}

#[test]
fn callee_incomplete_call_at_eof() {
    let tmp_main = "package main\n\nimport \"net/http\"\n\nfunc main() {\n\tclient := &http.Client{}\n\treq := &http.Request{}\n\tclient.Do(req, /*|*/";
    let c = call(tmp_main).expect("callee");
    assert_eq!(c.name, "Do");
    assert_eq!(c.active, 1);
    let c = call("package main\n\nfunc main() {\n\tHelper(1, \"a\"/*|*/").unwrap();
    assert_eq!(c.name, "Helper");
    assert_eq!(c.active, 1);
}

#[test]
fn callee_incomplete_call_followed_by_code() {
    let main = "package main\n\nfunc main() {\n\tHelper(1, /*|*/\n}\n\nfunc other() {}\n";
    let c = call(main).expect("callee");
    assert_eq!(c.name, "Helper");
    assert_eq!(c.active, 1);
}

#[test]
fn callee_builtins() {
    let c = call(&body("", "", "\tvar s []int\n\ts = append(s, /*|*/")).unwrap();
    assert_eq!(c.label, "func append(slice []T, elems ...T) []T");
    assert_eq!(c.params, ["slice []T", "elems ...T"]);
    assert_eq!(c.active, 1);
    let c = call(&body(
        "",
        "",
        "\tvar s []int\n\ts = append(s, 1, 2, 3, /*|*/",
    ))
    .unwrap();
    assert_eq!(c.active, 1);
    for name in ["make", "new", "len", "cap", "copy", "delete", "panic"] {
        let c = call(&body("", "", &format!("\t{name}(/*|*/"))).expect(name);
        assert_eq!(c.name, name);
        assert!(c.label.starts_with(&format!("func {name}(")), "{}", c.label);
    }
}

#[test]
fn callee_conversions_and_unhelped_builtins_are_none() {
    assert!(call(&body("", "", "\t_ = int(/*|*/")).is_none());
    assert!(call(&body("", "", "\t_ = string(/*|*/")).is_none());
    assert!(call(&body("", "", "\t_ = Strs(/*|*/")).is_none());
    assert!(call(&body("", "", "\tprintln(/*|*/")).is_none());
}

#[test]
fn callee_shadowed_builtin_uses_local() {
    let main = body(
        "",
        "",
        "\tlen := func(a, b int) int { return a }\n\t_ = len(1, /*|*/",
    );
    let c = call(&main).unwrap();
    assert_eq!(c.params, ["int", "int"]);
}

#[test]
fn callee_non_call_positions_are_none() {
    assert!(call(&body("", "", "\tx := (1 + /*|*/2)")).is_none());
    assert!(call(&body("", "", "\tif (true/*|*/) {\n\t}")).is_none());
    assert!(call(&body("", "", "\tHelper(1, \"a\")/*|*/")).is_none());
    assert!(call(&body("", "", "\tHel/*|*/per(1, \"a\")")).is_none());
    assert!(call(&body("", "", "\tx := []int{1, /*|*/2}")).is_none());
    assert!(call(&body("", "", "\t/*|*/")).is_none());
    assert!(call(&body("", "func Foo(a int, /*|*/b int) {}\n", "")).is_none());
    assert!(
        call(&body(
            "",
            "func (s *Server) Foo(a int, /*|*/b int) {}\n",
            ""
        ))
        .is_none()
    );
}

#[test]
fn callee_cursor_right_after_open_paren_and_in_gap() {
    let c = call(&body("", "", "\tHelper(/*|*/1, \"a\")")).unwrap();
    assert_eq!(c.active, 0);
    let c = call(&body("", "", "\tHelper(1,/*|*/ \"a\")")).unwrap();
    assert_eq!(c.active, 1);
}

#[test]
fn callee_in_go_and_defer_statements() {
    let c = call(&body("", "", "\tgo Helper(1, /*|*/")).unwrap();
    assert_eq!(c.name, "Helper");
    let c = call(&body("", "", "\tdefer Helper(/*|*/")).unwrap();
    assert_eq!(c.active, 0);
}

#[test]
fn callee_on_multiline_arguments() {
    let main = body("", "", "\tHelper(\n\t\t1,\n\t\t/*|*/\n\t)");
    let c = call(&main).unwrap();
    assert_eq!(c.name, "Helper");
    assert_eq!(c.active, 1);
}

#[test]
fn callee_never_panics_on_garbage() {
    for main in [
        "(/*|*/",
        "f(/*|*/",
        "package main\nfunc main() { ((((/*|*/",
        "package main\nfunc main() { a.b.c.(/*|*/",
        "\u{1F600}(/*|*/",
        "package main\nvar x = f(\"unterminated, /*|*/",
        "package main\nfunc main() { x[(/*|*/",
    ] {
        let _ = call(main);
        let _ = res(main);
    }
}
