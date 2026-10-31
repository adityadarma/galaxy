//! Names a TypeScript file adds to the global scope.

use super::*;

/// Global names declared by `text`. A script (no top-level import or export)
/// contributes every top-level declaration. A module contributes only its
/// `declare global { ... }` blocks and `export as namespace Name`.
pub fn ambient_global_names(path: impl AsRef<Path>, text: &str) -> Vec<String> {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_language(language_for_path(path.as_ref())))
        .is_err()
    {
        return Vec::new();
    }
    let Some(tree) = parser.parse(text, None) else {
        return Vec::new();
    };
    collect_global_names(tree.root_node(), text)
}

pub(super) fn collect_global_names(root: Node<'_>, text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut cursor = root.walk();
    let children = root.named_children(&mut cursor).collect::<Vec<_>>();
    let is_module = children
        .iter()
        .any(|node| matches!(node.kind(), "import_statement" | "export_statement"));
    for node in children {
        match node.kind() {
            "export_statement" => {
                if node_text(text, node)
                    .is_some_and(|source| source.trim_start().starts_with("export as namespace"))
                    && let Some(name) = node.named_child(0).and_then(|name| node_text(text, name))
                {
                    names.push(name.to_owned());
                }
            }
            "ambient_declaration" => collect_ambient(node, text, is_module, &mut names),
            _ if !is_module => collect_declaration_names(node, text, &mut names),
            _ => {}
        }
    }
    names
}

/// Exported declarations of every `declare module 'name' { ... }` block,
/// paired with the module name.
pub(super) fn collect_ambient_module_exports(
    root: Node<'_>,
    text: &str,
    locations: &Locations,
) -> Vec<(String, Declaration)> {
    let mut found = Vec::new();
    let mut cursor = root.walk();
    for node in root.named_children(&mut cursor) {
        if node.kind() != "ambient_declaration" {
            continue;
        }
        let mut children = node.walk();
        for module in node.named_children(&mut children) {
            if module.kind() != "module" {
                continue;
            }
            let Some(name) = module
                .child_by_field_name("name")
                .filter(|name| name.kind() == "string")
                .and_then(|name| node_text(text, name))
            else {
                continue;
            };
            let name = name.trim_matches(['\'', '"']);
            let Some(body) = module.child_by_field_name("body") else {
                continue;
            };
            let mut statements = body.walk();
            for statement in body.named_children(&mut statements) {
                if statement.kind() != "export_statement" {
                    continue;
                }
                let mut items = statement.walk();
                for item in statement.named_children(&mut items) {
                    for declaration in declarations_from_node(item, true, false, text, locations) {
                        found.push((name.to_owned(), declaration));
                    }
                }
            }
        }
    }
    found
}

fn collect_ambient(node: Node<'_>, text: &str, is_module: bool, names: &mut Vec<String>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            // `declare global { ... }`
            "statement_block" => collect_block_declarations(child, text, names),
            // `declare module "x" { global { ... } }`
            "module" => collect_module_globals(child, text, names),
            _ if !is_module => collect_declaration_names(child, text, names),
            _ => {}
        }
    }
}

fn collect_module_globals(module: Node<'_>, text: &str, names: &mut Vec<String>) {
    let Some(body) = module.child_by_field_name("body") else {
        return;
    };
    let mut cursor = body.walk();
    let children = body.named_children(&mut cursor).collect::<Vec<_>>();
    for (index, child) in children.iter().enumerate() {
        // The grammar reads `global {` inside a module body either as a
        // stray `global` followed by a block, or as a namespace named `global`.
        let after_global = child.kind() == "statement_block"
            && index > 0
            && node_text(text, children[index - 1]).is_some_and(|prev| prev.trim() == "global");
        if after_global {
            collect_block_declarations(*child, text, names);
        } else if child.kind() == "internal_module"
            && child
                .child_by_field_name("name")
                .and_then(|name| node_text(text, name))
                == Some("global")
            && let Some(block) = child.child_by_field_name("body")
        {
            collect_block_declarations(block, text, names);
        }
    }
}

fn collect_block_declarations(block: Node<'_>, text: &str, names: &mut Vec<String>) {
    let mut cursor = block.walk();
    for child in block.named_children(&mut cursor) {
        collect_declaration_names(child, text, names);
    }
}

fn collect_declaration_names(node: Node<'_>, text: &str, names: &mut Vec<String>) {
    match node.kind() {
        "interface_declaration"
        | "type_alias_declaration"
        | "class_declaration"
        | "abstract_class_declaration"
        | "function_declaration"
        | "function_signature"
        | "enum_declaration" => {
            if let Some(name) = node
                .child_by_field_name("name")
                .and_then(|name| node_text(text, name))
            {
                names.push(name.to_owned());
            }
        }
        "internal_module" | "module" => {
            // `namespace A.B {}` declares `A`; `declare module "x"` declares no name.
            let mut name = node.child_by_field_name("name");
            while let Some(current) = name.filter(|name| name.kind() == "nested_identifier") {
                name = current.child_by_field_name("object");
            }
            if let Some(name) = name
                .filter(|name| name.kind() == "identifier")
                .and_then(|name| node_text(text, name))
            {
                names.push(name.to_owned());
            }
        }
        "lexical_declaration" | "variable_declaration" => {
            let mut cursor = node.walk();
            for declarator in node
                .named_children(&mut cursor)
                .filter(|child| child.kind() == "variable_declarator")
            {
                if let Some(name) = declarator
                    .child_by_field_name("name")
                    .filter(|name| name.kind() == "identifier")
                    .and_then(|name| node_text(text, name))
                {
                    names.push(name.to_owned());
                }
            }
        }
        "export_statement" | "ambient_declaration" | "expression_statement" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_declaration_names(child, text, names);
            }
        }
        _ => {}
    }
}
