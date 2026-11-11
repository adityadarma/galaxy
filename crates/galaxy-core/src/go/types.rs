//! Static types of Go expressions.
//!
//! No type checker runs. [`eval_text`] walks the Tree-sitter nodes of an
//! expression and resolves names through the scope at the cursor
//! ([`super::scope`]), the package, its imports and the universe, following
//! fields, methods and promoted members through [`GoEngine::members_of`].
//! [`receiver_ty_at`] finds the expression before a `.` in the buffer, even
//! when the code around it does not parse, and evaluates it. Anything that
//! cannot be resolved is [`Ty::Unknown`]; nothing here panics or loops.

use std::path::Path;
use std::sync::Arc;

use tree_sitter::Tree;

use super::engine::{GoEngine, Member, Package};
use super::parse::GoFile;

pub use hints::parameter_hints;
pub use lookup::{Callee, Resolved, Symbol, callee_at, resolve_at, resolve_ids};

mod builtins;
mod eval;
mod generic;
mod hints;
mod lookup;
mod range;
mod receiver;
mod resolve;
mod site;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ty {
    /// A declared or predeclared type. `pkg` is the import path of the
    /// declaring package, empty for the universe (`int`, `error`, `any`).
    Named {
        pkg: String,
        name: String,
        args: Vec<Ty>,
    },
    Pointer(Box<Ty>),
    Slice(Box<Ty>),
    Array(Box<Ty>),
    Map(Box<Ty>, Box<Ty>),
    Chan(Box<Ty>),
    /// For a variadic function the last entry of `params` is the element type.
    Func {
        params: Vec<Ty>,
        results: Vec<Ty>,
        variadic: bool,
    },
    Struct,
    Interface,
    /// The results of a call with several.
    Tuple(Vec<Ty>),
    /// An imported package, by import path.
    Package(String),
    Unknown,
}

impl Ty {
    /// Go-like text with packages shown by the last element of the import
    /// path: `*http.Request`, `[]string`, `map[string]any`.
    pub fn display(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    /// The type behind any number of pointers.
    #[allow(clippy::should_implement_trait)]
    pub fn deref(&self) -> &Ty {
        let mut current = self;
        while let Ty::Pointer(inner) = current {
            current = inner;
        }
        current
    }

    /// Import path and name of a named type, through pointers.
    pub fn named(&self) -> Option<(&str, &str)> {
        match self.deref() {
            Ty::Named { pkg, name, .. } => Some((pkg, name)),
            _ => None,
        }
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self, Ty::Unknown)
    }

    fn write(&self, out: &mut String) {
        match self {
            Ty::Named { pkg, name, args } => {
                if !pkg.is_empty() {
                    out.push_str(&short_pkg(pkg));
                    out.push('.');
                }
                out.push_str(name);
                if !args.is_empty() {
                    out.push('[');
                    write_list(args, out);
                    out.push(']');
                }
            }
            Ty::Pointer(t) => {
                out.push('*');
                t.write(out);
            }
            Ty::Slice(t) => {
                out.push_str("[]");
                t.write(out);
            }
            Ty::Array(t) => {
                out.push_str("[...]");
                t.write(out);
            }
            Ty::Map(k, v) => {
                out.push_str("map[");
                k.write(out);
                out.push(']');
                v.write(out);
            }
            Ty::Chan(t) => {
                out.push_str("chan ");
                t.write(out);
            }
            Ty::Func {
                params,
                results,
                variadic,
            } => {
                out.push_str("func(");
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    if *variadic && i + 1 == params.len() {
                        out.push_str("...");
                    }
                    p.write(out);
                }
                out.push(')');
                match results.as_slice() {
                    [] => {}
                    [one] => {
                        out.push(' ');
                        one.write(out);
                    }
                    many => {
                        out.push_str(" (");
                        write_list(many, out);
                        out.push(')');
                    }
                }
            }
            Ty::Struct => out.push_str("struct{}"),
            Ty::Interface => out.push_str("interface{}"),
            Ty::Tuple(items) => {
                out.push('(');
                write_list(items, out);
                out.push(')');
            }
            Ty::Package(path) => {
                out.push_str("package ");
                out.push_str(&short_pkg(path));
            }
            Ty::Unknown => out.push('?'),
        }
    }
}

fn write_list(items: &[Ty], out: &mut String) {
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        item.write(out);
    }
}

fn is_major(text: &str) -> bool {
    text.strip_prefix('v')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The package name an import path suggests: its last element, or the one
/// before a `/vN` suffix, without a `.vN` suffix.
fn short_pkg(path: &str) -> String {
    let mut parts = path.rsplit('/');
    let last = parts.next().unwrap_or("");
    let element = match parts.next() {
        Some(before) if is_major(last) && !before.is_empty() => before,
        _ => last,
    };
    match element.rsplit_once('.') {
        Some((head, tail)) if !head.is_empty() && is_major(tail) => head.to_owned(),
        _ => element.to_owned(),
    }
}

/// The type of the expression `expr`, with the names in scope at `byte` of
/// `src` (the buffer of `path`, parsed as `tree`).
pub fn eval_text(
    engine: &mut GoEngine,
    path: &Path,
    src: &str,
    tree: &Tree,
    byte: usize,
    expr: &str,
) -> Ty {
    let Some(mut ev) = site::Eval::new(engine, path, src, tree) else {
        return Ty::Unknown;
    };
    let site = ev.site_at(byte.min(src.len()));
    ev.eval_text_in(&site, expr)
}

/// The type of the expression before the `.` at or just before `byte`:
/// `req.Hea|`, `req.|`, `a.b().c.|`, `pkg.|` (a [`Ty::Package`]), `(*p).|`,
/// `x[i].|`. [`Ty::Unknown`] when there is no such dot.
pub fn receiver_ty_at(
    engine: &mut GoEngine,
    path: &Path,
    src: &str,
    tree: &Tree,
    byte: usize,
) -> Ty {
    let byte = byte.min(src.len());
    let Some(range) = receiver::receiver_range(src, byte) else {
        return Ty::Unknown;
    };
    let Some(text) = src.get(range) else {
        return Ty::Unknown;
    };
    eval_text(engine, path, src, tree, byte, text)
}

/// What completion needs about the expression before a `.`.
pub struct ReceiverInfo {
    pub ty: Ty,
    /// Fields and methods of `ty`, promoted ones included; empty for
    /// anything but named types.
    pub members: Vec<Member>,
    /// The package of the buffer, with the buffer's text applied.
    pub pkg: Arc<Package>,
    pub file: Arc<GoFile>,
}

/// Evaluates `expr` at `byte` and lists its members.
pub fn expr_info(
    engine: &mut GoEngine,
    path: &Path,
    src: &str,
    tree: &Tree,
    byte: usize,
    expr: &str,
) -> Option<ReceiverInfo> {
    let mut ev = site::Eval::new(engine, path, src, tree)?;
    let site = ev.site_at(byte.min(src.len()));
    let ty = ev.eval_text_in(&site, expr);
    let members = ev.members(&ty);
    Some(ReceiverInfo {
        ty,
        members,
        pkg: ev.pkg.clone(),
        file: ev.file.clone(),
    })
}

/// Like [`receiver_ty_at`], with the members of the type.
pub fn receiver_info(
    engine: &mut GoEngine,
    path: &Path,
    src: &str,
    tree: &Tree,
    byte: usize,
) -> Option<ReceiverInfo> {
    let byte = byte.min(src.len());
    let range = receiver::receiver_range(src, byte)?;
    let text = src.get(range)?;
    expr_info(engine, path, src, tree, byte, text)
}

/// The package and file of the buffer, with the buffer's text applied.
pub fn buffer_package(
    engine: &mut GoEngine,
    path: &Path,
    src: &str,
    tree: &Tree,
) -> Option<(Arc<Package>, Arc<GoFile>)> {
    let ev = site::Eval::new(engine, path, src, tree)?;
    Some((ev.pkg.clone(), ev.file.clone()))
}
