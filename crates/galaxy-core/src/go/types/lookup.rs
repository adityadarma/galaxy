//! Symbol lookup for hover, go-to-definition and signature help.
//!
//! [`resolve_at`] says what the identifier at a position stands for and
//! where it is declared; [`callee_at`] describes the function whose call the
//! cursor is inside, with the argument that is being typed. Both reuse the
//! evaluator of [`super::eval_text`] and read declarations from the parsed
//! packages. Whatever cannot be resolved gives `None`; nothing here panics.

use std::path::{Path, PathBuf};

use tree_sitter::Tree;

use super::Ty;
use super::site::Eval;
use crate::go::engine::GoEngine;
use crate::go::parse::{IdentAt, Pos, identifier_at, utf16_col};

mod callee;
mod ident;
mod sig;

#[cfg(test)]
#[path = "lookup_tests.rs"]
mod lookup_tests;

/// What an identifier stands for. Lines and columns are 0-based, columns in
/// UTF-16 units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Symbol {
    /// A name declared inside a function. `kind` is one of `var`, `param`,
    /// `receiver`, `result`, `const`, `range var`, `type switch var`,
    /// `label`.
    Local {
        name: String,
        path: PathBuf,
        line: u32,
        column: u32,
        end_column: u32,
        kind: String,
    },
    Package {
        import_path: String,
    },
    Type {
        pkg: String,
        name: String,
        path: PathBuf,
        pos: Pos,
    },
    Func {
        pkg: String,
        name: String,
        path: PathBuf,
        pos: Pos,
    },
    Value {
        pkg: String,
        name: String,
        path: PathBuf,
        pos: Pos,
        is_const: bool,
    },
    Field {
        owner: String,
        name: String,
        path: PathBuf,
        pos: Pos,
    },
    Method {
        owner: String,
        name: String,
        path: PathBuf,
        pos: Pos,
    },
    TypeParam {
        name: String,
    },
    Builtin {
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub symbol: Symbol,
    pub ty: Ty,
    /// One-line declaration for hover.
    pub signature: String,
    pub doc: String,
    /// Range of the identifier under the cursor.
    pub start_line: u32,
    pub start_column: u32,
    pub end_column: u32,
}

/// The function a call under the cursor invokes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Callee {
    pub name: String,
    /// `func (c *Client) Do(req *Request) (*Response, error)`
    pub label: String,
    /// `req *Request`, or `args ...any` when variadic.
    pub params: Vec<String>,
    pub results: Vec<String>,
    pub doc: String,
    /// Index of the argument being typed.
    pub active: u32,
}

/// What the identifier at or just before `byte` of `src` (the buffer of
/// `path`, parsed as `tree`) stands for.
pub fn resolve_at(
    engine: &mut GoEngine,
    path: &Path,
    src: &str,
    tree: &Tree,
    byte: usize,
) -> Option<Resolved> {
    let byte = floor_boundary(src, byte);
    let (line, column) = line_column(src, byte);
    let id = identifier_at(tree, src, line, column)?;
    let mut ev = Eval::new(engine, path, src, tree)?;
    ident::resolve(&mut ev, src, &id)
}

/// [`resolve_at`] for many identifiers of one buffer, sharing the work of
/// reading the package. The result has one entry per identifier.
pub fn resolve_ids(
    engine: &mut GoEngine,
    path: &Path,
    src: &str,
    tree: &Tree,
    ids: &[IdentAt],
) -> Vec<Option<Resolved>> {
    let Some(mut ev) = Eval::new(engine, path, src, tree) else {
        return vec![None; ids.len()];
    };
    ids.iter()
        .map(|id| {
            ev.nodes = 0;
            ev.steps = 0;
            ev.depth = 0;
            ident::resolve(&mut ev, src, id)
        })
        .collect()
}

/// The function called by the innermost call whose parentheses hold `byte`.
/// Code that does not parse, such as `client.Do(req, ` at the end of the
/// buffer, is handled by reading the text before the cursor.
pub fn callee_at(
    engine: &mut GoEngine,
    path: &Path,
    src: &str,
    tree: &Tree,
    byte: usize,
) -> Option<Callee> {
    let byte = floor_boundary(src, byte);
    let mut ev = Eval::new(engine, path, src, tree)?;
    callee::callee(&mut ev, src, byte)
}

fn floor_boundary(src: &str, byte: usize) -> usize {
    let mut byte = byte.min(src.len());
    while !src.is_char_boundary(byte) {
        byte -= 1;
    }
    byte
}

/// 0-based line and UTF-16 column of `byte`.
fn line_column(src: &str, byte: usize) -> (u32, u32) {
    let head = &src[..byte];
    let line = head.matches('\n').count() as u32;
    let start = head.rfind('\n').map_or(0, |i| i + 1);
    (line, utf16_col(&src[start..], byte - start))
}
