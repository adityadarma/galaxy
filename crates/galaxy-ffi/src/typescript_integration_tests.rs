//! TypeScript, JavaScript, and component integration regressions.

use super::*;

/// Asks again until the index, built off-thread, is ready.
fn until(what: &str, mut ask: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ask() {
        assert!(Instant::now() < deadline, "{what}: engine never answered");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Opens a real TypeScript file for tests that require a path on disk. The
/// `.ts` extension matters: navigation dispatches on the file's language.
fn file_doc(text: &str) -> (tempfile::TempDir, PathBuf, Arc<DocumentHandle>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.ts");
    std::fs::write(&path, text).unwrap();
    let doc = Core::new()
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    (dir, path, doc)
}

fn doc_at(path: &str, text: &str, caret_utf16: u64) -> Arc<DocumentHandle> {
    let doc = Arc::new(DocumentHandle::new(
        Document::from_text(text),
        PathBuf::from(path),
    ));
    doc.set_selections(
        vec![TextSelection {
            anchor: TextPos {
                line: 0,
                col_utf16: caret_utf16,
            },
            head: TextPos {
                line: 0,
                col_utf16: caret_utf16,
            },
        }],
        0,
    );
    doc
}

fn ecmascript_declaration_position(name: &str) -> (u64, u64) {
    let declaration_prefixes = [
        format!("declare const {name}"),
        format!("declare class {name}"),
        format!("declare function {name}"),
        format!("declare type {name}"),
        format!("interface {name}"),
        format!("{name}("),
        format!("{name}<"),
    ];
    let (line, declaration) = declaration_prefixes
        .iter()
        .find_map(|prefix| {
            ECMASCRIPT_BUILTINS
                .lines()
                .enumerate()
                .find(|(_, line)| line.trim_start().starts_with(prefix))
        })
        .unwrap_or_else(|| panic!("missing ECMAScript declaration for {name}"));
    (line as u64, declaration.find(name).unwrap() as u64)
}

#[test]
fn component_script_source_preserves_script_positions_and_rejects_templates() {
    let source =
        "<template>\n  <Button />\n</template>\n<script lang=\"ts\">\nconst count = 1\n</script>\n";
    let virtual_source = component_script_source("Counter.vue", source, 4).unwrap();
    assert_eq!(virtual_source.lines().nth(4), Some("const count = 1"));
    assert!(component_script_source("Counter.vue", source, 1).is_none());
    assert!(component_script_source("Counter.svelte", source, 1).is_none());
}

#[test]
fn component_script_source_accepts_svelte_scripts() {
    let source = "<h1>{count}</h1>\n<script>\nlet count = 0\n</script>\n";
    let virtual_source = component_script_source("Counter.svelte", source, 2).unwrap();
    assert_eq!(virtual_source.lines().nth(2), Some("let count = 0"));
}

/// Opens a component file on disk with a fresh `Core`.
fn component_doc(name: &str, text: &str) -> (tempfile::TempDir, Arc<Core>, Arc<DocumentHandle>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, text).unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    (dir, core, document)
}

#[test]
fn component_virtual_source_blanks_markup_and_keeps_utf16_columns() {
    let source = "<h1>😀 {a}</h1>\n<script>\nlet a = 1\n</script>\n<p>x</p>\n";
    let virtual_source = component_virtual_source("A.svelte", source);
    assert_eq!(virtual_source.lines().count(), source.lines().count());
    // The emoji is two UTF-16 units, so it becomes two blanks.
    assert_eq!(virtual_source.lines().next(), Some("               "));
    assert_eq!(virtual_source.lines().nth(2), Some("let a = 1"));
    assert!(!virtual_source.contains('<'));
    assert_eq!(component_virtual_source("a.ts", "x"), "x");
}

#[test]
fn component_markup_mentions_matches_words_store_and_kebab_tags() {
    let text = "<template><my-card />{{ total }} $count</template>\n<script setup>\n</script>\n";
    assert!(component_markup_mentions(text, "total"));
    assert!(component_markup_mentions(text, "count"));
    assert!(component_markup_mentions(text, "MyCard"));
    assert!(!component_markup_mentions(text, "tot"));
    assert!(!component_markup_mentions(text, "unused"));
    assert!(!component_markup_mentions(text, "Total"));
}

#[test]
fn vue_script_hover_outline_references_and_rename_use_the_typescript_engine() {
    let source = "<template>\n  <p>hi</p>\n</template>\n<script setup lang=\"ts\">\nfunction refresh() {}\nconst counter = 1\nrefresh()\nconsole.log(counter)\n</script>\n";
    let (_dir, core, document) = component_doc("Counter.vue", source);

    assert!(core.request_hover(document.clone(), 4, 10));
    let hover = core.hover(document.clone()).unwrap();
    assert!(hover.contents.contains("function `refresh`"), "{hover:?}");
    assert_eq!(hover.start_line, Some(4));

    assert!(core.request_document_symbols(document.clone()));
    let names: Vec<_> = core
        .document_symbols(document.clone())
        .into_iter()
        .map(|symbol| (symbol.name, symbol.line))
        .collect();
    assert!(names.contains(&("refresh".into(), 4)), "{names:?}");
    assert!(names.contains(&("counter".into(), 5)), "{names:?}");

    assert!(core.request_references(document.clone(), 6, 1));
    let lines: Vec<_> = core
        .references(document.clone())
        .into_iter()
        .map(|location| location.line)
        .collect();
    assert_eq!(lines, [4, 6]);

    assert!(core.request_rename(document.clone(), 5, 7, "total".into()));
    let edits = core.rename_edits(document);
    assert_eq!(
        edits
            .iter()
            .map(|edit| (edit.start_line, edit.start_character))
            .collect::<Vec<_>>(),
        [(5, 6), (7, 12)]
    );
    assert!(edits.iter().all(|edit| edit.new_text == "total"));
}

#[test]
fn vue_template_positions_and_template_used_names_stay_unresolved() {
    let source = "<template>\n  <p>{{ counter }}</p>\n</template>\n<script setup>\nconst counter = 1\n</script>\n";
    let (_dir, core, document) = component_doc("Counter.vue", source);

    // The cursor is on `counter` inside the template, which is not analysed.
    assert!(!core.request_hover(document.clone(), 1, 10));
    assert!(core.hover(document.clone()).is_none());
    assert!(!core.request_references(document.clone(), 1, 10));
    assert!(!core.request_rename(document.clone(), 1, 10, "total".into()));

    // The template uses `counter`, so a script-only rename would break it.
    assert!(!core.request_rename(document.clone(), 4, 7, "total".into()));
    assert!(core.rename_edits(document).is_empty());
}

#[test]
fn svelte_scripts_share_one_index_and_report_syntax_errors() {
    let source = "<script context=\"module\">\nexport const shared = 1\n</script>\n<script>\nlet local = shared\nlet broken = (\n</script>\n<h1>{local}</h1>\n";
    let (_dir, core, document) = component_doc("Card.svelte", source);

    // `shared` from the module script is visible from the instance script.
    assert!(core.request_definition(document.clone(), 4, 13));
    assert_eq!(
        core.definition_locations(document.clone())
            .into_iter()
            .map(|location| (location.line, location.column))
            .collect::<Vec<_>>(),
        [(1, 13)]
    );
    assert!(core.request_references(document.clone(), 4, 13));
    let lines: Vec<_> = core
        .references(document.clone())
        .into_iter()
        .map(|location| location.line)
        .collect();
    assert_eq!(lines, [1, 4]);

    assert!(core.register_document(document.clone(), "svelte".into(), String::new()));
    until("svelte syntax error", || {
        core.poll_analysis();
        !document.lock().diagnostics.is_empty()
    });
    let diagnostics = document.lock().diagnostics.clone();
    // Errors stay inside the scripts; the `{local}` markup never parses.
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| (4..=6).contains(&diagnostic.start_line)),
        "{diagnostics:?}"
    );
}

#[test]
fn component_without_scripts_has_no_outline_or_diagnostics() {
    let (_dir, core, document) = component_doc(
        "Plain.vue",
        "<template>\n  <div>{{ a + }}</div>\n</template>\n",
    );
    assert!(!core.request_document_symbols(document.clone()));
    assert!(core.register_document(document.clone(), "vue".into(), String::new()));
    core.poll_analysis();
    assert!(document.lock().diagnostics.is_empty());
}

/// Mirrors the published `@inertiajs/vue3` and `@inertiajs/core` type layout.
#[test]
fn vue_definition_resolves_inertia_package_reexports() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("package.json"), "{}\n").unwrap();
    let vue3 = root.join("node_modules/@inertiajs/vue3");
    let core_pkg = root.join("node_modules/@inertiajs/core");
    std::fs::create_dir_all(vue3.join("types")).unwrap();
    std::fs::create_dir_all(core_pkg.join("types")).unwrap();
    std::fs::write(
        vue3.join("package.json"),
        r#"{"name":"@inertiajs/vue3","type":"module","main":"dist/index.js","types":"types/index.d.ts"}"#,
    )
    .unwrap();
    std::fs::write(
        vue3.join("types/index.d.ts"),
        "export { http, progress, router } from '@inertiajs/core';\n\
         export { default as Head } from './head';\n\
         export { InertiaForm, InertiaFormProps, default as useForm } from './useForm';\n",
    )
    .unwrap();
    std::fs::write(
        vue3.join("types/head.d.ts"),
        "import { DefineComponent } from 'vue';\n\
         export type InertiaHead = DefineComponent<{\n    title?: string;\n}>;\n\
         declare const Head: InertiaHead;\n\
         export default Head;\n",
    )
    .unwrap();
    std::fs::write(
        vue3.join("types/useForm.d.ts"),
        "export interface InertiaFormProps<TForm extends object> { isDirty: boolean; }\n\
         export type InertiaForm<TForm extends object> = TForm & InertiaFormProps<TForm>;\n\
         export default function useForm<TForm>(rememberKey: string, data: TForm): InertiaForm<TForm>;\n\
         export default function useForm<TForm>(data: TForm): InertiaForm<TForm>;\n\
         export default function useForm<TForm>(): InertiaForm<TForm>;\n\
         export {};\n",
    )
    .unwrap();
    std::fs::write(
        core_pkg.join("package.json"),
        r#"{"name":"@inertiajs/core","type":"module","main":"dist/index.js","types":"types/index.d.ts","exports":{".":{"types":"./types/index.d.ts","import":"./dist/index.js"}}}"#,
    )
    .unwrap();
    std::fs::write(
        core_pkg.join("types/index.d.ts"),
        "import { Config } from './config';\n\
         import { Router } from './router';\n\
         export { http } from './http';\n\
         export { progress, default as setupProgress } from './progress';\n\
         export * from './types';\n\
         export { type Config, type Router };\n\
         export declare const router: Router;\n",
    )
    .unwrap();
    std::fs::write(
        core_pkg.join("types/router.d.ts"),
        "import Queue from './queue';\n\
         export declare class Router {\n\
         \x20   protected clientVisitQueue: Queue<Promise<void>>;\n\
         \x20   get<T extends RequestPayload = RequestPayload>(url: URL | string, data?: T): void;\n\
         \x20   post<T extends RequestPayload = RequestPayload>(url: URL | string, data?: T): void;\n\
         \x20   put<T extends RequestPayload = RequestPayload>(url: URL | string, data?: T): void;\n\
         \x20   delete<T extends RequestPayload = RequestPayload>(url: URL | string): void;\n\
         }\n",
    )
    .unwrap();
    std::fs::write(
        core_pkg.join("types/http.d.ts"),
        "export declare const http: {};\n",
    )
    .unwrap();
    let source = "<script setup lang=\"ts\">\n\
                  import { Head, useForm, router } from '@inertiajs/vue3';\n\
                  const submit = () => {\n\
                  \x20   router.put('/couriers/1', {});\n\
                  \x20   router.post('/couriers', {});\n\
                  \x20   router.delete('/couriers/1');\n\
                  };\n\
                  </script>\n";
    let page = root.join("resources/js/Pages/Outlets.vue");
    std::fs::create_dir_all(page.parent().unwrap()).unwrap();
    std::fs::write(&page, source).unwrap();
    let core = Core::new();
    let document = core
        .open_document(page.to_string_lossy().into_owned())
        .unwrap();

    let line = "import { Head, useForm, router } from '@inertiajs/vue3';";
    let cases = [
        ("Head", "vue3/types/head.d.ts", 4),
        ("useForm", "vue3/types/useForm.d.ts", 2),
        ("router", "core/types/index.d.ts", 6),
    ];
    for (name, file, expected_line) in cases {
        let column = line.find(name).unwrap() as u64 + 1;
        assert!(
            core.request_definition(document.clone(), 1, column),
            "{name}"
        );
        let locations = core.definition_locations(document.clone());
        assert_eq!(locations.len(), 1, "{name}: {locations:?}");
        assert!(
            locations[0].uri.ends_with(file) && locations[0].line == expected_line,
            "{name}: {locations:?}"
        );
    }

    // `router.put` follows `export declare const router: Router` to the
    // `Router` class methods in `router.d.ts`.
    for (line, method, expected_line) in [(3u64, "put", 5), (4, "post", 4), (5, "delete", 6)] {
        let column = "    router.".len() as u64 + 1;
        assert!(
            core.request_definition(document.clone(), line, column),
            "{method}"
        );
        let locations = core.definition_locations(document.clone());
        assert_eq!(locations.len(), 1, "{method}: {locations:?}");
        assert!(
            locations[0].uri.ends_with("core/types/router.d.ts")
                && locations[0].line == expected_line,
            "{method}: {locations:?}"
        );
    }
}

#[test]
fn component_script_insertion_line_stays_inside_script_setup() {
    let source = "<script setup lang=\"ts\">\nconst value = 1\n</script>\n";
    assert_eq!(
        component_script_insertion_line("Counter.vue", source, 1),
        Some(1)
    );
}

#[test]
fn typescript_navigation_cache_reuses_indexes_and_revalidates_disk_changes() {
    let dir = tempfile::tempdir().unwrap();
    let dependency = dir.path().join("dependency.ts");
    std::fs::write(&dependency, "export const first = 1;\n").unwrap();
    let mut cache = TypeScriptNavigationCache::default();

    let first = cache.index(&dependency).unwrap();
    let second = cache.index(&dependency).unwrap();
    assert!(Arc::ptr_eq(&first, &second));

    std::fs::write(&dependency, "export const changed = 22;\n").unwrap();
    let changed = cache.index(&dependency).unwrap();
    assert!(!Arc::ptr_eq(&first, &changed));
    assert!(changed.exported_declaration("changed").is_some());
}

#[test]
fn typescript_navigation_cache_revalidates_package_manifest_resolution() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    let package = dir.path().join("node_modules/pkg");
    std::fs::create_dir_all(&package).unwrap();
    let manifest = package.join("package.json");
    std::fs::write(&manifest, r#"{"exports":"./first.ts"}"#).unwrap();
    std::fs::write(package.join("first.ts"), "export const value = 1;\n").unwrap();
    std::fs::write(package.join("second.ts"), "export const value = 2;\n").unwrap();
    let importer = dir.path().join("main.ts");
    std::fs::write(&importer, "import { value } from 'pkg';\nvalue;\n").unwrap();
    let mut cache = TypeScriptNavigationCache::default();

    assert!(
        cache
            .resolve(&importer, "pkg")
            .unwrap()
            .ends_with("first.ts")
    );
    std::fs::write(&manifest, r#"{"exports":"./second.ts"}"#).unwrap();
    assert!(
        cache
            .resolve(&importer, "pkg")
            .unwrap()
            .ends_with("second.ts")
    );
}

#[test]
fn javascript_project_and_components_are_manifest_gated() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{"name":"web","dependencies":{"react":"19","vue":"3","svelte":"5"}}"#,
    )
    .unwrap();
    let react = src.join("Button.tsx");
    let vue = src.join("Panel.vue");
    let svelte = src.join("Card.svelte");
    std::fs::write(&react, "export const Button = () => null").unwrap();
    std::fs::write(&vue, "<template><div /></template>").unwrap();
    std::fs::write(&svelte, "<script>let value = 1;</script>").unwrap();
    let core = Core::new();
    let project = core
        .javascript_project(react.to_string_lossy().into_owned())
        .unwrap();
    assert_eq!(project.name.as_deref(), Some("web"));
    assert_eq!(project.frameworks, ["React", "Vue", "Svelte"]);
    let components = core.javascript_components(vue.to_string_lossy().into_owned());
    assert_eq!(
        components
            .iter()
            .map(|component| (
                component.framework.as_str(),
                component.name.as_str(),
                component
                    .symbols
                    .iter()
                    .map(|symbol| (symbol.name.as_str(), symbol.props.as_slice()))
                    .collect::<Vec<_>>(),
            ))
            .collect::<Vec<_>>(),
        vec![
            ("React", "Button", vec![]),
            ("Svelte", "Card", vec![("Card", &[][..])]),
            ("Vue", "Panel", vec![("Panel", &[][..])]),
        ]
    );
    assert_eq!(
        core.resolve_javascript_relative_import(
            "./Button.js".into(),
            vue.to_string_lossy().into_owned(),
        )
        .map(|target| target.uri),
        Some(file_uri(&react))
    );
}

#[test]
fn typescript_documents_provide_outline_and_parse_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("outline.ts");
    std::fs::write(
        &path,
        "export class App {}\nexport interface Config {}\nconst broken = (\n",
    )
    .unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.register_document(document.clone(), "typescript".into(), String::new()));
    assert_eq!(core.poll_analysis(), 1);
    let diagnostics = document.lock().diagnostics.clone();
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].severity, 1);
    assert!(diagnostics[0].message.contains("Syntax"), "{diagnostics:?}");

    assert!(core.request_document_symbols(document.clone()));
    let symbols: Vec<(String, String)> = core
        .document_symbols(document)
        .into_iter()
        .map(|symbol| (symbol.name, symbol.kind))
        .collect();
    assert_eq!(
        symbols,
        vec![("App".into(), "5".into()), ("Config".into(), "11".into())]
    );
}

#[test]
fn typescript_type_diagnostics_are_published_with_galaxy_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("types.ts");
    std::fs::write(
        &path,
        "const value: number = 'wrong';\nconst unknown = other?.field;\n",
    )
    .unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.register_document(document.clone(), "typescript".into(), String::new()));
    core.poll_analysis();
    let diagnostics = document.lock().diagnostics.clone();
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].severity, 1);
    assert!(
        diagnostics[0].message.contains("assignable"),
        "{diagnostics:?}"
    );
}

#[test]
fn typescript_completion_and_hover_are_local_and_range_exact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.ts");
    std::fs::write(
            &path,
            "import { remoteValue } from './dep';\nconst localValue = 1;\nfunction run(parameter: string) { return parameter; }\nloc\n",
        )
        .unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_completion(document.clone(), 3, 3));
    let labels: Vec<_> = core
        .completions(document.clone())
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert_eq!(labels, ["localValue"]);

    assert!(core.request_hover(document.clone(), 2, 9));
    let hover = core.hover(document).unwrap();
    assert!(hover.contents.contains("function `run`"), "{hover:?}");
    assert!(hover.contents.contains("main.ts"), "{hover:?}");
    assert_eq!(
        (
            hover.start_line,
            hover.start_character,
            hover.end_line,
            hover.end_character
        ),
        (Some(2), Some(9), Some(2), Some(12))
    );
}

#[test]
fn typescript_completion_includes_string_members_for_typed_parameters() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.ts");
    std::fs::write(
        &path,
        "function clean(filePath: string) { return filePath.starts; }\n",
    )
    .unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_completion(document.clone(), 0, 57));
    assert!(
        core.completions(document)
            .iter()
            .any(|item| item.label == "startsWith")
    );
}

#[test]
fn typescript_completion_includes_local_static_members_after_this() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.ts");
    std::fs::write(
            &path,
            "class Cdn {\n  static generateUrl(path: string) { return path; }\n  static generateUrls(paths: string[]) { return paths.map(path => this.gene); }\n}\n",
        )
        .unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_completion(document.clone(), 2, 75));
    assert!(
        core.completions(document)
            .iter()
            .any(|item| item.label == "generateUrl")
    );
}

#[test]
fn typescript_workspace_completion_imports_unique_exports() {
    let dir = tempfile::tempdir().unwrap();
    let models = dir.path().join("app/models");
    let controllers = dir.path().join("app/controllers");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::create_dir_all(&controllers).unwrap();
    std::fs::write(models.join("user.ts"), "export class User {}\n").unwrap();
    let consumer = controllers.join("auth.ts");
    std::fs::write(&consumer, "Us\n").unwrap();
    let core = Core::new();
    core.open_workspace(dir.path().to_string_lossy().into_owned())
        .unwrap();
    let document = core
        .open_document(consumer.to_string_lossy().into_owned())
        .unwrap();

    until("workspace TypeScript completion", || {
        core.request_completion(document.clone(), 0, 2)
    });
    let item = core
        .completions(document)
        .into_iter()
        .find(|item| item.label == "User")
        .unwrap();
    assert_eq!(item.detail.as_deref(), Some("class - app/models/user.ts"));
    assert_eq!(item.insert_text, "User");
    assert_eq!(item.additional_edits.len(), 1);
    assert_eq!(item.additional_edits[0].line, 0);
    assert_eq!(item.additional_edits[0].column, 0);
    assert_eq!(
        item.additional_edits[0].text,
        "import { User } from '../models/user.js';\n"
    );
}

#[test]
fn typescript_workspace_completion_prefers_package_import_alias_and_file_quotes() {
    let dir = tempfile::tempdir().unwrap();
    let services = dir.path().join("app/services");
    let controllers = dir.path().join("app/controllers");
    std::fs::create_dir_all(&services).unwrap();
    std::fs::create_dir_all(&controllers).unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r##"{"imports":{"#services/*":"./app/services/*.js"}}"##,
    )
    .unwrap();
    std::fs::write(
        services.join("auth_service.ts"),
        "export default class AuthService {}\n",
    )
    .unwrap();
    let consumer = controllers.join("auth_controller.ts");
    std::fs::write(&consumer, "import { inject } from \"x\"\nAuthSe\n").unwrap();
    let core = Core::new();
    core.open_workspace(dir.path().to_string_lossy().into_owned())
        .unwrap();
    let document = core
        .open_document(consumer.to_string_lossy().into_owned())
        .unwrap();

    until("workspace TypeScript completion", || {
        core.request_completion(document.clone(), 1, 6)
            && core
                .completions(document.clone())
                .iter()
                .any(|item| item.label == "AuthService")
    });
    let item = core
        .completions(document)
        .into_iter()
        .find(|item| item.label == "AuthService")
        .unwrap();
    assert_eq!(
        item.additional_edits[0].text,
        "import AuthService from \"#services/auth_service\";\n"
    );
}

#[test]
fn typescript_missing_import_of_workspace_export_warns() {
    let dir = tempfile::tempdir().unwrap();
    let services = dir.path().join("app/services");
    let controllers = dir.path().join("app/controllers");
    std::fs::create_dir_all(&services).unwrap();
    std::fs::create_dir_all(&controllers).unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r##"{"imports":{"#services/*":"./app/services/*.js"}}"##,
    )
    .unwrap();
    std::fs::write(
        services.join("auth_service.ts"),
        "export default class AuthService {}\n",
    )
    .unwrap();
    let consumer = controllers.join("auth_controller.ts");
    std::fs::write(
        &consumer,
        "// import AuthService from \"#services/auth_service\"\n\
         export default class AuthController {\n  \
         constructor(private authService: AuthService, other: Promise<string>) {}\n}\n",
    )
    .unwrap();
    let core = Core::new();
    core.open_workspace(dir.path().to_string_lossy().into_owned())
        .unwrap();
    let document = core
        .open_document(consumer.to_string_lossy().into_owned())
        .unwrap();
    assert!(core.register_document(document.clone(), "typescript".into(), String::new()));

    until("missing import warning", || {
        core.poll_analysis();
        !document.lock().diagnostics.is_empty()
    });
    let diagnostics = document.lock().diagnostics.clone();
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].severity, 2);
    assert_eq!(diagnostics[0].start_line, 2);
    assert!(
        diagnostics[0]
            .message
            .contains("'AuthService'. Did you forget to import it from '#services/auth_service'"),
        "{}",
        diagnostics[0].message
    );
}

#[test]
fn vue_completion_auto_imports_dependency_exports() {
    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("node_modules/vue");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{"dependencies":{"vue":"3"}}"#,
    )
    .unwrap();
    std::fs::write(
        package.join("package.json"),
        r#"{"exports":"./index.d.ts"}"#,
    )
    .unwrap();
    std::fs::write(
        package.join("index.d.ts"),
        "export declare function computed(): void;\n",
    )
    .unwrap();
    let path = dir.path().join("Counter.vue");
    std::fs::write(&path, "<script setup>\ncomp\n</script>\n").unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_completion(document.clone(), 1, 4));
    let item = core
        .completions(document)
        .into_iter()
        .find(|item| item.label == "computed")
        .unwrap();
    assert_eq!(item.insert_text, "computed");
    assert_eq!(item.additional_edits[0].line, 1);
    assert_eq!(
        item.additional_edits[0].text,
        "import { computed } from 'vue';\n"
    );
}

#[test]
fn vue_completion_merges_auto_import_into_an_existing_import() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{"dependencies":{"vue":"3"}}"#,
    )
    .unwrap();
    let complete = |name: &str, source: &str, line: u64, character: u64| {
        let path = dir.path().join(name);
        std::fs::write(&path, source).unwrap();
        let core = Core::new();
        let document = core
            .open_document(path.to_string_lossy().into_owned())
            .unwrap();
        assert!(core.request_completion(document.clone(), line, character));
        core.completions(document)
            .into_iter()
            .find(|item| item.label == "computed")
            .unwrap()
    };

    let merged = complete(
        "Merge.vue",
        "<script setup>\nimport { createApp, h } from 'vue'\ncomp\n</script>\n",
        2,
        4,
    );
    let edit = &merged.additional_edits[0];
    assert_eq!((edit.line, edit.column), (1, 21));
    assert_eq!(edit.text, ", computed");

    let default_only = complete(
        "Default.vue",
        "<script setup>\nimport Vue from 'vue'\ncomp\n</script>\n",
        2,
        4,
    );
    let edit = &default_only.additional_edits[0];
    assert_eq!((edit.line, edit.column), (1, 10));
    assert_eq!(edit.text, ", { computed }");

    let other_module = complete(
        "Other.vue",
        "<script setup>\nimport { x } from 'other'\ncomp\n</script>\n",
        2,
        4,
    );
    let edit = &other_module.additional_edits[0];
    assert_eq!((edit.line, edit.column), (2, 0));
    assert_eq!(edit.text, "import { computed } from 'vue';\n");

    let type_only = complete(
        "Type.vue",
        "<script setup lang=\"ts\">\nimport type { Ref } from 'vue'\ncomp\n</script>\n",
        2,
        4,
    );
    assert_eq!(
        type_only.additional_edits[0].text,
        "import { computed } from 'vue';\n"
    );
}

#[test]
fn typescript_workspace_completion_handles_default_and_suppresses_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/models")).unwrap();
    std::fs::create_dir_all(dir.path().join("app/controllers")).unwrap();
    std::fs::write(
        dir.path().join("app/models/session.ts"),
        "export default class Session {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/models/one.ts"),
        "export class Duplicate {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/models/two.ts"),
        "export class Duplicate {}\n",
    )
    .unwrap();
    let consumer = dir.path().join("app/controllers/auth.ts");
    std::fs::write(
        &consumer,
        "import { Session } from '../models/session.js';\n\nSe\n",
    )
    .unwrap();
    let core = Core::new();
    core.open_workspace(dir.path().to_string_lossy().into_owned())
        .unwrap();
    let imported_document = core
        .open_document(consumer.to_string_lossy().into_owned())
        .unwrap();

    let default_consumer = dir.path().join("app/controllers/default.ts");
    std::fs::write(&default_consumer, "Se\n").unwrap();
    let document = core
        .open_document(default_consumer.to_string_lossy().into_owned())
        .unwrap();
    until("workspace TypeScript default completion", || {
        core.request_completion(document.clone(), 0, 2)
    });
    let item = core
        .completions(document)
        .into_iter()
        .find(|item| item.label == "Session")
        .unwrap();
    assert_eq!(
        item.additional_edits[0].text,
        "import Session from '../models/session.js';\n"
    );

    assert!(core.request_completion(imported_document.clone(), 2, 2));
    let items = core.completions(imported_document);
    let session = items.iter().find(|item| item.label == "Session").unwrap();
    assert!(session.additional_edits.is_empty());
    assert!(!items.iter().any(|item| item.label == "Duplicate"));
}

#[test]
fn typescript_workspace_completion_offers_existing_default_import_without_edit() {
    let dir = tempfile::tempdir().unwrap();
    let models = dir.path().join("intrade/app/models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(models.join("user.ts"), "export default class User {}\n").unwrap();
    let consumer = models.join("market.ts");
    std::fs::write(&consumer, "import User from './user.js';\n\nUser\n").unwrap();
    let core = Core::new();
    core.open_workspace(dir.path().to_string_lossy().into_owned())
        .unwrap();
    let document = core
        .open_document(consumer.to_string_lossy().into_owned())
        .unwrap();

    until("existing TypeScript import completion", || {
        core.request_completion(document.clone(), 2, 4)
    });
    let item = core
        .completions(document)
        .into_iter()
        .find(|item| item.label == "User")
        .expect("User completion");
    assert!(item.additional_edits.is_empty());
}

#[test]
fn typescript_workspace_completion_offers_only_local_import_alias() {
    let dir = tempfile::tempdir().unwrap();
    let models = dir.path().join("app/models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(models.join("user.ts"), "export class User {}\n").unwrap();
    let consumer = dir.path().join("app/models/market.ts");
    std::fs::write(
        &consumer,
        "import { User as MarketUser } from './user.js';\n\nMarket\n",
    )
    .unwrap();
    let core = Core::new();
    core.open_workspace(dir.path().to_string_lossy().into_owned())
        .unwrap();
    let document = core
        .open_document(consumer.to_string_lossy().into_owned())
        .unwrap();

    until("aliased TypeScript import completion", || {
        core.request_completion(document.clone(), 2, 6)
    });
    let items = core.completions(document);
    let item = items
        .iter()
        .find(|item| item.label == "MarketUser")
        .expect("MarketUser completion");
    assert!(item.additional_edits.is_empty());
    assert!(!items.iter().any(|item| item.label == "User"));
}

#[test]
fn typescript_workspace_completion_imports_default_class_extending_a_call_expression() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/models")).unwrap();
    std::fs::create_dir_all(dir.path().join("app/controllers")).unwrap();
    std::fs::write(
            dir.path().join("app/models/user.ts"),
            "import { compose } from '@adonisjs/core/helpers';\nimport { BaseModel, column } from '@adonisjs/lucid/orm';\nimport { Trait } from '#mixins/trait';\n\nexport default class User extends compose(\n  BaseModel,\n  Trait\n) {\n  @column({ isPrimary: true })\n  declare id: string\n}\n",
        )
        .unwrap();
    let consumer = dir.path().join("app/controllers/auth.ts");
    std::fs::write(&consumer, "Us\n").unwrap();
    let core = Core::new();
    core.open_workspace(dir.path().to_string_lossy().into_owned())
        .unwrap();
    let document = core
        .open_document(consumer.to_string_lossy().into_owned())
        .unwrap();

    until("workspace TypeScript Adonis model completion", || {
        core.request_completion(document.clone(), 0, 2)
    });
    let item = core
        .completions(document)
        .into_iter()
        .find(|item| item.label == "User")
        .expect("User completion");
    assert_eq!(item.detail.as_deref(), Some("class - app/models/user.ts"));
    assert_eq!(item.insert_text, "User");
    assert_eq!(item.additional_edits.len(), 1);
    assert_eq!(
        item.additional_edits[0].text,
        "import User from '../models/user.js';\n"
    );
}

#[test]
fn typescript_definition_follows_migration_table_callback_parameter() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("database/migrations/create_roles_table.ts");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
            &path,
            "export default class {\n  async up() {\n    this.schema.createTable(this.tableName, (table) => {\n      table.uuid('id').primary()\n    })\n  }\n}\n",
        )
        .unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(document.clone(), 3, 7));
    let definition = core.definition_locations(document.clone());
    assert_eq!(definition.len(), 1);
    assert_eq!((definition[0].line, definition[0].column), (2, 45));
}

#[test]
fn typescript_definition_follows_migration_schema_builder_method() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    let lucid = dir.path().join("node_modules/@adonisjs/lucid");
    let knex = dir.path().join("node_modules/knex");
    std::fs::create_dir_all(lucid.join("build/src/schema")).unwrap();
    std::fs::create_dir_all(lucid.join("build/src/types")).unwrap();
    std::fs::create_dir_all(knex.join("types")).unwrap();
    std::fs::write(
        lucid.join("package.json"),
        r#"{"exports":{"./schema":"./build/src/schema/main.js"}}"#,
    )
    .unwrap();
    std::fs::write(
        knex.join("package.json"),
        r#"{"main":"knex.js","types":"types/index.d.ts"}"#,
    )
    .unwrap();
    std::fs::create_dir_all(knex.join("lib")).unwrap();
    std::fs::write(
        knex.join("knex.js"),
        "module.exports = require('./lib/index');\n",
    )
    .unwrap();
    std::fs::write(knex.join("lib/index.js"), "module.exports = {};\n").unwrap();
    std::fs::write(
            lucid.join("build/src/schema/main.d.ts"),
            "import type { Knex } from 'knex';\nimport type { QueryClientContract } from '../types/database.js';\nimport type { RawQueryBindings } from '../types/querybuilder.js';\nexport declare class BaseSchema { db: QueryClientContract; get schema(): Knex.SchemaBuilder; raw(bindings: RawQueryBindings): Knex.Raw; }\n",
        )
        .unwrap();
    std::fs::write(
        lucid.join("build/src/types/database.d.ts"),
        "export interface QueryClientContract { connectionName: string; }\n",
    )
    .unwrap();
    std::fs::write(
        lucid.join("build/src/types/querybuilder.d.ts"),
        "export type RawQueryBindings = Record<string, string>;\n",
    )
    .unwrap();
    std::fs::write(lucid.join("build/src/schema/main.js"), "").unwrap();
    std::fs::write(
            knex.join("types/index.d.ts"),
            "declare function knex(): Knex.Knex;\ndeclare namespace Knex {\n  export class SchemaBuilder {}\n  interface SchemaBuilder { createTable(name: string, callback: (tableBuilder: CreateTableBuilder) => any): void; }\n  interface CreateTableBuilder extends TableBuilder {}\n  interface TableBuilder { uuid(name: string): ColumnBuilder; string(name: string): ColumnBuilder; bigInteger(name: string): ColumnBuilder; }\n  interface ColumnBuilder { primary(): ColumnBuilder; notNullable(): ColumnBuilder; unique(): ColumnBuilder; unsigned(): ColumnBuilder; }\n}\nexport = knex;\n",
        )
        .unwrap();
    let path = dir.path().join("database/migrations/create_roles_table.ts");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
            &path,
            "import { BaseSchema } from '@adonisjs/lucid/schema'\n\nexport default class extends BaseSchema {\n  protected tableName = 'roles'\n\n  async up() {\n    this.schema.createTable(this.tableName, (table) => {\n      table.uuid('id').primary()\n      table.string('name').notNullable()\n      table.string('code').notNullable().unique()\n\n      table.bigInteger('created_at').unsigned()\n      table.bigInteger('updated_at').unsigned()\n    })\n  }\n}\n",
        )
        .unwrap();
    let core = Core::new();
    let declaration = DocumentIndex::parse(
        lucid.join("build/src/schema/main.d.ts"),
        "import type { Knex } from 'knex';\nimport type { QueryClientContract } from '../types/database.js';\nimport type { RawQueryBindings } from '../types/querybuilder.js';\nexport declare class BaseSchema { db: QueryClientContract; get schema(): Knex.SchemaBuilder; raw(bindings: RawQueryBindings): Knex.Raw; }\n",
    );
    assert!(declaration.exported_declaration("BaseSchema").is_some());
    assert_eq!(
        // `exports` points at emitted JavaScript; the colocated declaration wins.
        resolve_typescript_import_uncached(&path, "@adonisjs/lucid/schema"),
        Some(lucid.join("build/src/schema/main.d.ts"))
    );
    assert_eq!(
        resolve_typescript_import_uncached(&lucid.join("build/src/schema/main.d.ts"), "knex"),
        Some(knex.join("types/index.d.ts"))
    );
    let schema_document = core
        .open_document(
            lucid
                .join("build/src/schema/main.d.ts")
                .to_string_lossy()
                .into_owned(),
        )
        .unwrap();
    assert!(core.request_definition(schema_document.clone(), 0, 14));
    let namespace = core.definition_locations(schema_document.clone());
    assert_eq!(namespace.len(), 1);
    assert_eq!(
        namespace[0].uri,
        file_uri(&std::fs::canonicalize(knex.join("types/index.d.ts")).unwrap())
    );
    assert_eq!((namespace[0].line, namespace[0].column), (1, 18));
    assert!(core.request_definition(schema_document.clone(), 3, 73));
    let schema_builder_namespace = core.definition_locations(schema_document.clone());
    assert_eq!(schema_builder_namespace.len(), 1);
    assert_eq!(schema_builder_namespace[0].uri, namespace[0].uri);
    assert_eq!(
        (
            schema_builder_namespace[0].line,
            schema_builder_namespace[0].column
        ),
        (1, 18)
    );
    assert!(core.request_definition(schema_document.clone(), 3, 78));
    let schema_builder = core.definition_locations(schema_document.clone());
    assert_eq!(schema_builder.len(), 1);
    assert_eq!(
        schema_builder[0].uri,
        file_uri(&std::fs::canonicalize(knex.join("types/index.d.ts")).unwrap())
    );
    assert_eq!((schema_builder[0].line, schema_builder[0].column), (3, 12));
    assert!(core.request_definition(schema_document.clone(), 0, 28));
    let knex_module = core.definition_locations(schema_document.clone());
    assert_eq!(knex_module.len(), 1);
    assert_eq!(
        knex_module[0].uri,
        file_uri(&std::fs::canonicalize(knex.join("types/index.d.ts")).unwrap())
    );
    assert!(core.request_definition(schema_document.clone(), 1, 18));
    let query_client = core.definition_locations(schema_document.clone());
    assert_eq!(query_client.len(), 1);
    assert_eq!(
        query_client[0].uri,
        file_uri(&std::fs::canonicalize(lucid.join("build/src/types/database.d.ts")).unwrap())
    );
    assert_eq!((query_client[0].line, query_client[0].column), (0, 7));
    assert!(core.request_definition(schema_document.clone(), 3, 39));
    let query_client_annotation = core.definition_locations(schema_document.clone());
    assert_eq!(query_client_annotation.len(), 1);
    assert_eq!(query_client_annotation[0].uri, query_client[0].uri);
    assert_eq!(
        (
            query_client_annotation[0].line,
            query_client_annotation[0].column
        ),
        (0, 7)
    );
    assert!(core.request_definition(schema_document.clone(), 2, 18));
    let raw_bindings = core.definition_locations(schema_document.clone());
    assert_eq!(raw_bindings.len(), 1);
    assert_eq!(
        raw_bindings[0].uri,
        file_uri(&std::fs::canonicalize(lucid.join("build/src/types/querybuilder.d.ts")).unwrap())
    );
    assert_eq!((raw_bindings[0].line, raw_bindings[0].column), (0, 7));
    assert!(core.request_definition(schema_document.clone(), 2, 42));
    let querybuilder_module = core.definition_locations(schema_document.clone());
    assert_eq!(querybuilder_module.len(), 1);
    assert_eq!(
        querybuilder_module[0].uri,
        file_uri(&std::fs::canonicalize(lucid.join("build/src/types/querybuilder.d.ts")).unwrap())
    );

    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    assert!(core.request_definition(document.clone(), 0, 10));
    let base = core.definition_locations(document.clone());
    assert_eq!(base.len(), 1);
    assert_eq!(
        base[0].uri,
        file_uri(&std::fs::canonicalize(lucid.join("build/src/schema/main.d.ts")).unwrap())
    );
    assert!(core.request_definition(document.clone(), 6, 9));
    let schema = core.definition_locations(document.clone());
    assert_eq!(schema.len(), 1);
    assert_eq!(
        schema[0].uri,
        file_uri(&std::fs::canonicalize(lucid.join("build/src/schema/main.d.ts")).unwrap())
    );
    let migration_line = "      table.uuid('id').primary()";
    let method_column = |name: &str| {
        migration_line.find(&format!("table.{name}")).unwrap() as u64 + "table.".len() as u64
    };
    assert!(core.request_definition(document.clone(), 7, method_column("uuid")));
    let uuid = core.definition_locations(document.clone());
    assert_eq!(uuid.len(), 1);
    assert_eq!(
        uuid[0].uri,
        file_uri(&std::fs::canonicalize(knex.join("types/index.d.ts")).unwrap())
    );
    assert_eq!((uuid[0].line, uuid[0].column), (5, 27));
    let string_line = "      table.string('name').notNullable()";
    let string_column = string_line.find("table.string").unwrap() as u64 + "table.".len() as u64;
    assert!(core.request_definition(document.clone(), 8, string_column));
    let string = core.definition_locations(document.clone());
    assert_eq!(string.len(), 1);
    assert_eq!((string[0].line, string[0].column), (5, 62));
    let big_integer_line = "      table.bigInteger('created_at').unsigned()";
    let big_integer_column =
        big_integer_line.find("table.bigInteger").unwrap() as u64 + "table.".len() as u64;
    assert!(core.request_definition(document.clone(), 11, big_integer_column));
    let big_integer = core.definition_locations(document.clone());
    assert_eq!(big_integer.len(), 1);
    assert_eq!((big_integer[0].line, big_integer[0].column), (5, 99));
    let chained_line = "      table.string('name').notNullable()";
    let chained_column = |name: &str| chained_line.find(name).unwrap() as u64;
    assert!(core.request_definition(document.clone(), 8, chained_column("notNullable")));
    let not_nullable = core.definition_locations(document.clone());
    assert_eq!(not_nullable.len(), 1);
    assert_eq!((not_nullable[0].line, not_nullable[0].column), (6, 54));
    let unique_line = "      table.string('code').notNullable().unique()";
    assert!(core.request_definition(
        document.clone(),
        9,
        unique_line.find("unique").unwrap() as u64
    ));
    let unique = core.definition_locations(document.clone());
    assert_eq!(unique.len(), 1);
    assert_eq!((unique[0].line, unique[0].column), (6, 84));
    let unsigned_line = "      table.bigInteger('created_at').unsigned()";
    assert!(core.request_definition(
        document.clone(),
        11,
        unsigned_line.find("unsigned").unwrap() as u64
    ));
    let unsigned = core.definition_locations(document);
    assert_eq!(unsigned.len(), 1);
    assert_eq!((unsigned[0].line, unsigned[0].column), (6, 109));
}

#[test]
fn typescript_definition_resolves_regular_declaration_import_with_js_specifier() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir
        .path()
        .join("node_modules/@adonisjs/lucid/build/src/orm/base_model");
    let types = dir
        .path()
        .join("node_modules/@adonisjs/lucid/build/src/types");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::create_dir_all(&types).unwrap();
    let source = "import { LucidModel, QueryScopeCallback, QueryScope } from '../../types/model.js';\nexport declare const BaseModel: LucidModel;\n";
    let index = base.join("index.d.ts");
    std::fs::write(&index, source).unwrap();
    std::fs::write(
            types.join("model.d.ts"),
            "export interface LucidModel { boot(): void }\nexport interface QueryScopeCallback {}\nexport interface QueryScope {}\n",
        )
        .unwrap();
    let target = types.join("model.d.ts");
    assert_eq!(
        resolve_typescript_import_uncached(&index, "../../types/model.js"),
        Some(target.clone())
    );
    let core = Core::new();
    let document = core
        .open_document(index.to_string_lossy().into_owned())
        .unwrap();
    for (line, column) in [
        (0, source.find("LucidModel").unwrap() as u64),
        (1, "export declare const BaseModel: ".len() as u64),
    ] {
        assert!(core.request_definition(document.clone(), line, column));
        let definitions = core.definition_locations(document.clone());
        assert_eq!(definitions.len(), 1);
        assert_eq!(
            definitions[0].uri,
            file_uri(&std::fs::canonicalize(&target).unwrap())
        );
        assert_eq!((definitions[0].line, definitions[0].column), (0, 7));
    }
}

#[test]
fn typescript_definition_follows_named_adonis_alias_export() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/libraries")).unwrap();
    std::fs::create_dir_all(dir.path().join("app/audit_resolvers")).unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    std::fs::write(
            dir.path().join("tsconfig.json"),
            r##"{"extends":"@adonisjs/tsconfig/tsconfig.app.json","compilerOptions":{"paths":{"#libraries/*":["./app/libraries/*.js"]}}}"##,
        )
        .unwrap();
    let definition = dir.path().join("app/libraries/auditing.ts");
    std::fs::write(
        &definition,
        "export interface Resolver { resolve(): void }\n",
    )
    .unwrap();
    let source = "import { Resolver } from '#libraries/auditing'\nexport default class Audit implements Resolver {}\n";
    let importer = dir
        .path()
        .join("app/audit_resolvers/ip_address_resolver.ts");
    std::fs::write(&importer, source).unwrap();
    let core = Core::new();
    let document = core
        .open_document(importer.to_string_lossy().into_owned())
        .unwrap();
    assert!(core.request_definition(document.clone(), 0, 10));
    let definitions = core.definition_locations(document);
    assert_eq!(definitions.len(), 1);
    assert_eq!(
        definitions[0].uri,
        file_uri(&std::fs::canonicalize(definition).unwrap())
    );
    assert_eq!((definitions[0].line, definitions[0].column), (0, 7));
}

#[test]
fn typescript_definition_follows_adonis_on_query_callback() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    let package = dir.path().join("node_modules/@adonisjs/lucid");
    let lucid = package.join("build/src");
    std::fs::create_dir_all(lucid.join("orm/decorators")).unwrap();
    std::fs::create_dir_all(lucid.join("types")).unwrap();
    std::fs::write(
            package.join("package.json"),
            r##"{"exports":{"./orm":"./build/src/orm/decorators/index.js","./types/*":"./build/src/types/*.js"}}"##,
        )
        .unwrap();
    std::fs::write(lucid.join("orm/decorators/index.js"), "").unwrap();
    std::fs::write(lucid.join("types/querybuilder.js"), "").unwrap();
    std::fs::write(
        lucid.join("orm/decorators/index.d.ts"),
        "export declare const hasMany: unknown;\n",
    )
    .unwrap();
    std::fs::write(
        lucid.join("types/querybuilder.d.ts"),
        "export interface ChainableContract { where: unknown; orWhere: unknown; }\n",
    )
    .unwrap();
    let path = dir.path().join("app/models/user.ts");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let source = "import { hasMany } from '@adonisjs/lucid/orm'\n\nclass User {\n  @hasMany(() => Position, {\n    onQuery(query) {\n      query.where((subQuery) => {\n        subQuery.where('status', 'hold')\n        subQuery.orWhere('status', 'waiting')\n      })\n      query.where('market_type', 'future')\n    },\n  })\n  declare positions: Position[]\n}\nclass Position {}\n";
    std::fs::write(&path, source).unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    for (line, column, expected) in [
        (5, 12, "where"),
        (6, 17, "where"),
        (7, 17, "orWhere"),
        (9, 12, "where"),
    ] {
        assert!(
            core.request_definition(document.clone(), line, column),
            "{expected}"
        );
        let definitions = core.definition_locations(document.clone());
        assert_eq!(definitions.len(), 1);
        assert!(definitions[0].uri.ends_with("types/querybuilder.d.ts"));
        assert_eq!(definitions[0].line, 0);
    }
}

#[test]
fn typescript_definition_follows_adonis_model_query_chain() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    let package = dir.path().join("node_modules/@adonisjs/lucid");
    let orm = package.join("build/src/orm");
    let types = package.join("build/src/types");
    std::fs::create_dir_all(&orm).unwrap();
    std::fs::create_dir_all(&types).unwrap();
    std::fs::write(
            package.join("package.json"),
            r##"{"exports":{"./orm":"./build/src/orm/index.js","./types/*":"./build/src/types/*.js"}}"##,
        )
        .unwrap();
    std::fs::write(orm.join("index.js"), "").unwrap();
    std::fs::write(
        orm.join("index.d.ts"),
        "export declare const BaseModel: import('../../types/model.js').LucidModel;\n",
    )
    .unwrap();
    std::fs::write(
        types.join("model.d.ts"),
        "export interface LucidModel { query(): import('./querybuilder.js').ChainableContract }\n",
    )
    .unwrap();
    std::fs::write(
            types.join("querybuilder.d.ts"),
            "export interface ChainableContract { where(column: string, value: unknown): ChainableContract }\n",
        )
        .unwrap();
    let path = dir.path().join("app/auditing.ts");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let source = "import { BaseModel } from '@adonisjs/lucid/orm'\nclass Audit extends BaseModel {}\nfunction audits() { return Audit.query().where('auditableType', 'audit') }\n";
    std::fs::write(&path, source).unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    for (column, suffix) in [(33, "types/model.d.ts"), (41, "types/querybuilder.d.ts")] {
        assert!(core.request_definition(document.clone(), 2, column));
        let definitions = core.definition_locations(document.clone());
        assert_eq!(definitions.len(), 1);
        assert!(definitions[0].uri.ends_with(suffix));
        assert_eq!(definitions[0].line, 0);
    }
}

#[test]
fn typescript_adonis_on_query_fails_closed_outside_lucid_relations() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    let package = dir.path().join("node_modules/other");
    std::fs::create_dir_all(package.join("types")).unwrap();
    std::fs::write(
        package.join("package.json"),
        r#"{"exports":{".":"./index.js"}}"#,
    )
    .unwrap();
    std::fs::write(package.join("index.js"), "").unwrap();
    std::fs::write(
        package.join("index.d.ts"),
        "export declare const hasMany: unknown;\n",
    )
    .unwrap();
    let path = dir.path().join("app/user.ts");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    // Same names, but the decorator is not imported from Lucid; and an
    // `onQuery` option on a plain call.
    let source = "import { hasMany } from 'other'\nfunction run(opts: unknown) {}\nclass User {\n  @hasMany(() => User, {\n    onQuery(query) {\n      query.where('a', 1)\n    },\n  })\n  declare rows: User[]\n}\nrun({ onQuery(query) { query.where('a', 1) } })\n";
    std::fs::write(&path, source).unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    for (line, column) in [(5, 12), (10, 28)] {
        core.request_definition(document.clone(), line, column);
        assert!(core.definition_locations(document.clone()).is_empty());
    }
}

#[test]
fn typescript_definition_resolves_enum_member_through_package_import_alias() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/enums")).unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r##"{"imports":{"#enums/*":"./app/enums/*.js"}}"##,
    )
    .unwrap();
    std::fs::write(
            dir.path().join("app/enums/status.ts"),
            "export enum Status {\n  NEW = 'new',\n  HOLD = 'hold',\n  DONE,\n}\nexport const Local = { HOLD: 1 }\n",
        )
        .unwrap();
    let path = dir.path().join("app/use.ts");
    let source = "import { Status, Local } from '#enums/status'\nenum Own { A }\nStatus.HOLD\nStatus.DONE\nStatus.MISSING\nLocal.HOLD\nOwn.A\nfunction f(Status: { HOLD: number }) { return Status.HOLD }\n";
    std::fs::write(&path, source).unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    // (line, column, expected file suffix, expected target line)
    for (line, column, suffix, target) in [
        (2, 8, "enums/status.ts", 2),
        (3, 8, "enums/status.ts", 3),
        (6, 4, "app/use.ts", 1),
    ] {
        assert!(
            core.request_definition(document.clone(), line, column),
            "{line}:{column}"
        );
        let definitions = core.definition_locations(document.clone());
        assert_eq!(definitions.len(), 1);
        assert!(
            definitions[0].uri.ends_with(suffix),
            "{line}:{column} -> {} @{}:{}",
            definitions[0].uri,
            definitions[0].line,
            definitions[0].column
        );
        assert_eq!(definitions[0].line, target, "{line}:{column}");
    }
    // Missing member, non-enum export, and a shadowing parameter stay unresolved.
    for (line, column) in [(4, 10), (5, 8), (7, 54)] {
        core.request_definition(document.clone(), line, column);
        let definitions = core.definition_locations(document.clone());
        assert!(
            definitions
                .iter()
                .all(|item| !item.uri.ends_with("enums/status.ts")),
            "{line}:{column}"
        );
    }
}

#[test]
fn typescript_import_diagnostics_use_project_resolution() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("tsconfig.json"),
        r#"{"compilerOptions":{"baseUrl":".","paths":{"@lib/*":["src/*"]}}}"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/ok.ts"), "export const ok = 1;\n").unwrap();
    let path = dir.path().join("main.ts");
    std::fs::write(
            &path,
            "import { ok } from '@lib/ok';\nimport pkg from 'pkg';\nimport type { Type } from 'missing-types';\n",
        )
        .unwrap();
    let package = dir.path().join("node_modules/pkg");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("package.json"), r#"{"exports":"./index.ts"}"#).unwrap();
    std::fs::write(package.join("index.ts"), "export default 1;\n").unwrap();

    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    assert!(core.register_document(document.clone(), "typescript".into(), String::new()));
    core.poll_analysis();
    assert!(
        document.lock().diagnostics.is_empty(),
        "{:?}",
        document.lock().diagnostics
    );
    let uri = file_uri(&path);
    let first_revision = document.revision();
    core.poll_analysis();
    assert_eq!(
        core.typescript_checked.lock().unwrap().get(&uri),
        Some(&first_revision),
        "unchanged TypeScript buffers must not be reparsed by every analysis poll"
    );

    assert!(document.apply_rename_edits(vec![RenameEdit {
        uri: String::new(),
        start_line: 1,
        start_character: 17,
        end_line: 1,
        end_character: 20,
        // Bare package names are never reported; a relative path is.
        new_text: "./missing-pkg".into(),
    }]));
    core.poll_analysis();
    assert_eq!(
        core.typescript_checked.lock().unwrap().get(&uri),
        Some(&document.revision()),
        "an edit must invalidate the cached TypeScript analysis"
    );
    let diagnostics = document.lock().diagnostics.clone();
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].severity, 2);
    assert!(
        diagnostics[0].message.contains("missing-pkg"),
        "{diagnostics:?}"
    );
}

#[test]
fn typescript_builtin_utility_types_do_not_suggest_workspace_imports() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("shared")).unwrap();
    std::fs::write(
        dir.path().join("shared/record.ts"),
        "export const Record = 1;\n",
    )
    .unwrap();
    let path = dir.path().join("main.ts");
    std::fs::write(&path, "type Settings = Record<string, string>;\n").unwrap();

    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    assert!(core.register_document(document.clone(), "typescript".into(), String::new()));
    core.poll_analysis();

    assert!(
        document.lock().diagnostics.is_empty(),
        "{:?}",
        document.lock().diagnostics
    );
}

#[test]
fn typescript_local_navigation_is_shadow_aware_and_renames_stable_occurrences() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shadow.ts");
    std::fs::write(
        &path,
        "const emoji = '😀';\nconst value = 1;\n{ const value = 2; value; }\nvalue;\n",
    )
    .unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(document.clone(), 2, 19));
    assert_eq!(
        core.definition_locations(document.clone())
            .into_iter()
            .map(|location| (location.line, location.column))
            .collect::<Vec<_>>(),
        vec![(2, 8)]
    );

    assert!(core.request_references(document.clone(), 2, 19));
    assert_eq!(
        core.references(document.clone())
            .into_iter()
            .map(|location| (location.line, location.column))
            .collect::<Vec<_>>(),
        vec![(2, 8), (2, 19)]
    );

    assert!(core.request_rename(document.clone(), 2, 19, "innerValue".into()));
    let edits = core.rename_edits(document.clone());
    assert_eq!(edits.len(), 2);
    assert!(edits.iter().all(|edit| edit.new_text == "innerValue"));
    assert_eq!(
        edits
            .iter()
            .map(|edit| (edit.start_line, edit.start_character, edit.end_character))
            .collect::<Vec<_>>(),
        vec![(2, 8, 13), (2, 19, 24)]
    );
    assert!(!core.request_rename(document.clone(), 2, 19, "1bad".into()));
    assert!(core.rename_edits(document).is_empty());
}

#[test]
fn typescript_navigation_resolves_direct_imports_through_projects_only() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("tsconfig.json"),
        r#"{"compilerOptions":{"baseUrl":".","paths":{"@lib/*":["src/*"]}}}"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/dep.ts"),
        "const local = 1;\nexport { local as publicValue };\n",
    )
    .unwrap();
    let main_path = dir.path().join("main.ts");
    std::fs::write(
        &main_path,
        "import { publicValue as value } from '@lib/dep';\nvalue;\n",
    )
    .unwrap();
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(main.clone(), 1, 1));
    let definition = core.definition_locations(main.clone());
    assert_eq!(definition.len(), 1);
    assert!(definition[0].uri.ends_with("src/dep.ts"));
    assert_eq!((definition[0].line, definition[0].column), (0, 6));

    assert!(core.request_references(main.clone(), 1, 1));
    assert_eq!(core.references(main.clone()).len(), 3);

    std::fs::write(dir.path().join("src/unsafe.ts"), "export * from './dep';\n").unwrap();
    let unsafe_path = dir.path().join("unsafe-main.ts");
    std::fs::write(
        &unsafe_path,
        "import { publicValue } from '@lib/unsafe';\npublicValue;\n",
    )
    .unwrap();
    let unsafe_document = core
        .open_document(unsafe_path.to_string_lossy().into_owned())
        .unwrap();
    assert!(core.request_definition(unsafe_document.clone(), 1, 1));
    assert!(
        core.definition_locations(unsafe_document)[0]
            .uri
            .ends_with("src/dep.ts")
    );
}

#[test]
fn typescript_references_include_every_workspace_importer() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    let src = dir.path().join("packages/cli/src");
    let handlers = src.join("commands/handlers");
    std::fs::create_dir_all(&handlers).unwrap();
    let commands = src.join("commands/commands.ts");
    std::fs::write(&commands, "export const Commands = {};\nCommands;\n").unwrap();
    std::fs::write(
        handlers.join("api.ts"),
        "import { Commands } from '../commands';\nCommands;\nCommands;\n",
    )
    .unwrap();
    std::fs::write(
        handlers.join("serve.ts"),
        "import { Commands as CliCommands } from '../commands';\nCliCommands;\n",
    )
    .unwrap();
    std::fs::write(
        src.join("index.ts"),
        "import { Commands } from './commands/commands';\nCommands;\n",
    )
    .unwrap();
    let core = Core::new();
    let document = core
        .open_document(commands.to_string_lossy().into_owned())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let references = loop {
        core.request_references(document.clone(), 0, 14);
        let references = core.references(document.clone());
        if references
            .iter()
            .any(|location| location.uri.ends_with("api.ts"))
            || Instant::now() >= deadline
        {
            break references;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(references.len(), 9);
    assert_eq!(
        references
            .iter()
            .filter(|location| location.uri.ends_with("api.ts"))
            .count(),
        3
    );
    assert_eq!(
        references
            .iter()
            .filter(|location| location.uri.ends_with("serve.ts"))
            .count(),
        2
    );
}

#[test]
fn typescript_references_from_exported_function_declaration_include_importers() {
    // A function or class declaration range starts at the keyword, not at the
    // name, so the export must be matched by containment.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let host = src.join("mini-host.ts");
    std::fs::write(
        &host,
        "export function createMiniHost(input: { a: string }) {\n  return input\n}\n",
    )
    .unwrap();
    std::fs::write(
        src.join("mini.ts"),
        "import { createMiniHost } from './mini-host';\ncreateMiniHost({ a: '' });\n",
    )
    .unwrap();
    let core = Core::new();
    let document = core
        .open_document(host.to_string_lossy().into_owned())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let references = loop {
        core.request_references(document.clone(), 0, 20);
        let references = core.references(document.clone());
        if references
            .iter()
            .any(|location| location.uri.ends_with("mini.ts"))
            || Instant::now() >= deadline
        {
            break references;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(
        references
            .iter()
            .filter(|location| location.uri.ends_with("/mini.ts"))
            .count(),
        2,
        "{references:?}"
    );
}

#[test]
fn typescript_navigation_resolves_package_exports_and_type_only_fallbacks() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    let package = dir.path().join("node_modules/pkg");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("package.json"), r#"{"exports":"./entry.ts"}"#).unwrap();
    std::fs::write(
        package.join("entry.ts"),
        "export default function run() {}\n",
    )
    .unwrap();
    let main_path = dir.path().join("main.ts");
    std::fs::write(&main_path, "import run from 'pkg';\nrun();\n").unwrap();
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();
    assert!(core.request_definition(main.clone(), 1, 1));
    assert!(
        core.definition_locations(main.clone())[0]
            .uri
            .ends_with("entry.ts")
    );

    let unsupported_path = dir.path().join("unsupported.ts");
    std::fs::write(
        &unsupported_path,
        "import * as api from 'pkg';\nimport type Run from 'pkg';\napi.run();\n",
    )
    .unwrap();
    let unsupported = core
        .open_document(unsupported_path.to_string_lossy().into_owned())
        .unwrap();
    assert!(!core.request_definition(unsupported.clone(), 0, 13));
    assert!(core.request_definition(unsupported, 1, 13));
}

#[test]
fn typescript_navigation_resolves_static_module_strings_and_namespace_members() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}").unwrap();
    std::fs::write(
        dir.path().join("dep.ts"),
        "export const value = 1;\nexport const Component = () => null;\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("reexport.ts"),
        "export { value } from './dep.js';\n",
    )
    .unwrap();
    let main_path = dir.path().join("main.tsx");
    std::fs::write(
            &main_path,
            "import * as api from './dep.js';\nimport './dep.js';\nexport { value } from './reexport.js';\nexport * from './dep.js';\napi.value;\nconst View = <api.Component />;\napi.value.deep;\nconst value = 1; ({ value });\n",
        )
        .unwrap();
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();

    for (line, column) in [(0, 25), (1, 9), (2, 28), (3, 17)] {
        assert!(
            core.request_definition(main.clone(), line, column),
            "cursor {line}:{column}"
        );
        let locations = core.definition_locations(main.clone());
        assert_eq!(locations.len(), 1);
        assert!(
            locations[0]
                .uri
                .ends_with(if line == 2 { "reexport.ts" } else { "dep.ts" })
        );
        assert_eq!((locations[0].line, locations[0].column), (0, 0));
    }

    assert!(core.request_definition(main.clone(), 3, 10));
    let reexport_from = core.definition_locations(main.clone());
    assert_eq!(reexport_from.len(), 1);
    assert!(reexport_from[0].uri.ends_with("dep.ts"));

    assert!(core.request_definition(main.clone(), 4, 5));
    let value = core.definition_locations(main.clone());
    assert!(value[0].uri.ends_with("dep.ts"));
    assert_eq!((value[0].line, value[0].column), (0, 13));

    assert!(core.request_definition(main.clone(), 5, 21));
    let component = core.definition_locations(main.clone());
    assert!(component[0].uri.ends_with("dep.ts"));
    assert_eq!((component[0].line, component[0].column), (1, 13));

    assert!(!core.request_definition(main.clone(), 6, 5));
    assert!(!core.request_definition(main, 7, 20));
}

#[test]
fn typescript_definition_resolves_static_class_methods() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}").unwrap();
    std::fs::write(
            dir.path().join("cdn-helper.ts"),
            "export class CdnHelper {\n  static generatedUrlsIndexed(value: unknown) { return value; }\n}\n",
        )
        .unwrap();
    let main_path = dir.path().join("main.ts");
    std::fs::write(
        &main_path,
        "import { CdnHelper } from './cdn-helper.js';\nCdnHelper.generatedUrlsIndexed([]);\n",
    )
    .unwrap();
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(main.clone(), 1, 15));
    let locations = core.definition_locations(main);
    assert_eq!(locations.len(), 1);
    assert!(locations[0].uri.ends_with("cdn-helper.ts"));
    assert_eq!((locations[0].line, locations[0].column), (1, 9));
}

#[test]
fn typescript_definition_resolves_static_class_methods_from_package_declarations() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    let package = dir.path().join("node_modules/@adonisjs/core/build/src");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        dir.path().join("node_modules/@adonisjs/core/package.json"),
        r##"{"exports":{"./env":"./build/src/env.js"}}"##,
    )
    .unwrap();
    std::fs::write(
            package.join("env.d.ts"),
            "export declare class Env {\n  static create(value: URL): Promise<Env>;\n  get(name: string): string;\n}\n",
        )
        .unwrap();
    std::fs::write(package.join("env.js"), "export {};\n").unwrap();
    let main_path = dir.path().join("main.ts");
    std::fs::write(
            &main_path,
            "import { Env } from '@adonisjs/core/env';\nexport default await Env.create(new URL('file:///'));\n",
        )
        .unwrap();
    let project = javascript::TypeScriptProject::detect(&main_path).unwrap();
    assert_eq!(
        javascript::resolve_typescript_specifier(&project, "@adonisjs/core/env", &main_path),
        Some(package.join("env.d.ts"))
    );
    let source = std::fs::read_to_string(&main_path).unwrap();
    assert!(
        DocumentIndex::parse(&main_path, &source)
            .type_member_at(1, 28)
            .is_some()
    );
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(main.clone(), 1, 28));
    let locations = core.definition_locations(main);
    assert_eq!(locations.len(), 1);
    assert!(
        locations[0]
            .uri
            .ends_with("@adonisjs/core/build/src/env.d.ts")
    );
    assert_eq!((locations[0].line, locations[0].column), (1, 9));
}

#[test]
fn typescript_definition_resolves_local_static_class_methods() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.ts");
    std::fs::write(
            &path,
            "class CdnHelper {\n  static generatedUrlsIndexed(value: unknown) { return value; }\n}\nCdnHelper.generatedUrlsIndexed([]);\n",
        )
        .unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(document.clone(), 3, 15));
    let locations = core.definition_locations(document);
    assert_eq!(locations.len(), 1);
    assert_eq!((locations[0].line, locations[0].column), (1, 9));
}

#[test]
fn typescript_definition_resolves_cross_file_export_despite_target_diagnostic() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("cdn.ts");
    std::fs::write(
            &target,
            "export class CdnHelper {}\nexport function generateUrl(path: string) { return path; }\nconst unfinished = (\n",
        )
        .unwrap();
    let main_path = dir.path().join("main.ts");
    std::fs::write(
        &main_path,
        "import { CdnHelper, generateUrl } from './cdn.js';\nCdnHelper;\ngenerateUrl('file');\n",
    )
    .unwrap();
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();

    for (line, column, target_line, target_column) in [(1, 1, 0, 7), (2, 1, 1, 7)] {
        assert!(core.request_definition(main.clone(), line, column));
        let locations = core.definition_locations(main.clone());
        assert_eq!(locations.len(), 1);
        assert!(locations[0].uri.ends_with("cdn.ts"));
        assert_eq!(
            (locations[0].line, locations[0].column),
            (target_line, target_column)
        );
    }
}

#[test]
fn typescript_definition_resolves_default_factory_instance_methods() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("start")).unwrap();
    let package = dir.path().join("node_modules/@adonisjs/core/build/src");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        dir.path().join("tsconfig.json"),
        r##"{"compilerOptions":{"paths":{"#start/*":["./start/*.ts"]}}}"##,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("node_modules/@adonisjs/core/package.json"),
        r##"{"exports":{"./env":"./build/src/env.js"}}"##,
    )
    .unwrap();
    std::fs::write(
            package.join("env.d.ts"),
            "export declare class Env {\n  static create(value?: URL): Promise<Env>;\n  get(name: string): string;\n}\n",
        )
        .unwrap();
    std::fs::write(package.join("env.js"), "export {};\n").unwrap();
    std::fs::write(
        dir.path().join("start/env.ts"),
        "import { Env } from '@adonisjs/core/env';\nexport default await Env.create();\n",
    )
    .unwrap();
    let main_path = dir.path().join("main.ts");
    std::fs::write(
        &main_path,
        "import env from '#start/env';\nenv.get('CDN_URL');\n",
    )
    .unwrap();
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(main.clone(), 1, 5));
    let locations = core.definition_locations(main);
    assert_eq!(locations.len(), 1);
    assert!(
        locations[0]
            .uri
            .ends_with("@adonisjs/core/build/src/env.d.ts")
    );
    assert_eq!((locations[0].line, locations[0].column), (2, 2));
}

#[test]
fn typescript_definition_resolves_default_factory_instance_methods_through_reexport() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("start")).unwrap();
    std::fs::write(
        dir.path().join("tsconfig.json"),
        r##"{"compilerOptions":{"paths":{"#start/*":["./start/*.ts"]}}}"##,
    )
    .unwrap();
    std::fs::write(
            dir.path().join("env-contract.ts"),
            "export class Env {\n  static async create() { return new Env(); }\n  get(name: string) { return name; }\n}\n",
        )
        .unwrap();
    std::fs::write(
        dir.path().join("start/env-instance.ts"),
        "import { Env } from '../env-contract.js';\nexport default await Env.create();\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("start/env.ts"),
        "export { default } from './env-instance.js';\n",
    )
    .unwrap();
    let main_path = dir.path().join("main.ts");
    std::fs::write(
        &main_path,
        "import env from '#start/env';\nenv.get('CDN_URL');\n",
    )
    .unwrap();
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(main.clone(), 1, 5));
    let locations = core.definition_locations(main);
    assert_eq!(locations.len(), 1);
    assert!(locations[0].uri.ends_with("env-contract.ts"));
    assert_eq!((locations[0].line, locations[0].column), (2, 2));
}

#[test]
fn typescript_and_javascript_definitions_resolve_ecmascript_globals_and_methods() {
    let dir = tempfile::tempdir().unwrap();
    for extension in ["ts", "js"] {
        let path = dir.path().join(format!("main.{extension}"));
        std::fs::write(
            &path,
            "Object.keys({ value: 1 });\nJSON.stringify({ value: 1 });\n",
        )
        .unwrap();
        let core = Core::new();
        let document = core
            .open_document(path.to_string_lossy().into_owned())
            .unwrap();
        for (line, column, declaration) in [
            (0, 1, "Object"),
            (0, 8, "keys"),
            (1, 1, "JSON"),
            (1, 6, "stringify"),
        ] {
            assert!(
                core.request_definition(document.clone(), line, column),
                "{extension} cursor {line}:{column}"
            );
            let locations = core.definition_locations(document.clone());
            assert_eq!(locations.len(), 1);
            assert!(locations[0].uri.ends_with("ecmascript.d.ts"));
            assert_eq!(
                (locations[0].line, locations[0].column),
                ecmascript_declaration_position(declaration),
                "{extension} {declaration}"
            );
        }
    }
}

#[test]
fn typescript_definitions_resolve_each_ecmascript_global_declaration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.ts");
    let names = [
        "Object", "Array", "String", "Number", "Boolean", "Date", "Math", "Promise", "Map", "Set",
        "WeakMap", "WeakSet", "Error", "RegExp", "JSON",
    ];
    std::fs::write(
        &path,
        names
            .iter()
            .map(|name| format!("{name};"))
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    for (line, name) in names.iter().enumerate() {
        assert!(
            core.request_definition(document.clone(), line as u64, 1),
            "{name}"
        );
        let locations = core.definition_locations(document.clone());
        assert_eq!(locations.len(), 1, "{name}");
        assert!(locations[0].uri.ends_with("ecmascript.d.ts"), "{name}");
        assert_eq!(
            (locations[0].line, locations[0].column),
            ecmascript_declaration_position(name),
            "{name}"
        );
    }
}

#[test]
fn typescript_definition_resolves_instance_methods_on_primitive_parameters() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.ts");
    std::fs::write(
            &path,
            "class CdnHelper {\n  private static readonly CDN_URL = 'https://example.test';\n  static generateUrl(filePath: string): string {\n    const cleanPath = filePath.startsWith('/') ? filePath.slice(1) : filePath;\n    return `${this.CDN_URL}/${cleanPath}`;\n  }\n}\n",
        )
        .unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(document.clone(), 3, 31));
    let locations = core.definition_locations(document.clone());
    assert_eq!(locations.len(), 1);
    assert!(locations[0].uri.ends_with("ecmascript.d.ts"));
    assert_eq!(
        (locations[0].line, locations[0].column),
        ecmascript_declaration_position("startsWith")
    );

    assert!(core.request_definition(document.clone(), 3, 58));
    let locations = core.definition_locations(document.clone());
    assert_eq!(locations.len(), 1);
    assert!(locations[0].uri.ends_with("ecmascript.d.ts"));
    assert_eq!(
        (locations[0].line, locations[0].column),
        ecmascript_declaration_position("slice")
    );

    assert!(core.request_definition(document.clone(), 4, 20));
    let locations = core.definition_locations(document);
    assert_eq!(locations.len(), 1);
    assert_eq!((locations[0].line, locations[0].column), (1, 26));
}

#[test]
fn typescript_definition_resolves_array_methods_on_array_parameters() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.ts");
    std::fs::write(
        &path,
        "function urls(filePaths: string[]) { return filePaths.map(path => path); }\n",
    )
    .unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(document.clone(), 0, 55));
    let locations = core.definition_locations(document);
    assert_eq!(locations.len(), 1);
    assert!(locations[0].uri.ends_with("ecmascript.d.ts"));
    assert_eq!(
        (locations[0].line, locations[0].column),
        ecmascript_declaration_position("map")
    );
}

#[test]
fn ecmascript_receiver_recognizes_array_parameter_shorthand() {
    assert!(ecmascript_builtin_receiver(
        "function urls(filePaths: string[]) { return filePaths.map(path => path); }",
        "return filePaths.map(path => path);",
        "filePaths"
    ));
}

#[test]
fn typescript_definition_resolves_static_field_in_template_interpolation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cdn_helper.ts");
    std::fs::write(
            &path,
            "export class CdnHelper {\n  private static readonly CDN_URL = env.get('CDN_URL', 'https://cdn.test')\n\n  static generateUrl(filePath: string): string {\n    const cleanPath = filePath.startsWith('/') ? filePath.slice(1) : filePath\n    return `${this.CDN_URL}/${cleanPath}`\n  }\n}\n",
        )
        .unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(document.clone(), 5, 19));
    let locations = core.definition_locations(document);
    assert_eq!(locations.len(), 1);
    assert_eq!((locations[0].line, locations[0].column), (1, 26));
}

#[test]
fn typescript_definition_resolves_primitive_type_keywords() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.ts");
    std::fs::write(
        &path,
        "const title: string = 'Galaxy';\nconst count: number = 1;\nconst ready: boolean = true;\n",
    )
    .unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    for (line, column) in [(0, 14), (1, 14), (2, 14)] {
        assert!(
            core.request_definition(document.clone(), line, column),
            "cursor {line}:{column}"
        );
        let locations = core.definition_locations(document.clone());
        assert_eq!(locations.len(), 1);
        assert!(locations[0].uri.ends_with("ecmascript.d.ts"));
    }
}

#[test]
fn typescript_definitions_resolve_builtin_utility_types_and_classes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.ts");
    std::fs::write(
        &path,
        "type User = Record<string, Pick<{ id: string; name: string }, 'id'>>;\nMap.get;\n",
    )
    .unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    for (line, column) in [(0, 13), (0, 28), (1, 1), (1, 5)] {
        assert!(
            core.request_definition(document.clone(), line, column),
            "cursor {line}:{column}"
        );
        let locations = core.definition_locations(document.clone());
        assert_eq!(locations.len(), 1);
        assert!(locations[0].uri.ends_with("ecmascript.d.ts"));
    }
}

#[test]
fn typescript_navigation_resolves_package_imports_reexports_and_literal_dynamic_imports() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r##"{"imports":{"#alias/*":"./src/*.js"}}"##,
    )
    .unwrap();
    std::fs::write(dir.path().join("src/value.ts"), "export const value = 1;\n").unwrap();
    std::fs::write(
        dir.path().join("src/named.ts"),
        "export { value as named } from '#alias/value';\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/star.ts"),
        "export * from '#alias/named';\n",
    )
    .unwrap();
    let main_path = dir.path().join("main.ts");
    std::fs::write(
        &main_path,
        "import { named } from './src/star.js';\nnamed;\nimport('#alias/value');\nimport(alias);\n",
    )
    .unwrap();
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(main.clone(), 1, 1));
    let definition = core.definition_locations(main.clone());
    assert_eq!(definition.len(), 1);
    assert!(definition[0].uri.ends_with("src/value.ts"));
    assert_eq!((definition[0].line, definition[0].column), (0, 13));

    assert!(core.request_definition(main.clone(), 2, 10));
    assert!(
        core.definition_locations(main.clone())[0]
            .uri
            .ends_with("src/value.ts")
    );
    assert!(!core.request_definition(main, 3, 8));
}

#[test]
fn typescript_navigation_reexport_cycles_and_ambiguous_stars_fall_back_to_module_roots() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}").unwrap();
    std::fs::write(dir.path().join("a.ts"), "export * from './b.js';\n").unwrap();
    std::fs::write(dir.path().join("b.ts"), "export * from './a.js';\n").unwrap();
    std::fs::write(dir.path().join("one.ts"), "export const value = 1;\n").unwrap();
    std::fs::write(dir.path().join("two.ts"), "export const value = 2;\n").unwrap();
    std::fs::write(
        dir.path().join("ambiguous.ts"),
        "export * from './one.js';\nexport * from './two.js';\n",
    )
    .unwrap();
    let main_path = dir.path().join("main.ts");
    std::fs::write(
            &main_path,
            "import { value as cycle } from './a.js';\nimport { value } from './ambiguous.js';\ncycle;\nvalue;\n",
        )
        .unwrap();
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();
    for (line, target) in [(2, "a.ts"), (3, "ambiguous.ts")] {
        assert!(core.request_definition(main.clone(), line, 1));
        let definition = core.definition_locations(main.clone());
        assert!(definition[0].uri.ends_with(target), "{:?}", definition[0]);
        assert_eq!((definition[0].line, definition[0].column), (0, 0));
    }
}

#[test]
fn typescript_navigation_resolves_type_imports_annotations_and_direct_members() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    std::fs::write(
            dir.path().join("types.ts"),
            "export interface User { name: string; greet(): void }\nexport type Record = { title: string };\nexport const Value = 1;\n",
        )
        .unwrap();
    std::fs::write(
        dir.path().join("alias.ts"),
        "export interface User { name: string; greet(): void }\nexport { User as Person };\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("reexport.ts"),
        "export type { Record } from './types';\n",
    )
    .unwrap();
    let main_path = dir.path().join("main.ts");
    std::fs::write(
            &main_path,
            "import type { Person } from './alias';\nimport { type Record } from './reexport';\nimport { Value } from './types';\nconst user: Person = {} as Person;\nfunction use(record: Record) { user.name; user.greet(); record.title; }\nclass Box { user: Person; show() { return this.user.name; } }\nValue;\nconst union: Person | undefined = undefined; union?.name;\nuser['name']; user.name.deep;\n",
        )
        .unwrap();
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();

    for (line, column, target, target_line, target_column) in [
        (0, 14, "alias.ts", 0, 7),
        (1, 14, "types.ts", 1, 7),
        (3, 12, "alias.ts", 0, 7),
        (4, 36, "alias.ts", 0, 24),
        (4, 47, "alias.ts", 0, 38),
        (4, 63, "types.ts", 1, 23),
        (5, 53, "alias.ts", 0, 24),
    ] {
        assert!(
            core.request_definition(main.clone(), line, column),
            "cursor {line}:{column}"
        );
        let definition = core.definition_locations(main.clone());
        assert!(definition[0].uri.ends_with(target));
        assert_eq!(
            (definition[0].line, definition[0].column),
            (target_line, target_column),
            "cursor {line}:{column}"
        );
    }

    assert!(core.request_definition(main.clone(), 6, 1));
    assert!(
        core.definition_locations(main.clone())[0]
            .uri
            .ends_with("types.ts")
    );

    for (line, column) in [(7, 51), (8, 5), (8, 24)] {
        assert!(
            !core.request_definition(main.clone(), line, column),
            "cursor {line}:{column}"
        );
    }
}

#[test]
fn typescript_navigation_resolves_normal_imports_in_type_assertions() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    std::fs::write(
        dir.path().join("lucid_model.d.ts"),
        "export declare class LucidModel { static columnsDefinitions: Map<string, unknown> }\n",
    )
    .unwrap();
    let main_path = dir.path().join("main.ts");
    std::fs::write(
            &main_path,
            "import { LucidModel } from './lucid_model';\nconst model = this.constructor as LucidModel;\n",
        )
        .unwrap();
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(main.clone(), 1, 35));
    let definition = core.definition_locations(main);
    assert_eq!(definition.len(), 1);
    assert!(definition[0].uri.ends_with("lucid_model.d.ts"));
    assert_eq!((definition[0].line, definition[0].column), (0, 21));
}

#[test]
fn typescript_navigation_resolves_imported_decorator_option_keys() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/decorators")).unwrap();
    std::fs::create_dir_all(dir.path().join("app/models")).unwrap();
    std::fs::create_dir_all(dir.path().join("types")).unwrap();
    std::fs::write(
            dir.path().join("tsconfig.json"),
            r##"{"compilerOptions":{"baseUrl":".","paths":{"#decorators/*":["app/decorators/*"],"#types/*":["types/*"]}}}"##,
        )
        .unwrap();
    std::fs::write(
        dir.path().join("types/lucid.ts"),
        "export interface ColumnOptions { columnName?: string }\n\
             export type EpochDecorator = (option?: Partial<ColumnOptions & {\n\
               autoCreate: boolean;\n\
               autoUpdate: boolean;\n\
             }>) => DecoratorEpochFn\n\
             type DecoratorEpochFn = () => void\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/decorators/lucid.ts"),
        "import { EpochDecorator } from '#types/lucid'\n\
             export const epochTime: EpochDecorator = () => () => {}\n\
             export const unrelated = () => () => {}\n",
    )
    .unwrap();
    let model_path = dir.path().join("app/models/foo.ts");
    std::fs::write(
        &model_path,
        "import { epochTime, unrelated } from '#decorators/lucid'\n\
             @epochTime({ autoCreate: true, autoUpdate: true, columnName: 'created_at' })\n\
             class Model {}\n\
             @unrelated({ autoCreate: true })\n\
             class Other {}\n\
             epochTime({ autoCreate: true })\n",
    )
    .unwrap();
    let core = Core::new();
    let model = core
        .open_document(model_path.to_string_lossy().into_owned())
        .unwrap();

    for (column, target_line, target_column) in [(14, 2, 0), (32, 3, 0), (50, 0, 33)] {
        assert!(
            core.request_definition(model.clone(), 1, column),
            "cursor 1:{column}"
        );
        let definition = core.definition_locations(model.clone());
        assert_eq!(definition.len(), 1);
        assert!(definition[0].uri.ends_with("types/lucid.ts"));
        assert_eq!(
            (definition[0].line, definition[0].column),
            (target_line, target_column)
        );
    }

    for (line, column) in [(3, 13), (5, 12)] {
        assert!(
            !core.request_definition(model.clone(), line, column),
            "cursor {line}:{column}"
        );
    }
}

#[test]
fn typescript_navigation_resolves_option_keys_of_imported_function_calls() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    std::fs::create_dir_all(dir.path().join("node_modules/@lib/core/types")).unwrap();
    std::fs::create_dir_all(dir.path().join("node_modules/@lib/app/types")).unwrap();
    std::fs::write(
        dir.path().join("node_modules/@lib/core/package.json"),
        r#"{"types":"types/index.d.ts"}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("node_modules/@lib/core/types/index.d.ts"),
        "export * from './options';\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("node_modules/@lib/core/types/options.d.ts"),
        "interface BaseOptions<R> {\n  resolve: R;\n  title?: string;\n}\n\
         export interface CsrOptions<R> extends BaseOptions<R> {\n  progress?: boolean;\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("node_modules/@lib/app/package.json"),
        r#"{"types":"types/index.d.ts"}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("node_modules/@lib/app/types/index.d.ts"),
        "import { CsrOptions } from '@lib/core';\n\
         type AppOptions<S> = CsrOptions<S>;\n\
         export declare function createApp<S>(options: AppOptions<S>): void;\n\
         export declare function createApp<S>(options: AppOptions<S> & { render: 1 }): void;\n",
    )
    .unwrap();
    let main_path = dir.path().join("main.ts");
    std::fs::write(
        &main_path,
        "import { createApp } from '@lib/app';\n\
         createApp({ resolve: (n) => n, progress: true, other: 1 });\n",
    )
    .unwrap();
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();
    // `resolve` is declared on the base interface two hops away.
    assert!(core.request_definition(main.clone(), 1, 15));
    let definition = core.definition_locations(main.clone());
    assert_eq!(definition.len(), 1);
    assert!(definition[0].uri.ends_with("@lib/core/types/options.d.ts"));
    assert_eq!((definition[0].line, definition[0].column), (1, 2));
    // A key the type does not declare resolves to nothing.
    assert!(!core.request_definition(main, 1, 49));
}

#[test]
fn typescript_navigation_resolves_imported_object_member_decorator_option_keys() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    let package = dir.path().join("node_modules/decorators");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
            package.join("index.d.ts"),
            "export type Options = { autoCreate: boolean; autoUpdate: boolean }\n\
             export type DateColumnDecorator = (option?: Partial<Options>) => () => void\n\
             export type DateTimeDecorator = DateColumnDecorator\n\
             export type BaseDecorator = () => void\n\
             export declare const column: BaseDecorator & { dateTime: DateTimeDecorator }\n\
             export declare const ordinary: { dateTime: (option?: { autoCreate: boolean }) => void }\n",
        )
        .unwrap();
    let model_path = dir.path().join("model.ts");
    std::fs::write(
        &model_path,
        "import { column, ordinary } from 'decorators'\n\
             @column.dateTime({ autoCreate: true, autoUpdate: true })\n\
             class Model {}\n\
             @ordinary.dateTime({ autoCreate: true })\n\
             class Other {}\n",
    )
    .unwrap();
    let core = Core::new();
    let model = core
        .open_document(model_path.to_string_lossy().into_owned())
        .unwrap();

    for (column, target_column) in [(19, 24), (37, 45)] {
        assert!(
            core.request_definition(model.clone(), 1, column),
            "cursor 1:{column}"
        );
        let definition = core.definition_locations(model.clone());
        assert_eq!(definition.len(), 1);
        assert!(
            definition[0]
                .uri
                .ends_with("node_modules/decorators/index.d.ts")
        );
        assert_eq!(
            (definition[0].line, definition[0].column),
            (0, target_column)
        );
    }
    assert!(!core.request_definition(model, 3, 22));
}

#[test]
fn typescript_navigation_resolves_direct_inline_object_annotation_members() {
    let dir = tempfile::tempdir().unwrap();
    let main_path = dir.path().join("main.ts");
    std::fs::write(
        &main_path,
        "const value: { name: string; run(): void } = {} as never;\n\
             value.name; value?.name; value[\"name\"]; value?.[\"name\"];\n\
             function use(parameter: { name: string }) { parameter.name; }\n\
             class Box { field: { name: string }; show() { return this.field.name; } }\n\
             value.name.deep; value[key]; value[`name`];\n",
    )
    .unwrap();
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();

    for (line, column, target_line, target_column) in [
        (1, 6, 0, 15),
        (1, 19, 0, 15),
        (1, 32, 0, 15),
        (1, 48, 0, 15),
        (2, 56, 2, 26),
        (3, 65, 3, 21),
    ] {
        assert!(
            core.request_definition(main.clone(), line, column),
            "cursor {line}:{column}"
        );
        let definition = core.definition_locations(main.clone());
        assert_eq!(definition.len(), 1);
        assert_eq!(
            (definition[0].line, definition[0].column),
            (target_line, target_column),
            "cursor {line}:{column}"
        );
    }

    assert!(core.request_definition(main.clone(), 4, 6));
    let definition = core.definition_locations(main.clone());
    assert_eq!((definition[0].line, definition[0].column), (0, 15));
    for (line, column) in [(4, 11), (4, 24), (4, 35)] {
        assert!(
            !core.request_definition(main.clone(), line, column),
            "cursor {line}:{column}"
        );
    }
}

#[test]
fn typescript_navigation_resolves_imported_superclass_members() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    std::fs::write(
            dir.path().join("base.ts"),
            "export class NamedBase { protected schema = 1; create(): void {} }\nexport default class DefaultBase { public schema() {} }\nexport const NotClass = { schema: 1 };\n",
        )
        .unwrap();
    let main_path = dir.path().join("main.ts");
    std::fs::write(
            &main_path,
            "import { NamedBase } from './base';\nimport DefaultBase from './base';\nimport type { NamedBase as TypeBase } from './base';\nimport { NotClass } from './base';\nclass Named extends NamedBase { use() { this.schema; this?.schema; this[\"schema\"]; this.schema.create; this.unknown; } }\nclass Default extends DefaultBase { use() { this.schema; } }\nclass Typed extends TypeBase { use() { this.schema; } }\nclass Value extends NotClass { use() { this.schema; } }\nexport default class extends NamedBase { use() { this.schema; } }\n",
        )
        .unwrap();
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();

    for (line, column, target_line, target_column) in [
        (4, 45, 0, 35),
        (4, 59, 0, 35),
        (4, 74, 0, 35),
        (5, 51, 1, 42),
        (8, 55, 0, 35),
    ] {
        assert!(
            core.request_definition(main.clone(), line, column),
            "cursor {line}:{column}"
        );
        let definition = core.definition_locations(main.clone());
        assert_eq!(definition.len(), 1);
        assert!(definition[0].uri.ends_with("base.ts"));
        assert_eq!(
            (definition[0].line, definition[0].column),
            (target_line, target_column)
        );
    }
    for (line, column) in [(4, 81), (4, 94), (6, 43), (7, 43)] {
        assert!(
            !core.request_definition(main.clone(), line, column),
            "cursor {line}:{column}"
        );
    }
}

#[test]
fn typescript_definition_resolves_imported_superclass_in_heritage_clause() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    std::fs::write(
        dir.path().join("chainable.d.ts"),
        "export declare abstract class Chainable {}\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("chainable.js"), "export {};\n").unwrap();
    let main_path = dir.path().join("model_query_builder.ts");
    std::fs::write(
            &main_path,
            "import { Chainable } from './chainable.js';\nexport declare class ModelQueryBuilder extends Chainable {}\n",
        )
        .unwrap();
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();

    assert!(
        core.request_definition(main.clone(), 1, 50),
        "no definition for imported superclass"
    );
    let locations = core.definition_locations(main);
    assert_eq!(locations.len(), 1, "{locations:?}");
    assert!(
        locations[0].uri.ends_with("chainable.d.ts"),
        "{locations:?}"
    );
    assert_eq!((locations[0].line, locations[0].column), (0, 30));
}

#[test]
fn typescript_navigation_resolves_local_superclass_methods() {
    let text = "class baseService { setMessage() {} }\nclass Service extends baseService { use() { this.setMessage(); } }\n";
    let (_dir, path, document) = file_doc(text);
    let core = Core::new();
    let handle = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(handle.clone(), 1, 53));
    let definition = core.definition_locations(handle);
    assert_eq!(definition.len(), 1);
    assert_eq!((definition[0].line, definition[0].column), (0, 20));
    drop(document);
}

#[test]
fn typescript_navigation_resolves_fluent_local_superclass_methods() {
    let text = "declare class baseService { protected setCode(code: number): this; protected setMessage(message: string): this; }\nclass Service extends baseService { use() { return this.setCode(200).setMessage('ok'); } }\n";
    let (_dir, path, _document) = file_doc(text);
    let core = Core::new();
    let handle = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(handle.clone(), 1, 70));
    let definition = core.definition_locations(handle);
    assert_eq!(definition.len(), 1);
    assert_eq!((definition[0].line, definition[0].column), (0, 77));
}

#[test]
fn typescript_navigation_resolves_fluent_imported_declaration_methods() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    std::fs::write(
            dir.path().join("base.d.ts"),
            "export declare class BaseService { protected setCode(code: number): this; protected setMessage(message: string): this; }\n",
        )
        .unwrap();
    let main_path = dir.path().join("main.ts");
    std::fs::write(
            &main_path,
            "import { BaseService } from './base';\nclass Service extends BaseService { use() { return this.setCode(200).setMessage('ok'); } }\n",
        )
        .unwrap();
    let core = Core::new();
    let handle = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(handle.clone(), 1, 74));
    let definition = core.definition_locations(handle);
    assert_eq!(definition.len(), 1);
    assert!(definition[0].uri.ends_with("base.d.ts"));
    assert_eq!((definition[0].line, definition[0].column), (0, 84));
}

#[test]
fn typescript_definition_prefers_adjacent_package_declarations() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    let package = dir.path().join("node_modules/@adonisjs/lucid");
    std::fs::create_dir_all(package.join("build/src/schema")).unwrap();
    std::fs::write(
            package.join("package.json"),
            r#"{"exports":{"./schema":"./build/src/schema/main.js","./without-types":"./build/src/schema/without-types.js"}}"#,
        )
        .unwrap();
    std::fs::write(
        package.join("build/src/schema/main.js"),
        "export class BaseSchema { get schema() { return null } }\n",
    )
    .unwrap();
    std::fs::write(
        package.join("build/src/schema/main.d.ts"),
        "export class BaseSchema { schema: string }\n",
    )
    .unwrap();
    std::fs::write(
        package.join("build/src/schema/without-types.js"),
        "export class WithoutTypes {}\n",
    )
    .unwrap();
    let main_path = dir.path().join("main.ts");
    let migration = "export default class extends BaseSchema { up() { this.schema; this.schema; } }\nWithoutTypes;";
    let source = format!(
        "import {{ BaseSchema }} from '@adonisjs/lucid/schema';\nimport {{ WithoutTypes }} from '@adonisjs/lucid/without-types';\n{migration}\n"
    );
    std::fs::write(&main_path, source).unwrap();
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(main.clone(), 0, 30));
    assert!(
        core.definition_locations(main.clone())[0]
            .uri
            .ends_with("build/src/schema/main.d.ts")
    );

    assert!(core.request_definition(main.clone(), 0, 10));
    assert!(
        core.definition_locations(main.clone())[0]
            .uri
            .ends_with("build/src/schema/main.d.ts")
    );

    let first = migration.find("this.schema").unwrap() + "this.".len();
    let second = migration.rfind("this.schema").unwrap() + "this.".len();
    for column in [first, second] {
        assert!(
            core.request_definition(main.clone(), 2, column as u64),
            "cursor 2:{column}"
        );
        let definition = core.definition_locations(main.clone());
        assert_eq!(definition.len(), 1);
        assert!(definition[0].uri.ends_with("build/src/schema/main.d.ts"));
    }
    assert!(core.request_definition(main.clone(), 3, 1));
    assert!(
        core.definition_locations(main.clone())[0]
            .uri
            .ends_with("build/src/schema/without-types.js")
    );
    assert!(!core.request_definition(main, 2, (first + "schema".len()) as u64));
}

#[test]
fn typescript_definition_follows_package_declaration_reexports() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    let adonis = dir
        .path()
        .join("node_modules/@adonisjs/core/build/src/helpers");
    let poppinss = dir.path().join("node_modules/@poppinss/utils/build/src");
    std::fs::create_dir_all(&adonis).unwrap();
    std::fs::create_dir_all(&poppinss).unwrap();
    std::fs::write(
        dir.path().join("node_modules/@adonisjs/core/package.json"),
        r#"{"exports":{"./helpers":"./build/src/helpers/main.js"}}"#,
    )
    .unwrap();
    std::fs::write(adonis.join("main.js"), "").unwrap();
    std::fs::write(
        adonis.join("main.d.ts"),
        "export { compose } from '@poppinss/utils';\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("node_modules/@poppinss/utils/package.json"),
        r#"{"exports":{".":"./build/index.js"}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path()
            .join("node_modules/@poppinss/utils/build/index.js"),
        "",
    )
    .unwrap();
    std::fs::write(
        dir.path()
            .join("node_modules/@poppinss/utils/build/index.d.ts"),
        "export { compose } from './src/compose.js';\n",
    )
    .unwrap();
    std::fs::write(
        poppinss.join("compose.d.ts"),
        "export declare function compose(): void;\n",
    )
    .unwrap();
    let main_path = dir.path().join("app/models/order.ts");
    std::fs::create_dir_all(main_path.parent().unwrap()).unwrap();
    std::fs::write(
            &main_path,
            "import { DateTime } from 'luxon';\n\
             import { BaseModel, beforeCreate, belongsTo, column } from '@adonisjs/lucid/orm';\n\
             import { compose } from '@adonisjs/core/helpers';\n\
             import { withAuthFinder } from '@adonisjs/auth/mixins/lucid';\n\
             import hash from '@adonisjs/core/services/hash';\n\
             import type { BelongsTo } from '@adonisjs/lucid/types/relations';\n\
             import User from '#models/user';\n\
             import Product from '#models/product';\n\
             \n\
             \n\
             \n\
             \n\
             export default class Order extends compose(BaseModel, withAuthFinder(() => hash.use('scrypt'), {\n\
               uids: ['email'],\n\
               passwordColumn: 'password',\n\
             })) {}\n",
        )
        .unwrap();
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();

    let compose_in_import = 10;
    let compose_in_order_class = "export default class Order extends ".len() as u64;
    for (line, column) in [(2, compose_in_import), (12, compose_in_order_class)] {
        assert!(
            core.request_definition(main.clone(), line, column),
            "cursor {line}:{column}"
        );
        let definition = core.definition_locations(main.clone());
        assert_eq!(definition.len(), 1);
        assert!(
            definition[0]
                .uri
                .ends_with("@poppinss/utils/build/src/compose.d.ts"),
            "{definition:?}"
        );
        assert_eq!((definition[0].line, definition[0].column), (0, 24));
    }
}

#[test]
fn typescript_definition_resolves_named_declaration_reexports() {
    let dir = tempfile::tempdir().unwrap();
    let strategies = dir.path().join("naming_strategies");
    std::fs::create_dir_all(&strategies).unwrap();
    std::fs::write(
            strategies.join("camel_case.d.ts"),
            "export declare class CamelCaseNamingStrategy {}\nexport declare class Internal {}\nexport interface Typed {}\n",
        )
        .unwrap();
    let barrel_path = dir.path().join("main.d.ts");
    std::fs::write(
            &barrel_path,
            "export { CamelCaseNamingStrategy } from './naming_strategies/camel_case.js';\nexport { Internal as Public } from './naming_strategies/camel_case.js';\nexport type { Typed } from './naming_strategies/camel_case.js';\nexport * from './naming_strategies/camel_case.js';\nexport { Broken as } from './naming_strategies/camel_case.js';\n",
        )
        .unwrap();
    let core = Core::new();
    let barrel = core
        .open_document(barrel_path.to_string_lossy().into_owned())
        .unwrap();

    for (line, column, target_line, target_column) in [
        (0, 10, 0, 21),
        (1, 10, 1, 21),
        (1, 22, 1, 21),
        (2, 15, 2, 7),
    ] {
        assert!(
            core.request_definition(barrel.clone(), line, column),
            "cursor {line}:{column}"
        );
        let definition = core.definition_locations(barrel.clone());
        assert_eq!(definition.len(), 1);
        assert!(
            definition[0]
                .uri
                .ends_with("naming_strategies/camel_case.d.ts")
        );
        assert_eq!(definition[0].line, target_line);
        assert_eq!(definition[0].column, target_column);
    }
    assert!(!core.request_definition(barrel.clone(), 3, 7));
    assert!(!core.request_definition(barrel, 4, 10));
}

#[test]
fn typescript_navigation_resolves_node_modules_declaration_imports_safely() {
    let dir = tempfile::tempdir().unwrap();
    let lucid = dir
        .path()
        .join("node_modules/@adonisjs/lucid/build/src/connection");
    std::fs::create_dir_all(&lucid).unwrap();
    std::fs::write(
        dir.path().join("node_modules/@adonisjs/lucid/package.json"),
        r#"{"types":"./build/src/connection/index.d.ts"}"#,
    )
    .unwrap();
    std::fs::write(
        lucid.join("types.d.ts"),
        "export interface ConnectionConfig {}\nexport interface ConnectionContract {}\n",
    )
    .unwrap();
    let tarn = dir.path().join("node_modules/tarn/dist");
    std::fs::create_dir_all(&tarn).unwrap();
    std::fs::write(
        dir.path().join("node_modules/tarn/package.json"),
        r#"{"types":"./dist/tarn.d.ts"}"#,
    )
    .unwrap();
    std::fs::write(
        tarn.join("tarn.d.ts"),
        "export * from './one.js';\nexport * from './two.js';\n",
    )
    .unwrap();
    std::fs::write(tarn.join("one.js"), "").unwrap();
    std::fs::write(tarn.join("two.js"), "").unwrap();
    std::fs::write(tarn.join("one.d.ts"), "export declare class Pool {}\n").unwrap();
    std::fs::write(tarn.join("two.d.ts"), "export declare class Other {}\n").unwrap();
    let knex = dir.path().join("node_modules/knex/types");
    std::fs::create_dir_all(&knex).unwrap();
    std::fs::write(
        dir.path().join("node_modules/knex/package.json"),
        r#"{"types":"./types/index.d.ts"}"#,
    )
    .unwrap();
    std::fs::write(knex.join("index.d.ts"), "declare namespace Knex {}\n").unwrap();
    let core = dir.path().join("node_modules/@adonisjs/core");
    std::fs::create_dir_all(core.join("build")).unwrap();
    std::fs::write(
        core.join("package.json"),
        r#"{"exports":{"./logger":"./build/logger.js"}}"#,
    )
    .unwrap();
    std::fs::write(
        core.join("build/logger.d.ts"),
        "export as namespace Logger;\n",
    )
    .unwrap();
    let node = dir.path().join("node_modules/@types/node");
    std::fs::create_dir_all(&node).unwrap();
    std::fs::write(
        node.join("package.json"),
        r#"{"exports":{"./events":"./events.d.ts"}}"#,
    )
    .unwrap();
    std::fs::write(
        node.join("events.d.ts"),
        "export declare class EventEmitter {}\n",
    )
    .unwrap();
    let connection = lucid.join("index.d.ts");
    std::fs::write(
            &connection,
            "const broken = ;\nimport type { ConnectionConfig, ConnectionContract } from './types.js';\nimport { Pool } from 'tarn';\nimport type Knex from 'knex';\nimport Logger from '@adonisjs/core/logger';\nimport { EventEmitter } from 'node:events';\nexport type UsesConfig = ConnectionConfig;\nexport type UsesContract = ConnectionContract;\nexport type UsesPool = Pool;\nexport type UsesKnex = Knex;\nexport type UsesLogger = Logger;\nexport type UsesEmitter = EventEmitter;\n",
        )
        .unwrap();
    let core = Core::new();
    let document = core
        .open_document(connection.to_string_lossy().into_owned())
        .unwrap();

    for (line, column, target_line) in [(1, 14, 0), (1, 32, 1)] {
        assert!(
            core.request_definition(document.clone(), line, column),
            "cursor {line}:{column}"
        );
        let definition = core.definition_locations(document.clone());
        assert!(definition[0].uri.ends_with("connection/types.d.ts"));
        assert_eq!((definition[0].line, definition[0].column), (target_line, 7));
    }
    for (line, column, target, target_position) in [
        (2, 9, "tarn/dist/one.d.ts", (0, 21)),
        (3, 12, "knex/types/index.d.ts", (0, 0)),
        (4, 7, "@adonisjs/core/build/logger.d.ts", (0, 0)),
        (5, 9, "@types/node/events.d.ts", (0, 21)),
    ] {
        assert!(
            core.request_definition(document.clone(), line, column),
            "cursor {line}:{column}"
        );
        let definition = core.definition_locations(document.clone());
        assert!(definition[0].uri.ends_with(target));
        assert_eq!((definition[0].line, definition[0].column), target_position);
    }
    let malformed = lucid.join("malformed.d.ts");
    std::fs::write(&malformed, "import { Malformed from 'tarn';\n").unwrap();
    let malformed = core
        .open_document(malformed.to_string_lossy().into_owned())
        .unwrap();
    assert!(!core.request_definition(malformed, 0, 9));
}

#[test]
fn typescript_navigation_does_not_resolve_node_specifiers_without_local_types() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("node_modules/pkg/index.d.ts");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path.parent().unwrap().join("package.json"), "{}").unwrap();
    std::fs::write(
        &path,
        "import { EventEmitter } from 'node:events';\nEventEmitter;\n",
    )
    .unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    assert!(!core.request_definition(document, 0, 9));
}

#[test]
fn typescript_navigation_resolves_bare_node_builtin_module_strings() {
    let dir = tempfile::tempdir().unwrap();
    let types = dir.path().join("node_modules/@types/node");
    std::fs::create_dir_all(&types).unwrap();
    std::fs::write(
        types.join("package.json"),
        r#"{"exports":{"./path":"./path.d.ts"}}"#,
    )
    .unwrap();
    std::fs::write(
        types.join("path.d.ts"),
        "declare const path: { sep: string };\nexport = path;\n",
    )
    .unwrap();
    let path = dir.path().join("main.ts");
    std::fs::write(&path, "import path from 'path';\n").unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(document.clone(), 0, 18));
    let definition = core.definition_locations(document.clone());
    assert_eq!(definition.len(), 1);
    assert!(definition[0].uri.ends_with("@types/node/path.d.ts"));

    assert!(core.request_definition(document.clone(), 0, 7));
    let definition = core.definition_locations(document);
    assert_eq!(definition.len(), 1);
    assert!(definition[0].uri.ends_with("@types/node/path.d.ts"));
    assert_eq!((definition[0].line, definition[0].column), (0, 14));
}

#[test]
fn typescript_union_members_require_one_exact_definition() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    std::fs::write(
        dir.path().join("types.ts"),
        "export interface User { name: string }\nexport interface Other { name: string }\n",
    )
    .unwrap();
    let main_path = dir.path().join("main.ts");
    std::fs::write(
        &main_path,
        "import type { User, Other } from './types';\n\
             import { User as ValueUser } from './types';\n\
             const same: User | User = {} as never; same.name;\n\
             const different: User | Other = {} as never; different.name;\n\
             const inline: { name: string } | { name: string } = {} as never; inline.name;\n\
             const value: ValueUser = {} as never; value.name;\n\
             { interface User { local: string } const local: User = {} as never; local.local; }\n",
    )
    .unwrap();
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(main.clone(), 2, 45));
    let same = core.definition_locations(main.clone());
    assert_eq!(same.len(), 1);
    assert!(same[0].uri.ends_with("types.ts"));
    assert_eq!((same[0].line, same[0].column), (0, 24));
    for (line, column) in [(3, 57), (4, 83), (5, 53)] {
        assert!(
            !core.request_definition(main.clone(), line, column),
            "cursor {line}:{column}"
        );
    }
    assert!(core.request_definition(main, 6, 78));
}

#[test]
fn typescript_code_actions_are_not_dispatched_to_the_php_engine() {
    let core = Core::new();
    let doc = doc_at(
        "component.ts",
        "import { Widget } from './widget';\nWidget;\n",
        0,
    );

    assert!(!core.request_code_actions(doc, 42, 99));
}

#[test]
fn source_only_watcher_delta_updates_typescript_auto_imports() {
    let root = tempfile::tempdir().unwrap();
    let models = root.path().join("models");
    let consumers = root.path().join("consumers");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::create_dir_all(&consumers).unwrap();
    let model = models.join("user.ts");
    let consumer = consumers.join("auth.ts");
    std::fs::write(&model, "export class OldUser {}\n").unwrap();
    std::fs::write(&consumer, "Ol\n").unwrap();
    let core = Core::new();
    core.open_workspace(root.path().to_string_lossy().into_owned())
        .unwrap();
    let document = core
        .open_document(consumer.to_string_lossy().into_owned())
        .unwrap();
    until("TypeScript workspace completion", || {
        core.request_completion(document.clone(), 0, 2)
            && core
                .completions(document.clone())
                .iter()
                .any(|item| item.label == "OldUser")
    });
    std::fs::write(&model, "export class NewUser {}\n").unwrap();
    core.apply_workspace_changes(
        root.path().to_string_lossy().into_owned(),
        vec!["models/user.ts".into()],
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
    until("TypeScript delta completion", || {
        core.request_completion(document.clone(), 0, 2)
            && core
                .completions(document.clone())
                .iter()
                .any(|item| item.label == "NewUser")
    });
    assert!(
        core.completions(document)
            .iter()
            .all(|item| item.label != "OldUser")
    );
}

#[test]
fn route_name_at_accepts_only_the_first_string_of_a_bare_route_call() {
    let line = "router.delete(route('couriers.destroy', courier.id));";
    let at = line.find("couriers").unwrap();
    assert_eq!(route_name_at(line, at + 2), Some("couriers.destroy"));
    // Either quote counts, the arguments after the name do not.
    assert_eq!(route_name_at(line, at - 1), Some("couriers.destroy"));
    assert_eq!(route_name_at(line, line.find("courier.id").unwrap()), None);
    assert_eq!(route_name_at("confirm('couriers.index')", 12), None);
    assert_eq!(route_name_at("this.route('couriers.index')", 15), None);
    assert_eq!(route_name_at("$route('couriers.index')", 12), None);
    assert_eq!(route_name_at("route(`a.${b}`)", 8), None);
    assert_eq!(route_name_at("route( 'a.b' )", 9), Some("a.b"));
    assert_eq!(route_name_at("route('')", 7), None);
}

#[test]
fn vue_route_calls_resolve_to_laravel_routes_and_ziggy() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("artisan"), "#!/usr/bin/env php\n").unwrap();
    std::fs::write(root.join("package.json"), "{}\n").unwrap();
    std::fs::create_dir_all(root.join("routes")).unwrap();
    std::fs::write(
        root.join("routes/web.php"),
        "<?php\n\nuse Illuminate\\Support\\Facades\\Route;\n\n\
         Route::put('/couriers/{courier}', [CourierController::class, 'update'])->name('couriers.update');\n",
    )
    .unwrap();
    let ziggy = root.join("node_modules/ziggy-js");
    std::fs::create_dir_all(ziggy.join("src/js")).unwrap();
    std::fs::write(
        ziggy.join("package.json"),
        r#"{"name":"ziggy-js","type":"module","types":"./src/js/index.d.ts"}"#,
    )
    .unwrap();
    std::fs::write(
        ziggy.join("src/js/index.d.ts"),
        "export interface Router {}\n\
         export function route(): Router;\n\
         export function route(name: string): string;\n",
    )
    .unwrap();
    let source = "<script setup lang=\"ts\">\n\
                  const go = () => route('couriers.update', 1);\n\
                  const missing = route('couriers.nope');\n\
                  const text = confirm('couriers.update');\n\
                  </script>\n\
                  <template>\n\
                  \x20 <a :href=\"route('couriers.update', 1)\">Edit</a>\n\
                  </template>\n";
    let page = root.join("resources/js/Pages/Couriers/Index.vue");
    std::fs::create_dir_all(page.parent().unwrap()).unwrap();
    std::fs::write(&page, source).unwrap();
    let core = Core::new();
    let document = core
        .open_document(page.to_string_lossy().into_owned())
        .unwrap();
    let lines: Vec<&str> = source.lines().collect();

    // The route name, in the script and in a template expression.
    for line in [1u64, 6] {
        let column = lines[line as usize].find("couriers.update").unwrap() as u64 + 3;
        assert!(
            core.request_definition(document.clone(), line, column),
            "line {line}"
        );
        let locations = core.definition_locations(document.clone());
        assert_eq!(locations.len(), 1, "{locations:?}");
        assert!(
            locations[0].uri.ends_with("routes/web.php"),
            "{locations:?}"
        );
        assert_eq!(locations[0].line, 4, "{locations:?}");
    }

    // The `route` helper itself, in the script and in the template.
    for line in [1u64, 6] {
        let column = lines[line as usize].find("route(").unwrap() as u64 + 2;
        assert!(
            core.request_definition(document.clone(), line, column),
            "line {line}"
        );
        let locations = core.definition_locations(document.clone());
        assert_eq!(locations.len(), 1, "{locations:?}");
        assert!(
            locations[0].uri.ends_with("ziggy-js/src/js/index.d.ts") && locations[0].line == 1,
            "{locations:?}"
        );
    }

    // An unknown name and a string that is not a `route()` argument stay quiet.
    let unknown = lines[2].find("couriers.nope").unwrap() as u64 + 3;
    assert!(!core.request_definition(document.clone(), 2, unknown));
    let other = lines[3].find("couriers.update").unwrap() as u64 + 3;
    assert!(!core.request_definition(document.clone(), 3, other));
}

#[test]
fn vue_template_names_resolve_to_script_declarations_and_imports() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("package.json"), "{}\n").unwrap();
    std::fs::write(
        root.join("helper.ts"),
        "export function helper(n: number) { return n; }\n",
    )
    .unwrap();
    let source = "<script setup lang=\"ts\">\n\
                  import { helper } from './helper';\n\
                  const deleteOpname = (id: number) => id;\n\
                  </script>\n\
                  <template>\n\
                  \x20 <button @click=\"deleteOpname(op)\">{{ helper(1) }}</button>\n\
                  \x20 <p>{{ item.deleteOpname }}</p>\n\
                  </template>\n";
    let page = root.join("Index.vue");
    std::fs::write(&page, source).unwrap();
    let core = Core::new();
    let document = core
        .open_document(page.to_string_lossy().into_owned())
        .unwrap();
    let lines: Vec<&str> = source.lines().collect();

    let column = lines[5].find("deleteOpname").unwrap() as u64 + 3;
    assert!(core.request_definition(document.clone(), 5, column));
    let locations = core.definition_locations(document.clone());
    assert!(locations[0].uri.ends_with("Index.vue"), "{locations:?}");
    assert_eq!(locations[0].line, 2, "{locations:?}");

    let column = lines[5].find("helper").unwrap() as u64 + 2;
    assert!(core.request_definition(document.clone(), 5, column));
    let locations = core.definition_locations(document.clone());
    assert!(locations[0].uri.ends_with("helper.ts"), "{locations:?}");

    // A property of another object and a name the scripts lack stay unresolved.
    let column = lines[6].find("deleteOpname").unwrap() as u64 + 3;
    assert!(!core.request_definition(document.clone(), 6, column));
    let column = lines[5].find("op)").unwrap() as u64 + 1;
    assert!(!core.request_definition(document.clone(), 5, column));
}

#[test]
fn typescript_navigation_resolves_vite_import_meta_members_and_env_keys() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    std::fs::create_dir_all(dir.path().join("node_modules/vite/types")).unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("node_modules/vite/types/importMeta.d.ts"),
        "interface ImportMetaEnv extends Record<string, any> {\n  BASE_URL: string\n}\n\n\
         interface ImportMeta {\n  url: string\n  readonly env: ImportMetaEnv\n  glob: Fn\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join(".env"),
        "APP_NAME=x\nexport VITE_APP_NAME=\"Cafe\"\n# VITE_OFF=1\n",
    )
    .unwrap();
    let main_path = dir.path().join("src/main.ts");
    std::fs::write(
        &main_path,
        "const a = import.meta.env.VITE_APP_NAME;\nconst b = import.meta.glob('./x');\n\
         const c = import.meta.env.VITE_OFF;\nconst d = other.glob;\n",
    )
    .unwrap();
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();
    let locate = |line: u64, column: u64| {
        core.request_definition(main.clone(), line, column)
            .then(|| core.definition_locations(main.clone()))
    };

    let env = locate(0, 22).unwrap();
    assert!(env[0].uri.ends_with("vite/types/importMeta.d.ts"));
    assert_eq!((env[0].line, env[0].column), (6, 11));
    let key = locate(0, 30).unwrap();
    assert!(key[0].uri.ends_with("/.env"));
    assert_eq!((key[0].line, key[0].column), (1, 7));
    let glob = locate(1, 24).unwrap();
    assert!(glob[0].uri.ends_with("vite/types/importMeta.d.ts"));
    assert_eq!((glob[0].line, glob[0].column), (7, 2));
    // Commented-out keys and non-`import.meta` receivers resolve to nothing.
    assert!(locate(2, 30).is_none());
    assert!(locate(3, 17).is_none());
}

/// Opens `source` as `src/main.ts` of a project and returns the "Cannot find
/// name" messages once diagnostics were computed.
fn unknown_name_reports(dir: &Path, source: &str) -> Vec<String> {
    let path = dir.join("src/main.ts");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(&path, source).unwrap();
    let core = Core::new();
    core.open_workspace(dir.to_string_lossy().into_owned())
        .unwrap();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    assert!(core.register_document(document.clone(), "typescript".into(), String::new()));
    // Probe name in a sibling-free file: it is unknown in every project below.
    let mut messages = Vec::new();
    for _ in 0..60 {
        core.poll_analysis();
        std::thread::sleep(Duration::from_millis(50));
        messages = document
            .lock()
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.message.starts_with("Cannot find name"))
            .map(|diagnostic| diagnostic.message.clone())
            .collect();
        if !messages.is_empty() {
            break;
        }
    }
    messages
}

fn typescript_project(dir: &Path) {
    std::fs::write(dir.join("package.json"), "{}\n").unwrap();
    let lib = dir.join("node_modules/typescript/lib");
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::write(lib.join("lib.d.ts"), "/// <reference lib=\"es5\" />\n").unwrap();
    std::fs::write(
        lib.join("lib.es5.d.ts"),
        "interface Promise<T> {}\ndeclare var console: { log(m: string): void };\n\
         type Record<K extends keyof any, T> = { [P in K]: T };\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("types")).unwrap();
    std::fs::write(
        dir.join("types/globals.d.ts"),
        "declare var route: (name: string) => string;\n",
    )
    .unwrap();
}

#[test]
fn typescript_reports_names_no_global_or_declaration_provides() {
    let dir = tempfile::tempdir().unwrap();
    typescript_project(dir.path());
    let source = "import { thing } from './thing';\n\
         const { theme = 'x', ...rest } = thing;\n\
         type Cb = (event: string) => void;\n\
         console.log(route('a'), theme, rest);\n\
         const p: Promise<Record<string, number>> = missingValue;\n";
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/thing.ts"),
        "export const thing = {};\n",
    )
    .unwrap();
    let reports = unknown_name_reports(dir.path(), source);
    assert_eq!(reports, ["Cannot find name 'missingValue'.".to_owned()]);
}

#[test]
fn typescript_reports_no_unknown_names_when_globals_are_not_knowable() {
    // No TypeScript installation: lib globals cannot be listed.
    let bare = tempfile::tempdir().unwrap();
    std::fs::write(bare.path().join("package.json"), "{}\n").unwrap();
    assert!(unknown_name_reports(bare.path(), "missingValue;\n").is_empty());

    // Auto-import tooling adds globals at build time.
    let nuxt = tempfile::tempdir().unwrap();
    typescript_project(nuxt.path());
    std::fs::write(
        nuxt.path().join("package.json"),
        r#"{"dependencies":{"nuxt":"3"}}"#,
    )
    .unwrap();
    assert!(unknown_name_reports(nuxt.path(), "missingValue;\n").is_empty());
}

#[test]
fn svelte_runes_resolve_to_the_svelte_package_declarations() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("package.json"), "{}\n").unwrap();
    std::fs::create_dir_all(root.join("node_modules/svelte/types")).unwrap();
    std::fs::write(
        root.join("node_modules/svelte/types/index.d.ts"),
        "declare function $state<T>(initial: T): T;\n\n\
         declare function $props(): any;\ndeclare namespace $props {\n}\n",
    )
    .unwrap();
    let path = root.join("+layout.svelte");
    let source = "<script lang=\"ts\">\n\tlet { children } = $props();\n\tlet n = $state(0);\n</script>\n\n<main>\n\t{@render children()}\n</main>\n";
    std::fs::write(&path, source).unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(document.clone(), 1, 22));
    let props = core.definition_locations(document.clone());
    assert!(
        props[0]
            .uri
            .ends_with("node_modules/svelte/types/index.d.ts")
    );
    assert_eq!((props[0].line, props[0].column), (2, 17));
    assert!(core.request_definition(document.clone(), 2, 12));
    let state = core.definition_locations(document.clone());
    assert_eq!((state[0].line, state[0].column), (0, 17));

    // `children` in the template goes to the destructured prop.
    assert!(core.request_definition(document.clone(), 6, 12));
    let children = core.definition_locations(document.clone());
    assert!(children[0].uri.ends_with("+layout.svelte"));
    assert_eq!(children[0].line, 1);
}

#[test]
fn svelte_import_from_a_declare_module_block_goes_to_the_exported_declaration() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("package.json"), "{}\n").unwrap();
    std::fs::create_dir_all(root.join("node_modules/svelte/types")).unwrap();
    std::fs::write(
        root.join("node_modules/svelte/package.json"),
        r#"{"name":"svelte","types":"./types/index.d.ts","exports":{".":{"types":"./types/index.d.ts"},"./transition":{"types":"./types/index.d.ts"}}}"#,
    )
    .unwrap();
    std::fs::write(
        root.join("node_modules/svelte/types/index.d.ts"),
        "/// <reference types=\"esrap\" />\n\ndeclare module 'svelte' {\n\t/** docs */\n\texport function onMount<T>(fn: () => T): void;\n\texport interface ComponentConstructorOptions {}\n}\n\ndeclare module 'svelte/transition' {\n\texport function fly(node: Element): void;\n}\n",
    )
    .unwrap();
    let path = root.join("Page.svelte");
    let source = "<script lang=\"ts\">\n\timport { onMount } from 'svelte';\n\timport { fly } from 'svelte/transition';\n\tonMount(() => {});\n\tfly(null);\n</script>\n";
    std::fs::write(&path, source).unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();
    let locate = |line: u64, column: u64| {
        let found = core.request_definition(document.clone(), line, column);
        assert!(found, "no definition at {line}:{column}");
        core.definition_locations(document.clone())
    };
    let mount = locate(3, 3);
    assert!(mount[0].uri.ends_with("svelte/types/index.d.ts"));
    assert_eq!(mount[0].line, 4);
    let mount_import = locate(1, 12);
    assert_eq!(mount_import[0].line, 4);
    let fly = locate(4, 2);
    assert_eq!(fly[0].line, 9);
}

#[test]
fn svelte_component_package_import_goes_to_the_component_class() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("package.json"), "{}\n").unwrap();
    let dist = root.join("node_modules/icons/dist");
    std::fs::create_dir_all(dist.join("icons")).unwrap();
    std::fs::write(
        root.join("node_modules/icons/package.json"),
        r#"{"name":"icons","exports":{".":{"types":"./dist/index.d.ts","svelte":"./dist/index.js"}}}"#,
    )
    .unwrap();
    std::fs::write(dist.join("index.d.ts"), "export * from './icons/index';\n").unwrap();
    std::fs::write(
        dist.join("icons/index.d.ts"),
        "export { default as Github } from './github.svelte';\n",
    )
    .unwrap();
    std::fs::write(dist.join("icons/github.svelte"), "<svg></svg>\n").unwrap();
    std::fs::write(
        dist.join("icons/github.svelte.d.ts"),
        "import { SvelteComponentTyped } from \"svelte\";\nexport default class Github extends SvelteComponentTyped {\n}\n",
    )
    .unwrap();
    let path = root.join("Card.svelte");
    let source =
        "<script lang=\"ts\">\n\timport { Github } from 'icons';\n</script>\n\n<Github />\n";
    std::fs::write(&path, source).unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    for (line, column) in [(1, 11), (4, 2)] {
        assert!(core.request_definition(document.clone(), line, column));
        let found = core.definition_locations(document.clone());
        assert!(
            found[0].uri.ends_with("icons/github.svelte.d.ts"),
            "{found:?}"
        );
        assert_eq!((found[0].line, found[0].column), (1, 15));
    }
}

#[test]
fn parameter_hints_follow_local_imported_and_package_callees() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("package.json"), "{}\n").unwrap();
    std::fs::create_dir_all(root.join("node_modules/fmt")).unwrap();
    std::fs::write(
        root.join("node_modules/fmt/package.json"),
        r#"{"name":"fmt","types":"./index.d.ts"}"#,
    )
    .unwrap();
    std::fs::write(
        root.join("node_modules/fmt/index.d.ts"),
        "export declare function pad(value: string, width: number): string;\n",
    )
    .unwrap();
    std::fs::write(
        root.join("math.ts"),
        "export function clamp(value: number, min: number, max: number) {\n\treturn value;\n}\n",
    )
    .unwrap();
    let path = root.join("Card.svelte");
    let source = "<script lang=\"ts\">\n\timport { clamp } from './math';\n\timport { pad } from 'fmt';\n\tfunction greet(name: string, ...rest: string[]) {}\n\tconst value = 1;\n\tclamp(value, 0, 10);\n\tpad('x', 4);\n\tgreet('Ada', 'a', 'b');\n\tunknown(1);\n</script>\n\n<p>{greet('x')}</p>\n";
    std::fs::write(&path, source).unwrap();
    let core = Core::new();
    let document = core
        .open_document(path.to_string_lossy().into_owned())
        .unwrap();

    let hints = core.parameter_hints(document.clone(), 0, 20);
    assert_eq!(hints.revision, document.revision());
    assert!(
        hints
            .lines
            .iter()
            .all(|line| line.text == document.display_line(line.line)),
        "each line carries the text its columns refer to"
    );
    let found: Vec<(u64, u64, &str)> = hints
        .lines
        .iter()
        .flat_map(|line| &line.hints)
        .map(|hint| (hint.line, hint.column, hint.label.as_str()))
        .collect();
    assert_eq!(
        found,
        vec![
            // `value` repeats its parameter name.
            (5, 14, "min:"),
            (5, 17, "max:"),
            (6, 5, "value:"),
            (6, 10, "width:"),
            (7, 7, "name:"),
            (7, 14, "...rest:"),
        ]
    );
}
