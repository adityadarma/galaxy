//! Names TypeScript treats as globals: its own `lib.*.d.ts`, `@types/*`, the
//! workspace's declaration files and the declaration files they reference.
//!
//! The scan is a superset of what `tsconfig.json` selects (`lib`, `types`,
//! `include` are not consulted), so it can only hide a report, never invent
//! one. A root that cannot be scanned reliably yields `None`, and the host
//! then reports no unknown names for it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use galaxy_core::typescript::ambient_global_names;

const MAX_DECLARATION_FILES: usize = 4_000;
const MAX_DECLARATION_BYTES: u64 = 4 * 1024 * 1024;
const MAX_REFERENCE_DEPTH: usize = 8;
const MAX_DIRECTORY_DEPTH: usize = 6;

/// Packages that add globals at build time (auto-imports), which a purely
/// syntactic scan cannot see.
const AUTO_IMPORT_MARKERS: &[&str] = &[
    "\"nuxt\"",
    "\"nuxt3\"",
    "\"@nuxt/",
    "\"@nuxtjs/",
    "unplugin-auto-import",
    "unplugin-vue-components",
];

/// The global names of `root`, or `None` when they cannot be known: the
/// project has no TypeScript installation or generates globals at build time.
pub(super) fn scan_ambient_globals(root: &Path, paths: &[String]) -> Option<HashSet<String>> {
    if uses_auto_imports(root) {
        return None;
    }
    let lib = typescript_lib_dir(root)?;
    let mut scan = Scan {
        root,
        names: HashSet::new(),
        visited: HashSet::new(),
        files: 0,
    };
    let mut libs = std::fs::read_dir(&lib)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("lib.") && name.ends_with(".d.ts"))
        })
        .collect::<Vec<_>>();
    libs.sort();
    for file in libs {
        scan.file(&file, 0);
    }
    for directory in root.ancestors() {
        scan.directory(&directory.join("node_modules/@types"), 0);
    }
    for path in paths.iter().filter(|path| path.ends_with(".d.ts")) {
        scan.file(&root.join(path), 0);
    }
    // `compilerOptions.types` names packages whose declarations are global.
    for name in tsconfig_types(root) {
        if let Some(entry) = resolve_types_reference(root, &name) {
            scan.file(&entry, 0);
        }
    }
    Some(scan.names)
}

fn uses_auto_imports(root: &Path) -> bool {
    std::fs::read_to_string(root.join("package.json")).is_ok_and(|manifest| {
        AUTO_IMPORT_MARKERS
            .iter()
            .any(|marker| manifest.contains(marker))
    })
}

fn typescript_lib_dir(root: &Path) -> Option<PathBuf> {
    root.ancestors()
        .map(|directory| directory.join("node_modules/typescript/lib"))
        .find(|lib| lib.join("lib.d.ts").is_file())
}

struct Scan<'a> {
    root: &'a Path,
    names: HashSet<String>,
    visited: HashSet<PathBuf>,
    files: usize,
}

impl Scan<'_> {
    fn file(&mut self, path: &Path, depth: usize) {
        if self.files >= MAX_DECLARATION_FILES || !self.visited.insert(path.to_path_buf()) {
            return;
        }
        let Ok(metadata) = std::fs::metadata(path) else {
            return;
        };
        if metadata.len() > MAX_DECLARATION_BYTES {
            return;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        self.files += 1;
        self.names.extend(ambient_global_names(path, &text));
        if depth >= MAX_REFERENCE_DEPTH {
            return;
        }
        for reference in references(&text) {
            let target = match reference {
                Reference::Path(relative) => {
                    path.parent().map(|directory| directory.join(relative))
                }
                Reference::Types(name) => resolve_types_reference(self.root, &name),
            };
            if let Some(target) = target.filter(|target| target.is_file()) {
                self.file(&target, depth + 1);
            }
        }
    }

    fn directory(&mut self, directory: &Path, depth: usize) {
        if depth > MAX_DIRECTORY_DEPTH {
            return;
        }
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        let mut entries = entries
            .flatten()
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if path.is_dir() {
                if name != "node_modules" {
                    self.directory(&path, depth + 1);
                }
            } else if name.ends_with(".d.ts") {
                self.file(&path, 0);
            }
        }
    }
}

enum Reference {
    Path(String),
    Types(String),
}

/// `/// <reference path="..." />` and `/// <reference types="..." />`.
fn references(text: &str) -> Vec<Reference> {
    text.lines()
        .map(str::trim_start)
        .filter(|line| line.starts_with("///") && line.contains("<reference"))
        .filter_map(|line| {
            if let Some(value) = attribute(line, "path") {
                Some(Reference::Path(value))
            } else {
                attribute(line, "types").map(Reference::Types)
            }
        })
        .collect()
}

fn attribute(line: &str, name: &str) -> Option<String> {
    let start = line.find(&format!("{name}="))? + name.len() + 1;
    let rest = &line[start..];
    let quote = rest.chars().next().filter(|ch| matches!(ch, '"' | '\''))?;
    let rest = &rest[1..];
    rest.find(quote).map(|end| rest[..end].to_owned())
}

/// `vite/client` is `node_modules/vite/client.d.ts`; a plain package name
/// follows its manifest's `types`, then `index.d.ts`. `@types/*` is scanned
/// as a whole elsewhere.
fn resolve_types_reference(root: &Path, name: &str) -> Option<PathBuf> {
    root.ancestors().find_map(|directory| {
        let modules = directory.join("node_modules");
        let package = modules.join(name);
        [
            modules.join(format!("{name}.d.ts")),
            package_types_entry(&package)?,
            package.join("index.d.ts"),
        ]
        .into_iter()
        .find(|candidate| candidate.is_file())
    })
}

fn tsconfig_types(root: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(root.join("tsconfig.json")) else {
        return Vec::new();
    };
    let Ok(config) = galaxy_core::jsonc::parse(&text) else {
        return Vec::new();
    };
    match config
        .get("compilerOptions")
        .and_then(|options| options.get("types"))
    {
        Some(galaxy_core::jsonc::Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

fn package_types_entry(package: &Path) -> Option<PathBuf> {
    let manifest = std::fs::read_to_string(package.join("package.json")).ok()?;
    let manifest = galaxy_core::jsonc::parse(&manifest).ok()?;
    ["types", "typings"]
        .iter()
        .find_map(|key| manifest.get(key)?.as_str())
        .map(|entry| package.join(entry))
}
