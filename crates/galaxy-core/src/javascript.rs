//! Static JavaScript and TypeScript project metadata.
//!
//! This intentionally does not emulate TypeScript's type checker. It provides
//! bounded, static project and import resolution for editor navigation.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;
use tree_sitter::{Language, Node, Parser};

use crate::jsonc::{self, Value as JsoncValue};

const MAX_COMPONENTS: usize = 4000;
const MAX_COMPONENT_DEPTH: usize = 8;
const MAX_PROJECT_ANCESTORS: usize = 64;
const MAX_TSCONFIG_EXTENDS: usize = 32;
const COMPONENT_ROOTS: &[&str] = &["src", "app", "pages", "components"];
const EXCLUDED_DIRECTORIES: &[&str] = &["node_modules", "dist", "build", ".git"];
static IMPORT_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:\bfrom|\bimport|\brequire\s*\(|\bimport\s*\()\s*$")
        .expect("valid import prefix regex")
});

/// A UI framework declared by a package manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Framework {
    React,
    Vue,
    Svelte,
}

impl Framework {
    pub fn name(self) -> &'static str {
        match self {
            Self::React => "React",
            Self::Vue => "Vue",
            Self::Svelte => "Svelte",
        }
    }

    /// File types whose component syntax belongs to this framework.
    pub fn supports(self, path: &Path) -> bool {
        let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
        match self {
            Self::React => matches!(extension, "jsx" | "tsx"),
            Self::Vue => extension == "vue",
            Self::Svelte => extension == "svelte",
        }
    }
}

/// Package runner selected from lockfile names, matching the Run task policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Npm,
    Yarn,
    Pnpm,
}

impl PackageManager {
    pub fn name(self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Yarn => "yarn",
            Self::Pnpm => "pnpm",
        }
    }
}

/// Parsed metadata for one `package.json` directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub root: PathBuf,
    pub name: Option<String>,
    pub dependencies: BTreeSet<String>,
    pub frameworks: Vec<Framework>,
    pub package_manager: PackageManager,
}

/// A framework component found under a conventional project source root.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Component {
    pub framework: Framework,
    pub path: PathBuf,
    /// Extensionless path relative to its conventional source root.
    pub name: String,
    /// Named component declarations and their directly declared props.
    pub symbols: Vec<ComponentSymbol>,
}

/// A named component declaration and its direct props.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ComponentSymbol {
    pub name: String,
    pub props: Vec<String>,
}

/// Static TypeScript configuration discovered for an importer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeScriptProject {
    /// The directory used for project-local resolution.
    pub root: PathBuf,
    package_root: Option<PathBuf>,
    /// The nearest TypeScript configuration, when present.
    pub tsconfig: Option<PathBuf>,
    base_url: Option<PathBuf>,
    paths: Vec<(String, Vec<String>)>,
}

impl TypeScriptProject {
    /// Detects the nearest `tsconfig.json`, falling back to the nearest package root.
    /// Invalid configuration and invalid local `extends` chains are rejected.
    pub fn detect(path: &Path) -> Option<Self> {
        let start = if path.is_file() { path.parent()? } else { path };
        let ancestors = start
            .ancestors()
            .take(MAX_PROJECT_ANCESTORS)
            .collect::<Vec<_>>();
        let package_root = ancestors
            .iter()
            .find(|directory| directory.join("package.json").is_file())
            .map(|directory| (*directory).to_path_buf());
        if let Some(config) = ancestors
            .iter()
            .map(|dir| dir.join("tsconfig.json"))
            .find(|config| config.is_file())
        {
            return Self::from_tsconfig(config, package_root);
        }
        let root = package_root?;
        Some(Self {
            package_root: Some(root.clone()),
            root,
            tsconfig: None,
            base_url: None,
            paths: Vec::new(),
        })
    }

    fn from_tsconfig(config: PathBuf, package_root: Option<PathBuf>) -> Option<Self> {
        let mut seen = HashSet::new();
        let mut configs = Vec::new();
        load_tsconfig_chain(&config, &mut seen, &mut configs)?;
        let mut base_url = None;
        let mut paths = None;
        for (path, value) in configs {
            let options = value.get("compilerOptions");
            if let Some(base) = options.and_then(|options| options.get("baseUrl")) {
                let base = base.as_str()?;
                base_url = Some(lexical_normalize(&path.parent()?.join(base)));
            }
            if let Some(config_paths) = options.and_then(|options| options.get("paths")) {
                paths = Some(parse_paths(config_paths)?);
            }
        }
        Some(Self {
            root: config.parent()?.to_path_buf(),
            package_root,
            tsconfig: Some(config),
            base_url,
            paths: paths.unwrap_or_default(),
        })
    }
}

/// Resolves a TypeScript specifier using static project configuration only.
/// Relative imports retain [`resolve_relative_import`]'s existing behavior.
pub fn resolve_typescript_specifier(
    project: &TypeScriptProject,
    specifier: &str,
    from: &Path,
) -> Option<PathBuf> {
    if is_relative_typescript_specifier(specifier) {
        return resolve_relative_import(specifier, from);
    }
    if specifier.starts_with('/') || specifier.starts_with("node:") || specifier.contains("\\") {
        return None;
    }
    for (pattern, targets) in &project.paths {
        let Some(wildcard) = path_pattern_match(pattern, specifier) else {
            continue;
        };
        for target in targets {
            let Some(target) = substitute_path_wildcard(target, wildcard) else {
                continue;
            };
            if let Some(resolved) = resolve_module_path(
                project.base_url.as_deref().unwrap_or(&project.root),
                &target,
            ) {
                return Some(resolved);
            }
        }
    }
    if specifier.starts_with('#') {
        return project
            .package_root
            .as_deref()
            .and_then(|root| resolve_package_import(root, specifier));
    }
    if let Some(resolved) = project
        .base_url
        .as_deref()
        .and_then(|base_url| resolve_module_path(base_url, specifier))
    {
        return Some(resolved);
    }
    resolve_bare_package(project, specifier, from)
}

/// Resolves a `node:` specifier only through a locally installed `@types/node`.
/// This deliberately never consults system type roots or the network.
pub fn resolve_node_types_specifier(specifier: &str, from: &Path) -> Option<PathBuf> {
    let subpath = specifier.strip_prefix("node:")?;
    if subpath.is_empty()
        || subpath.contains('\\')
        || subpath
            .split('/')
            .any(|part| matches!(part, "" | "." | ".."))
    {
        return None;
    }
    let start = if from.is_file() { from.parent()? } else { from };
    for directory in start.ancestors().take(MAX_PROJECT_ANCESTORS) {
        let root = directory.join("node_modules/@types/node");
        if root.is_dir() {
            return resolve_package_root(&root, subpath);
        }
    }
    None
}

fn resolve_package_import(root: &Path, specifier: &str) -> Option<PathBuf> {
    let source = std::fs::read_to_string(root.join("package.json")).ok()?;
    let manifest = jsonc::parse(&source).ok()?;
    let target = package_import_target(manifest.get("imports")?, specifier)?;
    resolve_package_target(root, &target)
}

/// The string `imports` entries (`"#services/*": "./app/services/*.js"`) of
/// the package enclosing a file. Used to write auto-imports with the
/// package's own `#` aliases instead of relative paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageImports {
    root: PathBuf,
    entries: Vec<(String, String)>,
}

impl PackageImports {
    /// Reads the nearest `package.json` above `file`. `None` when it has no
    /// usable `imports` entries.
    pub fn detect(file: &Path) -> Option<Self> {
        let root = file
            .parent()?
            .ancestors()
            .take(MAX_PROJECT_ANCESTORS)
            .find(|directory| directory.join("package.json").is_file())?
            .to_path_buf();
        let source = std::fs::read_to_string(root.join("package.json")).ok()?;
        let manifest = jsonc::parse(&source).ok()?;
        let JsoncValue::Object(imports) = manifest.get("imports")? else {
            return None;
        };
        let entries = imports
            .iter()
            .filter_map(|(key, target)| Some((key.clone(), target.as_str()?.to_owned())))
            .filter(|(key, target)| {
                key.starts_with('#')
                    && valid_wildcard_pattern(key)
                    && valid_wildcard_pattern(target)
                    && key.contains('*') == target.contains('*')
            })
            .collect::<Vec<_>>();
        (!entries.is_empty()).then_some(Self { root, entries })
    }

    /// The shortest `#` specifier that resolves back to `target`, if any.
    /// Wildcards drop the source extension first, so `"./app/services/*.js"`
    /// maps `app/services/auth_service.ts` to `#services/auth_service`.
    pub fn specifier_for(&self, target: &Path) -> Option<String> {
        let target = lexical_normalize(target);
        let relative = target
            .strip_prefix(&self.root)
            .ok()?
            .to_str()?
            .replace('\\', "/");
        let mut best: Option<String> = None;
        for (key, value) in &self.entries {
            let Some(value_path) = value.strip_prefix("./") else {
                continue;
            };
            let candidates = match value_path.split_once('*') {
                None => vec![(key.clone(), value.clone())],
                Some((prefix, suffix)) => {
                    let Some(rest) = relative.strip_prefix(prefix) else {
                        continue;
                    };
                    let stem = Path::new(rest)
                        .with_extension("")
                        .to_string_lossy()
                        .replace('\\', "/");
                    [Some(stem), rest.strip_suffix(suffix).map(str::to_owned)]
                        .into_iter()
                        .flatten()
                        .filter(|wildcard| {
                            !wildcard.is_empty() && !wildcard.split('/').any(|part| part == "..")
                        })
                        .map(|wildcard| {
                            (
                                key.replacen('*', &wildcard, 1),
                                value.replacen('*', &wildcard, 1),
                            )
                        })
                        .collect()
                }
            };
            for (specifier, resolved_target) in candidates {
                let resolves_here = resolve_package_target(&self.root, &resolved_target)
                    .is_some_and(|resolved| lexical_normalize(&resolved) == target);
                if resolves_here
                    && best
                        .as_ref()
                        .is_none_or(|best| specifier.len() < best.len())
                {
                    best = Some(specifier);
                }
            }
        }
        best
    }
}

fn package_import_target(imports: &JsoncValue, specifier: &str) -> Option<String> {
    let JsoncValue::Object(entries) = imports else {
        return None;
    };
    entries
        .iter()
        .find(|(key, _)| key == specifier)
        .and_then(|(_, target)| target.as_str())
        .map(str::to_owned)
        .or_else(|| {
            entries.iter().find_map(|(pattern, target)| {
                let wildcard = path_pattern_match(pattern, specifier)?;
                substitute_path_wildcard(target.as_str()?, wildcard)
            })
        })
}

fn load_tsconfig_chain(
    path: &Path,
    seen: &mut HashSet<PathBuf>,
    configs: &mut Vec<(PathBuf, JsoncValue)>,
) -> Option<()> {
    if configs.len() == MAX_TSCONFIG_EXTENDS {
        return None;
    }
    let path = lexical_normalize(path);
    if !seen.insert(path.clone()) {
        return None;
    }
    let source = std::fs::read_to_string(&path).ok()?;
    let value = jsonc::parse(&source).ok()?;
    if !matches!(value, JsoncValue::Object(_)) {
        return None;
    }
    if let Some(extends) = value.get("extends") {
        let extends = extends.as_str()?;
        // Package-based presets (for example `@adonisjs/tsconfig/...`) are
        // useful to `tsc`, but this bounded resolver must not discard the
        // local config and its `paths` merely because that package preset is
        // outside its local extends subset.
        if let Some(parent) = local_tsconfig_path(path.parent()?, extends) {
            load_tsconfig_chain(&parent, seen, configs)?;
        }
    }
    configs.push((path, value));
    Some(())
}

fn local_tsconfig_path(directory: &Path, extends: &str) -> Option<PathBuf> {
    if !(extends.starts_with("./") || extends.starts_with("../")) || extends.contains('\\') {
        return None;
    }
    let path = lexical_normalize(&directory.join(extends));
    let path = if path.extension().is_some() {
        path
    } else {
        path.with_extension("json")
    };
    path.is_file().then_some(path)
}

fn parse_paths(value: &JsoncValue) -> Option<Vec<(String, Vec<String>)>> {
    let JsoncValue::Object(entries) = value else {
        return None;
    };
    entries
        .iter()
        .map(|(pattern, targets)| {
            valid_wildcard_pattern(pattern).then_some(())?;
            let JsoncValue::Array(targets) = targets else {
                return None;
            };
            let targets = targets
                .iter()
                .map(JsoncValue::as_str)
                .collect::<Option<Vec<_>>>()?
                .into_iter()
                .map(str::to_owned)
                .collect();
            Some((pattern.clone(), targets))
        })
        .collect()
}

fn valid_wildcard_pattern(value: &str) -> bool {
    value.matches('*').count() <= 1 && !value.is_empty()
}

pub fn is_relative_typescript_specifier(specifier: &str) -> bool {
    matches!(specifier, "." | "..") || specifier.starts_with("./") || specifier.starts_with("../")
}

fn path_pattern_match<'a>(pattern: &str, specifier: &'a str) -> Option<&'a str> {
    let Some((prefix, suffix)) = pattern.split_once('*') else {
        return (pattern == specifier).then_some("");
    };
    if !specifier.starts_with(prefix) || !specifier.ends_with(suffix) {
        return None;
    }
    let end = specifier.len().checked_sub(suffix.len())?;
    (end >= prefix.len()).then_some(&specifier[prefix.len()..end])
}

fn substitute_path_wildcard(target: &str, wildcard: &str) -> Option<String> {
    valid_wildcard_pattern(target).then(|| target.replacen('*', wildcard, 1))
}

fn resolve_module_path(base: &Path, specifier: &str) -> Option<PathBuf> {
    if specifier.starts_with('/') || specifier.contains('\\') {
        return None;
    }
    module_candidates(&lexical_normalize(&base.join(specifier)))
        .into_iter()
        .find(|candidate| candidate.is_file())
}

fn module_candidates(path: &Path) -> Vec<PathBuf> {
    let from = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("__galaxy__.ts");
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let mut candidates = relative_import_candidates(&format!("./{name}"), &from);
    candidates.extend([append_suffix(path, ".mts"), append_suffix(path, ".cts")]);
    candidates.extend([path.join("index.mts"), path.join("index.cts")]);
    candidates
}

fn resolve_bare_package(
    project: &TypeScriptProject,
    specifier: &str,
    from: &Path,
) -> Option<PathBuf> {
    let (package, subpath) = split_package_specifier(specifier)?;
    let start = if from.is_file() { from.parent()? } else { from };
    let mut searched = HashSet::new();
    for directory in start
        .ancestors()
        .chain(project.root.ancestors())
        .take(MAX_PROJECT_ANCESTORS)
    {
        if !searched.insert(directory.to_path_buf()) {
            continue;
        }
        let roots = std::iter::once(directory.join("node_modules").join(package)).chain(
            directory
                .file_name()
                .is_some_and(|name| name == "node_modules")
                .then(|| directory.join(package)),
        );
        for root in roots {
            if root.is_dir() {
                return resolve_package_root(&root, subpath);
            }
        }
    }
    None
}

fn split_package_specifier(specifier: &str) -> Option<(&str, &str)> {
    let mut parts = specifier.split('/');
    let first = parts.next()?;
    let package_parts = if first.starts_with('@') { 2 } else { 1 };
    let package_end = specifier
        .match_indices('/')
        .nth(package_parts - 1)
        .map_or(specifier.len(), |(index, _)| index);
    let package = &specifier[..package_end];
    let subpath = specifier[package_end..].strip_prefix('/').unwrap_or("");
    (!package.is_empty()
        && (!package.starts_with('@') || package.contains('/'))
        && !specifier.split('/').any(|part| part == "." || part == ".."))
    .then_some((package, subpath))
}

fn resolve_package_root(root: &Path, subpath: &str) -> Option<PathBuf> {
    let package_json = root.join("package.json");
    let manifest = std::fs::read_to_string(&package_json)
        .ok()
        .and_then(|source| jsonc::parse(&source).ok());
    if let Some(manifest) = &manifest {
        if let Some(exports) = manifest.get("exports") {
            if let Some(target) = package_export_target(exports, subpath) {
                let runtime = resolve_package_target(root, &target)?;
                return declaration_sibling(&runtime).or(Some(runtime));
            }
            return None;
        }
        if subpath.is_empty() {
            for key in ["types", "typings"] {
                if let Some(resolved) = manifest
                    .get(key)
                    .and_then(JsoncValue::as_str)
                    .and_then(|target| package_metadata_target(root, target))
                {
                    return Some(resolved);
                }
            }
            for key in ["main", "module"] {
                if let Some(resolved) = manifest
                    .get(key)
                    .and_then(JsoncValue::as_str)
                    .and_then(|target| resolve_package_target(root, target))
                {
                    return Some(resolved);
                }
            }
        }
    }
    resolve_module_path(root, subpath)
}

/// Published packages often point `exports` at emitted JavaScript while
/// colocating the navigable TypeScript declaration beside it.
fn declaration_sibling(runtime: &Path) -> Option<PathBuf> {
    runtime
        .extension()
        .is_some_and(|extension| matches!(extension.to_str(), Some("js" | "jsx" | "mjs" | "cjs")))
        .then(|| runtime.with_extension("d.ts"))?
        .is_file()
        .then(|| runtime.with_extension("d.ts"))
}

fn package_metadata_target(root: &Path, target: &str) -> Option<PathBuf> {
    (!Path::new(target).is_absolute() && !target.split('/').any(|component| component == ".."))
        .then(|| lexical_normalize(&root.join(target)))?
        .is_file()
        .then(|| lexical_normalize(&root.join(target)))
}

/// Chooses a package's declaration entry when a JavaScript runtime entry was
/// resolved first. Packages such as `knex` keep `types/index.d.ts` outside the
/// runtime `lib/` directory.
pub fn package_types_declaration(runtime: &Path) -> Option<PathBuf> {
    let mut directory = runtime.parent()?;
    while let Some(parent) = directory.parent() {
        let manifest_path = directory.join("package.json");
        if manifest_path.is_file() {
            let manifest = std::fs::read_to_string(manifest_path)
                .ok()
                .and_then(|source| jsonc::parse(&source).ok())?;
            for key in ["types", "typings"] {
                if let Some(target) = manifest.get(key).and_then(JsoncValue::as_str)
                    && let Some(declaration) = package_metadata_target(directory, target)
                    && declaration
                        .extension()
                        .is_some_and(|extension| extension == "ts")
                {
                    return Some(declaration);
                }
            }
            return None;
        }
        directory = parent;
    }
    None
}

fn package_export_target(exports: &JsoncValue, subpath: &str) -> Option<String> {
    if let Some(target) = exports.as_str() {
        return subpath.is_empty().then(|| target.to_owned());
    }
    let JsoncValue::Object(entries) = exports else {
        return None;
    };
    if subpath.is_empty() && !entries.iter().any(|(key, _)| key.starts_with('.')) {
        return conditional_export_target(exports);
    }
    let request = if subpath.is_empty() {
        ".".to_owned()
    } else {
        format!("./{subpath}")
    };
    entries
        .iter()
        .find(|(key, _)| key == &request)
        .and_then(|(_, target)| conditional_export_target(target))
        .or_else(|| {
            entries.iter().find_map(|(pattern, target)| {
                let wildcard = path_pattern_match(pattern, &request)?;
                substitute_path_wildcard(&conditional_export_target(target)?, wildcard)
            })
        })
}

/// Selects only direct string leaves from known package export conditions.
fn conditional_export_target(value: &JsoncValue) -> Option<String> {
    let JsoncValue::Object(entries) = value else {
        return value.as_str().map(str::to_owned);
    };
    ["types", "typings", "import", "default", "node"]
        .into_iter()
        .find_map(|condition| {
            entries
                .iter()
                .find(|(name, _)| name == condition)
                .and_then(|(_, target)| conditional_export_target(target))
        })
}

fn resolve_package_target(root: &Path, target: &str) -> Option<PathBuf> {
    if !target.starts_with("./") || target.split('/').any(|part| part == "..") {
        return None;
    }
    resolve_module_path(root, target.strip_prefix("./")?)
}

/// Returns the UTF-16 range inside an import-like string literal's quotes.
///
/// `string_start` and `string_end` are UTF-16 offsets for the complete literal,
/// including its quotes. This accepts the same syntax as the current editor
/// navigation: `from`, bare `import`, `require(...)`, and dynamic `import(...)`.
pub fn import_specifier_range(
    text: &str,
    string_start: usize,
    string_end: usize,
) -> Option<std::ops::Range<usize>> {
    let units: Vec<_> = text.encode_utf16().collect();
    if string_start > string_end || string_end > units.len() || string_end - string_start < 3 {
        return None;
    }
    let quote = units[string_start];
    if !matches!(quote, 0x22 | 0x27) || units[string_end - 1] != quote {
        return None;
    }
    let prefix = String::from_utf16_lossy(&units[..string_start]);
    IMPORT_PREFIX
        .is_match(&prefix)
        .then_some((string_start + 1)..(string_end - 1))
}

/// Builds the relative module resolution candidates in editor lookup order.
///
/// Package specifiers return no candidates. Paths are normalized lexically, so
/// candidates need not exist on disk.
pub fn relative_import_candidates(specifier: &str, from: &Path) -> Vec<PathBuf> {
    if !matches!(specifier, "." | "..")
        && !specifier.starts_with("./")
        && !specifier.starts_with("../")
    {
        return Vec::new();
    }

    let base = lexical_normalize(
        &from
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(specifier),
    );
    let mut candidates = [
        "", ".ts", ".tsx", ".d.ts", ".js", ".jsx", ".mjs", ".cjs", ".json",
    ]
    .into_iter()
    .map(|extension| append_suffix(&base, extension))
    .collect::<Vec<_>>();

    for (compiled, sources) in [
        (".js", &[".ts", ".tsx", ".d.ts"][..]),
        (".jsx", &[".tsx"][..]),
        (".mjs", &[".mts"][..]),
        (".cjs", &[".cts"][..]),
        // Declaration barrels often preserve a `.ts` source specifier even
        // though published packages contain only the emitted `.d.ts` file.
        (".ts", &[".d.ts"][..]),
        (".tsx", &[".d.ts"][..]),
        (".mts", &[".d.ts"][..]),
        (".cts", &[".d.ts"][..]),
    ] {
        if let Some(stem) = base.to_string_lossy().strip_suffix(compiled) {
            candidates.extend(
                sources
                    .iter()
                    .map(|source| PathBuf::from(format!("{stem}{source}"))),
            );
        }
    }
    candidates.extend(
        [
            "index.ts",
            "index.tsx",
            "index.d.ts",
            "index.js",
            "index.jsx",
        ]
        .into_iter()
        .map(|index| base.join(index)),
    );
    candidates
}

/// Resolves a relative JavaScript or TypeScript import to the first regular file.
///
/// The candidate order matches [`relative_import_candidates`]. Package specifiers
/// return `None` for language-server resolution.
pub fn resolve_relative_import(specifier: &str, from: &Path) -> Option<PathBuf> {
    relative_import_candidates(specifier, from)
        .into_iter()
        .find(|candidate| candidate.is_file())
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", path.to_string_lossy()))
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

impl Project {
    /// Reads the nearest package manifest at or above `path`.
    pub fn detect(path: &Path) -> Option<Self> {
        let start = if path.is_file() { path.parent()? } else { path };
        let root = start
            .ancestors()
            .find(|dir| dir.join("package.json").is_file())?;
        let source = std::fs::read_to_string(root.join("package.json")).ok()?;
        Self::parse(root, &source)
    }

    pub fn parse(root: &Path, source: &str) -> Option<Self> {
        let object = serde_json::from_str::<Value>(source)
            .ok()?
            .as_object()?
            .clone();
        let mut dependencies = BTreeSet::new();
        for section in [
            "dependencies",
            "devDependencies",
            "peerDependencies",
            "optionalDependencies",
        ] {
            if let Some(entries) = object.get(section).and_then(Value::as_object) {
                dependencies.extend(
                    entries
                        .iter()
                        .filter(|(_, value)| value.is_string())
                        .map(|(name, _)| name.clone()),
                );
            }
        }
        let frameworks = [
            (Framework::React, ["react", "react-dom", "next"].as_slice()),
            (Framework::Vue, ["vue", "nuxt"].as_slice()),
            (Framework::Svelte, ["svelte", "@sveltejs/kit"].as_slice()),
        ]
        .into_iter()
        .filter_map(|(framework, names)| {
            names
                .iter()
                .any(|name| dependencies.contains(*name))
                .then_some(framework)
        })
        .collect();
        Some(Self {
            root: root.to_path_buf(),
            name: object
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned),
            dependencies,
            frameworks,
            package_manager: package_manager(root),
        })
    }
}

/// Scans conventional source roots for components supported by declared frameworks.
///
/// Source text alone cannot identify a framework: only extensions supported by the
/// package metadata's exact framework markers are returned.
pub fn scan_components(project: &Project) -> Vec<Component> {
    fn scan_root(
        project: &Project,
        root: &Path,
        dir: &Path,
        depth: usize,
        out: &mut Vec<Component>,
    ) {
        if depth == 0 || out.len() >= MAX_COMPONENTS {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            if out.len() >= MAX_COMPONENTS {
                return;
            }
            let path = entry.path();
            if path.is_dir() {
                if !EXCLUDED_DIRECTORIES.contains(&entry.file_name().to_string_lossy().as_ref()) {
                    scan_root(project, root, &path, depth - 1, out);
                }
                continue;
            }
            let Some(framework) = project
                .frameworks
                .iter()
                .copied()
                .find(|framework| framework.supports(&path))
            else {
                continue;
            };
            let Some(relative) = path.strip_prefix(root).ok() else {
                continue;
            };
            let mut name = relative.to_path_buf();
            name.set_extension("");
            let name = name.to_string_lossy().into_owned();
            let source = std::fs::read_to_string(&path).unwrap_or_default();
            out.push(Component {
                framework,
                path,
                symbols: component_symbols(framework, &source, &name),
                name,
            });
        }
    }

    let mut components = Vec::new();
    for name in COMPONENT_ROOTS {
        let root = project.root.join(name);
        if root.is_dir() {
            scan_root(project, &root, &root, MAX_COMPONENT_DEPTH, &mut components);
        }
    }
    components.sort_by(|left, right| left.path.cmp(&right.path));
    components
}

fn component_symbols(
    framework: Framework,
    source: &str,
    fallback_name: &str,
) -> Vec<ComponentSymbol> {
    match framework {
        Framework::React => react_symbols(source),
        Framework::Vue => vue_symbols(source, fallback_name),
        Framework::Svelte => svelte_symbols(source, fallback_name),
    }
}

fn react_symbols(source: &str) -> Vec<ComponentSymbol> {
    let language: Language = if source.contains('<') {
        tree_sitter_typescript::LANGUAGE_TSX.into()
    } else {
        tree_sitter_javascript::LANGUAGE.into()
    };
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let mut symbols = Vec::new();
    let mut cursor = tree.root_node().walk();
    for export in tree.root_node().children(&mut cursor) {
        if export.kind() != "export_statement" {
            continue;
        }
        let mut declaration_cursor = export.walk();
        for declaration in export.named_children(&mut declaration_cursor) {
            match declaration.kind() {
                "function_declaration" if contains_jsx(declaration) => {
                    if let Some(name) = node_text(source, declaration.child_by_field_name("name"))
                        .filter(|name| is_pascal_case(name))
                    {
                        symbols.push(ComponentSymbol {
                            name: name.to_owned(),
                            props: function_props(source, declaration),
                        });
                    }
                }
                "lexical_declaration" | "variable_declaration" => {
                    let mut declarator_cursor = declaration.walk();
                    for declarator in declaration.named_children(&mut declarator_cursor) {
                        if declarator.kind() != "variable_declarator" {
                            continue;
                        }
                        let Some(value) = declarator.child_by_field_name("value") else {
                            continue;
                        };
                        let Some(name) = node_text(source, declarator.child_by_field_name("name"))
                            .filter(|name| is_pascal_case(name))
                        else {
                            continue;
                        };
                        if value.kind() == "arrow_function" && contains_jsx(value) {
                            symbols.push(ComponentSymbol {
                                name: name.to_owned(),
                                props: function_props(source, value),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    }
    symbols
}

fn vue_symbols(source: &str, fallback_name: &str) -> Vec<ComponentSymbol> {
    let scripts = script_blocks(source);
    let name = scripts
        .iter()
        .find_map(|script| vue_script_symbols(script).0)
        .unwrap_or_else(|| fallback_name.to_owned());
    let props = scripts
        .iter()
        .flat_map(|script| vue_script_symbols(script).1)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    vec![ComponentSymbol { name, props }]
}

fn svelte_symbols(source: &str, fallback_name: &str) -> Vec<ComponentSymbol> {
    let props = script_blocks(source)
        .iter()
        .filter(|script| !script.module)
        .flat_map(svelte_script_props)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    vec![ComponentSymbol {
        name: fallback_name.to_owned(),
        props,
    }]
}

#[derive(Clone, Copy)]
enum ScriptLanguage {
    JavaScript,
    TypeScript,
}

struct ScriptBlock<'a> {
    source: &'a str,
    language: ScriptLanguage,
    module: bool,
}

fn script_blocks(source: &str) -> Vec<ScriptBlock<'_>> {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_html::LANGUAGE.into())
        .is_err()
    {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let mut scripts = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "script_element" {
            let mut cursor = node.walk();
            let mut tag = None;
            let mut raw = None;
            for child in node.named_children(&mut cursor) {
                match child.kind() {
                    "start_tag" => tag = Some(child),
                    "raw_text" => raw = Some(child),
                    _ => {}
                }
            }
            let (Some(tag), Some(raw)) = (tag, raw) else {
                continue;
            };
            let attributes = html_attributes(source, tag);
            let language = match script_language(&attributes) {
                Some(language) => language,
                None => continue,
            };
            if let Some(source) = source.get(raw.byte_range()) {
                scripts.push(ScriptBlock {
                    source,
                    language,
                    module: attributes.iter().any(|(name, value)| {
                        *name == "module" || (*name == "context" && *value == Some("module"))
                    }),
                });
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    scripts
}

fn html_attributes<'a>(source: &'a str, tag: Node<'_>) -> Vec<(&'a str, Option<&'a str>)> {
    let mut cursor = tag.walk();
    tag.named_children(&mut cursor)
        .filter(|node| node.kind() == "attribute")
        .filter_map(|attribute| {
            let mut cursor = attribute.walk();
            let mut name = None;
            let mut value = None;
            for child in attribute.named_children(&mut cursor) {
                match child.kind() {
                    "attribute_name" => name = source.get(child.byte_range()),
                    "attribute_value" => value = source.get(child.byte_range()),
                    "quoted_attribute_value" => {
                        value = source
                            .get(child.byte_range())
                            .map(|value| value.trim_matches(['\'', '\"']));
                    }
                    _ => {}
                }
            }
            name.map(|name| (name, value))
        })
        .collect()
}

fn script_language(attributes: &[(&str, Option<&str>)]) -> Option<ScriptLanguage> {
    let lang = attributes
        .iter()
        .find_map(|(name, value)| (*name == "lang").then_some(*value).flatten());
    let mime = attributes
        .iter()
        .find_map(|(name, value)| (*name == "type").then_some(*value).flatten());
    match (lang, mime) {
        (Some("ts" | "typescript"), _)
        | (_, Some("text/typescript" | "application/typescript")) => {
            Some(ScriptLanguage::TypeScript)
        }
        (Some("js" | "javascript"), _)
        | (None, None | Some("module" | "text/javascript" | "application/javascript")) => {
            Some(ScriptLanguage::JavaScript)
        }
        _ => None,
    }
}

fn parse_script(source: &str, language: ScriptLanguage) -> Option<tree_sitter::Tree> {
    let mut parser = Parser::new();
    let language = match language {
        ScriptLanguage::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        ScriptLanguage::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    };
    parser.set_language(&language).ok()?;
    parser.parse(source, None)
}

fn vue_script_symbols(script: &ScriptBlock<'_>) -> (Option<String>, Vec<String>) {
    let Some(tree) = parse_script(script.source, script.language) else {
        return (None, Vec::new());
    };
    let mut name = None;
    let mut props = Vec::new();
    walk_nodes(tree.root_node(), &mut |node| {
        if node.kind() == "call_expression" {
            let Some(function) = node.child_by_field_name("function") else {
                return;
            };
            match node_text(script.source, Some(function)) {
                Some("defineOptions") => {
                    if let Some(argument) = node
                        .child_by_field_name("arguments")
                        .and_then(first_named_child)
                    {
                        name = object_property_value(script.source, argument, "name")
                            .and_then(|value| string_value(script.source, value));
                    }
                }
                Some("defineProps") => {
                    if let Some(argument) = node
                        .child_by_field_name("arguments")
                        .and_then(first_named_child)
                    {
                        props.extend(static_props_node(script.source, argument));
                    }
                }
                _ => {}
            }
        }
    });
    let mut cursor = tree.root_node().walk();
    for statement in tree.root_node().named_children(&mut cursor) {
        if statement.kind() != "export_statement"
            || !node_text(script.source, Some(statement))
                .is_some_and(|text| text.starts_with("export default"))
        {
            continue;
        }
        if let Some(object) = first_named_child(statement).filter(|node| node.kind() == "object") {
            name = name.or_else(|| {
                object_property_value(script.source, object, "name")
                    .and_then(|value| string_value(script.source, value))
            });
            if let Some(value) = object_property_value(script.source, object, "props") {
                props.extend(static_props_node(script.source, value));
            }
        }
    }
    (name, props)
}

fn svelte_script_props(script: &ScriptBlock<'_>) -> Vec<String> {
    let Some(tree) = parse_script(script.source, script.language) else {
        return Vec::new();
    };
    let mut props = Vec::new();
    let mut cursor = tree.root_node().walk();
    for statement in tree.root_node().named_children(&mut cursor) {
        match statement.kind() {
            "export_statement" => {
                let Some(declaration) = first_named_child(statement) else {
                    continue;
                };
                if matches!(
                    declaration.kind(),
                    "lexical_declaration" | "variable_declaration"
                ) && node_text(script.source, Some(declaration))
                    .is_some_and(|text| text.trim_start().starts_with("let "))
                {
                    let mut declarations = declaration.walk();
                    for declarator in declaration.named_children(&mut declarations) {
                        if declarator.kind() != "variable_declarator" {
                            continue;
                        }
                        if let Some(name) =
                            node_text(script.source, declarator.child_by_field_name("name"))
                        {
                            props.push(name.to_owned());
                        }
                    }
                }
            }
            "lexical_declaration" | "variable_declaration" => {
                let mut declarations = statement.walk();
                for declarator in statement.named_children(&mut declarations) {
                    let (Some(name), Some(value)) = (
                        declarator.child_by_field_name("name"),
                        declarator.child_by_field_name("value"),
                    ) else {
                        continue;
                    };
                    if name.kind() == "object_pattern" && is_props_call(script.source, value) {
                        props.extend(object_pattern_keys(script.source, name));
                    }
                }
            }
            _ => {}
        }
    }
    props
}

fn first_named_child(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).next()
}

fn walk_nodes(node: Node<'_>, visit: &mut impl FnMut(Node<'_>)) {
    visit(node);
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk_nodes(child, visit);
    }
}

fn object_property_value<'a>(source: &str, object: Node<'a>, key: &str) -> Option<Node<'a>> {
    if object.kind() != "object" {
        return None;
    }
    let mut cursor = object.walk();
    object.named_children(&mut cursor).find_map(|property| {
        (property.kind() == "pair"
            && node_text(source, property.child_by_field_name("key"))
                .map(|name| name.trim_matches(['\'', '\"']) == key)
                .unwrap_or(false))
        .then(|| property.child_by_field_name("value"))
        .flatten()
    })
}

fn string_value(source: &str, node: Node<'_>) -> Option<String> {
    (node.kind() == "string")
        .then(|| node_text(source, Some(node)))
        .flatten()
        .map(|value| value.trim_matches(['\'', '\"']).to_owned())
}

fn static_props_node(source: &str, node: Node<'_>) -> Vec<String> {
    match node.kind() {
        "array" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .filter_map(|child| string_value(source, child))
                .collect()
        }
        "object" => object_property_keys(source, node),
        _ => Vec::new(),
    }
}

fn object_property_keys(source: &str, object: Node<'_>) -> Vec<String> {
    let mut cursor = object.walk();
    object
        .named_children(&mut cursor)
        .filter(|property| property.kind() == "pair")
        .filter_map(|property| node_text(source, property.child_by_field_name("key")))
        .map(|key| key.trim_matches(['\'', '\"']).to_owned())
        .collect()
}

fn is_props_call(source: &str, node: Node<'_>) -> bool {
    node.kind() == "call_expression"
        && node_text(source, node.child_by_field_name("function")) == Some("$props")
        && node
            .child_by_field_name("arguments")
            .is_some_and(|arguments| first_named_child(arguments).is_none())
}

fn object_pattern_keys(source: &str, pattern: Node<'_>) -> Vec<String> {
    let mut cursor = pattern.walk();
    pattern
        .named_children(&mut cursor)
        .filter_map(|property| match property.kind() {
            "shorthand_property_identifier_pattern" => node_text(source, Some(property)),
            "pair_pattern" => node_text(source, property.child_by_field_name("key")),
            _ => None,
        })
        .map(str::to_owned)
        .collect()
}

fn object_keys(source: &str) -> Vec<String> {
    let key = Regex::new(r#"^\s*(?:['\"]([^'\"]+)['\"]|([A-Za-z_$][\w$]*))\s*(?::|$)"#)
        .expect("valid object key regex");
    source
        .split(',')
        .filter_map(|entry| {
            key.captures(
                entry
                    .trim()
                    .trim_start_matches('{')
                    .trim_end_matches('}')
                    .trim(),
            )
            .and_then(|key| {
                key.get(1)
                    .or_else(|| key.get(2))
                    .map(|key| key.as_str().to_owned())
            })
        })
        .collect()
}

fn contains_jsx(node: Node<'_>) -> bool {
    if matches!(
        node.kind(),
        "jsx_element" | "jsx_self_closing_element" | "jsx_fragment"
    ) {
        return true;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor).any(contains_jsx)
}

fn function_props(source: &str, function: Node<'_>) -> Vec<String> {
    let mut cursor = function.walk();
    function
        .children(&mut cursor)
        .filter(|child| child.kind() != "statement_block")
        .find_map(first_object_pattern)
        .map(|pattern| object_keys(source.get(pattern.byte_range()).unwrap_or_default()))
        .unwrap_or_default()
}

fn first_object_pattern(node: Node<'_>) -> Option<Node<'_>> {
    if node.kind() == "object_pattern" {
        return Some(node);
    }
    let mut cursor = node.walk();
    node.children(&mut cursor).find_map(first_object_pattern)
}

fn node_text<'a>(source: &'a str, node: Option<Node<'_>>) -> Option<&'a str> {
    source.get(node?.byte_range())
}

fn is_pascal_case(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}

fn package_manager(root: &Path) -> PackageManager {
    if root.join("pnpm-lock.yaml").is_file() {
        PackageManager::Pnpm
    } else if root.join("yarn.lock").is_file() {
        PackageManager::Yarn
    } else {
        PackageManager::Npm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_dependency_sections_and_detects_frameworks() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("pnpm-lock.yaml"), "lockfileVersion: '9'\n").unwrap();
        let project = Project::parse(
            root.path(),
            r#"{"name":"site","dependencies":{"react":"19"},"devDependencies":{"vue":"3","svelte":"4"},"peerDependencies":{"ignored":false}}"#,
        )
        .unwrap();
        assert_eq!(project.name.as_deref(), Some("site"));
        assert_eq!(project.package_manager, PackageManager::Pnpm);
        assert_eq!(
            project.frameworks,
            vec![Framework::React, Framework::Vue, Framework::Svelte]
        );
        assert!(!project.dependencies.contains("ignored"));
    }

    #[test]
    fn detects_nearest_monorepo_package() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("apps/web/src");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(
            root.path().join("package.json"),
            r#"{"name":"root","dependencies":{"vue":"3"}}"#,
        )
        .unwrap();
        let package = app.parent().unwrap().join("package.json");
        std::fs::write(&package, r#"{"name":"web","dependencies":{"react":"19"}}"#).unwrap();
        let project = Project::detect(&app.join("main.tsx")).unwrap();
        assert_eq!(project.root, app.parent().unwrap().to_path_buf());
        assert_eq!(project.frameworks, vec![Framework::React]);
    }

    #[test]
    fn scans_declared_framework_components_under_conventional_roots() {
        let root = tempfile::tempdir().unwrap();
        for path in [
            "src/App.tsx",
            "app/layout.jsx",
            "pages/index.vue",
            "components/Widget.svelte",
            "src/nested/Panel.tsx",
            "src/node_modules/Ignored.tsx",
            "src/dist/Ignored.tsx",
            "src/build/Ignored.tsx",
            "src/.git/Ignored.tsx",
            "other/Outside.tsx",
        ] {
            let path = root.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "component").unwrap();
        }
        let project = Project::parse(
            root.path(),
            r#"{"dependencies":{"react":"19","vue":"3","svelte":"5"}}"#,
        )
        .unwrap();

        assert_eq!(
            scan_components(&project),
            vec![
                Component {
                    framework: Framework::React,
                    path: root.path().join("app/layout.jsx"),
                    name: "layout".into(),
                    symbols: vec![],
                },
                Component {
                    framework: Framework::Svelte,
                    path: root.path().join("components/Widget.svelte"),
                    name: "Widget".into(),
                    symbols: vec![ComponentSymbol {
                        name: "Widget".into(),
                        props: vec![],
                    }],
                },
                Component {
                    framework: Framework::Vue,
                    path: root.path().join("pages/index.vue"),
                    name: "index".into(),
                    symbols: vec![ComponentSymbol {
                        name: "index".into(),
                        props: vec![],
                    }],
                },
                Component {
                    framework: Framework::React,
                    path: root.path().join("src/App.tsx"),
                    name: "App".into(),
                    symbols: vec![],
                },
                Component {
                    framework: Framework::React,
                    path: root.path().join("src/nested/Panel.tsx"),
                    name: "nested/Panel".into(),
                    symbols: vec![],
                },
            ]
        );
    }

    #[test]
    fn extracts_react_exported_jsx_symbols_and_direct_props() {
        assert_eq!(
            react_symbols(
                "export function Card({ title, count: total }) { return <article>{title}</article>; }\n\
                 export const Button = ({ label }) => <button>{label}</button>;\n\
                 export const helper = () => <span />;\n\
                 function Hidden({ ignored }) { return <div />; }"
            ),
            vec![
                ComponentSymbol {
                    name: "Card".into(),
                    props: vec!["title".into(), "count".into()],
                },
                ComponentSymbol {
                    name: "Button".into(),
                    props: vec!["label".into()],
                },
            ]
        );
    }

    #[test]
    fn extracts_vue_static_symbols_and_falls_back_to_source_name() {
        assert_eq!(
            vue_symbols(
                "<script setup>defineOptions({ name: 'StatusBadge' }); defineProps(['state', 'label']);</script>",
                "Status"
            ),
            vec![ComponentSymbol {
                name: "StatusBadge".into(),
                props: vec!["label".into(), "state".into()],
            }]
        );
        assert_eq!(
            vue_symbols(
                "<script>export default { name: 'LegacyCard', props: { title: String, active: Boolean } }</script>",
                "Card"
            ),
            vec![ComponentSymbol {
                name: "LegacyCard".into(),
                props: vec!["active".into(), "title".into()],
            }]
        );
        assert_eq!(
            vue_symbols("<template><div /></template>", "Fallback"),
            vec![ComponentSymbol {
                name: "Fallback".into(),
                props: vec![],
            }]
        );
    }

    #[test]
    fn extracts_svelte_direct_props_and_ignores_non_props() {
        assert_eq!(
            svelte_symbols(
                "<script>export let title; let { count: total, theme } = $props(); let ignored = value;</script>",
                "Counter"
            ),
            vec![ComponentSymbol {
                name: "Counter".into(),
                props: vec!["count".into(), "theme".into(), "title".into()],
            }]
        );
        assert_eq!(
            svelte_symbols("<script>let local = 1;</script>", "Fallback"),
            vec![ComponentSymbol {
                name: "Fallback".into(),
                props: vec![],
            }]
        );
    }

    #[test]
    fn component_metadata_uses_typed_script_asts() {
        assert_eq!(
            vue_symbols(
                r#"<script type="application/json">{"name":"Ignored"}</script>
                <script lang="ts">
                // defineOptions({ name: 'Comment' });
                const text = "defineProps(['string'])";
                defineOptions({ name: 'TypedBadge' });
                defineProps<{ title: string }>(['title']);
                </script>"#,
                "Fallback"
            ),
            vec![ComponentSymbol {
                name: "TypedBadge".into(),
                props: vec!["title".into()],
            }]
        );
        assert_eq!(
            svelte_symbols(
                r#"<script context="module">export let moduleOnly; let { moduleRune } = $props();</script>
                <script>/* export let comment; */ const text = "let { string } = $props()"; export const notAProp = true; export let title; let { count: total, theme } = $props();</script>"#,
                "Counter"
            ),
            vec![ComponentSymbol {
                name: "Counter".into(),
                props: vec!["count".into(), "theme".into(), "title".into()],
            }]
        );
    }

    #[test]
    fn source_files_without_declared_framework_are_not_components() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("src/App.tsx");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(source, "export function App() {}\n").unwrap();
        let project =
            Project::parse(root.path(), r#"{"dependencies":{"typescript":"5"}}"#).unwrap();

        assert!(scan_components(&project).is_empty());
    }

    #[test]
    fn finds_import_specifiers_with_utf16_offsets() {
        fn range(text: &str, literal: &str) -> Option<String> {
            let start = text.find(literal)?;
            let start = text[..start].encode_utf16().count();
            let end = start + literal.encode_utf16().count();
            let range = import_specifier_range(text, start, end)?;
            String::from_utf16(&text.encode_utf16().collect::<Vec<_>>()[range]).ok()
        }

        assert_eq!(
            range("import type { A } from '../config/env'", "'../config/env'"),
            Some("../config/env".into())
        );
        assert_eq!(
            range("import fs from 'node:fs/promises'", "'node:fs/promises'"),
            Some("node:fs/promises".into())
        );
        assert_eq!(
            range("export * from \"./x\"", "\"./x\""),
            Some("./x".into())
        );
        assert_eq!(
            range("import './polyfill'", "'./polyfill'"),
            Some("./polyfill".into())
        );
        assert_eq!(
            range("const a = require('lodash')", "'lodash'"),
            Some("lodash".into())
        );
        assert_eq!(
            range("const a = await import('./lazy')", "'./lazy'"),
            Some("./lazy".into())
        );
        assert_eq!(
            range("const crab = \"🦀\"; import './after'", "'./after'"),
            Some("./after".into())
        );
    }

    #[test]
    fn rejects_non_import_or_invalid_string_literals() {
        fn offsets(text: &str, literal: &str) -> (usize, usize) {
            let start = text.find(literal).unwrap();
            let start = text[..start].encode_utf16().count();
            (start, start + literal.encode_utf16().count())
        }

        for (text, literal) in [
            ("const s = 'hello'", "'hello'"),
            ("console.log('from')", "'from'"),
            ("import x from ''", "''"),
            ("import x from 'mismatched\"", "'mismatched\""),
        ] {
            let (start, end) = offsets(text, literal);
            assert_eq!(import_specifier_range(text, start, end), None, "{text}");
        }
        assert_eq!(import_specifier_range("import 'x'", 0, 99), None);
    }

    #[test]
    fn preserves_relative_import_candidate_order() {
        let from = Path::new("/project/src/main.ts");
        let candidates = relative_import_candidates("./feature", from);
        assert_eq!(
            candidates,
            [
                "/project/src/feature",
                "/project/src/feature.ts",
                "/project/src/feature.tsx",
                "/project/src/feature.d.ts",
                "/project/src/feature.js",
                "/project/src/feature.jsx",
                "/project/src/feature.mjs",
                "/project/src/feature.cjs",
                "/project/src/feature.json",
                "/project/src/feature/index.ts",
                "/project/src/feature/index.tsx",
                "/project/src/feature/index.d.ts",
                "/project/src/feature/index.js",
                "/project/src/feature/index.jsx",
            ]
            .map(PathBuf::from)
        );
    }

    #[test]
    fn adds_compiled_import_source_fallbacks_after_direct_candidates() {
        let candidates =
            relative_import_candidates("./feature.js", Path::new("/project/src/main.ts"));
        assert_eq!(
            &candidates[9..12],
            [
                PathBuf::from("/project/src/feature.ts"),
                PathBuf::from("/project/src/feature.tsx"),
                PathBuf::from("/project/src/feature.d.ts"),
            ]
        );
    }

    #[test]
    fn adds_declaration_fallback_for_explicit_typescript_specifiers() {
        let candidates =
            relative_import_candidates("./feature.ts", Path::new("/project/src/main.d.ts"));
        assert!(candidates.contains(&PathBuf::from("/project/src/feature.d.ts")));
    }

    #[test]
    fn resolves_first_file_and_rejects_directories_and_packages() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("src");
        std::fs::create_dir_all(src.join("directory")).unwrap();
        std::fs::create_dir_all(src.join("indexed")).unwrap();
        std::fs::write(src.join("feature.tsx"), "").unwrap();
        std::fs::write(src.join("indexed/index.d.ts"), "").unwrap();
        std::fs::write(src.join("compiled.ts"), "").unwrap();
        std::fs::write(src.join("declaration.d.ts"), "").unwrap();
        let from = src.join("main.ts");

        assert_eq!(
            resolve_relative_import("./feature", &from),
            Some(src.join("feature.tsx"))
        );
        assert_eq!(
            resolve_relative_import("./indexed", &from),
            Some(src.join("indexed/index.d.ts"))
        );
        assert_eq!(
            resolve_relative_import("./compiled.js", &from),
            Some(src.join("compiled.ts"))
        );
        assert_eq!(
            resolve_relative_import("./declaration.ts", &from),
            Some(src.join("declaration.d.ts"))
        );
        assert_eq!(resolve_relative_import("./directory", &from), None);
        assert!(relative_import_candidates("react", &from).is_empty());
        assert!(relative_import_candidates("node:fs", &from).is_empty());
    }

    #[test]
    fn resolves_tsconfig_base_url_paths_and_extends() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir_all(project.join("src/lib")).unwrap();
        std::fs::create_dir_all(project.join("generated")).unwrap();
        std::fs::write(project.join("src/lib/util.mts"), "").unwrap();
        std::fs::write(project.join("generated/exact.cts"), "").unwrap();
        std::fs::write(
            project.join("base.json"),
            r#"{ "compilerOptions": { "baseUrl": "./src", "paths": { "@lib/*": ["lib/*"], "exact": ["../generated/exact"] } } }"#,
        )
        .unwrap();
        std::fs::write(
            project.join("tsconfig.json"),
            r#"// local configuration
            { "extends": "./base", "compilerOptions": { "paths": { "exact": ["../generated/exact"], "@lib/*": ["missing/*", "lib/*"] } } }"#,
        )
        .unwrap();
        let from = project.join("src/main.ts");
        std::fs::write(&from, "").unwrap();
        let detected = TypeScriptProject::detect(&from).unwrap();
        assert_eq!(
            resolve_typescript_specifier(&detected, "@lib/util", &from),
            Some(project.join("src/lib/util.mts"))
        );
        assert_eq!(
            resolve_typescript_specifier(&detected, "exact", &from),
            Some(project.join("generated/exact.cts"))
        );
    }

    #[test]
    fn resolves_local_paths_when_tsconfig_extends_a_package_preset() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("app/libraries")).unwrap();
        std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r##"{"extends":"@adonisjs/tsconfig/tsconfig.app.json","compilerOptions":{"paths":{"#libraries/*":["./app/libraries/*.js"]}}}"##,
        )
        .unwrap();
        let target = dir.path().join("app/libraries/auditing.ts");
        std::fs::write(&target, "export const auditing = true;\n").unwrap();
        let importer = dir.path().join("app/resolver.ts");
        std::fs::write(&importer, "import '#libraries/auditing';\n").unwrap();

        let project = TypeScriptProject::detect(&importer).unwrap();
        assert_eq!(
            resolve_typescript_specifier(&project, "#libraries/auditing", &importer),
            Some(target)
        );
    }

    #[test]
    fn rejects_invalid_or_cyclic_tsconfig_extends() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("tsconfig.json"),
            r#"{ "extends": "./other" }"#,
        )
        .unwrap();
        std::fs::write(
            root.path().join("other.json"),
            r#"{ "extends": "./tsconfig" }"#,
        )
        .unwrap();
        assert!(TypeScriptProject::detect(root.path()).is_none());
        std::fs::write(root.path().join("tsconfig.json"), "{ invalid").unwrap();
        assert!(TypeScriptProject::detect(root.path()).is_none());
    }

    #[test]
    fn resolves_static_package_exports_and_fallbacks_without_escaping() {
        let root = tempfile::tempdir().unwrap();
        let project_root = root.path().join("project");
        let package_root = project_root.join("node_modules/pkg");
        std::fs::create_dir_all(package_root.join("dist")).unwrap();
        std::fs::write(project_root.join("package.json"), "{}").unwrap();
        std::fs::write(package_root.join("dist/index.ts"), "").unwrap();
        std::fs::write(package_root.join("dist/feature.ts"), "").unwrap();
        std::fs::write(
            package_root.join("package.json"),
            r#"{ "exports": { ".": "./dist/index", "./feature": "./dist/feature", "./wild/*": "./dist/*.ts" } }"#,
        )
        .unwrap();
        let from = project_root.join("src/main.ts");
        std::fs::create_dir_all(from.parent().unwrap()).unwrap();
        std::fs::write(&from, "").unwrap();
        let project = TypeScriptProject::detect(&from).unwrap();
        assert_eq!(
            resolve_typescript_specifier(&project, "pkg", &from),
            Some(package_root.join("dist/index.ts"))
        );
        assert_eq!(
            resolve_typescript_specifier(&project, "pkg/feature", &from),
            Some(package_root.join("dist/feature.ts"))
        );
        assert_eq!(
            resolve_typescript_specifier(&project, "pkg/missing", &from),
            None
        );
        assert_eq!(
            resolve_typescript_specifier(&project, "pkg/../../outside", &from),
            None
        );
        assert_eq!(
            resolve_typescript_specifier(&project, "absent", &from),
            None
        );

        let fallback = project_root.join("node_modules/fallback");
        std::fs::create_dir_all(&fallback).unwrap();
        std::fs::write(fallback.join("types.d.ts"), "").unwrap();
        std::fs::write(
            fallback.join("package.json"),
            r#"{ "types": "./types.d.ts" }"#,
        )
        .unwrap();
        assert_eq!(
            resolve_typescript_specifier(&project, "fallback", &from),
            Some(fallback.join("types.d.ts"))
        );
    }

    #[test]
    fn resolves_permitted_conditional_package_exports() {
        let root = tempfile::tempdir().unwrap();
        let project_root = root.path().join("project");
        let package_root = project_root.join("node_modules/pkg");
        std::fs::create_dir_all(package_root.join("dist")).unwrap();
        std::fs::write(project_root.join("package.json"), "{}").unwrap();
        for file in ["types.d.ts", "feature.d.ts"] {
            std::fs::write(package_root.join("dist").join(file), "").unwrap();
        }
        std::fs::write(
            package_root.join("package.json"),
            r#"{"exports":{".":{"browser":"./dist/browser.js","default":"./dist/default.js","types":"./dist/types.d.ts"},"./feature/*":{"import":"./dist/*.js","typings":"./dist/*.d.ts"},"./unsafe":{"browser":"./dist/browser.js"}}}"#,
        )
        .unwrap();
        let from = project_root.join("src/main.ts");
        std::fs::create_dir_all(from.parent().unwrap()).unwrap();
        std::fs::write(&from, "").unwrap();
        let project = TypeScriptProject::detect(&from).unwrap();

        assert_eq!(
            resolve_typescript_specifier(&project, "pkg", &from),
            Some(package_root.join("dist/types.d.ts"))
        );
        assert_eq!(
            resolve_typescript_specifier(&project, "pkg/feature/feature", &from),
            Some(package_root.join("dist/feature.d.ts"))
        );
        assert_eq!(
            resolve_typescript_specifier(&project, "pkg/unsafe", &from),
            None
        );
    }

    #[test]
    fn finds_package_types_for_runtime_submodule() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("node_modules/knex");
        std::fs::create_dir_all(package.join("lib")).unwrap();
        std::fs::create_dir_all(package.join("types")).unwrap();
        std::fs::write(
            package.join("package.json"),
            r#"{"types":"types/index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(package.join("lib/index.js"), "").unwrap();
        std::fs::write(package.join("types/index.d.ts"), "").unwrap();
        assert_eq!(
            package_types_declaration(&package.join("lib/index.js")),
            Some(package.join("types/index.d.ts"))
        );
    }

    #[test]
    fn resolves_nearest_package_imports_after_paths_without_escaping() {
        let root = tempfile::tempdir().unwrap();
        let project_root = root.path().join("project");
        let nested = project_root.join("src/nested");
        std::fs::create_dir_all(project_root.join("generated")).unwrap();
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(project_root.join("generated/exact.ts"), "").unwrap();
        std::fs::write(project_root.join("generated/feature.ts"), "").unwrap();
        std::fs::write(
            project_root.join("package.json"),
            r##"{"imports":{"#exact":"./generated/exact.js","#alias/*":"./generated/*.js","#conditional":{"default":"./generated/exact.js"},"#escape":"../outside.js"}}"##,
        )
        .unwrap();
        std::fs::write(
            project_root.join("tsconfig.json"),
            r##"{"compilerOptions":{"paths":{"#exact":["missing/*"]}}}"##,
        )
        .unwrap();
        let from = nested.join("main.ts");
        std::fs::write(&from, "").unwrap();
        let project = TypeScriptProject::detect(&from).unwrap();

        assert_eq!(
            resolve_typescript_specifier(&project, "#exact", &from),
            Some(project_root.join("generated/exact.ts"))
        );
        assert_eq!(
            resolve_typescript_specifier(&project, "#alias/feature", &from),
            Some(project_root.join("generated/feature.ts"))
        );
        assert_eq!(
            resolve_typescript_specifier(&project, "#conditional", &from),
            None
        );
        assert_eq!(
            resolve_typescript_specifier(&project, "#escape", &from),
            None
        );
    }
}
