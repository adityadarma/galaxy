//! Keys of a struct literal: `User{ | }`.

use super::{Cx, Item, is_ident};
use crate::go::engine::MemberKind;
use crate::go::parse::exported;
use crate::go::types::expr_info;
use crate::lang::ItemKind;

/// Keys already written at depth 0 of `seg` and whether the cursor, which
/// follows `seg`, starts a new element.
fn elements(seg: &str) -> (Vec<String>, bool) {
    let mut keys = Vec::new();
    let mut depth = 0i32;
    let mut in_str: Option<char> = None;
    let mut start = 0;
    let mut chars = seg.char_indices().peekable();
    let take = |from: usize, to: usize, keys: &mut Vec<String>| {
        let part = seg[from..to].trim();
        if let Some((name, rest)) = part.split_once(':')
            && !rest.starts_with('=')
            && !name.is_empty()
            && name.chars().all(is_ident)
        {
            keys.push(name.to_owned());
        }
    };
    while let Some((i, c)) = chars.next() {
        if let Some(q) = in_str {
            if c == '\\' && q != '`' {
                chars.next();
            } else if c == q {
                in_str = None;
            }
            continue;
        }
        match c {
            '"' | '`' | '\'' => in_str = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' | '\n' if depth == 0 => {
                take(start, i, &mut keys);
                start = i + 1;
            }
            _ => {}
        }
    }
    let fresh = depth == 0 && in_str.is_none() && seg[start..].trim().is_empty();
    (keys, fresh)
}

pub(super) fn key_items(cx: &mut Cx<'_>) -> Option<Vec<Item>> {
    let &(b'{', open) = cx.scan.stack.last()? else {
        return None;
    };
    let (keyed, fresh) = elements(cx.src.get(open + 1..cx.start)?);
    if !fresh {
        return None;
    }
    let head = &cx.src[..open];
    let name_start = head
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_ident(*c) || *c == '.')
        .last()
        .map(|(i, _)| i)?;
    let name = &head[name_start..];
    let first = name.chars().next()?;
    if first == '.' || name.ends_with('.') || first.is_ascii_digit() {
        return None;
    }
    if matches!(name, "struct" | "interface" | "else" | "range" | "func") {
        return None;
    }
    // `[]User{` and `map[string]User{` hold elements, not fields.
    if head[..name_start].ends_with(']') {
        return None;
    }
    let expr = format!("{name}{{}}");
    let info = expr_info(cx.engine, cx.path, cx.src, cx.tree, cx.byte, &expr)?;
    if !matches!(info.ty.deref(), crate::go::types::Ty::Named { .. }) {
        return None;
    }
    let here = info.pkg.dir.clone();
    let items: Vec<Item> = info
        .members
        .iter()
        .filter(|m| m.kind == MemberKind::Field && m.embedded_via.is_empty())
        .filter(|m| exported(&m.name) || m.path.parent() == Some(here.as_path()))
        .filter(|m| !keyed.contains(&m.name))
        .map(|m| {
            let mut item = Item::new(&m.name, ItemKind::Property, 0).detail(m.ty.clone());
            item.insert = format!("{}: ", m.name);
            item
        })
        .collect();
    let has_fields = info
        .members
        .iter()
        .any(|m| m.kind == MemberKind::Field && m.embedded_via.is_empty());
    has_fields.then_some(items)
}
