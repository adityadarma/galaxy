//! Fiber library integration.

use super::{GoIntegration, GoIntegrationKind};

/// Fiber is a library: normal Go package resolution remains the primary API
/// source, while library-specific behavior stays isolated here.
pub(super) const fn v2() -> GoIntegration {
    GoIntegration {
        name: "Fiber",
        kind: GoIntegrationKind::Library,
        module: "github.com/gofiber/fiber/v2",
    }
}

pub(super) const fn v3() -> GoIntegration {
    GoIntegration {
        name: "Fiber",
        kind: GoIntegrationKind::Library,
        module: "github.com/gofiber/fiber/v3",
    }
}
