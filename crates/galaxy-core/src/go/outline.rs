//! Outline of one Go file: types with their fields and methods, functions,
//! constants and variables, in source order.
//!
//! `kind` uses the strings the editor already maps for PHP (`Class`,
//! `Interface`, `Function`, `Method`, `Property`, `Constant`); a struct is a
//! `Class` and a field a `Property`. Other named types and variables get
//! their own strings (`Type`, `Variable`).

use std::collections::HashMap;
use std::path::Path;

use crate::lang::OutlineSymbol;

use super::parse::{Pos, TypeKind, parse_file};

/// Symbols returned, children included.
const MAX_SYMBOLS: usize = 2000;

fn symbol(name: &str, kind: &'static str, pos: Pos) -> OutlineSymbol {
    OutlineSymbol {
        name: name.to_owned(),
        kind,
        line: pos.line,
        column: pos.column,
        children: Vec::new(),
    }
}

/// Top-level declarations of `src`. Methods sit below their receiver type
/// when it is declared in the same file, else at top level. Capped at 2000
/// symbols.
pub fn outline(path: &Path, src: &str) -> Vec<OutlineSymbol> {
    let file = parse_file(path, src);
    let mut budget = MAX_SYMBOLS;
    let mut take = |count: usize| -> bool {
        if budget < count {
            return false;
        }
        budget -= count;
        true
    };
    let mut top: Vec<OutlineSymbol> = Vec::new();
    let mut by_name: HashMap<&str, usize> = HashMap::new();
    for ty in &file.types {
        if !take(1) {
            break;
        }
        let kind = match ty.kind {
            TypeKind::Struct => "Class",
            TypeKind::Interface => "Interface",
            TypeKind::Alias | TypeKind::Named => "Type",
        };
        let mut sym = symbol(&ty.name, kind, ty.pos);
        for field in &ty.fields {
            if take(1) {
                sym.children
                    .push(symbol(&field.name, "Property", field.pos));
            }
        }
        for method in &ty.methods {
            if take(1) {
                sym.children
                    .push(symbol(&method.name, "Method", method.pos));
            }
        }
        by_name.entry(&ty.name).or_insert(top.len());
        top.push(sym);
    }
    for func in &file.funcs {
        if !take(1) {
            break;
        }
        match &func.receiver {
            Some(recv) => {
                let sym = symbol(&func.name, "Method", func.pos);
                match by_name.get(recv.base.as_str()) {
                    Some(&at) => top[at].children.push(sym),
                    None => top.push(sym),
                }
            }
            None => top.push(symbol(&func.name, "Function", func.pos)),
        }
    }
    for value in file.consts.iter().chain(file.vars.iter()) {
        if !take(1) {
            break;
        }
        let kind = if value.is_const {
            "Constant"
        } else {
            "Variable"
        };
        top.push(symbol(&value.name, kind, value.pos));
    }
    for sym in &mut top {
        sym.children.sort_by_key(|c| (c.line, c.column));
    }
    top.sort_by_key(|s| (s.line, s.column));
    top
}

#[cfg(test)]
mod tests;
