//! Go module graph and import path to directory resolution.
//!
//! Reads `go.mod`, `go.work`, `go.sum`, `go.work.sum` and `vendor/modules.txt`
//! as plain text, plus the GOROOT and module cache layouts on disk, so a
//! project resolves imports without running the `go` tool.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

mod env;
mod modfile;
mod paths;
mod walk;

pub use env::{discover_goroot, discover_goroot_from, discover_modcache, discover_modcache_from};
pub use paths::escape_module_path;

use modfile::{
    ModFile, Replace, Target, Vendor, cmp_version, merge_max, read_mod_file, read_sums, read_vendor,
};
use paths::{
    join_rest, normalize, relative_slashes, rest_after, unescape_module_path, valid_import_path,
};
use walk::{Walk, collect_packages};

const MODULE_PACKAGE_CAP: usize = 5000;
const DEPENDENCY_PACKAGE_CAP: usize = 2000;

/// A Go module of the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    pub path: String,
    pub dir: PathBuf,
    pub go_version: Option<String>,
}

/// A Go project: the main module or workspace and where its imports live.
#[derive(Debug, Clone)]
pub struct GoProject {
    root: PathBuf,
    modules: Vec<Module>,
    main: Option<usize>,
    goroot: Option<PathBuf>,
    modcache: Option<PathBuf>,
    requires: Vec<(String, String)>,
    /// Modules and versions known from `go.sum` only, longest path first
    /// after `requires`.
    cache_mods: Vec<(String, String)>,
    replaces: Vec<Replace>,
    vendor: Option<Vendor>,
    std: OnceLock<Vec<String>>,
}

impl GoProject {
    /// The project around `root`, with GOROOT and the module cache taken
    /// from the environment.
    pub fn detect(root: &Path) -> Option<GoProject> {
        Self::detect_with(root, discover_goroot(), discover_modcache())
    }

    /// Like [`GoProject::detect`] with explicit GOROOT and module cache.
    pub fn detect_with(
        root: &Path,
        goroot: Option<PathBuf>,
        modcache: Option<PathBuf>,
    ) -> Option<GoProject> {
        let root = normalize(root);
        let start = if root.is_file() {
            root.parent()?
        } else {
            &root
        };
        let found = start
            .ancestors()
            .find(|d| d.join("go.work").is_file() || d.join("go.mod").is_file())?
            .to_path_buf();

        let mut work_dir = None;
        let mut work = ModFile::default();
        if found.join("go.work").is_file() {
            work = read_mod_file(&found.join("go.work"), &found);
            work_dir = Some(found.clone());
        } else if let Some(up) = found
            .ancestors()
            .skip(1)
            .find(|d| d.join("go.work").is_file())
        {
            let parsed = read_mod_file(&up.join("go.work"), up);
            if parsed.uses.contains(&found) {
                work = parsed;
                work_dir = Some(up.to_path_buf());
            }
        }

        let mut dirs = work.uses.clone();
        if dirs.is_empty() && found.join("go.mod").is_file() {
            dirs.push(found.clone());
        }
        let mut seen = HashSet::new();
        let mut modules = Vec::new();
        let mut files = Vec::new();
        for dir in dirs {
            if !seen.insert(dir.clone()) {
                continue;
            }
            let file = read_mod_file(&dir.join("go.mod"), &dir);
            let Some(path) = file.module.clone() else {
                continue;
            };
            modules.push(Module {
                path,
                dir,
                go_version: file.go.clone(),
            });
            files.push(file);
        }

        let load_dir = work_dir.clone().unwrap_or_else(|| found.clone());
        let mut required: HashMap<String, String> = HashMap::new();
        let mut summed: HashMap<String, String> = HashMap::new();
        let mut replaces = std::mem::take(&mut work.replaces);
        for file in files {
            for (path, version) in file.requires {
                merge_max(&mut required, path, version);
            }
            replaces.extend(file.replaces);
        }
        if work_dir.is_some() {
            read_sums(&load_dir.join("go.work.sum"), &mut summed);
        }
        for module in &modules {
            read_sums(&module.dir.join("go.sum"), &mut summed);
        }
        let mut requires: Vec<(String, String)> = required.into_iter().collect();
        requires.sort();
        let mut cache_mods = requires.clone();
        let mut extra: Vec<(String, String)> = summed
            .into_iter()
            .filter(|(path, _)| !requires.iter().any(|(p, _)| p == path))
            .collect();
        extra.sort();
        cache_mods.extend(extra);
        cache_mods.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));

        let main = modules
            .iter()
            .position(|m| m.dir == found)
            .or_else(|| modules.iter().position(|m| found.starts_with(&m.dir)))
            .or(if modules.is_empty() { None } else { Some(0) });
        let vendor = read_vendor(&load_dir);
        Some(GoProject {
            root: found,
            modules,
            main,
            goroot,
            modcache,
            requires,
            cache_mods,
            replaces,
            vendor,
            std: OnceLock::new(),
        })
    }

    /// The nearest directory holding `go.mod` or `go.work`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn main_module(&self) -> Option<&Module> {
        self.main.and_then(|i| self.modules.get(i))
    }

    /// The main module, or every module of a workspace.
    pub fn modules(&self) -> &[Module] {
        &self.modules
    }

    pub fn goroot(&self) -> Option<&Path> {
        self.goroot.as_deref()
    }

    pub fn modcache(&self) -> Option<&Path> {
        self.modcache.as_deref()
    }

    /// The innermost workspace module that contains `file`.
    pub fn module_for_file(&self, file: &Path) -> Option<&Module> {
        self.modules
            .iter()
            .filter(|m| file.starts_with(&m.dir))
            .max_by_key(|m| m.dir.components().count())
    }

    /// The directory holding the package `import_path`, if it exists.
    pub fn import_dir(&self, import_path: &str) -> Option<PathBuf> {
        if !valid_import_path(import_path) {
            return None;
        }
        if let Some(goroot) = &self.goroot {
            let src = goroot.join("src");
            let dotted = import_path
                .split('/')
                .next()
                .is_some_and(|f| f.contains('.'));
            let dir = if dotted {
                src.join("vendor").join(import_path)
            } else {
                src.join(import_path)
            };
            if dir.is_dir() {
                return Some(dir);
            }
        }
        let mut owners: Vec<(&Module, &str)> = self
            .modules
            .iter()
            .filter_map(|m| rest_after(import_path, &m.path).map(|rest| (m, rest)))
            .collect();
        owners.sort_by_key(|(m, _)| std::cmp::Reverse(m.path.len()));
        for (module, rest) in owners {
            let dir = join_rest(&module.dir, rest);
            if dir.is_dir() {
                return Some(dir);
            }
        }
        let mut replaced: Vec<(&Replace, &str)> = self
            .replaces
            .iter()
            .filter(|r| self.replace_applies(r))
            .filter_map(|r| rest_after(import_path, &r.old).map(|rest| (r, rest)))
            .collect();
        replaced.sort_by_key(|(r, _)| std::cmp::Reverse(r.old.len()));
        for (replace, rest) in replaced {
            if let Some(dir) = self.target_dir(&replace.target) {
                let dir = join_rest(&dir, rest);
                if dir.is_dir() {
                    return Some(dir);
                }
            }
        }
        if let Some(vendor) = &self.vendor {
            let listed = vendor
                .packages
                .as_ref()
                .is_none_or(|packages| packages.contains(import_path));
            let dir = vendor.dir.join(import_path);
            if listed && dir.is_dir() {
                return Some(dir);
            }
        }
        for (path, version) in &self.cache_mods {
            let Some(rest) = rest_after(import_path, path) else {
                continue;
            };
            if let Some(dir) = self.cache_dir(path, version) {
                let dir = join_rest(&dir, rest);
                if dir.is_dir() {
                    return Some(dir);
                }
            }
        }
        None
    }

    /// The import path of the package in `dir`: inside a workspace module,
    /// the standard library or the module cache.
    pub fn package_path_of_dir(&self, dir: &Path) -> Option<String> {
        let module = self
            .modules
            .iter()
            .filter(|m| dir.starts_with(&m.dir))
            .max_by_key(|m| m.dir.components().count());
        if let Some(module) = module {
            let rel = relative_slashes(dir.strip_prefix(&module.dir).ok()?)?;
            if rel.is_empty() {
                return Some(module.path.clone());
            }
            if let Some(vendored) = rel.strip_prefix("vendor/") {
                return Some(vendored.to_owned());
            }
            return Some(format!("{}/{rel}", module.path));
        }
        if let Some(goroot) = &self.goroot
            && let Ok(rel) = dir.strip_prefix(goroot.join("src"))
        {
            let rel = relative_slashes(rel)?;
            let rel = rel.strip_prefix("vendor/").unwrap_or(&rel);
            return (!rel.is_empty()).then(|| rel.to_owned());
        }
        self.cache_package_path(dir)
    }

    /// Every importable standard library package, sorted. Walked once.
    pub fn std_packages(&self) -> Vec<String> {
        self.std
            .get_or_init(|| {
                let Some(goroot) = &self.goroot else {
                    return Vec::new();
                };
                let walk = Walk {
                    skip_top: &["cmd"],
                    skip_internal: true,
                    skip_nested: false,
                    cap: usize::MAX,
                };
                let mut out = Vec::new();
                collect_packages(&goroot.join("src"), "", &walk, &mut out);
                out
            })
            .clone()
    }

    /// Import paths of every package directory under `module` (at most 5000).
    pub fn module_packages(&self, module: &Module) -> Vec<String> {
        let walk = Walk {
            skip_top: &[],
            skip_internal: false,
            skip_nested: true,
            cap: MODULE_PACKAGE_CAP,
        };
        let mut out = Vec::new();
        collect_packages(&module.dir, &module.path, &walk, &mut out);
        out
    }

    /// Modules from `require` lines, indirect ones included, sorted by path.
    pub fn required_modules(&self) -> Vec<(String, String)> {
        self.requires.clone()
    }

    /// Packages of the required module `module_path` present on disk (at
    /// most 2000), for import completion. `internal` packages are left out.
    pub fn dependency_packages(&self, module_path: &str) -> Vec<String> {
        let Some(dir) = self.module_root(module_path) else {
            return Vec::new();
        };
        let walk = Walk {
            skip_top: &[],
            skip_internal: true,
            skip_nested: true,
            cap: DEPENDENCY_PACKAGE_CAP,
        };
        let mut out = Vec::new();
        collect_packages(&dir, module_path, &walk, &mut out);
        out
    }

    fn version_of(&self, module: &str) -> Option<&str> {
        self.cache_mods
            .iter()
            .find(|(path, _)| path == module)
            .map(|(_, version)| version.as_str())
    }

    fn replace_applies(&self, replace: &Replace) -> bool {
        replace
            .old_version
            .as_deref()
            .is_none_or(|v| self.version_of(&replace.old) == Some(v))
    }

    /// The cache directory of `module` at `version`. When that exact version
    /// was never downloaded, the nearest cached one stands in: the highest
    /// version not above it, else the lowest above it. The API of a nearby
    /// version is far more useful than no answers at all.
    fn cache_dir(&self, module: &str, version: &str) -> Option<PathBuf> {
        let escaped = escape_module_path(module);
        let exact = self.modcache.as_ref()?.join(format!("{escaped}@{version}"));
        if exact.is_dir() {
            return Some(exact);
        }
        Some(self.nearest_cached(&escaped, version).unwrap_or(exact))
    }

    fn nearest_cached(&self, escaped: &str, version: &str) -> Option<PathBuf> {
        let cache = self.modcache.as_ref()?;
        let (parent, last) = match escaped.rsplit_once('/') {
            Some((head, last)) => (cache.join(head), last),
            None => (cache.clone(), escaped),
        };
        let prefix = format!("{last}@");
        let mut below: Option<(String, PathBuf)> = None;
        let mut above: Option<(String, PathBuf)> = None;
        for entry in std::fs::read_dir(&parent).ok()?.flatten().take(500) {
            let name = entry.file_name();
            let Some(found) = name.to_str().and_then(|n| n.strip_prefix(&prefix)) else {
                continue;
            };
            if !entry.path().is_dir() {
                continue;
            }
            let (slot, wanted) = if cmp_version(found, version) == Ordering::Greater {
                (&mut above, Ordering::Less)
            } else {
                (&mut below, Ordering::Greater)
            };
            let better = slot
                .as_ref()
                .is_none_or(|(best, _)| cmp_version(found, best) == wanted);
            if better {
                *slot = Some((found.to_owned(), entry.path()));
            }
        }
        below.or(above).map(|(_, dir)| dir)
    }

    fn target_dir(&self, target: &Target) -> Option<PathBuf> {
        match target {
            Target::Dir(dir) => Some(dir.clone()),
            Target::Module(path, version) => self.cache_dir(path, version),
        }
    }

    /// Where a required module lives: its replacement, else the cache.
    fn module_root(&self, module: &str) -> Option<PathBuf> {
        let replacement = self
            .replaces
            .iter()
            .filter(|r| r.old == module && self.replace_applies(r))
            .find_map(|r| self.target_dir(&r.target).filter(|d| d.is_dir()));
        let dir = match replacement {
            Some(dir) => dir,
            None => self.cache_dir(module, self.version_of(module)?)?,
        };
        dir.is_dir().then_some(dir)
    }

    fn cache_package_path(&self, dir: &Path) -> Option<String> {
        let rel = dir.strip_prefix(self.modcache.as_ref()?).ok()?;
        let parts: Vec<&str> = rel
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => s.to_str(),
                _ => None,
            })
            .collect();
        let at = parts.iter().position(|p| p.contains('@'))?;
        if parts.first() == Some(&"cache") {
            return None;
        }
        let mut out: Vec<String> = parts[..at]
            .iter()
            .map(|p| unescape_module_path(p))
            .collect();
        let (name, _version) = parts[at].split_once('@')?;
        out.push(unescape_module_path(name));
        out.extend(parts[at + 1..].iter().map(|p| (*p).to_owned()));
        Some(out.join("/"))
    }
}

#[cfg(test)]
mod tests;
