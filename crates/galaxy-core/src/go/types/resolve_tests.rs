use std::path::Path;

use super::*;
use crate::go::engine::GoEngine;
use crate::go::modules::GoProject;

const APP: &str = "example.com/app";

fn put(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn fake_goroot(root: &Path) -> std::path::PathBuf {
    let g = root.join("goroot");
    put(
        &g,
        "src/net/http/request.go",
        "package http\n\ntype Request struct {\n\tMethod string\n\tHeader Header\n}\n\nfunc (r *Request) Context() string { return \"\" }\n",
    );
    put(
        &g,
        "src/net/http/header.go",
        "package http\n\ntype Header map[string][]string\n\nfunc (h Header) Get(key string) string { return \"\" }\n",
    );
    put(
        &g,
        "src/errors/errors.go",
        "package errors\n\nfunc New(text string) error { return nil }\n",
    );
    g
}

/// Builds a module with `files` and `main.go` (`main`), then runs `f` on an
/// evaluator for `main.go` and the context of its top.
fn run<R>(files: &[(&str, &str)], main: &str, f: impl FnOnce(&mut Eval, &Ctx) -> R) -> R {
    let tmp = tempfile::tempdir().unwrap();
    let goroot = fake_goroot(tmp.path());
    let app = tmp.path().join("app");
    put(&app, "go.mod", "module example.com/app\n\ngo 1.22\n");
    for (rel, text) in files {
        put(&app, rel, text);
    }
    put(&app, "main.go", main);
    let project = GoProject::detect_with(&app, Some(goroot), None);
    assert!(project.is_some());
    let mut engine = GoEngine::with_project(project);
    let path = engine.root().join("main.go");
    let src = main.to_owned();
    let tree = crate::go::parse::parse(&path, &src).unwrap().1;
    let mut ev = Eval::new(&mut engine, &path, &src, &tree).unwrap();
    let ctx = ev.site_at(0);
    f(&mut ev, &ctx)
}

fn named(pkg: &str, name: &str) -> Ty {
    generic(pkg, name, vec![])
}

fn generic(pkg: &str, name: &str, args: Vec<Ty>) -> Ty {
    Ty::Named {
        pkg: pkg.to_owned(),
        name: name.to_owned(),
        args,
    }
}

fn app(name: &str) -> Ty {
    named(APP, name)
}

fn ptr(t: Ty) -> Ty {
    Ty::Pointer(Box::new(t))
}

fn slice(t: Ty) -> Ty {
    Ty::Slice(Box::new(t))
}

fn func(params: Vec<Ty>, results: Vec<Ty>) -> Ty {
    Ty::Func {
        params,
        results,
        variadic: false,
    }
}

const HEAD: &str = "package main\n\nimport \"net/http\"\n\n";

/// Evaluates `text` in `main.go` made of `HEAD` and `decls`.
fn text_ty(decls: &str, text: &str) -> Ty {
    let main = format!("{HEAD}{decls}");
    run(&[], &main, |ev, ctx| ev.ty_of_text(ctx, text, &Env::new()))
}

#[test]
fn universe_names() {
    for n in [
        "int", "string", "error", "any", "bool", "byte", "rune", "float64",
    ] {
        assert_eq!(text_ty("", n), uni(n), "{n}");
    }
    assert_eq!(text_ty("", "comparable"), uni("comparable"));
}

#[test]
fn pointer_slice_map() {
    assert_eq!(text_ty("", "*int"), ptr(uni("int")));
    assert_eq!(text_ty("", "[]string"), slice(uni("string")));
    assert_eq!(
        text_ty("type User struct{}\n", "[]map[string]User"),
        slice(Ty::Map(Box::new(uni("string")), Box::new(app("User"))))
    );
}

#[test]
fn array_and_chan_shapes() {
    assert_eq!(text_ty("", "[4]int"), Ty::Array(Box::new(uni("int"))));
    assert_eq!(text_ty("", "[...]int"), Ty::Array(Box::new(uni("int"))));
    for text in ["chan int", "chan<- int", "<-chan int"] {
        assert_eq!(text_ty("", text), Ty::Chan(Box::new(uni("int"))), "{text}");
    }
}

#[test]
fn func_shapes() {
    assert_eq!(
        text_ty("", "func(int) (string, error)"),
        func(vec![uni("int")], vec![uni("string"), uni("error")])
    );
    assert_eq!(text_ty("", "func()"), func(vec![], vec![]));
    assert_eq!(
        text_ty("", "func(a int, rest ...string) bool"),
        Ty::Func {
            params: vec![uni("int"), uni("string")],
            results: vec![uni("bool")],
            variadic: true,
        }
    );
}

#[test]
fn ellipsis_is_a_slice() {
    assert_eq!(text_ty("", "...int"), slice(uni("int")));
}

#[test]
fn struct_and_interface_literals() {
    assert_eq!(text_ty("", "struct{ a int }"), Ty::Struct);
    assert_eq!(text_ty("", "interface{ M() }"), Ty::Interface);
    assert_eq!(text_ty("", "any"), uni("any"));
    assert_eq!(text_ty("", "interface{}"), Ty::Interface);
}

#[test]
fn package_types_belong_to_the_package() {
    assert_eq!(text_ty("type T struct{}\n", "T"), app("T"));
    assert_eq!(text_ty("type T struct{}\n", "*T"), ptr(app("T")));
}

#[test]
fn qualified_names_use_imports() {
    assert_eq!(
        text_ty("", "*http.Request"),
        ptr(named("net/http", "Request"))
    );
    assert_eq!(text_ty("", "http.Header"), named("net/http", "Header"));
}

#[test]
fn aliased_import_names() {
    let main = "package main\n\nimport h \"net/http\"\n";
    let ty = run(&[], main, |ev, ctx| {
        ev.ty_of_text(ctx, "h.Request", &Env::new())
    });
    assert_eq!(ty, named("net/http", "Request"));
}

#[test]
fn unknown_names_are_unknown() {
    assert_eq!(text_ty("", "Nope"), Ty::Unknown);
    assert_eq!(text_ty("", "zzz.Nope"), Ty::Unknown);
    assert_eq!(text_ty("", "[]Nope"), slice(Ty::Unknown));
    assert_eq!(text_ty("", ""), Ty::Unknown);
    assert_eq!(text_ty("", "int string"), Ty::Unknown);
}

#[test]
fn env_binds_type_parameters() {
    let main = format!("{HEAD}type Repo[T any] struct{{ v T }}\n");
    run(&[], &main, |ev, ctx| {
        let mut env = Env::new();
        env.insert("T".to_owned(), uni("int"));
        assert_eq!(ev.ty_of_text(ctx, "T", &env), uni("int"));
        assert_eq!(ev.ty_of_text(ctx, "[]T", &env), slice(uni("int")));
        assert_eq!(
            ev.ty_of_text(ctx, "map[string]*T", &env),
            Ty::Map(Box::new(uni("string")), Box::new(ptr(uni("int"))))
        );
        assert_eq!(
            ev.ty_of_text(ctx, "Repo[T]", &env),
            generic(APP, "Repo", vec![uni("int")])
        );
        assert_eq!(ev.ty_of_text(ctx, "T", &Env::new()), Ty::Unknown);
    });
}

#[test]
fn deep_nesting_does_not_panic() {
    let text = format!("{}int", "[]".repeat(300));
    let _ = text_ty("", &text);
    let text = format!("{}int", "*".repeat(300));
    let _ = text_ty("", &text);
}

const USERS: &str = "type Base struct{ ID int }\n\nfunc (b *Base) Hello() string { return \"\" }\n\ntype User struct {\n\tBase\n\tName string\n\tTags []string\n}\n\nfunc (u *User) Save(force bool) error { return nil }\n\nfunc (u User) Label(parts ...string) (string, bool) { return \"\", false }\n";

fn users<R>(f: impl FnOnce(&mut Eval) -> R) -> R {
    let main = format!("{HEAD}{USERS}");
    run(&[], &main, |ev, _| f(ev))
}

#[test]
fn struct_field_types() {
    users(|ev| {
        assert_eq!(ev.member_ty(&app("User"), "Name"), Some(uni("string")));
        assert_eq!(
            ev.member_ty(&app("User"), "Tags"),
            Some(slice(uni("string")))
        );
        assert_eq!(ev.member_ty(&app("User"), "Nope"), None);
    });
}

#[test]
fn promoted_members_through_embedding() {
    users(|ev| {
        assert_eq!(ev.member_ty(&app("User"), "ID"), Some(uni("int")));
        assert_eq!(
            ev.member_ty(&app("User"), "Hello"),
            Some(func(vec![], vec![uni("string")]))
        );
        assert_eq!(ev.member_ty(&app("User"), "Base"), Some(app("Base")));
    });
}

#[test]
fn pointer_receiver_method_is_a_func() {
    users(|ev| {
        let want = Some(func(vec![uni("bool")], vec![uni("error")]));
        assert_eq!(ev.member_ty(&app("User"), "Save"), want);
        assert_eq!(ev.member_ty(&ptr(app("User")), "Save"), want);
    });
}

#[test]
fn variadic_method_keeps_element_type() {
    users(|ev| {
        assert_eq!(
            ev.member_ty(&ptr(app("User")), "Label"),
            Some(Ty::Func {
                params: vec![uni("string")],
                results: vec![uni("string"), uni("bool")],
                variadic: true,
            })
        );
    });
}

const BOX: &str = "type Box[T any] struct{ v T }\n\nfunc (b *Box[U]) Get() U { var z U; return z }\n\nfunc (b *Box[U]) Put(x U, rest ...U) {}\n\ntype Wrapper struct {\n\tBox[string]\n}\n";

#[test]
fn generic_field_and_method_bind_receiver_args() {
    let main = format!("{HEAD}{BOX}");
    run(&[], &main, |ev, _| {
        let boxed = generic(APP, "Box", vec![uni("int")]);
        assert_eq!(ev.member_ty(&boxed, "v"), Some(uni("int")));
        assert_eq!(
            ev.member_ty(&ptr(boxed.clone()), "Get"),
            Some(func(vec![], vec![uni("int")]))
        );
        assert_eq!(
            ev.member_ty(&boxed, "Put"),
            Some(Ty::Func {
                params: vec![uni("int"), uni("int")],
                results: vec![],
                variadic: true,
            })
        );
        // Missing arguments are unknown.
        assert_eq!(ev.member_ty(&app("Box"), "v"), Some(Ty::Unknown));
    });
}

#[test]
fn generic_embedded_in_plain_struct() {
    let main = format!("{HEAD}{BOX}");
    run(&[], &main, |ev, _| {
        assert_eq!(ev.member_ty(&app("Wrapper"), "v"), Some(uni("string")));
        assert_eq!(
            ev.member_ty(&app("Wrapper"), "Get"),
            Some(func(vec![], vec![uni("string")]))
        );
    });
}

#[test]
fn underlying_of_named_slice_and_func() {
    let decls = "type Strs []string\n\ntype H func(int)\n\ntype Chained Strs\n";
    let main = format!("{HEAD}{decls}");
    run(&[], &main, |ev, _| {
        assert_eq!(ev.underlying(&app("Strs")), slice(uni("string")));
        assert_eq!(ev.underlying(&app("H")), func(vec![uni("int")], vec![]));
        assert_eq!(ev.underlying(&app("Chained")), slice(uni("string")));
    });
}

#[test]
fn underlying_of_struct_interface_and_alias() {
    let decls = "type S struct{ a int }\n\ntype I interface{ M() }\n\ntype A = S\n\ntype PS = *S\n";
    let main = format!("{HEAD}{decls}");
    run(&[], &main, |ev, _| {
        assert_eq!(ev.underlying(&app("S")), Ty::Struct);
        assert_eq!(ev.underlying(&app("I")), Ty::Interface);
        assert_eq!(ev.underlying(&app("A")), Ty::Struct);
        assert_eq!(ev.underlying(&app("PS")), ptr(app("S")));
    });
}

#[test]
fn underlying_binds_type_arguments() {
    let main = format!("{HEAD}type List[T any] []T\n");
    run(&[], &main, |ev, _| {
        let list = generic(APP, "List", vec![uni("int")]);
        assert_eq!(ev.underlying(&list), slice(uni("int")));
        assert_eq!(ev.underlying(&app("List")), slice(Ty::Unknown));
    });
}

#[test]
fn underlying_keeps_non_named_and_universe() {
    run(&[], HEAD, |ev, _| {
        assert_eq!(ev.underlying(&slice(uni("int"))), slice(uni("int")));
        assert_eq!(ev.underlying(&Ty::Unknown), Ty::Unknown);
        assert_eq!(ev.underlying(&uni("string")), uni("string"));
        assert_eq!(ev.underlying(&uni("error")), Ty::Interface);
        assert_eq!(ev.underlying(&named("net/http", "Header")), {
            Ty::Map(Box::new(uni("string")), Box::new(slice(uni("string"))))
        });
        assert_eq!(ev.underlying(&named("net/http", "Missing")), Ty::Unknown);
    });
}

#[test]
fn cycles_terminate() {
    let main = format!("{HEAD}type A B\n\ntype B A\n\ntype C = D\n\ntype D = C\n");
    run(&[], &main, |ev, _| {
        assert_eq!(ev.underlying(&app("A")), Ty::Unknown);
        assert_eq!(ev.underlying(&app("C")), Ty::Unknown);
        assert_eq!(ev.member_ty(&app("A"), "x"), None);
    });
}

#[test]
fn interface_methods() {
    let decls = "type Reader interface {\n\tRead(p []byte) (int, error)\n}\n\ntype RC interface {\n\tReader\n\tClose() error\n}\n";
    let main = format!("{HEAD}{decls}");
    run(&[], &main, |ev, _| {
        let read = func(vec![slice(uni("byte"))], vec![uni("int"), uni("error")]);
        assert_eq!(ev.member_ty(&app("Reader"), "Read"), Some(read.clone()));
        assert_eq!(ev.member_ty(&app("RC"), "Read"), Some(read));
        assert_eq!(
            ev.member_ty(&app("RC"), "Close"),
            Some(func(vec![], vec![uni("error")]))
        );
    });
}

#[test]
fn error_has_an_error_method() {
    run(&[], HEAD, |ev, _| {
        assert_eq!(
            ev.member_ty(&uni("error"), "Error"),
            Some(func(vec![], vec![uni("string")]))
        );
        assert_eq!(ev.member_ty(&uni("error"), "Nope"), None);
        assert_eq!(ev.members(&uni("error")).len(), 1);
    });
}

#[test]
fn non_named_receivers_have_no_members() {
    run(&[], HEAD, |ev, _| {
        assert_eq!(ev.member_ty(&slice(uni("int")), "len"), None);
        assert_eq!(ev.member_ty(&Ty::Unknown, "x"), None);
        assert!(ev.members(&slice(uni("int"))).is_empty());
        assert!(ev.members(&Ty::Unknown).is_empty());
        assert!(ev.members(&uni("int")).is_empty());
    });
}

#[test]
fn fields_resolve_in_the_declaring_packages_imports() {
    // `a` imports net/http as `http`; `main.go` binds `http` to nothing.
    let a = "package a\n\nimport \"net/http\"\n\ntype T struct {\n\tR *http.Request\n\tH http.Header\n}\n";
    let main = "package main\n\nimport http \"errors\"\n";
    run(&[("a/a.go", a)], main, |ev, ctx| {
        let t = named("example.com/app/a", "T");
        assert_eq!(
            ev.member_ty(&t, "R"),
            Some(ptr(named("net/http", "Request")))
        );
        assert_eq!(ev.member_ty(&t, "H"), Some(named("net/http", "Header")));
        // In the caller's context `http` is the errors package.
        assert_eq!(
            ev.ty_of_text(ctx, "http.Request", &Env::new()),
            named("errors", "Request")
        );
    });
}

#[test]
fn std_members_and_promotion_from_another_package() {
    let decls = "type Mine struct {\n\thttp.Request\n}\n";
    let main = format!("{HEAD}{decls}");
    run(&[], &main, |ev, _| {
        let req = named("net/http", "Request");
        assert_eq!(ev.member_ty(&req, "Method"), Some(uni("string")));
        assert_eq!(
            ev.member_ty(&req, "Header"),
            Some(named("net/http", "Header"))
        );
        let header = named("net/http", "Header");
        assert_eq!(
            ev.member_ty(&header, "Get"),
            Some(func(vec![uni("string")], vec![uni("string")]))
        );
        assert_eq!(ev.member_ty(&app("Mine"), "Method"), Some(uni("string")));
        assert_eq!(
            ev.member_ty(&app("Mine"), "Context"),
            Some(func(vec![], vec![uni("string")]))
        );
    });
}

#[test]
fn members_lists_promoted_members() {
    users(|ev| {
        let names: Vec<String> = ev
            .members(&ptr(app("User")))
            .into_iter()
            .map(|m| m.name)
            .collect();
        for want in ["Name", "Tags", "ID", "Hello", "Save", "Label", "Base"] {
            assert!(names.iter().any(|n| n == want), "{want} in {names:?}");
        }
        assert!(ev.members(&app("Missing")).is_empty());
    });
}

#[test]
fn call_result_shapes() {
    run(&[], HEAD, |ev, _| {
        assert_eq!(ev.call_result(&func(vec![], vec![])), Ty::Tuple(vec![]));
        assert_eq!(ev.call_result(&func(vec![], vec![uni("int")])), uni("int"));
        assert_eq!(
            ev.call_result(&func(vec![], vec![uni("int"), uni("error")])),
            Ty::Tuple(vec![uni("int"), uni("error")])
        );
        assert_eq!(ev.call_result(&uni("int")), Ty::Unknown);
    });
}
