//! Optional framework and library markers for Go projects.
//!
//! The generic resolver does not depend on these modules. They provide a
//! narrow home for project-aware behavior when a framework or library needs
//! it, following the PHP engine's framework separation.

mod fiber;
mod goravel;

use super::modules::GoProject;

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
    [goravel::integration(), fiber::v2(), fiber::v3()]
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
            "module example.com/app\n\nrequire (\n\tgithub.com/goravel/framework v1.16.3\n\tgithub.com/gofiber/fiber/v2 v2.52.10\n)\n",
        )
        .unwrap();
        let project = GoProject::detect_with(root.path(), None, None).unwrap();
        assert_eq!(
            detect_all(&project),
            vec![goravel::integration(), fiber::v2()]
        );
    }
}
