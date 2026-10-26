//! Members reached through an imported superclass, and callbacks passed to its typed properties.

use super::*;

pub(super) fn collect_inherited_member_uses(
    root: Node<'_>,
    text: &str,
    locations: &Locations,
    imports: &[Import],
    symbols: &[SymbolOccurrences],
    import_symbols: &[SymbolOccurrences],
) -> Vec<TypeMemberUse> {
    let mut uses = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "arrow_function" {
            let callback = (|| {
                let parameter = sole_callback_parameter(node)?;
                let call = enclosing_call(node)?;
                let function = call.child_by_field_name("function")?;
                (function.kind() == "member_expression").then_some(())?;
                let object = function.child_by_field_name("object")?;
                let method = function.child_by_field_name("property")?;
                (object.kind() == "member_expression").then_some(())?;
                let receiver = object.child_by_field_name("object")?;
                let intermediate = object.child_by_field_name("property")?;
                (receiver.kind() == "this").then_some(())?;
                let callback_name = direct_member_name(text, method)?;
                let intermediate_name = direct_member_name(text, intermediate)?;
                let class = enclosing_class(node)?;
                let binding = imported_superclass_binding(
                    class,
                    text,
                    locations,
                    imports,
                    symbols,
                    import_symbols,
                )?;
                Some((parameter, binding, intermediate_name, callback_name))
            })();
            if let Some((parameter, binding, intermediate_name, callback_name)) = callback {
                let parameter_name = node_text(text, parameter).unwrap_or_default();
                let mut cursor = node.walk();
                for descendant in node.named_children(&mut cursor) {
                    collect_callback_member_uses(
                        descendant,
                        text,
                        locations,
                        parameter_name,
                        binding,
                        &intermediate_name,
                        &callback_name,
                        &mut uses,
                    );
                }
            }
        }
        if matches!(node.kind(), "member_expression" | "subscript_expression")
            && let (Some(object), Some(property)) = (
                node.child_by_field_name("object"),
                node.child_by_field_name(if node.kind() == "member_expression" {
                    "property"
                } else {
                    "index"
                }),
            )
            && object.kind() == "this"
            && let Some(member_name) = direct_member_name(text, property)
            && let Some(class) = enclosing_class(node)
            && let Some(binding_range) = imported_superclass_binding(
                class,
                text,
                locations,
                imports,
                symbols,
                import_symbols,
            )
        {
            uses.push(TypeMemberUse {
                target: TypeMemberTarget::SuperImported(binding_range),
                member_name,
                member_range: locations.range(property),
            });
        }
        if matches!(node.kind(), "member_expression" | "subscript_expression")
            && let (Some(object), Some(property)) = (
                node.child_by_field_name("object"),
                node.child_by_field_name(if node.kind() == "member_expression" {
                    "property"
                } else {
                    "index"
                }),
            )
            && object.kind() == "this"
            && let Some(member_name) = direct_member_name(text, property)
            && let Some(class) = enclosing_class(node)
            && imported_superclass_binding(class, text, locations, imports, symbols, import_symbols)
                .is_none()
            && let Some(declaration_range) =
                local_superclass_declaration(class, text, locations, symbols)
        {
            uses.push(TypeMemberUse {
                target: TypeMemberTarget::SuperLocal(declaration_range),
                member_name,
                member_range: locations.range(property),
            });
        }
        if node.kind() == "member_expression"
            && let Some(property) = node.child_by_field_name("property")
            && let Some(member_name) = direct_member_name(text, property)
            && let Some(object) = node.child_by_field_name("object")
            && let Some((chain, class)) = fluent_this_method_chain(object, text)
        {
            if let Some(binding) = imported_superclass_binding(
                class,
                text,
                locations,
                imports,
                symbols,
                import_symbols,
            ) {
                uses.push(TypeMemberUse {
                    target: TypeMemberTarget::SuperImportedChain { binding, chain },
                    member_name,
                    member_range: locations.range(property),
                });
            } else if let Some(declaration) =
                local_superclass_declaration(class, text, locations, symbols)
            {
                uses.push(TypeMemberUse {
                    target: TypeMemberTarget::SuperLocalChain { declaration, chain },
                    member_name,
                    member_range: locations.range(property),
                });
            }
        }
        if node.kind() == "member_expression"
            && let (Some(object), Some(property)) = (
                node.child_by_field_name("object"),
                node.child_by_field_name("property"),
            )
            && object.kind() == "member_expression"
            && let (Some(receiver), Some(intermediate)) = (
                object.child_by_field_name("object"),
                object.child_by_field_name("property"),
            )
            && receiver.kind() == "this"
            && let (Some(member_name), Some(intermediate_name), Some(class)) = (
                direct_member_name(text, property),
                direct_member_name(text, intermediate),
                enclosing_class(node),
            )
            && let Some(binding) = imported_superclass_binding(
                class,
                text,
                locations,
                imports,
                symbols,
                import_symbols,
            )
        {
            uses.push(TypeMemberUse {
                target: TypeMemberTarget::SuperMember {
                    binding,
                    member_name: intermediate_name,
                },
                member_name,
                member_range: locations.range(property),
            });
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    uses
}

fn fluent_this_method_chain<'a>(node: Node<'a>, text: &str) -> Option<(Vec<String>, Node<'a>)> {
    let call = (node.kind() == "call_expression").then_some(node)?;
    let function = call.child_by_field_name("function")?;
    let property = function.child_by_field_name("property")?;
    let method = direct_member_name(text, property)?;
    let object = function.child_by_field_name("object")?;
    if object.kind() == "this" {
        return Some((vec![method], enclosing_class(node)?));
    }
    let (mut chain, class) = fluent_this_method_chain(object, text)?;
    chain.push(method);
    Some((chain, class))
}

fn local_superclass_declaration(
    class: Node<'_>,
    text: &str,
    locations: &Locations,
    symbols: &[SymbolOccurrences],
) -> Option<Utf16Range> {
    let mut cursor = class.walk();
    let heritage = class
        .named_children(&mut cursor)
        .find(|child| child.kind() == "class_heritage")?;
    let mut cursor = heritage.walk();
    let clause = heritage
        .named_children(&mut cursor)
        .find(|child| child.kind() == "extends_clause")?;
    let mut cursor = clause.walk();
    let base = clause
        .named_children(&mut cursor)
        .find(|child| matches!(child.kind(), "identifier" | "type_identifier"))?;
    let name = node_text(text, base)?;
    let base_range = locations.range(base);
    symbols
        .iter()
        .find(|symbol| {
            symbol.name == name
                && symbol.kind == DeclarationKind::Class
                && symbol.occurrences.contains(&base_range)
        })
        .map(|symbol| symbol.declaration)
}

pub(super) fn sole_callback_parameter(node: Node<'_>) -> Option<Node<'_>> {
    let parameters = node
        .child_by_field_name("parameter")
        .or_else(|| node.child_by_field_name("parameters"))?;
    let mut descendants = vec![parameters];
    let mut identifiers = Vec::new();
    while let Some(descendant) = descendants.pop() {
        if descendant.kind() == "identifier" {
            identifiers.push(descendant);
        }
        let mut cursor = descendant.walk();
        descendants.extend(descendant.named_children(&mut cursor));
    }
    let [parameter] = identifiers.as_slice() else {
        return None;
    };
    Some(*parameter)
}

pub(super) fn enclosing_call(mut node: Node<'_>) -> Option<Node<'_>> {
    while let Some(parent) = node.parent() {
        if parent.kind() == "call_expression"
            && parent
                .child_by_field_name("arguments")
                .is_some_and(|arguments| arguments == node)
        {
            return Some(parent);
        }
        node = parent;
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(super) fn collect_callback_member_uses(
    node: Node<'_>,
    text: &str,
    locations: &Locations,
    parameter_name: &str,
    binding: Utf16Range,
    intermediate_name: &str,
    member_name: &str,
    uses: &mut Vec<TypeMemberUse>,
) {
    if node.kind() == "member_expression"
        && let (Some(object), Some(property)) = (
            node.child_by_field_name("object"),
            node.child_by_field_name("property"),
        )
        && let Some(name) = direct_member_name(text, property)
    {
        let chain = if node_text(text, object) == Some(parameter_name) {
            Some(Vec::new())
        } else {
            callback_method_chain(object, text, parameter_name)
        };
        if let Some(chain) = chain {
            uses.push(TypeMemberUse {
                target: TypeMemberTarget::SuperMemberCallback {
                    binding,
                    member_name: intermediate_name.to_owned(),
                    callback_name: member_name.to_owned(),
                    chain,
                },
                member_name: name,
                member_range: locations.range(property),
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_callback_member_uses(
            child,
            text,
            locations,
            parameter_name,
            binding,
            intermediate_name,
            member_name,
            uses,
        );
    }
}

pub(super) fn callback_method_chain(
    node: Node<'_>,
    text: &str,
    parameter_name: &str,
) -> Option<Vec<String>> {
    let call = (node.kind() == "call_expression").then_some(node)?;
    let function = call.child_by_field_name("function")?;
    let object = function.child_by_field_name("object")?;
    let method = direct_member_name(text, function.child_by_field_name("property")?)?;
    let mut chain = if node_text(text, object) == Some(parameter_name) {
        Vec::new()
    } else {
        callback_method_chain(object, text, parameter_name)?
    };
    chain.push(method);
    Some(chain)
}

pub(super) fn enclosing_class(mut node: Node<'_>) -> Option<Node<'_>> {
    while let Some(parent) = node.parent() {
        if matches!(parent.kind(), "class_declaration" | "class") {
            return Some(parent);
        }
        node = parent;
    }
    None
}

pub(super) fn imported_superclass_binding(
    class: Node<'_>,
    text: &str,
    locations: &Locations,
    imports: &[Import],
    symbols: &[SymbolOccurrences],
    import_symbols: &[SymbolOccurrences],
) -> Option<Utf16Range> {
    let mut cursor = class.walk();
    let heritage = class
        .named_children(&mut cursor)
        .find(|child| child.kind() == "class_heritage")?;
    let mut cursor = heritage.walk();
    let clauses = heritage
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "extends_clause")
        .collect::<Vec<_>>();
    let [clause] = clauses.as_slice() else {
        return None;
    };
    let mut cursor = clause.walk();
    let bases = clause
        .named_children(&mut cursor)
        .filter(|child| matches!(child.kind(), "identifier" | "type_identifier"))
        .collect::<Vec<_>>();
    let [base] = bases.as_slice() else {
        return None;
    };
    let base_range = locations.range(*base);
    let binding_range = import_symbols
        .iter()
        .find(|symbol| symbol.occurrences.contains(&base_range))
        .map(|symbol| symbol.declaration)
        .or_else(|| {
            let name = node_text(text, *base)?;
            (!symbols.iter().any(|symbol| symbol.name == name))
                .then(|| {
                    imports
                        .iter()
                        .flat_map(|import| import.bindings.iter())
                        .filter(|binding| binding.local_name == name)
                        .map(|binding| binding.range)
                        .collect::<Vec<_>>()
                })
                .and_then(|matches| (matches.len() == 1).then(|| matches[0]))
        })?;
    let (_, binding) = imports.iter().find_map(|import| {
        import
            .bindings
            .iter()
            .find(|binding| binding.range == binding_range)
            .map(|binding| (import, binding))
    })?;
    (matches!(binding.kind, ImportKind::Named | ImportKind::Default) && !binding.type_only)
        .then_some(binding.range)
}
