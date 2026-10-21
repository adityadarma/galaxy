//! Built-in Go language engine.
//!
//! - [`modules`] and [`build`]: `go.mod`/`go.work`/vendor/module cache
//!   resolution of import paths to directories, and build constraints
//!   (`//go:build`, file name suffixes), all read statically; `go` never runs.
//! - [`engine`] indexes packages and finds members, including promoted ones;
//!   [`scope`] lists the local names visible at a cursor position.
//! - [`parse`] reads the declarations of one Go file with Tree-sitter
//!   (package, imports, types, functions, constants and variables, with docs
//!   and UTF-16 positions). [`texpr`] turns the type text kept in those
//!   declarations into a structured [`TypeExpr`].

pub mod actions;
pub mod api;
pub mod build;
pub mod complete;
pub mod diag;
pub mod engine;
pub mod integration;
pub mod modules;
pub mod outline;
pub mod parse;
pub mod refs;
pub mod scope;
pub mod texpr;
pub mod types;

pub use actions::actions;
pub use api::project_root;
pub use build::{BuildContext, constraint_of, eval_constraint, file_matches, go_files};
pub use complete::complete;
pub use diag::diagnostics;
pub use engine::{Built, FileSymbol, GoEngine, Member, MemberKind, Members, Package};
pub use integration::{
    FiberRoute, FiberRoutePath, GinRoute, GinRoutePath, GoConfigEntry, GoIntegration,
    GoIntegrationKind, GoRoute, GoravelConfigEntry, GoravelRoute, GoravelRoutePath,
    scan_fiber_routes, scan_fiber_tree, scan_gin_routes, scan_gin_tree, scan_goravel_config,
    scan_goravel_config_tree, scan_goravel_routes, scan_goravel_tree,
};
pub use modules::{GoProject, Module, discover_goroot, discover_modcache, escape_module_path};
pub use outline::outline;
pub use parse::{
    Field, FuncDecl, GoFile, IdentAt, IfaceMethod, Import, Param, Pos, Receiver, TypeDecl,
    TypeKind, TypeParam, ValueDecl, exported, identifier_at, parse, parse_file, utf16_col,
};
pub use refs::{
    GoRefQuery, can_rename, reference_query, references, rename_edits, scan_references, valid_name,
};
pub use scope::{Binding, BindingKind, FuncContext, Origin, Scope, enclosing_func, scope_at};
pub use texpr::{ChanDir, TypeExpr};
pub use types::{Callee, Resolved, Symbol, callee_at, resolve_at};
pub use types::{Ty, eval_text, receiver_ty_at};
