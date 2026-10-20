//! Framework extensions for the built-in PHP engine.
//!
//! The engine (`php::engine`) knows PHP only: Composer autoloading, class
//! lineage, docblock types. Everything a framework adds on top lives here
//! behind the [`Framework`] trait: strings that name framework things
//! (config keys, routes, views), templates the framework compiles to PHP,
//! and calls the framework forwards at run time (Eloquent models to the
//! query builder).
//!
//! To add a framework:
//!
//! 1. Create `framework/<name>/` with a type that implements [`Framework`].
//!    Every method has a neutral default, so implement only what applies.
//! 2. Return it from [`detect_all`] when the project has the framework's
//!    marker files. Detection runs on the index worker thread.
//! 3. If it owns a template syntax, make [`is_template_name`] recognize the
//!    file name so the PHP analysis leaves those files alone.
//!
//! The engine asks each registered framework in order. For answers, the
//! first framework that has one wins; problems and actions are collected
//! from all of them.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::php::engine::{Action, Completion, HoverResult, PhpEngine, Problem, Target};
use crate::php::parse::{ClassInfo, Member};

pub mod codeigniter;
pub mod laravel;
pub mod symfony;

/// What a framework adds to the PHP engine.
///
/// Methods get the engine so they can resolve classes and types with the
/// same machinery plain PHP uses. The engine clones its framework list
/// before calling, so a method may borrow the engine mutably.
pub trait Framework: Send + Sync + std::fmt::Debug {
    /// Stable lowercase name, e.g. `"laravel"`.
    fn name(&self) -> &'static str;

    /// `true` when `byte` is inside a string literal the framework handles,
    /// so plain PHP completion stays quiet there.
    fn owns_string(&self, _text: &str, _byte: usize) -> bool {
        false
    }

    /// Suggestions at `(line, column)`; `None` to leave it to the next
    /// framework and then plain PHP.
    fn complete(
        &self,
        _engine: &mut PhpEngine,
        _path: &Path,
        _text: &str,
        _line: u32,
        _column: u32,
    ) -> Option<Vec<Completion>> {
        None
    }

    /// Where the framework thing under the cursor is defined.
    fn definition(
        &self,
        _engine: &mut PhpEngine,
        _path: &Path,
        _text: &str,
        _line: u32,
        _column: u32,
    ) -> Option<Vec<Target>> {
        None
    }

    /// Documentation for the framework thing under the cursor.
    fn hover(
        &self,
        _engine: &mut PhpEngine,
        _path: &Path,
        _text: &str,
        _line: u32,
        _column: u32,
    ) -> Option<HoverResult> {
        None
    }

    /// Names in `text` that the project does not define.
    fn problems(&self, _engine: &mut PhpEngine, _path: &Path, _text: &str) -> Vec<Problem> {
        Vec::new()
    }

    /// Quick fixes at `byte`, appended to `out`.
    fn actions(
        &self,
        _engine: &mut PhpEngine,
        _path: &Path,
        _text: &str,
        _byte: usize,
        _out: &mut Vec<Action>,
    ) {
    }

    /// A call `class` does not declare that the framework forwards
    /// elsewhere (an Eloquent model to its query builder).
    fn forwarded_method(
        &self,
        _engine: &mut PhpEngine,
        _class: &Arc<ClassInfo>,
        _name: &str,
    ) -> Option<(Arc<ClassInfo>, Member)> {
        None
    }

    /// The type that calls `chain` does not declare are forwarded to, when
    /// `chain` (a class with its ancestors and template bindings) is one
    /// the framework forwards.
    fn forwarding_builder(
        &self,
        _engine: &mut PhpEngine,
        _chain: &[(Arc<ClassInfo>, HashMap<String, String>)],
    ) -> Option<String> {
        None
    }
}

/// Every framework whose marker files exist under `root`. Called on the
/// index worker thread, so it may read the file system.
pub fn detect_all(root: &Path) -> Vec<Arc<dyn Framework>> {
    let mut found: Vec<Arc<dyn Framework>> = Vec::new();
    if let Some(framework) = laravel::LaravelFramework::detect(root) {
        found.push(Arc::new(framework));
    }
    if let Some(framework) = codeigniter::CodeIgniterFramework::detect(root) {
        found.push(Arc::new(framework));
    }
    if let Some(framework) = symfony::SymfonyFramework::detect(root) {
        found.push(Arc::new(framework));
    }
    found
}

/// `true` for a file name of a template syntax some framework owns
/// (`*.blade.php`). Those are not PHP: the analysis skips them and only
/// the framework answers.
pub fn is_template_name(name: &str) -> bool {
    laravel::blade::is_blade_name(name)
}

/// [`is_template_name`] for the file at `path`.
pub fn is_template(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_template_name)
}
