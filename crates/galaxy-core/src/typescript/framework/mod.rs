//! Framework adapters for the TypeScript and JavaScript index.
//!
//! The index knows the language only: syntax, scopes, imports, explicit
//! types. Anything a framework adds lives here, one module per framework.
//!
//! Rules for an adapter:
//!
//! 1. Activate from an import of a known package, never from a project path
//!    or a variable name.
//! 2. Be syntactic. Emit [`TypeMemberTarget::PackageDeclaration`] to name the
//!    declaration that owns a member; the host resolves it on disk.
//! 3. Fail closed. When a shape is not recognised, emit nothing.
//! 4. Test with temp-dir fixtures that mimic the package layout, not with a
//!    real project.
//!
//! To add a framework: create `<name>.rs` with a `collect` function and call
//! it from [`collect_uses`].

use std::path::Path;

use tree_sitter::Node;

use super::{Import, Locations, SymbolOccurrences, TypeMemberUse};

mod adonisjs;

/// Syntax facts an adapter may read. Adapters never see the file system.
pub(super) struct Context<'a> {
    pub text: &'a str,
    pub locations: &'a Locations<'a>,
    pub imports: &'a [Import],
    pub import_symbols: &'a [SymbolOccurrences],
}

/// Member uses that frameworks make resolvable.
pub(super) fn collect_uses(root: Node<'_>, context: &Context<'_>) -> Vec<TypeMemberUse> {
    let mut uses = Vec::new();
    uses.extend(adonisjs::collect(root, context));
    uses
}

/// Local classes that inherit AdonisJS's `@poppinss/macroable` hook.
pub(super) fn macroable_classes(path: &Path, root: Node<'_>, context: &Context<'_>) -> Vec<String> {
    adonisjs::macroable_classes(path, root, context)
}
