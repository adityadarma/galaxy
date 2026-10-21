use super::*;
use crate::go::diag::fixture::{Fx, apply, put};

/// Splits a `|` cursor marker out of `text`.
fn cursor(text: &str) -> (String, u32, u32) {
    let at = text.find('|').expect("cursor marker");
    let src = format!("{}{}", &text[..at], &text[at + 1..]);
    let line = text[..at].matches('\n').count() as u32;
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let column = text[start..at].encode_utf16().count() as u32;
    (src, line, column)
}

fn run(fx: &mut Fx, marked: &str) -> (String, Vec<Action>) {
    let (src, line, column) = cursor(marked);
    let path = fx.main();
    let found = actions(&mut fx.engine, &path, &src, line, column);
    (src, found)
}

fn titles(found: &[Action]) -> Vec<&str> {
    found.iter().map(|a| a.title.as_str()).collect()
}

fn result(src: &str, found: &[Action], title: &str) -> String {
    let action = found
        .iter()
        .find(|a| a.title == title)
        .unwrap_or_else(|| panic!("no {title:?} in {:?}", titles(found)));
    apply(src, &action.edits)
}

// ---- add import -------------------------------------------------------

#[test]
fn offers_candidates_std_first() {
    let mut fx = Fx::new(&[]);
    let (_, found) = run(&mut fx, "package main\n\nvar _ = ht|tp.Get\n");
    assert_eq!(
        titles(&found),
        [
            "Import \"net/http\"",
            "Import \"example.com/app/lib/http\"",
            "Import \"example.com/other/http\"",
        ]
    );
}

#[test]
fn cursor_may_sit_on_the_member() {
    let mut fx = Fx::new(&[]);
    let (_, found) = run(&mut fx, "package main\n\nvar _ = http.G|et\n");
    assert_eq!(found[0].title, "Import \"net/http\"");
}

#[test]
fn type_position_qualifier_gets_actions() {
    let mut fx = Fx::new(&[]);
    let (_, found) = run(
        &mut fx,
        "package main\n\nfunc f(w ht|tp.ResponseWriter) {}\n",
    );
    assert_eq!(found[0].title, "Import \"net/http\"");
}

#[test]
fn insert_into_sorted_position_of_std_group() {
    let mut fx = Fx::new(&[]);
    let (src, found) = run(
        &mut fx,
        "package main\n\nimport (\n\t\"fmt\"\n\t\"os\"\n\n\t\"example.com/dep\"\n)\n\nvar _ = fmt.Sprint\nvar _ = os.Args\nvar _ = ht|tp.Get\n",
    );
    assert_eq!(
        result(&src, &found, "Import \"net/http\""),
        "package main\n\nimport (\n\t\"fmt\"\n\t\"net/http\"\n\t\"os\"\n\n\t\"example.com/dep\"\n)\n\nvar _ = fmt.Sprint\nvar _ = os.Args\nvar _ = http.Get\n"
    );
}

#[test]
fn insert_at_end_of_std_group() {
    let mut fx = Fx::new(&[]);
    let (src, found) = run(
        &mut fx,
        "package main\n\nimport (\n\t\"fmt\"\n\n\t\"example.com/dep\"\n)\n\nvar _ = fmt.Sprint\nvar _ = dep.X\nvar _ = so|rt.Ints\n",
    );
    assert_eq!(
        result(&src, &found, "Import \"sort\""),
        "package main\n\nimport (\n\t\"fmt\"\n\t\"sort\"\n\n\t\"example.com/dep\"\n)\n\nvar _ = fmt.Sprint\nvar _ = dep.X\nvar _ = sort.Ints\n"
    );
}

#[test]
fn third_party_goes_into_the_last_group() {
    let mut fx = Fx::new(&[]);
    let (src, found) = run(
        &mut fx,
        "package main\n\nimport (\n\t\"fmt\"\n\n\t\"example.com/dep\"\n)\n\nvar _ = fmt.Sprint\nvar _ = dep.X\nvar _ = ut|il.X\n",
    );
    assert_eq!(
        result(&src, &found, "Import \"example.com/dep/util\""),
        "package main\n\nimport (\n\t\"fmt\"\n\n\t\"example.com/dep\"\n\t\"example.com/dep/util\"\n)\n\nvar _ = fmt.Sprint\nvar _ = dep.X\nvar _ = util.X\n"
    );
}

#[test]
fn third_party_starts_a_new_group_after_std_only_block() {
    let mut fx = Fx::new(&[]);
    let (src, found) = run(
        &mut fx,
        "package main\n\nimport (\n\t\"fmt\"\n)\n\nvar _ = fmt.Sprint\nvar _ = ut|il.X\n",
    );
    assert_eq!(
        result(&src, &found, "Import \"example.com/dep/util\""),
        "package main\n\nimport (\n\t\"fmt\"\n\n\t\"example.com/dep/util\"\n)\n\nvar _ = fmt.Sprint\nvar _ = util.X\n"
    );
}

#[test]
fn std_starts_a_new_group_before_third_party_only_block() {
    let mut fx = Fx::new(&[]);
    let (src, found) = run(
        &mut fx,
        "package main\n\nimport (\n\t\"example.com/dep\"\n)\n\nvar _ = dep.X\nvar _ = so|rt.Ints\n",
    );
    assert_eq!(
        result(&src, &found, "Import \"sort\""),
        "package main\n\nimport (\n\t\"sort\"\n\n\t\"example.com/dep\"\n)\n\nvar _ = dep.X\nvar _ = sort.Ints\n"
    );
}

#[test]
fn single_import_becomes_a_block() {
    let mut fx = Fx::new(&[]);
    let (src, found) = run(
        &mut fx,
        "package main\n\nimport \"os\"\n\nvar _ = os.Args\nvar _ = ht|tp.Get\n",
    );
    assert_eq!(
        result(&src, &found, "Import \"net/http\""),
        "package main\n\nimport (\n\t\"net/http\"\n\t\"os\"\n)\n\nvar _ = os.Args\nvar _ = http.Get\n"
    );
}

#[test]
fn single_import_with_alias_and_other_group() {
    let mut fx = Fx::new(&[]);
    let (src, found) = run(
        &mut fx,
        "package main\n\nimport d \"example.com/dep\"\n\nvar _ = d.X\nvar _ = ht|tp.Get\n",
    );
    assert_eq!(
        result(&src, &found, "Import \"net/http\""),
        "package main\n\nimport (\n\t\"net/http\"\n\n\td \"example.com/dep\"\n)\n\nvar _ = d.X\nvar _ = http.Get\n"
    );
}

#[test]
fn no_imports_adds_one_after_the_package_clause() {
    let mut fx = Fx::new(&[]);
    let (src, found) = run(&mut fx, "package main\n\nvar _ = ht|tp.Get\n");
    assert_eq!(
        result(&src, &found, "Import \"net/http\""),
        "package main\n\nimport \"net/http\"\n\nvar _ = http.Get\n"
    );
}

#[test]
fn crlf_files_keep_their_line_endings() {
    let mut fx = Fx::new(&[]);
    let (src, found) = run(
        &mut fx,
        "package main\r\n\r\nimport (\r\n\t\"os\"\r\n)\r\n\r\nvar _ = os.Args\r\nvar _ = ht|tp.Get\r\n",
    );
    assert_eq!(
        result(&src, &found, "Import \"net/http\""),
        "package main\r\n\r\nimport (\r\n\t\"net/http\"\r\n\t\"os\"\r\n)\r\n\r\nvar _ = os.Args\r\nvar _ = http.Get\r\n"
    );
}

#[test]
fn no_import_actions_for_known_names() {
    let mut fx = Fx::new(&[]);
    for marked in [
        // Already imported.
        "package main\n\nimport \"net/http\"\n\nvar _ = ht|tp.Get\n",
        // A local variable.
        "package main\n\nfunc f() {\n\thttp := 1\n\t_ = ht|tp.X\n}\n",
        // A parameter.
        "package main\n\nfunc f(http int) { _ = ht|tp.X }\n",
        // Package-level declarations.
        "package main\n\nvar http = 1\n\nvar _ = ht|tp.X\n",
        "package main\n\nfunc http() {}\n\nvar _ = ht|tp.X\n",
        // Not a qualifier.
        "package main\n\nvar _ = ht|tp\n",
        // No candidates.
        "package main\n\nvar _ = zz|z.X\n",
    ] {
        let (_, found) = run(&mut fx, marked);
        assert!(
            !found.iter().any(|a| a.title.starts_with("Import ")),
            "{marked}: {:?}",
            titles(&found)
        );
    }
}

#[test]
fn already_imported_paths_are_not_offered_twice() {
    let mut fx = Fx::new(&[]);
    let (_, found) = run(
        &mut fx,
        "package main\n\nimport h \"net/http\"\n\nvar _ = h.Get\nvar _ = ht|tp.Get\n",
    );
    assert!(!titles(&found).contains(&"Import \"net/http\""));
}

#[test]
fn candidates_are_capped_and_ranked_by_length() {
    let mut fx = Fx::new(&[]);
    for i in 0..12 {
        put(
            &fx.app,
            &format!("m{}/zed/zed.go", "x".repeat(i)),
            "package zed\n",
        );
    }
    fx.engine.reset_index();
    let (_, found) = run(&mut fx, "package main\n\nvar _ = ze|d.X\n");
    let names = titles(&found);
    assert_eq!(names.len(), 8, "{names:?}");
    assert_eq!(names[0], "Import \"example.com/app/m/zed\"");
    assert_eq!(names[1], "Import \"example.com/app/mx/zed\"");
}

// ---- remove imports ---------------------------------------------------

#[test]
fn removes_the_unused_import_on_the_cursor_line() {
    let mut fx = Fx::new(&[]);
    let (src, found) = run(
        &mut fx,
        "package main\n\nimport (\n\t\"fmt\"\n\t\"o|s\"\n)\n\nvar _ = fmt.Sprint\n",
    );
    assert_eq!(titles(&found), ["Remove unused import \"os\""]);
    assert_eq!(
        apply(&src, &found[0].edits),
        "package main\n\nimport (\n\t\"fmt\"\n)\n\nvar _ = fmt.Sprint\n"
    );
}

#[test]
fn removes_a_single_unused_import_declaration() {
    let mut fx = Fx::new(&[]);
    let (src, found) = run(&mut fx, "package main\n\nimport \"o|s\"\n\nvar a = 1\n");
    assert_eq!(
        result(&src, &found, "Remove unused import \"os\""),
        "package main\n\nvar a = 1\n"
    );
}

#[test]
fn remove_all_unused_imports() {
    let mut fx = Fx::new(&[]);
    let (src, found) = run(
        &mut fx,
        "package main\n\nimport (\n\t\"fmt\"\n\t\"io\"\n\t\"os\"\n\n\t\"example.com/dep\"\n)\n\nvar _ = os.Args|\n",
    );
    assert_eq!(titles(&found), ["Remove unused imports"]);
    assert_eq!(
        apply(&src, &found[0].edits),
        "package main\n\nimport (\n\t\"os\"\n)\n\nvar _ = os.Args\n"
    );
}

#[test]
fn remove_all_offered_next_to_the_single_action() {
    let mut fx = Fx::new(&[]);
    let (_, found) = run(
        &mut fx,
        "package main\n\nimport (\n\t\"io|\"\n\t\"os\"\n)\n",
    );
    assert_eq!(
        titles(&found),
        ["Remove unused import \"io\"", "Remove unused imports"]
    );
}

#[test]
fn removing_a_whole_group_drops_its_blank_line() {
    let mut fx = Fx::new(&[]);
    let (src, found) = run(
        &mut fx,
        "package main\n\nimport (\n\t\"fmt\"\n\n\t\"example.com/dep\"\n)\n\nvar _ = fmt.Sprint|\n",
    );
    assert_eq!(
        apply(&src, &found[0].edits),
        "package main\n\nimport (\n\t\"fmt\"\n)\n\nvar _ = fmt.Sprint\n"
    );
}

#[test]
fn used_imports_and_other_lines_offer_nothing() {
    let mut fx = Fx::new(&[]);
    let (_, found) = run(
        &mut fx,
        "package main\n\nimport \"o|s\"\n\nvar _ = os.Args\n",
    );
    assert!(found.is_empty());
    let (_, found) = run(&mut fx, "package main\n\nfunc f() {|}\n");
    assert!(found.is_empty());
}

#[test]
fn blank_and_unresolved_imports_are_never_removed() {
    let mut fx = Fx::new(&[]);
    let (_, found) = run(
        &mut fx,
        "package main\n\nimport (\n\t_ \"o|s\"\n\t\"github.com/nobody/missing\"\n)\n",
    );
    assert!(found.is_empty());
}

#[test]
fn comments_attached_to_a_removed_import_go_with_it() {
    let mut fx = Fx::new(&[]);
    let (src, found) = run(
        &mut fx,
        "package main\n\nimport (\n\t\"fmt\"\n\t// the os package\n\t\"o|s\"\n)\n\nvar _ = fmt.Sprint\n",
    );
    assert_eq!(
        apply(&src, &found[0].edits),
        "package main\n\nimport (\n\t\"fmt\"\n)\n\nvar _ = fmt.Sprint\n"
    );
}

#[test]
fn import_sharing_a_line_is_left_alone() {
    let mut fx = Fx::new(&[]);
    let (_, found) = run(
        &mut fx,
        "package main\n\nimport (\n\t\"fmt\"; \"o|s\"\n)\n\nvar _ = fmt.Sprint\n",
    );
    assert!(found.is_empty(), "{:?}", titles(&found));
}

#[test]
fn garbage_does_not_panic() {
    let mut fx = Fx::new(&[]);
    for src in ["", "package", "import (", "import \"", "\u{1F600}"] {
        let path = fx.main();
        let _ = actions(&mut fx.engine, &path, src, 0, 3);
        let _ = actions(&mut fx.engine, &path, src, 99, 99);
    }
}
