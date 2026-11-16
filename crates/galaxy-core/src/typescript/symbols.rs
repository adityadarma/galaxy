//! Lexical scopes, declarations and same-file symbol occurrences.

use super::*;

pub(super) fn resolve_symbols(
    text: &str,
    root: Node<'_>,
    locations: &Locations,
    include_imports: bool,
) -> Vec<SymbolOccurrences> {
    let mut scopes = vec![Scope {
        parent: None,
        range: locations.range(root),
    }];
    let mut declarations = Vec::new();
    collect_scoped_declarations(
        root,
        0,
        text,
        locations,
        &mut scopes,
        &mut declarations,
        include_imports,
    );
    let mut occurrences = declarations
        .iter()
        .map(|item| vec![item.range])
        .collect::<Vec<_>>();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "identifier" | "type_identifier") {
            let range = locations.range(node);
            if !declarations
                .iter()
                .any(|declaration| declaration.range == range)
                && is_local_reference(node)
            {
                let name = node_text(text, node).unwrap_or_default();
                let scope = innermost_scope(range.start, &scopes);
                if let Some(index) = resolve_declaration(name, scope, &declarations, &scopes) {
                    occurrences[index].push(range);
                }
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    declarations
        .into_iter()
        .zip(occurrences)
        .map(|(declaration, mut occurrences)| {
            occurrences.sort_by_key(|range| (range.start.line, range.start.col));
            occurrences.dedup();
            SymbolOccurrences {
                name: declaration.name,
                kind: declaration.kind,
                declaration: declaration.range,
                occurrences,
            }
        })
        .collect()
}

/// Identifier and type references whose name this file never declares or
/// imports, in any scope. Deliberately loose about scope (a name declared
/// anywhere counts) so callers can flag missing imports without false
/// positives from scoping gaps; callers still decide which names matter.
pub(super) fn unresolved_references(
    text: &str,
    root: Node<'_>,
    locations: &Locations,
) -> Vec<(String, Utf16Range)> {
    let mut scopes = vec![Scope {
        parent: None,
        range: locations.range(root),
    }];
    let mut declarations = Vec::new();
    collect_scoped_declarations(
        root,
        0,
        text,
        locations,
        &mut scopes,
        &mut declarations,
        true,
    );
    let mut declared = declarations
        .iter()
        .map(|declaration| declaration.name.as_str())
        .collect::<std::collections::HashSet<_>>();
    let declared_ranges = declarations
        .iter()
        .map(|declaration| {
            let range = declaration.range;
            (
                range.start.line,
                range.start.col,
                range.end.line,
                range.end.col,
            )
        })
        .collect::<std::collections::HashSet<_>>();
    let mut references = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        // `Operations & Automation` in JSX: the words are text, not names.
        if is_bare_jsx_ampersand(text, node) {
            continue;
        }
        match node.kind() {
            // `<T>` and `infer T` introduce names the scope walk does not track.
            "type_parameter" | "infer_type" => {
                let mut cursor = node.walk();
                if let Some(name) = node
                    .child_by_field_name("name")
                    .or_else(|| node.named_children(&mut cursor).next())
                    .and_then(|name| node_text(text, name))
                {
                    declared.insert(name);
                }
            }
            // `catch (error)`, class expressions and named function
            // expressions bind names outside `collect_scoped_declarations`.
            "catch_clause" | "class" | "function_expression" => {
                if node.kind() == "catch_clause"
                    && let Some(parameter) = node.child_by_field_name("parameter")
                {
                    declare_pattern_names(parameter, text, &mut declared);
                } else if let Some(name) = node
                    .child_by_field_name("parameter")
                    .or_else(|| node.child_by_field_name("name"))
                    .and_then(|name| node_text(text, name))
                {
                    declared.insert(name);
                }
            }
            // Parameters of function types, signatures and named tuple
            // members, `[key: string]` and `for (const x of xs)` bind names
            // the scope walk does not track.
            "required_parameter" | "optional_parameter" => {
                for field in ["pattern", "name"] {
                    if let Some(pattern) = node.child_by_field_name(field) {
                        declare_pattern_names(pattern, text, &mut declared);
                    }
                }
            }
            "index_signature" | "function_signature" => {
                if let Some(name) = node
                    .child_by_field_name("name")
                    .and_then(|name| node_text(text, name))
                {
                    declared.insert(name);
                }
            }
            "for_in_statement" => {
                if let Some(left) = node.child_by_field_name("left") {
                    declare_pattern_names(left, text, &mut declared);
                }
            }
            "enum_body" => {
                let mut cursor = node.walk();
                for member in node.named_children(&mut cursor) {
                    let name = if member.kind() == "enum_assignment" {
                        member.child_by_field_name("name")
                    } else {
                        Some(member)
                    };
                    if let Some(name) = name.and_then(|name| node_text(text, name)) {
                        declared.insert(name.trim_matches(['\'', '"']));
                    }
                }
            }
            // `import a = require('x')` and `import B = A.C`.
            "import_require_clause" | "import_alias" => {
                if let Some(name) = node
                    .named_child(0)
                    .filter(|name| name.kind() == "identifier")
                    .and_then(|name| node_text(text, name))
                {
                    declared.insert(name);
                }
            }
            // `namespace A.B {}` declares `A`.
            "internal_module" | "module" => {
                let mut name = node.child_by_field_name("name");
                while let Some(current) = name.filter(|name| name.kind() == "nested_identifier") {
                    name = current.child_by_field_name("object");
                }
                if let Some(name) = name
                    .filter(|name| name.kind() == "identifier")
                    .and_then(|name| node_text(text, name))
                {
                    declared.insert(name);
                }
            }
            "mapped_type_clause" => {
                if let Some(name) = node
                    .child_by_field_name("name")
                    .and_then(|name| node_text(text, name))
                {
                    declared.insert(name);
                }
            }
            // Ambient blocks (`declare global`, `declare module 'x'`) talk
            // about names owned elsewhere.
            "ambient_declaration" => {
                // `declare function f(): void` names `f` for the whole file.
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if child.kind() == "function_signature"
                        && let Some(name) = child
                            .child_by_field_name("name")
                            .and_then(|name| node_text(text, name))
                    {
                        declared.insert(name);
                    }
                }
                continue;
            }
            "identifier" | "type_identifier" => {
                let range = locations.range(node);
                // `Ns.Type`: only `Ns` is looked up in this file.
                let qualified_member = node.parent().is_some_and(|parent| {
                    parent.kind() == "nested_type_identifier"
                        && parent
                            .child_by_field_name("name")
                            .is_some_and(|name| name.id() == node.id())
                });
                // Only the head of `A.B` is looked up; `<div>` is an intrinsic
                // element, not a reference.
                let trailing_segment = node.parent().is_some_and(|parent| {
                    parent.kind() == "nested_identifier"
                        && parent
                            .child_by_field_name("object")
                            .is_none_or(|object| object.id() != node.id())
                });
                let intrinsic_element = node.parent().is_some_and(|parent| {
                    matches!(
                        parent.kind(),
                        "jsx_opening_element" | "jsx_closing_element" | "jsx_self_closing_element"
                    )
                }) && node_text(text, node)
                    .is_some_and(|name| name.starts_with(|ch: char| ch.is_ascii_lowercase()));
                if !declared_ranges.contains(&(
                    range.start.line,
                    range.start.col,
                    range.end.line,
                    range.end.col,
                )) && !qualified_member
                    && !trailing_segment
                    && !intrinsic_element
                    && is_local_reference(node)
                    && let Some(name) = node_text(text, node)
                {
                    references.push((name, range));
                }
                continue;
            }
            _ => {}
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    let mut unresolved = references
        .into_iter()
        .filter(|(name, _)| !declared.contains(name))
        .map(|(name, range)| (name.to_owned(), range))
        .collect::<Vec<_>>();
    unresolved.sort_by_key(|(_, range)| (range.start.line, range.start.col));
    unresolved
}

/// Every name a binding pattern introduces. Loose on purpose: default values
/// inside the pattern count as names too, which can only hide a report.
fn declare_pattern_names<'a>(
    node: Node<'_>,
    text: &'a str,
    declared: &mut std::collections::HashSet<&'a str>,
) {
    if matches!(
        node.kind(),
        "identifier" | "shorthand_property_identifier_pattern"
    ) {
        if let Some(name) = node_text(text, node) {
            declared.insert(name);
        }
        return;
    }
    if node.kind() == "type_annotation" {
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        declare_pattern_names(child, text, declared);
    }
}

pub(super) fn collect_scoped_declarations(
    node: Node<'_>,
    scope: usize,
    text: &str,
    locations: &Locations,
    scopes: &mut Vec<Scope>,
    declarations: &mut Vec<ResolvedDeclaration>,
    include_imports: bool,
) {
    if include_imports && node.kind() == "import_statement" {
        let statement_type_only = has_keyword(node, text, "type");
        let mut bindings = Vec::new();
        collect_import_bindings(text, node, statement_type_only, locations, &mut bindings);
        for binding in bindings {
            declarations.push(ResolvedDeclaration {
                name: binding.local_name,
                kind: DeclarationKind::Variable,
                range: binding.range,
                scope,
            });
        }
    }
    let is_function = matches!(
        node.kind(),
        "function_declaration"
            | "generator_function_declaration"
            | "arrow_function"
            | "function_expression"
            | "method_definition"
    );
    if matches!(
        node.kind(),
        "function_declaration"
            | "generator_function_declaration"
            | "class_declaration"
            | "abstract_class_declaration"
            | "interface_declaration"
            | "type_alias_declaration"
            | "enum_declaration"
            | "internal_module"
            | "module"
            | "namespace_declaration"
    ) && let Some(name) = node.child_by_field_name("name")
    {
        declare(
            name,
            DeclarationKind::Function,
            scope,
            text,
            locations,
            declarations,
        );
        if node.kind() != "function_declaration" && node.kind() != "generator_function_declaration"
        {
            declarations.last_mut().unwrap().kind = declaration_kind(node.kind()).unwrap();
        }
    }
    let scope = if is_function || node.kind() == "statement_block" {
        let index = scopes.len();
        scopes.push(Scope {
            parent: Some(scope),
            range: locations.range(node),
        });
        if is_function {
            if let Some(parameters) = node.child_by_field_name("parameters") {
                collect_parameter_bindings(parameters, index, text, locations, declarations);
            } else if let Some(parameter) = node.child_by_field_name("parameter") {
                collect_pattern_declarations(
                    parameter,
                    DeclarationKind::Parameter,
                    index,
                    text,
                    locations,
                    declarations,
                );
            }
        }
        index
    } else {
        scope
    };
    if matches!(node.kind(), "lexical_declaration" | "variable_declaration") {
        let mut cursor = node.walk();
        for declarator in node
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "variable_declarator")
        {
            if let Some(name) = declarator.child_by_field_name("name") {
                collect_pattern_declarations(
                    name,
                    DeclarationKind::Variable,
                    scope,
                    text,
                    locations,
                    declarations,
                );
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_scoped_declarations(
            child,
            scope,
            text,
            locations,
            scopes,
            declarations,
            include_imports,
        );
    }
}

pub(super) fn declaration_kind(kind: &str) -> Option<DeclarationKind> {
    Some(match kind {
        "class_declaration" | "abstract_class_declaration" | "class" => DeclarationKind::Class,
        "interface_declaration" => DeclarationKind::Interface,
        "type_alias_declaration" => DeclarationKind::Type,
        "enum_declaration" => DeclarationKind::Enum,
        "internal_module" | "module" | "namespace_declaration" => DeclarationKind::Namespace,
        _ => return None,
    })
}

pub(super) fn collect_parameter_bindings(
    node: Node<'_>,
    scope: usize,
    text: &str,
    locations: &Locations,
    declarations: &mut Vec<ResolvedDeclaration>,
) {
    let mut cursor = node.walk();
    for parameter in node.named_children(&mut cursor) {
        if let Some(pattern) = parameter.child_by_field_name("pattern") {
            collect_pattern_declarations(
                pattern,
                DeclarationKind::Parameter,
                scope,
                text,
                locations,
                declarations,
            );
        } else if let Some(name) = parameter.child_by_field_name("name") {
            collect_pattern_declarations(
                name,
                DeclarationKind::Parameter,
                scope,
                text,
                locations,
                declarations,
            );
        } else if parameter.kind() == "identifier" {
            declare(
                parameter,
                DeclarationKind::Parameter,
                scope,
                text,
                locations,
                declarations,
            );
        }
    }
}

pub(super) fn collect_pattern_declarations(
    node: Node<'_>,
    kind: DeclarationKind,
    scope: usize,
    text: &str,
    locations: &Locations,
    declarations: &mut Vec<ResolvedDeclaration>,
) {
    match node.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            declare(node, kind, scope, text, locations, declarations)
        }
        "pair_pattern" => {
            if let Some(value) = node.child_by_field_name("value") {
                collect_pattern_declarations(value, kind, scope, text, locations, declarations);
            }
        }
        "assignment_pattern" | "object_assignment_pattern" => {
            if let Some(left) = node.child_by_field_name("left") {
                collect_pattern_declarations(left, kind, scope, text, locations, declarations);
            }
        }
        "rest_pattern" => {
            if let Some(argument) = node
                .child_by_field_name("argument")
                .or_else(|| node.named_child(0))
            {
                collect_pattern_declarations(argument, kind, scope, text, locations, declarations);
            }
        }
        "array_pattern" | "object_pattern" | "required_parameter" | "optional_parameter" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_pattern_declarations(child, kind, scope, text, locations, declarations);
            }
        }
        _ => {}
    }
}

pub(super) fn declare(
    node: Node<'_>,
    kind: DeclarationKind,
    scope: usize,
    text: &str,
    locations: &Locations,
    declarations: &mut Vec<ResolvedDeclaration>,
) {
    if let Some(name) = node_text(text, node) {
        declarations.push(ResolvedDeclaration {
            name: name.to_owned(),
            kind,
            range: locations.range(node),
            scope,
        });
    }
}

pub(super) fn innermost_scope(position: Utf16Position, scopes: &[Scope]) -> usize {
    scopes
        .iter()
        .enumerate()
        .filter(|(_, scope)| contains(scope.range, position))
        .max_by_key(|(_, scope)| (scope.range.start.line, scope.range.start.col))
        .map(|(index, _)| index)
        .unwrap_or(0)
}

pub(super) fn resolve_declaration(
    name: &str,
    mut scope: usize,
    declarations: &[ResolvedDeclaration],
    scopes: &[Scope],
) -> Option<usize> {
    loop {
        if let Some((index, _)) = declarations
            .iter()
            .enumerate()
            .rev()
            .find(|(_, declaration)| declaration.scope == scope && declaration.name == name)
        {
            return Some(index);
        }
        scope = scopes[scope].parent?;
    }
}

pub(super) fn is_local_reference(node: Node<'_>) -> bool {
    let mut current = Some(node);
    while let Some(item) = current {
        if matches!(item.kind(), "import_statement" | "labeled_statement")
            || item.kind() == "export_statement"
                && !item.named_children(&mut item.walk()).any(|child| {
                    matches!(
                        child.kind(),
                        "function_declaration"
                            | "generator_function_declaration"
                            | "class_declaration"
                            | "abstract_class_declaration"
                            | "class"
                            | "interface_declaration"
                            | "type_alias_declaration"
                            | "enum_declaration"
                            | "lexical_declaration"
                            | "variable_declaration"
                    )
                })
        {
            return false;
        }
        if item.kind() == "member_expression"
            && item
                .child_by_field_name("property")
                .is_some_and(|property| property.id() == node.id())
        {
            return false;
        }
        if item.kind() == "pair"
            && item
                .child_by_field_name("key")
                .is_some_and(|key| key.id() == node.id())
        {
            return false;
        }
        current = item.parent();
    }
    true
}

pub(super) fn declarations_from_node(
    node: Node<'_>,
    exported: bool,
    default: bool,
    text: &str,
    locations: &Locations,
) -> Vec<Declaration> {
    if node.kind() == "ambient_declaration" {
        let mut cursor = node.walk();
        return node
            .named_children(&mut cursor)
            .flat_map(|child| {
                let name_range = child
                    .child_by_field_name("name")
                    .map(|name| locations.range(name));
                declarations_from_node(child, exported, default, text, locations)
                    .into_iter()
                    .map(move |mut declaration| {
                        if let Some(range) = name_range {
                            declaration.range = range;
                        }
                        declaration
                    })
            })
            .collect();
    }
    let kind = match node.kind() {
        "function_declaration" | "function_signature" | "generator_function_declaration" => {
            DeclarationKind::Function
        }
        "class_declaration" | "abstract_class_declaration" | "class" => DeclarationKind::Class,
        "interface_declaration" => DeclarationKind::Interface,
        "type_alias_declaration" => DeclarationKind::Type,
        "enum_declaration" => DeclarationKind::Enum,
        "internal_module" | "module" | "namespace_declaration" => DeclarationKind::Namespace,
        "lexical_declaration" | "variable_declaration" => {
            return variable_declarations(node, exported, default, text, locations);
        }
        _ => return Vec::new(),
    };
    node.child_by_field_name("name")
        .and_then(|name| node_text(text, name).map(str::to_owned))
        .map(|name| Declaration {
            name,
            kind,
            range: locations.range(node),
            exported,
            default,
        })
        .into_iter()
        .collect()
}

pub(super) fn variable_declarations(
    node: Node<'_>,
    exported: bool,
    default: bool,
    text: &str,
    locations: &Locations,
) -> Vec<Declaration> {
    let mut declarations = Vec::new();
    let mut cursor = node.walk();
    for declarator in node.named_children(&mut cursor) {
        if declarator.kind() != "variable_declarator" {
            continue;
        }
        let Some(name) = declarator.child_by_field_name("name") else {
            continue;
        };
        let mut bindings = Vec::new();
        collect_bindings(name, text, &mut bindings);
        declarations.extend(bindings.into_iter().map(|name| Declaration {
            name,
            kind: DeclarationKind::Variable,
            range: locations.range(declarator),
            exported,
            default,
        }));
    }
    declarations
}

pub(super) fn collect_bindings(node: Node<'_>, text: &str, bindings: &mut Vec<String>) {
    match node.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            if let Some(name) = node_text(text, node) {
                bindings.push(name.to_owned());
            }
        }
        "pair_pattern" => {
            if let Some(value) = node.child_by_field_name("value") {
                collect_bindings(value, text, bindings);
            }
        }
        "assignment_pattern" => {
            if let Some(left) = node.child_by_field_name("left") {
                collect_bindings(left, text, bindings);
            }
        }
        "rest_pattern" => {
            if let Some(argument) = node.child_by_field_name("argument") {
                collect_bindings(argument, text, bindings);
            }
        }
        "array_pattern" | "object_pattern" | "parenthesized_expression" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_bindings(child, text, bindings);
            }
        }
        _ => {}
    }
}
