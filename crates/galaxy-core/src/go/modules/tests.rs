use super::*;

fn put(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn goroot(root: &Path) -> PathBuf {
    let g = root.join("goroot");
    for file in [
        "src/runtime/rt.go",
        "src/net/http/h.go",
        "src/os/os.go",
        "src/strings/strings.go",
        "src/strings/strings_test.go",
        "src/onlytest/a_test.go",
        "src/encoding/json/j.go",
        "src/encoding/notes.txt",
        "src/vendor/golang.org/x/net/idna/idna.go",
        "src/cmd/go/main.go",
        "src/internal/abi/abi.go",
        "src/net/http/internal/i.go",
        "src/net/testdata/t.go",
        "src/net/_skip/s.go",
        "src/empty/.keep",
    ] {
        put(&g, file, "package x\n");
    }
    g
}

fn app(root: &Path, go_mod: &str) -> PathBuf {
    let dir = root.join("app");
    put(&dir, "go.mod", go_mod);
    dir
}

#[test]
fn no_go_files_means_no_project() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(GoProject::detect_with(tmp.path(), None, None).is_none());
    std::fs::create_dir_all(tmp.path().join("go.mod")).unwrap();
    assert!(GoProject::detect_with(tmp.path(), None, None).is_none());
}

#[test]
fn parses_go_mod_directives() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = app(
        tmp.path(),
        "// header\nmodule \"example.com/app\" // trailing\ngo 1.25.0\ntoolchain go1.25.1\n\
             require github.com/a/b v1.2.3\n\
             require (\n\tgithub.com/c/d v0.1.0 // indirect\n\t\"github.com/e/f\" v2.0.0+incompatible\n)\n\
             exclude github.com/x/y v1.0.0\n\
             retract (\n\tv1.0.0\n\t[v1.1.0, v1.2.0]\n)\n",
    );
    let project = GoProject::detect_with(&dir, None, None).unwrap();
    let main = project.main_module().unwrap();
    assert_eq!(main.path, "example.com/app");
    assert_eq!(main.go_version.as_deref(), Some("1.25.0"));
    assert_eq!(main.dir, dir);
    assert_eq!(
        project.required_modules(),
        [
            ("github.com/a/b".to_owned(), "v1.2.3".to_owned()),
            ("github.com/c/d".to_owned(), "v0.1.0".to_owned()),
            (
                "github.com/e/f".to_owned(),
                "v2.0.0+incompatible".to_owned()
            ),
        ]
    );
}

#[test]
fn detect_walks_up_and_accepts_files() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = app(tmp.path(), "module example.com/app\n");
    put(&dir, "a/b/c/main.go", "package main\n");
    let from_sub = GoProject::detect_with(&dir.join("a/b/c"), None, None).unwrap();
    assert_eq!(from_sub.root(), dir);
    let from_file = GoProject::detect_with(&dir.join("a/b/c/main.go"), None, None).unwrap();
    assert_eq!(from_file.root(), dir);
}

#[test]
fn resolves_main_module_packages() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = app(tmp.path(), "module example.com/app\n");
    put(&dir, "internal/util/u.go", "package util\n");
    let project = GoProject::detect_with(&dir, None, None).unwrap();
    assert_eq!(project.import_dir("example.com/app"), Some(dir.clone()));
    assert_eq!(
        project.import_dir("example.com/app/internal/util"),
        Some(dir.join("internal/util"))
    );
    assert_eq!(project.import_dir("example.com/app/missing"), None);
    assert_eq!(project.import_dir("example.com/appx/util"), None);
}

#[test]
fn resolves_std_and_std_vendor() {
    let tmp = tempfile::tempdir().unwrap();
    let g = goroot(tmp.path());
    let dir = app(tmp.path(), "module example.com/app\n");
    let project = GoProject::detect_with(&dir, Some(g.clone()), None).unwrap();
    assert_eq!(project.goroot(), Some(g.as_path()));
    assert_eq!(project.import_dir("net/http"), Some(g.join("src/net/http")));
    assert_eq!(
        project.import_dir("golang.org/x/net/idna"),
        Some(g.join("src/vendor/golang.org/x/net/idna"))
    );
    assert_eq!(project.import_dir("nope/pkg"), None);
}

#[test]
fn rejects_unsafe_import_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let g = goroot(tmp.path());
    let project =
        GoProject::detect_with(&app(tmp.path(), "module example.com/app\n"), Some(g), None)
            .unwrap();
    for bad in [
        "",
        "../etc",
        "net/../os",
        "net//http",
        "/os",
        "net\\http",
        ".",
    ] {
        assert_eq!(project.import_dir(bad), None, "{bad:?}");
    }
}

#[test]
fn local_replace_directives() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = app(
        tmp.path(),
        "module example.com/app\nrequire example.com/lib v1.0.0\n\
             replace example.com/lib => ../lib\n\
             replace (\n\texample.com/vend v1.0.0 => ./third_party/vend\n)\n",
    );
    put(tmp.path(), "lib/sub/x.go", "package sub\n");
    put(&dir, "third_party/vend/v.go", "package vend\n");
    let project = GoProject::detect_with(&dir, None, None).unwrap();
    assert_eq!(
        project.import_dir("example.com/lib/sub"),
        Some(tmp.path().join("lib/sub"))
    );
    assert_eq!(
        project.import_dir("example.com/lib"),
        Some(tmp.path().join("lib"))
    );
    // Versioned replace applies only to that version; vend is not required.
    assert_eq!(project.import_dir("example.com/vend"), None);
}

#[test]
fn module_replace_uses_cache_and_escapes() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("mod");
    put(&cache, "example.com/!new@v1.5.0/pkg/p.go", "package pkg\n");
    let dir = app(
        tmp.path(),
        "module example.com/app\nrequire example.com/old v1.0.0\n\
             replace example.com/old => example.com/New v1.5.0\n",
    );
    let project = GoProject::detect_with(&dir, None, Some(cache.clone())).unwrap();
    assert_eq!(
        project.import_dir("example.com/old/pkg"),
        Some(cache.join("example.com/!new@v1.5.0/pkg"))
    );
}

#[test]
fn replace_for_other_version_is_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("mod");
    put(&cache, "example.com/old@v1.0.0/pkg/p.go", "package pkg\n");
    put(tmp.path(), "x/pkg/p.go", "package pkg\n");
    let dir = app(
        tmp.path(),
        "module example.com/app\nrequire example.com/old v1.0.0\n\
             replace example.com/old v2.0.0 => ../x\n",
    );
    let project = GoProject::detect_with(&dir, None, Some(cache.clone())).unwrap();
    assert_eq!(
        project.import_dir("example.com/old/pkg"),
        Some(cache.join("example.com/old@v1.0.0/pkg"))
    );
}

#[test]
fn cyclic_replaces_terminate() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = app(
        tmp.path(),
        "module a.com/app\nreplace a.com/x => ./\n\
             replace a.com/x => a.com/y v1.0.0\nreplace a.com/y => a.com/x v1.0.0\n",
    );
    put(&dir, "pkg/p.go", "package pkg\n");
    let cache = tmp.path().join("mod");
    std::fs::create_dir_all(&cache).unwrap();
    let project = GoProject::detect_with(&dir, None, Some(cache)).unwrap();
    assert_eq!(project.import_dir("a.com/x/pkg"), Some(dir.join("pkg")));
    assert_eq!(project.import_dir("a.com/y/pkg"), None);
    assert!(project.dependency_packages("a.com/y").is_empty());
}

#[test]
fn cache_lookup_uses_longest_module_and_case_encoding() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("mod");
    put(&cache, "github.com/!azure/sdk@v1.0.0/x/x.go", "package x\n");
    put(
        &cache,
        "github.com/!azure/sdk/sub@v2.0.0/y/y.go",
        "package y\n",
    );
    let dir = app(
        tmp.path(),
        "module example.com/app\nrequire (\n\tgithub.com/Azure/sdk v1.0.0\n\
             \tgithub.com/Azure/sdk/sub v2.0.0\n)\n",
    );
    let project = GoProject::detect_with(&dir, None, Some(cache.clone())).unwrap();
    assert_eq!(
        project.import_dir("github.com/Azure/sdk/sub/y"),
        Some(cache.join("github.com/!azure/sdk/sub@v2.0.0/y"))
    );
    assert_eq!(
        project.import_dir("github.com/Azure/sdk/x"),
        Some(cache.join("github.com/!azure/sdk@v1.0.0/x"))
    );
    assert_eq!(project.import_dir("github.com/Azure/sdk/sub/zzz"), None);
    assert_eq!(project.import_dir("github.com/azure/sdk/x"), None);
}

#[test]
fn vendor_manifest_decides_packages() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = app(tmp.path(), "module example.com/app\n");
    put(
        &dir,
        "vendor/modules.txt",
        "# github.com/a/b v1.0.0\n## explicit; go 1.20\ngithub.com/a/b\ngithub.com/a/b/sub\n",
    );
    put(&dir, "vendor/github.com/a/b/sub/s.go", "package sub\n");
    put(&dir, "vendor/github.com/a/b/other/o.go", "package other\n");
    let project = GoProject::detect_with(&dir, None, None).unwrap();
    assert_eq!(
        project.import_dir("github.com/a/b/sub"),
        Some(dir.join("vendor/github.com/a/b/sub"))
    );
    assert_eq!(project.import_dir("github.com/a/b/other"), None);
    assert_eq!(
        project.package_path_of_dir(&dir.join("vendor/github.com/a/b/sub")),
        Some("github.com/a/b/sub".to_owned())
    );
}

fn workspace(root: &Path) {
    put(
        root,
        "go.work",
        "go 1.25\nuse (\n\t./svc\n\t./libs/shared\n)\nreplace example.com/ext => ./ext\n",
    );
    put(
        root,
        "svc/go.mod",
        "module example.com/svc\ngo 1.24\nrequire example.com/third v1.0.0\n",
    );
    put(root, "libs/shared/go.mod", "module example.com/shared\n");
    put(root, "libs/shared/x/x.go", "package x\n");
    put(root, "ext/e/e.go", "package e\n");
    put(
        root,
        "go.work.sum",
        "example.com/sumonly v0.3.0 h1:abc\nexample.com/sumonly v0.3.0/go.mod h1:def\n",
    );
}

#[test]
fn workspace_modules_replaces_and_sums() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace(root);
    let cache = root.join("mod");
    put(&cache, "example.com/third@v1.0.0/t/t.go", "package t\n");
    put(&cache, "example.com/sumonly@v0.3.0/s/s.go", "package s\n");
    let project = GoProject::detect_with(root, None, Some(cache.clone())).unwrap();
    assert_eq!(project.root(), root);
    assert_eq!(project.modules().len(), 2);
    assert_eq!(project.main_module().unwrap().path, "example.com/svc");
    assert_eq!(
        project.import_dir("example.com/shared/x"),
        Some(root.join("libs/shared/x"))
    );
    assert_eq!(
        project.import_dir("example.com/ext/e"),
        Some(root.join("ext/e"))
    );
    assert_eq!(
        project.import_dir("example.com/third/t"),
        Some(cache.join("example.com/third@v1.0.0/t"))
    );
    assert_eq!(
        project.import_dir("example.com/sumonly/s"),
        Some(cache.join("example.com/sumonly@v0.3.0/s"))
    );
    assert_eq!(
        project
            .module_for_file(&root.join("libs/shared/x/x.go"))
            .unwrap()
            .path,
        "example.com/shared"
    );
    assert_eq!(project.module_for_file(&root.join("other.go")), None);
}

#[test]
fn module_inside_workspace_sees_siblings() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace(root);
    let project = GoProject::detect_with(&root.join("libs/shared/x"), None, None).unwrap();
    assert_eq!(project.root(), root.join("libs/shared"));
    assert_eq!(project.main_module().unwrap().path, "example.com/shared");
    assert_eq!(project.modules().len(), 2);
    assert_eq!(
        project.import_dir("example.com/ext/e"),
        Some(root.join("ext/e"))
    );
}

#[test]
fn package_paths_of_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let g = goroot(tmp.path());
    let cache = tmp.path().join("mod");
    std::fs::create_dir_all(cache.join("github.com/!azure/sdk@v1.0.0/x")).unwrap();
    let dir = app(tmp.path(), "module example.com/app\n");
    put(&dir, "internal/util/u.go", "package util\n");
    let project = GoProject::detect_with(&dir, Some(g.clone()), Some(cache.clone())).unwrap();
    assert_eq!(
        project.package_path_of_dir(&dir),
        Some("example.com/app".to_owned())
    );
    assert_eq!(
        project.package_path_of_dir(&dir.join("internal/util")),
        Some("example.com/app/internal/util".to_owned())
    );
    assert_eq!(
        project.package_path_of_dir(&g.join("src/net/http")),
        Some("net/http".to_owned())
    );
    assert_eq!(
        project.package_path_of_dir(&g.join("src/vendor/golang.org/x/net/idna")),
        Some("golang.org/x/net/idna".to_owned())
    );
    assert_eq!(
        project.package_path_of_dir(&cache.join("github.com/!azure/sdk@v1.0.0/x")),
        Some("github.com/Azure/sdk/x".to_owned())
    );
    assert_eq!(project.package_path_of_dir(tmp.path()), None);
}

#[test]
fn lists_std_packages_once() {
    let tmp = tempfile::tempdir().unwrap();
    let g = goroot(tmp.path());
    let dir = app(tmp.path(), "module example.com/app\n");
    let project = GoProject::detect_with(&dir, Some(g), None).unwrap();
    let expected = ["encoding/json", "net/http", "os", "runtime", "strings"];
    assert_eq!(project.std_packages(), expected);
    assert_eq!(project.std_packages(), expected);
    let none = GoProject::detect_with(&dir, None, None).unwrap();
    assert!(none.std_packages().is_empty());
}

#[test]
fn lists_module_packages_with_skips() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = app(tmp.path(), "module example.com/app\n");
    for file in [
        "main.go",
        "cmd/tool/main.go",
        "internal/x/x.go",
        "vendor/v/v.go",
        "testdata/t/t.go",
        ".hidden/h.go",
        "_skip/s.go",
        "nested/go.mod",
        "nested/n.go",
        "pkg/only_test.go",
        "docs/readme.md",
    ] {
        put(&dir, file, "package x\n");
    }
    let project = GoProject::detect_with(&dir, None, None).unwrap();
    let module = project.main_module().unwrap();
    assert_eq!(
        project.module_packages(module),
        [
            "example.com/app",
            "example.com/app/cmd/tool",
            "example.com/app/internal/x"
        ]
    );
}

#[test]
fn dependency_packages_from_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("mod");
    for file in [
        "dep.go",
        "a/a.go",
        "b/b.go",
        "b/internal/i.go",
        "vendor/v/v.go",
        "c/go.mod",
    ] {
        put(&cache.join("example.com/dep@v1.0.0"), file, "package x\n");
    }
    let dir = app(
        tmp.path(),
        "module example.com/app\nrequire example.com/dep v1.0.0\n",
    );
    let project = GoProject::detect_with(&dir, None, Some(cache)).unwrap();
    assert_eq!(
        project.dependency_packages("example.com/dep"),
        ["example.com/dep", "example.com/dep/a", "example.com/dep/b"]
    );
    assert!(
        project
            .dependency_packages("example.com/unknown")
            .is_empty()
    );
}

#[test]
fn huge_go_mod_is_capped() {
    let tmp = tempfile::tempdir().unwrap();
    let mut text = String::from("module example.com/big\nrequire example.com/early v1.0.0\n");
    text.push_str(&"// padding\n".repeat(120_000));
    text.push_str("require example.com/late v1.0.0\n");
    let dir = app(tmp.path(), &text);
    let project = GoProject::detect_with(&dir, None, None).unwrap();
    assert_eq!(project.main_module().unwrap().path, "example.com/big");
    let required = project.required_modules();
    assert!(required.iter().any(|(p, _)| p == "example.com/early"));
    assert!(!required.iter().any(|(p, _)| p == "example.com/late"));
}

#[cfg(unix)]
#[test]
fn unreadable_go_mod_does_not_panic() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let dir = app(tmp.path(), "module example.com/app\n");
    let file = dir.join("go.mod");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
    let project = GoProject::detect_with(&dir, None, None).unwrap();
    assert!(project.import_dir("example.com/app").is_none() || project.main_module().is_some());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn missing_version_falls_back_to_the_nearest_cached_one() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    for v in ["v2.52.9", "v2.52.10"] {
        put(
            &cache,
            &format!("github.com/gofiber/fiber/v2@{v}/app.go"),
            "package fiber\n",
        );
    }
    let dir = app(
        tmp.path(),
        "module example.com/app\n\nrequire github.com/gofiber/fiber/v2 v2.52.12\n",
    );
    let project =
        GoProject::detect_with(&dir, Some(goroot(tmp.path())), Some(cache.clone())).unwrap();
    // Nothing above v2.52.12 exists, so the highest version below it wins.
    assert_eq!(
        project.import_dir("github.com/gofiber/fiber/v2"),
        Some(cache.join("github.com/gofiber/fiber/v2@v2.52.10"))
    );
}

#[test]
fn exact_version_wins_over_a_nearer_neighbour() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    for v in ["v1.0.0", "v1.1.0", "v1.2.0"] {
        put(
            &cache,
            &format!("example.com/lib@{v}/a.go"),
            "package lib\n",
        );
    }
    let dir = app(
        tmp.path(),
        "module example.com/app\n\nrequire example.com/lib v1.1.0\n",
    );
    let project =
        GoProject::detect_with(&dir, Some(goroot(tmp.path())), Some(cache.clone())).unwrap();
    assert_eq!(
        project.import_dir("example.com/lib"),
        Some(cache.join("example.com/lib@v1.1.0"))
    );
}

#[test]
fn only_higher_versions_cached_picks_the_lowest_of_them() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    for v in ["v1.5.0", "v1.3.0"] {
        put(
            &cache,
            &format!("example.com/lib@{v}/a.go"),
            "package lib\n",
        );
    }
    let dir = app(
        tmp.path(),
        "module example.com/app\n\nrequire example.com/lib v1.1.0\n",
    );
    let project =
        GoProject::detect_with(&dir, Some(goroot(tmp.path())), Some(cache.clone())).unwrap();
    assert_eq!(
        project.import_dir("example.com/lib"),
        Some(cache.join("example.com/lib@v1.3.0"))
    );
}
