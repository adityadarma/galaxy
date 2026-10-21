use std::path::{Path, PathBuf};

use super::*;
use crate::go::modules::GoProject;

const MARK: &str = "/*|*/";
const GO_MOD: &str = "module example.com/app\n\ngo 1.22\n";

fn put(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn fake_goroot(root: &Path) -> PathBuf {
    let g = root.join("goroot");
    put(
        &g,
        "src/net/http/http.go",
        "package http\n\ntype Client struct{ Timeout int }\n\nfunc (c *Client) Do() error { return nil }\n\nfunc Get(url string) error { return nil }\n",
    );
    g
}

struct Fx {
    _tmp: tempfile::TempDir,
    engine: GoEngine,
    app: PathBuf,
    /// File with the cursor, its stripped text, line and UTF-16 column.
    cursor: (PathBuf, String, u32, u32),
}

fn fx(files: &[(&str, &str)]) -> Fx {
    fx_with(GO_MOD, files, false)
}

fn fx_with(go_mod: &str, files: &[(&str, &str)], modcache: bool) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let goroot = fake_goroot(tmp.path());
    let cache = tmp.path().join("modcache");
    put(
        &cache,
        "example.com/ext@v1.0.0/ext.go",
        "package ext\n\nfunc Do() {}\n",
    );
    let app = tmp.path().join("app");
    put(&app, "go.mod", go_mod);
    let mut cursor = None;
    for (rel, text) in files {
        let mut body = (*text).to_owned();
        if let Some(at) = body.find(MARK) {
            body = body.replacen(MARK, "", 1);
            let line = body[..at].matches('\n').count() as u32;
            let start = body[..at].rfind('\n').map_or(0, |i| i + 1);
            let column = body[start..at].encode_utf16().count() as u32;
            cursor = Some((app.join(rel), body.clone(), line, column));
        }
        put(&app, rel, &body);
    }
    let project = GoProject::detect_with(&app, Some(goroot), modcache.then_some(cache));
    assert!(project.is_some());
    Fx {
        _tmp: tmp,
        engine: GoEngine::with_project(project),
        app,
        cursor: cursor.expect("marker"),
    }
}

impl Fx {
    fn tree(&self) -> tree_sitter::Tree {
        crate::go::parse::parse(&self.cursor.0, &self.cursor.1)
            .unwrap()
            .1
    }

    fn occurrences(&mut self) -> Vec<Occurrence> {
        let tree = self.tree();
        let (path, src, line, column) = self.cursor.clone();
        references(&mut self.engine, &path, &src, &tree, line, column)
    }

    fn query(&mut self) -> Option<GoRefQuery> {
        let tree = self.tree();
        let (path, src, line, column) = self.cursor.clone();
        reference_query(&mut self.engine, &path, &src, &tree, line, column)
    }

    /// `file: line text` of each occurrence.
    fn refs(&mut self) -> Vec<String> {
        let found = self.occurrences();
        self.describe(&found)
    }

    fn describe(&self, found: &[Occurrence]) -> Vec<String> {
        found
            .iter()
            .map(|o| {
                let text = if o.path == self.cursor.0 {
                    self.cursor.1.clone()
                } else if let Some(open) = self.engine.open_text(&o.path) {
                    open.to_string()
                } else {
                    std::fs::read_to_string(&o.path).unwrap()
                };
                let rel = o
                    .path
                    .strip_prefix(&self.app)
                    .unwrap()
                    .display()
                    .to_string();
                let line = text.lines().nth(o.line as usize).unwrap_or("");
                let units: Vec<u16> = line.encode_utf16().collect();
                let word = String::from_utf16_lossy(
                    &units[o.start_column as usize..o.end_column as usize],
                );
                format!("{rel}: {} [{word}]", line.trim())
            })
            .collect()
    }
}

fn main_go(body: &str) -> [(&'static str, String); 1] {
    [("main.go", format!("package main\n\n{body}\n"))]
}

fn one(body: &str) -> Vec<String> {
    let files = main_go(body);
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (*a, b.as_str())).collect();
    fx(&refs).refs()
}

fn lines(found: &[String]) -> Vec<&str> {
    found
        .iter()
        .map(|s| s.split_once(": ").unwrap().1)
        .collect()
}

// ---- locals -----------------------------------------------------------------

#[test]
fn local_var_declaration_and_uses() {
    let found = one("func main() {\n\t/*|*/count := 1\n\tcount++\n\tuse(count)\n}");
    assert_eq!(
        lines(&found),
        [
            "count := 1 [count]",
            "count++ [count]",
            "use(count) [count]"
        ]
    );
}

#[test]
fn local_from_a_use() {
    let found = one("func main() {\n\tcount := 1\n\tcount++\n\tuse(/*|*/count)\n}");
    assert_eq!(found.len(), 3);
}

#[test]
fn shadowed_local_outer() {
    let found = one(
        "func main() {\n\tx := 1\n\tif true {\n\t\tx := 2\n\t\tinner(x)\n\t}\n\touter(/*|*/x)\n}",
    );
    assert_eq!(lines(&found), ["x := 1 [x]", "outer(x) [x]"]);
}

#[test]
fn shadowed_local_inner() {
    let found = one(
        "func main() {\n\tx := 1\n\tif true {\n\t\t/*|*/x := 2\n\t\tinner(x)\n\t}\n\touter(x)\n}",
    );
    assert_eq!(lines(&found), ["x := 2 [x]", "inner(x) [x]"]);
}

#[test]
fn local_stays_in_its_function() {
    let found = one("func a() {\n\t/*|*/n := 1\n\tuse(n)\n}\n\nfunc b() {\n\tn := 2\n\tuse(n)\n}");
    assert_eq!(lines(&found), ["n := 1 [n]", "use(n) [n]"]);
}

#[test]
fn parameter() {
    let found = one("func add(/*|*/a int, b int) int {\n\treturn a + b\n}");
    assert_eq!(
        lines(&found),
        ["func add(a int, b int) int { [a]", "return a + b [a]"].map(|s| s)
    );
}

#[test]
fn receiver() {
    let found = one("type T struct{}\n\nfunc (/*|*/t *T) Name() string {\n\treturn t.String()\n}");
    assert_eq!(found.len(), 2, "{found:?}");
}

#[test]
fn named_result() {
    let found = one("func f() (/*|*/err error) {\n\terr = nil\n\treturn\n}");
    assert_eq!(found.len(), 2, "{found:?}");
}

#[test]
fn range_variables() {
    let found =
        one("func f(xs []int) {\n\tfor i, /*|*/v := range xs {\n\t\tuse(i, v)\n\t}\n\tuse(v)\n}");
    assert_eq!(
        lines(&found),
        ["for i, v := range xs { [v]", "use(i, v) [v]"]
    );
}

#[test]
fn type_switch_variable() {
    let found = one(
        "func f(v any) {\n\tswitch /*|*/t := v.(type) {\n\tcase int:\n\t\tuseInt(t)\n\tcase string:\n\t\tuseStr(t)\n\t}\n}",
    );
    assert_eq!(found.len(), 3, "{found:?}");
}

#[test]
fn label() {
    let found = one("func f() {\n/*|*/outer:\n\tfor {\n\t\tbreak outer\n\t}\n}");
    assert_eq!(found.len(), 2, "{found:?}");
}

#[test]
fn closure_capture() {
    let found = one(
        "func f() {\n\ttotal := 0\n\tadd := func(n int) {\n\t\ttotal += n\n\t}\n\tadd(1)\n\tuse(/*|*/total)\n}",
    );
    assert_eq!(found.len(), 3, "{found:?}");
}

#[test]
fn closure_param_is_separate() {
    let found = one("func f() {\n\tn := 0\n\tg := func(/*|*/n int) { use(n) }\n\tg(n)\n}");
    assert_eq!(
        lines(&found),
        [
            "g := func(n int) { use(n) } [n]",
            "g := func(n int) { use(n) } [n]"
        ]
    );
}

#[test]
fn unused_name_is_only_its_declaration() {
    let found = one("func f() {\n\t/*|*/unused := 1\n\t_ = 2\n}");
    assert_eq!(lines(&found), ["unused := 1 [unused]"]);
}

#[test]
fn blank_identifier_has_no_query() {
    let mut f = fx(&[("main.go", "package main\n\nfunc f() {\n\t/*|*/_ = 1\n}\n")]);
    assert!(f.query().is_none());
    assert!(f.refs().is_empty());
}

#[test]
fn local_is_not_external() {
    let mut f = fx(&[(
        "main.go",
        "package main\n\nfunc f() {\n\t/*|*/a := 1\n\tuse(a)\n}\n",
    )]);
    assert!(!f.query().unwrap().external);
}

// ---- package level ------------------------------------------------------------

#[test]
fn function_in_one_file() {
    let found = one("func /*|*/helper() {}\n\nfunc main() {\n\thelper()\n\thelper()\n}");
    assert_eq!(found.len(), 3);
}

#[test]
fn function_across_files_of_a_package_with_tests() {
    let mut f = fx(&[
        (
            "main.go",
            "package main\n\nfunc /*|*/helper() int { return 1 }\n",
        ),
        ("b.go", "package main\n\nfunc b() int { return helper() }\n"),
        ("b_test.go", "package main\n\nfunc TestB() { helper() }\n"),
    ]);
    let found = f.refs();
    assert_eq!(found.len(), 3, "{found:?}");
    assert!(found.iter().any(|s| s.starts_with("b_test.go")));
}

#[test]
fn type_declaration_and_uses() {
    let found = one(
        "type /*|*/Server struct{}\n\nfunc New() *Server {\n\treturn &Server{}\n}\n\nfunc (s *Server) Run() {}",
    );
    assert_eq!(found.len(), 4, "{found:?}");
}

#[test]
fn constant() {
    let found = one("const /*|*/Limit = 10\n\nfunc f() int {\n\treturn Limit * 2\n}");
    assert_eq!(found.len(), 2, "{found:?}");
}

#[test]
fn package_variable() {
    let found = one(
        "var /*|*/counter int\n\nfunc inc() {\n\tcounter++\n}\n\nfunc get() int {\n\treturn counter\n}",
    );
    assert_eq!(found.len(), 3, "{found:?}");
}

#[test]
fn package_variable_from_a_use() {
    let found = one("var counter int\n\nfunc inc() {\n\t/*|*/counter++\n}");
    assert_eq!(found.len(), 2, "{found:?}");
}

#[test]
fn local_shadows_package_name() {
    let found = one(
        "var value int\n\nfunc f() {\n\tvalue := 2\n\tuse(value)\n}\n\nfunc g() int {\n\treturn /*|*/value\n}",
    );
    assert_eq!(
        lines(&found),
        ["var value int [value]", "return value [value]"]
    );
}

#[test]
fn package_name_not_taken_by_shadowing_local() {
    let found = one(
        "func /*|*/run() {}\n\nfunc f() {\n\trun := 1\n\tuse(run)\n}\n\nfunc g() {\n\trun()\n}",
    );
    assert_eq!(lines(&found), ["func run() {} [run]", "run() [run]"]);
}

fn util_files() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "util/util.go",
            "package util\n\nfunc /*|*/Helper() int { return 1 }\n\nfunc helper() int { return 2 }\n\nfunc inner() int { return Helper() }\n",
        ),
        (
            "main.go",
            "package main\n\nimport u \"example.com/app/util\"\n\nfunc main() { u.Helper() }\n",
        ),
        (
            "cmd/run.go",
            "package main\n\nimport \"example.com/app/util\"\n\nfunc run() { util.Helper(); util.Helper() }\n",
        ),
    ]
}

#[test]
fn function_used_from_other_packages_through_aliases() {
    let mut f = fx(&util_files());
    let found = f.refs();
    assert_eq!(found.len(), 5, "{found:?}");
    assert!(found.iter().any(|s| s.starts_with("main.go")));
    assert_eq!(
        found.iter().filter(|s| s.starts_with("cmd/run.go")).count(),
        2
    );
}

#[test]
fn qualified_use_leads_to_the_same_symbol() {
    let mut files = util_files();
    files[0].1 = "package util\n\nfunc Helper() int { return 1 }\n";
    files[1].1 =
        "package main\n\nimport u \"example.com/app/util\"\n\nfunc main() { u./*|*/Helper() }\n";
    let found = fx(&files).refs();
    assert_eq!(found.len(), 4, "{found:?}");
}

#[test]
fn same_name_in_another_package_is_excluded() {
    let mut files = util_files();
    files.push((
        "other/other.go",
        "package other\n\nfunc Helper() int { return 3 }\n\nfunc x() int { return Helper() }\n",
    ));
    files.push((
        "cmd/other.go",
        "package main\n\nimport \"example.com/app/other\"\n\nfunc o() { other.Helper() }\n",
    ));
    let found = fx(&files).refs();
    assert_eq!(found.len(), 5, "{found:?}");
    assert!(
        found
            .iter()
            .all(|s| !s.starts_with("other/") && !s.starts_with("cmd/other.go"))
    );
}

#[test]
fn unexported_same_name_in_other_package_is_excluded() {
    let files = [
        (
            "a/a.go",
            "package a\n\nfunc /*|*/helper() {}\n\nfunc x() { helper() }\n",
        ),
        (
            "b/b.go",
            "package b\n\nfunc helper() {}\n\nfunc y() { helper() }\n",
        ),
    ];
    let found = fx(&files).refs();
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.iter().all(|s| s.starts_with("a/")));
}

#[test]
fn type_used_in_a_qualified_type() {
    let files = [
        ("m/m.go", "package m\n\ntype /*|*/Item struct{}\n"),
        (
            "main.go",
            "package main\n\nimport \"example.com/app/m\"\n\nvar items []m.Item\n\nfunc f(i m.Item) m.Item { return i }\n",
        ),
    ];
    let found = fx(&files).refs();
    assert_eq!(found.len(), 4, "{found:?}");
}

// ---- fields and methods -------------------------------------------------------

const TWO_TYPES: &str = "package main\n\ntype A struct{ Name string }\n\ntype B struct{ Name string }\n\nfunc (a A) /*|*/Run() {}\n\nfunc (b B) Run() {}\n\nfunc f(a A, b B) {\n\ta.Run()\n\tb.Run()\n\tuse(a.Name, b.Name)\n}\n";

#[test]
fn method_is_told_apart_by_receiver_type() {
    let found = fx(&[("main.go", TWO_TYPES)]).refs();
    assert_eq!(
        lines(&found),
        ["func (a A) Run() {} [Run]", "a.Run() [Run]"]
    );
}

#[test]
fn method_from_a_call_site() {
    let src = TWO_TYPES
        .replace("/*|*/", "")
        .replace("\tb.Run()", "\tb./*|*/Run()");
    let found = fx(&[("main.go", &src)]).refs();
    assert_eq!(
        lines(&found),
        ["func (b B) Run() {} [Run]", "b.Run() [Run]"]
    );
}

#[test]
fn field_is_told_apart_by_struct() {
    let src = TWO_TYPES
        .replace("/*|*/", "")
        .replace("A struct{ Name", "A struct{ /*|*/Name");
    let found = fx(&[("main.go", &src)]).refs();
    assert_eq!(
        lines(&found),
        [
            "type A struct{ Name string } [Name]",
            "use(a.Name, b.Name) [Name]"
        ]
    );
}

#[test]
fn keyed_literal_key_is_a_field_use() {
    let found = one(
        "type Config struct {\n\t/*|*/Port int\n\tHost string\n}\n\nfunc f() Config {\n\treturn Config{Port: 1, Host: \"x\"}\n}\n\nfunc g(c Config) int {\n\treturn c.Port\n}",
    );
    assert_eq!(found.len(), 3, "{found:?}");
    assert!(found.iter().any(|s| s.contains("Config{Port: 1")));
}

#[test]
fn keyed_literal_of_another_struct_is_excluded() {
    let found = one(
        "type A struct{ /*|*/ID int }\n\ntype B struct{ ID int }\n\nvar x = A{ID: 1}\n\nvar y = B{ID: 2}",
    );
    assert_eq!(found.len(), 2, "{found:?}");
}

#[test]
fn promoted_method_through_embedding() {
    let found = one(
        "type Base struct{}\n\nfunc (b *Base) /*|*/Hello() {}\n\ntype Derived struct {\n\tBase\n}\n\nfunc f(d Derived, b *Base) {\n\td.Hello()\n\tb.Hello()\n}",
    );
    assert_eq!(found.len(), 3, "{found:?}");
}

#[test]
fn promoted_field_through_embedding() {
    let found = one(
        "type Base struct{ /*|*/ID int }\n\ntype Derived struct {\n\tBase\n}\n\nfunc f(d Derived) int {\n\treturn d.ID\n}",
    );
    assert_eq!(found.len(), 2, "{found:?}");
}

#[test]
fn embedded_type_name_is_a_type_use() {
    let found = one("type /*|*/Base struct{}\n\ntype Derived struct {\n\tBase\n}");
    assert_eq!(found.len(), 2, "{found:?}");
}

#[test]
fn method_used_from_another_package() {
    let files = [
        (
            "m/m.go",
            "package m\n\ntype T struct{ Size int }\n\nfunc (t T) /*|*/Open() {}\n",
        ),
        (
            "main.go",
            "package main\n\nimport \"example.com/app/m\"\n\nfunc f() {\n\tvar t m.T\n\tt.Open()\n\t_ = t.Size\n}\n",
        ),
    ];
    let found = fx(&files).refs();
    assert_eq!(found.len(), 2, "{found:?}");
}

#[test]
fn field_used_from_another_package() {
    let files = [
        (
            "m/m.go",
            "package m\n\ntype T struct{ /*|*/Size int }\n\nfunc New() T { return T{Size: 1} }\n",
        ),
        (
            "main.go",
            "package main\n\nimport \"example.com/app/m\"\n\nfunc f(t m.T) int {\n\treturn t.Size\n}\n",
        ),
    ];
    let found = fx(&files).refs();
    assert_eq!(found.len(), 3, "{found:?}");
}

#[test]
fn interface_method_is_linked_to_implementations() {
    let found = one(
        "type Runner interface {\n\t/*|*/Run()\n}\n\ntype Impl struct{}\n\nfunc (Impl) Run() {}\n\nfunc f(r Runner) {\n\tr.Run()\n}",
    );
    assert_eq!(
        lines(&found),
        ["Run() [Run]", "func (Impl) Run() {} [Run]", "r.Run() [Run]"]
    );
}

#[test]
fn concrete_method_is_linked_to_interface() {
    let found = one(
        "type Runner interface {\n\tRun()\n}\n\ntype Impl struct{}\n\nfunc (Impl) /*|*/Run() {}\n\nfunc f(r Runner) {\n\tr.Run()\n}",
    );
    assert_eq!(
        lines(&found),
        ["Run() [Run]", "func (Impl) Run() {} [Run]", "r.Run() [Run]"]
    );
}

#[test]
fn interface_method_does_not_link_incompatible_signature() {
    let found = one(
        "type Runner interface {\n\t/*|*/Run(string)\n}\n\ntype Compatible struct{}\n\nfunc (Compatible) Run(string) {}\n\ntype Other struct{}\n\nfunc (Other) Run(int) {}\n",
    );
    assert_eq!(
        lines(&found),
        [
            "Run(string) [Run]",
            "func (Compatible) Run(string) {} [Run]"
        ]
    );
}

// ---- type parameters and packages -----------------------------------------------

#[test]
fn type_parameter_of_a_function() {
    let found = one(
        "func Map[/*|*/T any](xs []T) []T {\n\tvar out []T\n\treturn out\n}\n\nfunc Other[T any](x T) T { return x }",
    );
    assert_eq!(found.len(), 4, "{found:?}");
    assert!(found.iter().all(|s| !s.contains("Other")));
}

#[test]
fn type_parameter_of_a_type() {
    let found = one(
        "type Box[/*|*/T any] struct {\n\tvalue T\n}\n\nfunc (b Box[T]) Get() T { return b.value }",
    );
    assert!(found.len() >= 2, "{found:?}");
    assert!(found[0].contains("type Box"));
}

#[test]
fn import_name_and_qualifiers() {
    let files = [(
        "main.go",
        "package main\n\nimport \"net/http\"\n\nfunc f() {\n\t/*|*/http.Get(\"x\")\n\t_ = http.Client{}\n}\n",
    )];
    let found = fx(&files).refs();
    assert_eq!(found.len(), 2, "{found:?}");
}

#[test]
fn aliased_import_is_renamable() {
    let files = [(
        "main.go",
        "package main\n\nimport h \"net/http\"\n\nfunc f() {\n\t/*|*/h.Get(\"x\")\n\t_ = h.Client{}\n}\n",
    )];
    let mut f = fx(&files);
    let query = f.query().unwrap();
    assert!(can_rename(&query, "web").is_ok());
    let found = f.refs();
    assert_eq!(found.len(), 3, "{found:?}");
}

#[test]
fn unaliased_import_cannot_be_renamed() {
    let files = [(
        "main.go",
        "package main\n\nimport \"net/http\"\n\nfunc f() {\n\t/*|*/http.Get(\"x\")\n}\n",
    )];
    let query = fx(&files).query().unwrap();
    assert!(can_rename(&query, "web").is_err());
}

#[test]
fn builtin_has_no_query() {
    let mut f = fx(&[(
        "main.go",
        "package main\n\nfunc f(xs []int) int {\n\treturn /*|*/len(xs)\n}\n",
    )]);
    assert!(f.query().is_none());
    assert!(f.refs().is_empty());
}

#[test]
fn predeclared_type_has_no_query() {
    let mut f = fx(&[("main.go", "package main\n\nvar x /*|*/int\n")]);
    assert!(f.query().is_none());
}

// ---- external symbols ------------------------------------------------------------

#[test]
fn goroot_symbol_is_external() {
    let files = [(
        "main.go",
        "package main\n\nimport \"net/http\"\n\nfunc f() {\n\thttp./*|*/Get(\"x\")\n\thttp.Get(\"y\")\n}\n",
    )];
    let mut f = fx(&files);
    let query = f.query().unwrap();
    assert!(query.external);
    assert_eq!(f.refs().len(), 2);
}

#[test]
fn goroot_method_is_external() {
    let files = [(
        "main.go",
        "package main\n\nimport \"net/http\"\n\nfunc f(c *http.Client) {\n\tc./*|*/Do()\n}\n",
    )];
    assert!(fx(&files).query().unwrap().external);
}

#[test]
fn module_cache_symbol_is_external() {
    let files = [(
        "main.go",
        "package main\n\nimport \"example.com/ext\"\n\nfunc f() {\n\text./*|*/Do()\n}\n",
    )];
    let go_mod = format!("{GO_MOD}\nrequire example.com/ext v1.0.0\n");
    let mut f = fx_with(&go_mod, &files, true);
    assert!(f.query().unwrap().external);
}

#[test]
fn project_symbol_is_not_external() {
    let mut f = fx(&util_files());
    assert!(!f.query().unwrap().external);
}

// ---- buffers, robustness ------------------------------------------------------------

#[test]
fn open_buffer_overrides_disk_for_another_file() {
    let mut f = fx(&[
        ("main.go", "package main\n\nfunc /*|*/helper() {}\n"),
        ("b.go", "package main\n\nfunc b() {}\n"),
    ]);
    assert_eq!(f.refs().len(), 1);
    let b = f.app.join("b.go");
    f.engine
        .open_document(&b, "package main\n\nfunc b() { helper() }\n");
    let found = f.refs();
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.iter().any(|s| s.starts_with("b.go")));
}

#[test]
fn open_buffer_hides_a_use_that_is_gone() {
    let mut f = fx(&[
        ("main.go", "package main\n\nfunc /*|*/helper() {}\n"),
        ("b.go", "package main\n\nfunc b() { helper() }\n"),
    ]);
    assert_eq!(f.refs().len(), 2);
    let b = f.app.join("b.go");
    f.engine.open_document(&b, "package main\n\nfunc b() {}\n");
    assert_eq!(f.refs().len(), 1);
}

#[test]
fn buffer_text_of_the_current_file_wins_over_disk() {
    let mut f = fx(&[("main.go", "package main\n\nfunc /*|*/helper() {}\n")]);
    f.cursor.1 = "package main\n\nfunc helper() {}\n\nfunc g() { helper() }\n".to_owned();
    assert_eq!(f.refs().len(), 2);
    // The engine is left as it was.
    assert!(f.engine.open_text(&f.cursor.0).is_none());
}

#[test]
fn current_buffer_does_not_stay_registered_over_an_open_one() {
    let mut f = fx(&[("main.go", "package main\n\nfunc /*|*/helper() {}\n")]);
    let path = f.cursor.0.clone();
    f.engine
        .open_document(&path, "package main\n\nfunc helper() {}\n");
    f.cursor.1 = "package main\n\nfunc helper() {}\n\nfunc g() { helper() }\n".to_owned();
    f.refs();
    assert_eq!(
        f.engine.open_text(&path).as_deref(),
        Some("package main\n\nfunc helper() {}\n")
    );
}

#[test]
fn incomplete_code_does_not_panic() {
    for body in [
        "func f() {\n\tx := /*|*/y.\n",
        "func f( {\n\t/*|*/a := \n",
        "type T struct {\n\t/*|*/A int\n\tB\n",
        "func /*|*/f() {\n\tfor i := range {\n\t\tuse(i\n",
        "import (\n\t\"fmt\n)\nfunc /*|*/g() { g(",
    ] {
        let src = format!("package main\n\n{body}");
        let mut f = fx(&[("main.go", &src)]);
        let _ = f.query();
        let _ = f.refs();
    }
}

#[test]
fn position_on_whitespace_gives_nothing() {
    let mut f = fx(&[("main.go", "package main\n\n/*|*/\n\nfunc f() {}\n")]);
    assert!(f.query().is_none());
}

#[test]
fn columns_are_utf16() {
    let src = "package main\n\nfunc f() {\n\ts := \"😀é\"; /*|*/n := 1; use(s, n)\n}\n";
    let mut f = fx(&[("main.go", src)]);
    let found = f.occurrences();
    let line = f.cursor.1.lines().nth(3).unwrap().to_owned();
    let first = line.find("n := 1").unwrap();
    let col = line[..first].encode_utf16().count() as u32;
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].start_column, col);
    assert_eq!(found[0].end_column, col + 1);
    assert!(found.iter().all(|o| !o.dollar));
}

#[test]
fn vendor_and_testdata_are_skipped() {
    let mut f = fx(&[
        ("main.go", "package main\n\nfunc /*|*/Helper() {}\n"),
        (
            "vendor/x/x.go",
            "package x\n\nimport \"example.com/app\"\n\nfunc g() { app.Helper() }\n",
        ),
        ("testdata/t.go", "package main\n\nfunc t() { Helper() }\n"),
        (".hidden/h.go", "package main\n\nfunc h() { Helper() }\n"),
    ]);
    assert_eq!(f.refs().len(), 1);
}

#[test]
fn results_are_sorted_and_unique() {
    let mut f = fx(&util_files());
    let found = f.occurrences();
    let mut sorted = found.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(found, sorted);
}

#[test]
fn result_count_is_capped() {
    // Spread over many functions: that is what real code looks like, and a
    // single body with thousands of calls is a different (slower) case.
    let mut body = String::from("package main\n\nfunc /*|*/helper() {}\n");
    for i in 0..50 {
        body.push_str(&format!("\nfunc g{i}() {{\n"));
        for _ in 0..50 {
            body.push_str("\thelper()\n");
        }
        body.push_str("}\n");
    }
    let mut f = fx(&[("main.go", &body)]);
    assert_eq!(f.occurrences().len(), MAX_OCCURRENCES);
}

#[test]
fn thousands_of_uses_in_one_function_stay_fast() {
    let mut body = String::from("package main\n\nfunc /*|*/helper() {}\n\nfunc g() {\n");
    for _ in 0..2500 {
        body.push_str("\thelper()\n");
    }
    body.push_str("}\n");
    let mut f = fx(&[("main.go", &body)]);
    let started = std::time::Instant::now();
    assert_eq!(f.occurrences().len(), MAX_OCCURRENCES);
    // Was 18 s before scopes were indexed; the bound is loose for slow CI.
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

// ---- scan_references --------------------------------------------------------------------

#[test]
fn scan_references_reads_one_file() {
    let mut f = fx(&[
        ("main.go", "package main\n\nfunc /*|*/helper() {}\n"),
        ("b.go", "package main\n\nfunc b() { helper(); helper() }\n"),
    ]);
    let query = f.query().unwrap();
    let b = f.app.join("b.go");
    let text = std::fs::read_to_string(&b).unwrap();
    assert_eq!(scan_references(&mut f.engine, &query, &b, &text).len(), 2);
    let own = f.cursor.0.clone();
    let src = f.cursor.1.clone();
    assert_eq!(scan_references(&mut f.engine, &query, &own, &src).len(), 1);
}

#[test]
fn scan_references_of_a_local_ignores_other_files() {
    let mut f = fx(&[
        (
            "main.go",
            "package main\n\nfunc f() {\n\t/*|*/a := 1\n\tuse(a)\n}\n",
        ),
        (
            "b.go",
            "package main\n\nfunc g() {\n\ta := 1\n\tuse(a)\n}\n",
        ),
    ]);
    let query = f.query().unwrap();
    let b = f.app.join("b.go");
    let text = std::fs::read_to_string(&b).unwrap();
    assert!(scan_references(&mut f.engine, &query, &b, &text).is_empty());
}

// ---- names and renames --------------------------------------------------------------------

#[test]
fn names_are_checked() {
    assert!(valid_name("total"));
    assert!(valid_name("_x1"));
    assert!(valid_name("Größe"));
    assert!(valid_name("名前"));
    assert!(!valid_name(""));
    assert!(!valid_name("_"));
    assert!(!valid_name("1x"));
    assert!(!valid_name("a-b"));
    assert!(!valid_name("a b"));
    assert!(!valid_name("$a"));
    assert!(!valid_name("func"));
    assert!(!valid_name("range"));
}

fn local_query() -> GoRefQuery {
    fx(&[(
        "main.go",
        "package main\n\nfunc f() {\n\t/*|*/a := 1\n\tuse(a)\n}\n",
    )])
    .query()
    .unwrap()
}

#[test]
fn can_rename_accepts_a_plain_name() {
    assert_eq!(can_rename(&local_query(), "count"), Ok(()));
}

#[test]
fn can_rename_refuses_invalid_names() {
    let q = local_query();
    assert_eq!(can_rename(&q, "9a"), Err("invalid name".to_owned()));
    assert_eq!(can_rename(&q, "type"), Err("invalid name".to_owned()));
    assert_eq!(can_rename(&q, "_"), Err("invalid name".to_owned()));
    assert_eq!(can_rename(&q, ""), Err("invalid name".to_owned()));
}

#[test]
fn can_rename_refuses_external_symbols() {
    let files = [(
        "main.go",
        "package main\n\nimport \"net/http\"\n\nfunc f() {\n\thttp./*|*/Get(\"x\")\n}\n",
    )];
    let q = fx(&files).query().unwrap();
    assert_eq!(
        can_rename(&q, "Fetch"),
        Err("cannot rename external symbol".to_owned())
    );
}

#[test]
fn can_rename_refuses_init_and_main() {
    let q = fx(&[(
        "main.go",
        "package main\n\nfunc /*|*/init() {}\n\nfunc main() {}\n",
    )])
    .query()
    .unwrap();
    assert_eq!(
        can_rename(&q, "setup"),
        Err("cannot rename init/main".to_owned())
    );
    let q = fx(&[("main.go", "package main\n\nfunc /*|*/main() {}\n")])
        .query()
        .unwrap();
    assert_eq!(
        can_rename(&q, "start"),
        Err("cannot rename init/main".to_owned())
    );
    assert_eq!(
        can_rename(&local_query(), "main"),
        Err("cannot rename init/main".to_owned())
    );
}

#[test]
fn rename_edits_are_sorted_last_column_first() {
    let occ = |p: &str, line, start, end| Occurrence {
        path: PathBuf::from(p),
        line,
        start_column: start,
        end_column: end,
        dollar: false,
    };
    let edits = rename_edits(
        &[
            occ("b.go", 1, 2, 3),
            occ("a.go", 4, 1, 2),
            occ("a.go", 4, 9, 10),
            occ("a.go", 0, 5, 6),
        ],
        "n",
    );
    let order: Vec<_> = edits
        .iter()
        .map(|e| (e.0.to_str().unwrap(), e.1, e.2))
        .collect();
    assert_eq!(
        order,
        [
            ("a.go", 0, 5),
            ("a.go", 4, 9),
            ("a.go", 4, 1),
            ("b.go", 1, 2)
        ]
    );
    assert!(edits.iter().all(|e| e.4 == "n"));
}

type Edit = (PathBuf, u32, u32, u32, String);

/// Applies edits to the files in `app` (and the cursor buffer).
fn apply(f: &mut Fx, edits: &[Edit]) {
    use std::collections::BTreeMap;
    let mut by_file: BTreeMap<&PathBuf, Vec<&Edit>> = BTreeMap::new();
    for e in edits {
        by_file.entry(&e.0).or_default().push(e);
    }
    for (path, list) in by_file {
        let text = if *path == f.cursor.0 {
            f.cursor.1.clone()
        } else {
            std::fs::read_to_string(path).unwrap()
        };
        let mut rows: Vec<String> = text.split('\n').map(str::to_owned).collect();
        for (_, line, start, end, new) in list {
            let units: Vec<u16> = rows[*line as usize].encode_utf16().collect();
            let mut out = String::from_utf16(&units[..*start as usize]).unwrap();
            out.push_str(new);
            out.push_str(&String::from_utf16(&units[*end as usize..]).unwrap());
            rows[*line as usize] = out;
        }
        let joined = rows.join("\n");
        if *path == f.cursor.0 {
            f.cursor.1 = joined.clone();
        }
        std::fs::write(path, joined).unwrap();
    }
}

fn rename_roundtrip(files: &[(&str, &str)], new: &str) {
    let mut f = fx(files);
    let found = f.occurrences();
    assert!(!found.is_empty());
    let query = f.query().unwrap();
    assert_eq!(can_rename(&query, new), Ok(()));
    let edits = rename_edits(&found, new);
    apply(&mut f, &edits);
    // Put the cursor on the first renamed name.
    let first = &found[0];
    f.cursor.0 = first.path.clone();
    f.cursor.1 = std::fs::read_to_string(&first.path).unwrap();
    f.cursor.2 = first.line;
    f.cursor.3 = first.start_column;
    f.engine.reset_index();
    let again = f.occurrences();
    assert_eq!(again.len(), found.len(), "{again:?} vs {found:?}");
    assert!(
        f.describe(&again)
            .iter()
            .all(|s| s.ends_with(&format!("[{new}]")))
    );
}

#[test]
fn rename_a_local_and_find_it_again() {
    rename_roundtrip(
        &[(
            "main.go",
            "package main\n\nfunc f() {\n\t/*|*/a := 1\n\ta++\n\tuse(a, a)\n}\n",
        )],
        "total",
    );
}

#[test]
fn rename_a_function_across_packages_and_find_it_again() {
    rename_roundtrip(&util_files(), "Assist");
}

#[test]
fn rename_a_method_and_find_it_again() {
    rename_roundtrip(&[("main.go", TWO_TYPES)], "Execute");
}

#[test]
fn rename_a_field_with_keyed_literal_and_find_it_again() {
    rename_roundtrip(
        &[(
            "main.go",
            "package main\n\ntype C struct{ /*|*/Port int }\n\nvar c = C{Port: 1}\n\nfunc f() int { return c.Port }\n",
        )],
        "Number",
    );
}
