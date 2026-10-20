//! CodeIgniter 4 for the built-in PHP engine: the strings of a project that
//! name views, routes, config classes, models, services, language keys,
//! helpers and env keys, and what they point to. Everything is read from
//! the project's files; PHP never runs.
//!
//! The [`Index`] of an application (found by its `spark` script) and the
//! [`site`] scan of PHP text feed completion, definitions, hover,
//! diagnostics and quick fixes. Models forward calls they do not declare to
//! `CodeIgniter\Database\BaseBuilder`.

mod engine;
pub mod index;
mod model;
pub mod site;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

pub use index::Index;
pub use site::{Kind, Site, in_string, is_routes_file, site_at, site_at_in, sites, sites_in};

use super::Framework;
use crate::php::engine::{Action, Completion, HoverResult, PhpEngine, Problem, Target};
use crate::php::parse::{ClassInfo, Member};

/// CodeIgniter 4 as a [`Framework`].
#[derive(Debug)]
pub struct CodeIgniterFramework {
    /// What the application defines.
    index: Arc<Index>,
}

impl CodeIgniterFramework {
    pub fn new(index: Arc<Index>) -> Self {
        Self { index }
    }

    /// `Some` when `root` is a CodeIgniter 4 application: a `spark` script
    /// and either `app/Config/App.php` or a Composer requirement on
    /// `codeigniter4/framework` or `codeigniter4/appstarter`.
    pub fn detect(root: &Path) -> Option<Self> {
        Index::detect(root).map(|index| Self::new(Arc::new(index)))
    }

    /// What the application defines.
    pub fn index(&self) -> &Arc<Index> {
        &self.index
    }
}

impl Framework for CodeIgniterFramework {
    fn name(&self) -> &'static str {
        "codeigniter"
    }

    fn owns_string(&self, text: &str, byte: usize) -> bool {
        site_at(text, byte).is_some()
    }

    fn complete(
        &self,
        engine: &mut PhpEngine,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<Vec<Completion>> {
        engine.codeigniter_complete(self.index.clone(), path, text, line, column)
    }

    fn definition(
        &self,
        engine: &mut PhpEngine,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<Vec<Target>> {
        engine.codeigniter_definition(self.index.clone(), path, text, line, column)
    }

    fn hover(
        &self,
        engine: &mut PhpEngine,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<HoverResult> {
        engine.codeigniter_hover(self.index.clone(), path, text, line, column)
    }

    fn problems(&self, engine: &mut PhpEngine, path: &Path, text: &str) -> Vec<Problem> {
        engine.codeigniter_problems(self.index.clone(), path, text)
    }

    fn actions(
        &self,
        engine: &mut PhpEngine,
        path: &Path,
        text: &str,
        byte: usize,
        out: &mut Vec<Action>,
    ) {
        engine.codeigniter_actions(self.index.clone(), path, text, byte, out);
    }

    fn forwarded_method(
        &self,
        engine: &mut PhpEngine,
        class: &Arc<ClassInfo>,
        name: &str,
    ) -> Option<(Arc<ClassInfo>, Member)> {
        model::forwarded_method(engine, class, name)
    }

    fn forwarding_builder(
        &self,
        engine: &mut PhpEngine,
        chain: &[(Arc<ClassInfo>, HashMap<String, String>)],
    ) -> Option<String> {
        model::forwarding_builder(engine, chain)
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
    fn spark_project_is_detected_with_an_index() {
        let dir = project(&[
            ("spark", "<?php"),
            ("app/Config/App.php", "<?php"),
            ("app/Views/home.php", "<?php"),
        ]);
        let found = CodeIgniterFramework::detect(dir.path()).expect("detected");
        assert_eq!(found.name(), "codeigniter");
        assert_eq!(found.index().views.len(), 1);
    }

    #[test]
    fn other_projects_are_not_detected() {
        let laravel = project(&[("artisan", "<?php")]);
        assert!(CodeIgniterFramework::detect(laravel.path()).is_none());
        let empty = project(&[("composer.json", "{}")]);
        assert!(CodeIgniterFramework::detect(empty.path()).is_none());
    }

    #[test]
    fn owns_strings_that_name_framework_things() {
        let framework = CodeIgniterFramework::new(Arc::new(Index::default()));
        let text = "<?php view('home'); $x = 'plain';";
        assert!(framework.owns_string(text, text.find("home").unwrap()));
        assert!(!framework.owns_string(text, text.find("plain").unwrap()));
        assert!(!framework.owns_string(text, 2));
    }
}
