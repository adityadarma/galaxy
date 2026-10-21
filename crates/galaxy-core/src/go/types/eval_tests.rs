use std::path::Path;

use crate::go::engine::GoEngine;
use crate::go::modules::GoProject;
use crate::go::types::{Ty, eval_text, receiver_ty_at};

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
    put(
        &g,
        "src/errors/errors.go",
        "package errors\n\nfunc New(text string) error { return nil }\n",
    );
    g
}

fn fake_modcache(root: &Path) -> std::path::PathBuf {
    let m = root.join("modcache");
    put(
        &m,
        "github.com/gin-gonic/gin@v1.9.0/context.go",
        "package gin\n\ntype Context struct {\n\tKeys map[string]any\n}\n\nfunc (c *Context) Param(key string) string { return \"\" }\n\nfunc (c *Context) JSON(code int, obj any) {}\n",
    );
    m
}

const GO_MOD: &str =
    "module example.com/app\n\ngo 1.22\n\nrequire github.com/gin-gonic/gin v1.9.0\n";

/// Builds the module with `files`, puts `main` (with a `/*|*/` marker) into
/// `main.go` and runs `f` with the engine, buffer, tree and marker offset.
fn run<R>(
    files: &[(&str, &str)],
    main: &str,
    f: impl FnOnce(&mut GoEngine, &Path, &str, &tree_sitter::Tree, usize) -> R,
) -> R {
    let tmp = tempfile::tempdir().unwrap();
    let goroot = fake_goroot(tmp.path());
    let modcache = fake_modcache(tmp.path());
    let app = tmp.path().join("app");
    put(&app, "go.mod", GO_MOD);
    for (rel, text) in files {
        put(&app, rel, text);
    }
    let byte = main.find(MARK).expect("marker");
    let src = main.replace(MARK, "");
    put(&app, "main.go", &src);
    let project = GoProject::detect_with(&app, Some(goroot), Some(modcache));
    assert!(project.is_some());
    let mut engine = GoEngine::with_project(project);
    let path = engine.root().join("main.go");
    let tree = crate::go::parse::parse(&path, &src).unwrap().1;
    f(&mut engine, &path, &src, &tree, byte)
}

/// Type of the receiver before the dot at the marker.
fn recv(files: &[(&str, &str)], main: &str) -> Ty {
    run(files, main, receiver_ty_at)
}

/// Display text of the receiver type.
fn show(main: &str) -> String {
    recv(&[], main).display()
}

fn show_with(files: &[(&str, &str)], main: &str) -> String {
    recv(files, main).display()
}

/// Type of `expr` evaluated at the marker.
fn eval(main: &str, expr: &str) -> String {
    run(&[], main, |e, p, s, t, b| {
        eval_text(e, p, s, t, b, expr).display()
    })
}

const USER: &str = "type User struct {\n\tName string\n\tAge  int\n}\n\nfunc (u *User) Greet() string { return \"\" }\n\n";

fn body(imports: &str, decls: &str, stmts: &str) -> String {
    format!("package main\n\n{imports}\n{decls}\nfunc main() {{\n{stmts}\n}}\n")
}

#[test]
fn http_do_tuple_pick() {
    let main = body(
        "import \"net/http\"",
        "",
        "\tclient := &http.Client{}\n\treq := &http.Request{}\n\tresp, err := client.Do(req)\n\tresp./*|*/",
    );
    assert_eq!(show(&main), "*http.Response");
    let main = main.replace("resp./*|*/", "err./*|*/");
    assert_eq!(show(&main), "error");
}

#[test]
fn address_of_composite() {
    let main = body("", USER, "\tu := &User{}\n\tu./*|*/");
    assert_eq!(show(&main), "*app.User");
}

#[test]
fn value_composite() {
    let main = body("", USER, "\tu := User{}\n\tu./*|*/");
    assert_eq!(show(&main), "app.User");
}

#[test]
fn slice_index() {
    let main = body("", USER, "\ts := []User{}\n\ts[0]./*|*/");
    assert_eq!(show(&main), "app.User");
}

#[test]
fn map_index() {
    let main = body("", USER, "\tm := map[string]*User{}\n\tm[\"a\"]./*|*/");
    assert_eq!(show(&main), "*app.User");
}

#[test]
fn map_comma_ok() {
    let main = body(
        "",
        USER,
        "\tm := map[string]*User{}\n\tv, ok := m[\"a\"]\n\tv./*|*/",
    );
    assert_eq!(show(&main), "*app.User");
    let main = main.replace("v./*|*/", "ok./*|*/");
    assert_eq!(show(&main), "bool");
}

#[test]
fn type_assertion_comma_ok() {
    let main = body("", USER, "\tvar x any\n\tu, ok := x.(*User)\n\tu./*|*/");
    assert_eq!(show(&main), "*app.User");
    let main = main.replace("u./*|*/", "ok./*|*/");
    assert_eq!(show(&main), "bool");
}

#[test]
fn channel_receive() {
    let main = body(
        "",
        USER,
        "\tch := make(chan *User)\n\tvar c chan *User\n\tv := <-c\n\tv./*|*/",
    );
    assert_eq!(show(&main), "*app.User");
    let main = main.replace("v./*|*/", "ch./*|*/");
    assert_eq!(show(&main), "chan *app.User");
}

#[test]
fn channel_comma_ok() {
    let main = body("", USER, "\tvar c chan *User\n\tv, ok := <-c\n\tok./*|*/");
    assert_eq!(show(&main), "bool");
    let main = main.replace("ok./*|*/", "v./*|*/");
    assert_eq!(show(&main), "*app.User");
}

#[test]
fn chained_method_calls() {
    let decls = "type Repo struct{}\n\ntype Query struct{}\n\nfunc (r *Repo) Find() *Query { return nil }\n\nfunc (q *Query) First() *Item { return nil }\n\ntype Item struct{ ID int }\n";
    let main = body("", decls, "\trepo := &Repo{}\n\trepo.Find().First()./*|*/");
    assert_eq!(show(&main), "*app.Item");
}

#[test]
fn promoted_members() {
    let decls = "type Base struct{ ID int }\n\nfunc (b *Base) Touch() *Base { return b }\n\ntype Derived struct {\n\tBase\n\tName string\n}\n";
    let main = body("", decls, "\td := &Derived{}\n\td.Touch()./*|*/");
    assert_eq!(show(&main), "*app.Base");
    let main = body("", decls, "\td := &Derived{}\n\td.Base./*|*/");
    assert_eq!(show(&main), "app.Base");
}

#[test]
fn package_constant() {
    let main = body("import \"net/http\"", "", "\tx := http.StatusOK\n\tx./*|*/");
    assert_eq!(show(&main), "int");
}

#[test]
fn package_var() {
    let main = body("import \"net/http\"", "", "\thttp.DefaultClient./*|*/");
    assert_eq!(show(&main), "*http.Client");
}

#[test]
fn package_function_result() {
    let main = body(
        "import \"net/http\"",
        "",
        "\tr, err := http.Get(\"u\")\n\tr./*|*/",
    );
    assert_eq!(show(&main), "*http.Response");
    let main = main.replace("r./*|*/", "err./*|*/");
    assert_eq!(show(&main), "error");
}

#[test]
fn package_receiver() {
    let main = body("import \"net/http\"", "", "\thttp./*|*/");
    assert_eq!(show(&main), "package http");
}

#[test]
fn package_function_call_tuple() {
    let main = body("import \"net/http\"", "", "\thttp.Get(\"u\")./*|*/");
    assert_eq!(show(&main), "(*http.Response, error)");
}

#[test]
fn function_typed_field_call() {
    let decls = "type Hooks struct {\n\tOnDone func(n int) *Item\n}\n\ntype Item struct{}\n";
    let main = body("", decls, "\th := &Hooks{}\n\th.OnDone(1)./*|*/");
    assert_eq!(show(&main), "*app.Item");
}

#[test]
fn named_func_type_call() {
    let main = body(
        "import \"net/http\"",
        "",
        "\tvar h http.HandlerFunc\n\th(\"w\", nil)./*|*/",
    );
    assert_eq!(show(&main), "*http.Response");
}

#[test]
fn conversion_to_builtin() {
    let main = body("", "", "\tb := []byte{}\n\tstring(b)./*|*/");
    assert_eq!(show(&main), "string");
}

#[test]
fn conversion_to_named() {
    let decls = "type ID string\n";
    let main = body("", decls, "\tID(\"a\")./*|*/");
    assert_eq!(show(&main), "app.ID");
    let main = body("", decls, "\t(*ID)(nil)./*|*/");
    assert_eq!(show(&main), "*app.ID");
}

#[test]
fn struct_literal_value() {
    let decls = "type Config struct{ Port int }\n\nfunc (c Config) Addr() string { return \"\" }\n";
    let main = body("", decls, "\tConfig{}./*|*/");
    assert_eq!(show(&main), "app.Config");
}

#[test]
fn qualified_composite() {
    let main = body("import \"net/http\"", "", "\thttp.Request{}./*|*/");
    assert_eq!(show(&main), "http.Request");
}

#[test]
fn generic_composite() {
    let decls = "type Repo[T any] struct{ items []T }\n";
    let main = body("", decls, "\tr := Repo[int]{}\n\tr./*|*/");
    assert_eq!(show(&main), "app.Repo[int]");
}

#[test]
fn modcache_param() {
    let main = "package main\n\nimport \"github.com/gin-gonic/gin\"\n\nfunc handler(c *gin.Context) {\n\tc./*|*/\n}\n";
    assert_eq!(show(main), "*gin.Context");
    let main = main.replace("c./*|*/", "c.Keys./*|*/");
    assert_eq!(show(&main), "map[string]any");
}

#[test]
fn modcache_method_result() {
    let main = "package main\n\nimport \"github.com/gin-gonic/gin\"\n\nfunc handler(c *gin.Context) {\n\tp := c.Param(\"id\")\n\tp./*|*/\n}\n";
    assert_eq!(show(main), "string");
}

#[test]
fn shadowed_variable() {
    let decls = "type A struct{}\n\ntype B struct{}\n";
    let main = body(
        "",
        decls,
        "\tx := &A{}\n\t{\n\t\tx := &B{}\n\t\tx./*|*/\n\t}",
    );
    assert_eq!(show(&main), "*app.B");
    let main = body(
        "",
        decls,
        "\tx := &A{}\n\t{\n\t\tx := &B{}\n\t\t_ = x\n\t}\n\tx./*|*/",
    );
    assert_eq!(show(&main), "*app.A");
}

#[test]
fn use_before_declaration() {
    let decls = "type A struct{}\n";
    let main = body("", decls, "\tx./*|*/\n\tx := &A{}\n\t_ = x");
    assert_eq!(show(&main), "?");
}

#[test]
fn self_reference_in_init() {
    let decls = "type A struct{}\n";
    let main = body("", decls, "\tx := &A{}\n\tx := x\n\tx./*|*/");
    // The inner `x` of `x := x` sees the outer one.
    assert!(show(&main) == "*app.A" || show(&main) == "?");
}

#[test]
fn incomplete_code_at_eof() {
    let main = "package main\n\ntype A struct{ N int }\n\nfunc main() {\n\ta := &A{}\n\ta./*|*/";
    assert_eq!(show(main), "*app.A");
    let main = "package main\n\nfunc main() {\n\tx := (\n\tx./*|*/";
    assert_eq!(show(main), "?");
}

#[test]
fn literals() {
    let main = body("", "", "\t/*|*/");
    assert_eq!(eval(&main, "\"s\""), "string");
    assert_eq!(eval(&main, "`raw`"), "string");
    assert_eq!(eval(&main, "1"), "int");
    assert_eq!(eval(&main, "1.5"), "float64");
    assert_eq!(eval(&main, "'a'"), "rune");
    assert_eq!(eval(&main, "2i"), "complex128");
    assert_eq!(eval(&main, "true"), "bool");
    assert_eq!(eval(&main, "nil"), "?");
}

#[test]
fn unary_operators() {
    let main = body(
        "",
        USER,
        "\tu := &User{}\n\tn := 3\n\tvar c chan int\n\t/*|*/",
    );
    assert_eq!(eval(&main, "*u"), "app.User");
    assert_eq!(eval(&main, "&n"), "*int");
    assert_eq!(eval(&main, "-n"), "int");
    assert_eq!(eval(&main, "^n"), "int");
    assert_eq!(eval(&main, "!true"), "bool");
    assert_eq!(eval(&main, "<-c"), "int");
    assert_eq!(eval(&main, "*n"), "?");
}

#[test]
fn binary_operators() {
    let main = body("", "", "\tn := 3\n\ts := \"a\"\n\tvar f float32\n\t/*|*/");
    assert_eq!(eval(&main, "n == 3"), "bool");
    assert_eq!(eval(&main, "n < 3 && s != \"\""), "bool");
    assert_eq!(eval(&main, "s + \"x\""), "string");
    assert_eq!(eval(&main, "1 + f"), "float32");
    assert_eq!(eval(&main, "n << 2"), "int");
    assert_eq!(eval(&main, "1 << n"), "int");
    assert_eq!(eval(&main, "n * 2"), "int");
}

#[test]
fn parenthesized_and_deref() {
    let main = body("", USER, "\tu := &User{}\n\t(*u)./*|*/");
    assert_eq!(show(&main), "app.User");
    let main = body("", USER, "\tu := &User{}\n\t(u)./*|*/");
    assert_eq!(show(&main), "*app.User");
}

#[test]
fn indexing_rules() {
    let main = body(
        "",
        "type Names []string\n\ntype Grid [3]int\n",
        "\tvar n Names\n\tvar g Grid\n\tvar p *[4]byte\n\ts := \"abc\"\n\tm := map[int]bool{}\n\t/*|*/",
    );
    assert_eq!(eval(&main, "n[0]"), "string");
    assert_eq!(eval(&main, "g[1]"), "int");
    assert_eq!(eval(&main, "p[1]"), "uint8".replace("uint8", "byte"));
    assert_eq!(eval(&main, "s[0]"), "byte");
    assert_eq!(eval(&main, "m[1]"), "bool");
    assert_eq!(eval(&main, "n[0][1]"), "byte");
}

#[test]
fn slicing_rules() {
    let main = body(
        "",
        "",
        "\txs := []int{}\n\tvar arr [3]string\n\ts := \"abc\"\n\t/*|*/",
    );
    assert_eq!(eval(&main, "xs[1:]"), "[]int");
    assert_eq!(eval(&main, "arr[:2]"), "[]string");
    assert_eq!(eval(&main, "s[1:2]"), "string");
    assert_eq!(eval(&main, "xs[0:1][0]"), "int");
}

#[test]
fn func_literal_type() {
    let main = body(
        "",
        USER,
        "\tf := func(a int, rest ...string) (*User, error) { return nil, nil }\n\t/*|*/",
    );
    assert_eq!(eval(&main, "f"), "func(int, ...string) (*app.User, error)");
    assert_eq!(eval(&main, "func() bool { return true }()"), "bool");
    let main = body("", USER, "\tf := func() *User { return nil }\n\tf()./*|*/");
    assert_eq!(show(&main), "*app.User");
}

#[test]
fn package_level_function_and_value() {
    let decls = "type Item struct{}\n\nfunc NewItem(n int) *Item { return nil }\n\nvar Default = NewItem(1)\n\nvar Typed *Item\n\nconst Limit = 10\n";
    let main = body("", decls, "\t/*|*/");
    assert_eq!(eval(&main, "NewItem(1)"), "*app.Item");
    assert_eq!(eval(&main, "Default"), "*app.Item");
    assert_eq!(eval(&main, "Typed"), "*app.Item");
    assert_eq!(eval(&main, "Limit"), "int");
    assert_eq!(eval(&main, "NewItem"), "func(int) *app.Item");
}

#[test]
fn package_level_cycle_terminates() {
    let decls = "var a = b\n\nvar b = a\n";
    let main = body("", decls, "\t/*|*/");
    assert_eq!(eval(&main, "a"), "?");
}

#[test]
fn local_cycle_terminates() {
    let main = body("", "", "\tx := x\n\tx./*|*/");
    assert_eq!(show(&main), "?");
}

#[test]
fn type_names_are_not_values() {
    let main = body("", USER, "\t/*|*/");
    assert_eq!(eval(&main, "User"), "?");
    assert_eq!(eval(&main, "User.Greet"), "?");
    assert_eq!(eval(&main, "string"), "?");
}

#[test]
fn local_shadows_type_in_call() {
    // `User` is a local function value here, not a conversion.
    let main = body("", USER, "\tUser := func() int { return 1 }\n\t/*|*/");
    assert_eq!(eval(&main, "User()"), "int");
}

#[test]
fn imported_package_name_and_alias() {
    let main = "package main\n\nimport h \"net/http\"\n\nfunc main() {\n\tx := h.Get\n\t/*|*/\n}\n";
    assert_eq!(eval(main, "x"), "func(string) (*http.Response, error)");
}

#[test]
fn other_file_of_package() {
    let files = [(
        "other.go",
        "package main\n\ntype Thing struct{}\n\nfunc MakeThing() *Thing { return nil }\n",
    )];
    let main = "package main\n\nfunc main() {\n\tt := MakeThing()\n\tt./*|*/\n}\n";
    assert_eq!(show_with(&files, main), "*app.Thing");
}

#[test]
fn unresolvable_is_unknown() {
    let main = body("", "", "\t/*|*/");
    assert_eq!(eval(&main, "nope"), "?");
    assert_eq!(eval(&main, "nope.x.y()"), "?");
    assert_eq!(eval(&main, "fmt.Println"), "?");
    assert_eq!(eval(&main, ""), "?");
    assert_eq!(eval(&main, "((("), "?");
}

#[test]
fn deep_nesting_is_bounded() {
    let main = body("", "", "\t/*|*/");
    let deep = format!("{}1{}", "(".repeat(500), ")".repeat(500));
    // Must return, whatever the answer.
    let _ = eval(&main, &deep);
    let long = vec!["1"; 2000].join(" + ");
    let _ = eval(&main, &long);
}
