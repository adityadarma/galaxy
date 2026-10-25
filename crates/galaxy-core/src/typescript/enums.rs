//! Enum members and `Enum.Member` accesses.

use super::*;

/// Members of an `enum` body, with or without an initializer.
pub(super) fn enum_members(body: Node<'_>, text: &str, locations: &Locations) -> Vec<TypeMember> {
    let mut cursor = body.walk();
    body.named_children(&mut cursor)
        .filter_map(|member| {
            let name = if member.kind() == "enum_assignment" {
                member.child_by_field_name("name")?
            } else {
                member
            };
            Some(TypeMember {
                name: direct_member_name(text, name)?,
                range: locations.range(name),
                type_reference: None,
                callback_parameter_type: None,
                return_type: None,
            })
        })
        .collect()
}

/// `Enum.Member` where `Enum` is a same-file enum or a value import. The
/// receiver is resolved by symbol, so a shadowing local never matches.
pub(super) fn collect_enum_member_uses(
    root: Node<'_>,
    text: &str,
    locations: &Locations,
    imports: &[Import],
    type_declarations: &[TypeDeclaration],
    symbols: &[SymbolOccurrences],
    import_symbols: &[SymbolOccurrences],
) -> Vec<TypeMemberUse> {
    let mut uses = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "member_expression"
            && let (Some(object), Some(property)) = (
                node.child_by_field_name("object"),
                node.child_by_field_name("property"),
            )
            && object.kind() == "identifier"
            && property.kind() == "property_identifier"
            && let Some(member_name) = direct_member_name(text, property)
        {
            let range = locations.range(object);
            let local = symbols
                .iter()
                .find(|symbol| symbol.occurrences.contains(&range))
                .and_then(|symbol| {
                    type_declarations
                        .iter()
                        .any(|item| {
                            item.range == symbol.declaration && item.kind == DeclarationKind::Enum
                        })
                        .then_some(TypeMemberTarget::Named(symbol.declaration))
                });
            let imported = || {
                let symbol = import_symbols
                    .iter()
                    .find(|symbol| symbol.occurrences.contains(&range))?;
                imports
                    .iter()
                    .flat_map(|import| import.bindings.iter())
                    .find(|binding| {
                        binding.range == symbol.declaration
                            && !binding.type_only
                            && matches!(binding.kind, ImportKind::Named | ImportKind::Default)
                    })
                    .map(|binding| TypeMemberTarget::ImportedValue(binding.range))
            };
            if let Some(target) = local.or_else(imported) {
                uses.push(TypeMemberUse {
                    target,
                    member_name,
                    member_range: locations.range(property),
                });
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    uses
}
