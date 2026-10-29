//! Workspace indexing, exploration, diagnostics, and watcher integration regressions.

use super::*;

struct ChannelListener(Mutex<std::sync::mpsc::Sender<CoreEvent>>);

impl CoreListener for ChannelListener {
    fn on_event(&self, event: CoreEvent) {
        let _ = self.0.lock().unwrap().send(event);
    }
}

/// Asks again until the index, built off-thread, is ready.
fn until(what: &str, mut ask: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ask() {
        assert!(Instant::now() < deadline, "{what}: engine never answered");
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct Collect(std::sync::mpsc::Sender<WatchBatch>);

impl WorkspaceListener for Collect {
    fn on_changes(&self, batch: WatchBatch) {
        let _ = self.0.send(batch);
    }
}

#[test]
fn file_finder_searches_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/main.rs"), "").unwrap();
    std::fs::write(dir.path().join("README.md"), "").unwrap();
    let ws = Core::new()
        .open_workspace(dir.path().to_string_lossy().into_owned())
        .unwrap();
    let finder = ws.build_file_finder();
    assert_eq!(finder.file_count(), 2);
    let found = finder.search("main".into(), 10);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].rel_path, "src/main.rs");
    assert_eq!(found[0].name_start, 4);
    assert_eq!(
        found[0].highlights,
        [Utf16Range {
            location: 4,
            length: 4
        }]
    );
    finder.set_context(None, vec!["README.md".into()]);
    let recent = finder.search(String::new(), 10);
    assert_eq!(recent[0].rel_path, "README.md");
    assert!(recent[0].is_recent);
}

#[test]
fn opening_workspace_indexes_all_files_with_shared_progress() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    std::fs::write(dir.path().join("tsconfig.json"), "{}\n").unwrap();
    std::fs::write(dir.path().join("main.ts"), "export const value = 1;\n").unwrap();
    std::fs::write(
        dir.path().join("go.mod"),
        "module example.com/test\n\ngo 1.22\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("main.go"), "package test\n").unwrap();
    std::fs::write(dir.path().join("composer.json"), "{}\n").unwrap();

    let (tx, rx) = std::sync::mpsc::channel();
    let core = Core::new();
    core.set_listener(Arc::new(ChannelListener(Mutex::new(tx))));
    core.open_workspace(dir.path().to_string_lossy().into_owned())
        .unwrap();

    let root = dir.path().to_string_lossy().into_owned();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut started = None;
    let mut progress = 0;
    let mut finished = None;
    while Instant::now() < deadline && finished.is_none() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Ok(event) = rx.recv_timeout(remaining) else {
            break;
        };
        match event {
            CoreEvent::WorkspaceIndexStarted {
                root: event_root,
                total,
            } if event_root == root => {
                started = Some(total);
            }
            CoreEvent::WorkspaceIndexProgress {
                root: event_root,
                completed,
                ..
            } if event_root == root => {
                progress = completed;
            }
            CoreEvent::WorkspaceIndexFinished {
                root: event_root,
                total,
                ..
            } if event_root == root => {
                finished = Some(total);
            }
            _ => {}
        }
    }
    assert_eq!(started, Some(6));
    assert_eq!(finished, Some(6));
    assert_eq!(progress, 6);

    let plain = tempfile::tempdir().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let core = Core::new();
    core.set_listener(Arc::new(ChannelListener(Mutex::new(tx))));
    core.open_workspace(plain.path().to_string_lossy().into_owned())
        .unwrap();
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        CoreEvent::WorkspaceIndexStarted { total: 0, .. }
    ));
}

#[test]
fn workspace_lists_lazily_and_opens_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::create_dir_all(dir.path().join("target")).unwrap();
    std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(dir.path().join("README.md"), "hi").unwrap();

    let ws = Core::new()
        .open_workspace(dir.path().to_string_lossy().into_owned())
        .unwrap();
    let root: Vec<_> = ws
        .list_dir(String::new())
        .unwrap()
        .into_iter()
        .map(|e| (e.name, e.is_dir))
        .collect();
    assert_eq!(
        root,
        vec![
            ("src".into(), true),
            ("target".into(), true),
            ("README.md".into(), false)
        ]
    );
    assert!(ws.set_explorer_excludes(vec!["target".into()]));
    let root: Vec<_> = ws
        .list_dir(String::new())
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(root, vec!["src", "README.md"]);

    let src = ws.list_dir("src".into()).unwrap();
    assert_eq!(src[0].rel_path, "src/main.rs");
    let doc = ws.open_file(src[0].rel_path.clone()).unwrap();
    assert_eq!(doc.lines(0, 1), vec!["fn main() {}"]);
    assert!(doc.path().ends_with("src/main.rs"));

    assert!(ws.list_dir("..".into()).is_err());
    assert!(ws.open_file("../outside".into()).is_err());
    assert!(ws.open_file("/etc/passwd".into()).is_err());
    assert!(
        Core::new()
            .open_workspace("/nonexistent/dir".into())
            .is_err()
    );
}

#[test]
fn source_only_watcher_delta_updates_dart_and_rust_completion() {
    let dart_root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dart_root.path().join("lib")).unwrap();
    std::fs::write(dart_root.path().join("pubspec.yaml"), "name: delta_app\n").unwrap();
    let dart_model = dart_root.path().join("lib/model.dart");
    let dart_consumer = dart_root.path().join("lib/main.dart");
    std::fs::write(&dart_model, "class OldModel {}\n").unwrap();
    std::fs::write(&dart_consumer, "Ol\n").unwrap();
    let core = Core::new();
    core.open_workspace(dart_root.path().to_string_lossy().into_owned())
        .unwrap();
    let document = core
        .open_document(dart_consumer.to_string_lossy().into_owned())
        .unwrap();
    until("Dart workspace completion", || {
        core.request_completion(document.clone(), 0, 2)
    });
    assert!(
        core.completions(document.clone())
            .iter()
            .any(|item| item.label == "OldModel")
    );
    std::fs::write(&dart_model, "class NewModel {}\n").unwrap();
    core.apply_workspace_changes(
        dart_root.path().to_string_lossy().into_owned(),
        vec!["lib/model.dart".into()],
        false,
    );
    document.apply_rename_edits(vec![RenameEdit {
        uri: String::new(),
        start_line: 0,
        start_character: 0,
        end_line: 0,
        end_character: 2,
        new_text: "Ne".into(),
    }]);
    until("Dart delta completion", || {
        core.request_completion(document.clone(), 0, 2)
            && core
                .completions(document.clone())
                .iter()
                .any(|item| item.label == "NewModel")
    });
    assert!(
        core.completions(document.clone())
            .iter()
            .any(|item| item.label == "NewModel")
    );

    let rust_root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(rust_root.path().join("src")).unwrap();
    std::fs::write(
        rust_root.path().join("Cargo.toml"),
        "[package]\nname='delta'\nversion='0.1.0'\n",
    )
    .unwrap();
    let rust_model = rust_root.path().join("src/model.rs");
    let rust_consumer = rust_root.path().join("src/main.rs");
    std::fs::write(&rust_model, "pub fn old_name() {}\n").unwrap();
    std::fs::write(&rust_consumer, "ol\n").unwrap();
    let core = Core::new();
    core.open_workspace(rust_root.path().to_string_lossy().into_owned())
        .unwrap();
    let document = core
        .open_document(rust_consumer.to_string_lossy().into_owned())
        .unwrap();
    until("Rust workspace completion", || {
        core.request_completion(document.clone(), 0, 2)
    });
    assert!(
        core.completions(document.clone())
            .iter()
            .any(|item| item.label == "old_name")
    );
    std::fs::write(&rust_model, "pub fn new_name() {}\n").unwrap();
    core.apply_workspace_changes(
        rust_root.path().to_string_lossy().into_owned(),
        vec!["src/model.rs".into()],
        false,
    );
    document.apply_rename_edits(vec![RenameEdit {
        uri: String::new(),
        start_line: 0,
        start_character: 0,
        end_line: 0,
        end_character: 2,
        new_text: "ne".into(),
    }]);
    until("Rust delta completion", || {
        core.request_completion(document.clone(), 0, 2)
            && core
                .completions(document.clone())
                .iter()
                .any(|item| item.label == "new_name")
    });
    assert!(
        core.completions(document)
            .iter()
            .any(|item| item.label == "new_name")
    );
}

#[test]
fn workspace_rust_diagnostics_include_unopened_sources() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    let path = dir.path().join("src/unopened.rs");
    std::fs::write(&path, "fn run() { missing(); }\n").unwrap();
    let analysis = Arc::new(Mutex::new(BuiltinStore::default()));
    let engines = Arc::new(Mutex::new(HashMap::new()));

    publish_workspace_rust_diagnostics(
        &analysis,
        dir.path(),
        &["src/unopened.rs".into()],
        &engines,
    );

    let analysis = analysis.lock().unwrap();
    let problems = analysis.problems();
    assert!(
        problems
            .iter()
            .any(|problem| problem.uri == file_uri(&path))
    );
}

#[test]
fn workspace_typescript_refresh_keeps_rust_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let rust_path = dir.path().join("src/lib.rs");
    std::fs::create_dir_all(rust_path.parent().unwrap()).unwrap();
    std::fs::write(&rust_path, "fn run() { missing(); }\n").unwrap();
    let analysis = Arc::new(Mutex::new(BuiltinStore::default()));
    let engines = Arc::new(Mutex::new(HashMap::new()));
    publish_workspace_rust_diagnostics(&analysis, dir.path(), &["src/lib.rs".into()], &engines);

    publish_workspace_typescript_diagnostics(&analysis, dir.path(), &[]);

    assert!(
        analysis
            .lock()
            .unwrap()
            .problems()
            .iter()
            .any(|problem| problem.uri == file_uri(&rust_path))
    );
}

#[test]
fn watching_feeds_the_file_finder() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "x").unwrap();
    let ws = Core::new()
        .open_workspace(dir.path().to_string_lossy().into_owned())
        .unwrap();
    let finder = ws.build_file_finder();
    assert_eq!(finder.file_count(), 1);
    let (tx, rx) = std::sync::mpsc::channel();
    let watch = ws.watch(Arc::new(Collect(tx))).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));

    std::fs::write(dir.path().join("b.rs"), "x").unwrap();
    let mut paths = Vec::new();
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    while !paths.iter().any(|p: &String| p == "b.rs") && Instant::now() < deadline {
        if let Ok(b) = rx.recv_timeout(std::time::Duration::from_millis(500)) {
            paths.extend(b.paths);
        }
    }
    assert!(finder.apply_changes(Arc::clone(&ws), paths));
    assert_eq!(finder.file_count(), 2);
    assert_eq!(finder.search("b.rs".into(), 5)[0].rel_path, "b.rs");

    watch.stop();
    std::fs::write(dir.path().join("c.rs"), "x").unwrap();
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(800))
            .is_err()
    );
}
