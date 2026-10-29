//! Explicitly typed receivers and the type members they reach.

use super::*;

pub(super) fn collect_type_navigation_declarations(
    node: Node<'_>,
    text: &str,
    locations: &Locations,
    declarations: &mut Vec<TypeDeclaration>,
) {
    if matches!(
        node.kind(),
        "interface_declaration"
            | "type_alias_declaration"
            | "class_declaration"
            | "abstract_class_declaration"
            | "class"
    ) && let Some(name_node) = node.child_by_field_name("name")
        && let Some(name) = node_text(text, name_node)
        && let Some(members) = direct_type_members(node, text, locations)
    {
        declarations.push(TypeDeclaration {
            name: name.to_owned(),
            range: locations.range(name_node),
            members,
            kind: declaration_kind(node.kind()).unwrap_or(DeclarationKind::Class),
            extends: type_declaration_extends(node, text),
        });
    }
    if node.kind() == "enum_declaration"
        && let Some(name_node) = node.child_by_field_name("name")
        && let Some(name) = node_text(text, name_node)
        && let Some(body) = node.child_by_field_name("body")
    {
        declarations.push(TypeDeclaration {
            name: name.to_owned(),
            range: locations.range(name_node),
            members: enum_members(body, text, locations),
            kind: DeclarationKind::Enum,
            extends: Vec::new(),
        });
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_type_navigation_declarations(child, text, locations, declarations);
    }
}

pub(super) fn type_declaration_extends(node: Node<'_>, text: &str) -> Vec<String> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| matches!(child.kind(), "extends_type_clause" | "class_heritage"))
        .flat_map(|clause| {
            let mut descendants = vec![clause];
            let mut names = Vec::new();
            while let Some(descendant) = descendants.pop() {
                if matches!(descendant.kind(), "type_identifier" | "identifier")
                    && let Some(name) = node_text(text, descendant)
                {
                    names.push(name.to_owned());
                }
                let mut cursor = descendant.walk();
                descendants.extend(descendant.named_children(&mut cursor));
            }
            names
        })
        .collect()
}

pub(super) fn direct_type_members(
    node: Node<'_>,
    text: &str,
    locations: &Locations,
) -> Option<Vec<TypeMember>> {
    let body = if node.kind() == "type_alias_declaration" {
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .find(|child| child.kind() == "object_type")?
    } else {
        node.child_by_field_name("body")?
    };
    type_members_from_body(body, text, locations)
}

pub(super) fn type_members_from_body(
    body: Node<'_>,
    text: &str,
    locations: &Locations,
) -> Option<Vec<TypeMember>> {
    let mut cursor = body.walk();
    let mut members = Vec::new();
    for member in body.named_children(&mut cursor) {
        if !matches!(
            member.kind(),
            "property_signature"
                | "method_signature"
                | "public_field_definition"
                | "method_definition"
        ) {
            continue;
        }
        let Some(name) = member.child_by_field_name("name") else {
            continue;
        };
        if name.kind() != "property_identifier" && name.kind() != "identifier" {
            continue;
        }
        if let Some(member_name) = node_text(text, name) {
            members.push(TypeMember {
                name: member_name.to_owned(),
                range: locations.range(name),
                type_reference: member_type_reference(member, text),
                callback_parameter_type: member_callback_parameter_type(member, text),
                return_type: member_return_type(member, text),
            });
        }
    }
    Some(members)
}

pub(super) fn collect_decorator_option_types(
    root: Node<'_>,
    text: &str,
    locations: &Locations,
) -> Vec<(String, DecoratorOptionType)> {
    let mut types = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "type_alias_declaration"
            && let Some(name) = node.child_by_field_name("name")
            && let Some(name) = node_text(text, name)
            && let Some(option) = callable_decorator_option_type(node, text, locations)
        {
            types.push((name.to_owned(), option));
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    types
}

fn callable_decorator_option_type(
    node: Node<'_>,
    text: &str,
    locations: &Locations,
) -> Option<DecoratorOptionType> {
    let value = node.named_child(1)?;
    if value.kind() == "type_identifier" {
        return Some(DecoratorOptionType {
            members: Vec::new(),
            references: vec![node_text(text, value)?.to_owned()],
        });
    }
    let function = (value.kind() == "function_type").then_some(value)?;
    let parameters = function.child_by_field_name("parameters")?;
    let parameter = sole_type_parameter(parameters)?;
    let annotation = parameter.child_by_field_name("type")?;
    decorator_option_shape(annotation.named_child(0)?, text, locations)
}

pub(super) fn collect_callable_type_annotations(
    root: Node<'_>,
    text: &str,
) -> Vec<CallableTypeAnnotation> {
    let mut annotations = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "variable_declarator"
            && let Some(name) = node.child_by_field_name("name")
            && name.kind() == "identifier"
            && let Some(variable_name) = node_text(text, name)
            && let Some(annotation) = node.child_by_field_name("type")
            && let Some(type_name) = annotation
                .named_child(0)
                .filter(|node| node.kind() == "type_identifier")
                .and_then(|node| node_text(text, node))
        {
            annotations.push(CallableTypeAnnotation {
                name: variable_name.to_owned(),
                type_name: type_name.to_owned(),
            });
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    annotations
}

pub(super) fn collect_callable_member_type_annotations(
    root: Node<'_>,
    text: &str,
) -> Vec<CallableMemberTypeAnnotation> {
    let mut annotations = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "variable_declarator"
            && let Some(name) = node.child_by_field_name("name")
            && name.kind() == "identifier"
            && let Some(object_name) = node_text(text, name)
            && let Some(annotation) = node.child_by_field_name("type")
            && let Some(type_node) = annotation.named_child(0)
        {
            collect_callable_members_from_type(type_node, object_name, text, &mut annotations);
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    annotations
}

fn collect_callable_members_from_type(
    node: Node<'_>,
    object_name: &str,
    text: &str,
    annotations: &mut Vec<CallableMemberTypeAnnotation>,
) {
    match node.kind() {
        "intersection_type" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_callable_members_from_type(child, object_name, text, annotations);
            }
        }
        "object_type" => {
            let mut cursor = node.walk();
            for member in node.named_children(&mut cursor) {
                if member.kind() != "property_signature" {
                    continue;
                }
                let Some(name) = member.child_by_field_name("name") else {
                    continue;
                };
                let Some(annotation) = member.child_by_field_name("type") else {
                    continue;
                };
                let Some(type_name) = annotation
                    .named_child(0)
                    .filter(|node| node.kind() == "type_identifier")
                    .and_then(|node| node_text(text, node))
                else {
                    continue;
                };
                let Some(member_name) = node_text(text, name) else {
                    continue;
                };
                annotations.push(CallableMemberTypeAnnotation {
                    object_name: object_name.to_owned(),
                    member_name: member_name.to_owned(),
                    type_name: type_name.to_owned(),
                });
            }
        }
        _ => {}
    }
}

fn decorator_option_shape(
    node: Node<'_>,
    text: &str,
    locations: &Locations,
) -> Option<DecoratorOptionType> {
    let mut members = Vec::new();
    let mut references = Vec::new();
    collect_decorator_option_shape(node, text, locations, &mut members, &mut references)?;
    Some(DecoratorOptionType {
        members,
        references,
    })
}

fn collect_decorator_option_shape(
    node: Node<'_>,
    text: &str,
    locations: &Locations,
    members: &mut Vec<TypeMember>,
    references: &mut Vec<String>,
) -> Option<()> {
    match node.kind() {
        "object_type" => members.extend(type_members_from_body(node, text, locations)?),
        "intersection_type" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_decorator_option_shape(child, text, locations, members, references)?;
            }
        }
        "type_identifier" => references.push(node_text(text, node)?.to_owned()),
        "generic_type" => {
            let base = node.named_child(0)?;
            if node_text(text, base) != Some("Partial") {
                return None;
            }
            let arguments = node.named_child(1)?;
            let mut cursor = arguments.walk();
            let arguments = arguments.named_children(&mut cursor).collect::<Vec<_>>();
            let [argument] = arguments.as_slice() else {
                return None;
            };
            collect_decorator_option_shape(*argument, text, locations, members, references)?;
        }
        _ => return None,
    }
    Some(())
}

pub(super) fn member_return_type(member: Node<'_>, text: &str) -> Option<String> {
    let annotation = member.child_by_field_name("return_type");
    if annotation.is_none() && member.kind() == "method_definition" && method_returns_this(member) {
        return Some("this".to_owned());
    }
    let annotation = annotation?;
    if annotation
        .named_child(0)
        .is_some_and(|node| node.kind() == "this_type")
    {
        return Some("this".to_owned());
    }
    let mut names = Vec::new();
    let mut stack = vec![annotation];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "identifier" | "type_identifier") {
            names.push(node_text(text, node)?.to_owned());
        }
        let mut cursor = node.walk();
        let children = node.named_children(&mut cursor).collect::<Vec<_>>();
        stack.extend(children.into_iter().rev());
    }
    matches!(names.as_slice(), [_] | [_, _]).then(|| names.pop().unwrap())
}

fn method_returns_this(member: Node<'_>) -> bool {
    let Some(body) = member.child_by_field_name("body") else {
        return false;
    };
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        if node.kind() == "return_statement"
            && node
                .named_child(0)
                .is_some_and(|value| value.kind() == "this")
        {
            return true;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    false
}

pub(super) fn member_callback_parameter_type(member: Node<'_>, text: &str) -> Option<String> {
    let mut candidates = Vec::new();
    let mut stack = vec![member];
    while let Some(node) = stack.pop() {
        if node.kind() == "function_type"
            && let Some(parameters) = node.child_by_field_name("parameters")
            && let Some(parameter) = sole_type_parameter(parameters)
            && let Some(annotation) = parameter.child_by_field_name("type")
            && let Some(type_name) = first_type_identifier(annotation, text)
        {
            candidates.push(type_name);
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    (candidates.len() == 1).then(|| candidates.remove(0))
}

pub(super) fn sole_type_parameter(parameters: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = parameters.walk();
    let parameters = parameters.named_children(&mut cursor).collect::<Vec<_>>();
    let [parameter] = parameters.as_slice() else {
        return None;
    };
    matches!(
        parameter.kind(),
        "required_parameter" | "optional_parameter"
    )
    .then_some(*parameter)
}

pub(super) fn first_type_identifier(node: Node<'_>, text: &str) -> Option<String> {
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if node.kind() == "type_identifier" {
            return node_text(text, node).map(ToOwned::to_owned);
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    None
}

pub(super) fn member_type_reference(member: Node<'_>, text: &str) -> Option<(String, String)> {
    let annotation = member
        .child_by_field_name("type")
        .or_else(|| member.child_by_field_name("return_type"))?;
    let mut names = Vec::new();
    let mut stack = vec![annotation];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "identifier" | "type_identifier") {
            names.push(node_text(text, node)?.to_owned());
        }
        let mut cursor = node.walk();
        let children = node.named_children(&mut cursor).collect::<Vec<_>>();
        stack.extend(children.into_iter().rev());
    }
    let [namespace, name] = names.as_slice() else {
        return None;
    };
    Some((namespace.clone(), name.clone()))
}

pub(super) fn collect_type_member_uses(
    text_root: Node<'_>,
    text: &str,
    locations: &Locations,
    imports: &[Import],
    type_declarations: &[TypeDeclaration],
    symbols: &[SymbolOccurrences],
    import_symbols: &[SymbolOccurrences],
) -> Vec<TypeMemberUse> {
    let mut bindings = Vec::new();
    collect_explicit_type_bindings(
        text_root,
        text,
        locations,
        imports,
        type_declarations,
        symbols,
        import_symbols,
        &mut bindings,
    );
    let mut uses = Vec::new();
    let mut stack = vec![text_root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "member_expression" | "subscript_expression")
            && let (Some(object), Some(property)) = (
                node.child_by_field_name("object"),
                node.child_by_field_name(if node.kind() == "member_expression" {
                    "property"
                } else {
                    "index"
                }),
            )
            && let Some(member_name) = direct_member_name(text, property)
        {
            // `this.field` belongs to the nearest enclosing local class. It is
            // not an explicitly typed receiver, so resolve it before type bindings.
            if object.kind() == "this"
                && let Some(class) = enclosing_class(node)
                && let Some(name) = class.child_by_field_name("name")
                && let Some(name) = node_text(text, name)
                && let Some(declaration) = type_declarations.iter().find(|declaration| {
                    declaration.kind == DeclarationKind::Class && declaration.name == name
                })
            {
                uses.push(TypeMemberUse {
                    target: TypeMemberTarget::Named(declaration.range),
                    member_name,
                    member_range: locations.range(property),
                });
                continue;
            }
            let is_this_field = object.kind() == "member_expression"
                && object
                    .child_by_field_name("object")
                    .is_some_and(|item| item.kind() == "this");
            let receiver = if object.kind() == "identifier" {
                node_text(text, object)
            } else if is_this_field
                && let Some(field) = object.child_by_field_name("property")
                && field.kind() == "property_identifier"
            {
                node_text(text, field)
            } else {
                None
            };
            if let Some(receiver) = receiver {
                let object_range = locations.range(object);
                // A PascalCase receiver can name a local class directly,
                // without requiring a variable type annotation.
                if let Some(declaration) = type_declarations.iter().find(|declaration| {
                    declaration.name == receiver && declaration.kind == DeclarationKind::Class
                }) {
                    uses.push(TypeMemberUse {
                        target: TypeMemberTarget::Named(declaration.range),
                        member_name,
                        member_range: locations.range(property),
                    });
                    continue;
                }
                let declaration = symbols
                    .iter()
                    .chain(import_symbols)
                    .find(|symbol| {
                        symbol.name == receiver && symbol.occurrences.contains(&object_range)
                    })
                    .map(|symbol| symbol.declaration);
                let imported_binding = imports.iter().flat_map(|import| {
                    import.bindings.iter().filter(move |binding| {
                        binding.local_name == receiver
                            && !binding.type_only
                            && matches!(binding.kind, ImportKind::Named | ImportKind::Default)
                    })
                });
                // Named and default value imports can be classes. Record a
                // static member use so the host can resolve its declaration.
                if member_access_is_called(node)
                    && receiver.chars().next().is_some_and(char::is_uppercase)
                    && let Some(binding) = imported_binding.clone().next()
                {
                    uses.push(TypeMemberUse {
                        target: TypeMemberTarget::ImportedClass(binding.range),
                        member_name,
                        member_range: locations.range(property),
                    });
                    continue;
                }
                if member_access_is_called(node)
                    && let Some(binding) = imported_binding
                        .filter(|binding| binding.kind == ImportKind::Default)
                        .next()
                {
                    uses.push(TypeMemberUse {
                        target: TypeMemberTarget::ImportedFactory(binding.range),
                        member_name,
                        member_range: locations.range(property),
                    });
                    continue;
                }
                let lexical_targets = bindings
                    .iter()
                    .filter(|(name, _, scope, _)| {
                        *name == receiver && contains(*scope, object_range.start)
                    })
                    .map(|(_, _, scope, target)| (*scope, target))
                    .collect::<Vec<_>>();
                let lexical_targets = lexical_targets
                    .iter()
                    .map(|(scope, _)| scope)
                    .max_by_key(|scope| (scope.start.line, scope.start.col))
                    .map(|scope| {
                        lexical_targets
                            .iter()
                            .filter(|(candidate, _)| candidate == scope)
                            .map(|(_, target)| *target)
                            .collect::<Vec<_>>()
                    });
                let targets = lexical_targets.or_else(|| {
                    declaration
                        .map(|declaration| {
                            bindings
                                .iter()
                                .filter(|(_, range, _, _)| *range == declaration)
                                .map(|(_, _, _, target)| target)
                                .collect::<Vec<_>>()
                        })
                        .or_else(|| {
                            is_this_field
                                .then(|| {
                                    let targets = bindings
                                        .iter()
                                        .filter(|(name, _, scope, _)| {
                                            *name == receiver
                                                && contains(*scope, object_range.start)
                                        })
                                        .map(|(_, _, _, target)| target)
                                        .collect::<Vec<_>>();
                                    (targets.len() == 1).then_some(targets)
                                })
                                .flatten()
                        })
                });
                if let Some(targets) = targets
                    && let Some(target) = targets.first()
                    && targets.iter().all(|candidate| candidate == target)
                    && (node.kind() == "member_expression"
                        || matches!(target, TypeMemberTarget::Inline(_)))
                {
                    uses.push(TypeMemberUse {
                        target: (*target).clone(),
                        member_name,
                        member_range: locations.range(property),
                    });
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    uses
}

fn enclosing_class(mut node: Node<'_>) -> Option<Node<'_>> {
    while let Some(parent) = node.parent() {
        if matches!(parent.kind(), "class_declaration" | "class") {
            return Some(parent);
        }
        node = parent;
    }
    None
}

fn member_access_is_called(node: Node<'_>) -> bool {
    node.parent()
        .is_some_and(|parent| parent.kind() == "call_expression")
        || node
            .parent()
            .and_then(|parent| parent.parent())
            .is_some_and(|parent| parent.kind() == "call_expression")
}

pub(super) fn collect_default_export_factory_import(
    root: Node<'_>,
    text: &str,
    imports: &[Import],
) -> Option<Utf16Range> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "export_statement"
            && node_text(text, node)
                .is_some_and(|source| source.trim_start().starts_with("export default"))
        {
            let mut descendants = vec![node];
            while let Some(candidate) = descendants.pop() {
                if candidate.kind() == "member_expression"
                    && let (Some(object), Some(property)) = (
                        candidate.child_by_field_name("object"),
                        candidate.child_by_field_name("property"),
                    )
                    && object.kind() == "identifier"
                    && property.kind() == "property_identifier"
                    && node_text(text, property) == Some("create")
                    && let Some(name) = node_text(text, object)
                    && let Some(binding) = imports
                        .iter()
                        .flat_map(|import| import.bindings.iter())
                        .find(|binding| {
                            binding.local_name == name
                                && !binding.type_only
                                && matches!(binding.kind, ImportKind::Named | ImportKind::Default)
                        })
                {
                    return Some(binding.range);
                }
                let mut cursor = candidate.walk();
                descendants.extend(candidate.named_children(&mut cursor));
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(super) fn collect_explicit_type_bindings<'a>(
    node: Node<'a>,
    text: &'a str,
    locations: &Locations,
    imports: &[Import],
    type_declarations: &[TypeDeclaration],
    symbols: &[SymbolOccurrences],
    import_symbols: &[SymbolOccurrences],
    bindings: &mut Vec<(&'a str, Utf16Range, Utf16Range, TypeMemberTarget)>,
) {
    if matches!(
        node.kind(),
        "variable_declarator"
            | "required_parameter"
            | "optional_parameter"
            | "property_definition"
            | "field_definition"
            | "public_field_definition"
    ) && let Some(name) = node
        .child_by_field_name("name")
        .or_else(|| node.child_by_field_name("pattern"))
        && (name.kind() == "identifier" || name.kind() == "property_identifier")
        && let Some(target) = direct_type_annotation(
            node,
            text,
            locations,
            imports,
            type_declarations,
            symbols,
            import_symbols,
        )
    {
        let range = locations.range(name);
        if let Some(name) = node_text(text, name) {
            bindings.push((name, range, binding_scope(node, locations), target));
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_explicit_type_bindings(
            child,
            text,
            locations,
            imports,
            type_declarations,
            symbols,
            import_symbols,
            bindings,
        );
    }
}

pub(super) fn binding_scope(node: Node<'_>, locations: &Locations) -> Utf16Range {
    let mut current = Some(node);
    while let Some(item) = current {
        // A parameter is visible only inside its own function.
        if matches!(
            item.kind(),
            "statement_block"
                | "class_body"
                | "program"
                | "function_declaration"
                | "function_expression"
                | "generator_function"
                | "generator_function_declaration"
                | "arrow_function"
                | "method_definition"
        ) {
            return locations.range(item);
        }
        current = item.parent();
    }
    locations.range(node)
}

pub(super) fn direct_type_annotation(
    node: Node<'_>,
    text: &str,
    locations: &Locations,
    imports: &[Import],
    type_declarations: &[TypeDeclaration],
    symbols: &[SymbolOccurrences],
    import_symbols: &[SymbolOccurrences],
) -> Option<TypeMemberTarget> {
    let annotation = node.child_by_field_name("type")?;
    if annotation.kind() != "type_annotation" {
        return None;
    }
    let ty = annotation.named_child(0)?;
    type_target(
        ty,
        text,
        locations,
        imports,
        type_declarations,
        symbols,
        import_symbols,
    )
}

pub(super) fn type_target(
    ty: Node<'_>,
    text: &str,
    locations: &Locations,
    imports: &[Import],
    type_declarations: &[TypeDeclaration],
    symbols: &[SymbolOccurrences],
    import_symbols: &[SymbolOccurrences],
) -> Option<TypeMemberTarget> {
    match ty.kind() {
        "object_type" => type_members_from_body(ty, text, locations).map(TypeMemberTarget::Inline),
        "type_identifier" => {
            let range = locations.range(ty);
            if let Some(symbol) = symbols
                .iter()
                .find(|symbol| symbol.occurrences.contains(&range))
            {
                return type_declarations
                    .iter()
                    .any(|item| item.range == symbol.declaration)
                    .then_some(TypeMemberTarget::Named(symbol.declaration));
            }
            let symbol = import_symbols
                .iter()
                .find(|symbol| symbol.occurrences.contains(&range))?;
            let binding = imports
                .iter()
                .flat_map(|import| import.bindings.iter())
                .find(|binding| binding.range == symbol.declaration && binding.type_only)?;
            Some(TypeMemberTarget::Imported(binding.range))
        }
        "union_type" => {
            let mut cursor = ty.walk();
            let arms = ty
                .named_children(&mut cursor)
                .map(|arm| {
                    type_target(
                        arm,
                        text,
                        locations,
                        imports,
                        type_declarations,
                        symbols,
                        import_symbols,
                    )
                })
                .collect::<Option<Vec<_>>>()?;
            (!arms.is_empty()).then_some(TypeMemberTarget::Union(arms))
        }
        _ => None,
    }
}

pub(super) fn direct_member_name(text: &str, property: Node<'_>) -> Option<String> {
    match property.kind() {
        "property_identifier" => node_text(text, property).map(str::to_owned),
        "string" => node_text(text, property).map(|name| name.trim_matches(['\'', '"']).to_owned()),
        _ => None,
    }
}
