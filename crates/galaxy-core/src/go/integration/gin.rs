//! Gin framework integration.

use std::collections::HashSet;

use tree_sitter::{Node, Parser, Tree};

use super::{GoIntegration, GoIntegrationKind};
use crate::go::parse::{Pos, utf16_col};

const MODULE: &str = "github.com/gin-gonic/gin";
const ROUTE_METHODS: &[&str] = &[
    "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "Any",
];

/// A path supplied to a Gin routing call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GinRoutePath {
    /// A quoted Go string, without its delimiters.
    Literal(String),
    /// Source text for a non-literal expression.
    Dynamic(String),
}

/// A statically discovered Gin route registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GinRoute {
    /// The Gin method, such as `GET` or `Any`.
    pub method: String,
    /// The route path.
    pub path: GinRoutePath,
    /// Source text for the final handler expression.
    pub handler: String,
    /// Source text for handler expressions before [`Self::handler`].
    pub middleware: Vec<String>,
    /// Position of the complete call expression.
    pub pos: Pos,
}

/// Gin is a framework: project-aware route behavior stays isolated here.
pub(super) const fn integration() -> GoIntegration {
    GoIntegration {
        name: "Gin",
        kind: GoIntegrationKind::Framework,
        module: MODULE,
    }
}

/// Scans Gin route registrations in `src`.
pub fn scan_routes(src: &str) -> Vec<GinRoute> {
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

/// Scans Gin route registrations using an already parsed Go syntax tree.
pub fn scan_tree(src: &str, tree: &Tree) -> Vec<GinRoute> {
    let aliases = gin_aliases(tree.root_node(), src);
    if aliases.is_empty() {
        return Vec::new();
    }

    let mut nodes = Vec::new();
    collect_nodes(tree.root_node(), &mut nodes);
    nodes.sort_unstable_by_key(Node::start_byte);

    let mut receivers = HashSet::new();
    let mut routes = Vec::new();
    for node in nodes {
        match node.kind() {
            "short_var_declaration" | "assignment_statement" | "var_spec" => {
                if let Some((name, value)) = assignment(node, src)
                    && (is_gin_constructor(value, src, &aliases)
                        || is_group(value, src, &receivers))
                {
                    receivers.insert(name.to_owned());
                }
            }
            "call_expression" => {
                if let Some(route) = route(node, src, &receivers) {
                    routes.push(route);
                }
            }
            _ => {}
        }
    }
    routes
}

fn collect_nodes<'tree>(node: Node<'tree>, out: &mut Vec<Node<'tree>>) {
    if matches!(
        node.kind(),
        "short_var_declaration" | "assignment_statement" | "var_spec" | "call_expression"
    ) {
        out.push(node);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_nodes(child, out);
    }
}

fn gin_aliases(root: Node<'_>, src: &str) -> HashSet<String> {
    let mut specs = Vec::new();
    collect_kind(root, "import_spec", &mut specs);
    specs
        .into_iter()
        .filter_map(|spec| {
            let text = source(src, spec);
            let path = text.split_whitespace().last()?.trim_matches('"');
            (path == MODULE).then(|| {
                let alias = text.split_whitespace().next().unwrap_or_default();
                if alias.starts_with('"') {
                    "gin".to_owned()
                } else {
                    alias.to_owned()
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

fn assignment<'tree>(node: Node<'tree>, src: &'tree str) -> Option<(&'tree str, Node<'tree>)> {
    if node.kind() == "var_spec" {
        let mut cursor = node.walk();
        let name = node
            .named_children(&mut cursor)
            .find(|child| child.kind() == "identifier")?;
        let value = expression_lists(node).next()?.named_child(0)?;
        return Some((source(src, name), value));
    }
    let mut lists = expression_lists(node);
    let name = lists.next()?.named_child(0)?;
    let value = lists.next()?.named_child(0)?;
    (name.kind() == "identifier").then_some((source(src, name), value))
}

fn expression_lists(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| child.kind() == "expression_list")
        .collect::<Vec<_>>()
        .into_iter()
}

fn is_gin_constructor(value: Node<'_>, src: &str, aliases: &HashSet<String>) -> bool {
    callee(value, src).is_some_and(|(receiver, method)| {
        aliases.contains(receiver) && matches!(method, "New" | "Default")
    })
}

fn is_group(value: Node<'_>, src: &str, receivers: &HashSet<String>) -> bool {
    callee(value, src)
        .is_some_and(|(receiver, method)| method == "Group" && receivers.contains(receiver))
}

fn route(call: Node<'_>, src: &str, receivers: &HashSet<String>) -> Option<GinRoute> {
    let (receiver, method) = callee(call, src)?;
    if !receivers.contains(receiver) || !ROUTE_METHODS.contains(&method) {
        return None;
    }
    let arguments = arguments(call);
    let (route_path, handlers) = (arguments.first()?, arguments.get(1..)?);
    let (handler, middleware) = handlers.split_last()?;
    Some(GinRoute {
        method: method.to_owned(),
        path: path(*route_path, src),
        handler: source(src, *handler).to_owned(),
        middleware: middleware
            .iter()
            .map(|node| source(src, *node).to_owned())
            .collect(),
        pos: position(call, src),
    })
}

fn callee<'tree>(call: Node<'tree>, src: &'tree str) -> Option<(&'tree str, &'tree str)> {
    let function = call.child_by_field_name("function")?;
    (function.kind() == "selector_expression").then_some(())?;
    let receiver = function.child_by_field_name("operand")?;
    let method = function.child_by_field_name("field")?;
    (receiver.kind() == "identifier" && method.kind() == "field_identifier")
        .then_some((source(src, receiver), source(src, method)))
}

fn arguments(call: Node<'_>) -> Vec<Node<'_>> {
    call.child_by_field_name("arguments")
        .map_or_else(Vec::new, |args| {
            let mut cursor = args.walk();
            args.named_children(&mut cursor).collect()
        })
}

fn path(node: Node<'_>, src: &str) -> GinRoutePath {
    let text = source(src, node);
    if matches!(
        node.kind(),
        "interpreted_string_literal" | "raw_string_literal"
    ) {
        GinRoutePath::Literal(text[1..text.len().saturating_sub(1)].to_owned())
    } else {
        GinRoutePath::Dynamic(text.to_owned())
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
    fn scans_routes_from_constructors_and_groups() {
        let routes = scan_routes(
            r#"package main
import g "github.com/gin-gonic/gin"
func main() {
    app := g.Default()
    api := app.Group("/api", auth)
    api.GET("/users/:id", audit, users.Show)
    app.POST(prefix+"/users", create)
}"#,
        );
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[0].path, GinRoutePath::Literal("/users/:id".into()));
        assert_eq!(routes[0].middleware, ["audit"]);
        assert_eq!(routes[0].handler, "users.Show");
        assert_eq!(routes[0].pos.line, 5);
        assert_eq!(
            routes[1].path,
            GinRoutePath::Dynamic("prefix+\"/users\"".into())
        );
    }

    #[test]
    fn supports_route_methods_and_ignores_other_calls_and_receivers() {
        let methods = [
            "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "Any",
        ];
        let registrations = methods
            .iter()
            .map(|method| format!("app.{method}(\"/x\", handler)"))
            .chain([
                "app.Handle(\"GET\", \"/x\", handler)".into(),
                "app.Static(\"/x\", \"./x\")".into(),
                "app.Use(auth)".into(),
                "other.GET(\"/x\", handler)".into(),
            ])
            .collect::<Vec<_>>()
            .join("\n");
        let routes = scan_routes(&format!(
            "package main\nimport \"github.com/gin-gonic/gin\"\nfunc main() {{\napp := gin.New()\n{registrations}\n}}"
        ));
        assert_eq!(routes.len(), methods.len());
    }
}
