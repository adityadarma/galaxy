//! Optional framework and library markers for Go projects.
//!
//! The generic resolver does not depend on these modules. They provide a
//! narrow home for project-aware behavior when a framework or library needs
//! it, following the PHP engine's framework separation.

mod fiber;
mod gin;
mod goravel;

pub use fiber::{
    FiberRoute, FiberRoutePath, scan_routes as scan_fiber_routes, scan_tree as scan_fiber_tree,
};
pub use gin::{GinRoute, GinRoutePath, scan_routes as scan_gin_routes, scan_tree as scan_gin_tree};
pub use goravel::{
    GoravelConfigEntry, GoravelRoute, GoravelRoutePath, scan_config as scan_goravel_config,
    scan_config_tree as scan_goravel_config_tree, scan_routes as scan_goravel_routes,
    scan_tree as scan_goravel_tree,
};

use super::modules::GoProject;
use std::path::PathBuf;

/// A framework or library route with its workspace file location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoRoute {
    Fiber { file: PathBuf, route: FiberRoute },
    Gin { file: PathBuf, route: GinRoute },
    Goravel { file: PathBuf, route: GoravelRoute },
}

/// A literal Goravel configuration entry with its workspace file location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoConfigEntry {
    pub file: PathBuf,
    pub entry: GoravelConfigEntry,
}

/// Category of a detected Go integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoIntegrationKind {
    Framework,
    Library,
}

/// A known dependency detected from the project's module requirements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoIntegration {
    pub name: &'static str,
    pub kind: GoIntegrationKind,
    pub module: &'static str,
}

/// Known integrations required by the project. Source text cannot opt a
/// project in; detection uses only static module metadata.
pub fn detect_all(project: &GoProject) -> Vec<GoIntegration> {
    let modules: Vec<String> = project
        .required_modules()
        .into_iter()
        .map(|(module, _)| module)
        .collect();
    [
        goravel::integration(),
        gin::integration(),
        fiber::v2(),
        fiber::v3(),
    ]
    .into_iter()
    .filter(|known| modules.iter().any(|module| module == known.module))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::go::modules::GoProject;

    #[test]
    fn detects_framework_and_library_from_requirements() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("go.mod"),
            "module example.com/app\n\nrequire (\n\tgithub.com/goravel/framework v1.16.3\n\tgithub.com/gin-gonic/gin v1.10.0\n\tgithub.com/gofiber/fiber/v2 v2.52.10\n)\n",
        )
        .unwrap();
        let project = GoProject::detect_with(root.path(), None, None).unwrap();
        assert_eq!(
            detect_all(&project),
            vec![goravel::integration(), gin::integration(), fiber::v2()]
        );
    }
}
