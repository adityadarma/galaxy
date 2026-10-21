//! Builtins, `range`, type switches and generics.

use std::path::Path;

use crate::go::engine::GoEngine;
use crate::go::modules::GoProject;
use crate::go::types::{eval_text, receiver_ty_at};

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
        "src/errors/errors.go",
        "package errors\n\nfunc New(text string) error { return nil }\n",
    );
    put(
        &g,
        "src/fmt/fmt.go",
        "package fmt\n\nfunc Sprintf(format string, a ...any) string { return \"\" }\n",
    );
    put(
        &g,
        "src/iter/iter.go",
        "package iter\n\ntype Seq[V any] func(yield func(V) bool)\n\ntype Seq2[K, V any] func(yield func(K, V) bool)\n",
    );
    g
}

fn run<R>(
    main: &str,
    f: impl FnOnce(&mut GoEngine, &Path, &str, &tree_sitter::Tree, usize) -> R,
) -> R {
    let tmp = tempfile::tempdir().unwrap();
    let goroot = fake_goroot(tmp.path());
    let app = tmp.path().join("app");
    put(&app, "go.mod", "module example.com/app\n\ngo 1.22\n");
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

fn show(main: &str) -> String {
    run(main, receiver_ty_at).display()
}

fn eval(main: &str, expr: &str) -> String {
    run(main, |e, p, s, t, b| {
        eval_text(e, p, s, t, b, expr).display()
    })
}

const DECLS: &str =
    "type User struct{ Name string }\n\nfunc (u *User) Greet() string { return \"\" }\n\n";

fn body(imports: &str, decls: &str, stmts: &str) -> String {
    format!("package main\n\n{imports}\n{DECLS}{decls}\nfunc main() {{\n{stmts}\n}}\n")
}

/// Type of `expr` after `stmts` ran in `main`.
fn ev(decls: &str, stmts: &str, expr: &str) -> String {
    eval(&body("", decls, &format!("{stmts}\n\t{MARK}")), expr)
}

const GENERICS: &str = "func Map[T, U any](s []T, f func(T) U) []U { return nil }\n\nfunc Filter[T any](s []T, keep func(T) bool) []T { return nil }\n\nfunc Keys[K comparable, V any](m map[K]V) []K { return nil }\n\nfunc Reduce[T, A any](s []T, init A, f func(A, T) A) A { var a A; return a }\n\nfunc Zero[T any]() T { var z T; return z }\n\nfunc First[T any](xs ...T) T { var z T; return z }\n\ntype Box[T any] struct{ V T }\n\nfunc (b *Box[T]) Get() T { return b.V }\n\nfunc Unbox[T any](b *Box[T]) T { return b.V }\n\nfunc Pair[A, B any](a A, b B) (A, B) { return a, b }\n\n";

// ---- builtins ----

#[test]
fn builtin_make() {
    assert_eq!(ev("", "", "make([]User, 3)"), "[]app.User");
    assert_eq!(ev("", "", "make(map[string]int)"), "map[string]int");
    assert_eq!(ev("", "", "make(chan *User, 1)"), "chan *app.User");
}

#[test]
fn builtin_new() {
    assert_eq!(ev("", "", "new(User)"), "*app.User");
    assert_eq!(ev("", "", "new(int)"), "*int");
}

#[test]
fn builtin_make_result_is_usable() {
    let main = body("", "", "\tch := make(chan *User)\n\tv := <-ch\n\tv./*|*/");
    assert_eq!(show(&main), "*app.User");
    let main = body("", "", "\tu := new(User)\n\tu./*|*/");
    assert_eq!(show(&main), "*app.User");
}

#[test]
fn builtin_append() {
    assert_eq!(ev("", "xs := []User{}", "append(xs, User{})"), "[]app.User");
    assert_eq!(ev("", "xs := []int{}", "append(xs, 1, 2, 3)"), "[]int");
}

#[test]
fn builtin_append_variadic_spread() {
    assert_eq!(
        ev("", "xs := []int{}\n\tys := []int{}", "append(xs, ys...)"),
        "[]int"
    );
}

#[test]
fn builtin_int_results() {
    assert_eq!(ev("", "xs := []int{}", "len(xs)"), "int");
    assert_eq!(ev("", "xs := []int{}", "cap(xs)"), "int");
    assert_eq!(
        ev("", "xs := []int{}\n\tys := []int{}", "copy(xs, ys)"),
        "int"
    );
}

#[test]
fn builtin_without_result() {
    for call in [
        "delete(m, 1)",
        "panic(\"x\")",
        "print(1)",
        "println(1)",
        "clear(m)",
    ] {
        assert_eq!(ev("", "m := map[int]int{}", call), "()", "{call}");
    }
    assert_eq!(ev("", "var c chan int", "close(c)"), "()");
}

#[test]
fn builtin_min_max() {
    assert_eq!(ev("", "n := 3", "min(n, 1)"), "int");
    assert_eq!(ev("", "var f float32", "max(1, f)"), "float32");
    assert_eq!(ev("", "", "max(1.5, 2)"), "float64");
}

#[test]
fn builtin_recover_and_complex() {
    assert_eq!(
        ev("", "", "recover()"),
        "interface{}".replace("interface{}", "any")
    );
    assert_eq!(ev("", "", "complex(1, 2)"), "complex128");
    assert_eq!(ev("", "c := 2i", "real(c)"), "float64");
    assert_eq!(ev("", "c := 2i", "imag(c)"), "float64");
}

#[test]
fn builtin_shadowed_by_local() {
    assert_eq!(
        ev(
            "",
            "len := func(s string) *User { return nil }",
            "len(\"a\")"
        ),
        "*app.User"
    );
}

#[test]
fn builtin_shadowed_by_package_func() {
    let decls = "func max(a, b int) string { return \"\" }\n\n";
    assert_eq!(ev(decls, "", "max(1, 2)"), "string");
}

#[test]
fn conversions_still_work() {
    assert_eq!(ev("", "b := []byte{}", "string(b)"), "string");
    assert_eq!(
        ev("", "s := \"a\"", "[]byte(s)"),
        "[]uint8".replace("uint8", "byte")
    );
}

// ---- range ----

fn ranged(decls: &str, setup: &str, header: &str, name: &str) -> String {
    let main = body(
        "",
        decls,
        &format!("{setup}\n\tfor {header} {{\n\t\t{name}./*|*/\n\t}}"),
    );
    show(&main)
}

#[test]
fn range_slice() {
    assert_eq!(
        ranged("", "xs := []*User{}", "i, x := range xs", "x"),
        "*app.User"
    );
    assert_eq!(
        ranged("", "xs := []*User{}", "i, x := range xs", "i"),
        "int"
    );
}

#[test]
fn range_array_and_pointer_to_array() {
    assert_eq!(
        ranged("", "var a [3]User", "_, x := range a", "x"),
        "app.User"
    );
    assert_eq!(
        ranged("", "var a *[3]User", "_, x := range a", "x"),
        "app.User"
    );
}

#[test]
fn range_string() {
    assert_eq!(ranged("", "s := \"abc\"", "i, r := range s", "r"), "rune");
    assert_eq!(ranged("", "s := \"abc\"", "i, r := range s", "i"), "int");
}

#[test]
fn range_map() {
    let setup = "m := map[string]*User{}";
    assert_eq!(ranged("", setup, "k, v := range m", "k"), "string");
    assert_eq!(ranged("", setup, "k, v := range m", "v"), "*app.User");
}

#[test]
fn range_channel() {
    assert_eq!(
        ranged("", "var c chan *User", "x := range c", "x"),
        "*app.User"
    );
}

#[test]
fn range_int() {
    assert_eq!(ranged("", "n := 10", "i := range n", "i"), "int");
    assert_eq!(ranged("", "", "i := range 10", "i"), "int");
}

#[test]
fn range_named_slice_type() {
    assert_eq!(
        ranged(
            "type Users []*User\n\n",
            "var us Users",
            "_, u := range us",
            "u"
        ),
        "*app.User"
    );
}

#[test]
fn range_func_iterator() {
    let setup = "var seq func(yield func(string, *User) bool)";
    assert_eq!(ranged("", setup, "k, v := range seq", "k"), "string");
    assert_eq!(ranged("", setup, "k, v := range seq", "v"), "*app.User");
    let one = "var seq func(yield func(*User) bool)";
    assert_eq!(ranged("", one, "v := range seq", "v"), "*app.User");
}

#[test]
fn range_iter_seq() {
    let main = "package main\n\nimport \"iter\"\n\ntype User struct{}\n\nfunc main() {\n\tvar s iter.Seq[*User]\n\tfor u := range s {\n\t\tu./*|*/\n\t}\n}\n";
    assert_eq!(show(main), "*app.User");
}

#[test]
fn range_iter_seq2() {
    let main = "package main\n\nimport \"iter\"\n\ntype User struct{}\n\nfunc main() {\n\tvar s iter.Seq2[string, *User]\n\tfor k, v := range s {\n\t\tk./*|*/\n\t}\n}\n";
    assert_eq!(show(main), "string");
    let main = main.replace("k./*|*/", "v./*|*/");
    assert_eq!(show(&main), "*app.User");
}

#[test]
fn range_over_call_result() {
    let decls = "func users() []*User { return nil }\n\n";
    assert_eq!(ranged(decls, "", "_, u := range users()", "u"), "*app.User");
}

#[test]
fn range_over_unknown_stays_unknown() {
    assert_eq!(ranged("", "", "_, x := range nope", "x"), "?");
}

// ---- type switch ----

fn switched(case: &str) -> String {
    let main = body(
        "",
        "type Shape struct{}\n\n",
        &format!(
            "\tvar x any\n\tvar s *Shape\n\tswitch v := x.(type) {{\n\t{case}\n\t\tv./*|*/\n\t}}\n\t_ = s"
        ),
    );
    show(&main)
}

#[test]
fn type_switch_single_case() {
    assert_eq!(switched("case *User:"), "*app.User");
    assert_eq!(switched("case Shape:"), "app.Shape");
}

#[test]
fn type_switch_multi_case_uses_subject() {
    assert_eq!(
        switched("case *User, Shape:"),
        "interface{}".replace("interface{}", "any")
    );
}

#[test]
fn type_switch_default_uses_subject() {
    assert_eq!(switched("default:"), "any");
}

#[test]
fn type_switch_nil_case_uses_subject() {
    assert_eq!(switched("case nil:"), "any");
}

#[test]
fn type_switch_subject_typed() {
    let main = body(
        "",
        "",
        "\tvar e error\n\tswitch v := e.(type) {\n\tdefault:\n\t\tv./*|*/\n\t}",
    );
    assert_eq!(show(&main), "error");
}

// ---- comma ok, calls ----

#[test]
fn channel_comma_ok_from_make() {
    let main = body(
        "",
        "",
        "\tch := make(chan *User)\n\tx, ok := <-ch\n\tx./*|*/",
    );
    assert_eq!(show(&main), "*app.User");
    let main = main.replace("x./*|*/", "ok./*|*/");
    assert_eq!(show(&main), "bool");
}

#[test]
fn tuple_result_with_index() {
    let decls = "func pair() (*User, error) { return nil, nil }\n\n";
    let main = body("", decls, "\ta, b := pair()\n\tb./*|*/");
    assert_eq!(show(&main), "error");
    let main = main.replace("b./*|*/", "a./*|*/");
    assert_eq!(show(&main), "*app.User");
}

#[test]
fn closure_variable_call() {
    let main = body(
        "",
        "",
        "\thandler := func(name string) (*User, error) { return nil, nil }\n\tu, err := handler(\"a\")\n\tu./*|*/",
    );
    assert_eq!(show(&main), "*app.User");
}

#[test]
fn func_typed_variable_call() {
    let decls = "type Handler func(req string) *User\n\n";
    let main = body("", decls, "\tvar handler Handler\n\thandler(\"r\")./*|*/");
    assert_eq!(show(&main), "*app.User");
}

#[test]
fn errors_new_receiver() {
    let main = body("import \"errors\"", "", "\terrors.New(\"x\")./*|*/");
    assert_eq!(show(&main), "error");
}

#[test]
fn fmt_sprintf_result() {
    let main = body(
        "import \"fmt\"",
        "",
        "\ts := fmt.Sprintf(\"%d\", 1)\n\ts./*|*/",
    );
    assert_eq!(show(&main), "string");
}

// ---- generics ----

fn generic(stmts: &str, expr: &str) -> String {
    ev(GENERICS, stmts, expr)
}

#[test]
fn generic_map() {
    let setup = "xs := []int{}\n\tf := func(n int) string { return \"\" }";
    assert_eq!(generic(setup, "Map(xs, f)"), "[]string");
}

#[test]
fn generic_map_with_literal_func() {
    let setup = "xs := []*User{}";
    assert_eq!(
        generic(setup, "Map(xs, func(u *User) string { return u.Name })"),
        "[]string"
    );
}

#[test]
fn generic_filter() {
    let setup = "xs := []*User{}";
    assert_eq!(
        generic(setup, "Filter(xs, func(u *User) bool { return true })"),
        "[]*app.User"
    );
}

#[test]
fn generic_keys() {
    assert_eq!(generic("m := map[string]*User{}", "Keys(m)"), "[]string");
}

#[test]
fn generic_reduce() {
    let setup = "xs := []*User{}";
    assert_eq!(
        generic(
            setup,
            "Reduce(xs, 0, func(a int, u *User) int { return a })"
        ),
        "int"
    );
}

#[test]
fn generic_explicit_instantiation() {
    assert_eq!(generic("", "Zero[int]()"), "int");
    assert_eq!(generic("", "Zero[*User]()"), "*app.User");
    assert_eq!(generic("", "Pair[int, string](1, \"a\")"), "(int, string)");
}

#[test]
fn generic_unbound_stays_unknown() {
    assert_eq!(generic("", "Zero()"), "?");
    assert_eq!(generic("", "First()"), "?");
}

#[test]
fn generic_variadic() {
    assert_eq!(generic("u := &User{}", "First(u, u)"), "*app.User");
    assert_eq!(generic("xs := []int{}", "First(xs...)"), "int");
}

#[test]
fn generic_named_type_argument() {
    assert_eq!(generic("b := &Box[*User]{}", "Unbox(b)"), "*app.User");
}

#[test]
fn generic_struct_method_result() {
    let main = body("", GENERICS, "\tb := &Box[*User]{}\n\tb.Get()./*|*/");
    assert_eq!(show(&main), "*app.User");
}

#[test]
fn generic_nested_calls() {
    let setup = "xs := []*User{}\n\tp := func(u *User) bool { return true }\n\tf := func(u *User) string { return \"\" }";
    assert_eq!(generic(setup, "Map(Filter(xs, p), f)"), "[]string");
}

#[test]
fn generic_result_is_usable() {
    let main = body(
        "",
        GENERICS,
        "\tm := map[string]*User{}\n\tks := Keys(m)\n\tfor _, k := range ks {\n\t\tk./*|*/\n\t}",
    );
    assert_eq!(show(&main), "string");
}

#[test]
fn non_generic_call_unchanged() {
    let decls = "func plain(n int) *User { return nil }\n\n";
    assert_eq!(ev(decls, "", "plain(1)"), "*app.User");
}
