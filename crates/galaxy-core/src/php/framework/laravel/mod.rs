//! Laravel for the built-in PHP engine: the strings of a project that name
//! config keys, routes, views and the rest, and what they point to, plus
//! Blade templates and Eloquent's call forwarding. Everything is read from
//! the project's files; PHP never runs.
//!
//! Two parts are independent. The string features need the [`Index`] of an
//! application (found by its `artisan` script). Eloquent forwarding only
//! needs the framework's classes in the autoload data, so it also works for
//! a project that uses Eloquent without being an application.

pub mod blade;
pub mod config;
mod eloquent;
mod engine;
pub mod index;
pub mod site;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

pub use index::{Index, Item, Loc, Table};
pub use site::{Kind, Site, in_string, site_at, sites};

use super::Framework;
use crate::php::engine::{Action, Completion, HoverResult, PhpEngine, Problem, Target};
use crate::php::parse::{ClassInfo, Member};

/// Laravel as a [`Framework`].
#[derive(Debug)]
pub struct LaravelFramework {
    /// What the application defines; `None` without an `artisan` script.
    index: Option<Arc<Index>>,
}

impl LaravelFramework {
    /// A Laravel application's framework, or just Eloquent forwarding
    /// when `index` is `None`.
    pub fn new(index: Option<Arc<Index>>) -> Self {
        Self { index }
    }

    /// `Some` when `root` is a Laravel application (an `artisan` script) or
    /// otherwise uses Laravel's components: `composer.json` requires
    /// `laravel/framework` or an `illuminate/*` package, or they are
    /// installed under `vendor/`.
    pub fn detect(root: &Path) -> Option<Self> {
        let index = Index::detect(root).map(Arc::new);
        (index.is_some() || uses_components(root)).then_some(Self { index })
    }

    /// What the application defines, when it is one.
    pub fn index(&self) -> Option<&Arc<Index>> {
        self.index.as_ref()
    }
}

fn uses_components(root: &Path) -> bool {
    if root.join("vendor/laravel/framework").is_dir() || root.join("vendor/illuminate").is_dir() {
        return true;
    }
    std::fs::read_to_string(root.join("composer.json")).is_ok_and(|text| {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            return false;
        };
        ["require", "require-dev"].iter().any(|section| {
            value
                .get(section)
                .and_then(|v| v.as_object())
                .is_some_and(|deps| {
                    deps.keys()
                        .any(|name| name == "laravel/framework" || name.starts_with("illuminate/"))
                })
        })
    })
}

impl Framework for LaravelFramework {
    fn name(&self) -> &'static str {
        "laravel"
    }

    fn owns_string(&self, text: &str, byte: usize) -> bool {
        self.index.is_some() && in_string(text, byte, false)
    }

    fn complete(
        &self,
        engine: &mut PhpEngine,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<Vec<Completion>> {
        let idx = self.index.clone()?;
        engine.laravel_complete(idx, path, text, line, column)
    }

    fn definition(
        &self,
        engine: &mut PhpEngine,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<Vec<Target>> {
        let idx = self.index.clone()?;
        engine.laravel_definition(idx, path, text, line, column)
    }

    fn hover(
        &self,
        engine: &mut PhpEngine,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<HoverResult> {
        let idx = self.index.clone()?;
        engine.laravel_hover(idx, path, text, line, column)
    }

    fn problems(&self, engine: &mut PhpEngine, path: &Path, text: &str) -> Vec<Problem> {
        match self.index.clone() {
            Some(idx) => engine.laravel_problems(idx, path, text),
            None => Vec::new(),
        }
    }

    fn actions(
        &self,
        engine: &mut PhpEngine,
        path: &Path,
        text: &str,
        byte: usize,
        out: &mut Vec<Action>,
    ) {
        if let Some(idx) = self.index.clone() {
            engine.laravel_actions(idx, path, text, byte, out);
        }
    }

    fn forwarded_method(
        &self,
        engine: &mut PhpEngine,
        class: &Arc<ClassInfo>,
        name: &str,
    ) -> Option<(Arc<ClassInfo>, Member)> {
        eloquent::forwarded_method(engine, class, name)
    }

    fn callback_parameter_type(
        &self,
        engine: &mut PhpEngine,
        method: &str,
        receiver: &str,
        chain: &[(Arc<ClassInfo>, HashMap<String, String>)],
    ) -> Option<String> {
        eloquent::callback_parameter_type(engine, method, receiver, chain)
    }

    fn forwarding_builder(
        &self,
        engine: &mut PhpEngine,
        chain: &[(Arc<ClassInfo>, HashMap<String, String>)],
    ) -> Option<String> {
        eloquent::forwarding_builder(engine, chain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, body) in files {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        dir
    }

    #[test]
    fn artisan_makes_an_application_with_an_index() {
        let dir = project(&[("artisan", "<?php")]);
        let found = LaravelFramework::detect(dir.path()).expect("detected");
        assert!(found.index().is_some());
    }

    #[test]
    fn requiring_laravel_components_gives_eloquent_without_an_index() {
        for body in [
            r#"{"require":{"laravel/framework":"^11"}}"#,
            r#"{"require-dev":{"illuminate/database":"^11"}}"#,
        ] {
            let dir = project(&[("composer.json", body)]);
            let found = LaravelFramework::detect(dir.path()).expect("detected");
            assert!(found.index().is_none());
        }
    }

    #[test]
    fn plain_php_projects_are_not_laravel() {
        let dir = project(&[("composer.json", r#"{"require":{"monolog/monolog":"^3"}}"#)]);
        assert!(LaravelFramework::detect(dir.path()).is_none());
        assert!(super::super::detect_all(dir.path()).is_empty());
    }
}
