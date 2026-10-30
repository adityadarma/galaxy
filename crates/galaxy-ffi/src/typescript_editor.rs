use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Component, Path, PathBuf};

use galaxy_core::analysis::{
    CompletionItem as LspCompletionItem, DocumentSymbolInfo, Range as LspRange, RenameEdit,
};
use galaxy_core::javascript::PackageImports;
use galaxy_core::typescript::{Declaration, DeclarationKind, DocumentIndex, ImportKind};

use crate::typescript_navigation::{
    TypeScriptNavigationCache, typescript_dependency_packages, typescript_package_exports,
};
use std::sync::Mutex;

/// Shared built-in completion cap. Engines may collect more candidates, but
/// the FFI never hands more than this to the UI ranking path.
const MAX_TYPESCRIPT_COMPLETIONS: usize = 200;
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

/// What one workspace file contributes: importable exports and global names.
#[derive(Debug, Default)]
struct WorkspaceFile {
    exports: Vec<WorkspaceExport>,
    globals: Vec<String>,
}

/// FFI owns disk-backed workspace state; core supplies the per-file parser.
#[derive(Debug, Default)]
pub(super) struct TypeScriptWorkspaceExports {
    roots: BTreeMap<PathBuf, BTreeMap<String, WorkspaceFile>>,
    /// Per root, the global names of `lib`, `@types` and declaration files;
    /// `None` when they cannot be known (see `typescript_globals`).
    ambient: BTreeMap<PathBuf, Option<HashSet<String>>>,
    generation: u64,
}

impl TypeScriptWorkspaceExports {
    pub(super) fn clear(&mut self) {
        self.roots.clear();
        self.ambient.clear();
        self.generation += 1;
    }

    pub(super) fn set_ambient(&mut self, root: &Path, names: Option<HashSet<String>>) {
        self.generation += 1;
        self.ambient.insert(root.to_path_buf(), names);
    }

    /// Whether `name` is certainly not a global for `document`'s project.
    /// `None` when the project's globals are not known.
    pub(super) fn is_unknown_global(&self, document: &Path, name: &str) -> Option<bool> {
        let (root, files) = self
            .roots
            .iter()
            .filter(|(root, _)| document.strip_prefix(root).is_ok())
            .max_by_key(|(root, _)| root.components().count())?;
        let ambient = self.ambient.get(root)?.as_ref()?;
        Some(
            !ambient.contains(name)
                && !files
                    .values()
                    .any(|file| file.globals.iter().any(|global| global == name)),
        )
    }

    pub(super) fn refresh(&mut self, root: &Path, paths: &[String]) {
        self.generation += 1;
        let mut files = BTreeMap::new();
        for path in paths
            .iter()
            .filter(|path| is_indexed_path(path))
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
            files.insert(path.clone(), workspace_file(&full_path, &text, path));
        }
        // A name resolving to more than one module cannot be imported safely.
        if !self.roots.contains_key(root) && self.roots.len() >= MAX_WORKSPACE_TYPESCRIPT_ROOTS {
            self.roots.pop_first();
        }
        self.roots.insert(root.to_path_buf(), files);
    }

    /// Updates only watcher-reported source files. The flattened completion
    /// view remains derived at query time so duplicate exports stay rejected.
    pub(super) fn apply_changes(&mut self, root: &Path, paths: &[String]) {
        let Some(files) = self.roots.get_mut(root) else {
            return;
        };
        self.generation += 1;
        for path in paths.iter().filter(|path| is_indexed_path(path)) {
            let full_path = root.join(path);
            match std::fs::read_to_string(&full_path) {
                Ok(text) if text.len() as u64 <= MAX_WORKSPACE_TYPESCRIPT_FILE_BYTES => {
                    files.insert(path.clone(), workspace_file(&full_path, &text, path));
                }
                _ => {
                    files.remove(path);
                }
            }
        }
    }

    /// Bumped whenever the index changes, so cached diagnostics that depend
    /// on it can be recomputed.
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    /// For each name in `names` that a single workspace module exports, the
    /// import a quick fix would add: `(name, specifier)`. The specifier
    /// follows the same rules as completion auto-imports.
    pub(super) fn import_suggestions(
        &self,
        document: &Path,
        names: &BTreeSet<&str>,
    ) -> BTreeMap<String, String> {
        let mut suggestions = BTreeMap::new();
        if names.is_empty() {
            return suggestions;
        }
        let Some((root, exports)) = self.exports_for(document) else {
            return suggestions;
        };
        let Ok(document_relative) = document.strip_prefix(root) else {
            return suggestions;
        };
        let package_imports = PackageImports::detect(document);
        for export in exports {
            if !names.contains(export.name.as_str()) || Path::new(&export.path) == document_relative
            {
                continue;
            }
            if let Some(specifier) = package_imports
                .as_ref()
                .and_then(|imports| imports.specifier_for(&root.join(&export.path)))
                .or_else(|| workspace_module_specifier(document_relative, &export.path))
            {
                suggestions.insert(export.name, specifier);
            }
        }
        suggestions
    }

    fn exports_for(&self, path: &Path) -> Option<(&Path, Vec<WorkspaceExport>)> {
        self.roots
            .iter()
            .filter(|(root, _)| path.strip_prefix(root).is_ok())
            .max_by_key(|(root, _)| root.components().count())
            .map(|(root, exports)| {
                let mut by_name = BTreeMap::<String, Vec<WorkspaceExport>>::new();
                for file in exports.values() {
                    for export in &file.exports {
                        by_name
                            .entry(export.name.clone())
                            .or_default()
                            .push(export.clone());
                    }
                }
                let unique = by_name
                    .into_values()
                    .filter_map(|exports| {
                        (exports.len() == 1).then(|| exports.into_iter().next().unwrap())
                    })
                    .collect::<Vec<_>>();
                (root.as_path(), unique)
            })
    }
}

fn workspace_file(path: &Path, text: &str, relative: &str) -> WorkspaceFile {
    let index = DocumentIndex::parse(path, text);
    // Declaration files hold globals, not modules to import from.
    let exports = if relative.ends_with(".d.ts") {
        Vec::new()
    } else {
        index
            .declarations
            .iter()
            .filter(|item| item.exported)
            .map(|declaration| WorkspaceExport {
                name: declaration.name.clone(),
                kind: declaration.kind,
                path: relative.to_owned(),
                default: declaration.default,
            })
            .collect()
    };
    WorkspaceFile {
        exports,
        globals: index.global_names,
    }
}

#[allow(clippy::too_many_arguments)]
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
    if is_this_member_access(text, line, character, &prefix) {
        for member in index
            .local_type_members()
            .filter(|member| member.name.starts_with(&prefix))
        {
            items
                .entry(member.name.clone())
                .or_insert_with(|| LspCompletionItem {
                    label: member.name.clone(),
                    detail: Some("method".into()),
                    insert_text: member.name.clone(),
                    is_snippet: false,
                    sort_text: Some(member.name.clone()),
                    additional_edits: Vec::new(),
                    replace_from: None,
                    kind: Some(2),
                });
        }
    }
    for name in string_member_completions(text, line, character, &prefix) {
        items
            .entry(name.to_owned())
            .or_insert_with(|| LspCompletionItem {
                label: name.to_owned(),
                detail: Some("method - String".into()),
                insert_text: name.to_owned(),
                is_snippet: false,
                sort_text: Some(name.to_owned()),
                additional_edits: Vec::new(),
                replace_from: None,
                kind: Some(2),
            });
    }
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
        // package.json `imports` aliases win over relative paths, as the
        // project asked for them (`#services/auth_service`).
        let package_imports = PackageImports::detect(document_path);
        let quote = preferred_quote(&index, text);
        for export in exports {
            if Path::new(&export.path) == document_relative
                || !export.name.starts_with(&prefix)
                || unavailable.contains(export.name.as_str())
            {
                continue;
            }
            let Some(specifier) = package_imports
                .as_ref()
                .and_then(|imports| imports.specifier_for(&workspace_root.join(&export.path)))
                .or_else(|| workspace_module_specifier(document_relative, &export.path))
            else {
                continue;
            };
            let edit = import_edit(
                &index,
                edit_uri,
                &specifier,
                &export.name,
                export.default,
                quote,
                insertion,
            );
            items
                .entry(export.name.clone())
                .or_insert_with(|| LspCompletionItem {
                    detail: Some(format!(
                        "{} - {}",
                        typescript_declaration_kind(export.kind),
                        export.path
                    )),
                    additional_edits: vec![edit],
                    ..typescript_completion_item(&export.name, export.kind)
                });
        }
    }
    let mut package_specifiers = index
        .imports
        .iter()
        .map(|import| import.specifier.as_str())
        .filter(|specifier| {
            !specifier.starts_with('.')
                && !specifier.starts_with('/')
                && !specifier.starts_with("node:")
        })
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let extension = component_extension.or_else(|| {
        document_path
            .extension()
            .and_then(|extension| extension.to_str())
    });
    for (name, specifier) in framework_component_completions(extension, &prefix) {
        if unavailable.contains(name) {
            continue;
        }
        let edit = import_edit(&index, edit_uri, specifier, name, false, '\'', insertion);
        items
            .entry(name.to_owned())
            .or_insert_with(|| LspCompletionItem {
                detail: Some(format!("function - {specifier}")),
                additional_edits: vec![edit],
                ..typescript_completion_item(name, DeclarationKind::Function)
            });
    }
    if !matches!(extension, Some("vue" | "svelte")) && prefix.len() >= 3 {
        package_specifiers.extend(typescript_dependency_packages(document_path));
    }
    // Vue and Svelte components get their own framework imports above.
    package_specifiers.retain(|specifier| {
        !matches!(
            (extension, specifier.as_str()),
            (Some("vue"), "vue") | (Some("svelte"), "svelte")
        )
    });
    for specifier in package_specifiers {
        for (name, kind) in
            typescript_package_exports(navigation_cache, document_path, &specifier, &prefix)
        {
            if unavailable.contains(name.as_str()) {
                continue;
            }
            let edit = import_edit(&index, edit_uri, &specifier, &name, false, '\'', insertion);
            items
                .entry(name.clone())
                .or_insert_with(|| LspCompletionItem {
                    detail: Some(format!(
                        "{} - {specifier}",
                        typescript_declaration_kind(kind)
                    )),
                    additional_edits: vec![edit],
                    ..typescript_completion_item(&name, kind)
                });
        }
    }
    items
        .into_values()
        .take(MAX_TYPESCRIPT_COMPLETIONS)
        .collect()
}

fn framework_component_completions<'a>(
    extension: Option<&str>,
    prefix: &str,
) -> Vec<(&'a str, &'a str)> {
    let (package, names): (&str, &[&str]) = match extension {
        Some("vue") => (
            "vue",
            &[
                "computed",
                "ref",
                "reactive",
                "readonly",
                "watch",
                "watchEffect",
                "watchPostEffect",
                "watchSyncEffect",
                "toRef",
                "toRefs",
                "unref",
                "isRef",
                "nextTick",
                "onMounted",
                "onUnmounted",
                "onUpdated",
                "onBeforeMount",
                "onBeforeUnmount",
                "onBeforeUpdate",
                "defineProps",
                "defineEmits",
                "defineExpose",
                "defineModel",
                "defineOptions",
                "provide",
                "inject",
                "useTemplateRef",
            ],
        ),
        Some("svelte") => (
            "svelte",
            &[
                "onMount",
                "onDestroy",
                "beforeUpdate",
                "afterUpdate",
                "tick",
                "createEventDispatcher",
            ],
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

fn string_member_completions<'a>(
    text: &str,
    line: usize,
    character: usize,
    prefix: &str,
) -> Vec<&'a str> {
    let Some(line_text) = text.lines().nth(line) else {
        return Vec::new();
    };
    let before = line_text.get(..character).unwrap_or(line_text);
    let Some(receiver) = before
        .strip_suffix(prefix)
        .and_then(|before_prefix| before_prefix.strip_suffix('.'))
        .and_then(|before_dot| {
            before_dot
                .rsplit(|ch: char| !is_typescript_identifier_char(ch))
                .next()
        })
        .filter(|receiver| !receiver.is_empty())
    else {
        return Vec::new();
    };
    let typed_receiver = [format!("{receiver}: string"), format!("{receiver}: String")];
    if !typed_receiver
        .iter()
        .any(|annotation| text.contains(annotation))
    {
        return Vec::new();
    }
    [
        "at",
        "charAt",
        "charCodeAt",
        "codePointAt",
        "concat",
        "endsWith",
        "includes",
        "indexOf",
        "lastIndexOf",
        "localeCompare",
        "match",
        "matchAll",
        "normalize",
        "padEnd",
        "padStart",
        "repeat",
        "replace",
        "replaceAll",
        "search",
        "slice",
        "split",
        "startsWith",
        "substring",
        "toLocaleLowerCase",
        "toLocaleUpperCase",
        "toLowerCase",
        "toString",
        "toUpperCase",
        "trim",
        "trimEnd",
        "trimStart",
        "valueOf",
    ]
    .into_iter()
    .filter(|name| name.starts_with(prefix))
    .collect()
}

fn is_this_member_access(text: &str, line: usize, character: usize, prefix: &str) -> bool {
    text.lines()
        .nth(line)
        .and_then(|line_text| line_text.get(..character))
        .and_then(|before| before.strip_suffix(prefix))
        .is_some_and(|before| before.ends_with("this."))
}

fn is_indexed_path(path: &str) -> bool {
    is_source_path(path) || path.ends_with(".d.ts")
}

fn is_source_path(path: &str) -> bool {
    !path.ends_with(".d.ts")
        && [".ts", ".tsx", ".mts", ".cts"]
            .iter()
            .any(|extension| path.ends_with(extension))
}

/// The edit that imports `name` from `specifier`. Merges into an existing
/// value import of the same module (`import { a } from 'x'` gains `, name`)
/// and falls back to a new statement at `insertion`.
fn import_edit(
    index: &DocumentIndex,
    edit_uri: &str,
    specifier: &str,
    name: &str,
    default: bool,
    quote: char,
    insertion: usize,
) -> RenameEdit {
    let at = |line: usize, character: usize, new_text: String| RenameEdit {
        uri: edit_uri.to_owned(),
        range: LspRange {
            start_line: line as u64,
            start_character: character as u64,
            end_line: line as u64,
            end_character: character as u64,
        },
        new_text,
    };
    let existing = index.imports.iter().find(|import| {
        import.specifier == specifier
            && !import.type_only
            && !import.bindings.is_empty()
            && import
                .bindings
                .iter()
                .all(|binding| binding.kind != ImportKind::Namespace)
    });
    if !default && let Some(import) = existing {
        if let Some(last) = import
            .bindings
            .iter()
            .rfind(|binding| binding.kind == ImportKind::Named)
        {
            return at(last.range.end.line, last.range.end.col, format!(", {name}"));
        }
        // Only a default binding: `import Foo from 'x'` gains `, { name }`.
        if let Some(last) = import.bindings.last() {
            return at(
                last.range.end.line,
                last.range.end.col,
                format!(", {{ {name} }}"),
            );
        }
    }
    let new_text = if default {
        format!("import {name} from {quote}{specifier}{quote};\n")
    } else {
        format!("import {{ {name} }} from {quote}{specifier}{quote};\n")
    };
    at(insertion, 0, new_text)
}

fn import_insertion_point(index: &DocumentIndex, fallback: usize) -> usize {
    index
        .imports
        .iter()
        .map(|import| import.range.end.line + 1)
        .max()
        .unwrap_or(fallback)
}

/// The quote the file's existing imports use; single quotes otherwise.
fn preferred_quote(index: &DocumentIndex, text: &str) -> char {
    let lines = text.lines().collect::<Vec<_>>();
    index
        .imports
        .iter()
        .find_map(|import| {
            let start = import.specifier_range.start;
            let line = lines.get(start.line)?;
            // Columns are UTF-16; the quote opens the literal.
            String::from_utf16(
                &line
                    .encode_utf16()
                    .skip(start.col)
                    .take(1)
                    .collect::<Vec<_>>(),
            )
            .ok()?
            .chars()
            .next()
            .filter(|ch| matches!(ch, '"' | '\''))
        })
        .unwrap_or('\'')
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_delta_replaces_and_removes_workspace_exports() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("model.ts");
        std::fs::write(&file, "export class OldModel {}\n").unwrap();
        let mut exports = TypeScriptWorkspaceExports::default();
        exports.refresh(root.path(), &["model.ts".into()]);
        assert_eq!(
            exports.exports_for(root.path()).unwrap().1[0].name,
            "OldModel"
        );

        std::fs::write(&file, "export class NewModel {}\n").unwrap();
        exports.apply_changes(root.path(), &["model.ts".into()]);
        assert_eq!(
            exports.exports_for(root.path()).unwrap().1[0].name,
            "NewModel"
        );

        std::fs::remove_file(&file).unwrap();
        exports.apply_changes(root.path(), &["model.ts".into()]);
        assert!(exports.exports_for(root.path()).unwrap().1.is_empty());
    }
}
