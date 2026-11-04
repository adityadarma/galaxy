//! Built-in PHP language engine.
//!
//! Answers completion, definition and hover in-process from the project's
//! own sources, with no language server. The engine itself knows no
//! framework; those plug in through [`framework`]. Class files come from
//! Composer's autoload data (or a workspace scan when a project has none),
//! types from declarations and docblocks (`@var`, `@return`, `@method`,
//! `@property`, `@mixin`).

pub mod composer;
pub mod engine;
pub mod framework;
mod generics;
pub mod parse;
mod stub_files;
mod stubs;
mod version;

pub use engine::{
    Action, Built, Completion, Edit, HoverResult, Insertion, ItemKind, Occurrence, OutlineSymbol,
    PhpEngine, Problem, RefQuery, Severity, Signature, Target, build, finish, outline,
    project_root, valid_name,
};

pub(crate) use stubs::builtin_function_names;
