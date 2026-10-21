//! Goravel project integration.

use std::collections::HashSet;

use tree_sitter::{Node, Parser, Tree};

use super::{GoIntegration, GoIntegrationKind};
use crate::go::parse::{Pos, utf16_col};

const FACADES_MODULE: &str = "github.com/goravel/framework/facades";
const ROUTE_METHODS: &[&str] = &["Get", "Post", "Put", "Patch", "Delete", "Any"];

/// A path supplied to a Goravel routing call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoravelRoutePath {
    /// A quoted Go string, without its delimiters.
    Literal(String),
    /// Source text for a non-literal expression.
    Dynamic(String),
}

/// A statically discovered Goravel route registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoravelRoute {
    /// The Goravel route method, such as `Get` or `Any`.
    pub method: String,
    /// The raw route path.
    pub path: GoravelRoutePath,
    /// Source text for the final handler expression.
    pub handler: String,
    /// Position of the complete call expression.
    pub pos: Pos,
}

/// A statically discovered Goravel configuration value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoravelConfigEntry {
    /// Dot-separated literal keys passed to `Config().Add` and nested maps.
    pub key: String,
    /// Position of the literal map value.
    pub pos: Pos,
    /// Source text for the literal value.
    pub value: String,
}

/// Goravel is a framework: future behavior belongs here rather than in the
/// generic resolver because it depends on project conventions.
pub(super) const fn integration() -> GoIntegration {
    GoIntegration {
        name: "Goravel",
        kind: GoIntegrationKind::Framework,
        module: "github.com/goravel/framework",
    }
}

/// Scans Goravel facade route registrations in `src`.
pub fn scan_routes(src: &str) -> Vec<GoravelRoute> {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .is_err()
    {
        return Vec::new();
    }
    parser
        .parse(src, None)
        .map_or_else(Vec::new, |tree| scan_tree(src, &tree))
}

/// Scans literal configuration maps added through a Goravel config facade.
pub fn scan_config(src: &str) -> Vec<GoravelConfigEntry> {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .is_err()
    {
        return Vec::new();
    }
    parser
        .parse(src, None)
        .map_or_else(Vec::new, |tree| scan_config_tree(src, &tree))
}

/// Scans Goravel facade route registrations using an already parsed Go syntax tree.
pub fn scan_tree(src: &str, tree: &Tree) -> Vec<GoravelRoute> {
    let aliases = facade_aliases(tree.root_node(), src);
    if aliases.is_empty() {
        return Vec::new();
    }

    let mut calls = Vec::new();
    collect_calls(tree.root_node(), &mut calls);
    calls.sort_unstable_by_key(Node::start_byte);
    calls
        .into_iter()
        .filter_map(|call| route(call, src, &aliases))
        .collect()
}

/// Scans literal configuration maps using an already parsed Go syntax tree.
pub fn scan_config_tree(src: &str, tree: &Tree) -> Vec<GoravelConfigEntry> {
    let aliases = facade_aliases(tree.root_node(), src);
    if aliases.is_empty() {
        return Vec::new();
    }

    let mut nodes = Vec::new();
    collect_config_nodes(tree.root_node(), &mut nodes);
    nodes.sort_unstable_by_key(Node::start_byte);

    let mut configs = HashSet::new();
    let mut entries = Vec::new();
    for node in nodes {
        match node.kind() {
            "short_var_declaration" => {
                if let Some((name, value)) = assignment(node)
                    && is_facade_config(value, src, &aliases)
                {
                    configs.insert(source(src, name).to_owned());
                }
            }
            "call_expression" => entries.extend(config_entries(node, src, &configs)),
            _ => {}
        }
    }
    entries
}

fn facade_aliases(root: Node<'_>, src: &str) -> HashSet<String> {
    let mut specs = Vec::new();
    collect_kind(root, "import_spec", &mut specs);
    specs
        .into_iter()
        .filter_map(|spec| {
            let text = source(src, spec);
            let path = text.split_whitespace().last()?.trim_matches('"');
            (path == FACADES_MODULE).then(|| {
                let first = text.split_whitespace().next().unwrap_or_default();
                if first.starts_with('"') {
                    "facades".to_owned()
                } else {
                    first.to_owned()
                }
            })
        })
        .filter(|alias| !alias.is_empty() && alias != "_" && alias != ".")
        .collect()
}

fn collect_kind<'tree>(node: Node<'tree>, kind: &str, out: &mut Vec<Node<'tree>>) {
    if node.kind() == kind {
        out.push(node);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_kind(child, kind, out);
    }
}

fn collect_calls<'tree>(node: Node<'tree>, out: &mut Vec<Node<'tree>>) {
    if node.kind() == "call_expression" {
        out.push(node);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_calls(child, out);
    }
}

fn collect_config_nodes<'tree>(node: Node<'tree>, out: &mut Vec<Node<'tree>>) {
    if matches!(node.kind(), "short_var_declaration" | "call_expression") {
        out.push(node);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_config_nodes(child, out);
    }
}

fn assignment(node: Node<'_>) -> Option<(Node<'_>, Node<'_>)> {
    let mut cursor = node.walk();
    let mut lists = node
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "expression_list");
    let name = lists.next()?.named_child(0)?;
    let value = lists.next()?.named_child(0)?;
    (name.kind() == "identifier").then_some((name, value))
}

fn is_facade_config(value: Node<'_>, src: &str, aliases: &HashSet<String>) -> bool {
    selector(value, src).is_some_and(|(receiver, method)| {
        method == "Config"
            && receiver.kind() == "identifier"
            && aliases.contains(source(src, receiver))
    })
}

fn config_entries(call: Node<'_>, src: &str, configs: &HashSet<String>) -> Vec<GoravelConfigEntry> {
    let Some((receiver, method)) = selector(call, src) else {
        return Vec::new();
    };
    if method != "Add"
        || receiver.kind() != "identifier"
        || !configs.contains(source(src, receiver))
    {
        return Vec::new();
    }
    let arguments = arguments(call);
    let Some((namespace, value)) = arguments.first().zip(arguments.get(1)) else {
        return Vec::new();
    };
    let Some(namespace) = string_literal(*namespace, src) else {
        return Vec::new();
    };
    map_entries(*value, src, &namespace)
}

fn map_entries(map: Node<'_>, src: &str, prefix: &str) -> Vec<GoravelConfigEntry> {
    let map = literal_element(map);
    if map.kind() != "composite_literal"
        || map
            .child_by_field_name("type")
            .is_none_or(|ty| ty.kind() != "map_type")
    {
        return Vec::new();
    }
    let Some(values) = map.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    let mut cursor = values.walk();
    for element in values.named_children(&mut cursor) {
        if element.kind() != "keyed_element" {
            continue;
        }
        let Some(key) = element
            .named_child(0)
            .map(literal_element)
            .and_then(|key| string_literal(key, src))
        else {
            continue;
        };
        let Some(value) = element.named_child(1).map(literal_element) else {
            continue;
        };
        let key = format!("{prefix}.{key}");
        let nested = map_entries(value, src, &key);
        if nested.is_empty() {
            entries.push(GoravelConfigEntry {
                key,
                pos: position(value, src),
                value: source(src, value).to_owned(),
            });
        } else {
            entries.extend(nested);
        }
    }
    entries
}

fn string_literal(node: Node<'_>, src: &str) -> Option<String> {
    matches!(
        node.kind(),
        "interpreted_string_literal" | "raw_string_literal"
    )
    .then(|| source(src, node))
    .map(|text| text[1..text.len().saturating_sub(1)].to_owned())
}

fn literal_element(node: Node<'_>) -> Node<'_> {
    (node.kind() == "literal_element")
        .then(|| node.named_child(0))
        .flatten()
        .unwrap_or(node)
}

fn route(call: Node<'_>, src: &str, aliases: &HashSet<String>) -> Option<GoravelRoute> {
    let (receiver, method) = selector(call, src)?;
    if !ROUTE_METHODS.contains(&method) || !is_route_receiver(receiver, src, aliases) {
        return None;
    }
    let arguments = arguments(call);
    let (path_node, handler) = arguments.split_first()?;
    let handler = handler.last()?;
    Some(GoravelRoute {
        method: method.to_owned(),
        path: path(*path_node, src),
        handler: source(src, *handler).to_owned(),
        pos: position(call, src),
    })
}

/// Accept `facades.Route()` and any chain of `Group` calls rooted in it.
fn is_route_receiver(node: Node<'_>, src: &str, aliases: &HashSet<String>) -> bool {
    let Some((receiver, method)) = selector(node, src) else {
        return false;
    };
    if method == "Route" {
        return receiver.kind() == "identifier" && aliases.contains(source(src, receiver));
    }
    method == "Group" && is_route_receiver(receiver, src, aliases)
}

fn selector<'tree>(call: Node<'tree>, src: &'tree str) -> Option<(Node<'tree>, &'tree str)> {
    (call.kind() == "call_expression").then_some(())?;
    let function = call.child_by_field_name("function")?;
    (function.kind() == "selector_expression").then_some(())?;
    let receiver = function.child_by_field_name("operand")?;
    let method = function.child_by_field_name("field")?;
    (method.kind() == "field_identifier").then_some((receiver, source(src, method)))
}

fn arguments(call: Node<'_>) -> Vec<Node<'_>> {
    let args = call.child_by_field_name("arguments");
    args.map_or_else(Vec::new, |args| {
        let mut cursor = args.walk();
        args.named_children(&mut cursor).collect()
    })
}

fn path(node: Node<'_>, src: &str) -> GoravelRoutePath {
    let text = source(src, node);
    if matches!(
        node.kind(),
        "interpreted_string_literal" | "raw_string_literal"
    ) {
        GoravelRoutePath::Literal(text[1..text.len().saturating_sub(1)].to_owned())
    } else {
        GoravelRoutePath::Dynamic(text.to_owned())
    }
}

fn position(node: Node<'_>, src: &str) -> Pos {
    let point = node.start_position();
    let line_start = node.start_byte().saturating_sub(point.column);
    let end = node.end_position();
    let end_line_start = node.end_byte().saturating_sub(end.column);
    Pos {
        line: point.row as u32,
        column: utf16_col(src.get(line_start..).unwrap_or(""), point.column),
        end_column: utf16_col(src.get(end_line_start..).unwrap_or(""), end.column),
    }
}

fn source<'a>(src: &'a str, node: Node<'_>) -> &'a str {
    src.get(node.byte_range()).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_facade_routes_and_preserves_constructor_handlers() {
        let routes = scan_routes(
            r#"package routes
import "github.com/goravel/framework/facades"
func Web() {
    userController := controllers.NewUserController()
    facades.Route().Get("/users", userController.Index)
    facades.Route().Post(prefix+"/users", userController.Store)
}"#,
        );
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].method, "Get");
        assert_eq!(routes[0].path, GoravelRoutePath::Literal("/users".into()));
        assert_eq!(routes[0].handler, "userController.Index");
        assert_eq!(routes[0].pos.line, 4);
        assert_eq!(
            routes[1].path,
            GoravelRoutePath::Dynamic("prefix+\"/users\"".into())
        );
    }

    #[test]
    fn scans_aliases_methods_and_chained_groups() {
        let methods = ["Get", "Post", "Put", "Patch", "Delete", "Any"];
        let registrations = methods
            .iter()
            .map(|method| format!("route.Route().Group(\"/api\").{method}(\"/x\", handler)"))
            .collect::<Vec<_>>()
            .join("\n");
        let routes = scan_routes(&format!(
            "package routes\nimport route \"github.com/goravel/framework/facades\"\nfunc Web() {{\n{registrations}\n}}"
        ));
        assert_eq!(routes.len(), methods.len());
        assert_eq!(
            routes
                .iter()
                .map(|route| route.method.as_str())
                .collect::<Vec<_>>(),
            methods
        );
        assert!(routes.iter().all(|route| route.handler == "handler"));
    }

    #[test]
    fn ignores_unrelated_route_calls() {
        assert!(
            scan_routes("package routes\nfunc Web() { facades.Route().Get(\"/x\", handler) }")
                .is_empty()
        );
    }

    #[test]
    fn scans_nested_literal_config_maps() {
        let entries = scan_config(
            r#"package config
import "github.com/goravel/framework/facades"
func Config() {
    config := facades.Config()
    config.Add("app", map[string]any{
        "name": "Galaxy",
        "database": map[string]any{
            "host": "localhost",
            "port": 5432,
        },
    })
}"#,
        );
        assert_eq!(
            entries,
            vec![
                GoravelConfigEntry {
                    key: "app.name".into(),
                    pos: Pos {
                        line: 5,
                        column: 16,
                        end_column: 24,
                    },
                    value: "\"Galaxy\"".into(),
                },
                GoravelConfigEntry {
                    key: "app.database.host".into(),
                    pos: Pos {
                        line: 7,
                        column: 20,
                        end_column: 31,
                    },
                    value: "\"localhost\"".into(),
                },
                GoravelConfigEntry {
                    key: "app.database.port".into(),
                    pos: Pos {
                        line: 8,
                        column: 20,
                        end_column: 24,
                    },
                    value: "5432".into(),
                },
            ]
        );
    }

    #[test]
    fn scans_aliased_config_facade() {
        let entries = scan_config(
            r#"package config
import facade "github.com/goravel/framework/facades"
func Config() {
    config := facade.Config()
    config.Add("cache", map[string]any{"driver": "memory"})
}"#,
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "cache.driver");
        assert_eq!(entries[0].value, "\"memory\"");
    }

    #[test]
    fn ignores_unrelated_calls_and_dynamic_map_keys() {
        let entries = scan_config(
            r#"package config
import "github.com/goravel/framework/facades"
func Config() {
    config := facades.Config()
    other := facades.Config()
    values := map[string]any{"name": "dynamic"}
    config.Add("app", values)
    other.Add(namespace, map[string]any{"name": "dynamic"})
    config.Add("app", map[string]any{key: "dynamic", "name": "static"})
    unknown.Add("app", map[string]any{"name": "ignored"})
}"#,
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "app.name");
        assert_eq!(entries[0].value, "\"static\"");
    }
}
