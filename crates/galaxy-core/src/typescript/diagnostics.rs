//! Syntax errors and the small static type checker.

use super::*;

pub(super) fn diagnostics(root: Node<'_>, locations: &Locations) -> Vec<ParseDiagnostic> {
    let mut diagnostics = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_error() || node.is_missing() {
            diagnostics.push(ParseDiagnostic {
                message: if node.is_missing() {
                    "Missing syntax"
                } else {
                    "Syntax error"
                }
                .to_owned(),
                range: locations.range(node),
            });
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    diagnostics.sort_by_key(|diagnostic| (diagnostic.range.start.line, diagnostic.range.start.col));
    diagnostics
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Type {
    Unknown,
    Primitive(&'static str),
    Null,
    Undefined,
    Object(std::collections::BTreeMap<String, Type>),
    Function(Vec<Type>, Box<Type>, Vec<String>),
    Named(String, Vec<Type>),
    Union(Vec<Type>),
}

#[derive(Default, Clone)]
pub(super) struct TypeState {
    values: std::collections::HashMap<String, Type>,
    aliases: std::collections::HashMap<String, (Vec<String>, Type)>,
    diagnostics: Vec<TypeDiagnostic>,
}

pub(super) fn type_diagnostics(
    text: &str,
    root: Node<'_>,
    locations: &Locations,
) -> Vec<TypeDiagnostic> {
    let mut state = TypeState::default();
    // First collect named declarations so forward references and aliases are stable.
    collect_type_declarations(text, root, &mut state);
    check_type_node(text, root, locations, &mut state);
    state.diagnostics
}

pub(super) fn collect_type_declarations(text: &str, node: Node<'_>, state: &mut TypeState) {
    match node.kind() {
        "type_alias_declaration" => {
            if let Some(name) = node
                .child_by_field_name("name")
                .and_then(|n| node_text(text, n))
            {
                let parameters = type_parameters(text, node);
                if let Some(value) = node.child_by_field_name("value") {
                    state
                        .aliases
                        .insert(name.to_owned(), (parameters, parse_type(text, value)));
                }
            }
        }
        "interface_declaration" | "class_declaration" => {
            if let Some(name) = node
                .child_by_field_name("name")
                .and_then(|n| node_text(text, n))
            {
                let mut properties = std::collections::BTreeMap::new();
                let mut stack = vec![node];
                while let Some(item) = stack.pop() {
                    if matches!(
                        item.kind(),
                        "property_signature" | "public_field_definition"
                    ) && let Some(field) = item
                        .child_by_field_name("name")
                        .and_then(|n| node_text(text, n))
                    {
                        let ty = item
                            .child_by_field_name("type")
                            .map(|n| parse_type(text, n))
                            .unwrap_or(Type::Unknown);
                        properties.insert(field.to_owned(), ty);
                    }
                    let mut cursor = item.walk();
                    stack.extend(item.named_children(&mut cursor));
                }
                let parameters = type_parameters(text, node);
                let ty = if node.kind() == "class_declaration" {
                    Type::Function(
                        Vec::new(),
                        Box::new(Type::Object(properties)),
                        parameters.clone(),
                    )
                } else {
                    Type::Object(properties)
                };
                state.aliases.insert(name.to_owned(), (parameters, ty));
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_type_declarations(text, child, state);
    }
}

pub(super) fn check_type_node(
    text: &str,
    node: Node<'_>,
    locations: &Locations,
    state: &mut TypeState,
) {
    match node.kind() {
        "lexical_declaration" | "variable_declaration" => {
            let mut cursor = node.walk();
            for declarator in node
                .named_children(&mut cursor)
                .filter(|n| n.kind() == "variable_declarator")
            {
                let Some(name) = declarator
                    .child_by_field_name("name")
                    .and_then(|n| node_text(text, n))
                else {
                    continue;
                };
                let declared = declarator
                    .child_by_field_name("type")
                    .map(|n| parse_type(text, n));
                let value = declarator
                    .child_by_field_name("value")
                    .map(|n| infer_type(text, n, state));
                if let (Some(expected), Some(actual)) = (&declared, &value) {
                    report_mismatch(
                        expected,
                        actual,
                        locations.range(declarator.child_by_field_name("value").unwrap()),
                        state,
                    );
                }
                state
                    .values
                    .insert(name.to_owned(), declared.or(value).unwrap_or(Type::Unknown));
            }
            return;
        }
        "function_declaration" | "method_definition" => {
            let Some(name) = node
                .child_by_field_name("name")
                .and_then(|n| node_text(text, n))
            else {
                return;
            };
            let mut parameters = Vec::new();
            if let Some(params) = node.child_by_field_name("parameters") {
                let mut cursor = params.walk();
                for parameter in params.named_children(&mut cursor) {
                    parameters.push(
                        parameter
                            .child_by_field_name("type")
                            .map(|n| parse_type(text, n))
                            .unwrap_or(Type::Unknown),
                    );
                }
            }
            let result = node
                .child_by_field_name("return_type")
                .map(|n| parse_type(text, n))
                .unwrap_or(Type::Unknown);
            state.values.insert(
                name.to_owned(),
                Type::Function(parameters, Box::new(result), type_parameters(text, node)),
            );
        }
        "assignment_expression" => {
            let right = node.child_by_field_name("right");
            let left = node.child_by_field_name("left");
            if let (Some(left), Some(right)) = (left, right) {
                let actual = infer_type(text, right, state);
                if left.kind() == "identifier"
                    && let Some(expected) = node_text(text, left)
                        .and_then(|name| state.values.get(name))
                        .cloned()
                {
                    report_mismatch(&expected, &actual, locations.range(right), state);
                }
            }
        }
        "call_expression" | "new_expression" => check_call(text, node, locations, state),
        "if_statement" => {
            let condition = node.child_by_field_name("condition");
            let consequence = node.child_by_field_name("consequence");
            if let (Some(condition), Some(consequence)) = (condition, consequence) {
                let mut narrowed = state.clone();
                let diagnostic_count = narrowed.diagnostics.len();
                narrow_condition(text, condition, &mut narrowed);
                check_type_node(text, consequence, locations, &mut narrowed);
                state
                    .diagnostics
                    .extend(narrowed.diagnostics.into_iter().skip(diagnostic_count));
            }
            if let Some(alternative) = node.child_by_field_name("alternative") {
                check_type_node(text, alternative, locations, state);
            }
            return;
        }
        "member_expression" => {
            let object = node
                .child_by_field_name("object")
                .map(|n| infer_type(text, n, state))
                .unwrap_or(Type::Unknown);
            let property = node.child_by_field_name("property");
            if let (Type::Object(fields), Some(property)) = (resolve(&object, state), property) {
                let name = node_text(text, property).unwrap_or("");
                if !fields.contains_key(name) {
                    state.diagnostics.push(TypeDiagnostic {
                        code: "TS2339".into(),
                        message: format!("Property '{}' does not exist on this type", name),
                        range: locations.range(property),
                    });
                }
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        check_type_node(text, child, locations, state);
    }
}

pub(super) fn infer_type(text: &str, node: Node<'_>, state: &mut TypeState) -> Type {
    match node.kind() {
        "number" => Type::Primitive("number"),
        "string" | "template_string" => Type::Primitive("string"),
        "true" | "false" => Type::Primitive("boolean"),
        "null" => Type::Null,
        "undefined" => Type::Undefined,
        "identifier" => node_text(text, node)
            .map(|name| {
                state.values.get(name).cloned().unwrap_or_else(|| {
                    if state.aliases.contains_key(name) {
                        Type::Named(name.to_owned(), Vec::new())
                    } else {
                        Type::Unknown
                    }
                })
            })
            .unwrap_or(Type::Unknown),
        "object" => {
            let mut fields = std::collections::BTreeMap::new();
            let mut cursor = node.walk();
            for pair in node
                .named_children(&mut cursor)
                .filter(|n| n.kind() == "pair")
            {
                if let (Some(key), Some(value)) = (
                    pair.child_by_field_name("key")
                        .and_then(|n| node_text(text, n)),
                    pair.child_by_field_name("value"),
                ) {
                    fields.insert(
                        key.trim_matches(['\'', '"']).to_owned(),
                        infer_type(text, value, state),
                    );
                }
            }
            Type::Object(fields)
        }
        "arrow_function" | "function_expression" => {
            Type::Function(Vec::new(), Box::new(Type::Unknown), Vec::new())
        }
        "member_expression" => {
            let object = node
                .child_by_field_name("object")
                .map(|n| infer_type(text, n, state))
                .unwrap_or(Type::Unknown);
            let property = node
                .child_by_field_name("property")
                .and_then(|n| node_text(text, n))
                .unwrap_or("");
            match resolve(&object, state) {
                Type::Object(fields) => fields.get(property).cloned().unwrap_or(Type::Unknown),
                _ => Type::Unknown,
            }
        }
        "parenthesized_expression" => node
            .named_child(0)
            .map(|n| infer_type(text, n, state))
            .unwrap_or(Type::Unknown),
        "call_expression" | "new_expression" => {
            let callee = node
                .child_by_field_name("function")
                .or_else(|| node.child_by_field_name("constructor"));
            let Some(callee) = callee else {
                return Type::Unknown;
            };
            let Type::Function(_, result, parameters) =
                resolve(&infer_type(text, callee, state), state)
            else {
                return Type::Unknown;
            };
            let args = node
                .child_by_field_name("type_arguments")
                .map(|arguments| {
                    node_text(text, arguments)
                        .unwrap_or("")
                        .trim_matches(['<', '>'])
                        .split(',')
                        .map(parse_type_text)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            substitute(&result, &parameters, &args)
        }
        _ => Type::Unknown,
    }
}

pub(super) fn check_call(text: &str, node: Node<'_>, locations: &Locations, state: &mut TypeState) {
    let function = node
        .child_by_field_name("function")
        .or_else(|| node.child_by_field_name("constructor"));
    let Some(function) = function else { return };
    let mut callee = infer_type(text, function, state);
    if matches!(node.kind(), "new_expression") {
        callee = resolve(&callee, state);
    }
    let Type::Function(parameters, _, generic_parameters) = callee else {
        return;
    };
    let explicit_arguments = node
        .child_by_field_name("type_arguments")
        .map(|arguments| {
            node_text(text, arguments)
                .unwrap_or("")
                .trim_matches(['<', '>'])
                .split(',')
                .map(parse_type_text)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let parameters: Vec<_> = parameters
        .iter()
        .map(|ty| substitute(ty, &generic_parameters, &explicit_arguments))
        .collect();
    let arguments = node.child_by_field_name("arguments");
    let args: Vec<_> = arguments
        .into_iter()
        .flat_map(|list| {
            let mut cursor = list.walk();
            list.named_children(&mut cursor).collect::<Vec<_>>()
        })
        .collect();
    if args.len() != parameters.len() {
        state.diagnostics.push(TypeDiagnostic {
            code: "TS2554".into(),
            message: format!(
                "Expected {} arguments, but got {}",
                parameters.len(),
                args.len()
            ),
            range: locations.range(node),
        });
    }
    for (argument, expected) in args.into_iter().zip(parameters) {
        let actual = infer_type(text, argument, state);
        report_mismatch(&expected, &actual, locations.range(argument), state);
    }
}

pub(super) fn report_mismatch(
    expected: &Type,
    actual: &Type,
    range: Utf16Range,
    state: &mut TypeState,
) {
    if !assignable(expected, actual, state)
        && !matches!(expected, Type::Unknown)
        && !matches!(actual, Type::Unknown)
    {
        state.diagnostics.push(TypeDiagnostic {
            code: "TS2322".into(),
            message: "Type is not assignable to the declared type".into(),
            range,
        });
    }
}

pub(super) fn assignable(expected: &Type, actual: &Type, state: &TypeState) -> bool {
    match (resolve(expected, state), resolve(actual, state)) {
        (Type::Unknown, _) | (_, Type::Unknown) => true,
        (Type::Union(types), actual) => types.iter().any(|ty| assignable(ty, &actual, state)),
        (Type::Primitive(a), Type::Primitive(b)) => a == b,
        (Type::Null, Type::Null) | (Type::Undefined, Type::Undefined) => true,
        (Type::Object(expected), Type::Object(actual)) => expected.iter().all(|(name, ty)| {
            actual
                .get(name)
                .is_some_and(|actual| assignable(ty, actual, state))
        }),
        _ => false,
    }
}

pub(super) fn resolve(ty: &Type, state: &TypeState) -> Type {
    match ty {
        Type::Named(name, args) => state
            .aliases
            .get(name)
            .map_or(Type::Unknown, |(parameters, value)| {
                substitute(value, parameters, args)
            }),
        _ => ty.clone(),
    }
}

pub(super) fn substitute(ty: &Type, parameters: &[String], args: &[Type]) -> Type {
    match ty {
        Type::Named(name, nested) if nested.is_empty() => parameters
            .iter()
            .position(|parameter| parameter == name)
            .and_then(|index| args.get(index))
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        Type::Union(items) => Type::Union(
            items
                .iter()
                .map(|item| substitute(item, parameters, args))
                .collect(),
        ),
        Type::Object(items) => Type::Object(
            items
                .iter()
                .map(|(name, item)| (name.clone(), substitute(item, parameters, args)))
                .collect(),
        ),
        Type::Function(items, result, generic) => Type::Function(
            items
                .iter()
                .map(|item| substitute(item, parameters, args))
                .collect(),
            Box::new(substitute(result, parameters, args)),
            generic.clone(),
        ),
        _ => ty.clone(),
    }
}

pub(super) fn parse_type(text: &str, node: Node<'_>) -> Type {
    parse_type_text(node_text(text, node).unwrap_or(""))
}

pub(super) fn narrow_condition(text: &str, condition: Node<'_>, state: &mut TypeState) {
    let condition = node_text(text, condition).unwrap_or("").trim();
    let (name, narrowed) = if let Some(rest) = condition.strip_prefix("typeof ") {
        let Some((name, value)) = rest.split_once("===") else {
            return;
        };
        let value = value.trim().trim_matches(['\'', '"']);
        let ty = match value {
            "string" => Type::Primitive("string"),
            "number" => Type::Primitive("number"),
            "boolean" => Type::Primitive("boolean"),
            _ => return,
        };
        (name.trim(), ty)
    } else {
        let Some((name, value)) = condition
            .split_once("!==")
            .or_else(|| condition.split_once("!="))
        else {
            return;
        };
        let ty = match value.trim() {
            "null" => Type::Null,
            "undefined" => Type::Undefined,
            _ => return,
        };
        let Some(Type::Union(items)) = state.values.get(name.trim()) else {
            return;
        };
        let items: Vec<_> = items.iter().filter(|item| **item != ty).cloned().collect();
        if items.len() != 1 {
            return;
        }
        (name.trim(), items[0].clone())
    };
    if let Some(Type::Union(items)) = state.values.get(name)
        && items.contains(&narrowed)
    {
        state.values.insert(name.to_owned(), narrowed);
    }
}

pub(super) fn parse_type_text(text: &str) -> Type {
    let text = text.trim().trim_start_matches(':').trim();
    let union: Vec<_> = text.split('|').map(str::trim).collect();
    if union.len() > 1 {
        return Type::Union(union.into_iter().map(parse_type_text).collect());
    }
    match text {
        "string" => Type::Primitive("string"),
        "number" => Type::Primitive("number"),
        "boolean" => Type::Primitive("boolean"),
        "void" => Type::Primitive("void"),
        "null" => Type::Null,
        "undefined" => Type::Undefined,
        _ if text.starts_with('{') && text.ends_with('}') => Type::Object(
            text[1..text.len() - 1]
                .split(';')
                .filter_map(|field| {
                    field.split_once(':').map(|(name, ty)| {
                        (
                            name.trim().trim_end_matches('?').to_owned(),
                            parse_type_text(ty),
                        )
                    })
                })
                .collect(),
        ),
        _ => {
            let (name, args) = text
                .split_once('<')
                .map_or((text, Vec::new()), |(name, args)| {
                    (
                        name.trim(),
                        args.trim_end_matches('>')
                            .split(',')
                            .map(parse_type_text)
                            .collect(),
                    )
                });
            Type::Named(name.to_owned(), args)
        }
    }
}

pub(super) fn type_parameters(text: &str, node: Node<'_>) -> Vec<String> {
    node.child_by_field_name("type_parameters")
        .map(|params| {
            node_text(text, params)
                .unwrap_or("")
                .trim_matches(['<', '>'])
                .split(',')
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .collect()
        })
        .unwrap_or_default()
}
