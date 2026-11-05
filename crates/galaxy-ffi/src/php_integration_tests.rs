//! End-to-end PHP engine regressions.

use super::*;

#[test]
fn detects_php_frameworks_without_composer() {
    let laravel = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(laravel.path().join("app/Http")).unwrap();
    std::fs::write(
        laravel.path().join("app/Http/Controller.php"),
        "<?php class Controller {}\n",
    )
    .unwrap();
    assert!(php_workspace_detected(laravel.path()));

    let symfony = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(symfony.path().join("src/Controller")).unwrap();
    std::fs::write(
        symfony.path().join("src/Controller/Home.php"),
        "<?php class Home {}\n",
    )
    .unwrap();
    assert!(php_workspace_detected(symfony.path()));

    let plain = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(plain.path().join("node_modules/pkg")).unwrap();
    std::fs::write(
        plain.path().join("node_modules/pkg/Library.php"),
        "<?php class Library {}\n",
    )
    .unwrap();
    assert!(!php_workspace_detected(plain.path()));
}

fn php_project() -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("app")).unwrap();
    std::fs::write(
        root.join("composer.json"),
        r#"{"autoload":{"psr-4":{"App\\":"app/"}}}"#,
    )
    .unwrap();
    let order = root.join("app/Order.php");
    let report = root.join("app/Report.php");
    std::fs::write(
        &order,
        "<?php\nnamespace App;\n\nclass Order\n{\n    public function total(): int\n    {\n        return 1;\n    }\n}\n",
    )
    .unwrap();
    std::fs::write(
        &report,
        "<?php\nnamespace App;\n\nclass Report\n{\n    public function run(Order $order): int\n    {\n        return $order->total();\n    }\n}\n",
    )
    .unwrap();
    (
        dir,
        order.to_string_lossy().into_owned(),
        report.to_string_lossy().into_owned(),
    )
}

#[test]
fn php_problems_reach_the_document_without_a_language_server() {
    let (_dir, _order, report_path) = php_project();
    let core = Core::new();
    let report = core.open_document(report_path.clone()).unwrap();
    core.php_documents
        .lock()
        .unwrap()
        .insert(file_uri(Path::new(&report_path)), Arc::downgrade(&report));
    let wait = |want: usize| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            core.poll_analysis();
            let found = report.lock().diagnostics.clone();
            if found.len() == want {
                return found;
            }
            assert!(Instant::now() < deadline, "got {found:?}, wanted {want}");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    // A clean file has none; the first poll only notes the text.
    wait(0);

    // Typing a class that does not exist is reported once text settles.
    assert!(report.apply_rename_edits(vec![RenameEdit {
        uri: String::new(),
        start_line: 5,
        start_character: 24,
        end_line: 5,
        end_character: 24,
        new_text: "Ghost $g, ".into(),
    }]));
    let found = wait(1);
    assert_eq!(found[0].severity, 2);
    assert!(found[0].message.contains("Ghost"), "{found:?}");

    // Fixing it clears the problem.
    assert!(report.undo());
    wait(0);
}

#[test]
fn blade_templates_get_laravel_names_from_the_engine() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let write = |rel: &str, text: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("artisan", "<?php");
    write(
        "composer.json",
        r#"{"autoload":{"psr-4":{"App\\":"app/"}}}"#,
    );
    write("resources/views/welcome.blade.php", "hi");
    let page = root.join("resources/views/page.blade.php");
    write(
        "resources/views/page.blade.php",
        "@include('wel')\n@include('gone')\n",
    );
    let page_path = page.to_string_lossy().into_owned();

    let core = Core::new();
    let doc = core.open_document(page_path.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let items = loop {
        core.request_completion(doc.clone(), 0, 13);
        let items = core.completions(doc.clone());
        if !items.is_empty() {
            break items;
        }
        assert!(Instant::now() < deadline, "engine never became ready");
        std::thread::sleep(Duration::from_millis(20));
    };
    let item = items
        .iter()
        .find(|i| i.label == "welcome")
        .expect("welcome");
    assert_eq!(item.replace_from, Some(10));

    // A view that does not exist is reported, with a fix creating it.
    core.php_documents
        .lock()
        .unwrap()
        .insert(file_uri(&page), Arc::downgrade(&doc));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        core.poll_analysis();
        let found = doc.lock().diagnostics.clone();
        if found.len() == 2 {
            // `wel` is only a prefix of `welcome`, so it is missing too.
            assert!(found[1].message.contains("gone"), "{found:?}");
            break;
        }
        assert!(Instant::now() < deadline, "got {found:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(core.request_code_actions(doc.clone(), 1, 14));
    let actions = core.code_actions(doc);
    assert_eq!(actions.len(), 1);
    assert!(
        actions[0].edits[0]
            .uri
            .ends_with("resources/views/gone.blade.php"),
        "{actions:?}"
    );
}

#[test]
fn php_references_and_rename_come_from_the_engine() {
    let (_dir, order_path, report_path) = php_project();
    let core = Core::new();
    let report = core.open_document(report_path.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    // The index is built off-thread; ask again until it is ready.
    while !core.request_references(report.clone(), 7, 24) {
        assert!(Instant::now() < deadline, "engine never became ready");
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut found: Vec<(String, u64, u64)> = core
        .references(report.clone())
        .into_iter()
        .map(|r| {
            (
                r.uri.rsplit('/').next().unwrap().to_owned(),
                r.line,
                r.column,
            )
        })
        .collect();
    found.sort();
    assert_eq!(
        found,
        vec![("Order.php".into(), 5, 20), ("Report.php".into(), 7, 23)]
    );

    // Rename edits cover the declaration in the other file too.
    assert!(core.request_rename(report.clone(), 7, 24, "sum".into()));
    let mut edits: Vec<(String, u64, u64, String)> = core
        .rename_edits(report.clone())
        .into_iter()
        .map(|e| {
            (
                e.uri.rsplit('/').next().unwrap().to_owned(),
                e.start_line,
                e.start_character,
                e.new_text,
            )
        })
        .collect();
    edits.sort();
    assert_eq!(
        edits,
        vec![
            ("Order.php".into(), 5, 20, "sum".into()),
            ("Report.php".into(), 7, 23, "sum".into()),
        ]
    );
    // A name PHP would reject is not applied.
    assert!(!core.request_rename(report, 7, 24, "1 bad".into()));
    let _ = order_path;
}

#[test]
fn php_definition_comes_from_the_engine() {
    let (_dir, order_path, report_path) = php_project();
    let core = Core::new();
    let report = core.open_document(report_path).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !core.request_definition(report.clone(), 7, 24) {
        assert!(Instant::now() < deadline, "engine never became ready");
        std::thread::sleep(Duration::from_millis(20));
    }
    let locations = core.definition_locations(report);
    assert_eq!(locations.len(), 1);
    assert!(locations[0].uri.ends_with("app/Order.php"));
    assert_eq!(locations[0].line, 5);
    assert_eq!(locations[0].column, 20);
    let _ = order_path;
}

#[test]
fn php_signature_outline_and_actions_come_from_the_engine() {
    let (dir, _order_path, report_path) = php_project();
    let extra_path = dir.path().join("app/Extra.php");
    std::fs::write(
        &extra_path,
        "<?php\nnamespace App\\Services;\n\nuse App\\Order;\n\nclass Extra\n{\n}\n",
    )
    .unwrap();
    let core = Core::new();
    let report = core.open_document(report_path).unwrap();
    let extra = core
        .open_document(extra_path.to_string_lossy().into_owned())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);

    // Inside `total(` of `return $order->total();`.
    while !core.request_signature_help(report.clone(), 7, 29) {
        assert!(Instant::now() < deadline, "engine never became ready");
        std::thread::sleep(Duration::from_millis(20));
    }
    let signature = core.signature_help(report.clone()).expect("signature");
    assert_eq!(signature.label, "total(): int");
    assert!(signature.parameters.is_empty());

    assert!(core.request_document_symbols(report.clone()));
    let names: Vec<(String, String)> = core
        .document_symbols(report)
        .into_iter()
        .map(|s| (s.name, s.kind))
        .collect();
    // Kind 5 is a class, 6 a method.
    assert_eq!(
        names,
        vec![("Report".into(), "5".into()), ("run".into(), "6".into())]
    );

    // The unused `use App\Order;` on line 3 can be removed.
    assert!(core.request_code_actions(extra.clone(), 3, 5));
    let actions = core.code_actions(extra);
    assert_eq!(actions.len(), 1, "{actions:?}");
    assert_eq!(actions[0].title, "Remove unused import");
    let edit = &actions[0].edits[0];
    assert_eq!(
        (
            edit.start_line,
            edit.start_character,
            edit.end_line,
            edit.end_character
        ),
        (3, 0, 4, 0)
    );
    assert!(edit.new_text.is_empty());
}
