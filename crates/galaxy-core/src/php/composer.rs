//! Class-to-file mapping from Composer's autoload data.
//!
//! Reads `vendor/composer/autoload_psr4.php` and `autoload_classmap.php`
//! (plain generated PHP, read as text) plus the project's own
//! `composer.json`, so a project works before `composer install` too.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

/// A class the autoload data can reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassName {
    pub short: String,
    pub fqn: String,
    /// Lives under `vendor/`, so project classes can rank first.
    pub vendor: bool,
}

#[derive(Debug, Default, Clone)]
pub struct Autoload {
    root: PathBuf,
    /// Files Composer loads on every request (helper functions).
    files: Vec<PathBuf>,
    /// Namespace prefix (with trailing `\`) and its directories, longest
    /// prefix first.
    psr4: Vec<(String, Vec<PathBuf>)>,
    classmap: HashMap<String, PathBuf>,
}

impl Autoload {
    pub fn load(root: &Path) -> Self {
        let mut psr4: HashMap<String, Vec<PathBuf>> = HashMap::new();
        let mut classmap = HashMap::new();
        let composer = root.join("vendor/composer");
        if let Ok(text) = std::fs::read_to_string(composer.join("autoload_psr4.php")) {
            read_psr4_php(&text, root, &mut psr4);
        }
        if let Ok(text) = std::fs::read_to_string(composer.join("autoload_classmap.php")) {
            read_classmap_php(&text, root, &mut classmap);
        }
        let mut files = Vec::new();
        if let Ok(text) = std::fs::read_to_string(composer.join("autoload_files.php")) {
            read_files_php(&text, root, &mut files);
        }
        if let Ok(text) = std::fs::read_to_string(root.join("composer.json")) {
            read_composer_json(&text, root, &mut psr4, &mut files);
        }
        let mut psr4: Vec<_> = psr4.into_iter().collect();
        psr4.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        Self {
            root: root.to_path_buf(),
            files,
            psr4,
            classmap,
        }
    }

    /// No autoload data at all (a project without Composer).
    pub fn is_empty(&self) -> bool {
        self.psr4.is_empty() && self.classmap.is_empty()
    }

    /// Files Composer includes on every request; they declare functions.
    pub fn function_files(&self) -> &[PathBuf] {
        &self.files
    }

    /// The file that should declare `fqn`, if it exists.
    pub fn path_for(&self, fqn: &str) -> Option<PathBuf> {
        let fqn = fqn.trim_start_matches('\\');
        if let Some(path) = self.classmap.get(fqn)
            && path.is_file()
        {
            return Some(path.clone());
        }
        for (prefix, dirs) in &self.psr4 {
            let Some(rest) = fqn.strip_prefix(prefix.as_str()) else {
                continue;
            };
            let relative = format!("{}.php", rest.replace('\\', "/"));
            for dir in dirs {
                let candidate = dir.join(&relative);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    }

    /// Whether the autoload data covers the namespace of `fqn`: a PSR-4
    /// prefix of it, or a class map entry. A class under such a name that
    /// no file declares is missing; under any other name nothing is known.
    pub fn owns(&self, fqn: &str) -> bool {
        let fqn = fqn.trim_start_matches('\\');
        self.classmap.contains_key(fqn)
            || self
                .psr4
                .iter()
                .any(|(prefix, _)| !prefix.is_empty() && fqn.starts_with(prefix.as_str()))
    }

    /// Whether `fqn` names a namespace (a folder of a PSR-4 root) rather
    /// than a class, as in `use App\Models;` followed by `Models\User`.
    pub fn is_namespace(&self, fqn: &str) -> bool {
        let probe = format!("{}\\", fqn.trim_start_matches('\\'));
        for (prefix, dirs) in &self.psr4 {
            if prefix.is_empty() {
                continue;
            }
            if let Some(rest) = probe.strip_prefix(prefix.as_str()) {
                let relative = rest.trim_end_matches('\\').replace('\\', "/");
                if relative.is_empty() || dirs.iter().any(|dir| dir.join(&relative).is_dir()) {
                    return true;
                }
            } else if prefix.starts_with(&probe) {
                return true;
            }
        }
        false
    }

    /// Every class the mapping can reach, found by walking the PSR-4
    /// directories (test folders skipped) and reading the class map.
    pub fn all_classes(&self) -> Vec<ClassName> {
        self.indexed_classes().0
    }

    /// Class names plus the number of unique PHP source files examined while
    /// building them. This includes non-class PHP files in autoload roots so
    /// index telemetry represents work done rather than just declarations.
    pub fn indexed_classes(&self) -> (Vec<ClassName>, usize) {
        let vendor_dir = self.root.join("vendor");
        let mut seen = HashSet::new();
        let mut files = HashSet::new();
        let mut out = Vec::new();
        for (prefix, dirs) in &self.psr4 {
            for dir in dirs {
                let vendor = dir.starts_with(&vendor_dir);
                walk(
                    dir,
                    prefix,
                    &mut |fqn: String, path: PathBuf| {
                        files.insert(path);
                        if !fqn.is_empty() && seen.insert(fqn.clone()) {
                            let short = fqn.rsplit('\\').next().unwrap_or(&fqn).to_owned();
                            out.push(ClassName { short, fqn, vendor });
                        }
                    },
                    0,
                );
            }
        }
        for (fqn, path) in &self.classmap {
            files.insert(path.clone());
            if seen.insert(fqn.clone()) {
                let short = fqn.rsplit('\\').next().unwrap_or(fqn).to_owned();
                out.push(ClassName {
                    short,
                    fqn: fqn.clone(),
                    vendor: path.starts_with(&vendor_dir),
                });
            }
        }
        files.extend(self.files.iter().cloned());
        out.sort_by(|a, b| {
            a.short
                .to_lowercase()
                .cmp(&b.short.to_lowercase())
                .then_with(|| a.fqn.cmp(&b.fqn))
        });
        (out, files.len())
    }
}

fn walk(dir: &Path, namespace: &str, add: &mut impl FnMut(String, PathBuf), depth: usize) {
    if depth > 12 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if name.starts_with('.') || matches!(name, "tests" | "Tests" | "test" | "node_modules")
            {
                continue;
            }
            walk(
                &entry.path(),
                &format!("{namespace}{name}\\"),
                add,
                depth + 1,
            );
        } else if let Some(stem) = name.strip_suffix(".php") {
            let path = entry.path();
            if !stem.contains('.') && stem.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                add(format!("{namespace}{stem}"), path);
            } else {
                // Report all PHP sources traversed by an autoload root even if
                // their basename cannot represent a PSR-4 class name.
                add(String::new(), path);
            }
        }
    }
}

fn unescape(raw: &str) -> String {
    raw.replace("\\\\", "\\")
}

fn base(kind: &str, root: &Path) -> PathBuf {
    if kind == "vendorDir" {
        root.join("vendor")
    } else {
        root.to_path_buf()
    }
}

fn read_psr4_php(text: &str, root: &Path, out: &mut HashMap<String, Vec<PathBuf>>) {
    static LINE: OnceLock<Regex> = OnceLock::new();
    static PATH: OnceLock<Regex> = OnceLock::new();
    let line = LINE
        .get_or_init(|| Regex::new(r"'((?:[^'\\]|\\\\)*)'\s*=>\s*array\((.*)\)").expect("valid"));
    let path =
        PATH.get_or_init(|| Regex::new(r"\$(vendorDir|baseDir)\s*\.\s*'([^']*)'").expect("valid"));
    for caps in line.captures_iter(text) {
        let prefix = unescape(&caps[1]);
        let dirs = out.entry(prefix).or_default();
        for found in path.captures_iter(&caps[2]) {
            let dir = base(&found[1], root).join(found[2].trim_start_matches('/'));
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
}

fn read_files_php(text: &str, root: &Path, out: &mut Vec<PathBuf>) {
    static LINE: OnceLock<Regex> = OnceLock::new();
    let line = LINE
        .get_or_init(|| Regex::new(r"=>\s*\$(vendorDir|baseDir)\s*\.\s*'([^']*)'").expect("valid"));
    for caps in line.captures_iter(text) {
        out.push(base(&caps[1], root).join(caps[2].trim_start_matches('/')));
    }
}

fn read_classmap_php(text: &str, root: &Path, out: &mut HashMap<String, PathBuf>) {
    static LINE: OnceLock<Regex> = OnceLock::new();
    let line = LINE.get_or_init(|| {
        Regex::new(r"'((?:[^'\\]|\\\\)*)'\s*=>\s*\$(vendorDir|baseDir)\s*\.\s*'([^']*)'")
            .expect("valid")
    });
    for caps in line.captures_iter(text) {
        out.insert(
            unescape(&caps[1]),
            base(&caps[2], root).join(caps[3].trim_start_matches('/')),
        );
    }
}

fn read_composer_json(
    text: &str,
    root: &Path,
    out: &mut HashMap<String, Vec<PathBuf>>,
    files: &mut Vec<PathBuf>,
) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    for section in ["autoload", "autoload-dev"] {
        if let Some(map) = value[section]["psr-4"].as_object() {
            for (prefix, dirs) in map {
                let list: Vec<&str> = match dirs {
                    serde_json::Value::String(one) => vec![one.as_str()],
                    serde_json::Value::Array(many) => {
                        many.iter().filter_map(|v| v.as_str()).collect()
                    }
                    _ => continue,
                };
                let entry = out.entry(prefix.clone()).or_default();
                for dir in list {
                    let dir = root.join(dir.trim_end_matches('/'));
                    if !entry.contains(&dir) {
                        entry.push(dir);
                    }
                }
            }
        }
        if let Some(helper_files) = value[section]["files"].as_array() {
            for file in helper_files.iter().filter_map(|value| value.as_str()) {
                let file = root.join(file);
                if !files.contains(&file) {
                    files.push(file);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("vendor/composer")).unwrap();
        std::fs::create_dir_all(root.join("app/Models")).unwrap();
        std::fs::create_dir_all(root.join("vendor/acme/lib/src/Sub")).unwrap();
        std::fs::write(root.join("app/Models/User.php"), "<?php").unwrap();
        std::fs::write(root.join("vendor/acme/lib/src/Sub/Thing.php"), "<?php").unwrap();
        std::fs::write(
            root.join("vendor/composer/autoload_psr4.php"),
            "<?php\nreturn array(\n    'Acme\\\\Lib\\\\' => array($vendorDir . '/acme/lib/src'),\n    'App\\\\' => array($baseDir . '/app'),\n);\n",
        )
        .unwrap();
        std::fs::write(
            root.join("vendor/composer/autoload_classmap.php"),
            "<?php\nreturn array(\n    'Mapped\\\\One' => $vendorDir . '/acme/lib/src/Sub/Thing.php',\n);\n",
        )
        .unwrap();
        std::fs::write(
            root.join("composer.json"),
            r#"{"autoload":{"psr-4":{"App\\":"app/"}}}"#,
        )
        .unwrap();
        dir
    }

    #[test]
    fn maps_classes_to_files() {
        let dir = project();
        let autoload = Autoload::load(dir.path());
        assert_eq!(
            autoload.path_for("App\\Models\\User"),
            Some(dir.path().join("app/Models/User.php"))
        );
        assert_eq!(
            autoload.path_for("\\Acme\\Lib\\Sub\\Thing"),
            Some(dir.path().join("vendor/acme/lib/src/Sub/Thing.php"))
        );
        assert_eq!(
            autoload.path_for("Mapped\\One"),
            Some(dir.path().join("vendor/acme/lib/src/Sub/Thing.php"))
        );
        assert_eq!(autoload.path_for("App\\Missing"), None);
        assert_eq!(autoload.path_for("Other\\User"), None);
    }

    #[test]
    fn lists_reachable_classes() {
        let dir = project();
        let names: Vec<String> = Autoload::load(dir.path())
            .all_classes()
            .into_iter()
            .map(|class| class.fqn)
            .collect();
        assert!(names.contains(&"App\\Models\\User".to_owned()));
        assert!(names.contains(&"Acme\\Lib\\Sub\\Thing".to_owned()));
        assert!(names.contains(&"Mapped\\One".to_owned()));
    }

    #[test]
    fn counts_every_php_file_in_an_autoload_root() {
        let dir = project();
        std::fs::write(dir.path().join("app/helpers.php"), "<?php").unwrap();

        let (_, files) = Autoload::load(dir.path()).indexed_classes();

        assert_eq!(files, 3);
    }

    #[test]
    fn reads_helper_files_from_composer_json() {
        let dir = project();
        std::fs::write(
            dir.path().join("composer.json"),
            r#"{"autoload":{"files":["app/helpers.php"]}}"#,
        )
        .unwrap();
        let helper = dir.path().join("app/helpers.php");

        assert_eq!(Autoload::load(dir.path()).function_files(), [helper]);
    }
}
