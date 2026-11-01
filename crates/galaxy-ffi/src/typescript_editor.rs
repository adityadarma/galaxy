use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use galaxy_core::analysis::{
    CompletionItem as LspCompletionItem, DefinitionLocation as LspDefinitionLocation,
    Diagnostic as LspDiagnostic, DiagnosticSeverity, DocumentSymbolInfo, Range as LspRange,
    RenameEdit,
};
use galaxy_core::javascript::PackageImports;
use galaxy_core::typescript::{Declaration, DeclarationKind, DocumentIndex, ImportKind};

use crate::typescript_navigation::{
    TypeScriptNavigationCache, direct_imported_name, module_identity,
    resolve_typescript_definition_import, resolve_typescript_export,
    resolve_typescript_type_export, typescript_dependency_packages, typescript_package_exports,
};
use std::sync::Mutex;

/// Shared built-in completion cap. Engines may collect more candidates, but
/// the FFI never hands more than this to the UI ranking path.
const MAX_TYPESCRIPT_COMPLETIONS: usize = 200;
const MAX_WORKSPACE_TYPESCRIPT_FILES: usize = 10_000;
const MAX_WORKSPACE_TYPESCRIPT_FILE_BYTES: u64 = 1_048_576;
const MAX_WORKSPACE_TYPESCRIPT_ROOTS: usize = 8;
const MAX_REFERENCES_CACHE_ENTRIES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ReferencesCacheKey {
    project_root: Option<PathBuf>,
    uri: String,
    start_line: u64,
    start_character: u64,
    end_line: u64,
    end_character: u64,
}

#[derive(Debug, Clone)]
struct WorkspaceExport {
    name: String,
    kind: DeclarationKind,
    path: String,
    default: bool,
}

type RangeKey = (u64, u64, u64, u64);

fn range_key(range: &LspRange) -> RangeKey {
    (
        range.start_line,
        range.start_character,
        range.end_line,
        range.end_character,
    )
}

/// One import binding of a workspace file, resolved through re-export chains
/// to the exported declaration it refers to.
#[derive(Debug, Clone)]
struct ImportUse {
    /// Position of the binding in `DocumentIndex::imports`.
    import: usize,
    binding: usize,
    target_path: PathBuf,
    target_range: RangeKey,
}

/// Reverse import index of one root: `(module, declaration range)` to the files
/// importing it. Built while indexing, so a references query reads memory and
/// never resolves a module or touches the disk. May hold extra entries for
/// files since re-resolved; queries verify against `WorkspaceFile::uses`.
#[derive(Debug, Default)]
struct ImportIndex {
    reverse: HashMap<(PathBuf, RangeKey), BTreeSet<String>>,
}

/// The parts of a file that decide how other files resolve imports of it.
/// Ranges are excluded: edits shift them without changing resolution.
type ExportSurface = (
    Vec<(String, DeclarationKind, bool)>,
    Vec<(Option<String>, Option<String>, Option<String>, bool, bool)>,
);

fn export_surface(index: &DocumentIndex) -> ExportSurface {
    (
        index
            .declarations
            .iter()
            .filter(|declaration| declaration.exported)
            .map(|declaration| {
                (
                    declaration.name.clone(),
                    declaration.kind,
                    declaration.default,
                )
            })
            .collect(),
        index
            .exports
            .iter()
            .map(|export| {
                (
                    export.exported_name.clone(),
                    export.name.clone(),
                    export.specifier.clone(),
                    export.default,
                    export.type_only,
                )
            })
            .collect(),
    )
}

/// What one workspace file contributes: importable exports and global names.
#[derive(Debug)]
struct WorkspaceFile {
    exports: Vec<WorkspaceExport>,
    globals: Vec<String>,
    index: DocumentIndex,
    /// `None` until the reverse import index has resolved this file.
    uses: Option<Vec<ImportUse>>,
}

/// FFI owns disk-backed workspace state; core supplies the per-file parser.
#[derive(Debug, Default)]
pub(super) struct TypeScriptWorkspaceExports {
    roots: BTreeMap<PathBuf, BTreeMap<String, WorkspaceFile>>,
    /// Per root, the global names of `lib`, `@types` and declaration files;
    /// `None` when they cannot be known (see `typescript_globals`).
    ambient: BTreeMap<PathBuf, Option<HashSet<String>>>,
    generation: u64,
    references_cache: HashMap<ReferencesCacheKey, Vec<LspDefinitionLocation>>,
    import_indexes: BTreeMap<PathBuf, ImportIndex>,
}

impl TypeScriptWorkspaceExports {
    pub(super) fn index_stats(&self, root: &Path) -> galaxy_core::LanguageIndexStats {
        let Some(files) = self.roots.get(root) else {
            return galaxy_core::LanguageIndexStats::default();
        };
        let symbols = files
            .values()
            .map(|file| file.index.declarations.len())
            .sum();
        let imports = files
            .values()
            .flat_map(|file| &file.index.imports)
            .map(|import| import.bindings.len())
            .sum();
        let import_uses = files
            .values()
            .filter_map(|file| file.uses.as_ref())
            .map(Vec::len)
            .sum::<usize>();
        let (reverse_keys, reverse_sites) = self.import_indexes.get(root).map_or((0, 0), |index| {
            (
                index.reverse.len(),
                index.reverse.values().map(BTreeSet::len).sum(),
            )
        });
        galaxy_core::LanguageIndexStats {
            files: files.len(),
            symbols,
            imports,
            reverse_keys,
            reverse_sites,
            // Source text is discarded after parsing; previews read open
            // buffers or disk separately.
            source_bytes: 0,
            heap_lower_bound_bytes: files.len()
                * (std::mem::size_of::<String>()
                    + std::mem::size_of::<WorkspaceFile>()
                    + std::mem::size_of::<DocumentIndex>())
                + symbols * std::mem::size_of::<Declaration>()
                + import_uses * std::mem::size_of::<ImportUse>()
                + reverse_keys
                    * (std::mem::size_of::<PathBuf>()
                        + std::mem::size_of::<RangeKey>()
                        + std::mem::size_of::<BTreeSet<String>>())
                + reverse_sites * std::mem::size_of::<String>(),
        }
    }

    pub(super) fn clear(&mut self) {
        self.roots.clear();
        self.ambient.clear();
        self.references_cache.clear();
        self.import_indexes.clear();
        self.generation += 1;
    }

    pub(super) fn set_ambient(&mut self, root: &Path, names: Option<HashSet<String>>) {
        self.generation += 1;
        self.references_cache.clear();
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

    pub(super) fn build(root: &Path, paths: &[String]) -> Self {
        Self::build_with_progress(root, paths, |_, _| {})
    }

    pub(super) fn build_with_progress(
        root: &Path,
        paths: &[String],
        mut progress: impl FnMut(u64, u64),
    ) -> Self {
        let mut files = BTreeMap::new();
        let total = paths.len() as u64;
        let mut indexed = 0;
        for (offset, path) in paths.iter().enumerate() {
            if is_indexed_path(path) && indexed < MAX_WORKSPACE_TYPESCRIPT_FILES {
                indexed += 1;
                let full_path = root.join(path);
                if let Ok(metadata) = std::fs::metadata(&full_path)
                    && metadata.len() <= MAX_WORKSPACE_TYPESCRIPT_FILE_BYTES
                    && let Ok(text) = std::fs::read_to_string(&full_path)
                {
                    files.insert(path.clone(), workspace_file(&full_path, &text, path));
                }
            }
            let completed = (offset + 1) as u64;
            if completed == total || completed.is_multiple_of(25) {
                progress(completed, total);
            }
        }
        Self {
            roots: BTreeMap::from([(root.to_path_buf(), files)]),
            ambient: BTreeMap::new(),
            generation: 1,
            references_cache: HashMap::new(),
            import_indexes: BTreeMap::new(),
        }
    }

    pub(super) fn install(&mut self, root: &Path, mut built: Self) {
        self.generation += 1;
        self.references_cache.clear();
        let files = built.roots.remove(root).unwrap_or_default();
        // A name resolving to more than one module cannot be imported safely.
        if !self.roots.contains_key(root)
            && self.roots.len() >= MAX_WORKSPACE_TYPESCRIPT_ROOTS
            && let Some((evicted, _)) = self.roots.pop_first()
        {
            self.import_indexes.remove(&evicted);
        }
        match built.import_indexes.remove(root) {
            Some(index) => self.import_indexes.insert(root.to_path_buf(), index),
            None => self.import_indexes.remove(root),
        };
        self.roots.insert(root.to_path_buf(), files);
    }

    pub(super) fn diagnostics(&self) -> Vec<(String, Vec<LspDiagnostic>)> {
        self.roots
            .iter()
            .flat_map(|(root, files)| {
                files.iter().filter_map(move |(relative, file)| {
                    if relative.ends_with(".d.ts") {
                        return None;
                    }
                    let uri = crate::file_uri(&root.join(relative));
                    let mut diagnostics = file
                        .index
                        .diagnostics
                        .iter()
                        .map(|diagnostic| LspDiagnostic {
                            uri: uri.clone(),
                            range: crate::typescript_range(diagnostic.range),
                            severity: DiagnosticSeverity::Error,
                            message: diagnostic.message.clone(),
                            source: Some("galaxy".into()),
                            code: None,
                            unnecessary: false,
                        })
                        .collect::<Vec<_>>();
                    diagnostics.extend(file.index.type_diagnostics.iter().map(|diagnostic| {
                        LspDiagnostic {
                            uri: uri.clone(),
                            range: crate::typescript_range(diagnostic.range),
                            severity: DiagnosticSeverity::Error,
                            message: diagnostic.message.clone(),
                            source: Some("galaxy".into()),
                            code: Some(diagnostic.code.clone()),
                            unnecessary: false,
                        }
                    }));
                    Some((uri, diagnostics))
                })
            })
            .collect()
    }

    /// Updates only watcher-reported source files. The flattened completion
    /// view remains derived at query time so duplicate exports stay rejected.
    pub(super) fn apply_changes(&mut self, root: &Path, paths: &[String]) {
        let Some(files) = self.roots.get_mut(root) else {
            return;
        };
        self.generation += 1;
        self.references_cache.clear();
        let mut surface_changed = false;
        let mut touched = Vec::new();
        for path in paths.iter().filter(|path| is_indexed_path(path)) {
            let full_path = root.join(path);
            let before = files.get(path).map(|file| export_surface(&file.index));
            match std::fs::read_to_string(&full_path) {
                Ok(text) if text.len() as u64 <= MAX_WORKSPACE_TYPESCRIPT_FILE_BYTES => {
                    let file = workspace_file(&full_path, &text, path);
                    surface_changed |= before.as_ref() != Some(&export_surface(&file.index));
                    touched.push(module_identity(&full_path));
                    files.insert(path.clone(), file);
                }
                _ => {
                    surface_changed |= files.remove(path).is_some();
                }
            }
        }
        if surface_changed {
            // A new, removed or re-exporting module can change how any file
            // resolves its imports: resolve everything again on next use.
            for file in files.values_mut() {
                file.uses = None;
            }
            self.import_indexes.remove(root);
        } else if let Some(index) = self.import_indexes.get(root) {
            // Same exports, but declaration ranges may have moved: re-resolve
            // the files importing from an edited module.
            let dependents = index
                .reverse
                .iter()
                .filter(|((module, _), _)| touched.contains(module))
                .flat_map(|(_, importers)| importers.iter())
                .cloned()
                .collect::<BTreeSet<_>>();
            for dependent in dependents {
                if let Some(file) = files.get_mut(&dependent) {
                    file.uses = None;
                }
            }
        }
    }

    /// Bumped whenever the index changes, so cached diagnostics that depend
    /// on it can be recomputed.
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) fn has_snapshot_for(&self, document: &Path) -> bool {
        self.roots
            .keys()
            .any(|root| document.strip_prefix(root).is_ok())
    }

    /// Resolves every import binding not yet resolved and records it in the
    /// reverse import index. Run by the indexing worker before `install`, so
    /// queries find the work already done; later edits only re-resolve the
    /// files they touch.
    pub(super) fn resolve_imports(
        &mut self,
        root: &Path,
        navigation_cache: &Mutex<TypeScriptNavigationCache>,
    ) {
        let Some(files) = self.roots.get_mut(root) else {
            return;
        };
        let index = self.import_indexes.entry(root.to_path_buf()).or_default();
        let mut memo = HashMap::new();
        for (relative, file) in files.iter_mut() {
            if file.uses.is_some() {
                continue;
            }
            let uses = resolve_uses(
                &root.join(relative),
                &file.index,
                navigation_cache,
                &mut memo,
            );
            for import_use in &uses {
                index
                    .reverse
                    .entry((import_use.target_path.clone(), import_use.target_range))
                    .or_default()
                    .insert(relative.clone());
            }
            file.uses = Some(uses);
        }
    }

    pub(super) fn references_to(
        &mut self,
        document: &Path,
        target: &LspDefinitionLocation,
        navigation_cache: &Mutex<TypeScriptNavigationCache>,
    ) -> Vec<LspDefinitionLocation> {
        let Some(root) = self
            .roots
            .keys()
            .filter(|root| document.strip_prefix(root).is_ok())
            .max_by_key(|root| root.components().count())
            .cloned()
        else {
            return Vec::new();
        };
        let project_root = galaxy_core::javascript::TypeScriptProject::detect(document)
            .map(|project| module_identity(&project.root));
        let cache_key = ReferencesCacheKey {
            project_root: project_root.clone(),
            uri: target.uri.clone(),
            start_line: target.range.start_line,
            start_character: target.range.start_character,
            end_line: target.range.end_line,
            end_character: target.range.end_character,
        };
        if let Some(cached) = self.references_cache.get(&cache_key) {
            return cached.clone();
        }
        // Files edited since indexing are resolved here, a few at a time.
        self.resolve_imports(&root, navigation_cache);
        let Some(target_path) = target
            .uri
            .strip_prefix("file://")
            .map(Path::new)
            .map(module_identity)
        else {
            return Vec::new();
        };
        let target_range = range_key(&target.range);
        let canonical_root = module_identity(&root);
        let mut found = Vec::new();
        let files = &self.roots[&root];
        let importers = self
            .import_indexes
            .get(&root)
            .and_then(|index| index.reverse.get(&(target_path.clone(), target_range)));
        for relative in importers.into_iter().flatten() {
            let Some(file) = files.get(relative) else {
                continue;
            };
            if project_root
                .as_ref()
                .is_some_and(|project| !canonical_root.join(relative).starts_with(project))
            {
                continue;
            }
            let uri = crate::file_uri(&root.join(relative));
            for import_use in file.uses.iter().flatten() {
                if import_use.target_path != target_path || import_use.target_range != target_range
                {
                    continue;
                }
                let binding = &file.index.imports[import_use.import].bindings[import_use.binding];
                let Some(occurrences) = file.index.import_binding_occurrences(binding) else {
                    continue;
                };
                found.extend(occurrences.occurrences.into_iter().map(|range| {
                    LspDefinitionLocation {
                        uri: uri.clone(),
                        range: crate::typescript_range(range),
                    }
                }));
            }
        }
        if self.references_cache.len() >= MAX_REFERENCES_CACHE_ENTRIES {
            self.references_cache.clear();
        }
        self.references_cache.insert(cache_key, found.clone());
        found
    }

    pub(super) fn build_nearest_project(document: &Path) -> Option<Self> {
        let root = galaxy_core::javascript::TypeScriptProject::detect(document)?.root;
        let mut pending = vec![root.clone()];
        let mut paths = Vec::new();
        while let Some(directory) = pending.pop() {
            let entries = std::fs::read_dir(&directory).ok()?;
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                let file_type = entry.file_type().ok()?;
                if file_type.is_symlink() {
                    continue;
                }
                if file_type.is_dir() {
                    let excluded =
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| {
                                matches!(
                                    name,
                                    "node_modules" | ".git" | "dist" | "build" | "coverage"
                                )
                            });
                    if !excluded {
                        pending.push(path);
                    }
                    continue;
                }
                if file_type.is_file()
                    && is_indexed_path(&path.to_string_lossy())
                    && let Ok(relative) = path.strip_prefix(&root)
                {
                    paths.push(relative.to_string_lossy().replace('\\', "/"));
                    if paths.len() == MAX_WORKSPACE_TYPESCRIPT_FILES {
                        break;
                    }
                }
            }
            if paths.len() == MAX_WORKSPACE_TYPESCRIPT_FILES {
                break;
            }
        }
        Some(Self::build(&root, &paths))
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

/// Resolves each direct import binding of one file to the declaration it
/// refers to. `memo` shares work between files importing the same name.
fn resolve_uses(
    path: &Path,
    index: &DocumentIndex,
    navigation_cache: &Mutex<TypeScriptNavigationCache>,
    memo: &mut HashMap<(PathBuf, String), Option<(PathBuf, RangeKey)>>,
) -> Vec<ImportUse> {
    let mut uses = Vec::new();
    for (import_position, import) in index.imports.iter().enumerate() {
        if !index.import_is_syntax_valid(import) {
            continue;
        }
        let Some(import_path) =
            resolve_typescript_definition_import(navigation_cache, path, &import.specifier)
        else {
            continue;
        };
        for (binding_position, binding) in import.bindings.iter().enumerate() {
            let Some(imported_name) = direct_imported_name(binding) else {
                continue;
            };
            let resolved = memo
                .entry((import_path.clone(), imported_name.to_owned()))
                .or_insert_with(|| {
                    resolve_typescript_type_export(
                        navigation_cache,
                        import_path.clone(),
                        imported_name,
                    )
                    .or_else(|| {
                        resolve_typescript_export(
                            navigation_cache,
                            import_path.clone(),
                            imported_name,
                        )
                    })
                    .map(|(resolved_path, declaration)| {
                        (
                            module_identity(&resolved_path),
                            range_key(&crate::typescript_range(declaration.range)),
                        )
                    })
                });
            if let Some((target_path, target_range)) = resolved.clone() {
                uses.push(ImportUse {
                    import: import_position,
                    binding: binding_position,
                    target_path,
                    target_range,
                });
            }
        }
    }
    uses
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
        globals: index.global_names.clone(),
        index,
        uses: None,
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
    fn workspace_build_reports_throttled_realtime_progress() {
        let root = tempfile::tempdir().unwrap();
        let paths = (0..53)
            .map(|index| format!("file-{index}.txt"))
            .collect::<Vec<_>>();
        let mut progress = Vec::new();

        let _ = TypeScriptWorkspaceExports::build_with_progress(
            root.path(),
            &paths,
            |completed, total| progress.push((completed, total)),
        );

        assert_eq!(progress, vec![(25, 53), (50, 53), (53, 53)]);
    }

    #[test]
    fn disk_delta_replaces_and_removes_workspace_exports() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("model.ts");
        std::fs::write(&file, "export class OldModel {}\n").unwrap();
        let mut exports = TypeScriptWorkspaceExports::default();
        let built = TypeScriptWorkspaceExports::build(root.path(), &["model.ts".into()]);
        exports.install(root.path(), built);
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

    #[test]
    fn installed_workspace_snapshot_is_available_even_without_matching_references() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("packages/cli/src/env.ts");
        let outside = tempfile::tempdir().unwrap();
        let mut exports = TypeScriptWorkspaceExports::default();

        assert!(!exports.has_snapshot_for(&nested));
        exports.install(
            root.path(),
            TypeScriptWorkspaceExports::build(root.path(), &[]),
        );

        assert!(exports.has_snapshot_for(&nested));
        assert!(!exports.has_snapshot_for(&outside.path().join("env.ts")));
    }

    #[test]
    fn references_cache_is_reused_and_invalidated_by_workspace_changes() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("package.json"), "{}\n").unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        let declaration_path = root.path().join("src/commands.ts");
        let importer_path = root.path().join("src/main.ts");
        let declaration_text = "export const Commands = {};\n";
        std::fs::write(&declaration_path, declaration_text).unwrap();
        std::fs::write(
            &importer_path,
            "import { Commands } from './commands';\nCommands;\n",
        )
        .unwrap();
        let mut exports = TypeScriptWorkspaceExports::build(
            root.path(),
            &["src/commands.ts".into(), "src/main.ts".into()],
        );
        let declaration =
            DocumentIndex::parse(&declaration_path, declaration_text).declarations[0].range;
        let target = LspDefinitionLocation {
            uri: crate::file_uri(&declaration_path),
            range: crate::typescript_range(declaration),
        };
        let navigation = Mutex::new(TypeScriptNavigationCache::default());

        let first = exports.references_to(&declaration_path, &target, &navigation);
        assert_eq!(first.len(), 2);
        assert_eq!(exports.references_cache.len(), 1);
        assert_eq!(
            exports.references_to(&declaration_path, &target, &navigation),
            first
        );
        assert_eq!(exports.references_cache.len(), 1);

        std::fs::write(&importer_path, "export {};\n").unwrap();
        exports.apply_changes(root.path(), &["src/main.ts".into()]);
        assert!(exports.references_cache.is_empty());
        assert!(
            exports
                .references_to(&declaration_path, &target, &navigation)
                .is_empty()
        );
    }

    fn target_of(path: &Path, text: &str, name: &str) -> LspDefinitionLocation {
        let declaration = DocumentIndex::parse(path, text)
            .declarations
            .into_iter()
            .find(|declaration| declaration.name == name)
            .unwrap();
        LspDefinitionLocation {
            uri: crate::file_uri(path),
            range: crate::typescript_range(declaration.range),
        }
    }

    #[test]
    fn reverse_import_index_answers_without_reading_importers() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("package.json"), "{}\n").unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        let host = root.path().join("src/host.ts");
        let host_text = "export function createHost() {}\n";
        std::fs::write(&host, host_text).unwrap();
        let importer = root.path().join("src/main.ts");
        std::fs::write(
            &importer,
            "import { createHost } from './host';\ncreateHost();\n",
        )
        .unwrap();
        let paths = ["src/host.ts".into(), "src/main.ts".into()];
        let mut exports = TypeScriptWorkspaceExports::build(root.path(), &paths);
        let navigation = Mutex::new(TypeScriptNavigationCache::default());
        exports.resolve_imports(root.path(), &navigation);

        // Nothing but memory may be needed to answer once indexing is done.
        std::fs::remove_file(&importer).unwrap();
        navigation.lock().unwrap().clear();
        let target = target_of(&host, host_text, "createHost");
        assert_eq!(exports.references_to(&host, &target, &navigation).len(), 2);
        let stats = exports.index_stats(root.path());
        assert_eq!(stats.files, 2);
        assert_eq!(stats.imports, 1);
        assert_eq!(stats.reverse_keys, 1);
        assert_eq!(stats.reverse_sites, 1);
        assert_eq!(stats.source_bytes, 0, "TypeScript source is not retained");
        assert!(stats.heap_lower_bound_bytes > 0);
    }

    #[test]
    fn reverse_import_index_follows_edits_that_move_a_declaration() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("package.json"), "{}\n").unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        let host = root.path().join("src/host.ts");
        let before = "export function createHost() {}\n";
        std::fs::write(&host, before).unwrap();
        std::fs::write(
            root.path().join("src/main.ts"),
            "import { createHost } from './host';\ncreateHost();\n",
        )
        .unwrap();
        let paths = ["src/host.ts".into(), "src/main.ts".into()];
        let mut exports = TypeScriptWorkspaceExports::build(root.path(), &paths);
        let navigation = Mutex::new(TypeScriptNavigationCache::default());
        exports.resolve_imports(root.path(), &navigation);
        let old_target = target_of(&host, before, "createHost");
        assert_eq!(
            exports.references_to(&host, &old_target, &navigation).len(),
            2
        );

        // A comment above shifts the declaration; the exports are unchanged.
        let after = "// moved\nexport function createHost() {}\n";
        std::fs::write(&host, after).unwrap();
        exports.apply_changes(root.path(), &["src/host.ts".into()]);
        let new_target = target_of(&host, after, "createHost");
        assert_eq!(
            exports.references_to(&host, &new_target, &navigation).len(),
            2
        );
        assert!(
            exports
                .references_to(&host, &old_target, &navigation)
                .is_empty()
        );
    }

    #[test]
    fn reverse_import_index_follows_new_reexports_and_removed_importers() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("package.json"), "{}\n").unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        let host = root.path().join("src/host.ts");
        let text = "export const Host = 1;\n";
        std::fs::write(&host, text).unwrap();
        let barrel = root.path().join("src/index.ts");
        std::fs::write(&barrel, "export {};\n").unwrap();
        let main = root.path().join("src/main.ts");
        std::fs::write(&main, "import { Host } from './index';\nHost;\n").unwrap();
        let paths = [
            "src/host.ts".into(),
            "src/index.ts".into(),
            "src/main.ts".into(),
        ];
        let mut exports = TypeScriptWorkspaceExports::build(root.path(), &paths);
        let navigation = Mutex::new(TypeScriptNavigationCache::default());
        exports.resolve_imports(root.path(), &navigation);
        let target = target_of(&host, text, "Host");
        assert!(
            exports
                .references_to(&host, &target, &navigation)
                .is_empty()
        );

        // The barrel now re-exports Host, so main.ts reaches the declaration.
        std::fs::write(&barrel, "export { Host } from './host';\n").unwrap();
        exports.apply_changes(root.path(), &["src/index.ts".into()]);
        assert_eq!(exports.references_to(&host, &target, &navigation).len(), 2);

        std::fs::remove_file(&main).unwrap();
        exports.apply_changes(root.path(), &["src/main.ts".into()]);
        assert!(
            exports
                .references_to(&host, &target, &navigation)
                .is_empty()
        );
    }
}
