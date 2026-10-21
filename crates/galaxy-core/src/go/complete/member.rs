//! Members after a `.`: fields and methods, or the names of a package.

use super::scan::{self, Pos};
use super::{Cx, Item, is_ident};
use crate::go::engine::{MemberKind, Package};
use crate::go::parse::{FuncDecl, Param, TypeKind, ValueDecl, exported};
use crate::go::types::{Ty, receiver_info};
use crate::lang::ItemKind;

/// Whether the `.` at `dot` follows a number literal (`1.`, `0x1F.`).
pub(super) fn after_number(src: &str, dot: usize) -> bool {
    let token_start = src[..dot]
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '_' || *c == '.')
        .last()
        .map_or(dot, |(i, _)| i);
    src[token_start..dot]
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit())
}

/// `(a int, b ...string) (string, error)`.
pub(super) fn signature(func: &FuncDecl) -> String {
    let param = |p: &Param| {
        let ty = if p.variadic {
            format!("...{}", p.ty)
        } else {
            p.ty.clone()
        };
        if p.name.is_empty() {
            ty
        } else {
            format!("{} {ty}", p.name)
        }
    };
    let params: Vec<String> = func.params.iter().map(param).collect();
    let mut out = format!("({})", params.join(", "));
    match func.results.as_slice() {
        [] => {}
        [one] if one.name.is_empty() => {
            out.push(' ');
            out.push_str(&param(one));
        }
        many => {
            let list: Vec<String> = many.iter().map(param).collect();
            out.push_str(&format!(" ({})", list.join(", ")));
        }
    }
    out
}

pub(super) fn type_kind(kind: TypeKind) -> ItemKind {
    match kind {
        TypeKind::Interface => ItemKind::Interface,
        _ => ItemKind::Class,
    }
}

pub(super) fn value_item(v: &ValueDecl, tier: u8) -> Item {
    let kind = if v.is_const {
        ItemKind::Constant
    } else {
        ItemKind::Variable
    };
    let detail =
        v.ty.clone()
            .or_else(|| v.group_type.clone())
            .or_else(|| {
                v.value
                    .clone()
                    .filter(|t| t.len() <= 40 && !t.contains('\n'))
            })
            .unwrap_or_default();
    Item::new(&v.name, kind, tier).detail(detail)
}

/// Top-level declarations of `pkg`: `exported_only` keeps the exported
/// ones, `types_only` leaves out functions and values.
pub(super) fn package_items(
    pkg: &Package,
    tier: u8,
    exported_only: bool,
    types_only: bool,
) -> Vec<Item> {
    let keep = |name: &str| name != "_" && (!exported_only || exported(name));
    let mut out = Vec::new();
    for file in &pkg.files {
        for t in file.types.iter().filter(|t| keep(&t.name)) {
            out.push(Item::new(&t.name, type_kind(t.kind), tier));
        }
        if types_only {
            continue;
        }
        for f in file
            .funcs
            .iter()
            .filter(|f| f.receiver.is_none() && keep(&f.name) && f.name != "init")
        {
            out.push(
                Item::new(&f.name, ItemKind::Function, tier)
                    .detail(signature(f))
                    .call(!f.params.is_empty()),
            );
        }
        for v in file
            .consts
            .iter()
            .chain(file.vars.iter())
            .filter(|v| keep(&v.name))
        {
            out.push(value_item(v, tier));
        }
    }
    out
}

pub(super) fn items(cx: &mut Cx<'_>, dot: usize) -> Vec<Item> {
    let Some(info) = receiver_info(cx.engine, cx.path, cx.src, cx.tree, cx.byte) else {
        return Vec::new();
    };
    if let Ty::Package(path) = &info.ty {
        let types_only = {
            let qstart = cx.src[..dot]
                .char_indices()
                .rev()
                .take_while(|(_, c)| is_ident(*c))
                .last()
                .map_or(dot, |(i, _)| i);
            scan::type_position(cx.src, qstart, &cx.scan.stack) == Pos::Type
        };
        let pkg = if *path == info.pkg.import_path {
            Some(info.pkg.clone())
        } else {
            cx.engine.package_by_import(path)
        };
        return pkg
            .map(|p| package_items(&p, 0, *path != info.pkg.import_path, types_only))
            .unwrap_or_default();
    }
    let here = info.pkg.dir.clone();
    info.members
        .iter()
        .filter(|m| exported(&m.name) || m.path.parent() == Some(here.as_path()))
        .map(|m| match m.kind {
            MemberKind::Field => Item::new(&m.name, ItemKind::Property, 0).detail(m.ty.clone()),
            MemberKind::Method => Item::new(&m.name, ItemKind::Method, 0)
                .detail(m.ty.clone())
                .call(!m.ty.starts_with("()")),
        })
        .collect()
}
