//! Package index and symbol lookup for Go.
//!
//! A [`GoEngine`] resolves an import path (or a directory) to a [`Package`],
//! the parsed declarations of the files that take part in the build, and
//! answers which fields and methods a type has, promoted ones included.
//! Open buffers win over files on disk. Parsed files are cached by
//! modification time and size; packages are cached until their directory or
//! one of their files changes. Completion and hover are built on top of it.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use super::build::{BuildContext, eval_constraint, file_matches, go_files};
use super::integration::{
    GoConfigEntry, GoRoute, scan_fiber_routes, scan_gin_routes, scan_goravel_config,
    scan_goravel_routes,
};
use super::integration::{GoIntegration, detect_all};
use super::modules::{GoProject, discover_goroot};
use super::parse::{FuncDecl, GoFile, TypeDecl, ValueDecl, exported, parse_file};
use super::texpr::TypeExpr;

mod members;
#[cfg(test)]
mod tests;

pub use members::{Member, MemberKind, Members};

/// Files read per package.
const MAX_PACKAGE_FILES: usize = 400;
/// Parsed files kept; the least recently used quarter goes when exceeded.
const MAX_CACHED_FILES: usize = 5000;
const MAX_CACHED_PACKAGES: usize = 1500;
/// Larger files are skipped.
const MAX_FILE_BYTES: u64 = 8 << 20;
/// Files [`GoEngine::build`] parses ahead of time.
const PREPARSE_FILES: usize = 2000;

const UNIVERSE_FUNCS: &[&str] = &[
    "len", "cap", "make", "new", "append", "copy", "delete", "panic", "recover", "print",
    "println", "close", "min", "max", "clear", "complex", "real", "imag",
];
const UNIVERSE_CONSTS: &[&str] = &["true", "false", "nil", "iota"];

/// Modification time and size of a file when it was read.
type Stamp = (Option<SystemTime>, u64);

/// The Go files of one directory that take part in a build.
#[derive(Debug, Clone)]
pub struct Package {
    pub import_path: String,
    pub name: String,
    pub dir: PathBuf,
    /// Sorted by path.
    pub files: Vec<Arc<GoFile>>,
}

impl Package {
    pub fn type_decl(&self, name: &str) -> Option<(&GoFile, &TypeDecl)> {
        self.files.iter().find_map(|file| {
            file.types
                .iter()
                .find(|t| t.name == name)
                .map(|t| (&**file, t))
        })
    }

    /// A function that is not a method.
    pub fn func(&self, name: &str) -> Option<(&GoFile, &FuncDecl)> {
        self.files.iter().find_map(|file| {
            file.funcs
                .iter()
                .find(|f| f.receiver.is_none() && f.name == name)
                .map(|f| (&**file, f))
        })
    }

    /// A constant or variable.
    pub fn value(&self, name: &str) -> Option<(&GoFile, &ValueDecl)> {
        self.files.iter().find_map(|file| {
            file.consts
                .iter()
                .chain(file.vars.iter())
                .find(|v| v.name == name)
                .map(|v| (&**file, v))
        })
    }

    /// Methods declared on `type_name` in any file, sorted by name.
    pub fn methods_of(&self, type_name: &str) -> Vec<(&GoFile, &FuncDecl)> {
        let mut out: Vec<(&GoFile, &FuncDecl)> = self
            .files
            .iter()
            .flat_map(|file| {
                file.funcs
                    .iter()
                    .filter(|f| f.receiver.as_ref().is_some_and(|r| r.base == type_name))
                    .map(move |f| (&**file, f))
            })
            .collect();
        // Stable, so the first file wins among duplicates.
        out.sort_by(|a, b| a.1.name.cmp(&b.1.name));
        out.dedup_by(|a, b| a.1.name == b.1.name);
        out
    }

    /// Every exported top-level name, sorted.
    pub fn exported_names(&self) -> Vec<String> {
        let mut names = BTreeSet::new();
        for file in &self.files {
            let types = file.types.iter().map(|t| &t.name);
            let funcs = file
                .funcs
                .iter()
                .filter(|f| f.receiver.is_none())
                .map(|f| &f.name);
            let values = file.consts.iter().chain(file.vars.iter()).map(|v| &v.name);
            for name in types.chain(funcs).chain(values) {
                if exported(name) {
                    names.insert(name.clone());
                }
            }
        }
        names.into_iter().collect()
    }
}

/// What a bare identifier at file scope stands for.
#[derive(Debug, Clone)]
pub enum FileSymbol {
    Type {
        pkg: Arc<Package>,
        name: String,
    },
    Func {
        pkg: Arc<Package>,
        name: String,
    },
    Value {
        pkg: Arc<Package>,
        name: String,
    },
    /// An imported package, by import path.
    Import {
        path: String,
    },
    /// A predeclared type, function or constant.
    Builtin,
}

#[derive(Debug)]
struct Parsed {
    stamp: Stamp,
    file: Arc<GoFile>,
    used: u64,
}

#[derive(Debug)]
struct CachedPackage {
    pkg: Arc<Package>,
    dir_stamp: Option<SystemTime>,
    /// Files of the directory as read, with their stamps; open buffers have
    /// none.
    files: Vec<(PathBuf, Option<Stamp>)>,
    open_gen: u64,
    used: u64,
}

#[derive(Debug)]
struct Index {
    std: Vec<String>,
    module: Vec<String>,
}

/// The project and package lists of a workspace, built off the UI thread.
#[derive(Debug)]
pub struct Built {
    project: Option<GoProject>,
    integrations: Vec<GoIntegration>,
    std: Vec<String>,
    module: Vec<String>,
    parsed: Vec<(PathBuf, Stamp, Arc<GoFile>)>,
}

#[derive(Debug)]
pub struct GoEngine {
    root: PathBuf,
    project: Option<GoProject>,
    integrations: Vec<GoIntegration>,
    /// GOROOT to use when there is no project.
    goroot: Option<PathBuf>,
    ctx: BuildContext,
    open: HashMap<PathBuf, Arc<GoFile>>,
    /// Text of the open buffers, for searches that read other files.
    open_text: HashMap<PathBuf, Arc<str>>,
    /// Bumped per directory when one of its open buffers changes.
    open_gens: HashMap<PathBuf, u64>,
    parsed: HashMap<PathBuf, Parsed>,
    packages: HashMap<(PathBuf, bool), CachedPackage>,
    tick: u64,
    file_cap: usize,
    index: Option<Index>,
    lazy_std: OnceLock<Vec<String>>,
    lazy_module: OnceLock<Vec<String>>,
    building: bool,
    built_at: Option<Instant>,
}

impl GoEngine {
    pub fn index_stats(&self) -> crate::LanguageIndexStats {
        let package_symbols = self
            .index
            .as_ref()
            .map_or(0, |index| index.std.len() + index.module.len());
        let source_bytes = self.open_text.values().map(|text| text.len()).sum();
        crate::LanguageIndexStats {
            files: self.parsed.len(),
            symbols: package_symbols,
            source_bytes,
            heap_lower_bound_bytes: self.parsed.len() * std::mem::size_of::<Parsed>()
                + package_symbols * std::mem::size_of::<String>()
                + source_bytes,
            ..Default::default()
        }
    }

    pub fn new(root: &Path) -> Self {
        let mut engine = Self::with_project(GoProject::detect(root));
        engine.root = root.to_path_buf();
        engine
    }

    pub fn with_project(project: Option<GoProject>) -> Self {
        let root = project
            .as_ref()
            .map(|p| p.root().to_path_buf())
            .unwrap_or_default();
        let goroot = if project.is_none() {
            discover_goroot()
        } else {
            None
        };
        Self {
            root,
            project,
            integrations: Vec::new(),
            goroot,
            ctx: BuildContext::host(),
            open: HashMap::new(),
            open_text: HashMap::new(),
            open_gens: HashMap::new(),
            parsed: HashMap::new(),
            packages: HashMap::new(),
            tick: 0,
            file_cap: MAX_CACHED_FILES,
            index: None,
            lazy_std: OnceLock::new(),
            lazy_module: OnceLock::new(),
            building: false,
            built_at: None,
        }
    }

    /// Sets the project root of an engine that has no project yet.
    pub(super) fn set_root(&mut self, root: &Path) {
        self.root = root.to_path_buf();
    }

    pub fn project(&self) -> Option<&GoProject> {
        self.project.as_ref()
    }

    /// Frameworks and libraries declared by the project's module files.
    pub fn integrations(&self) -> &[GoIntegration] {
        &self.integrations
    }

    /// Routes found in the workspace's indexed or open Go source files.
    /// Open text wins so route navigation reflects unsaved editor changes.
    pub fn routes(&mut self) -> Vec<GoRoute> {
        let fiber = self
            .integrations
            .iter()
            .any(|integration| integration.module.starts_with("github.com/gofiber/fiber/"));
        let goravel = self
            .integrations
            .iter()
            .any(|integration| integration.module == "github.com/goravel/framework");
        let gin = self
            .integrations
            .iter()
            .any(|integration| integration.module == "github.com/gin-gonic/gin");
        if !fiber && !gin && !goravel {
            return Vec::new();
        }
        let mut files: Vec<PathBuf> = self
            .module_package_paths()
            .iter()
            .filter_map(|path| self.project.as_ref()?.import_dir(path))
            .flat_map(|dir| go_files(&dir, &self.ctx))
            .collect();
        files.extend(self.open_text.keys().cloned());
        files.sort();
        files.dedup();
        files
            .into_iter()
            .flat_map(|path| {
                let text = self
                    .open_text(&path)
                    .map(|text| text.to_string())
                    .or_else(|| std::fs::read_to_string(&path).ok());
                text.map_or_else(Vec::new, |text| {
                    let mut routes = Vec::new();
                    if fiber {
                        routes.extend(scan_fiber_routes(&text).into_iter().map(|route| {
                            GoRoute::Fiber {
                                file: path.clone(),
                                route,
                            }
                        }));
                    }
                    if gin {
                        routes.extend(scan_gin_routes(&text).into_iter().map(|route| {
                            GoRoute::Gin {
                                file: path.clone(),
                                route,
                            }
                        }));
                    }
                    if goravel {
                        routes.extend(scan_goravel_routes(&text).into_iter().map(|route| {
                            GoRoute::Goravel {
                                file: path.clone(),
                                route,
                            }
                        }));
                    }
                    routes
                })
            })
            .collect()
    }

    /// Literal configuration keys from a detected Goravel workspace.
    pub fn config_entries(&mut self) -> Vec<GoConfigEntry> {
        if !self
            .integrations
            .iter()
            .any(|integration| integration.module == "github.com/goravel/framework")
        {
            return Vec::new();
        }
        let mut files: Vec<PathBuf> = self
            .module_package_paths()
            .iter()
            .filter_map(|path| self.project.as_ref()?.import_dir(path))
            .flat_map(|dir| go_files(&dir, &self.ctx))
            .collect();
        files.extend(self.open_text.keys().cloned());
        files.sort();
        files.dedup();
        files
            .into_iter()
            .flat_map(|file| {
                let text = self
                    .open_text(&file)
                    .map(|text| text.to_string())
                    .or_else(|| std::fs::read_to_string(&file).ok());
                text.map_or_else(Vec::new, |text| {
                    scan_goravel_config(&text)
                        .into_iter()
                        .map(|entry| GoConfigEntry {
                            file: file.clone(),
                            entry,
                        })
                        .collect()
                })
            })
            .collect()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Registers the text of an open buffer; it wins over the file on disk.
    pub fn open_document(&mut self, path: &Path, text: &str) {
        let file = Arc::new(parse_file(path, text));
        self.open.insert(path.to_path_buf(), file);
        self.open_text.insert(path.to_path_buf(), Arc::from(text));
        self.bump(path);
    }

    /// The text registered by [`GoEngine::open_document`].
    pub fn open_text(&self, path: &Path) -> Option<Arc<str>> {
        self.open_text.get(path).cloned()
    }

    /// Paths of the open buffers.
    pub fn open_paths(&self) -> Vec<PathBuf> {
        self.open_text.keys().cloned().collect()
    }

    pub fn close_document(&mut self, path: &Path) {
        self.open_text.remove(path);
        if self.open.remove(path).is_some() {
            self.bump(path);
        }
    }

    /// Forgets what was read from disk and detects the project again, e.g.
    /// after `go.mod` or files changed. Open buffers stay.
    pub fn reset_index(&mut self) {
        if !self.root.as_os_str().is_empty() {
            self.project = match &self.project {
                Some(old) => GoProject::detect_with(
                    &self.root,
                    old.goroot().map(Path::to_path_buf),
                    old.modcache().map(Path::to_path_buf),
                ),
                None => GoProject::detect(&self.root),
            };
        }
        self.integrations = self.project.as_ref().map_or_else(Vec::new, detect_all);
        self.parsed.clear();
        self.packages.clear();
        self.index = None;
        self.lazy_std = OnceLock::new();
        self.lazy_module = OnceLock::new();
    }

    /// The index was installed at least once.
    pub fn is_ready(&self) -> bool {
        self.built_at.is_some()
    }

    /// `true` when the caller should run [`GoEngine::build`] and
    /// [`GoEngine::install`]: no index yet, or one older than `max_age`.
    /// Asks once until installed.
    pub fn wants_index(&mut self, max_age: Duration) -> bool {
        if self.building {
            return false;
        }
        let stale = self.built_at.is_none_or(|at| at.elapsed() > max_age);
        self.building = stale;
        stale
    }

    pub fn install(&mut self, built: Built) {
        if let Some(project) = built.project {
            self.project = Some(project);
        }
        self.integrations = built.integrations;
        self.index = Some(Index {
            std: built.std,
            module: built.module,
        });
        for (path, stamp, file) in built.parsed {
            self.tick += 1;
            let newer = self.parsed.get(&path).is_some_and(|p| p.stamp != stamp);
            if !newer {
                self.parsed.insert(
                    path,
                    Parsed {
                        stamp,
                        file,
                        used: self.tick,
                    },
                );
            }
        }
        self.evict_files();
        self.building = false;
        self.built_at = Some(Instant::now());
    }

    /// Finds the project, lists the standard library and module packages and
    /// parses the module's own files. Slow with a big module, so callers run
    /// it on a worker thread without holding the engine.
    pub fn build(root: &Path) -> Built {
        let Some(project) = GoProject::detect(root) else {
            return Built {
                project: None,
                integrations: Vec::new(),
                std: Vec::new(),
                module: Vec::new(),
                parsed: Vec::new(),
            };
        };
        let std = project.std_packages();
        let module = module_paths(&project);
        let integrations = detect_all(&project);
        let ctx = BuildContext::host();
        let mut parsed = Vec::new();
        'packages: for path in &module {
            let Some(dir) = project.import_dir(path) else {
                continue;
            };
            for file in go_files(&dir, &ctx) {
                if parsed.len() >= PREPARSE_FILES {
                    break 'packages;
                }
                if let Some((stamp, parsed_file)) = read_file(&file) {
                    parsed.push((file, stamp, parsed_file));
                }
            }
        }
        Built {
            project: Some(project),
            integrations,
            std,
            module,
            parsed,
        }
    }

    /// Importable standard library packages, sorted. Empty without GOROOT.
    pub fn std_package_paths(&self) -> &[String] {
        if let Some(index) = &self.index {
            return &index.std;
        }
        self.lazy_std.get_or_init(|| {
            self.project
                .as_ref()
                .map(GoProject::std_packages)
                .unwrap_or_default()
        })
    }

    /// Packages of the workspace modules, sorted.
    pub fn module_package_paths(&self) -> &[String] {
        if let Some(index) = &self.index {
            return &index.module;
        }
        self.lazy_module
            .get_or_init(|| self.project.as_ref().map(module_paths).unwrap_or_default())
    }

    /// The package `import_path`, from the project, the module cache or
    /// GOROOT.
    pub fn package_by_import(&mut self, import_path: &str) -> Option<Arc<Package>> {
        let dir = self.import_dir(import_path)?;
        self.package_in(import_path, &dir, false)
    }

    /// The package in `dir`, open buffers applied.
    pub fn package_at_dir(&mut self, dir: &Path) -> Option<Arc<Package>> {
        let import_path = self.import_path_of(dir);
        self.package_in(&import_path, dir, false)
    }

    /// The package holding `path`; for a `_test.go` file it includes the
    /// test files of the directory.
    pub fn package_of_file(&mut self, path: &Path) -> Option<Arc<Package>> {
        let dir = path.parent()?;
        let tests = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with("_test.go"));
        let import_path = self.import_path_of(dir);
        self.package_in(&import_path, dir, tests)
    }

    /// Local name to import path for the imports of `file`.
    pub fn import_names(&mut self, file: &GoFile) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for import in &file.imports {
            let name = match import.alias.as_deref() {
                Some("_" | ".") => continue,
                Some(alias) if !alias.is_empty() => alias.to_owned(),
                _ => self
                    .package_by_import(&import.path)
                    .map(|p| p.name.clone())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| guess_name(&import.path)),
            };
            out.entry(name).or_insert_with(|| import.path.clone());
        }
        out
    }

    /// What the bare identifier `name` means in `file`, a file of `pkg`: a
    /// package-level declaration, an imported package or a predeclared name.
    pub fn lookup_in_file_scope(
        &mut self,
        file: &GoFile,
        pkg: &Package,
        name: &str,
    ) -> Option<FileSymbol> {
        if name.is_empty() || name == "_" {
            return None;
        }
        let owned = || Arc::new(pkg.clone());
        if pkg.type_decl(name).is_some() {
            return Some(FileSymbol::Type {
                pkg: owned(),
                name: name.to_owned(),
            });
        }
        if pkg.func(name).is_some() {
            return Some(FileSymbol::Func {
                pkg: owned(),
                name: name.to_owned(),
            });
        }
        if pkg.value(name).is_some() {
            return Some(FileSymbol::Value {
                pkg: owned(),
                name: name.to_owned(),
            });
        }
        let universe = is_universe(name);
        // A predeclared name is only worth resolving imports for when one
        // could bind it.
        let may_import = !universe
            || file.imports.iter().any(|i| match i.alias.as_deref() {
                Some(alias) => alias == name,
                None => guess_name(&i.path) == name,
            });
        if may_import && let Some(path) = self.import_names(file).remove(name) {
            return Some(FileSymbol::Import { path });
        }
        universe.then_some(FileSymbol::Builtin)
    }

    // ---- internals --------------------------------------------------

    fn bump(&mut self, path: &Path) {
        if let Some(dir) = path.parent() {
            *self.open_gens.entry(dir.to_path_buf()).or_insert(0) += 1;
        }
    }

    fn gen_of(&self, dir: &Path) -> u64 {
        self.open_gens.get(dir).copied().unwrap_or(0)
    }

    fn import_dir(&self, import_path: &str) -> Option<PathBuf> {
        if let Some(project) = &self.project {
            return project.import_dir(import_path);
        }
        let src = self.goroot.as_ref()?.join("src");
        if import_path.is_empty()
            || import_path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return None;
        }
        let dotted = import_path
            .split('/')
            .next()
            .is_some_and(|first| first.contains('.'));
        let dir = if dotted {
            src.join("vendor").join(import_path)
        } else {
            src.join(import_path)
        };
        dir.is_dir().then_some(dir)
    }

    fn import_path_of(&self, dir: &Path) -> String {
        self.project
            .as_ref()
            .and_then(|p| p.package_path_of_dir(dir))
            .unwrap_or_else(|| dir.to_string_lossy().into_owned())
    }

    /// The parsed file at `path` with the stamp it was read at; open
    /// buffers have none.
    fn load(&mut self, path: &Path) -> Option<(Arc<GoFile>, Option<Stamp>)> {
        if let Some(file) = self.open.get(path) {
            return Some((file.clone(), None));
        }
        let stamp = stamp_of(path)?;
        self.tick += 1;
        if let Some(cached) = self.parsed.get_mut(path)
            && cached.stamp == stamp
        {
            cached.used = self.tick;
            return Some((cached.file.clone(), Some(stamp)));
        }
        let (stamp, file) = read_file(path)?;
        self.parsed.insert(
            path.to_path_buf(),
            Parsed {
                stamp,
                file: file.clone(),
                used: self.tick,
            },
        );
        self.evict_files();
        Some((file, Some(stamp)))
    }

    fn evict_files(&mut self) {
        if self.parsed.len() <= self.file_cap {
            return;
        }
        let mut order: Vec<(u64, PathBuf)> = self
            .parsed
            .iter()
            .map(|(path, p)| (p.used, path.clone()))
            .collect();
        order.sort();
        let drop = (self.parsed.len() - self.file_cap).max(self.file_cap / 4);
        for (_, path) in order.into_iter().take(drop) {
            self.parsed.remove(&path);
        }
    }

    fn evict_packages(&mut self) {
        if self.packages.len() <= MAX_CACHED_PACKAGES {
            return;
        }
        let mut order: Vec<(u64, (PathBuf, bool))> = self
            .packages
            .iter()
            .map(|(key, p)| (p.used, key.clone()))
            .collect();
        order.sort();
        for (_, key) in order.into_iter().take(MAX_CACHED_PACKAGES / 4) {
            self.packages.remove(&key);
        }
    }

    fn cached_package(&mut self, key: &(PathBuf, bool)) -> Option<Arc<Package>> {
        let entry = self.packages.get(key)?;
        let fresh = entry.open_gen == self.gen_of(&key.0)
            && entry.dir_stamp == dir_stamp(&key.0)
            && entry
                .files
                .iter()
                .all(|(path, stamp)| stamp.is_none() || stamp_of(path) == *stamp);
        if !fresh {
            return None;
        }
        self.tick += 1;
        let entry = self.packages.get_mut(key)?;
        entry.used = self.tick;
        Some(entry.pkg.clone())
    }

    fn package_in(&mut self, import_path: &str, dir: &Path, tests: bool) -> Option<Arc<Package>> {
        let key = (dir.to_path_buf(), tests);
        if let Some(pkg) = self.cached_package(&key) {
            return Some(pkg);
        }
        let open_gen = self.gen_of(dir);
        let dir_stamp = dir_stamp(dir);
        let loaded = self.package_files(dir, tests);
        if loaded.is_empty() {
            self.packages.remove(&key);
            return None;
        }
        let name = package_name(&loaded, import_path);
        let files: Vec<(PathBuf, Option<Stamp>)> = loaded
            .iter()
            .map(|(file, stamp)| (file.path.clone(), *stamp))
            .collect();
        let test_name = format!("{name}_test");
        let kept: Vec<Arc<GoFile>> = loaded
            .into_iter()
            .map(|(file, _)| file)
            .filter(|f| f.package.is_empty() || f.package == name || f.package == test_name)
            .collect();
        let pkg = Arc::new(Package {
            import_path: import_path.to_owned(),
            name,
            dir: dir.to_path_buf(),
            files: kept,
        });
        self.tick += 1;
        self.packages.insert(
            key,
            CachedPackage {
                pkg: pkg.clone(),
                dir_stamp,
                files,
                open_gen,
                used: self.tick,
            },
        );
        self.evict_packages();
        Some(pkg)
    }

    /// The files of `dir` in the build, open buffers applied, parsed.
    fn package_files(&mut self, dir: &Path, tests: bool) -> Vec<(Arc<GoFile>, Option<Stamp>)> {
        let ctx = if tests {
            self.ctx.clone().with_tests()
        } else {
            self.ctx.clone()
        };
        let mut paths = go_files(dir, &ctx);
        for (path, file) in &self.open {
            if path.parent() != Some(dir) {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let keeps = file_matches(&ctx, name, "")
                && file
                    .build_constraint
                    .as_deref()
                    .is_none_or(|expr| eval_constraint(expr, &ctx));
            if keeps {
                if !paths.contains(path) {
                    paths.push(path.clone());
                }
            } else {
                paths.retain(|p| p != path);
            }
        }
        paths.sort();
        paths.dedup();
        paths.truncate(MAX_PACKAGE_FILES);
        paths.iter().filter_map(|p| self.load(p)).collect()
    }
}

fn module_paths(project: &GoProject) -> Vec<String> {
    let mut all: Vec<String> = project
        .modules()
        .iter()
        .flat_map(|m| project.module_packages(m))
        .collect();
    all.sort();
    all.dedup();
    all
}

fn stamp_of(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    meta.is_file().then(|| (meta.modified().ok(), meta.len()))
}

fn dir_stamp(dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(dir).and_then(|m| m.modified()).ok()
}

/// Reads and parses a file; large or unreadable files give `None`.
fn read_file(path: &Path) -> Option<(Stamp, Arc<GoFile>)> {
    let stamp = stamp_of(path)?;
    if stamp.1 > MAX_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    Some((stamp, Arc::new(parse_file(path, &text))))
}

/// The most common package clause that is neither `main` nor a test
/// package; else the most common clause with `_test` removed; else a guess
/// from the import path.
fn package_name(files: &[(Arc<GoFile>, Option<Stamp>)], import_path: &str) -> String {
    let pick = |skip_special: bool| {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for (file, _) in files {
            let clause = file.package.as_str();
            let clause = if skip_special {
                clause
            } else {
                clause.strip_suffix("_test").unwrap_or(clause)
            };
            if clause.is_empty()
                || (skip_special && (clause == "main" || clause.ends_with("_test")))
            {
                continue;
            }
            *counts.entry(clause).or_insert(0) += 1;
        }
        counts
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
            .map(|(name, _)| name.to_owned())
    };
    pick(true)
        .or_else(|| pick(false))
        .unwrap_or_else(|| guess_name(import_path))
}

fn is_major(text: &str) -> bool {
    text.strip_prefix('v')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The package name an import path suggests: its last element, or the one
/// before a `/vN` suffix, without a `.vN` suffix (`gopkg.in/yaml.v3`).
fn guess_name(import_path: &str) -> String {
    let mut parts = import_path.rsplit('/');
    let last = parts.next().unwrap_or("");
    let element = match parts.next() {
        Some(before) if is_major(last) && !before.is_empty() => before,
        _ => last,
    };
    match element.rsplit_once('.') {
        Some((head, tail)) if !head.is_empty() && is_major(tail) => head.to_owned(),
        _ => element.to_owned(),
    }
}

fn is_universe(name: &str) -> bool {
    TypeExpr::is_builtin(name) || UNIVERSE_FUNCS.contains(&name) || UNIVERSE_CONSTS.contains(&name)
}
