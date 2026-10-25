use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use galaxy_core::analysis::{DefinitionLocation as LspDefinitionLocation, Range as LspRange};
use galaxy_core::javascript;
use galaxy_core::position::Utf16Position;
use galaxy_core::typescript::{
    DeclarationKind, DocumentIndex, ImportBinding, ImportKind, TypeMemberTarget,
};

use crate::{file_uri, is_typescript_path, typescript_range};

const MAX_TYPESCRIPT_NAVIGATION_CACHE_ENTRIES: usize = 256;
pub(super) const MAX_TYPESCRIPT_REEXPORT_DEPTH: usize = 32;
const MAX_TYPESCRIPT_WARM_SOURCE_CANDIDATES: usize = 5000;
const MAX_TYPESCRIPT_WARM_IMPORT_TARGETS: usize = 512;
const TYPESCRIPT_WARM_EXCLUDED_DIRECTORIES: &[&str] = &[
    "node_modules",
    ".git",
    ".hg",
    ".svn",
    "target",
    "dist",
    "build",
    ".next",
    "coverage",
    "vendor",
];

#[derive(Clone, PartialEq, Eq)]
pub(super) struct FileFreshness {
    modified: Option<SystemTime>,
    len: u64,
}

impl FileFreshness {
    fn read(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(Self {
            modified: metadata.modified().ok(),
            len: metadata.len(),
        })
    }
}

#[derive(Clone)]
pub(super) struct CachedTypeScriptIndex {
    freshness: FileFreshness,
    index: Arc<DocumentIndex>,
}

#[derive(Clone)]
pub(super) struct CachedTypeScriptResolution {
    pub(super) target: PathBuf,
    dependencies: Vec<(PathBuf, Option<FileFreshness>)>,
}

#[derive(Default)]
pub(super) struct TypeScriptNavigationCache {
    pub(super) indexes: HashMap<PathBuf, CachedTypeScriptIndex>,
    pub(super) resolutions: HashMap<(PathBuf, String), CachedTypeScriptResolution>,
}

impl TypeScriptNavigationCache {
    pub(super) fn index(&mut self, path: &Path) -> Option<Arc<DocumentIndex>> {
        let path = module_identity(path);
        let freshness = FileFreshness::read(&path)?;
        if let Some(cached) = self.indexes.get(&path)
            && cached.freshness == freshness
        {
            return Some(Arc::clone(&cached.index));
        }
        let text = std::fs::read_to_string(&path).ok()?;
        let index = Arc::new(DocumentIndex::parse(&path, &text));
        if self.indexes.len() >= MAX_TYPESCRIPT_NAVIGATION_CACHE_ENTRIES {
            self.indexes.clear();
        }
        self.indexes.insert(
            path,
            CachedTypeScriptIndex {
                freshness,
                index: Arc::clone(&index),
            },
        );
        Some(index)
    }

    pub(super) fn resolve(&mut self, importer: &Path, specifier: &str) -> Option<PathBuf> {
        let importer = module_identity(importer);
        let key = (importer.clone(), specifier.to_owned());
        if let Some(cached) = self.resolutions.get(&key)
            && cached.target.is_file()
            && cached
                .dependencies
                .iter()
                .all(|(path, freshness)| FileFreshness::read(path) == *freshness)
        {
            return Some(cached.target.clone());
        }
        self.resolutions.remove(&key);
        let target = prefer_typescript_declaration(resolve_typescript_import_uncached(
            &importer, specifier,
        )?);
        if !target.is_file() {
            return None;
        }
        let dependencies = typescript_resolution_dependencies(&importer, specifier);
        if self.resolutions.len() >= MAX_TYPESCRIPT_NAVIGATION_CACHE_ENTRIES {
            self.resolutions.clear();
        }
        self.resolutions.insert(
            key,
            CachedTypeScriptResolution {
                target: target.clone(),
                dependencies,
            },
        );
        Some(target)
    }
}

pub(super) fn typescript_resolution_dependencies(
    importer: &Path,
    specifier: &str,
) -> Vec<(PathBuf, Option<FileFreshness>)> {
    let mut dependencies = vec![(importer.to_path_buf(), FileFreshness::read(importer))];
    let mut directory = importer.parent();
    while let Some(path) = directory {
        for name in ["package.json", "tsconfig.json"] {
            let dependency = path.join(name);
            dependencies.push((dependency.clone(), FileFreshness::read(&dependency)));
        }
        directory = path.parent();
    }
    if !javascript::is_relative_typescript_specifier(specifier) && !specifier.starts_with("node:") {
        let package = specifier
            .split('/')
            .take(if specifier.starts_with('@') { 2 } else { 1 })
            .collect::<Vec<_>>()
            .join("/");
        let mut directory = importer.parent();
        while let Some(path) = directory {
            let manifest = path
                .join("node_modules")
                .join(&package)
                .join("package.json");
            dependencies.push((manifest.clone(), FileFreshness::read(&manifest)));
            directory = path.parent();
        }
    }
    dependencies
}

pub(super) fn prefer_typescript_declaration(target: PathBuf) -> PathBuf {
    let declaration = target.with_extension("d.ts");
    if target
        .extension()
        .is_some_and(|extension| extension == "js")
        && declaration.is_file()
    {
        declaration
    } else if target
        .extension()
        .is_some_and(|extension| matches!(extension.to_str(), Some("js" | "mjs" | "cjs")))
    {
        javascript::package_types_declaration(&target).unwrap_or(target)
    } else {
        target
    }
}

pub(super) fn typescript_import_target(
    cache: &Mutex<TypeScriptNavigationCache>,
    importer_path: &str,
    index: &DocumentIndex,
    line: usize,
    character: usize,
) -> Option<(
    LspDefinitionLocation,
    Vec<galaxy_core::typescript::Utf16Range>,
)> {
    let (import, binding, occurrences) = index.imports.iter().find_map(|import| {
        index
            .import_is_syntax_valid(import)
            .then_some(import)
            .and_then(|import| {
                import.bindings.iter().find_map(|binding| {
                    let position = Utf16Position::new(line, character);
                    let occurrences = index.import_binding_occurrences(binding).unwrap_or(
                        galaxy_core::typescript::SymbolOccurrences {
                            name: binding.local_name.clone(),
                            kind: DeclarationKind::Variable,
                            declaration: binding.range,
                            occurrences: vec![binding.range],
                        },
                    );
                    (binding.range.start <= position && position < binding.range.end
                        || occurrences
                            .occurrences
                            .iter()
                            .any(|range| range.start <= position && position < range.end))
                    .then_some((import, binding, occurrences))
                })
            })
    })?;
    let imported_name = direct_imported_name(binding)?;
    let target_path =
        resolve_typescript_definition_import(cache, Path::new(importer_path), &import.specifier)?;
    if !is_typescript_path(&target_path) {
        return None;
    }
    let location = resolve_typescript_type_export(cache, target_path.clone(), imported_name)
        .or_else(|| resolve_typescript_export(cache, target_path.clone(), imported_name))
        .map(|(path, declaration)| LspDefinitionLocation {
            uri: file_uri(&path),
            range: typescript_range(declaration.range),
        })
        .unwrap_or_else(|| module_root_location(&target_path));
    Some((location, occurrences.occurrences))
}

pub(super) fn typescript_type_target(
    cache: &Mutex<TypeScriptNavigationCache>,
    importer_path: &str,
    index: &DocumentIndex,
    line: usize,
    character: usize,
) -> Option<LspDefinitionLocation> {
    let position = Utf16Position::new(line, character);
    let (import, binding) = index.imports.iter().find_map(|import| {
        index
            .import_is_syntax_valid(import)
            .then_some(import)
            .and_then(|import| {
                import
                    .bindings
                    .iter()
                    .find(|binding| {
                        binding.type_only
                            && (binding.range.start <= position && position < binding.range.end
                                || index.import_binding_occurrences(binding).is_some_and(
                                    |occurrences| {
                                        occurrences.occurrences.iter().any(|range| {
                                            range.start <= position && position < range.end
                                        })
                                    },
                                ))
                    })
                    .map(|binding| (import, binding))
            })
    })?;
    let imported_name = binding.imported_name.as_deref()?;
    if !matches!(binding.kind, ImportKind::Named | ImportKind::Default) {
        return None;
    }
    let target =
        resolve_typescript_definition_import(cache, Path::new(importer_path), &import.specifier)?;
    Some(
        resolve_typescript_type_export(cache, target.clone(), imported_name)
            .or_else(|| resolve_typescript_export(cache, target.clone(), imported_name))
            .map(|(path, declaration)| LspDefinitionLocation {
                uri: file_uri(&path),
                range: typescript_range(declaration.range),
            })
            .unwrap_or_else(|| module_root_location(&target)),
    )
}

pub(super) fn typescript_type_member_target(
    cache: &Mutex<TypeScriptNavigationCache>,
    importer_path: &str,
    index: &DocumentIndex,
    line: usize,
    character: usize,
) -> Option<LspDefinitionLocation> {
    let usage = index.type_member_at(line, character)?;
    resolve_typescript_type_member_target(
        cache,
        importer_path,
        index,
        &usage.target,
        &usage.member_name,
    )
}

pub(super) fn typescript_decorator_option_target(
    cache: &Mutex<TypeScriptNavigationCache>,
    importer_path: &str,
    index: &DocumentIndex,
    line: usize,
    character: usize,
) -> Option<LspDefinitionLocation> {
    let usage = index.decorator_option_at(line, character)?;
    let (import, binding) = index.import_binding_by_range(usage.binding_range)?;
    let imported_name = direct_imported_name(binding)?;
    let decorator_path =
        resolve_typescript_definition_import(cache, Path::new(importer_path), &import.specifier)?;
    let (decorator_path, decorator) =
        resolve_typescript_export(cache, decorator_path, imported_name)?;
    let decorator_index = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .index(&decorator_path)?;
    let type_name = match usage.member_name.as_deref() {
        Some(member_name) => {
            decorator_index.callable_member_type_annotation(&decorator.name, member_name)?
        }
        None => decorator_index.callable_type_annotation(&decorator.name)?,
    };
    let (type_path, type_name) =
        resolve_decorator_type(cache, &decorator_path, &decorator_index, type_name)?;
    let member = resolve_decorator_option_member(
        cache,
        &type_path,
        &type_name,
        &usage.key_name,
        0,
        &mut HashSet::new(),
    )?;
    Some(LspDefinitionLocation {
        uri: file_uri(&member.0),
        range: typescript_range(member.1),
    })
}

fn resolve_decorator_type(
    cache: &Mutex<TypeScriptNavigationCache>,
    path: &Path,
    index: &DocumentIndex,
    name: &str,
) -> Option<(PathBuf, String)> {
    if index.decorator_option_type(name).is_some() || index.type_declaration(name).is_some() {
        return Some((path.to_path_buf(), name.to_owned()));
    }
    let imports = index
        .imports
        .iter()
        .flat_map(|import| import.bindings.iter().map(move |binding| (import, binding)))
        .filter(|(_, binding)| {
            binding.local_name == name
                && matches!(binding.kind, ImportKind::Named | ImportKind::Default)
        })
        .collect::<Vec<_>>();
    let [(import, binding)] = imports.as_slice() else {
        return None;
    };
    let imported_name = binding.imported_name.as_deref()?;
    let path = resolve_typescript_definition_import(cache, path, &import.specifier)?;
    let (path, declaration) = resolve_typescript_type_export(cache, path, imported_name)?;
    Some((path, declaration.name))
}

fn resolve_decorator_option_member(
    cache: &Mutex<TypeScriptNavigationCache>,
    path: &Path,
    type_name: &str,
    member_name: &str,
    depth: usize,
    visited: &mut HashSet<(PathBuf, String)>,
) -> Option<(PathBuf, galaxy_core::typescript::Utf16Range)> {
    if depth == MAX_TYPESCRIPT_REEXPORT_DEPTH
        || !visited.insert((module_identity(path), type_name.to_owned()))
    {
        return None;
    }
    let index = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .index(path)?;
    let mut candidates = Vec::new();
    if let Some(option) = index.decorator_option_type(type_name) {
        candidates.extend(
            option
                .members
                .iter()
                .filter(|member| member.name == member_name)
                .map(|member| (path.to_path_buf(), member.range)),
        );
        for reference in &option.references {
            let Some((reference_path, reference_name)) =
                resolve_decorator_type(cache, path, &index, reference)
            else {
                continue;
            };
            if let Some(candidate) = resolve_decorator_option_member(
                cache,
                &reference_path,
                &reference_name,
                member_name,
                depth + 1,
                visited,
            ) && !candidates.contains(&candidate)
            {
                candidates.push(candidate);
            }
        }
    } else if let Some(member) = index.type_member(type_name, member_name) {
        candidates.push((path.to_path_buf(), member.range));
    }
    (candidates.len() == 1).then(|| candidates.remove(0))
}

pub(super) fn resolve_typescript_type_member_target(
    cache: &Mutex<TypeScriptNavigationCache>,
    importer_path: &str,
    index: &DocumentIndex,
    target: &TypeMemberTarget,
    member_name: &str,
) -> Option<LspDefinitionLocation> {
    match target {
        TypeMemberTarget::Inline(members) => {
            unique_type_member_location(members, member_name, Path::new(importer_path))
        }
        TypeMemberTarget::Named(range) => {
            let member = index
                .type_declaration_at(*range)?
                .iter()
                .filter(|member| member.name == member_name)
                .collect::<Vec<_>>();
            let [member] = member.as_slice() else {
                return None;
            };
            Some(LspDefinitionLocation {
                uri: file_uri(Path::new(importer_path)),
                range: typescript_range(member.range),
            })
        }
        member_target @ (TypeMemberTarget::Imported(range)
        | TypeMemberTarget::ImportedValue(range)
        | TypeMemberTarget::SuperImported(range)) => {
            let (import, binding) = index.import_binding_by_range(*range)?;
            let imported_name = binding.imported_name.as_deref()?;
            let is_type = matches!(member_target, TypeMemberTarget::Imported(_));
            if is_type
                && (!binding.type_only
                    || !matches!(binding.kind, ImportKind::Named | ImportKind::Default))
            {
                return None;
            }
            if matches!(target, TypeMemberTarget::SuperImported(_))
                && (binding.type_only
                    || !matches!(binding.kind, ImportKind::Named | ImportKind::Default))
            {
                return None;
            }
            let target = resolve_typescript_definition_import(
                cache,
                Path::new(importer_path),
                &import.specifier,
            )?;
            let (path, declaration) = if is_type {
                resolve_typescript_type_export(cache, target, imported_name)?
            } else {
                resolve_typescript_export(cache, target, imported_name)?
            };
            if matches!(member_target, TypeMemberTarget::ImportedValue(_))
                && declaration.kind != DeclarationKind::Enum
            {
                return None;
            }
            if matches!(member_target, TypeMemberTarget::SuperImported(_))
                && declaration.kind != DeclarationKind::Class
            {
                return None;
            }
            let target_index = cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .index(&path)?;
            let member = target_index.type_member(&declaration.name, member_name)?;
            Some(LspDefinitionLocation {
                uri: file_uri(&path),
                range: typescript_range(member.range),
            })
        }
        TypeMemberTarget::SuperMember {
            binding,
            member_name: property,
        } => {
            let (import, binding) = index.import_binding_by_range(*binding)?;
            let imported_name = binding.imported_name.as_deref()?;
            let superclass_path = resolve_typescript_definition_import(
                cache,
                Path::new(importer_path),
                &import.specifier,
            )?;
            let (superclass_path, superclass) =
                resolve_typescript_export(cache, superclass_path, imported_name)?;
            let superclass_index = cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .index(&superclass_path)?;
            let property = superclass_index.type_member(&superclass.name, property)?;
            let (namespace, type_name) = property.type_reference.as_ref()?;
            let type_import = superclass_index.imports.iter().find(|import| {
                import
                    .bindings
                    .iter()
                    .any(|binding| binding.local_name == *namespace)
            })?;
            let target = resolve_typescript_definition_import(
                cache,
                &superclass_path,
                &type_import.specifier,
            )?;
            let target_index = cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .index(&target)?;
            let member = target_index.type_member(type_name, member_name)?;
            Some(LspDefinitionLocation {
                uri: file_uri(&target),
                range: typescript_range(member.range),
            })
        }
        TypeMemberTarget::SuperMemberCallback {
            binding,
            member_name: property,
            callback_name,
            chain,
        } => {
            let (import, binding) = index.import_binding_by_range(*binding)?;
            let imported_name = binding.imported_name.as_deref()?;
            let superclass_path = resolve_typescript_definition_import(
                cache,
                Path::new(importer_path),
                &import.specifier,
            )?;
            let (superclass_path, superclass) =
                resolve_typescript_export(cache, superclass_path, imported_name)?;
            let superclass_index = cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .index(&superclass_path)?;
            let property = superclass_index.type_member(&superclass.name, property)?;
            let (namespace, type_name) = property.type_reference.as_ref()?;
            let type_import = superclass_index.imports.iter().find(|import| {
                import
                    .bindings
                    .iter()
                    .any(|binding| binding.type_only && binding.local_name == *namespace)
            })?;
            let target = resolve_typescript_definition_import(
                cache,
                &superclass_path,
                &type_import.specifier,
            )?;
            let target_index = cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .index(&target)?;
            let callback = target_index.type_member(type_name, callback_name)?;
            let callback_type = callback.callback_parameter_type.as_deref()?;
            let final_type =
                chain
                    .iter()
                    .try_fold(callback_type.to_owned(), |type_name, method| {
                        target_index
                            .type_member(&type_name, method)?
                            .return_type
                            .clone()
                    })?;
            let member = target_index.type_member(&final_type, member_name)?;
            Some(LspDefinitionLocation {
                uri: file_uri(&target),
                range: typescript_range(member.range),
            })
        }
        TypeMemberTarget::PackageDeclaration {
            specifier,
            type_name,
        } => {
            let path =
                resolve_typescript_definition_import(cache, Path::new(importer_path), specifier)?;
            let target = cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .index(&path)?;
            let member = target.type_member(type_name, member_name)?;
            Some(LspDefinitionLocation {
                uri: file_uri(&path),
                range: typescript_range(member.range),
            })
        }
        TypeMemberTarget::Union(arms) => {
            let mut locations = arms.iter().map(|arm| match arm {
                TypeMemberTarget::Inline(_) => None,
                _ => resolve_typescript_type_member_target(
                    cache,
                    importer_path,
                    index,
                    arm,
                    member_name,
                ),
            });
            let first = locations.next()??;
            locations
                .all(|location| location.as_ref() == Some(&first))
                .then_some(first)
        }
    }
}

fn unique_type_member_location(
    members: &[galaxy_core::typescript::TypeMember],
    member_name: &str,
    path: &Path,
) -> Option<LspDefinitionLocation> {
    let matches = members
        .iter()
        .filter(|member| member.name == member_name)
        .collect::<Vec<_>>();
    let [member] = matches.as_slice() else {
        return None;
    };
    Some(LspDefinitionLocation {
        uri: file_uri(path),
        range: typescript_range(member.range),
    })
}

pub(super) fn module_root_location(path: &Path) -> LspDefinitionLocation {
    LspDefinitionLocation {
        uri: file_uri(path),
        range: LspRange {
            start_line: 0,
            start_character: 0,
            end_line: 0,
            end_character: 0,
        },
    }
}

pub(super) fn resolve_typescript_export(
    cache: &Mutex<TypeScriptNavigationCache>,
    path: PathBuf,
    name: &str,
) -> Option<(PathBuf, galaxy_core::typescript::Declaration)> {
    resolve_typescript_export_inner(cache, path, name, 0, &mut HashSet::new())
}

pub(super) fn resolve_typescript_type_export(
    cache: &Mutex<TypeScriptNavigationCache>,
    path: PathBuf,
    name: &str,
) -> Option<(PathBuf, galaxy_core::typescript::Declaration)> {
    resolve_typescript_type_export_inner(cache, path, name, 0, &mut HashSet::new())
}

fn resolve_typescript_type_export_inner(
    cache: &Mutex<TypeScriptNavigationCache>,
    path: PathBuf,
    name: &str,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
) -> Option<(PathBuf, galaxy_core::typescript::Declaration)> {
    if depth == MAX_TYPESCRIPT_REEXPORT_DEPTH || !visited.insert(module_identity(&path)) {
        return None;
    }
    let index = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .index(&path)?;
    if let Some(declaration) = index.exported_type_declaration(name) {
        return Some((path, declaration.clone()));
    }
    if let Some(declaration) = index.export_assignment_namespace(name) {
        return Some((path, declaration.clone()));
    }
    let candidates = index
        .exports
        .iter()
        .filter_map(|export| {
            (export.type_only && export.exported_name.as_deref() == Some(name))
                .then_some((export.specifier.as_deref()?, export.name.as_deref()?))
        })
        .filter_map(|(specifier, imported_name)| {
            resolve_typescript_definition_import(cache, &path, specifier).and_then(|target| {
                resolve_typescript_type_export_inner(
                    cache,
                    target,
                    imported_name,
                    depth + 1,
                    visited,
                )
            })
        })
        .collect::<Vec<_>>();
    (candidates.len() == 1).then(|| candidates.into_iter().next().unwrap())
}

fn resolve_typescript_export_inner(
    cache: &Mutex<TypeScriptNavigationCache>,
    path: PathBuf,
    name: &str,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
) -> Option<(PathBuf, galaxy_core::typescript::Declaration)> {
    if depth == MAX_TYPESCRIPT_REEXPORT_DEPTH || !visited.insert(module_identity(&path)) {
        return None;
    }
    let index = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .index(&path)?;
    if let Some(declaration) = index.exported_declaration(name) {
        return Some((path, declaration.clone()));
    }
    let mut candidates: Vec<(PathBuf, galaxy_core::typescript::Declaration)> = Vec::new();
    for export in index.exports.iter().filter(|export| !export.type_only) {
        let Some(specifier) = export.specifier.as_deref() else {
            continue;
        };
        let imported_name = if export.exported_name.as_deref() == Some(name) {
            export.name.as_deref()
        } else if export.exported_name.is_none() {
            Some(name)
        } else {
            None
        };
        let Some(imported_name) = imported_name else {
            continue;
        };
        let Some(target) = resolve_typescript_definition_import(cache, &path, specifier) else {
            continue;
        };
        if let Some(candidate) =
            resolve_typescript_export_inner(cache, target, imported_name, depth + 1, visited)
            && !candidates.iter().any(|(candidate_path, declaration)| {
                candidate_path == &candidate.0 && declaration.range == candidate.1.range
            })
        {
            candidates.push(candidate);
        }
    }
    (candidates.len() == 1).then(|| candidates.remove(0))
}

pub(super) fn module_identity(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub(super) fn resolve_typescript_import_uncached(
    importer: &Path,
    specifier: &str,
) -> Option<PathBuf> {
    if javascript::is_relative_typescript_specifier(specifier) {
        return javascript::resolve_relative_import(specifier, importer);
    }
    if specifier.starts_with("node:") {
        return javascript::resolve_node_types_specifier(specifier, importer);
    }
    let project = javascript::TypeScriptProject::detect(importer)?;
    javascript::resolve_typescript_specifier(&project, specifier, importer)
}

pub(super) fn resolve_typescript_definition_import(
    cache: &Mutex<TypeScriptNavigationCache>,
    importer: &Path,
    specifier: &str,
) -> Option<PathBuf> {
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .resolve(importer, specifier)
}

pub(super) fn direct_imported_name(binding: &ImportBinding) -> Option<&str> {
    (!binding.type_only && matches!(binding.kind, ImportKind::Named | ImportKind::Default))
        .then_some(binding.imported_name.as_deref())
        .flatten()
}

/// A JavaScript package, project configuration, or common framework marker
/// opts the workspace into the JavaScript/TypeScript navigation warm-up.
pub(super) fn workspace_detected(root: &Path) -> bool {
    [
        "package.json",
        "tsconfig.json",
        "jsconfig.json",
        // Next.js, Nuxt, NestJS, Remix, Gatsby, Astro, SvelteKit, Vite,
        // Angular, React Native, and Expo.
        "next.config.js",
        "next.config.mjs",
        "next.config.ts",
        "nuxt.config.js",
        "nuxt.config.ts",
        "nest-cli.json",
        "remix.config.js",
        "remix.config.mjs",
        "remix.config.ts",
        "gatsby-config.js",
        "gatsby-config.ts",
        "astro.config.mjs",
        "astro.config.ts",
        "svelte.config.js",
        "svelte.config.ts",
        "vite.config.js",
        "vite.config.ts",
        "angular.json",
        "app.json",
        "app.config.js",
        "app.config.ts",
    ]
    .into_iter()
    .any(|name| root.join(name).is_file())
}

pub(super) fn spawn_warmup(
    root: &Path,
    cache: Arc<Mutex<TypeScriptNavigationCache>>,
) -> Option<std::thread::JoinHandle<()>> {
    let root = root.to_path_buf();
    std::thread::Builder::new()
        .name("typescript-navigation-warmup".into())
        // Keep opening a workspace instant while bounding work in large repositories.
        .spawn(move || warm_typescript_navigation(&root, &cache))
        .ok()
}

fn warm_typescript_navigation(root: &Path, cache: &Mutex<TypeScriptNavigationCache>) {
    let mut sources = vec![root.to_path_buf()];
    let mut seen_imports = HashSet::new();
    let mut candidates = 0;
    let mut warmed = 0;

    while let Some(path) = sources.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if !typescript_warm_excluded_directory(&path)
                    && sources.len() < MAX_TYPESCRIPT_WARM_SOURCE_CANDIDATES
                {
                    sources.push(path);
                }
                continue;
            }
            if !file_type.is_file() || !is_typescript_path(&path) {
                continue;
            }
            candidates += 1;
            if candidates > MAX_TYPESCRIPT_WARM_SOURCE_CANDIDATES {
                return;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let index = DocumentIndex::parse(&path, &text);
            let specifiers = index
                .imports
                .iter()
                .filter(|import| index.import_is_syntax_valid(import))
                .map(|import| import.specifier.as_str())
                .chain(
                    index
                        .exports
                        .iter()
                        .filter_map(|export| export.specifier.as_deref()),
                );
            for specifier in specifiers {
                if warmed == MAX_TYPESCRIPT_WARM_IMPORT_TARGETS {
                    return;
                }
                let importer = module_identity(&path);
                if !seen_imports.insert((importer, specifier.to_owned())) {
                    continue;
                }
                let Some(target) = resolve_typescript_definition_import(cache, &path, specifier)
                else {
                    continue;
                };
                if !is_typescript_path(&target) {
                    continue;
                }
                warmed += 1;
                warm_typescript_declaration_chain(
                    cache,
                    target,
                    0,
                    &mut warmed,
                    &mut HashSet::new(),
                );
            }
        }
    }
}

fn typescript_warm_excluded_directory(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| TYPESCRIPT_WARM_EXCLUDED_DIRECTORIES.contains(&name))
}

fn warm_typescript_declaration_chain(
    cache: &Mutex<TypeScriptNavigationCache>,
    path: PathBuf,
    depth: usize,
    warmed: &mut usize,
    visited: &mut HashSet<PathBuf>,
) {
    if depth == MAX_TYPESCRIPT_REEXPORT_DEPTH
        || *warmed == MAX_TYPESCRIPT_WARM_IMPORT_TARGETS
        || !visited.insert(module_identity(&path))
    {
        return;
    }
    let Some(index) = cache.lock().unwrap_or_else(|e| e.into_inner()).index(&path) else {
        return;
    };
    let specifiers = index
        .exports
        .iter()
        .filter_map(|export| export.specifier.as_deref())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    for specifier in specifiers {
        if *warmed == MAX_TYPESCRIPT_WARM_IMPORT_TARGETS {
            return;
        }
        if let Some(target) = resolve_typescript_definition_import(cache, &path, &specifier) {
            if !is_typescript_path(&target) {
                continue;
            }
            *warmed += 1;
            warm_typescript_declaration_chain(cache, target, depth + 1, warmed, visited);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_common_typescript_frameworks_without_package_manifest() {
        for marker in [
            "next.config.ts",
            "nuxt.config.ts",
            "nest-cli.json",
            "remix.config.ts",
            "gatsby-config.ts",
            "astro.config.mjs",
            "svelte.config.js",
            "vite.config.ts",
            "angular.json",
            "app.config.ts",
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join(marker), "").unwrap();
            assert!(workspace_detected(dir.path()), "{marker}");
        }

        let plain = tempfile::tempdir().unwrap();
        assert!(!workspace_detected(plain.path()));
    }

    #[test]
    fn warmup_caches_import_targets_and_reexports() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
        let package = dir.path().join("node_modules/pkg");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("package.json"), r#"{"exports":"./entry.js"}"#).unwrap();
        std::fs::write(
            package.join("entry.js"),
            "export { value } from './types.js';\n",
        )
        .unwrap();
        std::fs::write(
            package.join("entry.d.ts"),
            "export { value } from './types.js';\n",
        )
        .unwrap();
        std::fs::write(
            package.join("types.d.ts"),
            "export declare const value: number;\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.ts"),
            "import { value } from 'pkg';\nvalue;\n",
        )
        .unwrap();

        let cache = Mutex::new(TypeScriptNavigationCache::default());
        warm_typescript_navigation(dir.path(), &cache);

        let cache = cache.lock().unwrap();
        assert!(
            cache
                .resolutions
                .values()
                .any(|resolution| resolution.target.ends_with("entry.d.ts"))
        );
        assert!(
            cache
                .indexes
                .keys()
                .any(|path| path.ends_with("entry.d.ts"))
        );
        assert!(
            cache
                .indexes
                .keys()
                .any(|path| path.ends_with("types.d.ts"))
        );
    }

    #[test]
    fn warmup_excludes_node_modules_sources() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
        let ignored = dir.path().join("node_modules/ignored");
        std::fs::create_dir_all(&ignored).unwrap();
        std::fs::write(
            ignored.join("package.json"),
            r#"{"exports":"./entry.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            ignored.join("entry.d.ts"),
            "export declare const ignored: number;\n",
        )
        .unwrap();
        std::fs::write(
            ignored.join("source.ts"),
            "import { ignored } from 'ignored';\nignored;\n",
        )
        .unwrap();

        let cache = Mutex::new(TypeScriptNavigationCache::default());
        warm_typescript_navigation(dir.path(), &cache);

        let cache = cache.lock().unwrap();
        assert!(cache.resolutions.is_empty());
        assert!(cache.indexes.is_empty());
    }
}
