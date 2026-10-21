//! Fiber library integration.

use std::collections::HashSet;

use tree_sitter::{Node, Parser, Tree};

use super::{GoIntegration, GoIntegrationKind};
use crate::go::parse::{Pos, utf16_col};

/// A path supplied to a Fiber routing call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FiberRoutePath {
    /// A quoted Go string, without its delimiters.
    Literal(String),
    /// Source text for a non-literal expression.
    Dynamic(String),
}

/// A statically discovered Fiber route or middleware registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FiberRoute {
    /// The Fiber method, including `Get`, `Use`, and `Static`.
    pub method: String,
    /// The route path when the call accepts one. `Use(handler)` has no path.
    pub path: Option<FiberRoutePath>,
    /// Source text for the final handler expression.
    pub handler: String,
    /// Source text for handler expressions before [`Self::handler`].
    pub middleware: Vec<String>,
    /// Position of the complete call expression.
    pub pos: Pos,
}

const ROUTE_METHODS: &[&str] = &[
    "Get", "Post", "Put", "Patch", "Delete", "Head", "Options", "All", "Use", "Static",
];

/// Fiber is a library: normal Go package resolution remains the primary API
/// source, while library-specific behavior stays isolated here.
pub(super) const fn v2() -> GoIntegration {
    GoIntegration {
        name: "Fiber",
        kind: GoIntegrationKind::Library,
        module: "github.com/gofiber/fiber/v2",
    }
}

pub(super) const fn v3() -> GoIntegration {
    GoIntegration {
        name: "Fiber",
        kind: GoIntegrationKind::Library,
        module: "github.com/gofiber/fiber/v3",
    }
}

/// Scans Fiber registrations in `src`.
///
/// The scanner is intentionally syntactic: a receiver becomes known after a
/// `fiber.New()` assignment or assignment from `Group`, then only calls on
/// those variables are reported.
pub fn scan_routes(src: &str) -> Vec<FiberRoute> {
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

/// Scans Fiber registrations using an already parsed Go syntax tree.
pub fn scan_tree(src: &str, tree: &Tree) -> Vec<FiberRoute> {
    let root = tree.root_node();
    let fiber_aliases = fiber_aliases(root, src);
    if fiber_aliases.is_empty() {
        return Vec::new();
    }

    let mut nodes = Vec::new();
    collect_nodes(root, &mut nodes);
    nodes.sort_unstable_by_key(Node::start_byte);

    let mut receivers = HashSet::new();
    let mut routes = Vec::new();
    for node in nodes {
        match node.kind() {
            "short_var_declaration" | "assignment_statement" | "var_spec" => {
                if let Some((name, value)) = assignment(node, src)
                    && (is_fiber_new(value, src, &fiber_aliases)
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

fn fiber_aliases(root: Node<'_>, src: &str) -> HashSet<String> {
    let mut specs = Vec::new();
    collect_kind(root, "import_spec", &mut specs);
    specs
        .into_iter()
        .filter_map(|spec| {
            let text = source(src, spec);
            let path = text.split_whitespace().last()?.trim_matches('"');
            if !matches!(
                path,
                "github.com/gofiber/fiber/v2" | "github.com/gofiber/fiber/v3"
            ) {
                return None;
            }
            let alias = text.split_whitespace().next()?;
            Some(if alias.starts_with('"') {
                "fiber".to_owned()
            } else {
                alias.to_owned()
            })
        })
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

fn is_fiber_new(value: Node<'_>, src: &str, aliases: &HashSet<String>) -> bool {
    callee(value, src)
        .is_some_and(|(receiver, method)| method == "New" && aliases.contains(receiver))
}

fn is_group(value: Node<'_>, src: &str, receivers: &HashSet<String>) -> bool {
    callee(value, src)
        .is_some_and(|(receiver, method)| method == "Group" && receivers.contains(receiver))
}

fn route(call: Node<'_>, src: &str, receivers: &HashSet<String>) -> Option<FiberRoute> {
    let (receiver, method) = callee(call, src)?;
    if !receivers.contains(receiver) || !ROUTE_METHODS.contains(&method) {
        return None;
    }
    let arguments = arguments(call);
    let (path, handlers) = if method == "Use" && !arguments.first().is_some_and(is_path) {
        (None, arguments.as_slice())
    } else {
        (
            arguments.first().map(|node| path(*node, src)),
            arguments.get(1..)?,
        )
    };
    let (handler, middleware) = handlers.split_last()?;
    Some(FiberRoute {
        method: method.to_owned(),
        path,
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
    let args = call.child_by_field_name("arguments");
    args.map_or_else(Vec::new, |args| {
        let mut cursor = args.walk();
        args.named_children(&mut cursor).collect()
    })
}

fn is_path(node: &Node<'_>) -> bool {
    matches!(
        node.kind(),
        "interpreted_string_literal" | "raw_string_literal"
    )
}

fn path(node: Node<'_>, src: &str) -> FiberRoutePath {
    let text = source(src, node);
    if is_path(&node) {
        FiberRoutePath::Literal(text[1..text.len().saturating_sub(1)].to_owned())
    } else {
        FiberRoutePath::Dynamic(text.to_owned())
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
    fn scans_grouped_routes_with_middleware() {
        let routes = scan_routes(
            r#"package main
import fiber "github.com/gofiber/fiber/v2"
func main() {
    app := fiber.New()
    api := app.Group("/api", auth)
    api.Get("/users/:id", audit, users.Show)
}"#,
        );
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].method, "Get");
        assert_eq!(
            routes[0].path,
            Some(FiberRoutePath::Literal("/users/:id".into()))
        );
        assert_eq!(routes[0].middleware, ["audit"]);
        assert_eq!(routes[0].handler, "users.Show");
        assert_eq!(routes[0].pos.line, 5);
    }

    #[test]
    fn preserves_dynamic_paths_and_supports_all_registration_methods() {
        let methods = [
            "Get", "Post", "Put", "Patch", "Delete", "Head", "Options", "All", "Static",
        ];
        let registrations = methods
            .iter()
            .map(|method| format!("app.{method}(prefix+\"/x\", handler)"))
            .chain(std::iter::once("app.Use(auth, handler)".to_owned()))
            .collect::<Vec<_>>()
            .join("\n");
        let routes = scan_routes(&format!(
            "package main\nimport \"github.com/gofiber/fiber/v3\"\nfunc main() {{\napp := fiber.New()\n{registrations}\n}}"
        ));
        assert_eq!(routes.len(), 10);
        assert_eq!(
            routes[0].path,
            Some(FiberRoutePath::Dynamic("prefix+\"/x\"".into()))
        );
        assert_eq!(routes[9].path, None);
        assert_eq!(routes[9].middleware, ["auth"]);
        assert_eq!(routes[9].handler, "handler");
    }

    #[test]
    fn ignores_calls_on_unknown_receivers() {
        assert!(scan_routes("package main\nfunc f() { app.Get(\"/x\", handler) }").is_empty());
    }
}
