//! Inline hints for Dart: parameter names before call arguments, and the
//! type of `var`, `final` and `const` variables.
//!
//! The resolver is lexical, so a callee is used only when it resolves to
//! exactly one function or method, and a return type only when it is
//! written on a non-generic declaration.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tree_sitter::{Node, Tree};

use super::{DartEngine, Declaration, parse_tree, utf8_at, utf16};
use crate::lang::ItemKind;
use crate::parameter_hints::{self, Argument, Parameter, ParameterHint};

/// Longest type written in a label; longer ones would crowd the line.
const MAX_LABEL_CHARS: usize = 48;

type Parsed = Option<(String, Tree)>;

impl DartEngine {
    /// Hints for the calls and variables that touch `first..=last`.
    pub fn parameter_hints(
        &self,
        path: &Path,
        text: &str,
        first: u32,
        last: u32,
    ) -> Vec<ParameterHint> {
        let Some(tree) = parse_tree(text) else {
            return Vec::new();
        };
        let lines: Vec<&str> = text.split('\n').collect();
        let (first, last) = (first as usize, last as usize);
        let mut calls = Vec::new();
        let mut variables = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.end_position().row < first || node.start_position().row > last {
                continue;
            }
            match node.kind() {
                "call_expression" if calls.len() < parameter_hints::MAX_CALLS => calls.push(node),
                "initialized_variable_definition"
                    if variables.len() < parameter_hints::MAX_CALLS =>
                {
                    variables.push(node)
                }
                _ => {}
            }
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
        }

        let mut cache: HashMap<PathBuf, Parsed> = HashMap::new();
        let mut out = Vec::new();
        for call in calls {
            let Some((declaration, _)) = self.callee(path, text, call, &lines) else {
                continue;
            };
            let Some((source, target)) = self.target(&mut cache, path, text, &declaration) else {
                continue;
            };
            let Some(signature) = signature_of(source, target, &declaration) else {
                continue;
            };
            let Some(parameters) = positional_parameters(source, signature) else {
                continue;
            };
            let Some(arguments) = call_arguments(text, call, &lines) else {
                continue;
            };
            out.extend(parameter_hints::hints(&parameters, &arguments));
        }
        for variable in variables {
            if let Some(hint) = self.variable_hint(path, text, variable, &mut cache, &lines) {
                out.push(hint);
            }
        }
        out.sort_by_key(|hint| (hint.line, hint.column));
        out
    }

    /// The single function or method that `call` names, and its name node.
    fn callee<'t>(
        &self,
        path: &Path,
        text: &str,
        call: Node<'t>,
        lines: &[&str],
    ) -> Option<(Declaration, Node<'t>)> {
        let function = call.child_by_field_name("function")?;
        let name = match function.kind() {
            "identifier" => function,
            "member_expression" => function.child_by_field_name("property")?,
            _ => return None,
        };
        let word = name.utf8_text(text.as_bytes()).ok()?;
        let position = name.start_position();
        let column = column_of(lines, position.row, position.column);
        let declarations = self.resolve(path, text, position.row as u32, column, word);
        let [declaration] = declarations.as_slice() else {
            return None;
        };
        matches!(declaration.kind, ItemKind::Function | ItemKind::Method)
            .then(|| (declaration.clone(), name))
    }

    /// Source and syntax tree of the file holding `declaration`.
    fn target<'a>(
        &self,
        cache: &'a mut HashMap<PathBuf, Parsed>,
        path: &Path,
        text: &str,
        declaration: &Declaration,
    ) -> Option<(&'a str, &'a Tree)> {
        let entry = cache.entry(declaration.path.clone()).or_insert_with(|| {
            let source = if declaration.path == path {
                text.to_owned()
            } else {
                self.sources
                    .get(&declaration.path)
                    .cloned()
                    .or_else(|| std::fs::read_to_string(&declaration.path).ok())?
            };
            let tree = parse_tree(&source)?;
            Some((source, tree))
        });
        entry.as_ref().map(|(source, tree)| (source.as_str(), tree))
    }

    fn variable_hint(
        &self,
        path: &Path,
        text: &str,
        variable: Node<'_>,
        cache: &mut HashMap<PathBuf, Parsed>,
        lines: &[&str],
    ) -> Option<ParameterHint> {
        let name = variable.child_by_field_name("name")?;
        if name.kind() != "identifier" {
            return None;
        }
        // `int e = 2` already says what it is.
        let mut cursor = variable.walk();
        if variable
            .named_children(&mut cursor)
            .any(|child| child.kind() == "type")
        {
            return None;
        }
        let mut value = variable.child_by_field_name("value")?;
        let mut awaited = false;
        if value.kind() == "await_expression" {
            awaited = true;
            let mut inner = value.walk();
            value = value.named_children(&mut inner).next()?;
        }
        let label = if let Some(literal) = literal_type(text, value).filter(|_| !awaited) {
            literal.to_owned()
        } else {
            let (declaration, _) = self.callee(path, text, value, lines)?;
            let (source, tree) = self.target(cache, path, text, &declaration)?;
            let written = return_type(source, signature_of(source, tree, &declaration)?)?;
            if awaited {
                written
                    .strip_prefix("Future<")?
                    .strip_suffix('>')?
                    .trim()
                    .to_owned()
            } else {
                written
            }
        };
        let end = name.end_position();
        Some(ParameterHint {
            line: end.row as u32,
            column: column_of(lines, end.row, end.column),
            label: format!(": {label}"),
            return_type: false,
        })
    }
}

fn column_of(lines: &[&str], row: usize, byte: usize) -> u32 {
    let line = lines.get(row).copied().unwrap_or("");
    utf16(line.get(..byte).unwrap_or(line))
}

/// The type of a literal. Strings, numbers and booleans cannot be anything
/// else when the declaration has no type.
fn literal_type(text: &str, value: Node<'_>) -> Option<&'static str> {
    match value.kind() {
        "string_literal" => Some("String"),
        "decimal_integer_literal" | "hex_integer_literal" => Some("int"),
        "decimal_floating_point_literal" => Some("double"),
        "true" | "false" => Some("bool"),
        "unary_expression" if text.get(value.start_byte()..)?.starts_with('-') => {
            let mut cursor = value.walk();
            let operand = value.named_children(&mut cursor).last()?;
            matches!(
                operand.kind(),
                "decimal_integer_literal" | "decimal_floating_point_literal"
            )
            .then(|| literal_type(text, operand))
            .flatten()
        }
        _ => None,
    }
}

/// The signature node of the function declared at `declaration`.
fn signature_of<'t>(source: &str, tree: &'t Tree, declaration: &Declaration) -> Option<Node<'t>> {
    let mut start = 0;
    let mut line_text = "";
    for (index, line) in source.split('\n').enumerate() {
        if index == declaration.line as usize {
            line_text = line;
            break;
        }
        start += line.len() + 1;
    }
    let byte = start + utf8_at(line_text, declaration.column);
    let mut node = tree.root_node().descendant_for_byte_range(byte, byte)?;
    for _ in 0..4 {
        if node.kind() == "function_signature" || node.child_by_field_name("parameters").is_some() {
            return Some(node);
        }
        node = node.parent()?;
    }
    None
}

/// The parameters an argument can fill by position. `{named}` parameters
/// are passed with their label, so they end the list.
fn positional_parameters(source: &str, signature: Node<'_>) -> Option<Vec<Parameter>> {
    let list = signature.child_by_field_name("parameters")?;
    let mut out = Vec::new();
    let mut cursor = list.walk();
    for child in list.named_children(&mut cursor) {
        match child.kind() {
            "formal_parameter" => out.push(parameter(source, child)),
            "optional_formal_parameters" => {
                if !source.get(child.start_byte()..)?.starts_with('[') {
                    break;
                }
                let mut inner = child.walk();
                for optional in child.named_children(&mut inner) {
                    out.push(parameter(source, optional));
                }
            }
            _ => out.push(Parameter {
                name: String::new(),
                rest: false,
            }),
        }
    }
    Some(out)
}

fn parameter(source: &str, node: Node<'_>) -> Parameter {
    let name = node
        .child_by_field_name("name")
        .filter(|name| name.kind() == "identifier")
        .and_then(|name| name.utf8_text(source.as_bytes()).ok())
        .unwrap_or("");
    Parameter {
        name: name.to_owned(),
        rest: false,
    }
}

fn call_arguments(text: &str, call: Node<'_>, lines: &[&str]) -> Option<Vec<Argument>> {
    let list = call.child_by_field_name("arguments")?;
    let mut cursor = list.walk();
    let arguments: Vec<_> = list
        .named_children(&mut cursor)
        .filter(|argument| !matches!(argument.kind(), "comment" | "documentation_comment"))
        .map(|argument| {
            let named = match argument.kind() {
                "identifier" => Some(argument),
                "member_expression" => argument.child_by_field_name("property"),
                _ => None,
            };
            let start = argument.start_position();
            Argument {
                line: start.row as u32,
                column: column_of(lines, start.row, start.column),
                name: named
                    .and_then(|node| node.utf8_text(text.as_bytes()).ok())
                    .map(str::to_owned),
                spread: false,
                named: argument.kind() == "named_argument",
            }
        })
        .collect();
    (!arguments.is_empty()).then_some(arguments)
}

/// The return type written on `signature`, unless it can mention a type
/// parameter of the function or its class.
fn return_type(source: &str, signature: Node<'_>) -> Option<String> {
    let written = signature
        .child_by_field_name("return_type")?
        .utf8_text(source.as_bytes())
        .ok()?
        .trim();
    let mut owner = Some(signature);
    while let Some(node) = owner {
        let mut cursor = node.walk();
        if node
            .named_children(&mut cursor)
            .any(|child| child.kind() == "type_parameters")
        {
            return None;
        }
        owner = node.parent();
    }
    let unusable = written.is_empty()
        || written == "void"
        || written == "dynamic"
        || written.contains('\n')
        || written.chars().count() > MAX_LABEL_CHARS;
    (!unusable).then(|| written.to_owned())
}
