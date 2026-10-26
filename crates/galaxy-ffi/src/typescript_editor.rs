use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use galaxy_core::analysis::{
    CompletionItem as LspCompletionItem, DocumentSymbolInfo, Range as LspRange, RenameEdit,
};
use galaxy_core::typescript::{Declaration, DeclarationKind, DocumentIndex};

use crate::typescript_navigation::{
    TypeScriptNavigationCache, typescript_dependency_packages, typescript_package_exports,
};
use std::sync::Mutex;

const MAX_TYPESCRIPT_COMPLETIONS: usize = 100;
const MAX_WORKSPACE_TYPESCRIPT_FILES: usize = 10_000;
const MAX_WORKSPACE_TYPESCRIPT_FILE_BYTES: u64 = 1_048_576;
const MAX_WORKSPACE_TYPESCRIPT_ROOTS: usize = 8;

#[derive(Debug, Clone)]
struct WorkspaceExport {
    name: String,
    kind: DeclarationKind,
    path: String,
    default: bool,
}

/// FFI owns disk-backed workspace state; core supplies the per-file parser.
#[derive(Debug, Default)]
pub(super) struct TypeScriptWorkspaceExports {
    roots: BTreeMap<PathBuf, Vec<WorkspaceExport>>,
}

impl TypeScriptWorkspaceExports {
    pub(super) fn refresh(&mut self, root: &Path, paths: &[String]) {
        let mut by_name = BTreeMap::<String, Vec<WorkspaceExport>>::new();
        for path in paths
            .iter()
            .filter(|path| is_source_path(path))
            .take(MAX_WORKSPACE_TYPESCRIPT_FILES)
        {
            let full_path = root.join(path);
            let Ok(metadata) = std::fs::metadata(&full_path) else {
                continue;
            };
            if metadata.len() > MAX_WORKSPACE_TYPESCRIPT_FILE_BYTES {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&full_path) else {
                continue;
            };
            let index = DocumentIndex::parse(&full_path, &text);
            for declaration in index.declarations.iter().filter(|item| item.exported) {
                by_name
                    .entry(declaration.name.clone())
                    .or_default()
                    .push(WorkspaceExport {
                        name: declaration.name.clone(),
                        kind: declaration.kind,
                        path: path.clone(),
                        default: declaration.default,
                    });
            }
        }
        // A name resolving to more than one module cannot be imported safely.
        if !self.roots.contains_key(root) && self.roots.len() >= MAX_WORKSPACE_TYPESCRIPT_ROOTS {
            self.roots.pop_first();
        }
        self.roots.insert(
            root.to_path_buf(),
            by_name
                .into_values()
                .filter_map(|exports| {
                    (exports.len() == 1).then(|| exports.into_iter().next().unwrap())
                })
                .collect(),
        );
    }

    fn exports_for(&self, path: &Path) -> Option<(&Path, &[WorkspaceExport])> {
        self.roots
            .iter()
            .filter(|(root, _)| path.strip_prefix(root).is_ok())
            .max_by_key(|(root, _)| root.components().count())
            .map(|(root, exports)| (root.as_path(), exports.as_slice()))
    }
}

pub(super) fn typescript_completions(
    path: &str,
    edit_uri: &str,
    text: &str,
    line: usize,
    character: usize,
    workspace_exports: &TypeScriptWorkspaceExports,
    navigation_cache: &Mutex<TypeScriptNavigationCache>,
    script_insertion_line: usize,
    component_extension: Option<&str>,
) -> Vec<LspCompletionItem> {
    let Some(prefix) = typescript_identifier_prefix(text, line, character) else {
        return Vec::new();
    };
    let index = DocumentIndex::parse(path, text);
    let insertion = import_insertion_point(&index, script_insertion_line);
    let mut items = BTreeMap::new();
    if character > 0
        && let Some(symbol) = index.symbol_at(line, character - 1)
        && symbol.name.starts_with(&prefix)
    {
        items
            .entry(symbol.name.clone())
            .or_insert_with(|| typescript_completion_item(&symbol.name, symbol.kind));
    }
    for declaration in &index.declarations {
        if declaration.name.starts_with(&prefix) {
            items
                .entry(declaration.name.clone())
                .or_insert_with(|| typescript_completion_item(&declaration.name, declaration.kind));
        }
    }
    for binding in index
        .imports
        .iter()
        .flat_map(|import| import.bindings.iter())
        .filter(|binding| !binding.type_only && binding.local_name.starts_with(&prefix))
    {
        items.entry(binding.local_name.clone()).or_insert_with(|| {
            typescript_completion_item(&binding.local_name, DeclarationKind::Variable)
        });
    }
    let unavailable = index
        .declarations
        .iter()
        .map(|item| item.name.as_str())
        .chain(index.imports.iter().flat_map(|import| {
            import.bindings.iter().flat_map(|binding| {
                [
                    Some(binding.local_name.as_str()),
                    binding.imported_name.as_deref(),
                ]
                .into_iter()
                .flatten()
            })
        }))
        .collect::<BTreeSet<_>>();
    let document_path = Path::new(path);
    if let Some((workspace_root, exports)) = workspace_exports.exports_for(document_path) {
        let Some(document_relative) = document_path.strip_prefix(workspace_root).ok() else {
            return Vec::new();
        };
        for export in exports {
            if Path::new(&export.path) == document_relative
                || !export.name.starts_with(&prefix)
                || unavailable.contains(export.name.as_str())
            {
                continue;
            }
            let Some(specifier) = workspace_module_specifier(document_relative, &export.path)
            else {
                continue;
            };
            let import = if export.default {
                format!("import {} from '{specifier}';\n", export.name)
            } else {
                format!("import {{ {} }} from '{specifier}';\n", export.name)
            };
            items
                .entry(export.name.clone())
                .or_insert_with(|| LspCompletionItem {
                    detail: Some(format!(
                        "{} - {}",
                        typescript_declaration_kind(export.kind),
                        export.path
                    )),
                    additional_edits: vec![RenameEdit {
                        uri: edit_uri.to_owned(),
                        range: LspRange {
                            start_line: insertion as u64,
                            start_character: 0,
                            end_line: insertion as u64,
                            end_character: 0,
                        },
                        new_text: import,
                    }],
                    ..typescript_completion_item(&export.name, export.kind)
                });
        }
    }
    let mut package_specifiers = index
        .imports
        .iter()
        .map(|import| import.specifier.as_str())
        .filter(|specifier| !specifier.starts_with('.') && !specifier.starts_with('/') && !specifier.starts_with("node:"))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let extension = component_extension
        .or_else(|| document_path.extension().and_then(|extension| extension.to_str()));
    for (name, specifier) in framework_component_completions(extension, &prefix) {
        if unavailable.contains(name) {
            continue;
        }
        items.entry(name.to_owned()).or_insert_with(|| LspCompletionItem {
            detail: Some(format!("function - {specifier}")),
            additional_edits: vec![RenameEdit {
                uri: edit_uri.to_owned(),
                range: LspRange {
                    start_line: insertion as u64,
                    start_character: 0,
                    end_line: insertion as u64,
                    end_character: 0,
                },
                new_text: format!("import {{ {name} }} from '{specifier}';\n"),
            }],
            ..typescript_completion_item(name, DeclarationKind::Function)
        });
    }
    if !matches!(extension, Some("vue" | "svelte")) && prefix.len() >= 3 {
        package_specifiers.extend(typescript_dependency_packages(document_path));
    }
    package_specifiers.retain(|specifier| {
        !(extension == Some("vue") && specifier == "vue")
            && !(extension == Some("svelte") && specifier == "svelte")
    });
    for specifier in package_specifiers {
        for (name, kind) in typescript_package_exports(navigation_cache, document_path, &specifier, &prefix) {
            if unavailable.contains(name.as_str()) {
                continue;
            }
            items.entry(name.clone()).or_insert_with(|| LspCompletionItem {
                detail: Some(format!("{} - {specifier}", typescript_declaration_kind(kind))),
                additional_edits: vec![RenameEdit {
                    uri: edit_uri.to_owned(),
                    range: LspRange {
                        start_line: insertion as u64,
                        start_character: 0,
                        end_line: insertion as u64,
                        end_character: 0,
                    },
                    new_text: format!("import {{ {name} }} from '{specifier}';\n"),
                }],
                ..typescript_completion_item(&name, kind)
            });
        }
    }
    items
        .into_values()
        .take(MAX_TYPESCRIPT_COMPLETIONS)
        .collect()
}

fn framework_component_completions<'a>(extension: Option<&str>, prefix: &str) -> Vec<(&'a str, &'a str)> {
    let (package, names): (&str, &[&str]) = match extension {
        Some("vue") => (
            "vue",
            &[
                "computed", "ref", "reactive", "readonly", "watch", "watchEffect", "watchPostEffect",
                "watchSyncEffect", "toRef", "toRefs", "unref", "isRef", "nextTick", "onMounted",
                "onUnmounted", "onUpdated", "onBeforeMount", "onBeforeUnmount", "onBeforeUpdate",
                "defineProps", "defineEmits", "defineExpose", "defineModel", "defineOptions",
                "provide", "inject", "useTemplateRef",
            ],
        ),
        Some("svelte") => (
            "svelte",
            &["onMount", "onDestroy", "beforeUpdate", "afterUpdate", "tick", "createEventDispatcher"],
        ),
        _ => return Vec::new(),
    };
    names
        .iter()
        .copied()
        .filter(|name| name.starts_with(prefix))
        .map(|name| (name, package))
        .collect()
}

fn is_source_path(path: &str) -> bool {
    !path.ends_with(".d.ts")
        && [".ts", ".tsx", ".mts", ".cts"]
            .iter()
            .any(|extension| path.ends_with(extension))
}

fn import_insertion_point(index: &DocumentIndex, fallback: usize) -> usize {
    index
        .imports
        .iter()
        .map(|import| import.range.end.line + 1)
        .max()
        .unwrap_or(fallback)
}

fn workspace_module_specifier(document: &Path, exported_path: &str) -> Option<String> {
    let from = document.parent()?;
    let target = Path::new(exported_path).with_extension("js");
    let mut from_components = from.components().filter_map(normal_component).peekable();
    let mut target_components = target.components().filter_map(normal_component).peekable();
    while from_components.peek().is_some() && from_components.peek() == target_components.peek() {
        from_components.next();
        target_components.next();
    }
    let mut parts = vec![".."; from_components.count()];
    parts.extend(target_components);
    (!parts.is_empty()).then(|| {
        let value = parts.join("/");
        if value.starts_with('.') {
            value
        } else {
            format!("./{value}")
        }
    })
}

fn normal_component(component: Component<'_>) -> Option<&str> {
    match component {
        Component::Normal(component) => component.to_str(),
        _ => None,
    }
}

pub(super) fn typescript_identifier_prefix(
    text: &str,
    line: usize,
    character: usize,
) -> Option<String> {
    let line = text.split('\n').nth(line)?;
    let line = line.strip_suffix('\r').unwrap_or(line);
    let mut column = 0;
    let mut byte = 0;
    for (offset, ch) in line.char_indices() {
        let width = ch.len_utf16();
        if column + width > character {
            return None;
        }
        column += width;
        byte = offset + ch.len_utf8();
        if column == character {
            break;
        }
    }
    if column != character {
        return None;
    }
    let prefix = &line[..byte];
    let start = prefix
        .char_indices()
        .rev()
        .find(|(_, ch)| !is_typescript_identifier_char(*ch))
        .map_or(0, |(offset, ch)| offset + ch.len_utf8());
    Some(prefix[start..].to_owned())
}

pub(super) fn is_typescript_identifier_char(ch: char) -> bool {
    ch == '_' || ch == '$' || ch.is_ascii_alphanumeric()
}

pub(super) fn typescript_completion_item(name: &str, kind: DeclarationKind) -> LspCompletionItem {
    LspCompletionItem {
        label: name.to_owned(),
        detail: Some(typescript_declaration_kind(kind).to_owned()),
        insert_text: name.to_owned(),
        is_snippet: false,
        sort_text: Some(name.to_owned()),
        additional_edits: Vec::new(),
        replace_from: None,
        kind: Some(typescript_completion_kind(kind)),
    }
}

pub(super) fn typescript_hover_markdown(kind: DeclarationKind, name: &str, path: &Path) -> String {
    format!(
        "**{} `{}`**\n\n`{}`",
        typescript_declaration_kind(kind),
        name,
        path.display()
    )
}

pub(super) fn typescript_declaration_kind(kind: DeclarationKind) -> &'static str {
    match kind {
        DeclarationKind::Function => "function",
        DeclarationKind::Class => "class",
        DeclarationKind::Interface => "interface",
        DeclarationKind::Type => "type",
        DeclarationKind::Enum => "enum",
        DeclarationKind::Namespace => "namespace",
        DeclarationKind::Variable => "variable",
        DeclarationKind::Parameter => "parameter",
    }
}

pub(super) fn typescript_completion_kind(kind: DeclarationKind) -> u64 {
    match kind {
        DeclarationKind::Class => 7,
        DeclarationKind::Interface => 8,
        DeclarationKind::Enum => 13,
        DeclarationKind::Function => 3,
        DeclarationKind::Variable | DeclarationKind::Parameter => 6,
        DeclarationKind::Type | DeclarationKind::Namespace => 9,
    }
}

pub(super) fn typescript_outline_symbol(
    declaration: &Declaration,
    uri: &str,
) -> DocumentSymbolInfo {
    let kind = match declaration.kind {
        DeclarationKind::Function => 12,
        DeclarationKind::Class => 5,
        DeclarationKind::Interface => 11,
        DeclarationKind::Type => 26,
        DeclarationKind::Enum => 10,
        DeclarationKind::Namespace => 3,
        DeclarationKind::Variable => 13,
        DeclarationKind::Parameter => 13,
    };
    DocumentSymbolInfo {
        uri: Some(uri.to_owned()),
        name: declaration.name.clone(),
        kind,
        range: LspRange {
            start_line: declaration.range.start.line as u64,
            start_character: declaration.range.start.col as u64,
            end_line: declaration.range.end.line as u64,
            end_character: declaration.range.end.col as u64,
        },
        children: Vec::new(),
    }
}
