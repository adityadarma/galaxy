//! Call sites and declared parameters, for inline parameter hints.

use super::*;
use crate::parameter_hints::{Argument, Parameter};

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
        | "construct_signature" => formal_parameters(text, node),
        "variable_declarator" | "public_field_definition" | "pair" | "property_signature" => {
            let value = node.child_by_field_name("value").or_else(|| {
                // `run: (a: string) => void` is typed, not assigned.
                let annotation = node.child_by_field_name("type")?;
                let mut cursor = annotation.walk();
                annotation.named_children(&mut cursor).next()
            })?;
            callable_parameters(text, value)
        }
        "lexical_declaration" | "variable_declaration" => {
            let mut cursor = node.walk();
            let declarator = node
                .named_children(&mut cursor)
                .find(|child| child.kind() == "variable_declarator")?;
            callable_parameters(text, declarator)
        }
        "export_statement" => callable_parameters(text, node.child_by_field_name("declaration")?),
        // `declare function f(a: string): void;`
        "ambient_declaration" => {
            let mut cursor = node.walk();
            let inner = node.named_children(&mut cursor).next()?;
            callable_parameters(text, inner)
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
            formal_parameters(text, constructor)
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
}
