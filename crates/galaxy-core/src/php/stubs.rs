//! PHP's own classes and functions (`Exception`, `ArrayAccess`, `strlen`,
//! `DateTime`, ...), read from official php-src declarations embedded in the
//! binary. Each selected PHP baseline is parsed once per process.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use super::composer::ClassName;
use super::parse::{ClassInfo, FunctionInfo, parse_file};
use super::stub_files::FILES;
use super::version::PhpVersion;

/// `empty()` and `isset()` look like functions but are language constructs,
/// so php-src declarations leave them out. Declared here so they can be
/// hovered and opened like any other call.
const CONSTRUCTS: (&str, &str) = (
    "Core/constructs.php",
    r#"<?php

/**
 * Determine whether a variable is considered empty. A variable is empty if it
 * does not exist or its value equals `false`. No warning is raised when the
 * variable does not exist.
 *
 * This is a language construct, not a function.
 */
function empty(mixed $var): bool {}

/**
 * Determine if a variable is declared and is different than `null`. Several
 * variables can be checked at once; the result is `true` only if all are set.
 *
 * This is a language construct, not a function.
 */
function isset(mixed $var, mixed ...$vars): bool {}

/**
 * Destroy the specified variables. Several can be given at once. Array
 * elements and object properties can be unset too.
 *
 * This is a language construct, not a function.
 */
function unset(mixed $var, mixed ...$vars): void {}
"#,
);

/// Prefix of the pseudo paths given to stub declarations. They do not
/// exist on disk, so navigation must not try to open them.
const SCHEME: &str = "phpstub:";

#[derive(Debug, Default)]
pub struct Stubs {
    classes: HashMap<String, Arc<ClassInfo>>,
    functions: HashMap<String, Arc<FunctionInfo>>,
    names: Vec<ClassName>,
}

impl Stubs {
    /// Parsed stubs, shared by every engine. The first call parses them
    /// all, so run it on a worker thread.
    pub fn for_version(version: PhpVersion) -> &'static Stubs {
        static STUBS: [OnceLock<Stubs>; 6] = [
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
        ];
        let index = match version {
            PhpVersion::V80 => 0,
            PhpVersion::V81 => 1,
            PhpVersion::V82 => 2,
            PhpVersion::V83 => 3,
            PhpVersion::V84 => 4,
            PhpVersion::V85 => 5,
        };
        STUBS[index].get_or_init(|| Stubs::parse(FILES[index]))
    }

    fn parse(files: &[(&str, &str)]) -> Stubs {
        let mut stubs = Stubs::default();
        for (name, text) in files.iter().chain(std::iter::once(&CONSTRUCTS)) {
            let path = PathBuf::from(format!("{SCHEME}{name}"));
            let file = parse_file(&path, text);
            for class in &file.classes {
                let key = class.fqn.to_lowercase();
                if stubs.classes.contains_key(&key) {
                    continue;
                }
                stubs.names.push(ClassName {
                    short: class.name.clone(),
                    fqn: class.fqn.clone(),
                    vendor: true,
                });
                stubs.classes.insert(key, class.clone());
            }
            for function in &file.functions {
                stubs
                    .functions
                    .entry(function.fqn.to_lowercase())
                    .or_insert_with(|| function.clone());
            }
        }
        stubs
    }

    pub fn class(&self, fqn: &str) -> Option<Arc<ClassInfo>> {
        self.classes.get(&fqn.to_lowercase()).cloned()
    }

    pub fn function(&self, fqn: &str) -> Option<Arc<FunctionInfo>> {
        self.functions.get(&fqn.to_lowercase()).cloned()
    }

    pub fn functions(&self) -> impl Iterator<Item = &Arc<FunctionInfo>> {
        self.functions.values()
    }

    pub fn class_names(&self) -> &[ClassName] {
        &self.names
    }
}

/// Names of global functions declared by the newest bundled PHP baseline.
/// Syntax highlighting cannot depend on the open project's Composer version,
/// so use the broadest supported built-in set.
pub fn builtin_function_names() -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names = Stubs::for_version(PhpVersion::V85)
            .functions()
            // Global PHP functions have a leading namespace separator, e.g.
            // `\\strlen`; namespaced extension APIs have another separator.
            .filter(|function| !function.fqn.trim_start_matches('\\').contains('\\'))
            .map(|function| function.name.to_ascii_lowercase())
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        names
    })
}

/// `true` for paths of stub declarations.
pub fn is_stub_path(path: &Path) -> bool {
    path.to_str().is_some_and(|path| path.starts_with(SCHEME))
}

/// Writes the stub file behind `path` to disk and returns where, so
/// navigation can open it. `None` when `path` is not a stub.
pub fn materialize(path: &Path) -> Option<PathBuf> {
    let name = path.to_str()?.strip_prefix(SCHEME)?;
    let (_, text) = FILES
        .iter()
        .flat_map(|files| files.iter())
        .chain(std::iter::once(&CONSTRUCTS))
        .find(|(file, _)| *file == name)?;
    let target = std::env::temp_dir().join("galaxy-php-stubs").join(name);
    if std::fs::read_to_string(&target).ok().as_deref() != Some(*text) {
        std::fs::create_dir_all(target.parent()?).ok()?;
        std::fs::write(&target, text).ok()?;
    }
    Some(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_names_include_global_core_and_extension_functions() {
        let names = builtin_function_names();
        assert!(names.binary_search(&"strlen".to_string()).is_ok());
        assert!(names.binary_search(&"sprintf".to_string()).is_ok());
        assert!(names.binary_search(&"preg_quote".to_string()).is_ok());
    }
}
