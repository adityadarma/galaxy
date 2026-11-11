//! Completion, definition and hover for PHP, answered from sources.
//!
//! Nothing here knows a framework by name. Classes come from Composer's
//! autoload data (or a workspace scan), and types from declarations and
//! docblocks, so facades (`@method`), ide-helper models (`@property`),
//! container helpers (`@template` + `class-string<T>`) and plain PHP all
//! work the same way. Whatever a framework adds is asked through the
//! [`Framework`] trait; see [`crate::php::framework`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use regex::Regex;
use tree_sitter::{Node, Tree};

use super::composer::{Autoload, ClassName};
use super::framework::{self, Framework};
use super::generics;
use super::parse::{
    ClassInfo, ClassKind, FileSymbols, FunctionInfo, Member, MemberKind, Visibility, parse_file,
    parse_with_tree, resolve_name, resolve_type,
};
use super::stubs::{Stubs, is_stub_path, materialize};
use super::version::{VersionSelection, for_project};

mod assist;
mod complete;
mod diag;
mod helpers;
mod lookup;
mod navigate;
mod refs;
#[cfg(test)]
mod tests;
mod types;

use helpers::*;
pub(in crate::php) use helpers::{
    byte_offset, fuzzy, member_operator, position_of, rank, receiver_start,
};

pub use crate::lang::{
    Action, Completion, Edit, FileChange, HoverResult, Insertion, ItemKind, Occurrence,
    OutlineSymbol, Problem, Severity, Signature, Target,
};
pub use assist::outline;
pub use refs::{RefQuery, finish, valid_name};

const MAX_ITEMS: usize = 200;
const MAX_DEPTH: u8 = 8;

#[derive(Debug)]
struct Parsed {
    mtime: Option<SystemTime>,
    file: Arc<FileSymbols>,
}

#[derive(Debug, Clone)]
pub(in crate::php) struct Ctx<'a> {
    pub(in crate::php) src: &'a str,
    pub(in crate::php) tree: &'a Tree,
    pub(in crate::php) file: &'a FileSymbols,
    pub(in crate::php) byte: usize,
    pub(in crate::php) class: Option<Arc<ClassInfo>>,
}

enum Symbol {
    Class(String),
    Member {
        class: String,
        name: String,
        call: bool,
    },
    Function(String),
    Variable(String),
}

enum Lookup {
    Found(Option<String>),
    Missing,
}

/// Autoload data and class names of a project, built off the UI thread.
#[derive(Debug)]
pub struct Built {
    autoload: Autoload,
    names: Vec<ClassName>,
    files: usize,
    scanned: HashMap<String, PathBuf>,
    frameworks: Vec<Arc<dyn Framework>>,
}

/// Reads Composer's data and walks the autoload directories (and, without
/// Composer data, the source tree). Slow on big `vendor/` trees, so callers
/// run it on a worker thread.
pub fn build(root: &Path) -> Built {
    crate::index_scheduler::shared_index_scheduler().run(|| build_inner(root))
}

fn build_inner(root: &Path) -> Built {
    let autoload = Autoload::load(root);
    let frameworks = framework::detect_all(root);
    let (mut names, mut files) = autoload.indexed_classes();
    if let VersionSelection::Supported(version) = for_project(root) {
        names.extend(Stubs::for_version(version).class_names().iter().cloned());
    }
    let mut scanned = HashMap::new();
    if autoload.is_empty() {
        for (fqn, path) in scan_sources(root, frameworks.iter().any(|fw| fw.name() == "laravel")) {
            let short = fqn.rsplit('\\').next().unwrap_or(&fqn).to_owned();
            names.push(ClassName {
                short,
                fqn: fqn.clone(),
                vendor: false,
            });
            scanned.entry(fqn.to_lowercase()).or_insert(path);
        }
        files = scanned.len();
    }
    Built {
        autoload,
        names,
        files,
        scanned,
        frameworks,
    }
}

/// The PHP project containing `file`: the nearest ancestor with a
/// `vendor/composer`, else `composer.json`, else one with `.git`, else the
/// file's own folder. A file in a dependency must use the application's
/// Composer map so definitions can cross from one vendor package to another.
pub fn project_root(file: &Path) -> PathBuf {
    let marker = |name: &str| {
        file.ancestors()
            .skip(1)
            .find(|dir| dir.join(name).exists())
            .map(Path::to_path_buf)
    };
    marker("vendor/composer")
        .or_else(|| marker("composer.json"))
        .or_else(|| marker(".git"))
        .or_else(|| file.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[derive(Debug)]
pub struct PhpEngine {
    root: PathBuf,
    autoload: Autoload,
    parsed: HashMap<PathBuf, Parsed>,
    open: HashMap<PathBuf, Arc<FileSymbols>>,
    names: Option<Arc<Vec<ClassName>>>,
    files: usize,
    scanned: Option<HashMap<String, PathBuf>>,
    deferred: bool,
    building: bool,
    built_at: Option<Instant>,
    functions: Option<HashMap<String, Arc<FunctionInfo>>>,
    /// The frameworks the project uses, asked in order.
    frameworks: Vec<Arc<dyn Framework>>,
    /// Nesting of return types worked out from method bodies.
    infer_depth: u8,
    php_version: VersionSelection,
}

impl PhpEngine {
    pub fn index_stats(&self) -> crate::LanguageIndexStats {
        let symbols = self.names.as_ref().map_or(0, |names| names.len());
        let files = self.files;
        crate::LanguageIndexStats {
            files,
            symbols,
            heap_lower_bound_bytes: files
                * (std::mem::size_of::<String>() + std::mem::size_of::<PathBuf>())
                + symbols * std::mem::size_of::<ClassName>(),
            ..Default::default()
        }
    }

    pub fn new(root: &Path) -> Self {
        Self::with_frameworks(root, framework::detect_all(root))
    }

    /// Like [`Self::new`], with `frameworks` instead of the detected ones.
    pub fn with_frameworks(root: &Path, frameworks: Vec<Arc<dyn Framework>>) -> Self {
        Self {
            root: root.to_path_buf(),
            autoload: Autoload::load(root),
            parsed: HashMap::new(),
            open: HashMap::new(),
            names: None,
            files: 0,
            scanned: None,
            deferred: false,
            building: false,
            built_at: None,
            functions: None,
            frameworks,
            infer_depth: 0,
            php_version: for_project(root),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Overrides Composer's PHP target for compatibility diagnostics and stubs.
    pub fn set_php_version(&mut self, version: Option<super::version::PhpVersion>) {
        self.php_version = version
            .map(VersionSelection::Supported)
            .unwrap_or_else(|| for_project(&self.root));
    }

    /// `true` when the project uses the framework called `name`.
    pub fn is_framework(&self, name: &str) -> bool {
        self.frameworks.iter().any(|fw| fw.name() == name)
    }

    /// Whether an active framework owns `path` as a non-PHP template.
    pub(super) fn is_template(&self, path: &Path) -> bool {
        self.frameworks
            .iter()
            .any(|framework| framework.owns_template(path))
    }

    /// An engine that reads nothing yet: Composer data and class names
    /// arrive through [`Self::install`] from a worker thread. Until then
    /// it answers only from open buffers.
    pub fn pending(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            autoload: Autoload::default(),
            parsed: HashMap::new(),
            open: HashMap::new(),
            names: None,
            files: 0,
            scanned: None,
            deferred: true,
            building: true,
            built_at: None,
            functions: None,
            frameworks: Vec::new(),
            infer_depth: 0,
            php_version: for_project(root),
        }
    }

    /// The index was installed at least once.
    pub fn is_ready(&self) -> bool {
        self.built_at.is_some()
    }

    /// Until [`Self::install`] runs, class names are not offered and
    /// nothing walks the file system on the calling thread.
    pub fn defer_index(&mut self) {
        self.deferred = true;
    }

    /// `true` when the caller should run [`build`] and [`Self::install`]:
    /// no index yet, or one older than `max_age`. Asks once until installed.
    pub fn wants_index(&mut self, max_age: Duration) -> bool {
        if self.building {
            return false;
        }
        let stale = self.built_at.is_none_or(|at| at.elapsed() > max_age);
        self.building = stale;
        stale
    }

    /// Marks the current snapshot for replacement by a worker. The old index
    /// stays available for navigation until [`Self::install`] atomically puts
    /// the rebuilt Composer/vendor snapshot in place.
    pub fn begin_rebuild(&mut self) -> bool {
        if self.building {
            return false;
        }
        self.building = true;
        true
    }

    pub fn install(&mut self, built: Built) {
        self.autoload = built.autoload;
        self.files = built.files;
        self.scanned = Some(built.scanned);
        self.names = Some(Arc::new(built.names));
        self.functions = None;
        self.frameworks = built.frameworks;
        self.building = false;
        self.built_at = Some(Instant::now());
    }

    /// Registers the text of an open buffer; it wins over the file on disk.
    pub fn open_document(&mut self, path: &Path, text: &str) {
        let file = Arc::new(parse_file(path, text));
        self.open.insert(path.to_path_buf(), file);
    }

    pub fn close_document(&mut self, path: &Path) {
        self.open.remove(path);
    }

    /// Forgets what was read from disk, e.g. after files were added or
    /// `composer dump-autoload` ran.
    pub fn reset_index(&mut self) {
        self.autoload = Autoload::load(&self.root);
        self.parsed.clear();
        self.names = None;
        self.files = 0;
        self.scanned = None;
        self.functions = None;
    }
}
