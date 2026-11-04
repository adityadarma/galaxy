//! Problems found without running PHP: syntax errors, classes that cannot
//! be found and imports nothing uses.
//!
//! A class counts as missing only where the autoload data covers its
//! namespace, so names from code outside the project are never guessed at.

use std::collections::HashMap;
use std::path::Path;

use regex::Regex;
use tree_sitter::Node;

use super::{PhpEngine, position_of};
use crate::lang::{Problem, Severity};
use crate::php::parse::{parse_with_tree, resolve_name};

const MAX_PROBLEMS: usize = 200;

/// Names that look like classes to the grammar but are not.
const NOT_CLASSES: &[&str] = &[
    "self", "static", "parent", "array", "callable", "iterable", "object", "mixed", "void", "null",
    "never", "false", "true", "bool", "int", "float", "string",
];

impl PhpEngine {
    /// Problems in `text`, the content of `path`.
    pub fn diagnostics(&mut self, path: &Path, text: &str) -> Vec<Problem> {
        let previous = self.register_current(path, text);
        let mut out = if self.is_template(path) {
            Vec::new()
        } else {
            self.diagnostics_inner(path, text)
        };
        if !out.iter().any(|p| p.severity == Severity::Error) {
            let frameworks = self.frameworks.clone();
            for fw in &frameworks {
                out.extend(fw.problems(self, path, text));
            }
        }
        self.restore_current(path, previous);
        out
    }

    fn diagnostics_inner(&mut self, path: &Path, text: &str) -> Vec<Problem> {
        let Some((file, tree)) = parse_with_tree(path, text) else {
            return Vec::new();
        };
        let root = tree.root_node();
        let mut out = Vec::new();
        if root.has_error() {
            syntax_errors(root, text, &mut out);
            return out;
        }
        let known = !self.autoload.is_empty();
        let mut verdicts: HashMap<String, bool> = HashMap::new();
        let mut imports: Vec<Node<'_>> = Vec::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                stack.push(child);
            }
            match node.kind() {
                "namespace_use_declaration" => imports.push(node),
                "name" | "qualified_name" if known && is_class_reference(node) => {
                    let raw = &text[node.byte_range()];
                    if NOT_CLASSES.contains(&raw.to_ascii_lowercase().as_str()) {
                        continue;
                    }
                    let fqn = resolve_name(raw, &file.namespace, &file.uses);
                    if self.is_missing_class(&fqn, &mut verdicts) {
                        out.push(problem(
                            text,
                            node,
                            Severity::Warning,
                            format!("Class `{}` not found", fqn.trim_start_matches('\\')),
                            false,
                        ));
                    }
                }
                _ => {}
            }
        }
        self.check_imports(text, &imports, known, &mut verdicts, &mut out);
        out.sort_by_key(|p| (p.start_line, p.start_column));
        out.truncate(MAX_PROBLEMS);
        out
    }

    fn is_missing_class(&mut self, fqn: &str, verdicts: &mut HashMap<String, bool>) -> bool {
        let key = fqn.trim_start_matches('\\').to_lowercase();
        if let Some(known) = verdicts.get(&key) {
            return *known;
        }
        let missing = self.autoload.owns(fqn) && self.class(fqn).is_none();
        verdicts.insert(key, missing);
        missing
    }

    /// Imports that name no class, and imports nothing in the file uses.
    fn check_imports(
        &mut self,
        text: &str,
        imports: &[Node<'_>],
        known: bool,
        verdicts: &mut HashMap<String, bool>,
        out: &mut Vec<Problem>,
    ) {
        // The text outside the `use` statements, to look for uses of an alias.
        let mut spans: Vec<(usize, usize)> = imports
            .iter()
            .map(|n| (n.start_byte(), n.end_byte()))
            .collect();
        spans.sort_unstable();
        let mut rest = String::with_capacity(text.len());
        let mut at = 0;
        for (start, end) in spans {
            if start >= at {
                rest.push_str(&text[at..start]);
                at = end;
            }
        }
        rest.push_str(&text[at.min(text.len())..]);

        for declaration in imports {
            // Grouped imports and `use function` / `use const` are left alone.
            if declaration.child_by_field_name("body").is_some()
                || declaration.child_by_field_name("type").is_some()
            {
                continue;
            }
            let mut cursor = declaration.walk();
            let clauses: Vec<Node<'_>> = declaration
                .named_children(&mut cursor)
                .filter(|n| n.kind() == "namespace_use_clause")
                .collect();
            for clause in clauses {
                let mut inner = clause.walk();
                let Some(name) = clause
                    .named_children(&mut inner)
                    .find(|n| matches!(n.kind(), "name" | "qualified_name"))
                else {
                    continue;
                };
                let imported = text[name.byte_range()].trim_start_matches('\\');
                let alias = clause
                    .child_by_field_name("alias")
                    .map(|n| text[n.byte_range()].to_owned())
                    .unwrap_or_else(|| imported.rsplit('\\').next().unwrap_or(imported).to_owned());
                if known
                    && !self.autoload.is_namespace(imported)
                    && self.is_missing_class(imported, verdicts)
                {
                    out.push(problem(
                        text,
                        name,
                        Severity::Warning,
                        format!("Class `{imported}` not found"),
                        false,
                    ));
                    continue;
                }
                let used = Regex::new(&format!(r"(?i)\b{}\b", regex::escape(&alias)))
                    .map(|re| re.is_match(&rest))
                    .unwrap_or(true);
                if !used {
                    out.push(problem(
                        text,
                        clause,
                        Severity::Hint,
                        format!("Unused import `{imported}`"),
                        true,
                    ));
                }
            }
        }
    }
}

fn problem(
    text: &str,
    node: Node<'_>,
    severity: Severity,
    message: String,
    unnecessary: bool,
) -> Problem {
    let (start_line, start_column, _) = position_of(text, node.start_byte());
    let (end_line, end_column, _) = position_of(text, node.end_byte());
    Problem {
        start_line,
        start_column,
        end_line,
        end_column,
        severity,
        message,
        unnecessary,
    }
}

/// Parse errors and the tokens the parser had to invent.
fn syntax_errors(root: Node<'_>, text: &str, out: &mut Vec<Problem>) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if out.len() >= MAX_PROBLEMS {
            return;
        }
        if node.is_missing() {
            out.push(problem(
                text,
                node,
                Severity::Error,
                format!("Syntax error: expected `{}`", node.kind()),
                false,
            ));
            continue;
        }
        if node.is_error() {
            // Mark the first line only: an error often swallows the rest.
            let (start_line, start_column, _) = position_of(text, node.start_byte());
            let line_end = text[node.start_byte()..]
                .find('\n')
                .map_or(text.len(), |i| node.start_byte() + i);
            let end = node
                .end_byte()
                .min(line_end)
                .max(node.start_byte() + 1)
                .min(text.len());
            let (end_line, end_column, _) = position_of(text, end);
            out.push(Problem {
                start_line,
                start_column,
                end_line,
                end_column,
                severity: Severity::Error,
                message: "Syntax error".into(),
                unnecessary: false,
            });
            continue;
        }
        if node.has_error() {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }
    }
    out.sort_by_key(|p| (p.start_line, p.start_column));
}

/// Whether `node`, a `name` or `qualified_name`, stands where a class is
/// expected: a type, `new`, `extends`, `Foo::bar`, `instanceof`, `use` of a
/// trait.
fn is_class_reference(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let is = |other: Option<Node<'_>>| other.is_some_and(|n| n.id() == node.id());
    match parent.kind() {
        "named_type" | "base_clause" | "class_interface_clause" | "use_declaration" => true,
        "object_creation_expression" => is(parent.named_child(0)),
        "scoped_call_expression" | "scoped_property_access_expression" => {
            is(parent.child_by_field_name("scope"))
        }
        "class_constant_access_expression" => is(parent.named_child(0)),
        "instanceof_expression" => {
            is(parent.named_child(parent.named_child_count().saturating_sub(1)))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("app/Models")).unwrap();
        std::fs::write(
            root.join("composer.json"),
            r#"{"autoload":{"psr-4":{"App\\":"app/"}}}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("app/Models/User.php"),
            "<?php\nnamespace App\\Models;\n\nclass User\n{\n}\n",
        )
        .unwrap();
        dir
    }

    fn run(dir: &Path, text: &str) -> Vec<(u32, u32, Severity, String)> {
        let mut engine = PhpEngine::new(dir);
        let path = dir.join("app/Service.php");
        engine
            .diagnostics(&path, text)
            .into_iter()
            .map(|p| (p.start_line, p.start_column, p.severity, p.message))
            .collect()
    }

    #[test]
    fn clean_code_has_no_problems() {
        let dir = project();
        let text = "<?php\nnamespace App;\n\nuse App\\Models\\User;\nuse Exception;\n\nclass Service\n{\n    public function run(User $user): ?User\n    {\n        try {\n            return new User();\n        } catch (Exception $e) {\n            return null;\n        }\n    }\n}\n";
        assert_eq!(run(dir.path(), text), vec![]);
    }

    #[test]
    fn syntax_errors_are_marked() {
        let dir = project();
        let text = "<?php\nnamespace App;\n\nclass Service\n{\n    public function run()\n    {\n        $x = ;\n    }\n}\n";
        let found = run(dir.path(), text);
        assert!(!found.is_empty());
        assert!(found.iter().all(|f| f.2 == Severity::Error));
        assert!(
            found.iter().all(|f| f.3.starts_with("Syntax error")),
            "{found:?}"
        );
        assert_eq!(found[0].0, 7);
    }

    #[test]
    fn missing_classes_are_found_only_in_known_namespaces() {
        let dir = project();
        let text = "<?php\nnamespace App;\n\nuse App\\Models\\Ghost;\nuse Vendor\\Pkg\\Thing;\n\nclass Service\n{\n    public function run(Missing $m, Thing $t, \\DateTime $d, Ghost $g)\n    {\n        return new Nope();\n    }\n}\n";
        let found = run(dir.path(), text);
        let messages: Vec<&str> = found.iter().map(|f| f.3.as_str()).collect();
        assert_eq!(
            messages,
            vec![
                "Class `App\\Models\\Ghost` not found",
                "Class `App\\Missing` not found",
                "Class `App\\Models\\Ghost` not found",
                "Class `App\\Nope` not found",
            ],
            "Thing (another vendor) and DateTime (global) are not guessed at: {found:?}"
        );
        // The unknown vendor import is neither missing nor, being used, unused.
    }

    #[test]
    fn unused_imports_are_hints() {
        let dir = project();
        let text = "<?php\nnamespace App;\n\nuse App\\Models\\User;\nuse DateTime as Clock;\nuse Exception;\n\n/** @throws Exception */\nclass Service\n{\n}\n";
        let found = run(dir.path(), text);
        assert_eq!(
            found,
            vec![
                (
                    3,
                    4,
                    Severity::Hint,
                    "Unused import `App\\Models\\User`".into()
                ),
                (4, 4, Severity::Hint, "Unused import `DateTime`".into()),
            ],
            "a name mentioned only in a docblock still counts as used"
        );
    }

    #[test]
    fn code_without_composer_data_is_not_judged() {
        let dir = tempfile::tempdir().unwrap();
        let text = "<?php\nnamespace App;\n\nclass Service\n{\n    public function run(Missing $m) {}\n}\n";
        assert_eq!(run(dir.path(), text), vec![]);
    }
}
