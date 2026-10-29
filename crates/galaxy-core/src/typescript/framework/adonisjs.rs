//! AdonisJS Lucid.
//!
//! Relation decorators (`@hasMany(() => X, { onQuery(query) {} })`) type the
//! `onQuery` parameter through a generic indexed access that the index does
//! not evaluate. Lucid declares every chainable method on
//! `ChainableContract`, so a member used on the parameter, or on a nested
//! builder callback parameter, resolves there.

use std::path::Path;

use tree_sitter::Node;

use super::Context;
use crate::typescript::{
    ImportKind, TypeMemberTarget, TypeMemberUse, direct_member_name, node_text,
    sole_callback_parameter, type_declaration_extends,
};

const PACKAGE: &str = "@adonisjs/lucid/orm";
const DATABASE_PACKAGE: &str = "@adonisjs/lucid/database";
const MACROABLE_BUILDERS: [&str; 2] = ["DatabaseQueryBuilder", "ModelQueryBuilder"];
/// Public subpath export of the package (`"./types/*"`), stable across builds.
const DECLARATION: &str = "@adonisjs/lucid/types/querybuilder";
const BUILDER: &str = "ChainableContract";
const MODEL_DECLARATION: &str = "@adonisjs/lucid/types/model";
const MODEL: &str = "LucidModel";
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
        collect_model_query_members(node, root, context, &mut uses);
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

/// Finds local classes that descend from the AdonisJS Macroable import. This
/// stays in the framework adapter so the core checker has no package policy.
pub(super) fn macroable_classes(path: &Path, root: Node<'_>, context: &Context<'_>) -> Vec<String> {
    let mut macroable = context
        .imports
        .iter()
        .filter(|import| import.specifier == "@poppinss/macroable")
        .flat_map(|import| import.bindings.iter())
        .filter(|binding| {
            !binding.type_only && matches!(binding.kind, ImportKind::Default | ImportKind::Named)
        })
        .map(|binding| binding.local_name.clone())
        .collect::<std::collections::HashSet<_>>();
    macroable.extend(
        context
            .imports
            .iter()
            // `ModelQueryBuilder` ships from `/orm`; `DatabaseQueryBuilder`
            // is re-exported from `/database` (and `/orm` in older releases).
            .filter(|import| matches!(import.specifier.as_str(), PACKAGE | DATABASE_PACKAGE))
            .flat_map(|import| import.bindings.iter())
            .filter(|binding| {
                !binding.type_only
                    && matches!(binding.kind, ImportKind::Named)
                    && binding
                        .imported_name
                        .as_deref()
                        .is_some_and(|name| MACROABLE_BUILDERS.contains(&name))
            })
            .map(|binding| binding.local_name.clone()),
    );
    if path.ends_with("node_modules/@adonisjs/lucid/build/src/orm/query_builder/index.d.ts") {
        macroable.extend(MACROABLE_BUILDERS.map(str::to_owned));
    }
    // Imported Lucid builders are values in this file even though they are
    // not declared here; they must be returned alongside local subclasses.
    let imported_builders = macroable
        .iter()
        .filter(|name| {
            context.imports.iter().any(|import| {
                matches!(import.specifier.as_str(), PACKAGE | DATABASE_PACKAGE)
                    && import
                        .bindings
                        .iter()
                        .any(|binding| &binding.local_name == *name)
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut classes = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "class_declaration" | "abstract_class_declaration"
        ) && let Some(name) = node
            .child_by_field_name("name")
            .and_then(|name| node_text(context.text, name))
        {
            classes.push((
                name.to_owned(),
                type_declaration_extends(node, context.text),
            ));
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    while classes.iter().any(|(name, parents)| {
        !macroable.contains(name) && parents.iter().any(|parent| macroable.contains(parent))
    }) {
        for (name, parents) in &classes {
            if parents.iter().any(|parent| macroable.contains(parent)) {
                macroable.insert(name.clone());
            }
        }
    }
    let mut result = classes
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| macroable.contains(name))
        .collect::<Vec<_>>();
    for name in imported_builders {
        if !result.contains(&name) {
            result.push(name);
        }
    }
    result
}

/// `BaseModel` exposes static `query`; its result is a fluent Lucid builder.
/// Generic model inference is outside this syntactic index, but these two
/// public contracts cover direct model queries without guessing unrelated APIs.
fn collect_model_query_members(
    node: Node<'_>,
    root: Node<'_>,
    context: &Context<'_>,
    uses: &mut Vec<TypeMemberUse>,
) {
    let Some(property) = (node.kind() == "member_expression")
        .then(|| node.child_by_field_name("property"))
        .flatten()
    else {
        return;
    };
    let Some(member_name) = direct_member_name(context.text, property) else {
        return;
    };
    let Some(object) = node.child_by_field_name("object") else {
        return;
    };
    let target = if let Some(model_name) = node_text(context.text, object)
        && model_extends_lucid_base_model(root, context, model_name)
    {
        Some(TypeMemberTarget::PackageDeclaration {
            specifier: MODEL_DECLARATION,
            type_name: MODEL,
        })
    } else if query_call_model(object, context.text)
        .is_some_and(|model_name| model_extends_lucid_base_model(root, context, model_name))
    {
        Some(TypeMemberTarget::PackageDeclaration {
            specifier: DECLARATION,
            type_name: BUILDER,
        })
    } else {
        None
    };
    if let Some(target) = target {
        uses.push(TypeMemberUse {
            target,
            member_name,
            member_range: context.locations.range(property),
        });
    }
}

/// Returns the model receiver for `Audit.query()` and every fluent call rooted in it.
fn query_call_model<'a>(node: Node<'a>, text: &'a str) -> Option<&'a str> {
    let call = (node.kind() == "call_expression").then_some(node)?;
    let function = call.child_by_field_name("function")?;
    let object = function.child_by_field_name("object")?;
    let method = function.child_by_field_name("property")?;
    if direct_member_name(text, method)? == "query" {
        return node_text(text, object);
    }
    query_call_model(object, text)
}

fn model_extends_lucid_base_model(root: Node<'_>, context: &Context<'_>, model_name: &str) -> bool {
    let base_model_names = context
        .imports
        .iter()
        .filter(|import| import.specifier == PACKAGE)
        .flat_map(|import| import.bindings.iter())
        .filter(|binding| {
            !binding.type_only && binding.imported_name.as_deref() == Some("BaseModel")
        })
        .map(|binding| binding.local_name.as_str())
        .collect::<Vec<_>>();
    if base_model_names.is_empty() {
        return false;
    }
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "class_declaration" | "class")
            && node
                .child_by_field_name("name")
                .and_then(|name| node_text(context.text, name))
                == Some(model_name)
        {
            let mut cursor = node.walk();
            return node
                .named_children(&mut cursor)
                .filter(|child| child.kind() == "class_heritage")
                .flat_map(|heritage| {
                    let mut cursor = heritage.walk();
                    heritage.named_children(&mut cursor).collect::<Vec<_>>()
                })
                .filter(|clause| clause.kind() == "extends_clause")
                .flat_map(|clause| {
                    let mut cursor = clause.walk();
                    clause.named_children(&mut cursor).collect::<Vec<_>>()
                })
                .filter_map(|base| node_text(context.text, base))
                .any(|base| base_model_names.contains(&base));
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    false
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
