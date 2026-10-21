use std::path::{Path, PathBuf};

use super::*;
use crate::go::modules::GoProject;

const MAIN: &str = "package main\n\ntype User struct {\n\tName string\n}\n\n// Greet says hello.\nfunc (u *User) Greet(greeting string, times int) string {\n\treturn greeting + u.Name\n}\n\nfunc main() {\n\tu := &User{Name: \"a\"}\n\tu.Greet(\"hi\", 1)\n}\n";

fn project() -> (tempfile::TempDir, GoEngine, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("app");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("go.mod"), "module example.com/app\n\ngo 1.22\n").unwrap();
    let file = root.join("main.go");
    std::fs::write(&file, MAIN).unwrap();
    let goroot = tmp.path().join("goroot");
    std::fs::create_dir_all(goroot.join("src/runtime")).unwrap();
    std::fs::write(goroot.join("src/runtime/rt.go"), "package runtime\n").unwrap();
    let project = GoProject::detect_with(&root, Some(goroot), None);
    (tmp, GoEngine::with_project(project), file)
}

fn at(text: &str, needle: &str, extra: usize) -> (u32, u32) {
    let offset = text.find(needle).unwrap() + extra;
    let line = text[..offset].matches('\n').count() as u32;
    let start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    (line, text[start..offset].encode_utf16().count() as u32)
}

#[test]
fn project_root_prefers_go_work_then_go_mod_then_git() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("ws");
    let module = work.join("svc");
    std::fs::create_dir_all(module.join("pkg")).unwrap();
    std::fs::write(module.join("go.mod"), "module x\n").unwrap();
    assert_eq!(project_root(&module.join("pkg/a.go")), module);
    std::fs::write(work.join("go.work"), "go 1.22\n").unwrap();
    assert_eq!(project_root(&module.join("pkg/a.go")), work);
    let loose = tmp.path().join("loose");
    std::fs::create_dir_all(loose.join(".git")).unwrap();
    assert_eq!(project_root(&loose.join("a.go")), loose);
    let bare = tmp.path().join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    assert_eq!(project_root(&bare.join("a.go")), bare);
}

#[test]
fn definition_of_a_method_call_points_at_its_declaration() {
    let (_tmp, mut engine, file) = project();
    let (line, column) = at(MAIN, "Greet(\"hi\"", 1);
    let targets = engine.definition(&file, MAIN, line, column);
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].path, file);
    assert_eq!(targets[0].line, 7);
}

#[test]
fn hover_shows_the_signature_and_doc() {
    let (_tmp, mut engine, file) = project();
    let (line, column) = at(MAIN, "Greet(\"hi\"", 1);
    let hover = engine.hover(&file, MAIN, line, column).unwrap();
    assert!(hover.markdown.starts_with("```go\nfunc (u *User) Greet("));
    assert!(hover.markdown.contains("Greet says hello."));
    assert_eq!(hover.line, line);
}

#[test]
fn signature_reports_the_active_argument() {
    let (_tmp, mut engine, file) = project();
    let (line, column) = at(MAIN, "\"hi\", 1", 6);
    let signature = engine.signature(&file, MAIN, line, column).unwrap();
    assert_eq!(signature.parameters, ["greeting string", "times int"]);
    assert_eq!(signature.active, 1);
}

#[test]
fn completion_lists_members_after_a_dot() {
    let (_tmp, mut engine, file) = project();
    let text = MAIN.replace("u.Greet(\"hi\", 1)", "u.");
    let (line, column) = at(&text, "u.\n", 2);
    let items = engine.complete(&file, &text, line, column);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(labels.contains(&"Name"), "{labels:?}");
    assert!(labels.contains(&"Greet"), "{labels:?}");
}

#[test]
fn diagnostics_flag_an_unused_variable() {
    let (_tmp, mut engine, file) = project();
    let text = MAIN.replace("u.Greet(\"hi\", 1)", "unused := 1");
    let problems = engine.diagnostics(&file, &text);
    assert!(
        problems.iter().any(|p| p.message.contains("unused")),
        "{problems:?}"
    );
}

#[test]
fn references_and_rename_cover_every_use() {
    let (_tmp, mut engine, file) = project();
    let (line, column) = at(MAIN, "Greet(greeting", 1);
    let found = engine.references(&file, MAIN, line, column);
    assert_eq!(found.len(), 2, "{found:?}");
    let renamed = engine.rename(&file, MAIN, line, column, "Hello").unwrap();
    assert_eq!(renamed.len(), 2);
    assert!(engine.rename(&file, MAIN, line, column, "1bad").is_err());
    assert!(engine.rename(&file, MAIN, 0, 0, "x").is_err());
}

#[test]
fn unsaved_text_wins_over_the_disk() {
    let (_tmp, mut engine, file) = project();
    let edited = MAIN.replace("greeting + u.Name", "greeting");
    engine.diagnostics(&file, &edited);
    assert_eq!(engine.open_text(&file).as_deref(), Some(edited.as_str()));
    let _: &Path = &file;
}

#[test]
fn byte_at_counts_utf16_columns() {
    let text = "a😀b\nxy";
    assert_eq!(byte_at(text, 0, 3), "a😀".len());
    assert_eq!(byte_at(text, 1, 1), text.len() - 1);
    assert_eq!(byte_at(text, 9, 0), text.len());
}
