//! Galaxy editor core.
//!
//! This crate is UI-agnostic. The Swift/AppKit frontend talks to it through
//! `galaxy-ffi`. All heavy work (parsing, indexing, LSP) will live here so the
//! main thread only renders viewport snapshots.

pub mod change;
pub mod clipboard;
pub mod commands;
pub mod composition;
pub mod disk;
pub mod document;
pub mod edit;
pub mod file_finder;
pub mod find;
pub mod git;
pub mod grapheme;
pub mod history;
pub mod jsonc;
pub mod keymap;
pub mod logging;
pub mod lsp;
pub mod movement;
pub mod multicursor;
pub mod navigation;
pub mod position;
pub mod selection;
pub mod syntax;
pub mod terminal;
pub mod viewport;
pub mod watcher;
pub mod workspace;

/// Semantic version of the core, exposed to the UI for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Returns the core version string.
pub fn version() -> &'static str {
    VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_manifest() {
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
    }
}
