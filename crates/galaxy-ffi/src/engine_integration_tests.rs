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
    // A ceiling, not a delay: CI runners index far slower than a laptop
    // while the rest of the suite runs in parallel.
    let deadline = Instant::now() + Duration::from_secs(240);
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
fn rust_navigation_resolves_dependency_constant_and_into_at_editor_positions() {
    let dir = tempfile::tempdir().unwrap();
    let dependency = dir.path().join("tree-sitter-typescript");
    let app = dir.path().join("app");
    std::fs::create_dir_all(dependency.join("src")).unwrap();
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        dependency.join("Cargo.toml"),
        "[package]\nname = \"tree-sitter-typescript\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        dependency.join("src/lib.rs"),
        "pub const LANGUAGE_TYPESCRIPT: u8 = 1;\n",
    )
    .unwrap();
    std::fs::write(
        app.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\ntree-sitter-typescript.workspace = true\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"tree-sitter-typescript\"]\n[workspace.dependencies]\ntree-sitter-typescript = { path = \"tree-sitter-typescript\" }\n",
    )
    .unwrap();
    let source = "fn language() { let _ = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(); }\n";
    let path = app.join("src/lib.rs");
    std::fs::write(&path, source).unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    let into = source.find("into").unwrap() as u64;
    for column in into..=into + 4 {
        until("Rust Into definition", || {
            core.request_definition_for_navigation(document.clone(), 0, column, "definition".into())
        });
        let found = core.definition_locations(document.clone());
        assert_eq!(found.len(), 1, "column {column}");
        assert!(
            found[0].uri.ends_with("core/src/convert/mod.rs"),
            "column {column}: {}",
            found[0].uri
        );
    }
}

#[test]
fn rust_navigation_resolves_into_before_workspace_index_is_ready() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.rs");
    let source = "fn language(value: u8) { let _ = value.into(); }\n";
    std::fs::write(&path, source).unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    let root = rust::project_root(&path);
    core.rust.lock().unwrap().insert(
        root.clone(),
        Arc::new(Mutex::new(RustEngine::pending(&root))),
    );

    let column = source.find("into").unwrap() as u64 + 4;
    assert!(core.request_definition_for_navigation(
        document.clone(),
        0,
        column,
        "definition".into(),
    ));
    let found = core.definition_locations(document);
    assert_eq!(found.len(), 1);
    assert!(found[0].uri.ends_with("core/src/convert/mod.rs"));
}

#[test]
fn rust_navigation_resolves_real_tree_sitter_dependency() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../galaxy-core");
    let path = root.join("src/typescript/syntax.rs");
    let source = std::fs::read_to_string(&path).unwrap();
    let line = source
        .lines()
        .position(|line| line.contains("tree_sitter_javascript::LANGUAGE.into()"))
        .unwrap() as u64;
    let text = source.lines().nth(line as usize).unwrap();
    let column = text.find("tree_sitter_javascript").unwrap() as u64 + 1;
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    until("real Rust dependency definition", || {
        core.request_definition_for_navigation(document.clone(), line, column, "definition".into())
    });
    let found = core.definition_locations(document);
    assert_eq!(found.len(), 1);
    assert!(
        found[0]
            .uri
            .ends_with("tree-sitter-javascript-0.25.0/bindings/rust/lib.rs")
    );
}

#[test]
fn rust_diagnostics_are_rechecked_only_after_a_document_edit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.rs");
    // The Rust engine reports syntax errors only; a missing initializer is one.
    std::fs::write(&path, "fn main() { let x = ; }\n").unwrap();
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
        start_character: 20,
        end_line: 0,
        end_character: 20,
        new_text: "1".into(),
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
