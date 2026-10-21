use super::*;
use crate::go::parse::TypeKind;

fn put(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

const GO_MOD: &str = "module example.com/app\n\ngo 1.22\n";

/// A module `example.com/app` in `tmp/app` with `files`, and an engine on it.
fn fx(files: &[(&str, &str)]) -> (tempfile::TempDir, GoEngine) {
    fx_mod(GO_MOD, files, None, None)
}

fn fx_mod(
    go_mod: &str,
    files: &[(&str, &str)],
    goroot: Option<PathBuf>,
    modcache: Option<PathBuf>,
) -> (tempfile::TempDir, GoEngine) {
    let tmp = tempfile::tempdir().unwrap();
    let app = tmp.path().join("app");
    put(&app, "go.mod", go_mod);
    for (rel, text) in files {
        put(&app, rel, text);
    }
    let project = GoProject::detect_with(&app, goroot, modcache);
    assert!(project.is_some());
    (tmp, GoEngine::with_project(project))
}

fn member<'a>(members: &'a Members, name: &str) -> &'a Member {
    members
        .find(name)
        .unwrap_or_else(|| panic!("no member {name} in {:?}", names(members)))
}

fn names(members: &Members) -> Vec<&str> {
    members.items.iter().map(|m| m.name.as_str()).collect()
}

fn pkg(engine: &mut GoEngine, path: &str) -> Arc<Package> {
    engine
        .package_by_import(path)
        .unwrap_or_else(|| panic!("package {path}"))
}

#[test]
fn own_module_with_two_packages() {
    let (_tmp, mut e) = fx(&[
        (
            "a/a.go",
            "package a\n\ntype A struct{ N int }\n\nfunc NewA() *A { return nil }\n",
        ),
        (
            "b/b.go",
            "package beta\n\nconst Max = 3\n\nvar Name = \"x\"\n",
        ),
        ("main.go", "package main\n\nfunc main() {}\n"),
    ]);
    let a = pkg(&mut e, "example.com/app/a");
    assert_eq!(a.name, "a");
    assert_eq!(a.import_path, "example.com/app/a");
    assert!(a.type_decl("A").is_some());
    assert!(a.func("NewA").is_some());
    assert!(a.func("Missing").is_none());
    let b = pkg(&mut e, "example.com/app/b");
    assert_eq!(b.name, "beta");
    assert!(b.value("Max").is_some_and(|(_, v)| v.is_const));
    assert!(b.value("Name").is_some_and(|(_, v)| !v.is_const));
    let root = pkg(&mut e, "example.com/app");
    assert_eq!(root.name, "main");
    assert!(e.package_by_import("example.com/app/missing").is_none());
}

#[test]
fn package_at_dir_and_of_file() {
    let (_tmp, mut e) = fx(&[("a/a.go", "package a\ntype A struct{}\n")]);
    let dir = e.project().unwrap().root().join("a");
    let by_dir = e.package_at_dir(&dir).unwrap();
    assert_eq!(by_dir.import_path, "example.com/app/a");
    let by_file = e.package_of_file(&dir.join("a.go")).unwrap();
    assert!(Arc::ptr_eq(&by_dir, &by_file));
    assert!(e.package_at_dir(&dir.join("nope")).is_none());
}

#[test]
fn engine_without_project_reads_a_directory() {
    let tmp = tempfile::tempdir().unwrap();
    put(tmp.path(), "x/x.go", "package x\nfunc F() {}\n");
    let mut e = GoEngine::with_project(None);
    assert!(e.project().is_none());
    let p = e.package_at_dir(&tmp.path().join("x")).unwrap();
    assert_eq!(p.name, "x");
    assert!(p.func("F").is_some());
    assert!(e.package_by_import("example.com/app/x").is_none());
}

#[test]
fn new_detects_the_project() {
    let (tmp, _) = fx(&[("a/a.go", "package a\n")]);
    let e = GoEngine::new(&tmp.path().join("app"));
    assert!(e.project().is_some());
    assert_eq!(e.root(), tmp.path().join("app"));
}

#[test]
fn import_names_alias_blank_dot_and_declared_name() {
    let (_tmp, mut e) = fx(&[
        ("b/b.go", "package beta\n"),
        ("c/c.go", "package c\n"),
        ("odd/o.go", "package weird\n"),
        (
            "a/a.go",
            "package a\n\nimport (\n\tstr \"strings\"\n\t_ \"example.com/app/c\"\n\t. \"fmt\"\n\t\"example.com/app/b\"\n\t\"example.com/app/odd\"\n\t\"example.com/other/lib\"\n)\n",
        ),
    ]);
    let p = pkg(&mut e, "example.com/app/a");
    let names = e.import_names(&p.files[0]);
    assert_eq!(names.get("str").map(String::as_str), Some("strings"));
    assert_eq!(
        names.get("beta").map(String::as_str),
        Some("example.com/app/b")
    );
    assert_eq!(
        names.get("weird").map(String::as_str),
        Some("example.com/app/odd")
    );
    assert_eq!(
        names.get("lib").map(String::as_str),
        Some("example.com/other/lib")
    );
    assert!(!names.contains_key("_") && !names.contains_key("."));
    assert!(!names.contains_key("c") && !names.contains_key("fmt"));
    assert!(!names.contains_key("b"));
}

#[test]
fn import_names_version_suffixes() {
    let (_tmp, mut e) = fx(&[(
        "a/a.go",
        "package a\n\nimport (\n\t\"example.com/lib/v2\"\n\t\"gopkg.in/yaml.v3\"\n\t\"example.com/v1\"\n\t\"github.com/x/vault\"\n)\n",
    )]);
    let p = pkg(&mut e, "example.com/app/a");
    let names = e.import_names(&p.files[0]);
    assert!(names.contains_key("lib"));
    assert!(names.contains_key("yaml"));
    assert!(names.contains_key("example.com") || names.contains_key("v1"));
    assert!(names.contains_key("vault"));
}

#[test]
fn guess_name_cases() {
    assert_eq!(guess_name("fmt"), "fmt");
    assert_eq!(guess_name("net/http"), "http");
    assert_eq!(guess_name("github.com/a/b/v12"), "b");
    assert_eq!(guess_name("gopkg.in/yaml.v3"), "yaml");
    assert_eq!(guess_name("github.com/go-redis/redis/v8"), "redis");
    assert_eq!(guess_name("example.com/v2x"), "v2x");
    assert_eq!(guess_name("example.com/a.b"), "a.b");
}

#[test]
fn import_names_resolve_declared_name_from_module_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("modcache");
    put(&cache, "gopkg.in/foo.v1@v1.0.0/foo.go", "package bar\n");
    let (_t, mut e) = fx_mod(
        &format!("{GO_MOD}\nrequire gopkg.in/foo.v1 v1.0.0\n"),
        &[(
            "a/a.go",
            "package a\n\nimport (\n\t\"gopkg.in/foo.v1\"\n\t\"gopkg.in/yaml.v3\"\n)\n",
        )],
        None,
        Some(cache),
    );
    let p = pkg(&mut e, "example.com/app/a");
    let names = e.import_names(&p.files[0]);
    assert_eq!(
        names.get("bar").map(String::as_str),
        Some("gopkg.in/foo.v1")
    );
    assert!(!names.contains_key("foo"));
    assert_eq!(
        names.get("yaml").map(String::as_str),
        Some("gopkg.in/yaml.v3")
    );
}

#[test]
fn embedded_struct_promotion_two_levels() {
    let (_tmp, mut e) = fx(&[(
        "a/a.go",
        "package a\n\n// Base is the root.\ntype Base struct {\n\t// ID doc.\n\tID int\n}\n\n// Save stores it.\nfunc (b *Base) Save() error { return nil }\n\ntype Mid struct {\n\tBase\n\tName string\n}\n\nfunc (m Mid) Show() {}\n\ntype Top struct {\n\t*Mid\n\tExtra bool\n}\n",
    )]);
    let p = pkg(&mut e, "example.com/app/a");
    let m = e.members_of(&p, "Top");
    assert!(member(&m, "Extra").embedded_via.is_empty());
    assert_eq!(member(&m, "Extra").kind, MemberKind::Field);
    assert!(member(&m, "Mid").embedded_via.is_empty());
    assert_eq!(member(&m, "Name").embedded_via, ["Mid"]);
    assert_eq!(member(&m, "Show").embedded_via, ["Mid"]);
    assert_eq!(member(&m, "Show").owner, "Mid");
    let id = member(&m, "ID");
    assert_eq!(id.embedded_via, ["Mid", "Base"]);
    assert_eq!(id.owner, "Base");
    assert_eq!(id.ty, "int");
    assert_eq!(id.doc, "ID doc.");
    let save = member(&m, "Save");
    assert_eq!(save.embedded_via, ["Mid", "Base"]);
    assert!(save.pointer_receiver);
    assert_eq!(save.ty, "() error");
    assert_eq!(save.doc, "Save stores it.");
    assert!(save.path.ends_with("a/a.go"));
}

#[test]
fn embedded_interface_methods() {
    let (_tmp, mut e) = fx(&[(
        "a/a.go",
        "package a\n\ntype Reader interface {\n\tRead(p []byte) (n int, err error)\n}\n\ntype Closer interface{ Close() error }\n\ntype RC interface {\n\tReader\n\tCloser\n\tFlush()\n}\n\ntype Impl struct {\n\tRC\n}\n\ntype E interface {\n\terror\n\tCode() int\n}\n",
    )]);
    let p = pkg(&mut e, "example.com/app/a");
    let rc = e.members_of(&p, "RC");
    assert_eq!(member(&rc, "Flush").embedded_via.len(), 0);
    let read = member(&rc, "Read");
    assert_eq!(read.kind, MemberKind::Method);
    assert!(!read.pointer_receiver);
    assert_eq!(read.ty, "(p []byte) (n int, err error)");
    assert_eq!(read.embedded_via, ["Reader"]);
    assert_eq!(read.owner, "Reader");
    assert_eq!(member(&rc, "Close").embedded_via, ["Closer"]);
    let imp = e.members_of(&p, "Impl");
    assert_eq!(member(&imp, "Read").embedded_via, ["RC", "Reader"]);
    assert_eq!(member(&imp, "RC").kind, MemberKind::Field);
    let err = e.members_of(&p, "E");
    assert_eq!(member(&err, "Error").ty, "() string");
    assert!(err.find("Code").is_some());
}

#[test]
fn pointer_and_value_receivers() {
    let (_tmp, mut e) = fx(&[
        (
            "a/a.go",
            "package a\n\ntype T struct{ n int }\n\nfunc (t T) Get() int { return t.n }\n\nfunc (t *T) Set(n int) { t.n = n }\n\nfunc (*T) Anon(a, b string, rest ...int) (string, error) { return \"\", nil }\n",
        ),
        ("a/more.go", "package a\n\nfunc (t T) Other() {}\n"),
    ]);
    let p = pkg(&mut e, "example.com/app/a");
    let m = e.members_of(&p, "T");
    assert!(!member(&m, "Get").pointer_receiver);
    assert!(member(&m, "Set").pointer_receiver);
    assert!(member(&m, "Anon").pointer_receiver);
    assert!(!member(&m, "Other").pointer_receiver);
    assert_eq!(member(&m, "Set").ty, "(n int)");
    assert_eq!(
        member(&m, "Anon").ty,
        "(a string, b string, rest ...int) (string, error)"
    );
    assert!(m.find("n").is_some());
    let methods: Vec<_> = p
        .methods_of("T")
        .iter()
        .map(|(_, f)| f.name.clone())
        .collect();
    assert_eq!(methods, ["Anon", "Get", "Other", "Set"]);
}

#[test]
fn generic_receiver_and_type_parameters() {
    let (_tmp, mut e) = fx(&[(
        "a/a.go",
        "package a\n\ntype Server[T any] struct {\n\tItems []T\n\tOne   T\n}\n\nfunc (s *Server[T]) Do(x T) error { return nil }\n\nfunc (s Server[T]) Len() int { return 0 }\n\ntype Wrapper struct {\n\tServer[int]\n}\n\ntype Pair[K comparable, V any] struct{ Key K; Val V }\n",
    )]);
    let p = pkg(&mut e, "example.com/app/a");
    let m = e.members_of(&p, "Server");
    assert_eq!(member(&m, "Items").ty, "[]T");
    assert_eq!(member(&m, "One").ty, "T");
    assert!(member(&m, "Do").pointer_receiver);
    assert_eq!(member(&m, "Do").ty, "(x T) error");
    assert!(!member(&m, "Len").pointer_receiver);
    let w = e.members_of(&p, "Wrapper");
    assert_eq!(member(&w, "Do").embedded_via, ["Server"]);
    assert_eq!(member(&w, "Items").owner, "Server");
    let pair = e.members_of(&p, "Pair");
    assert_eq!(member(&pair, "Key").ty, "K");
    assert_eq!(p.methods_of("Server").len(), 2);
}

#[test]
fn cross_package_embedded_type_has_qualified_owner() {
    let (_tmp, mut e) = fx(&[
        (
            "h/h.go",
            "package h\n\ntype Header map[string][]string\n\nfunc (h Header) Get(key string) string { return \"\" }\n\ntype Meta struct{ Size int }\n\ntype Body struct {\n\tMeta\n}\n",
        ),
        (
            "a/a.go",
            "package a\n\nimport \"example.com/app/h\"\n\ntype Request struct {\n\th.Header\n\t*h.Body\n\tMethod string\n}\n",
        ),
    ]);
    let p = pkg(&mut e, "example.com/app/a");
    let m = e.members_of(&p, "Request");
    let get = member(&m, "Get");
    assert_eq!(get.owner, "h.Header");
    assert_eq!(get.embedded_via, ["Header"]);
    assert!(get.path.ends_with("h/h.go"));
    assert_eq!(member(&m, "Method").owner, "Request");
    let size = member(&m, "Size");
    assert_eq!(size.owner, "h.Meta");
    assert_eq!(size.embedded_via, ["Body", "Meta"]);
    assert_eq!(member(&m, "Header").owner, "Request");
}

#[test]
fn cross_package_embedded_through_alias_import() {
    let (_tmp, mut e) = fx(&[
        (
            "h/h.go",
            "package h\n\ntype Base struct{ ID int }\n\nfunc (Base) Name() string { return \"\" }\n",
        ),
        (
            "a/a.go",
            "package a\n\nimport hh \"example.com/app/h\"\n\ntype T struct{ hh.Base }\n",
        ),
    ]);
    let p = pkg(&mut e, "example.com/app/a");
    let m = e.members_of(&p, "T");
    assert_eq!(member(&m, "ID").owner, "h.Base");
    assert_eq!(member(&m, "Name").owner, "h.Base");
}

#[test]
fn shallow_members_hide_promoted_ones() {
    let (_tmp, mut e) = fx(&[(
        "a/a.go",
        "package a\n\ntype Inner struct{ Name string; Deep int }\n\nfunc (Inner) Run() {}\n\ntype Outer struct {\n\tInner\n\tName int\n}\n\nfunc (Outer) Run() {}\n",
    )]);
    let p = pkg(&mut e, "example.com/app/a");
    let m = e.members_of(&p, "Outer");
    assert_eq!(m.items.iter().filter(|i| i.name == "Name").count(), 1);
    assert_eq!(member(&m, "Name").ty, "int");
    assert!(member(&m, "Name").embedded_via.is_empty());
    assert_eq!(m.items.iter().filter(|i| i.name == "Run").count(), 1);
    assert_eq!(member(&m, "Run").owner, "Outer");
    assert_eq!(member(&m, "Deep").embedded_via, ["Inner"]);
}

#[test]
fn named_alias_and_underlying_types() {
    let (_tmp, mut e) = fx(&[(
        "a/a.go",
        "package a\n\ntype Base struct{ ID int }\n\nfunc (Base) Save() {}\n\ntype Alias = Base\n\ntype Derived Base\n\ntype Reader interface{ Read() }\n\ntype MyReader Reader\n\ntype Num int\n\nfunc (Num) Double() Num { return 0 }\n",
    )]);
    let p = pkg(&mut e, "example.com/app/a");
    let alias = e.members_of(&p, "Alias");
    assert!(alias.find("ID").is_some() && alias.find("Save").is_some());
    let derived = e.members_of(&p, "Derived");
    assert!(derived.find("ID").is_some());
    assert!(derived.find("Save").is_none());
    let mr = e.members_of(&p, "MyReader");
    assert_eq!(member(&mr, "Read").kind, MemberKind::Method);
    let num = e.members_of(&p, "Num");
    assert_eq!(names(&num), ["Double"]);
    assert!(e.members_of(&p, "Nope").items.is_empty());
    assert_eq!(p.type_decl("Alias").unwrap().1.kind, TypeKind::Alias);
}

#[test]
fn embedding_cycles_terminate() {
    let (_tmp, mut e) = fx(&[(
        "a/a.go",
        "package a\n\ntype A struct {\n\t*B\n\tX int\n}\n\ntype B struct {\n\tA\n\tY int\n}\n\ntype S struct {\n\t*S\n\tZ int\n}\n\ntype I interface{ J; M() }\n\ntype J interface{ I; N() }\n\ntype L1 = L2\n\ntype L2 = L1\n\ntype N1 N2\n\ntype N2 N1\n",
    )]);
    let p = pkg(&mut e, "example.com/app/a");
    let a = e.members_of(&p, "A");
    assert_eq!(member(&a, "Y").embedded_via, ["B"]);
    assert!(a.find("X").is_some());
    assert!(e.members_of(&p, "S").find("Z").is_some());
    let i = e.members_of(&p, "I");
    assert!(i.find("M").is_some() && i.find("N").is_some());
    assert!(e.members_of(&p, "L1").items.is_empty());
    assert!(e.members_of(&p, "N1").items.is_empty());
}

#[test]
fn open_document_overrides_disk() {
    let (_tmp, mut e) = fx(&[("a/a.go", "package a\n\ntype T struct{ Disk int }\n")]);
    let p = pkg(&mut e, "example.com/app/a");
    assert!(e.members_of(&p, "T").find("Disk").is_some());
    let path = p.dir.join("a.go");
    e.open_document(&path, "package a\n\ntype T struct{ Buffer int }\n");
    let p = pkg(&mut e, "example.com/app/a");
    let m = e.members_of(&p, "T");
    assert!(m.find("Buffer").is_some() && m.find("Disk").is_none());
    e.open_document(&p.dir.join("new.go"), "package a\n\ntype Fresh struct{}\n");
    let p = pkg(&mut e, "example.com/app/a");
    assert!(p.type_decl("Fresh").is_some());
    e.close_document(&path);
    e.close_document(&p.dir.join("new.go"));
    let p = pkg(&mut e, "example.com/app/a");
    assert!(e.members_of(&p, "T").find("Disk").is_some());
    assert!(p.type_decl("Fresh").is_none());
}

#[test]
fn open_buffer_of_unsaved_file_counts_for_package_of_file() {
    let (_tmp, mut e) = fx(&[("a/a.go", "package a\n")]);
    let dir = e.project().unwrap().root().join("a");
    e.open_document(&dir.join("draft.go"), "package a\n\nfunc Draft() {}\n");
    let p = e.package_of_file(&dir.join("draft.go")).unwrap();
    assert!(p.func("Draft").is_some());
}

#[test]
fn disk_changes_are_picked_up() {
    let (_tmp, mut e) = fx(&[("a/a.go", "package a\n\ntype T struct{ One int }\n")]);
    let p = pkg(&mut e, "example.com/app/a");
    assert!(e.members_of(&p, "T").find("One").is_some());
    let dir = p.dir.clone();
    put(
        &dir,
        "a.go",
        "package a\n\ntype T struct{ Two int; More string }\n",
    );
    let p = pkg(&mut e, "example.com/app/a");
    let m = e.members_of(&p, "T");
    assert!(m.find("Two").is_some() && m.find("One").is_none());
    put(&dir, "b.go", "package a\n\nfunc Added() {}\n");
    assert!(pkg(&mut e, "example.com/app/a").func("Added").is_some());
    std::fs::remove_file(dir.join("b.go")).unwrap();
    assert!(pkg(&mut e, "example.com/app/a").func("Added").is_none());
}

#[test]
fn test_files_only_with_a_test_file() {
    let (_tmp, mut e) = fx(&[
        ("a/a.go", "package a\n\nfunc Real() {}\n"),
        (
            "a/a_test.go",
            "package a\n\nfunc TestReal() {}\n\nfunc helper() {}\n",
        ),
        ("a/ext_test.go", "package a_test\n\nfunc TestExt() {}\n"),
    ]);
    let p = pkg(&mut e, "example.com/app/a");
    assert_eq!(p.files.len(), 1);
    assert!(p.func("TestReal").is_none());
    let dir = p.dir.clone();
    let from_src = e.package_of_file(&dir.join("a.go")).unwrap();
    assert!(from_src.func("TestReal").is_none());
    let from_test = e.package_of_file(&dir.join("a_test.go")).unwrap();
    assert_eq!(from_test.name, "a");
    assert!(from_test.func("Real").is_some());
    assert!(from_test.func("TestReal").is_some());
    assert!(from_test.func("TestExt").is_some());
    assert_eq!(from_test.files.len(), 3);
}

#[test]
fn build_constraints_exclude_files() {
    let (_tmp, mut e) = fx(&[
        ("a/a.go", "package a\n\nfunc Always() {}\n"),
        (
            "a/win.go",
            "//go:build windows\n\npackage a\n\nfunc OnlyWindows() {}\n",
        ),
        (
            "a/not_win.go",
            "//go:build !windows\n\npackage a\n\nfunc NotWindows() {}\n",
        ),
        (
            "a/ign.go",
            "//go:build ignore\n\npackage main\n\nfunc main() {}\n",
        ),
        ("a/x_windows.go", "package a\n\nfunc BySuffix() {}\n"),
    ]);
    let p = pkg(&mut e, "example.com/app/a");
    assert!(p.func("Always").is_some());
    assert!(p.func("NotWindows").is_some());
    assert!(p.func("OnlyWindows").is_none());
    assert!(p.func("BySuffix").is_none());
    assert_eq!(p.name, "a");
    let dir = p.dir.clone();
    e.open_document(
        &dir.join("buf.go"),
        "//go:build windows\n\npackage a\n\nfunc Buf() {}\n",
    );
    assert!(pkg(&mut e, "example.com/app/a").func("Buf").is_none());
    e.open_document(&dir.join("win.go"), "package a\n\nfunc OnlyWindows() {}\n");
    assert!(
        pkg(&mut e, "example.com/app/a")
            .func("OnlyWindows")
            .is_some()
    );
}

#[test]
fn package_name_is_the_most_common_non_main_clause() {
    let (_tmp, mut e) = fx(&[
        ("t/main.go", "package main\n\nfunc main() {}\n"),
        ("t/x.go", "package tool\n"),
        ("t/y.go", "package tool\n"),
        ("t/z.go", "package other\n"),
        ("m/m.go", "package main\n"),
    ]);
    let t = pkg(&mut e, "example.com/app/t");
    assert_eq!(t.name, "tool");
    // Files of other package clauses (`main`, `other`) are left out.
    assert_eq!(t.files.len(), 2);
    assert_eq!(pkg(&mut e, "example.com/app/m").name, "main");
}

#[test]
fn vendor_directory_resolves_imports() {
    let (_tmp, mut e) = fx(&[
        (
            "vendor/modules.txt",
            "# github.com/a/b v1.0.0\n## explicit\ngithub.com/a/b\n",
        ),
        (
            "vendor/github.com/a/b/b.go",
            "package b\n\ntype V struct{ N int }\n",
        ),
        ("vendor/github.com/a/c/c.go", "package c\n"),
        ("a/a.go", "package a\n\nimport \"github.com/a/b\"\n"),
    ]);
    let p = pkg(&mut e, "github.com/a/b");
    assert_eq!(p.name, "b");
    assert!(p.type_decl("V").is_some());
    assert!(e.package_by_import("github.com/a/c").is_none());
    let dir = p.dir.clone();
    assert_eq!(
        e.package_at_dir(&dir).unwrap().import_path,
        "github.com/a/b"
    );
    let a = pkg(&mut e, "example.com/app/a");
    assert_eq!(
        e.import_names(&a.files[0]).get("b").map(String::as_str),
        Some("github.com/a/b")
    );
}

#[test]
fn replace_to_a_local_directory() {
    let tmp = tempfile::tempdir().unwrap();
    put(
        tmp.path(),
        "ext/ext.go",
        "package ext\n\ntype X struct{ Y int }\n",
    );
    put(tmp.path(), "ext/sub/sub.go", "package sub\n");
    let (_t, mut e) = fx_mod(
        &format!(
            "{GO_MOD}\nrequire example.com/ext v0.0.0\n\nreplace example.com/ext => {}\n",
            tmp.path().join("ext").display()
        ),
        &[("a/a.go", "package a\n")],
        None,
        None,
    );
    let p = pkg(&mut e, "example.com/ext");
    assert_eq!(p.name, "ext");
    assert!(e.members_of(&p, "X").find("Y").is_some());
    assert_eq!(pkg(&mut e, "example.com/ext/sub").name, "sub");
}

fn fake_goroot(root: &Path) -> PathBuf {
    let g = root.join("goroot");
    put(
        &g,
        "src/net/http/request.go",
        "package http\n\nimport \"context\"\n\n// A Request represents an HTTP request.\ntype Request struct {\n\tMethod string\n\tHeader Header\n\tctx    context.Context\n}\n\n// Context returns the request's context.\nfunc (r *Request) Context() context.Context { return r.ctx }\n",
    );
    put(
        &g,
        "src/net/http/header.go",
        "package http\n\ntype Header map[string][]string\n\nfunc (h Header) Get(key string) string { return \"\" }\n",
    );
    put(
        &g,
        "src/net/http/client.go",
        "package http\n\ntype Client struct{ Timeout int }\n\nfunc (c *Client) Do(req *Request) (*Response, error) { return nil, nil }\n\ntype Response struct{ Request *Request }\n",
    );
    put(
        &g,
        "src/net/http/client_test.go",
        "package http\n\nfunc TestX() {}\n",
    );
    put(
        &g,
        "src/context/context.go",
        "package context\n\ntype Context interface{ Done() <-chan struct{} }\n",
    );
    put(&g, "src/internal/abi/abi.go", "package abi\n");
    g
}

#[test]
fn std_packages_from_a_fake_goroot() {
    let tmp = tempfile::tempdir().unwrap();
    let goroot = fake_goroot(tmp.path());
    let (_t, mut e) = fx_mod(
        GO_MOD,
        &[(
            "a/a.go",
            "package a\n\nimport \"net/http\"\n\ntype H struct{ http.Client }\n",
        )],
        Some(goroot),
        None,
    );
    let http = pkg(&mut e, "net/http");
    assert_eq!(http.name, "http");
    assert_eq!(http.import_path, "net/http");
    assert!(http.func("TestX").is_none());
    let req = e.members_of(&http, "Request");
    assert_eq!(member(&req, "Method").kind, MemberKind::Field);
    assert_eq!(member(&req, "Header").ty, "Header");
    let ctx = member(&req, "Context");
    assert_eq!(ctx.kind, MemberKind::Method);
    assert!(ctx.pointer_receiver);
    assert_eq!(ctx.doc, "Context returns the request's context.");
    assert!(e.members_of(&http, "Client").find("Do").is_some());
    let a = pkg(&mut e, "example.com/app/a");
    let h = e.members_of(&a, "H");
    assert_eq!(member(&h, "Do").owner, "http.Client");
    assert_eq!(member(&h, "Timeout").embedded_via, ["Client"]);
    let std = e.std_package_paths();
    assert!(std.iter().any(|p| p == "net/http"));
    assert!(std.iter().any(|p| p == "context"));
    assert!(!std.iter().any(|p| p.contains("internal")));
}

#[test]
fn file_scope_lookup() {
    let (_tmp, mut e) = fx(&[
        ("b/b.go", "package beta\n"),
        (
            "a/a.go",
            "package a\n\nimport (\n\t\"example.com/app/b\"\n\tstr \"strings\"\n)\n\ntype T struct{}\n\nfunc F() {}\n\nvar V = 1\n\nconst C = 2\n\nfunc max(a, b int) int { return a }\n",
        ),
    ]);
    let p = pkg(&mut e, "example.com/app/a");
    let file = p.files[0].clone();
    assert!(
        matches!(e.lookup_in_file_scope(&file, &p, "T"), Some(FileSymbol::Type { name, .. }) if name == "T")
    );
    assert!(
        matches!(e.lookup_in_file_scope(&file, &p, "F"), Some(FileSymbol::Func { name, .. }) if name == "F")
    );
    assert!(matches!(
        e.lookup_in_file_scope(&file, &p, "V"),
        Some(FileSymbol::Value { .. })
    ));
    assert!(matches!(
        e.lookup_in_file_scope(&file, &p, "C"),
        Some(FileSymbol::Value { .. })
    ));
    assert!(
        matches!(e.lookup_in_file_scope(&file, &p, "beta"), Some(FileSymbol::Import { path }) if path == "example.com/app/b")
    );
    assert!(
        matches!(e.lookup_in_file_scope(&file, &p, "str"), Some(FileSymbol::Import { path }) if path == "strings")
    );
    assert!(e.lookup_in_file_scope(&file, &p, "b").is_none());
    assert!(e.lookup_in_file_scope(&file, &p, "missing").is_none());
    assert!(e.lookup_in_file_scope(&file, &p, "").is_none());
    for builtin in [
        "int", "string", "error", "any", "len", "append", "make", "true", "nil", "iota", "min",
        "clear",
    ] {
        assert!(
            matches!(
                e.lookup_in_file_scope(&file, &p, builtin),
                Some(FileSymbol::Builtin)
            ),
            "{builtin}"
        );
    }
    // A package-level declaration hides the universe.
    assert!(matches!(
        e.lookup_in_file_scope(&file, &p, "max"),
        Some(FileSymbol::Func { .. })
    ));
}

#[test]
fn file_scope_lookup_returns_the_declaring_package() {
    let (_tmp, mut e) = fx(&[
        ("a/a.go", "package a\n\ntype T struct{}\n"),
        ("a/b.go", "package a\n\nfunc Use() {}\n"),
    ]);
    let p = pkg(&mut e, "example.com/app/a");
    let second = p.files[1].clone();
    match e.lookup_in_file_scope(&second, &p, "T") {
        Some(FileSymbol::Type { pkg, name }) => {
            assert_eq!(pkg.import_path, "example.com/app/a");
            assert_eq!(name, "T");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn exported_names_are_sorted_and_unique() {
    let (_tmp, mut e) = fx(&[
        (
            "a/b.go",
            "package a\n\nfunc Zed() {}\n\nfunc hidden() {}\n\nfunc (T) Method() {}\n\ntype T struct{}\n",
        ),
        (
            "a/a.go",
            "package a\n\nconst Alpha = 1\n\nvar Beta, gamma = 1, 2\n\ntype Zed2 int\n\nfunc Zed() {}\n",
        ),
    ]);
    let p = pkg(&mut e, "example.com/app/a");
    assert_eq!(p.exported_names(), ["Alpha", "Beta", "T", "Zed", "Zed2"]);
}

#[test]
fn duplicate_declarations_first_file_by_path_wins() {
    let (_tmp, mut e) = fx(&[
        (
            "a/b.go",
            "package a\n\ntype T struct{ FromB int }\n\n// B doc.\nfunc (T) M() {}\n",
        ),
        (
            "a/a.go",
            "package a\n\ntype T struct{ FromA int }\n\n// A doc.\nfunc (T) M() {}\n",
        ),
    ]);
    let p = pkg(&mut e, "example.com/app/a");
    let (file, decl) = p.type_decl("T").unwrap();
    assert!(file.path.ends_with("a.go"));
    assert_eq!(decl.fields[0].name, "FromA");
    let methods = p.methods_of("T");
    assert_eq!(methods.len(), 1);
    assert_eq!(methods[0].1.doc, "A doc.");
    let m = e.members_of(&p, "T");
    assert_eq!(member(&m, "M").doc, "A doc.");
}

#[test]
fn malformed_code_does_not_panic() {
    let (_tmp, mut e) = fx(&[
        (
            "a/a.go",
            "package a\n\ntype X struct {\n\tFoo\n\t*\n\tBar `\n}\n\nfunc (((( broken\n\ntype Y interface { Z; ;; }\n\ntype\n",
        ),
        ("a/bin.go", "\u{0}\u{1}\u{2} not go"),
        ("a/empty.go", ""),
    ]);
    std::fs::write(
        e.project().unwrap().root().join("a/lat.go"),
        [0xff, 0xfe, b'p', 0x80],
    )
    .unwrap();
    let p = pkg(&mut e, "example.com/app/a");
    let _ = e.members_of(&p, "X");
    let _ = e.members_of(&p, "Y");
    let _ = p.exported_names();
    let file = p.files[0].clone();
    let _ = e.import_names(&file);
    let _ = e.lookup_in_file_scope(&file, &p, "Foo");
}

#[test]
fn parsed_file_cache_is_capped() {
    let files: Vec<(String, String)> = (0..12)
        .map(|i| {
            (
                format!("a/f{i:02}.go"),
                format!("package a\n\nfunc F{i}() {{}}\n"),
            )
        })
        .collect();
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let (_tmp, mut e) = fx(&refs);
    e.file_cap = 4;
    let p = pkg(&mut e, "example.com/app/a");
    assert_eq!(p.files.len(), 12);
    assert!(e.parsed.len() <= 4);
    assert!(p.func("F11").is_some());
}

#[test]
fn reset_index_drops_caches_and_keeps_buffers() {
    let (_tmp, mut e) = fx(&[("a/a.go", "package a\n\nfunc One() {}\n")]);
    let p = pkg(&mut e, "example.com/app/a");
    let dir = p.dir.clone();
    e.open_document(&dir.join("buf.go"), "package a\n\nfunc Buf() {}\n");
    let _ = pkg(&mut e, "example.com/app/a");
    assert!(!e.parsed.is_empty());
    e.reset_index();
    assert!(e.parsed.is_empty() && e.packages.is_empty());
    assert!(e.project().is_some());
    let p = pkg(&mut e, "example.com/app/a");
    assert!(p.func("One").is_some() && p.func("Buf").is_some());
}

#[test]
fn build_install_and_readiness() {
    let (tmp, mut e) = fx(&[
        ("a/a.go", "package a\n\ntype T struct{ N int }\n"),
        ("a/sub/s.go", "package sub\n"),
        ("node_modules/x/x.go", "package x\n"),
    ]);
    let root = tmp.path().join("app");
    assert!(!e.is_ready());
    // Computed lazily before the index is installed.
    assert!(
        e.module_package_paths()
            .iter()
            .any(|p| p == "example.com/app/a")
    );
    assert!(e.wants_index(Duration::from_secs(3600)));
    assert!(!e.wants_index(Duration::from_secs(3600)));
    let built = GoEngine::build(&root);
    e.install(built);
    assert!(e.is_ready());
    let module = e.module_package_paths().to_vec();
    assert!(module.contains(&"example.com/app/a".to_owned()));
    assert!(module.contains(&"example.com/app/a/sub".to_owned()));
    assert!(module.windows(2).all(|w| w[0] <= w[1]));
    assert!(!e.parsed.is_empty());
    assert!(!e.wants_index(Duration::from_secs(3600)));
    std::thread::sleep(Duration::from_millis(5));
    assert!(e.wants_index(Duration::ZERO));
    let p = pkg(&mut e, "example.com/app/a");
    assert!(e.members_of(&p, "T").find("N").is_some());
}

#[test]
fn build_without_a_project_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let built = GoEngine::build(tmp.path());
    let mut e = GoEngine::with_project(None);
    e.install(built);
    assert!(e.is_ready());
    assert!(e.module_package_paths().is_empty());
    assert!(e.std_package_paths().is_empty());
}

#[test]
fn build_installs_framework_and_library_markers() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("app");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("go.mod"),
        "module example.com/app\n\nrequire (\n\tgithub.com/goravel/framework v1.16.3\n\tgithub.com/gofiber/fiber/v2 v2.52.10\n)\n",
    )
    .unwrap();
    let mut engine = GoEngine::with_project(None);
    engine.install(GoEngine::build(&root));
    assert_eq!(
        engine
            .integrations()
            .iter()
            .map(|integration| (integration.name, integration.kind))
            .collect::<Vec<_>>(),
        vec![
            ("Goravel", crate::go::GoIntegrationKind::Framework),
            ("Fiber", crate::go::GoIntegrationKind::Library),
        ]
    );
}

#[test]
fn workspace_modules_are_indexed() {
    let tmp = tempfile::tempdir().unwrap();
    put(
        tmp.path(),
        "go.work",
        "go 1.22\n\nuse (\n\t./one\n\t./two\n)\n",
    );
    put(tmp.path(), "one/go.mod", "module example.com/one\n");
    put(tmp.path(), "one/o.go", "package one\n\ntype O struct{}\n");
    put(tmp.path(), "two/go.mod", "module example.com/two\n");
    put(
        tmp.path(),
        "two/t.go",
        "package two\n\nimport \"example.com/one\"\n\ntype T struct{ one.O }\n",
    );
    let mut e = GoEngine::with_project(GoProject::detect_with(tmp.path(), None, None));
    let paths = e.module_package_paths().to_vec();
    assert!(paths.contains(&"example.com/one".to_owned()));
    assert!(paths.contains(&"example.com/two".to_owned()));
    let two = pkg(&mut e, "example.com/two");
    let m = e.members_of(&two, "T");
    assert_eq!(member(&m, "O").kind, MemberKind::Field);
}
