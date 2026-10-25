//! AdonisJS Lucid.
//!
//! Relation decorators (`@hasMany(() => X, { onQuery(query) {} })`) type the
//! `onQuery` parameter through a generic indexed access that the index does
//! not evaluate. Lucid declares every chainable method on
//! `ChainableContract`, so a member used on the parameter, or on a nested
//! builder callback parameter, resolves there.

use tree_sitter::Node;

use super::Context;
use crate::typescript::{
    TypeMemberTarget, TypeMemberUse, direct_member_name, node_text, sole_callback_parameter,
};

const PACKAGE: &str = "@adonisjs/lucid/orm";
/// Public subpath export of the package (`"./types/*"`), stable across builds.
const DECLARATION: &str = "@adonisjs/lucid/types/querybuilder";
const BUILDER: &str = "ChainableContract";
const RELATIONS: [&str; 5] = [
    "hasOne",
    "hasMany",
    "belongsTo",
    "manyToMany",
    "hasManyThrough",
];

fn is_callback(kind: &str) -> bool {
    matches!(
        kind,
        "arrow_function" | "function_expression" | "method_definition"
    )
}

pub(super) fn collect(root: Node<'_>, context: &Context<'_>) -> Vec<TypeMemberUse> {
    let mut uses = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if is_callback(node.kind())
            && let Some(parameter) = sole_callback_parameter(node)
            && is_relation_option(node, context)
        {
            let name = node_text(context.text, parameter).unwrap_or_default();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_members(child, context, name, &mut uses);
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    uses
}

/// `true` when `callback` is the `onQuery` option of a Lucid relation call.
fn is_relation_option(callback: Node<'_>, context: &Context<'_>) -> bool {
    relation_import(callback, context).is_some()
}

fn relation_import(callback: Node<'_>, context: &Context<'_>) -> Option<()> {
    let property = if callback.kind() == "method_definition" {
        callback
    } else {
        std::iter::successors(callback.parent(), |item| item.parent())
            .find(|item| matches!(item.kind(), "pair" | "method_definition"))?
    };
    let key = property.child_by_field_name(if property.kind() == "pair" {
        "key"
    } else {
        "name"
    })?;
    if node_text(context.text, key) != Some("onQuery") {
        return None;
    }
    let call = std::iter::successors(property.parent(), |item| item.parent())
        .find(|item| item.kind() == "call_expression")?;
    let function = call.child_by_field_name("function")?;
    if function.kind() != "identifier" {
        return None;
    }
    let name = node_text(context.text, function)?;
    let range = context.locations.range(function);
    let symbol = context
        .import_symbols
        .iter()
        .find(|symbol| symbol.name == name && symbol.occurrences.contains(&range))?;
    context
        .imports
        .iter()
        .filter(|import| import.specifier == PACKAGE)
        .flat_map(|import| import.bindings.iter())
        .any(|binding| {
            binding.range == symbol.declaration
                && binding
                    .imported_name
                    .as_deref()
                    .is_some_and(|name| RELATIONS.contains(&name))
        })
        .then_some(())
}

/// Records `parameter.member` uses and follows builder callbacks passed to a
/// method of `parameter`, such as `query.where((sub) => sub.where(...))`.
fn collect_members(
    node: Node<'_>,
    context: &Context<'_>,
    parameter: &str,
    uses: &mut Vec<TypeMemberUse>,
) {
    if node.kind() == "call_expression"
        && let Some(function) = node.child_by_field_name("function")
        && function.kind() == "member_expression"
        && let Some(object) = function.child_by_field_name("object")
        && node_text(context.text, object) == Some(parameter)
        && let Some(arguments) = node.child_by_field_name("arguments")
    {
        let mut cursor = arguments.walk();
        for argument in arguments.named_children(&mut cursor) {
            let callback = if argument.kind() == "parenthesized_expression" {
                argument.named_child(0)
            } else {
                Some(argument)
            };
            let Some(callback) = callback.filter(|item| is_callback(item.kind())) else {
                continue;
            };
            let Some(inner) = sole_callback_parameter(callback) else {
                continue;
            };
            let inner = node_text(context.text, inner).unwrap_or_default();
            let mut inner_cursor = callback.walk();
            for child in callback.named_children(&mut inner_cursor) {
                collect_members(child, context, inner, uses);
            }
        }
    }
    if node.kind() == "member_expression"
        && let (Some(object), Some(property)) = (
            node.child_by_field_name("object"),
            node.child_by_field_name("property"),
        )
        && node_text(context.text, object) == Some(parameter)
        && let Some(member_name) = direct_member_name(context.text, property)
    {
        uses.push(TypeMemberUse {
            target: TypeMemberTarget::PackageDeclaration {
                specifier: DECLARATION,
                type_name: BUILDER,
            },
            member_name,
            member_range: context.locations.range(property),
        });
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_members(child, context, parameter, uses);
    }
}
