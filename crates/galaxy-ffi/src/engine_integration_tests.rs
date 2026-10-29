//! End-to-end Go and Rust engine regressions.

use super::*;

const GO_MAIN: &str = "package main\n\ntype User struct {\n\tName string\n}\n\nfunc (u *User) Greet(greeting string) string {\n\treturn greeting + u.Name\n}\n\nfunc main() {\n\tu := &User{Name: \"a\"}\n\tu.Greet(\"hi\")\n}\n";

fn go_project() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("go.mod"),
        "module example.com/app\n\ngo 1.22\n",
    )
    .unwrap();
    let main = dir.path().join("main.go");
    std::fs::write(&main, GO_MAIN).unwrap();
    (dir, main.to_string_lossy().into_owned())
}

fn until(what: &str, mut ask: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ask() {
        assert!(Instant::now() < deadline, "{what}: engine never answered");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn go_files_are_registered_and_other_files_are_not() {
    let (dir, main_path) = go_project();
    let core = Core::new();
    let main = core.open_document(main_path).unwrap();
    assert!(core.register_document(main, "go".into(), String::new()));
    let notes = dir.path().join("notes.txt");
    std::fs::write(&notes, "x").unwrap();
    let notes = core
        .open_document(notes.to_string_lossy().into_owned())
        .unwrap();
    assert!(!core.register_document(notes, "text".into(), String::new()));
}

#[test]
fn go_navigation_comes_from_the_engine() {
    let (_dir, main_path) = go_project();
    let core = Core::new();
    let main = core.open_document(main_path).unwrap();
    until("definition", || {
        core.request_definition(main.clone(), 12, 4)
    });
    let found = core.definition_locations(main.clone());
    assert_eq!(found.len(), 1);
    assert!(found[0].uri.ends_with("main.go"));
    assert_eq!((found[0].line, found[0].column), (6, 15));
    until("hover", || core.request_hover(main.clone(), 12, 4));
    let hover = core.hover(main.clone()).unwrap();
    assert!(hover.contents.contains("Greet"), "{}", hover.contents);
    until("references", || {
        core.request_references(main.clone(), 12, 4)
    });
    let mut lines: Vec<u64> = core
        .references(main.clone())
        .into_iter()
        .map(|r| r.line)
        .collect();
    lines.sort();
    assert_eq!(lines, [6, 12]);
    assert!(core.request_rename(main.clone(), 12, 4, "Hello".into()));
    let edits = core.rename_edits(main.clone());
    assert_eq!(edits.len(), 2);
    assert!(edits.iter().all(|e| e.new_text == "Hello"));
    assert!(!core.request_rename(main.clone(), 12, 4, "1bad".into()));
    until("symbols", || core.request_document_symbols(main.clone()));
    let names: Vec<String> = core
        .document_symbols(main)
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert!(names.contains(&"User".to_owned()), "{names:?}");
    assert!(names.contains(&"Greet".to_owned()), "{names:?}");
}

#[test]
fn rust_definition_comes_from_the_engine() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.rs");
    std::fs::write(&path, "fn greet() {}\nfn main() { greet(); }\n").unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    until("Rust definition", || {
        core.request_definition(document.clone(), 1, 14)
    });
    let found = core.definition_locations(document);
    assert_eq!(found.len(), 1);
    assert!(found[0].uri.ends_with("main.rs"));
    assert_eq!((found[0].line, found[0].column), (0, 3));
}

#[test]
fn rust_diagnostics_are_rechecked_only_after_a_document_edit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.rs");
    std::fs::write(&path, "fn main() { missing(); }\n").unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    assert!(core.register_document(document.clone(), "rust".into(), String::new()));
    core.poll_analysis();
    assert!(!document.lock().diagnostics.is_empty());
    let uri = file_uri(&path);
    let revision = document.revision();
    assert_eq!(core.rust_checked.lock().unwrap().get(&uri), Some(&revision));
    document.apply_rename_edits(vec![RenameEdit {
        uri: String::new(),
        start_line: 0,
        start_character: 12,
        end_line: 0,
        end_character: 19,
        new_text: "println!".into(),
    }]);
    core.poll_analysis();
    assert!(document.lock().diagnostics.is_empty());
    assert_eq!(
        core.rust_checked.lock().unwrap().get(&uri),
        Some(&document.revision())
    );
}

#[test]
fn go_completion_lists_members_after_a_dot() {
    let (_dir, main_path) = go_project();
    let core = Core::new();
    let main = core.open_document(main_path).unwrap();
    assert!(main.apply_rename_edits(vec![RenameEdit {
        uri: String::new(),
        start_line: 12,
        start_character: 3,
        end_line: 12,
        end_character: 14,
        new_text: String::new()
    }]));
    until("completion", || {
        core.request_completion(main.clone(), 12, 3)
    });
    let labels: Vec<String> = core
        .completions(main)
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert!(labels.contains(&"Name".to_owned()), "{labels:?}");
    assert!(labels.contains(&"Greet".to_owned()), "{labels:?}");
}

#[test]
fn go_problems_reach_the_document_without_a_language_server() {
    let (_dir, main_path) = go_project();
    let core = Core::new();
    let main = core.open_document(main_path).unwrap();
    assert!(core.register_document(main.clone(), "go".into(), String::new()));
    let wait = |want: usize| {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            core.poll_analysis();
            let found = main.lock().diagnostics.clone();
            if found.len() == want {
                return found;
            }
            assert!(Instant::now() < deadline, "got {found:?}, wanted {want}");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    wait(0);
    assert!(main.apply_rename_edits(vec![RenameEdit {
        uri: String::new(),
        start_line: 12,
        start_character: 1,
        end_line: 12,
        end_character: 1,
        new_text: "unused := 1\n\t".into()
    }]));
    let found = wait(1);
    assert!(found[0].message.contains("unused"), "{found:?}");
    assert!(main.undo());
    wait(0);
}
