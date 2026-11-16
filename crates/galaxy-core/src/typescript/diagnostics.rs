//! Syntax errors and the small static type checker.

use super::*;

pub(super) fn diagnostics(
    root: Node<'_>,
    text: &str,
    locations: &Locations,
) -> Vec<ParseDiagnostic> {
    let mut diagnostics = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if is_bare_jsx_ampersand(text, node) {
            continue;
        }
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

/// `Operations & Automation` between JSX tags. The grammar only accepts
/// `&name;` references there, but TypeScript and Babel read a bare `&` as
/// text, so the `ERROR` it produces is not a real error.
pub(super) fn is_bare_jsx_ampersand(text: &str, node: Node<'_>) -> bool {
    node.is_error()
        && node
            .parent()
            .is_some_and(|parent| matches!(parent.kind(), "jsx_element" | "jsx_fragment"))
        && node_text(text, node)
            .is_some_and(|source| source.starts_with('&') && !source.contains(['<', '>', '{', '}']))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Type {
    Unknown,
    Primitive(&'static str),
    Null,
    Undefined,
    Object(std::collections::BTreeMap<String, Type>),
    Function(Vec<Type>, usize, Box<Type>, Vec<String>),
    Named(String, Vec<Type>),
    Union(Vec<Type>),
    LibraryValue(String),
    LibraryCallable(Vec<stdlib::LibSignature>),
}

#[derive(Clone)]
pub(super) struct TypeState {
    values: std::collections::HashMap<String, Type>,
    /// Annotated types of `let x: T` bindings. Narrowing changes `values`,
    /// but assignments are still checked against the declared type, so
    /// `x = null` stays valid after `if (x === null) return`.
    declared: std::collections::HashMap<String, Type>,
    aliases: std::collections::HashMap<String, (Vec<String>, Type)>,
    /// Local classes: the only declarations collected here that also create a
    /// runtime value. Interfaces and type aliases name types only, so an
    /// augmentation such as `declare module '@vinejs/vine' { interface Vine {} }`
    /// or `declare global { interface String {} }` must not replace the
    /// imported or global value in expressions like `Vine.macro`.
    classes: std::collections::HashSet<String>,
    unresolved: std::collections::HashSet<String>,
    stdlib: std::sync::Arc<stdlib::StdLib>,
    diagnostics: Vec<TypeDiagnostic>,
}

impl TypeState {
    fn new(path: &std::path::Path, unresolved: &[(String, Utf16Range)]) -> Self {
        Self {
            values: Default::default(),
            declared: Default::default(),
            aliases: Default::default(),
            classes: Default::default(),
            unresolved: unresolved.iter().map(|(name, _)| name.clone()).collect(),
            stdlib: std::sync::Arc::new(stdlib::StdLib::for_file(path)),
            diagnostics: Vec::new(),
        }
    }
}

pub(super) fn type_diagnostics(
    path: &std::path::Path,
    text: &str,
    root: Node<'_>,
    locations: &Locations,
    unresolved: &[(String, Utf16Range)],
) -> Vec<TypeDiagnostic> {
    let mut state = TypeState::new(path, unresolved);
    // First collect named declarations so forward references and aliases are stable.
    collect_type_declarations(text, root, &mut state);
    let mut cursor = root.walk();
    let imports = root
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "import_statement")
        .map(|node| import_from_node(text, node, locations))
        .collect::<Vec<_>>();
    let context = framework::Context {
        text,
        locations,
        imports: &imports,
        import_symbols: &[],
    };
    for name in framework::macroable_classes(path, root, &context) {
        state.values.insert(name, macroable_constructor());
    }
    check_type_node(text, root, locations, &mut state);
    state.diagnostics
}

fn macroable_constructor() -> Type {
    Type::Object(std::collections::BTreeMap::from([(
        "macro".to_owned(),
        Type::Function(
            vec![Type::Primitive("string"), Type::Unknown],
            2,
            Box::new(Type::Unknown),
            Vec::new(),
        ),
    )]))
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
        "interface_declaration" | "class_declaration" | "abstract_class_declaration" => {
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
                let is_class = matches!(
                    node.kind(),
                    "class_declaration" | "abstract_class_declaration"
                );
                let mut heritage_cursor = node.walk();
                let derived = is_class
                    && node
                        .named_children(&mut heritage_cursor)
                        .any(|child| child.kind() == "class_heritage");
                let ty = if derived && class_constructor(text, node).is_none() {
                    // A derived class without its own constructor inherits the
                    // superclass constructor and members (e.g. `Storage extends
                    // Disk` from flydrive). Neither is known here, so fail
                    // closed instead of assuming a zero-argument constructor.
                    state.classes.insert(name.to_owned());
                    Type::Unknown
                } else if is_class {
                    let constructor = class_constructor(text, node).unwrap_or_else(|| {
                        Type::Function(Vec::new(), 0, Box::new(Type::Unknown), parameters.clone())
                    });
                    let Type::Function(arguments, required, _, generic) = constructor else {
                        unreachable!("class constructors are functions")
                    };
                    state.classes.insert(name.to_owned());
                    Type::Function(
                        arguments,
                        required,
                        Box::new(Type::Object(properties)),
                        generic,
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
                match &declared {
                    Some(declared) => {
                        state.declared.insert(name.to_owned(), declared.clone());
                    }
                    None => {
                        state.declared.remove(name);
                    }
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
            state
                .values
                .insert(name.to_owned(), function_type(text, node));
        }
        "assignment_expression" => {
            let right = node.child_by_field_name("right");
            let left = node.child_by_field_name("left");
            if let (Some(left), Some(right)) = (left, right) {
                let actual = infer_type(text, right, state);
                if left.kind() == "identifier"
                    && let Some(expected) = node_text(text, left)
                        .and_then(|name| {
                            state.declared.get(name).or_else(|| state.values.get(name))
                        })
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
            if let Some(property) = property
                && let Type::Object(fields) = resolve(&object, state)
            {
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
    let mut narrowed = Vec::new();
    for child in node.named_children(&mut cursor) {
        check_type_node(text, child, locations, state);
        narrowed.extend(narrow_after_early_exit(text, child, state));
    }
    // The narrowing only holds for the rest of this block. Restore in
    // reverse so a name narrowed twice gets its original type back.
    for (name, previous) in narrowed.into_iter().rev() {
        state.values.insert(name, previous);
    }
}

/// `if (x === null) return` leaves `x` without `null` for the statements
/// after it in the same block. Only exits without an `else` are handled.
/// Returns the name and its type before narrowing.
fn narrow_after_early_exit(
    text: &str,
    node: Node<'_>,
    state: &mut TypeState,
) -> Option<(String, Type)> {
    if node.kind() != "if_statement" || node.child_by_field_name("alternative").is_some() {
        return None;
    }
    let (Some(condition), Some(consequence)) = (
        node.child_by_field_name("condition"),
        node.child_by_field_name("consequence"),
    ) else {
        return None;
    };
    let exit = if consequence.kind() == "statement_block" {
        let count = consequence.named_child_count();
        count
            .checked_sub(1)
            .and_then(|last| consequence.named_child(last))
    } else {
        Some(consequence)
    };
    if !exit.is_some_and(|exit| {
        matches!(
            exit.kind(),
            "return_statement" | "throw_statement" | "continue_statement" | "break_statement"
        )
    }) {
        return None;
    }
    let condition = node_text(text, condition)
        .unwrap_or("")
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim();
    let (name, value, loose) = condition
        .split_once("===")
        .map(|(name, value)| (name, value, false))
        .or_else(|| {
            condition
                .split_once("==")
                .map(|(name, value)| (name, value, true))
        })?;
    let removed: &[Type] = match (value.trim(), loose) {
        ("null", false) => &[Type::Null],
        ("undefined", false) => &[Type::Undefined],
        ("null" | "undefined", true) => &[Type::Null, Type::Undefined],
        _ => return None,
    };
    let name = name.trim();
    let Some(Type::Union(items)) = state.values.get(name) else {
        return None;
    };
    let mut items: Vec<_> = items
        .iter()
        .filter(|item| !removed.contains(item))
        .cloned()
        .collect();
    let narrowed = match items.len() {
        0 => return None,
        1 => items.remove(0),
        _ => Type::Union(items),
    };
    state
        .values
        .insert(name.to_owned(), narrowed)
        .map(|previous| (name.to_owned(), previous))
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
                    if state.unresolved.contains(name) && state.stdlib.has_global(name) {
                        global_library_type(name, state)
                    } else if state.classes.contains(name) && state.aliases.contains_key(name) {
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
            for member in node.named_children(&mut cursor) {
                match member.kind() {
                    "pair" => {
                        if let (Some(key), Some(value)) = (
                            member
                                .child_by_field_name("key")
                                .and_then(|n| node_text(text, n)),
                            member.child_by_field_name("value"),
                        ) {
                            fields.insert(
                                key.trim_matches(['\'', '"']).to_owned(),
                                infer_type(text, value, state),
                            );
                        }
                    }
                    // Object shorthand methods are siblings of `pair` nodes:
                    // `{ write(type: string) {} }` must infer a `write` field.
                    "method_definition" => {
                        if let Some(name) = member
                            .child_by_field_name("name")
                            .and_then(|n| node_text(text, n))
                        {
                            fields.insert(
                                name.trim_matches(['\'', '"']).to_owned(),
                                function_type(text, member),
                            );
                        }
                    }
                    // Object shorthand values are siblings of `pair` nodes:
                    // `{ data, cache }` keeps the existing bindings' types.
                    "shorthand_property_identifier" => {
                        if let Some(name) = node_text(text, member) {
                            fields.insert(name.to_owned(), infer_type(text, member, state));
                        }
                    }
                    _ => {}
                }
            }
            Type::Object(fields)
        }
        "arrow_function" | "function_expression" => function_type(text, node),
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
                Type::LibraryValue(owner) => state
                    .stdlib
                    .value_member(&owner, property)
                    .map(|member| library_member_type(&member))
                    .unwrap_or(Type::Unknown),
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
            let callee_type = resolve(&infer_type(text, callee, state), state);
            if let Type::LibraryCallable(signatures) = callee_type {
                return library_return_type(&signatures);
            }
            if let Type::LibraryValue(owner) = callee_type {
                return library_return_type(&library_value_signatures(
                    &owner,
                    node.kind() == "new_expression",
                    state,
                ));
            }
            let Type::Function(_, _, result, parameters) = callee_type else {
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

fn global_library_type(name: &str, _state: &TypeState) -> Type {
    Type::LibraryValue(name.to_owned())
}

fn library_value_signatures(
    owner: &str,
    construct: bool,
    state: &TypeState,
) -> Vec<stdlib::LibSignature> {
    let mut signatures = state.stdlib.value_signatures(owner).to_vec();
    if let Some(type_name) = state.stdlib.value_type(owner) {
        signatures.extend_from_slice(if construct {
            state.stdlib.construct_signatures(type_name)
        } else {
            state.stdlib.call_signatures(type_name)
        });
    }
    signatures
}

fn library_member_type(member: &stdlib::LibMember) -> Type {
    if !member.signatures.is_empty() {
        Type::LibraryCallable(member.signatures.clone())
    } else {
        member
            .type_name
            .as_deref()
            .map(library_type)
            .unwrap_or(Type::Unknown)
    }
}

fn library_type(name: &str) -> Type {
    let parsed = parse_type_text(name);
    match parsed {
        Type::Primitive(_) | Type::Null | Type::Undefined | Type::Union(_) => parsed,
        _ => Type::Unknown,
    }
}

fn library_return_type(signatures: &[stdlib::LibSignature]) -> Type {
    let mut returns = signatures
        .iter()
        .filter_map(|signature| signature.return_type.as_deref())
        .map(library_type);
    let Some(first) = returns.next() else {
        return Type::Unknown;
    };
    if returns.all(|return_type| return_type == first) {
        first
    } else {
        Type::Unknown
    }
}

fn class_constructor(text: &str, class: Node<'_>) -> Option<Type> {
    let body = class.child_by_field_name("body")?;
    let mut cursor = body.walk();
    body.named_children(&mut cursor)
        .find(|member| {
            member.kind() == "method_definition"
                && member
                    .child_by_field_name("name")
                    .and_then(|name| node_text(text, name))
                    == Some("constructor")
        })
        .map(|constructor| function_type(text, constructor))
}

fn function_type(text: &str, node: Node<'_>) -> Type {
    let mut parameters = Vec::new();
    let mut required_parameters = 0;
    if let Some(params) = node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for parameter in params.named_children(&mut cursor) {
            let has_default = node_text(text, parameter).is_some_and(|source| source.contains('='));
            if !matches!(
                parameter.kind(),
                "optional_parameter" | "assignment_pattern"
            ) && !has_default
            {
                required_parameters += 1;
            }
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
    Type::Function(
        parameters,
        required_parameters,
        Box::new(result),
        type_parameters(text, node),
    )
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
    let library_signatures = match &callee {
        Type::LibraryCallable(signatures) => Some(signatures.clone()),
        Type::LibraryValue(owner) => Some(library_value_signatures(
            owner,
            node.kind() == "new_expression",
            state,
        )),
        _ => None,
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
    let arguments = node.child_by_field_name("arguments");
    let args: Vec<_> = arguments
        .into_iter()
        .flat_map(|list| {
            let mut cursor = list.walk();
            list.named_children(&mut cursor).collect::<Vec<_>>()
        })
        .collect();
    if args
        .iter()
        .any(|argument| argument.kind() == "spread_element")
    {
        return;
    }
    if let Some(signatures) = library_signatures {
        if signatures.iter().any(|signature| {
            args.len() >= signature.required_parameters
                && signature
                    .maximum_parameters
                    .is_none_or(|maximum| args.len() <= maximum)
        }) {
            return;
        }
        let required = signatures
            .iter()
            .map(|signature| signature.required_parameters)
            .min()
            .unwrap_or(0);
        let maximum = signatures
            .iter()
            .filter_map(|signature| signature.maximum_parameters)
            .max();
        state.diagnostics.push(TypeDiagnostic {
            code: "TS2554".into(),
            message: format!(
                "Expected {} arguments, but got {}",
                maximum.unwrap_or(required),
                args.len()
            ),
            range: locations.range(node),
        });
        return;
    }
    let Type::Function(parameters, required_parameters, _, generic_parameters) = callee else {
        return;
    };
    let parameters: Vec<_> = parameters
        .iter()
        .map(|ty| substitute(ty, &generic_parameters, &explicit_arguments))
        .collect();
    if args.len() < required_parameters || args.len() > parameters.len() {
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
        // `number | null` fits `number | null`: every member must fit.
        (expected, Type::Union(actual)) => actual
            .iter()
            .all(|member| assignable(&expected, member, state)),
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
        Type::Function(items, required, result, generic) => Type::Function(
            items
                .iter()
                .map(|item| substitute(item, parameters, args))
                .collect(),
            *required,
            Box::new(substitute(result, parameters, args)),
            generic.clone(),
        ),
        Type::LibraryValue(_) | Type::LibraryCallable(_) => ty.clone(),
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
        // Index signatures (`{ [key: string]: T }`), nested objects and
        // function members are beyond this checker; splitting on `;` and `:`
        // would invent required fields such as `[key`.
        _ if text.starts_with('{')
            && text.ends_with('}')
            && text[1..text.len() - 1].contains(['[', '{', '(']) =>
        {
            Type::Unknown
        }
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
