//! The function around a cursor.

use tree_sitter::{Node, Tree};

use super::walk::orphan_at;
use super::{closed, reach, text, type_text};

/// Deepest type nesting followed to find a receiver's base name.
const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncContext {
    /// Empty for a function literal.
    pub name: String,
    /// Base type name of the receiver: `Server` for `*Server[T]`.
    pub receiver_type: Option<String>,
    pub is_method: bool,
    pub start_line: u32,
    pub end_line: u32,
    /// One entry per result, `a, b int` giving two.
    pub result_types: Vec<String>,
    pub is_literal: bool,
}

/// The innermost function declaration, method or function literal holding
/// `byte`.
pub fn enclosing_func(tree: &Tree, src: &str, byte: usize) -> Option<FuncContext> {
    let byte = byte.min(src.len());
    let root = tree.root_node();
    let mut found = None;
    let mut node = root;
    // Down to the cursor; the last function passed is the innermost.
    loop {
        let mut cursor = node.walk();
        let mut next = None;
        for child in node.children(&mut cursor) {
            if child.start_byte() > byte {
                break;
            }
            if byte <= reach(src, child) && !(byte == child.end_byte() && closed(child)) {
                if child.start_byte() < byte {
                    next = Some(child);
                    break;
                }
                next.get_or_insert(child);
            }
        }
        let Some(child) = next else { break };
        if matches!(
            child.kind(),
            "function_declaration" | "method_declaration" | "func_literal"
        ) {
            found = Some(child);
        }
        node = child;
    }
    found
        .or_else(|| orphan_at(root, byte).map(|(func, _)| func))
        .map(|func| context(src, func))
}

fn context(src: &str, node: Node<'_>) -> FuncContext {
    let is_method = node.kind() == "method_declaration";
    let receiver_type = node
        .child_by_field_name("receiver")
        .and_then(|list| {
            let mut cursor = list.walk();
            let decl = list
                .named_children(&mut cursor)
                .find(|n| n.kind() == "parameter_declaration")?;
            decl.child_by_field_name("type")
        })
        .and_then(|ty| base_name(src, ty));
    let mut result_types = Vec::new();
    match node.child_by_field_name("result") {
        Some(list) if list.kind() == "parameter_list" => {
            let mut cursor = list.walk();
            for decl in list.named_children(&mut cursor) {
                let Some(ty) = decl.child_by_field_name("type") else {
                    continue;
                };
                let ty = type_text(src, ty);
                let mut names = decl.walk();
                let count = decl.children_by_field_name("name", &mut names).count();
                for _ in 0..count.max(1) {
                    result_types.push(ty.clone());
                }
            }
        }
        Some(single) => result_types.push(type_text(src, single)),
        None => {}
    }
    FuncContext {
        name: node
            .child_by_field_name("name")
            .map(|n| text(src, n).to_owned())
            .unwrap_or_default(),
        receiver_type,
        is_method,
        start_line: node.start_position().row as u32,
        end_line: node.end_position().row as u32,
        result_types,
        is_literal: node.kind() == "func_literal",
    }
}

/// The identifier naming a (possibly pointer, generic or qualified) type.
fn base_name(src: &str, node: Node<'_>) -> Option<String> {
    let mut current = node;
    for _ in 0..MAX_DEPTH {
        match current.kind() {
            "type_identifier" => return Some(text(src, current).to_owned()),
            "qualified_type" => {
                return current
                    .child_by_field_name("name")
                    .map(|n| text(src, n).to_owned());
            }
            "generic_type" => current = current.child_by_field_name("type")?,
            "pointer_type" | "parenthesized_type" => current = current.named_child(0)?,
            _ => return None,
        }
    }
    None
}
