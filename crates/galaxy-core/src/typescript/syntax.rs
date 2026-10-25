//! Tree-sitter helpers shared by the other modules.

use super::*;

pub(super) fn contains(range: Utf16Range, position: Utf16Position) -> bool {
    range.start <= position && position < range.end
}

pub(super) fn language_for_path(path: &Path) -> DocumentLanguage {
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".d.ts"))
    {
        return DocumentLanguage::TypeScript;
    }
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("tsx") => DocumentLanguage::Tsx,
        Some("ts" | "mts" | "cts") => DocumentLanguage::TypeScript,
        _ => DocumentLanguage::JavaScript,
    }
}

pub(super) fn tree_sitter_language(language: DocumentLanguage) -> Language {
    match language {
        DocumentLanguage::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        DocumentLanguage::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        DocumentLanguage::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
    }
}

pub(super) fn has_keyword(node: Node<'_>, text: &str, keyword: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| node_text(text, child) == Some(keyword))
}

pub(super) fn identifiers(node: Node<'_>, text: &str) -> Vec<String> {
    let mut identifiers = Vec::new();
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier" {
            if let Some(name) = node_text(text, node) {
                identifiers.push(name.to_owned());
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    identifiers.reverse();
    identifiers
}

pub(super) fn identifier_nodes<'a>(node: Node<'a>, text: &str) -> Vec<Node<'a>> {
    let mut identifiers = Vec::new();
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier" {
            if node_text(text, node).is_some() {
                identifiers.push(node);
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    identifiers.reverse();
    identifiers
}

pub(super) fn last_identifier_node<'a>(node: Node<'a>, text: &str) -> Option<Node<'a>> {
    let mut identifiers = Vec::new();
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier" {
            if node_text(text, node).is_some() {
                identifiers.push(node);
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    identifiers.reverse();
    identifiers.pop()
}

pub(super) fn node_text<'a>(text: &'a str, node: Node<'_>) -> Option<&'a str> {
    text.get(node.start_byte()..node.end_byte())
}
