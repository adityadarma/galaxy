//! Goravel project integration.

use super::{GoIntegration, GoIntegrationKind};

/// Goravel is a framework: future behavior belongs here rather than in the
/// generic resolver because it depends on project conventions.
pub(super) const fn integration() -> GoIntegration {
    GoIntegration {
        name: "Goravel",
        kind: GoIntegrationKind::Framework,
        module: "github.com/goravel/framework",
    }
}
