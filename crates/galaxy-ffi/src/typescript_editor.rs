use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use galaxy_core::analysis::{
    CompletionItem as LspCompletionItem, DefinitionLocation as LspDefinitionLocation,
    Diagnostic as LspDiagnostic, DiagnosticSeverity, DocumentSymbolInfo, Range as LspRange,
    RenameEdit,
};
use galaxy_core::index_scheduler::shared_index_scheduler;
use galaxy_core::javascript::PackageImports;
use galaxy_core::typescript::{Declaration, DeclarationKind, DocumentIndex, ImportKind};

use crate::typescript_navigation::{
    TypeScriptNavigationCache, direct_imported_name, module_identity,
    resolve_typescript_definition_import, resolve_typescript_export_with_indexes,
    resolve_typescript_type_export_with_indexes, typescript_dependency_packages,
    typescript_package_exports,
};
use std::sync::{Arc, Mutex};

/// Shared built-in completion cap. Engines may collect more candidates, but
/// the FFI never hands more than this to the UI ranking path.
const MAX_TYPESCRIPT_COMPLETIONS: usize = 200;
const MAX_WORKSPACE_TYPESCRIPT_FILES: usize = 10_000;
const MAX_WORKSPACE_TYPESCRIPT_FILE_BYTES: u64 = 1_048_576;
const MAX_WORKSPACE_TYPESCRIPT_ROOTS: usize = 8;
const MAX_REFERENCES_CACHE_ENTRIES: usize = 256;
const PARALLEL_WORKSPACE_INDEX_THRESHOLD: usize = 64;
const TYPESCRIPT_CACHE_SCHEMA: u32 = 2;
#[cfg_attr(not(test), allow(dead_code))]
const IMPORT_RESOLUTION_BATCH_SIZE: usize = 32;

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
#[allow(dead_code)]
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
#[derive(Debug, Default, Clone)]
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
#[derive(Debug, Clone)]
struct WorkspaceFile {
    exports: Vec<WorkspaceExport>,
    globals: Vec<String>,
    index: Arc<DocumentIndex>,
    /// `None` until the reverse import index has resolved this file.
    uses: Option<Vec<ImportUse>>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedWorkspace {
    schema: u32,
    root: PathBuf,
    files: BTreeMap<String, CachedWorkspaceFile>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedWorkspaceFile {
    size: u64,
    modified_nanos: u128,
    index: DocumentIndex,
}

/// FFI owns disk-backed workspace state; core supplies the per-file parser.
#[derive(Debug, Default, Clone)]
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
        let total = paths.len() as u64;
        let cache_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let mut cached = load_typescript_cache(root)
            .filter(|cache| cache.schema == TYPESCRIPT_CACHE_SCHEMA && cache.root == cache_root)
            .map(|cache| cache.files)
            .unwrap_or_default();
        let candidates = paths
            .iter()
            .enumerate()
            .filter(|(_, path)| is_indexed_path(path))
            .take(MAX_WORKSPACE_TYPESCRIPT_FILES)
            .map(|(offset, path)| {
                let cached = cached.remove(path).filter(|cached| {
                    workspace_file_metadata(&root.join(path))
                        .is_some_and(|metadata| metadata == (cached.size, cached.modified_nanos))
                });
                (offset, path.clone(), cached.map(|cached| cached.index))
            })
            .collect::<Vec<_>>();
        let mut files = BTreeMap::new();
        if candidates.len() < PARALLEL_WORKSPACE_INDEX_THRESHOLD {
            let mut next_candidate = 0;
            for (offset, path) in paths.iter().enumerate() {
                if candidates
                    .get(next_candidate)
                    .is_some_and(|(candidate_offset, _, _)| *candidate_offset == offset)
                {
                    let (_, _, cached) = &candidates[next_candidate];
                    next_candidate += 1;
                    if let Some(file) = cached
                        .clone()
                        .map(|index| workspace_file_from_index(path, index))
                        .or_else(|| read_workspace_file(root, path))
                    {
                        files.insert(path.clone(), file);
                    }
                }
                let completed = (offset + 1) as u64;
                if completed == total || completed.is_multiple_of(25) {
                    progress(completed, total);
                }
            }
        } else {
            let skipped = paths.len().saturating_sub(candidates.len()) as u64;
            let resolved = shared_index_scheduler().map_ordered_with_progress(
                candidates,
                |(_, path, cached)| {
                    cached
                        .map(|index| workspace_file_from_index(&path, index))
                        .or_else(|| read_workspace_file(root, &path))
                        .map(|file| (path, file))
                },
                |completed, _| {
                    let completed = skipped + completed as u64;
                    if completed == total || completed.is_multiple_of(25) {
                        progress(completed, total);
                    }
                },
            );
            for (path, file) in resolved.into_iter().flatten() {
                files.insert(path, file);
            }
        }
        let built = Self {
            roots: BTreeMap::from([(root.to_path_buf(), files)]),
            ambient: BTreeMap::new(),
            generation: 1,
            references_cache: HashMap::new(),
            import_indexes: BTreeMap::new(),
        };
        if let Err(error) = built.save_cache(root) {
            tracing::warn!(root = %root.display(), %error, "failed to save TypeScript workspace cache");
        }
        built
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
        if let Err(error) = self.save_cache(root) {
            tracing::warn!(root = %root.display(), %error, "failed to update TypeScript workspace cache");
        }
    }

    fn save_cache(&self, root: &Path) -> io::Result<()> {
        let Some(files) = self.roots.get(root) else {
            return Ok(());
        };
        let files = files
            .iter()
            .filter_map(|(relative, file)| {
                let (size, modified_nanos) = workspace_file_metadata(&root.join(relative))?;
                Some((
                    relative.clone(),
                    CachedWorkspaceFile {
                        size,
                        modified_nanos,
                        index: (*file.index).clone(),
                    },
                ))
            })
            .collect();
        save_typescript_cache(
            root,
            &CachedWorkspace {
                schema: TYPESCRIPT_CACHE_SCHEMA,
                root: root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
                files,
            },
        )
    }

    /// Bumped whenever the index changes, so cached diagnostics that depend
    /// on it can be recomputed.
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn unresolved_reference_snapshot(&self, document: &Path) -> Option<(PathBuf, Self)> {
        let (root, files) = self
            .roots
            .iter()
            .filter(|(root, _)| document.strip_prefix(root).is_ok())
            .max_by_key(|(root, _)| root.components().count())?;
        files
            .values()
            .any(|file| file.uses.is_none())
            .then(|| (root.clone(), self.clone()))
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
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn resolve_imports(
        &mut self,
        root: &Path,
        navigation_cache: &Mutex<TypeScriptNavigationCache>,
    ) {
        self.resolve_imports_with_workers(root, navigation_cache, usize::MAX);
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn resolve_imports_with_workers(
        &mut self,
        root: &Path,
        navigation_cache: &Mutex<TypeScriptNavigationCache>,
        max_workers: usize,
    ) {
        let Some(files) = self.roots.get_mut(root) else {
            return;
        };
        let pending = files
            .iter()
            .filter(|(_, file)| file.uses.is_none())
            .map(|(relative, file)| (relative.clone(), &file.index))
            .collect::<Vec<_>>();
        let workspace_indexes = files
            .values()
            .map(|file| (module_identity(&file.index.path), Arc::clone(&file.index)))
            .collect::<HashMap<_, _>>();
        let resolved = if pending.len() < PARALLEL_WORKSPACE_INDEX_THRESHOLD {
            let mut memo = HashMap::new();
            pending
                .into_iter()
                .map(|(relative, file_index)| {
                    let uses = resolve_uses(
                        &root.join(&relative),
                        file_index,
                        navigation_cache,
                        &workspace_indexes,
                        &mut memo,
                    );
                    (relative, uses)
                })
                .collect::<Vec<_>>()
        } else {
            let mut batches = Vec::new();
            let mut batch = Vec::with_capacity(IMPORT_RESOLUTION_BATCH_SIZE);
            for item in pending {
                batch.push(item);
                if batch.len() == IMPORT_RESOLUTION_BATCH_SIZE {
                    batches.push(std::mem::take(&mut batch));
                    batch = Vec::with_capacity(IMPORT_RESOLUTION_BATCH_SIZE);
                }
            }
            if !batch.is_empty() {
                batches.push(batch);
            }
            let resolved_batches =
                shared_index_scheduler().map_ordered_limited(batches, max_workers, |batch| {
                    let mut memo = HashMap::new();
                    let local_cache = Mutex::new(TypeScriptNavigationCache::default());
                    (
                        batch
                            .into_iter()
                            .map(|(relative, file_index)| {
                                let uses = resolve_uses(
                                    &root.join(&relative),
                                    file_index,
                                    &local_cache,
                                    &workspace_indexes,
                                    &mut memo,
                                );
                                (relative, uses)
                            })
                            .collect::<Vec<_>>(),
                        local_cache.into_inner().unwrap_or_else(|e| e.into_inner()),
                    )
                });
            let mut resolved = Vec::new();
            for (batch, local_cache) in resolved_batches {
                navigation_cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .merge(local_cache);
                resolved.extend(batch);
            }
            resolved
        };
        let index = self.import_indexes.entry(root.to_path_buf()).or_default();
        for (relative, uses) in resolved {
            for import_use in &uses {
                index
                    .reverse
                    .entry((import_use.target_path.clone(), import_use.target_range))
                    .or_default()
                    .insert(relative.clone());
            }
            if let Some(file) = files.get_mut(&relative) {
                file.uses = Some(uses);
            }
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn merge_resolved_imports(&mut self, root: &Path, mut resolved: Self) {
        let Some(resolved_files) = resolved.roots.remove(root) else {
            return;
        };
        let Some(files) = self.roots.get_mut(root) else {
            return;
        };
        let index = self.import_indexes.entry(root.to_path_buf()).or_default();
        let mut changed = false;
        for (relative, resolved_file) in resolved_files {
            let Some(uses) = resolved_file.uses else {
                continue;
            };
            let Some(file) = files.get_mut(&relative) else {
                continue;
            };
            if file.uses.is_some() || !Arc::ptr_eq(&file.index, &resolved_file.index) {
                continue;
            }
            for import_use in &uses {
                index
                    .reverse
                    .entry((import_use.target_path.clone(), import_use.target_range))
                    .or_default()
                    .insert(relative.clone());
            }
            file.uses = Some(uses);
            changed = true;
        }
        if changed {
            self.generation += 1;
            self.references_cache.clear();
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
        let workspace_indexes = files
            .values()
            .map(|file| (module_identity(&file.index.path), Arc::clone(&file.index)))
            .collect::<HashMap<_, _>>();
        let Some(target_index) = workspace_indexes.get(&target_path) else {
            return Vec::new();
        };
        let Some(target_declaration) = target_index.declarations.iter().find(|declaration| {
            range_key(&crate::typescript_range(declaration.range)) == target_range
        }) else {
            return Vec::new();
        };
        let mut candidate_names = HashSet::from([target_declaration.name.clone()]);
        if target_declaration.default {
            candidate_names.insert("default".into());
        }
        loop {
            let before = candidate_names.len();
            for file in files.values() {
                for export in &file.index.exports {
                    if export
                        .name
                        .as_ref()
                        .is_some_and(|name| candidate_names.contains(name))
                        && let Some(exported_name) = &export.exported_name
                    {
                        candidate_names.insert(exported_name.clone());
                    }
                }
            }
            if candidate_names.len() == before {
                break;
            }
        }
        let mut memo = HashMap::new();
        for (relative, file) in files {
            if project_root
                .as_ref()
                .is_some_and(|project| !canonical_root.join(relative).starts_with(project))
            {
                continue;
            }
            let uri = crate::file_uri(&root.join(relative));
            for import in &file.index.imports {
                if !file.index.import_is_syntax_valid(import) {
                    continue;
                }
                let candidates = import
                    .bindings
                    .iter()
                    .filter_map(|binding| {
                        let imported_name = direct_imported_name(binding)?;
                        candidate_names
                            .contains(imported_name)
                            .then_some((binding, imported_name))
                    })
                    .collect::<Vec<_>>();
                if candidates.is_empty() {
                    continue;
                }
                let Some(import_path) = resolve_typescript_definition_import(
                    navigation_cache,
                    &root.join(relative),
                    &import.specifier,
                ) else {
                    continue;
                };
                for (binding, imported_name) in candidates {
                    let resolved = memo
                        .entry((import_path.clone(), imported_name.to_owned()))
                        .or_insert_with(|| {
                            resolve_typescript_type_export_with_indexes(
                                navigation_cache,
                                &workspace_indexes,
                                import_path.clone(),
                                imported_name,
                            )
                            .or_else(|| {
                                resolve_typescript_export_with_indexes(
                                    navigation_cache,
                                    &workspace_indexes,
                                    import_path.clone(),
                                    imported_name,
                                )
                            })
                            .map(|(path, declaration)| {
                                (
                                    module_identity(&path),
                                    range_key(&crate::typescript_range(declaration.range)),
                                )
                            })
                        });
                    if resolved.as_ref() != Some(&(target_path.clone(), target_range)) {
                        continue;
                    }
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
#[cfg_attr(not(test), allow(dead_code))]
fn resolve_uses(
    path: &Path,
    index: &DocumentIndex,
    navigation_cache: &Mutex<TypeScriptNavigationCache>,
    workspace_indexes: &HashMap<PathBuf, Arc<DocumentIndex>>,
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
                    resolve_typescript_type_export_with_indexes(
                        navigation_cache,
                        workspace_indexes,
                        import_path.clone(),
                        imported_name,
                    )
                    .or_else(|| {
                        resolve_typescript_export_with_indexes(
                            navigation_cache,
                            workspace_indexes,
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

fn read_workspace_file(root: &Path, relative: &str) -> Option<WorkspaceFile> {
    let path = root.join(relative);
    let metadata = std::fs::metadata(&path).ok()?;
    if metadata.len() > MAX_WORKSPACE_TYPESCRIPT_FILE_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    Some(workspace_file(&path, &text, relative))
}

fn workspace_file_from_index(relative: &str, index: DocumentIndex) -> WorkspaceFile {
    workspace_file_with_index(relative, Arc::new(index))
}

fn workspace_file(path: &Path, text: &str, relative: &str) -> WorkspaceFile {
    workspace_file_with_index(relative, Arc::new(DocumentIndex::parse(path, text)))
}

fn workspace_file_with_index(relative: &str, index: Arc<DocumentIndex>) -> WorkspaceFile {
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

fn workspace_file_metadata(path: &Path) -> Option<(u64, u128)> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified_nanos = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((metadata.len(), modified_nanos))
}

fn typescript_cache_path(root: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    canonical.hash(&mut hasher);
    Some(
        PathBuf::from(home)
            .join("Library/Caches/Galaxy/workspace-index")
            .join(format!("{:016x}.typescript-v1", hasher.finish())),
    )
}

fn load_typescript_cache(root: &Path) -> Option<CachedWorkspace> {
    let bytes = std::fs::read(typescript_cache_path(root)?).ok()?;
    bincode::deserialize(&bytes).ok()
}

fn save_typescript_cache(root: &Path, cache: &CachedWorkspace) -> io::Result<()> {
    let Some(path) = typescript_cache_path(root) else {
        return Ok(());
    };
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("cache path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let bytes = bincode::serialize(cache).map_err(io::Error::other)?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temporary, bytes)?;
    if let Err(error) = std::fs::rename(&temporary, &path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
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
    fn workspace_build_reuses_fresh_persistent_cache() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("model.ts");
        std::fs::write(&source, "export class Model {}\n").unwrap();
        let paths = vec!["model.ts".to_owned()];
        let first = TypeScriptWorkspaceExports::build(root.path(), &paths);
        assert_eq!(
            first.roots[root.path()]["model.ts"].index.declarations[0].name,
            "Model"
        );

        let cache_path = typescript_cache_path(root.path()).unwrap();
        let mut cache: CachedWorkspace =
            bincode::deserialize(&std::fs::read(&cache_path).unwrap()).unwrap();
        cache.files.get_mut("model.ts").unwrap().index.declarations[0].name = "CachedModel".into();
        save_typescript_cache(root.path(), &cache).unwrap();

        let second = TypeScriptWorkspaceExports::build(root.path(), &paths);
        assert_eq!(
            second.roots[root.path()]["model.ts"].index.declarations[0].name,
            "CachedModel",
            "unchanged file metadata should reuse its serialized DocumentIndex"
        );
    }

    #[test]
    fn workspace_build_reparses_stale_cache_entry() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("model.ts");
        let paths = vec!["model.ts".to_owned()];
        std::fs::write(&source, "export class Old {}\n").unwrap();
        let _ = TypeScriptWorkspaceExports::build(root.path(), &paths);

        std::fs::write(&source, "export class NewModelWithDifferentSize {}\n").unwrap();
        let rebuilt = TypeScriptWorkspaceExports::build(root.path(), &paths);

        assert_eq!(
            rebuilt.roots[root.path()]["model.ts"].index.declarations[0].name,
            "NewModelWithDifferentSize"
        );
    }

    #[test]
    fn corrupt_workspace_cache_falls_back_to_fresh_parse() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("model.ts"), "export class Model {}\n").unwrap();
        let cache = typescript_cache_path(root.path()).unwrap();
        // A fresh HOME (as on CI) has no cache folder yet.
        std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
        std::fs::write(cache, b"not bincode").unwrap();

        let built = TypeScriptWorkspaceExports::build(root.path(), &["model.ts".into()]);

        assert_eq!(
            built.roots[root.path()]["model.ts"].index.declarations[0].name,
            "Model"
        );
    }

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
    fn parallel_workspace_build_indexes_every_typescript_file() {
        let root = tempfile::tempdir().unwrap();
        let paths = (0..PARALLEL_WORKSPACE_INDEX_THRESHOLD)
            .map(|index| {
                let path = format!("file-{index}.ts");
                std::fs::write(
                    root.path().join(&path),
                    format!("export const value{index} = {index};\n"),
                )
                .unwrap();
                path
            })
            .collect::<Vec<_>>();
        let mut progress = Vec::new();

        let built = TypeScriptWorkspaceExports::build_with_progress(
            root.path(),
            &paths,
            |completed, total| progress.push((completed, total)),
        );

        assert_eq!(built.roots[root.path()].len(), paths.len());
        assert_eq!(progress, vec![(25, 64), (50, 64), (64, 64)]);
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
    fn reference_snapshot_is_lazy_and_reused_until_a_file_changes() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("model.ts"), "export class Model {}\n").unwrap();
        let document = root.path().join("model.ts");
        let mut exports = TypeScriptWorkspaceExports::build(root.path(), &["model.ts".into()]);

        let (_, mut snapshot) = exports
            .unresolved_reference_snapshot(&document)
            .expect("fresh workspace should defer reference resolution");
        snapshot.resolve_imports_with_workers(
            root.path(),
            &Mutex::new(TypeScriptNavigationCache::default()),
            2,
        );
        exports.merge_resolved_imports(root.path(), snapshot);
        assert!(exports.unresolved_reference_snapshot(&document).is_none());

        exports.apply_changes(root.path(), &["model.ts".into()]);
        assert!(exports.unresolved_reference_snapshot(&document).is_some());
    }

    #[test]
    fn reference_index_merge_does_not_overwrite_a_newer_file() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("model.ts"), "export class Model {}\n").unwrap();
        std::fs::write(
            root.path().join("use.ts"),
            "import { Model } from './model';\nnew Model();\n",
        )
        .unwrap();
        let paths = vec!["model.ts".into(), "use.ts".into()];
        let mut installed = TypeScriptWorkspaceExports::build(root.path(), &paths);
        let mut background = installed.clone();
        installed.apply_changes(root.path(), &["use.ts".into()]);
        background.resolve_imports_with_workers(
            root.path(),
            &Mutex::new(TypeScriptNavigationCache::default()),
            2,
        );

        installed.merge_resolved_imports(root.path(), background);

        assert!(installed.roots[root.path()]["use.ts"].uses.is_none());
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
    fn targeted_references_skip_unrelated_import_resolution() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("package.json"), "{}\n").unwrap();
        let host = root.path().join("host.ts");
        let host_text = "export function createHost() {}\n";
        std::fs::write(&host, host_text).unwrap();
        std::fs::write(
            root.path().join("main.ts"),
            "import { createHost } from './host';\ncreateHost();\n",
        )
        .unwrap();
        let mut paths = vec!["host.ts".into(), "main.ts".into()];
        for index in 0..200 {
            let dependency = format!("dep-{index}.ts");
            let importer = format!("unrelated-{index}.ts");
            std::fs::write(
                root.path().join(&dependency),
                format!("export const Other{index} = {index};\n"),
            )
            .unwrap();
            std::fs::write(
                root.path().join(&importer),
                format!("import {{ Other{index} }} from './dep-{index}';\nOther{index};\n"),
            )
            .unwrap();
            paths.push(dependency);
            paths.push(importer);
        }
        let mut exports = TypeScriptWorkspaceExports::build(root.path(), &paths);
        let navigation = Mutex::new(TypeScriptNavigationCache::default());
        let target = target_of(&host, host_text, "createHost");

        assert_eq!(exports.references_to(&host, &target, &navigation).len(), 2);
        assert_eq!(
            navigation.lock().unwrap().resolutions.len(),
            1,
            "unrelated imports must not enter module resolution"
        );
    }

    #[test]
    fn parallel_import_resolution_indexes_every_importer() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("package.json"), "{}\n").unwrap();
        let host = root.path().join("host.ts");
        let host_text = "export function createHost() {}\n";
        std::fs::write(&host, host_text).unwrap();
        let mut paths = vec!["host.ts".into()];
        for index in 1..PARALLEL_WORKSPACE_INDEX_THRESHOLD {
            let relative = format!("importer-{index}.ts");
            std::fs::write(
                root.path().join(&relative),
                "import { createHost } from './host';\ncreateHost();\n",
            )
            .unwrap();
            paths.push(relative);
        }
        let mut exports = TypeScriptWorkspaceExports::build(root.path(), &paths);
        let navigation = Mutex::new(TypeScriptNavigationCache::default());

        exports.resolve_imports(root.path(), &navigation);

        let target = target_of(&host, host_text, "createHost");
        assert_eq!(
            exports.references_to(&host, &target, &navigation).len(),
            (PARALLEL_WORKSPACE_INDEX_THRESHOLD - 1) * 2
        );
        let stats = exports.index_stats(root.path());
        assert_eq!(stats.imports, PARALLEL_WORKSPACE_INDEX_THRESHOLD - 1);
        assert_eq!(stats.reverse_sites, PARALLEL_WORKSPACE_INDEX_THRESHOLD - 1);
        let navigation = navigation.lock().unwrap();
        assert!(navigation.indexes.is_empty());
        assert!(!navigation.resolutions.is_empty());
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
