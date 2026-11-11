//! Inline hints for Go: parameter names before call arguments, and the type
//! of variables declared with `:=` or `var x = ...`.
//!
//! Parameters come from [`callee_at`], types from [`eval_text`], so a hint
//! appears only where signature help and hover can resolve the name too.

use tree_sitter::{Node, Tree};

use super::{Ty, buffer_package, callee_at, eval_text};
use crate::go::engine::GoEngine;
use crate::go::parse::utf16_col;
use crate::parameter_hints::{self, Argument, Parameter, ParameterHint};
use std::path::Path;

/// Longest type written in a label; longer ones would crowd the line.
const MAX_LABEL_CHARS: usize = 48;

/// Predeclared functions: their parameter names (`v`, `t`) tell nothing.
const BUILTINS: &[&str] = &[
    "append", "cap", "clear", "close", "complex", "copy", "delete", "imag", "len", "make", "max",
    "min", "new", "panic", "print", "println", "real", "recover",
];

/// Hints for the calls and declarations that touch `first..=last`.
pub fn parameter_hints(
    engine: &mut GoEngine,
    path: &Path,
    src: &str,
    tree: &Tree,
    first: u32,
    last: u32,
) -> Vec<ParameterHint> {
    let (first, last) = (first as usize, last as usize);
    let lines: Vec<&str> = src.split('\n').collect();
    let column = |row: usize, byte_col: usize| -> u32 {
        utf16_col(lines.get(row).copied().unwrap_or(""), byte_col)
    };
    let mut calls = Vec::new();
    let mut declarations = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.end_position().row < first || node.start_position().row > last {
            continue;
        }
        match node.kind() {
            "call_expression" if calls.len() < parameter_hints::MAX_CALLS => calls.push(node),
            "short_var_declaration" | "var_spec"
                if declarations.len() < parameter_hints::MAX_CALLS =>
            {
                declarations.push(node)
            }
            _ => {}
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }

    let mut out = Vec::new();
    for call in calls {
        let Some(arguments) = call_arguments(src, call, &column) else {
            continue;
        };
        let Some(list) = call.child_by_field_name("arguments") else {
            continue;
        };
        let Some(callee) = callee_at(engine, path, src, tree, list.start_byte() + 1) else {
            continue;
        };
        let parameters: Vec<Parameter> = callee.params.iter().map(|p| parameter(p)).collect();
        out.extend(parameter_hints::hints(&parameters, &arguments));
    }

    if !declarations.is_empty()
        && let Some((package, _)) = buffer_package(engine, path, src, tree)
    {
        for declaration in declarations {
            declared_types(
                engine,
                path,
                src,
                tree,
                &package.import_path,
                declaration,
                &column,
                &mut out,
            );
        }
    }
    out.sort_by_key(|hint| (hint.line, hint.column));
    out
}

/// The arguments of `call`, or `None` for a builtin or a conversion.
fn call_arguments(
    src: &str,
    call: Node<'_>,
    column: &dyn Fn(usize, usize) -> u32,
) -> Option<Vec<Argument>> {
    let function = call.child_by_field_name("function")?;
    let name = function.utf8_text(src.as_bytes()).ok()?;
    if function.kind() == "identifier" && BUILTINS.contains(&name) {
        return None;
    }
    let list = call.child_by_field_name("arguments")?;
    let mut cursor = list.walk();
    let arguments: Vec<_> = list
        .named_children(&mut cursor)
        .filter(|argument| !matches!(argument.kind(), "comment"))
        .map(|argument| {
            let spread = argument.kind() == "variadic_argument";
            let named = match argument.kind() {
                "identifier" => Some(argument),
                "selector_expression" => argument.child_by_field_name("field"),
                _ => None,
            };
            let start = argument.start_position();
            Argument {
                line: start.row as u32,
                column: column(start.row, start.column),
                name: named
                    .and_then(|node| node.utf8_text(src.as_bytes()).ok())
                    .map(str::to_owned),
                spread,
                named: false,
            }
        })
        .collect();
    (!arguments.is_empty()).then_some(arguments)
}

/// `req *Request` -> `req`; `...any` and `*Request` have no name.
fn parameter(text: &str) -> Parameter {
    let text = text.trim();
    let (name, rest) = match text.split_once(' ') {
        Some((head, tail)) if is_name(head) && !tail.trim().is_empty() => (head, tail.trim()),
        _ => ("", text),
    };
    Parameter {
        name: if name == "_" {
            String::new()
        } else {
            name.to_owned()
        },
        rest: rest.starts_with("..."),
    }
}

fn is_name(word: &str) -> bool {
    !matches!(word, "chan" | "func" | "map" | "interface" | "struct")
        && word.starts_with(|c: char| c.is_alphabetic() || c == '_')
        && word.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// `: type` after each new variable of `x := value` or `var x = value`.
#[allow(clippy::too_many_arguments)]
fn declared_types(
    engine: &mut GoEngine,
    path: &Path,
    src: &str,
    tree: &Tree,
    package: &str,
    declaration: Node<'_>,
    column: &dyn Fn(usize, usize) -> u32,
    out: &mut Vec<ParameterHint>,
) {
    let (names, values) = if declaration.kind() == "short_var_declaration" {
        (
            declaration.child_by_field_name("left"),
            declaration.child_by_field_name("right"),
        )
    } else {
        if declaration.child_by_field_name("type").is_some() {
            return;
        }
        (
            declaration.child_by_field_name("name"),
            declaration.child_by_field_name("value"),
        )
    };
    let (Some(names), Some(values)) = (names, values) else {
        return;
    };
    let identifiers = list_items(names);
    let expressions = list_items(values);
    if identifiers.iter().any(|node| node.kind() != "identifier") {
        return;
    }
    // Evaluated where the statement starts: the new names are not visible
    // in their own initializer.
    let byte = declaration.start_byte();
    let types: Vec<Ty> = if expressions.len() == identifiers.len() {
        expressions
            .iter()
            .map(|expression| {
                let text = expression.utf8_text(src.as_bytes()).unwrap_or("");
                eval_text(engine, path, src, tree, byte, text)
            })
            .collect()
    } else if let [expression] = expressions.as_slice() {
        let text = expression.utf8_text(src.as_bytes()).unwrap_or("");
        match eval_text(engine, path, src, tree, byte, text) {
            Ty::Tuple(items) if items.len() == identifiers.len() => items,
            _ => return,
        }
    } else {
        return;
    };
    for (identifier, ty) in identifiers.iter().zip(types) {
        let Ok(name) = identifier.utf8_text(src.as_bytes()) else {
            continue;
        };
        if name == "_" || !known(&ty) {
            continue;
        }
        let shown = localize(&ty, package).display();
        if shown.chars().count() > MAX_LABEL_CHARS || shown.contains('\n') {
            continue;
        }
        let end = identifier.end_position();
        out.push(ParameterHint {
            line: end.row as u32,
            column: column(end.row, end.column),
            label: format!(": {shown}"),
            return_type: false,
        });
    }
}

fn list_items(node: Node<'_>) -> Vec<Node<'_>> {
    if node.kind() == "expression_list" {
        let mut cursor = node.walk();
        node.named_children(&mut cursor).collect()
    } else {
        vec![node]
    }
}

/// True when every part of `ty` is known and it is a value type.
fn known(ty: &Ty) -> bool {
    match ty {
        Ty::Unknown | Ty::Package(_) | Ty::Tuple(_) => false,
        Ty::Named { args, .. } => args.iter().all(known),
        Ty::Pointer(t) | Ty::Slice(t) | Ty::Array(t) | Ty::Chan(t) => known(t),
        Ty::Map(key, value) => known(key) && known(value),
        Ty::Func {
            params, results, ..
        } => params.iter().chain(results).all(known),
        Ty::Struct | Ty::Interface => true,
    }
}

/// `ty` with the types of package `package` unqualified, as written inside
/// that package.
fn localize(ty: &Ty, package: &str) -> Ty {
    let local = |t: &Ty| localize(t, package);
    let boxed = |t: &Ty| Box::new(localize(t, package));
    match ty {
        Ty::Named { pkg, name, args } => Ty::Named {
            pkg: if pkg == package {
                String::new()
            } else {
                pkg.clone()
            },
            name: name.clone(),
            args: args.iter().map(local).collect(),
        },
        Ty::Pointer(t) => Ty::Pointer(boxed(t)),
        Ty::Slice(t) => Ty::Slice(boxed(t)),
        Ty::Array(t) => Ty::Array(boxed(t)),
        Ty::Chan(t) => Ty::Chan(boxed(t)),
        Ty::Map(key, value) => Ty::Map(boxed(key), boxed(value)),
        Ty::Func {
            params,
            results,
            variadic,
        } => Ty::Func {
            params: params.iter().map(local).collect(),
            results: results.iter().map(local).collect(),
            variadic: *variadic,
        },
        Ty::Tuple(items) => Ty::Tuple(items.iter().map(local).collect()),
        other => other.clone(),
    }
}
