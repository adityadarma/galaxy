use super::fixture::Fx;
use super::*;
use crate::go::GoProject;

fn run(fx: &mut Fx, src: &str) -> Vec<Problem> {
    let path = fx.main();
    diagnostics(&mut fx.engine, &path, src)
}

fn diags(src: &str) -> Vec<Problem> {
    let mut fx = Fx::new(&[]);
    run(&mut fx, src)
}

fn messages(src: &str) -> Vec<String> {
    diags(src).into_iter().map(|p| p.message).collect()
}

// ---- syntax -----------------------------------------------------------

#[test]
fn valid_file_has_no_problems() {
    let src = "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(1)\n}\n";
    assert!(diags(src).is_empty());
}

#[test]
fn syntax_error_is_reported() {
    let src = "package main\n\nfunc main() {\n\tx := \n}\n";
    let problems = diags(src);
    assert!(!problems.is_empty());
    assert!(problems.iter().all(|p| p.severity == Severity::Error));
    assert!(
        problems
            .iter()
            .any(|p| p.start_line == 3 || p.start_line == 4)
    );
    assert!(
        problems
            .iter()
            .all(|p| p.message == "syntax error" || p.message.starts_with("missing "))
    );
}

#[test]
fn missing_token_message() {
    let problems = diags("package main\n\nfunc main() {\n\tf(1\n}\n");
    assert!(
        problems
            .iter()
            .any(|p| p.message.starts_with("missing ") || p.message == "syntax error"),
        "{problems:?}"
    );
}

#[test]
fn at_most_one_syntax_error_per_line() {
    let problems = diags("package main\n\nvar a = ) ) ) ) )\n");
    let mut lines: Vec<u32> = problems.iter().map(|p| p.start_line).collect();
    let before = lines.len();
    lines.dedup();
    assert_eq!(before, lines.len());
}

#[test]
fn many_syntax_errors_give_one_summary() {
    let mut src = String::from("package main\n\n");
    for _ in 0..120 {
        src.push_str("var a = )\n");
    }
    let problems = diags(&src);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].message.starts_with("syntax error"));
}

#[test]
fn syntax_error_range_is_never_empty() {
    for p in diags("package main\n\nfunc main() {\n\tx := \n") {
        assert!(
            p.end_line > p.start_line || p.end_column > p.start_column,
            "{p:?}"
        );
    }
}

// ---- unused imports ---------------------------------------------------

#[test]
fn unused_import_is_reported() {
    let src = "package main\n\nimport (\n\t\"fmt\"\n\t\"os\"\n)\n\nfunc main() { fmt.Println() }\n";
    let problems = diags(src);
    assert_eq!(problems.len(), 1);
    let p = &problems[0];
    assert_eq!(p.message, "\"os\" imported and not used");
    assert_eq!(
        (p.start_line, p.start_column, p.end_line, p.end_column),
        (4, 1, 4, 5)
    );
    assert_eq!(p.severity, Severity::Error);
    assert!(p.unnecessary);
}

#[test]
fn single_line_import_range_covers_spec() {
    let p = &diags("package main\n\nimport os \"os\"\n")[0];
    assert_eq!(p.message, "\"os\" imported as os and not used");
    assert_eq!((p.start_column, p.end_column), (7, 14));
}

#[test]
fn used_import_is_not_reported() {
    let src = "package main\n\nimport \"os\"\n\nvar a = os.Args\n";
    assert!(diags(src).is_empty());
}

#[test]
fn alias_is_what_counts() {
    let used = "package main\n\nimport o \"os\"\n\nvar a = o.Args\n";
    assert!(diags(used).is_empty());
    // The alias hides the package name, so `os.` does not use it.
    let unused = "package main\n\nimport o \"os\"\n\nvar a = os.Args\n";
    assert_eq!(messages(unused), ["\"os\" imported as o and not used"]);
}

#[test]
fn blank_and_dot_imports_are_skipped() {
    let src = "package main\n\nimport (\n\t_ \"os\"\n\t. \"fmt\"\n)\n";
    assert!(diags(src).is_empty());
}

#[test]
fn unresolvable_import_is_not_reported_as_unused() {
    let src = "package main\n\nimport \"github.com/nobody/missing\"\n";
    assert!(diags(src).is_empty());
}

#[test]
fn type_position_counts_as_use() {
    let src = "package main\n\nimport \"net/http\"\n\nfunc h(w http.ResponseWriter) {}\n\ntype T struct{ H http.Handler }\n";
    assert!(diags(src).is_empty());
}

#[test]
fn method_value_and_generic_use_count() {
    let src = "package main\n\nimport (\n\t\"os\"\n\t\"sort\"\n)\n\nvar f = os.Exit\n\nfunc g[T any](xs []T) { _ = sort.Ints }\n";
    assert!(diags(src).is_empty());
}

#[test]
fn shadowed_package_name_still_counts() {
    let src = "package main\n\nimport \"os\"\n\nfunc f() {\n\tos := struct{ Args int }{}\n\t_ = os.Args\n}\n";
    assert!(diags(src).is_empty());
}

#[test]
fn qualifier_inside_closure_or_composite_counts() {
    let src = "package main\n\nimport (\n\t\"errors\"\n\t\"strings\"\n)\n\nvar f = func() error {\n\t_ = []string{strings.ToUpper(\"a\")}\n\treturn errors.New(\"x\")\n}\n";
    assert!(diags(src).is_empty());
}

#[test]
fn qualifier_across_whitespace_counts() {
    let src = "package main\n\nimport \"os\"\n\nvar a = os .Args\n";
    assert!(diags(src).is_empty());
}

#[test]
fn package_name_differs_from_path() {
    // math/rand's package is rand, used as rand.Int.
    let ok = "package main\n\nimport \"math/rand\"\n\nvar a = rand.Int()\n";
    assert!(diags(ok).is_empty());
    let bad = "package main\n\nimport \"math/rand\"\n\nvar a = math.Int()\n";
    assert_eq!(messages(bad), ["\"math/rand\" imported and not used"]);
}

#[test]
fn versioned_import_uses_previous_element() {
    let src = "package main\n\nimport \"example.com/dep/util\"\n\nvar a = util.X\n";
    assert!(diags(src).is_empty());
}

#[test]
fn unused_imports_are_silent_when_file_has_syntax_errors() {
    let src = "package main\n\nimport \"os\"\n\nfunc main() {\n\tx := \n}\n";
    assert!(!messages(src).iter().any(|m| m.contains("imported")));
}

// ---- std imports ------------------------------------------------------

#[test]
fn missing_std_package_is_reported() {
    let src = "package main\n\nimport (\n\t\"fmt\"\n\t\"foo/bar\"\n)\n\nvar _ = fmt.Sprint\nvar _ = bar.X\n";
    let problems = diags(src);
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0].message, "package foo/bar is not in std");
    assert_eq!((problems[0].start_line, problems[0].start_column), (4, 1));
    assert_eq!(problems[0].severity, Severity::Error);
}

#[test]
fn dotted_and_module_paths_are_never_missing_std() {
    let src = "package main\n\nimport (\n\t_ \"github.com/nobody/missing\"\n\t_ \"example.com/app/lib\"\n\t_ \"example.com/app/nope\"\n\t_ \"example.com/dep/util\"\n)\n";
    assert!(diags(src).is_empty(), "{:?}", diags(src));
}

#[test]
fn existing_std_package_is_fine_and_unsafe_c_skipped() {
    let src = "package main\n\nimport (\n\t_ \"net/http\"\n\t_ \"unsafe\"\n\t\"C\"\n)\n";
    assert!(diags(src).is_empty());
}

#[test]
fn no_goroot_means_no_std_diagnostics() {
    let tmp = tempfile::tempdir().unwrap();
    fixture::put(tmp.path(), "go.mod", "module example.com/app\n");
    let project = GoProject::detect_with(tmp.path(), None, None);
    let mut engine = GoEngine::with_project(project);
    let path = tmp.path().join("main.go");
    let src = "package main\n\nimport _ \"foo/bar\"\n";
    assert!(diagnostics(&mut engine, &path, src).is_empty());
}

// ---- declared and not used --------------------------------------------

#[test]
fn unused_short_var_is_reported() {
    let src = "package main\n\nfunc main() {\n\tx := 1\n}\n";
    let problems = diags(src);
    assert_eq!(problems.len(), 1);
    let p = &problems[0];
    assert_eq!(p.message, "declared and not used: x");
    assert_eq!(
        (p.start_line, p.start_column, p.end_line, p.end_column),
        (3, 1, 3, 2)
    );
    assert!(p.unnecessary);
    assert_eq!(p.severity, Severity::Error);
}

#[test]
fn unused_var_declaration_is_reported() {
    let src = "package main\n\nfunc main() {\n\tvar a, b int\n\t_ = a\n}\n";
    assert_eq!(messages(src), ["declared and not used: b"]);
}

#[test]
fn any_other_mention_counts_as_use() {
    let src = "package main\n\nfunc main() {\n\tx := 1\n\tx = 2\n}\n";
    assert!(diags(src).is_empty());
}

#[test]
fn blank_identifier_is_skipped() {
    let src = "package main\n\nfunc main() {\n\t_ := 1\n\t_, err := f()\n\t_ = err\n}\n\nfunc f() (int, error) { return 0, nil }\n";
    assert!(diags(src).is_empty());
}

#[test]
fn parameters_and_results_are_skipped() {
    let src = "package main\n\nfunc f(a int, b string) (n int, err error) {\n\treturn\n}\n";
    assert!(diags(src).is_empty());
}

#[test]
fn closure_locals_are_checked_and_captures_count() {
    let used =
        "package main\n\nfunc main() {\n\tx := 1\n\tf := func() int { return x }\n\t_ = f\n}\n";
    assert!(diags(used).is_empty());
    let unused = "package main\n\nfunc main() {\n\tf := func() {\n\t\ty := 2\n\t}\n\t_ = f\n}\n";
    assert_eq!(messages(unused), ["declared and not used: y"]);
}

#[test]
fn unused_in_if_for_and_switch_headers() {
    let src = "package main\n\nfunc main() {\n\tif v := 1; true {\n\t}\n\tfor i := 0; ; {\n\t\tbreak\n\t}\n}\n";
    assert_eq!(
        messages(src),
        ["declared and not used: v", "declared and not used: i"]
    );
}

#[test]
fn type_switch_guard_used_in_any_clause() {
    let used = "package main\n\nfunc f(x any) {\n\tswitch v := x.(type) {\n\tcase int:\n\tcase string:\n\t\t_ = v\n\t}\n}\n";
    assert!(diags(used).is_empty());
    let unused = "package main\n\nfunc f(x any) {\n\tswitch v := x.(type) {\n\tcase int:\n\tcase string:\n\t}\n}\n";
    assert_eq!(messages(unused), ["declared and not used: v"]);
}

#[test]
fn generics_and_struct_tags_do_not_confuse() {
    let src = "package main\n\ntype P[T any] struct {\n\tA T `json:\"a\"`\n}\n\nfunc Map[T, U any](xs []T, f func(T) U) []U {\n\tout := make([]U, 0, len(xs))\n\tfor _, x := range xs {\n\t\tout = append(out, f(x))\n\t}\n\treturn out\n}\n";
    assert!(diags(src).is_empty());
}

#[test]
fn goto_labels_do_not_count_or_break() {
    let src =
        "package main\n\nfunc f() {\n\tn := 0\nloop:\n\tn++\n\tif n < 3 {\n\t\tgoto loop\n\t}\n}\n";
    assert!(diags(src).is_empty());
}

#[test]
fn init_function_and_method_locals() {
    let src = "package main\n\ntype T struct{}\n\nfunc init() {\n\tz := 1\n}\n\nfunc (t T) M() {\n\tw := 2\n}\n";
    assert_eq!(
        messages(src),
        ["declared and not used: z", "declared and not used: w"]
    );
}

#[test]
fn same_name_in_two_scopes_is_conservative() {
    // `x` occurs twice in the function, so neither declaration is reported.
    let src = "package main\n\nfunc f() {\n\tx := 1\n\t{\n\t\tx := 2\n\t\t_ = x\n\t}\n}\n";
    assert!(diags(src).is_empty());
}

#[test]
fn functions_with_syntax_errors_are_skipped() {
    let src = "package main\n\nfunc f() {\n\tx := 1\n\ty := \n}\n";
    assert!(!messages(src).iter().any(|m| m.starts_with("declared")));
}

#[test]
fn package_level_vars_are_never_unused() {
    assert!(diags("package main\n\nvar a = 1\nconst b = 2\n").is_empty());
}

// ---- duplicates -------------------------------------------------------

#[test]
fn duplicate_declarations_are_reported_at_the_second() {
    let src = "package main\n\nfunc a() {}\nfunc a() {}\n\ntype T int\ntype T string\n\nvar v = 1\nconst v = 2\n";
    let problems = diags(src);
    let got: Vec<(u32, &str)> = problems
        .iter()
        .map(|p| (p.start_line, p.message.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            (3, "a redeclared in this block"),
            (6, "T redeclared in this block"),
            (9, "v redeclared in this block"),
        ]
    );
    assert_eq!((problems[0].start_column, problems[0].end_column), (5, 6));
    assert!(!problems[0].unnecessary);
}

#[test]
fn init_blank_and_distinct_methods_are_fine() {
    let src = "package main\n\ntype A struct{}\ntype B struct{}\n\nfunc init() {}\nfunc init() {}\nfunc _() {}\nfunc _() {}\nvar _ = 1\nvar _ = 2\nfunc (A) M() {}\nfunc (B) M() {}\nfunc (*A) N() {}\n";
    assert!(diags(src).is_empty());
}

#[test]
fn duplicate_method_on_same_receiver() {
    let src = "package main\n\ntype A struct{}\n\nfunc (a A) M() {}\nfunc (a *A) M() {}\n";
    assert_eq!(messages(src), ["method A.M already declared"]);
}

#[test]
fn duplicates_in_grouped_declarations() {
    let src = "package main\n\nvar (\n\ta = 1\n\ta = 2\n)\n";
    assert_eq!(messages(src), ["a redeclared in this block"]);
}

// ---- general ----------------------------------------------------------

#[test]
fn problems_are_sorted_and_capped() {
    let mut src = String::from("package main\n\nfunc main() {\n");
    for i in 0..300 {
        src.push_str(&format!("\tv{i} := {i}\n"));
    }
    src.push_str("}\n");
    let problems = diags(&src);
    assert_eq!(problems.len(), 200);
    assert!(
        problems
            .windows(2)
            .all(|w| w[0].start_line <= w[1].start_line)
    );
}

#[test]
fn utf16_columns_after_wide_characters() {
    let src = "package main\n\nfunc main() {\n\ts := \"\u{1F600}\"; x := 1\n\t_ = s\n}\n";
    let p = &diags(src)[0];
    assert_eq!(p.message, "declared and not used: x");
    // tab, `s := "`, emoji (2), `"; ` then x.
    assert_eq!(p.start_column, 1 + 6 + 2 + 3);
}

#[test]
fn empty_and_garbage_input_do_not_panic() {
    for src in [
        "",
        "\n",
        "package",
        "}}}}",
        "\u{1F600}",
        "import (",
        "func (",
        "type T struct {",
    ] {
        let _ = diags(src);
    }
}
