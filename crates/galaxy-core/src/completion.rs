//! Same-file completions for Python and Swift. No external language server.

use std::collections::HashSet;

use tree_sitter::{Node, Parser};

use crate::lang::{Completion, ItemKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Python,
    Swift,
}

/// Names declared in the current file, with locals limited to the enclosing scope.
pub fn complete(language: Language, text: &str, line: u32, column: u32) -> Vec<Completion> {
    let Some(byte) = byte_offset(text, line, column) else {
        return Vec::new();
    };
    let start = text[..byte]
        .rfind(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .map_or(0, |index| index + 1);
    let prefix = &text[start..byte];
    if prefix.is_empty() || text[..start].ends_with('.') {
        return Vec::new();
    }

    let mut parser = Parser::new();
    let grammar = match language {
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::Swift => tree_sitter_swift::LANGUAGE.into(),
    };
    if parser.set_language(&grammar).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(text, None) else {
        return Vec::new();
    };
    let root = tree.root_node();
    // Do not offer identifier completions inside comments or literals.
    let mut at = root.descendant_for_byte_range(byte.saturating_sub(1), byte);
    while let Some(node) = at {
        if node.kind().contains("comment") || node.kind().contains("string") {
            return Vec::new();
        }
        at = node.parent();
    }

    let mut items = Vec::new();
    let mut seen = HashSet::new();
    let context = CollectContext {
        root,
        byte,
        prefix,
        language,
        text,
    };
    collect(root, &context, &mut seen, &mut items);
    items.sort_by(|a, b| a.sort.cmp(&b.sort));
    items.truncate(200);
    items
}

#[derive(Clone, Copy)]
struct CollectContext<'tree, 'text> {
    root: Node<'tree>,
    byte: usize,
    prefix: &'text str,
    language: Language,
    text: &'text str,
}

fn collect(
    node: Node<'_>,
    context: &CollectContext<'_, '_>,
    seen: &mut HashSet<String>,
    items: &mut Vec<Completion>,
) {
    let CollectContext {
        root,
        byte,
        prefix,
        language,
        text,
    } = *context;
    if node.start_byte() > byte {
        return;
    }
    let scope = if node.kind() == "assignment"
        && node
            .parent()
            .is_some_and(|parent| parent.kind() == "expression_statement")
    {
        node.parent()
            .and_then(|parent| parent.parent())
            .unwrap_or(root)
    } else {
        node.parent().unwrap_or(root)
    };
    let visible = scope == root || (scope.start_byte() <= byte && byte <= scope.end_byte());
    let mut add = |name: Node<'_>, kind: ItemKind| {
        if name.start_byte() >= byte || !visible {
            return;
        }
        let Some(label) = text.get(name.byte_range()) else {
            return;
        };
        if !label.starts_with(prefix) || label == prefix || !seen.insert(label.to_owned()) {
            return;
        }
        items.push(Completion {
            label: label.to_owned(),
            detail: None,
            insert_text: label.to_owned(),
            is_snippet: false,
            kind,
            sort: label.to_owned(),
            extra: Vec::new(),
            replace_from: None,
        });
    };
    match (language, node.kind()) {
        (Language::Python, "class_definition") => {
            if let Some(name) = node.child_by_field_name("name") {
                add(name, ItemKind::Class);
            }
        }
        (Language::Python, "function_definition") => {
            if let Some(name) = node.child_by_field_name("name") {
                add(name, ItemKind::Function);
            }
        }
        (Language::Python, "assignment") if node.start_byte() < byte => {
            if let Some(left) = node.child_by_field_name("left") {
                let name = if left.kind() == "identifier" {
                    Some(left)
                } else if left.kind() == "pattern" {
                    left.named_child(0)
                        .filter(|child| child.kind() == "identifier")
                } else {
                    None
                };
                if let Some(name) = name {
                    add(name, ItemKind::Variable);
                }
            }
        }
        (Language::Python, "parameters") => {
            let mut cursor = node.walk();
            for parameter in node.named_children(&mut cursor) {
                let name = match parameter.kind() {
                    "identifier" => Some(parameter),
                    "default_parameter" | "typed_default_parameter" => {
                        parameter.child_by_field_name("name")
                    }
                    "typed_parameter" => parameter
                        .named_child(0)
                        .filter(|child| child.kind() == "identifier"),
                    _ => None,
                };
                if let Some(name) = name {
                    add(name, ItemKind::Variable);
                }
            }
        }
        (Language::Swift, "class_declaration" | "protocol_declaration") => {
            if let Some(name) = node.child_by_field_name("name") {
                let kind = if node.kind() == "protocol_declaration" {
                    ItemKind::Interface
                } else {
                    ItemKind::Class
                };
                add(name, kind);
            }
        }
        (Language::Swift, "function_declaration") => {
            if let Some(name) = node.child_by_field_name("name")
                && name.kind() == "simple_identifier"
            {
                add(name, ItemKind::Function);
            }
        }
        (Language::Swift, "property_declaration") => {
            if let Some(pattern) = node.child_by_field_name("name") {
                if pattern.kind() == "simple_identifier" {
                    add(pattern, ItemKind::Variable);
                }
                let mut cursor = pattern.walk();
                for child in pattern.named_children(&mut cursor) {
                    if child.kind() == "simple_identifier" {
                        add(child, ItemKind::Variable);
                    } else if child.kind() == "pattern"
                        && let Some(name) = child.child_by_field_name("bound_identifier")
                    {
                        add(name, ItemKind::Variable);
                    }
                }
            }
        }
        _ => {}
    }
    // Do not leak a different function's or class's locals into this scope.
    let nested_scope = matches!(
        node.kind(),
        "function_definition"
            | "function_declaration"
            | "class_definition"
            | "class_declaration"
            | "protocol_declaration"
    );
    if nested_scope && !(node.start_byte() <= byte && byte <= node.end_byte()) {
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect(child, context, seen, items);
    }
}

fn byte_offset(text: &str, line: u32, column: u32) -> Option<usize> {
    let mut start = 0;
    for _ in 0..line {
        start += text[start..].find('\n')? + 1;
    }
    let rest = &text[start..];
    let line = &rest[..rest.find('\n').unwrap_or(rest.len())];
    let mut units = 0;
    for (index, ch) in line.char_indices() {
        if units == column {
            return Some(start + index);
        }
        units += ch.len_utf16() as u32;
        if units > column {
            return None;
        }
    }
    (units == column).then_some(start + line.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(language: Language, text: &str, line: u32, column: u32) -> Vec<String> {
        complete(language, text, line, column)
            .into_iter()
            .map(|item| item.label)
            .collect()
    }

    #[test]
    fn python_completion_uses_visible_names_and_exact_prefix() {
        let text = "class Logger:\n    pass\ndef log_info():\n    pass\nlog_count = 2\nlo";
        assert_eq!(
            labels(Language::Python, text, 5, 2),
            ["log_count", "log_info"]
        );
        assert!(labels(Language::Python, text, 5, 0).is_empty());
        assert_eq!(
            labels(Language::Python, "class Logger:\n    pass\nLog", 2, 3),
            ["Logger"]
        );
        assert_eq!(
            labels(Language::Python, text, 5, 1),
            ["log_count", "log_info"]
        );
    }

    #[test]
    fn swift_completion_uses_same_file_declarations() {
        let text = "struct Logger {}\nfunc logInfo() {}\nlet logCount = 2\nlo";
        assert_eq!(labels(Language::Swift, text, 3, 2), ["logCount", "logInfo"]);
        assert_eq!(
            labels(Language::Swift, "struct Logger {}\nLog", 1, 3),
            ["Logger"]
        );
    }

    #[test]
    fn comments_and_member_access_do_not_suggest_globals() {
        assert!(labels(Language::Python, "log = 1\n# lo", 1, 4).is_empty());
        assert!(labels(Language::Python, "log = 1\nobj.lo", 1, 6).is_empty());
        assert!(labels(Language::Swift, "let log = 1\nobj.lo", 1, 6).is_empty());
    }

    #[test]
    fn python_parameters_and_locals_stay_in_their_function() {
        let text = "def first(local_name):\n    local_value = 1\n    loc\ndef second():\n    loc";
        assert_eq!(
            labels(Language::Python, text, 2, 7),
            ["local_name", "local_value"]
        );
        assert!(labels(Language::Python, text, 4, 7).is_empty());
    }
}
