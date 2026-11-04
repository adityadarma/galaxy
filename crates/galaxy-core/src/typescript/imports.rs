//! Static imports, exports, dynamic imports and namespace member uses.

use super::*;

/// Type identifiers in declaration fields are not consistently included in
/// Tree-sitter's lexical reference set. Index only an unshadowed, unique
/// named/default import in a type position so Definition remains conservative.
/// Declaration files may use a regular import for interfaces.
pub(super) fn collect_type_import_references(
    root: Node<'_>,
    text: &str,
    locations: &Locations,
    imports: &[Import],
    symbols: &[SymbolOccurrences],
) -> Vec<(Utf16Range, Utf16Range)> {
    let mut references = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if (node.kind() == "type_identifier"
            || (node.kind() == "identifier"
                && node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "extends_clause")))
            && node
                .parent()
                .is_none_or(|parent| parent.kind() != "import_statement")
            && let Some(name) = node_text(text, node)
            && !symbols.iter().any(|symbol| symbol.name == name)
        {
            let bindings = imports
                .iter()
                .flat_map(|import| import.bindings.iter())
                .filter(|binding| {
                    matches!(binding.kind, ImportKind::Named | ImportKind::Default)
                        && binding.local_name == name
                })
                .collect::<Vec<_>>();
            if let [binding] = bindings.as_slice() {
                references.push((locations.range(node), binding.range));
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    references
}

pub(super) fn collect_qualified_type_imports(
    root: Node<'_>,
    text: &str,
    locations: &Locations,
    imports: &[Import],
) -> Vec<QualifiedTypeImport> {
    let mut references = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "type_annotation" {
            let mut identifiers = Vec::new();
            let mut descendants = vec![node];
            while let Some(descendant) = descendants.pop() {
                if matches!(descendant.kind(), "identifier" | "type_identifier") {
                    identifiers.push(descendant);
                }
                let mut cursor = descendant.walk();
                let children = descendant.named_children(&mut cursor).collect::<Vec<_>>();
                descendants.extend(children.into_iter().rev());
            }
            if let [namespace, member] = identifiers.as_slice()
                && let Some(namespace_name) = node_text(text, *namespace)
                && let Some(member_name) = node_text(text, *member)
            {
                let bindings = imports
                    .iter()
                    .flat_map(|import| import.bindings.iter())
                    .filter(|binding| {
                        binding.type_only
                            && matches!(binding.kind, ImportKind::Named | ImportKind::Default)
                            && binding.local_name == namespace_name
                    })
                    .collect::<Vec<_>>();
                if let [binding] = bindings.as_slice() {
                    references.push(QualifiedTypeImport {
                        namespace_range: locations.range(*namespace),
                        member_range: locations.range(*member),
                        binding_range: binding.range,
                        member_name: member_name.to_owned(),
                    });
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    references
}

pub(super) fn import_from_node(text: &str, node: Node<'_>, locations: &Locations) -> Import {
    let specifier = source_specifier(text, node).unwrap_or_default();
    let specifier_range = source_specifier_node(node)
        .map(|source| locations.range(source))
        .unwrap_or_default();
    let type_only = has_keyword(node, text, "type");
    let mut bindings = Vec::new();
    collect_import_bindings(text, node, type_only, locations, &mut bindings);
    Import {
        specifier,
        specifier_range,
        range: locations.range(node),
        bindings,
        type_only,
        syntax_valid: !node.has_error() && !node.is_error() && !node.is_missing(),
    }
}

pub(super) fn collect_dynamic_imports(
    text: &str,
    node: Node<'_>,
    locations: &Locations,
    imports: &mut Vec<Import>,
) {
    if node.kind() == "call_expression"
        && node
            .child_by_field_name("function")
            .is_some_and(|function| function.kind() == "import")
        && let Some(arguments) = node.child_by_field_name("arguments")
    {
        let mut cursor = arguments.walk();
        let mut arguments = arguments.named_children(&mut cursor);
        if let (Some(source), None) = (arguments.next(), arguments.next())
            && source.kind() == "string"
            && let Some(specifier) = node_text(text, source)
        {
            imports.push(Import {
                specifier: specifier.trim_matches(['\'', '"']).to_owned(),
                specifier_range: locations.range(source),
                range: locations.range(source),
                bindings: Vec::new(),
                type_only: false,
                syntax_valid: true,
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_dynamic_imports(text, child, locations, imports);
    }
}

pub(super) fn collect_import_bindings(
    text: &str,
    node: Node<'_>,
    statement_type_only: bool,
    locations: &Locations,
    bindings: &mut Vec<ImportBinding>,
) {
    match node.kind() {
        "import_specifier" => {
            bindings.push(import_binding(text, node, statement_type_only, locations))
        }
        "namespace_import" => {
            if let Some(name) = last_identifier_node(node, text) {
                bindings.push(ImportBinding {
                    local_name: node_text(text, name).unwrap_or_default().to_owned(),
                    imported_name: None,
                    kind: ImportKind::Namespace,
                    range: locations.range(name),
                    type_only: statement_type_only,
                });
            }
        }
        "import_clause" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "identifier" {
                    bindings.push(ImportBinding {
                        local_name: node_text(text, child).unwrap_or_default().to_owned(),
                        imported_name: Some("default".to_owned()),
                        kind: ImportKind::Default,
                        range: locations.range(child),
                        type_only: statement_type_only,
                    });
                } else {
                    collect_import_bindings(text, child, statement_type_only, locations, bindings);
                }
            }
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_import_bindings(text, child, statement_type_only, locations, bindings);
            }
        }
    }
}

pub(super) fn collect_decorator_option_uses(
    root: Node<'_>,
    text: &str,
    locations: &Locations,
    imports: &[Import],
    import_symbols: &[SymbolOccurrences],
) -> Vec<DecoratorOptionUse> {
    let mut uses = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "call_expression"
            && let call = node
            && let Some(function) = call.child_by_field_name("function")
            && let Some((name, member_name)) = decorator_callee(text, function)
            && let Some(binding_occurrence) = decorator_binding_occurrence(function)
            && let Some(arguments) = call.child_by_field_name("arguments")
            && let Some(object) = arguments
                .named_child(0)
                .filter(|node| node.kind() == "object")
            && arguments.named_child(1).is_none()
            && let Some(binding) = imported_value_binding(
                name,
                locations.range(binding_occurrence),
                imports,
                import_symbols,
            )
        {
            let mut cursor = object.walk();
            for property in object.named_children(&mut cursor) {
                let key = match property.kind() {
                    "pair" => property.child_by_field_name("key"),
                    "method_definition" => property.child_by_field_name("name"),
                    _ => None,
                };
                let Some(key) = key else {
                    continue;
                };
                if !matches!(key.kind(), "property_identifier" | "identifier") {
                    continue;
                }
                if let Some(key_name) = node_text(text, key) {
                    uses.push(DecoratorOptionUse {
                        binding_range: binding.range,
                        member_name: member_name.map(ToOwned::to_owned),
                        in_decorator: node
                            .parent()
                            .is_some_and(|parent| parent.kind() == "decorator"),
                        key_name: key_name.to_owned(),
                        key_range: locations.range(key),
                    });
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    uses
}

/// Accept only an imported identifier or one direct named member of one.
fn decorator_callee<'a>(text: &'a str, function: Node<'_>) -> Option<(&'a str, Option<&'a str>)> {
    if function.kind() == "identifier" {
        return Some((node_text(text, function)?, None));
    }
    if function.kind() != "member_expression" {
        return None;
    }
    let object = function.child_by_field_name("object")?;
    let property = function.child_by_field_name("property")?;
    (object.kind() == "identifier" && property.kind() == "property_identifier")
        .then(|| Some((node_text(text, object)?, Some(node_text(text, property)?))))?
}

fn decorator_binding_occurrence(function: Node<'_>) -> Option<Node<'_>> {
    match function.kind() {
        "identifier" => Some(function),
        "member_expression" => function.child_by_field_name("object"),
        _ => None,
    }
}

fn imported_value_binding<'a>(
    name: &str,
    occurrence: Utf16Range,
    imports: &'a [Import],
    import_symbols: &[SymbolOccurrences],
) -> Option<&'a ImportBinding> {
    let symbols = import_symbols
        .iter()
        .filter(|symbol| symbol.name == name && symbol.occurrences.contains(&occurrence))
        .collect::<Vec<_>>();
    let [symbol] = symbols.as_slice() else {
        return None;
    };
    let bindings = imports
        .iter()
        .flat_map(|import| import.bindings.iter())
        .filter(|binding| {
            binding.range == symbol.declaration
                && !binding.type_only
                && matches!(binding.kind, ImportKind::Named | ImportKind::Default)
        })
        .collect::<Vec<_>>();
    if bindings.len() == 1 {
        Some(bindings[0])
    } else {
        None
    }
}

pub(super) fn import_binding(
    text: &str,
    node: Node<'_>,
    statement_type_only: bool,
    locations: &Locations,
) -> ImportBinding {
    let identifiers = identifiers(node, text);
    let imported_name = identifiers.first().cloned();
    let local = last_identifier_node(node, text);
    let local_name = local
        .and_then(|node| node_text(text, node))
        .unwrap_or_default()
        .to_owned();
    ImportBinding {
        local_name,
        imported_name,
        kind: ImportKind::Named,
        range: local.map(|node| locations.range(node)).unwrap_or_default(),
        type_only: statement_type_only || has_keyword(node, text, "type"),
    }
}

pub(super) fn index_export(
    text: &str,
    node: Node<'_>,
    locations: &Locations,
    declarations: &mut Vec<Declaration>,
    exports: &mut Vec<Export>,
) {
    let default = has_keyword(node, text, "default");
    let type_only = has_keyword(node, text, "type");
    let specifier = source_specifier(text, node);
    let specifier_range = source_specifier_node(node).map(|source| locations.range(source));
    let mut declaration = None;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !declarations_from_node(child, true, default, text, locations).is_empty() {
            declaration = Some(child);
            break;
        }
    }
    if let Some(declaration) = declaration {
        let declared = declarations_from_node(declaration, true, default, text, locations);
        for item in &declared {
            exports.push(Export {
                name: Some(item.name.clone()),
                name_range: Some(item.range),
                exported_name: Some(if default {
                    "default".to_owned()
                } else {
                    item.name.clone()
                }),
                exported_name_range: Some(item.range),
                specifier: specifier.clone(),
                specifier_range,
                range: locations.range(node),
                default,
                type_only,
                syntax_valid: !node.has_error() && !node.is_error() && !node.is_missing(),
            });
        }
        declarations.extend(declared);
        return;
    }

    let mut found = false;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "export_clause" {
            continue;
        }
        let mut specifier_cursor = child.walk();
        for export_specifier in child.named_children(&mut specifier_cursor) {
            if export_specifier.kind() != "export_specifier" {
                continue;
            }
            let identifier_nodes = identifier_nodes(export_specifier, text);
            let name = identifier_nodes
                .first()
                .and_then(|identifier| node_text(text, *identifier))
                .map(str::to_owned);
            let exported_name = identifier_nodes
                .last()
                .and_then(|identifier| node_text(text, *identifier))
                .map(str::to_owned)
                .or_else(|| name.clone());
            let default_export = exported_name.as_deref() == Some("default");
            exports.push(Export {
                name,
                name_range: identifier_nodes
                    .first()
                    .map(|identifier| locations.range(*identifier)),
                exported_name,
                exported_name_range: identifier_nodes
                    .last()
                    .map(|identifier| locations.range(*identifier)),
                specifier: specifier.clone(),
                specifier_range,
                // Navigation from `from` needs the entire re-export statement,
                // while `name_range` retains the precise exported identifier.
                range: locations.range(node),
                default: default_export,
                type_only: type_only || has_keyword(export_specifier, text, "type"),
                syntax_valid: !node.has_error() && !node.is_error() && !node.is_missing(),
            });
            found = true;
        }
    }
    if !found {
        // `export default Head;` names a local binding, the form published
        // declaration files use for components such as Inertia's `Head`.
        let local = default
            .then(|| node.child_by_field_name("value"))
            .flatten()
            .filter(|value| value.kind() == "identifier");
        exports.push(Export {
            name: local
                .and_then(|value| node_text(text, value))
                .map(str::to_owned),
            name_range: local.map(|value| locations.range(value)),
            exported_name: if default {
                Some("default".to_owned())
            } else {
                None
            },
            exported_name_range: None,
            specifier,
            specifier_range,
            range: locations.range(node),
            default,
            type_only,
            syntax_valid: !node.has_error() && !node.is_error() && !node.is_missing(),
        });
    }
}

pub(super) fn export_assignment_name(text: &str, node: Node<'_>) -> Option<String> {
    let source = node_text(text, node)?.trim_start();
    let name = source
        .strip_prefix("export =")?
        .trim()
        .trim_end_matches(';')
        .trim();
    (!name.is_empty()
        && name
            .bytes()
            .all(|byte| byte == b'_' || byte == b'$' || byte.is_ascii_alphanumeric()))
    .then(|| name.to_owned())
}

pub(super) fn source_specifier(text: &str, node: Node<'_>) -> Option<String> {
    source_specifier_node(node)
        .and_then(|source| node_text(text, source))
        .map(|source| source.trim_matches(['\'', '"']).to_owned())
}

pub(super) fn source_specifier_node(node: Node<'_>) -> Option<Node<'_>> {
    node.child_by_field_name("source").or_else(|| {
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .find(|child| child.kind() == "string")
    })
}

pub(super) fn collect_namespace_members(
    text: &str,
    node: Node<'_>,
    locations: &Locations,
    members: &mut Vec<NamespaceMemberUse>,
) {
    if matches!(node.kind(), "member_expression" | "jsx_member_expression")
        && let (Some(object), Some(property)) = (node.named_child(0), node.named_child(1))
        && matches!(object.kind(), "identifier" | "jsx_identifier")
        && matches!(
            property.kind(),
            "identifier" | "property_identifier" | "jsx_identifier"
        )
        && !node.parent().is_some_and(|parent| {
            matches!(parent.kind(), "member_expression" | "jsx_member_expression")
                && parent
                    .named_child(0)
                    .is_some_and(|object| object.id() == node.id())
        })
        && let Some(member_name) = node_text(text, property)
    {
        members.push(NamespaceMemberUse {
            namespace_range: locations.range(object),
            namespace_name: node_text(text, object).unwrap_or_default().to_owned(),
            member_name: member_name.to_owned(),
            member_range: locations.range(property),
        });
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_namespace_members(text, child, locations, members);
    }
}
