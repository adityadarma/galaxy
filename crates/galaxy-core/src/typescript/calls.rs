//! Call sites and declared parameters, for inline parameter hints.

use super::*;
use crate::parameter_hints::{Argument, Parameter};

/// Longest type written in a label; longer ones would crowd the line.
const MAX_LABEL_CHARS: usize = 48;

/// A call in the requested lines: where its callee name is, so the host can
/// resolve it like Go to Definition, and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    pub callee_line: usize,
    pub callee_column: usize,
    pub arguments: Vec<Argument>,
}

fn parse(path: &Path, text: &str) -> Option<tree_sitter::Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_language(language_for_path(path)))
        .ok()?;
    parser.parse(text, None)
}

/// Calls and `new` expressions with arguments that touch `first..=last`.
/// The callee is the called name: `f` in `f(1)`, `log` in `console.log(1)`.
pub fn call_sites(path: &Path, text: &str, first: usize, last: usize) -> Vec<CallSite> {
    let Some(tree) = parse(path, text) else {
        return Vec::new();
    };
    let locations = Locations::new(text);
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.end_position().row < first || node.start_position().row > last {
            continue;
        }
        if let Some(call) = call_site(text, node, &locations) {
            out.push(call);
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    out.sort_by_key(|call| (call.callee_line, call.callee_column));
    out
}

fn call_site(text: &str, node: Node<'_>, locations: &Locations) -> Option<CallSite> {
    let callee = match node.kind() {
        "call_expression" => node.child_by_field_name("function")?,
        "new_expression" => node.child_by_field_name("constructor")?,
        _ => return None,
    };
    let arguments = node.child_by_field_name("arguments")?;
    if arguments.kind() != "arguments" {
        return None; // A tagged template.
    }
    let name = match callee.kind() {
        "identifier" => callee,
        "member_expression" => callee.child_by_field_name("property")?,
        _ => return None,
    };
    let mut cursor = arguments.walk();
    let arguments: Vec<_> = arguments
        .named_children(&mut cursor)
        .filter(|argument| argument.kind() != "comment")
        .map(|argument| {
            let position = locations.position(argument.start_byte());
            Argument {
                line: position.line as u32,
                column: position.col as u32,
                name: argument_name(text, argument),
                spread: argument.kind() == "spread_element",
                named: false,
            }
        })
        .collect();
    if arguments.is_empty() {
        return None;
    }
    let position = locations.position(name.start_byte());
    Some(CallSite {
        callee_line: position.line,
        callee_column: position.col,
        arguments,
    })
}

/// `user` for `user`, `name` for `user.name`; `None` for anything else.
fn argument_name(text: &str, argument: Node<'_>) -> Option<String> {
    let name = match argument.kind() {
        "identifier" => argument,
        "member_expression" => argument.child_by_field_name("property")?,
        _ => return None,
    };
    node_text(text, name).map(str::to_owned)
}

/// The parameters of the function, method or class constructor declared at
/// `line`/`column` (a Go to Definition target: its name, or its first token).
pub fn parameters_at(
    path: &Path,
    text: &str,
    line: usize,
    column: usize,
) -> Option<Vec<Parameter>> {
    let tree = parse(path, text)?;
    let byte = byte_at(text, line, column)?;
    let mut node = tree
        .root_node()
        .descendant_for_byte_range(byte, byte.saturating_add(1).min(text.len()))?;
    for _ in 0..4 {
        if let Some(parameters) = callable_parameters(text, node) {
            return Some(parameters);
        }
        node = node.parent()?;
    }
    None
}

fn callable_parameters(text: &str, node: Node<'_>) -> Option<Vec<Parameter>> {
    formal_parameters(text, callable_node(text, node)?)
}

/// The function-like node behind a declaration: the function itself, the
/// function a variable or property holds, or a class's constructor.
fn callable_node<'t>(text: &str, node: Node<'t>) -> Option<Node<'t>> {
    match node.kind() {
        "function_declaration"
        | "generator_function_declaration"
        | "function_signature"
        | "function_expression"
        | "function"
        | "arrow_function"
        | "method_definition"
        | "method_signature"
        | "abstract_method_signature"
        | "function_type"
        | "construct_signature" => Some(node),
        "variable_declarator" | "public_field_definition" | "pair" | "property_signature" => {
            let value = node.child_by_field_name("value").or_else(|| {
                // `run: (a: string) => void` is typed, not assigned.
                let annotation = node.child_by_field_name("type")?;
                let mut cursor = annotation.walk();
                annotation.named_children(&mut cursor).next()
            })?;
            callable_node(text, value)
        }
        "lexical_declaration" | "variable_declaration" => {
            let mut cursor = node.walk();
            let declarator = node
                .named_children(&mut cursor)
                .find(|child| child.kind() == "variable_declarator")?;
            callable_node(text, declarator)
        }
        "export_statement" => callable_node(text, node.child_by_field_name("declaration")?),
        // `declare function f(a: string): void;`
        "ambient_declaration" => {
            let mut cursor = node.walk();
            let inner = node.named_children(&mut cursor).next()?;
            callable_node(text, inner)
        }
        "class_declaration" | "class" | "abstract_class_declaration" => {
            let body = node.child_by_field_name("body")?;
            let mut cursor = body.walk();
            let constructor = body.named_children(&mut cursor).find(|member| {
                matches!(member.kind(), "method_definition" | "method_signature")
                    && member
                        .child_by_field_name("name")
                        .and_then(|name| node_text(text, name))
                        == Some("constructor")
            })?;
            Some(constructor)
        }
        _ => None,
    }
}

fn formal_parameters(text: &str, node: Node<'_>) -> Option<Vec<Parameter>> {
    let list = node.child_by_field_name("parameters")?;
    let mut cursor = list.walk();
    let mut out = Vec::new();
    for parameter in list.named_children(&mut cursor) {
        // TypeScript wraps each parameter; JavaScript lists patterns directly.
        let pattern = match parameter.kind() {
            "required_parameter" | "optional_parameter" => {
                parameter.child_by_field_name("pattern")?
            }
            "assignment_pattern" => parameter.child_by_field_name("left")?,
            "comment" => continue,
            _ => parameter,
        };
        match pattern.kind() {
            // `this: Window` types the receiver; it takes no argument.
            "this" => continue,
            "identifier" => out.push(Parameter {
                name: node_text(text, pattern)?.to_owned(),
                rest: false,
            }),
            "rest_pattern" => {
                let mut inner = pattern.walk();
                let name = pattern
                    .named_children(&mut inner)
                    .find(|child| child.kind() == "identifier")
                    .and_then(|child| node_text(text, child))
                    .unwrap_or("");
                out.push(Parameter {
                    name: name.to_owned(),
                    rest: true,
                });
            }
            _ => out.push(Parameter {
                name: String::new(),
                rest: false,
            }),
        }
    }
    Some(out)
}

/// A variable declared without a type, in the requested lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariableSite {
    /// Where the name ends, which is where the type label goes.
    pub line: usize,
    pub column: usize,
    pub source: VariableType,
}

/// Where the type of a variable without annotation comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VariableType {
    /// The widened type of a literal: `let n = 1` is `number`.
    Literal(&'static str),
    /// The result of the call whose callee name is at `line`/`column`.
    Call {
        line: usize,
        column: usize,
        awaited: bool,
    },
}

/// `let`, `var` and `const` declarations without a type annotation whose
/// initializer is a literal or a call, in `first..=last`. A `const` literal
/// keeps its literal type, which the code already shows, so it gets none.
pub fn variable_sites(path: &Path, text: &str, first: usize, last: usize) -> Vec<VariableSite> {
    let Some(tree) = parse(path, text) else {
        return Vec::new();
    };
    let locations = Locations::new(text);
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.end_position().row < first || node.start_position().row > last {
            continue;
        }
        if node.kind() == "variable_declarator"
            && out.len() < crate::parameter_hints::MAX_CALLS
            && let Some(site) = variable_site(text, node, &locations)
        {
            out.push(site);
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    out.sort_by_key(|site| (site.line, site.column));
    out
}

fn variable_site(text: &str, declarator: Node<'_>, locations: &Locations) -> Option<VariableSite> {
    let declaration = declarator.parent()?;
    if !matches!(
        declaration.kind(),
        "lexical_declaration" | "variable_declaration"
    ) {
        return None;
    }
    let name = declarator.child_by_field_name("name")?;
    if name.kind() != "identifier" || declarator.child_by_field_name("type").is_some() {
        return None;
    }
    let mut value = declarator.child_by_field_name("value")?;
    let constant = node_text(text, declaration)?.starts_with("const");
    let source = if let Some(literal) = (!constant).then(|| literal_type(text, value)).flatten() {
        VariableType::Literal(literal)
    } else {
        let awaited = value.kind() == "await_expression";
        if awaited {
            let mut cursor = value.walk();
            value = value.named_children(&mut cursor).next()?;
        }
        let callee = call_callee(value)?;
        let position = locations.position(callee.start_byte());
        VariableType::Call {
            line: position.line,
            column: position.col,
            awaited,
        }
    };
    let end = locations.position(name.end_byte());
    Some(VariableSite {
        line: end.line,
        column: end.col,
        source,
    })
}

/// The name called by `f(..)` or `x.f(..)`.
fn call_callee(call: Node<'_>) -> Option<Node<'_>> {
    if call.kind() != "call_expression" {
        return None;
    }
    let function = call.child_by_field_name("function")?;
    match function.kind() {
        "identifier" => Some(function),
        "member_expression" => function.child_by_field_name("property"),
        _ => None,
    }
}

/// What `let x = <literal>` is declared as: the literal's widened type.
fn literal_type(text: &str, value: Node<'_>) -> Option<&'static str> {
    match value.kind() {
        "number" => {
            let source = node_text(text, value)?;
            Some(if source.ends_with('n') {
                "bigint"
            } else {
                "number"
            })
        }
        "string" | "template_string" => Some("string"),
        "true" | "false" => Some("boolean"),
        "regex" => Some("RegExp"),
        "unary_expression" => {
            let operator = node_text(text, value.child_by_field_name("operator")?)?;
            let literal = literal_type(text, value.child_by_field_name("argument")?)?;
            (operator == "-" && matches!(literal, "number" | "bigint")).then_some(literal)
        }
        _ => None,
    }
}

/// The return type written on the function declared at `line`/`column`
/// (a Go to Definition target). `None` when nothing is written, or when the
/// type could mention a type parameter of the function or its class.
/// `awaited` unwraps `Promise<T>` for a call that is awaited.
pub fn return_type_at(
    path: &Path,
    text: &str,
    line: usize,
    column: usize,
    awaited: bool,
) -> Option<String> {
    let tree = parse(path, text)?;
    let byte = byte_at(text, line, column)?;
    let mut node = tree
        .root_node()
        .descendant_for_byte_range(byte, byte.saturating_add(1).min(text.len()))?;
    for _ in 0..4 {
        if matches!(
            node.kind(),
            "class_declaration" | "class" | "abstract_class_declaration"
        ) {
            return None;
        }
        // The target can be the `function` keyword, whose token shares its
        // kind name with the JavaScript function expression.
        if node.is_named()
            && let Some(function) = callable_node(text, node)
        {
            let written = written_return_type(text, function)?;
            return if awaited {
                let inner = written.strip_prefix("Promise<")?.strip_suffix('>')?;
                Some(inner.trim().to_owned())
            } else {
                Some(written)
            };
        }
        node = node.parent()?;
    }
    None
}

fn written_return_type(text: &str, function: Node<'_>) -> Option<String> {
    let annotation = function.child_by_field_name("return_type")?;
    if annotation.kind() != "type_annotation" {
        return None; // `x is string`, `asserts x`.
    }
    let mut owner = Some(function);
    while let Some(node) = owner {
        if node.child_by_field_name("type_parameters").is_some() {
            return None;
        }
        owner = node.parent();
    }
    let written = node_text(text, annotation)?.trim_start_matches(':').trim();
    let unusable = written.is_empty()
        || written.contains('\n')
        || written.chars().count() > MAX_LABEL_CHARS
        || written
            .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
            .any(|word| matches!(word, "this" | "infer" | "asserts"));
    (!unusable).then(|| written.to_owned())
}

fn byte_at(text: &str, line: usize, column: usize) -> Option<usize> {
    let start = if line == 0 {
        0
    } else {
        text.match_indices('\n').nth(line - 1)?.0 + 1
    };
    let end = text[start..].find('\n').map_or(text.len(), |at| start + at);
    let mut units = 0;
    for (offset, character) in text[start..end].char_indices() {
        if units >= column {
            return Some(start + offset);
        }
        units += character.len_utf16();
    }
    Some(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(parameters: Option<Vec<Parameter>>) -> Vec<String> {
        parameters
            .unwrap()
            .into_iter()
            .map(|parameter| {
                if parameter.rest {
                    format!("...{}", parameter.name)
                } else {
                    parameter.name
                }
            })
            .collect()
    }

    #[test]
    fn finds_calls_and_their_arguments_in_the_requested_lines() {
        let text = "greet(name, 42);\nconsole.log(...items);\nnew User('a');\nother(1);\n";
        let calls = call_sites(Path::new("a.ts"), text, 0, 2);
        assert_eq!(calls.len(), 3);
        assert_eq!((calls[0].callee_line, calls[0].callee_column), (0, 0));
        assert_eq!(calls[0].arguments[0].name.as_deref(), Some("name"));
        assert_eq!(calls[0].arguments[1].column, 12);
        assert_eq!((calls[1].callee_line, calls[1].callee_column), (1, 8));
        assert!(calls[1].arguments[0].spread);
        assert_eq!((calls[2].callee_line, calls[2].callee_column), (2, 4));
    }

    #[test]
    fn reads_parameters_of_every_declaration_form() {
        let text = "function greet(name: string, age = 1, ...rest: number[]) {}\n\
                    const add = (a, { b }) => a;\n\
                    class User { constructor(this: User, public id: number) {} }\n\
                    declare function fmt(value: string, opts?: object): string;\n\
                    export const run = function (task) {};\n\
                    interface Api { send(body: string): void; on: (event: string) => void }\n";
        let at = |line, column| names(parameters_at(Path::new("a.ts"), text, line, column));
        assert_eq!(at(0, 9), ["name", "age", "...rest"]);
        assert_eq!(at(1, 6), ["a", ""]);
        assert_eq!(at(2, 6), ["id"]);
        assert_eq!(at(3, 17), ["value", "opts"]);
        assert_eq!(at(4, 0), ["task"]);
        assert_eq!(at(5, 16), ["body"]);
        assert_eq!(at(5, 42), ["event"]);
        assert!(parameters_at(Path::new("a.ts"), "let x = 1;\n", 0, 4).is_none());
    }

    #[test]
    fn javascript_patterns_are_parameters_too() {
        let text = "function f(a, b = 2, ...c) {}\n";
        assert_eq!(
            names(parameters_at(Path::new("a.js"), text, 0, 9)),
            ["a", "b", "...c"]
        );
    }

    fn column(text: &str, line: usize, needle: &str) -> usize {
        text.lines().nth(line).unwrap().find(needle).unwrap()
    }

    #[test]
    fn variables_without_a_type_name_where_it_comes_from() {
        let text = "let a = 89;\nconst b = 89;\nlet c = `x${a}`;\nvar d = true;\nlet e: number = 1;\nlet f = -2;\nlet g = make();\nconst h = await api.load();\nlet { i } = obj;\nlet j = new Box();\nlet k = 5n;\n";
        let sites = variable_sites(Path::new("a.ts"), text, 0, 10);
        let seen: Vec<_> = sites
            .iter()
            .map(|site| (site.line, site.column, site.source.clone()))
            .collect();
        assert_eq!(
            seen,
            vec![
                (0, 5, VariableType::Literal("number")),
                (2, 5, VariableType::Literal("string")),
                (3, 5, VariableType::Literal("boolean")),
                (5, 5, VariableType::Literal("number")),
                (
                    6,
                    5,
                    VariableType::Call {
                        line: 6,
                        column: column(text, 6, "make"),
                        awaited: false
                    }
                ),
                (
                    7,
                    7,
                    VariableType::Call {
                        line: 7,
                        column: column(text, 7, "load"),
                        awaited: true
                    }
                ),
                (10, 5, VariableType::Literal("bigint")),
            ]
        );
        assert!(variable_sites(Path::new("a.ts"), text, 8, 9).is_empty());
    }

    #[test]
    fn javascript_variables_get_literal_types_too() {
        let sites = variable_sites(Path::new("a.js"), "let a = 'x';\n", 0, 0);
        assert_eq!(sites[0].source, VariableType::Literal("string"));
    }

    #[test]
    fn reads_written_return_types_only_when_they_are_certain() {
        let text = "function a(): number { return 1; }\n\
                    function b<T>(x: T): T { return x; }\n\
                    class Box<T> { get(): T { return this.v; } }\n\
                    const c = (): string => 'x';\n\
                    async function d(): Promise<User> {}\n\
                    function e(x) {}\n\
                    function f(x): x is string { return true; }\n";
        let at = |line, needle: &str, awaited| {
            return_type_at(
                Path::new("a.ts"),
                text,
                line,
                column(text, line, needle),
                awaited,
            )
        };
        assert_eq!(at(0, "a(", false).as_deref(), Some("number"));
        assert_eq!(at(1, "b<", false), None);
        assert_eq!(at(2, "get", false), None);
        assert_eq!(at(3, "c =", false).as_deref(), Some("string"));
        assert_eq!(at(4, "d(", true).as_deref(), Some("User"));
        assert_eq!(at(4, "d(", false).as_deref(), Some("Promise<User>"));
        assert_eq!(at(5, "e(", false), None);
        assert_eq!(at(6, "f(", false), None);
    }
}
