//! Problems found without running PHP: syntax errors, classes that cannot
//! be found and imports nothing uses.
//!
//! A class counts as missing only where the autoload data covers its
//! namespace, so names from code outside the project are never guessed at.

use std::collections::HashMap;
use std::path::Path;

use regex::Regex;
use tree_sitter::Node;

use super::{Ctx, PhpEngine, is_scope, position_of};
use crate::lang::{Problem, Severity};
use crate::php::generics;
use crate::php::parse::{FileSymbols, doc_tag_type, parse_with_tree, resolve_name};

const MAX_PROBLEMS: usize = 200;
/// How many unterminated statements are repaired before giving up.
const MAX_REPAIRS: usize = 8;

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
            return syntax_problems(root, text);
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
                "name" | "qualified_name" if is_class_reference(node) => {
                    let raw = &text[node.byte_range()];
                    if NOT_CLASSES.contains(&raw.to_ascii_lowercase().as_str()) {
                        continue;
                    }
                    if let Some(found) = simplifiable_name(text, node, &file) {
                        out.push(found);
                    }
                    let fqn = resolve_name(raw, &file.namespace, &file.uses);
                    if known && self.is_missing_class(&fqn, &mut verdicts) {
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
        self.check_returns(text, &file, &tree, &mut out);
        out.sort_by_key(|p| (p.start_line, p.start_column));
        out.truncate(MAX_PROBLEMS);
        out
    }

    /// `return` values that are a class the declared return type cannot
    /// hold. Only clear cases: the value must be a known class whose whole
    /// ancestry is known, and the declaration must list classes alone.
    fn check_returns(
        &mut self,
        text: &str,
        file: &FileSymbols,
        tree: &tree_sitter::Tree,
        out: &mut Vec<Problem>,
    ) {
        let mut declarations = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if matches!(node.kind(), "method_declaration" | "function_definition") {
                declarations.push(node);
            }
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
        }
        for node in declarations {
            let (Some(body), Some(name)) = (
                node.child_by_field_name("body"),
                node.child_by_field_name("name"),
            ) else {
                continue;
            };
            // The native type and the docblock both count: a return is fine
            // when either allows it.
            let mut raw = Vec::new();
            if let Some(declared) = node.child_by_field_name("return_type") {
                raw.push(text[declared.byte_range()].trim().to_owned());
            }
            if let Some(previous) = node.prev_named_sibling()
                && previous.kind() == "comment"
                && text[previous.byte_range()].starts_with("/**")
                && let Some(documented) = doc_tag_type(&text[previous.byte_range()], "return")
            {
                raw.push(documented);
            }
            if raw.is_empty() {
                continue;
            }
            let Some(expected) = expected_classes(&raw, file) else {
                continue;
            };
            let mut returns = Vec::new();
            let mut work = vec![body];
            while let Some(current) = work.pop() {
                let mut cursor = current.walk();
                for child in current.named_children(&mut cursor) {
                    if is_scope(child.kind()) {
                        continue;
                    }
                    if child.kind() == "return_statement" {
                        returns.push(child);
                    } else {
                        work.push(child);
                    }
                }
            }
            for statement in returns {
                let Some(value) = statement.named_child(0) else {
                    continue;
                };
                let row = statement.start_position().row as u32;
                let ctx = Ctx {
                    src: text,
                    tree,
                    file,
                    byte: statement.start_byte(),
                    class: file
                        .classes
                        .iter()
                        .find(|c| c.start_line <= row && row <= c.end_line)
                        .cloned(),
                };
                let Some(given) = self.eval(&ctx, &text[value.byte_range()], 0) else {
                    continue;
                };
                if !self.fits(&given, &expected) {
                    out.push(problem(
                        text,
                        value,
                        Severity::Warning,
                        format!(
                            "Return value of {}() is expected to be of type {}",
                            &text[name.byte_range()],
                            expected.join(", ")
                        ),
                        false,
                    ));
                }
            }
        }
    }

    /// `false` only when `given` is a class with a fully known ancestry
    /// that none of `expected` is part of.
    fn fits(&mut self, given: &str, expected: &[String]) -> bool {
        if given.is_empty()
            || !given
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '\\')
        {
            return true;
        }
        let mut names: Vec<String> = Vec::new();
        for (class, _) in self.lineage_env(given) {
            names.push(class.fqn.to_lowercase());
            for parent in class.extends.iter().chain(&class.implements) {
                if self.class(parent).is_none() {
                    // An ancestor outside what is known could be anything.
                    return true;
                }
                names.push(parent.trim_start_matches('\\').to_lowercase());
            }
        }
        if names.is_empty() {
            return true;
        }
        expected
            .iter()
            .any(|e| names.contains(&e.trim_start_matches('\\').to_lowercase()))
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

/// Message prefix of the problem [`simplifiable_name`] reports. The quick fix
/// reads the short name back out of it.
pub(super) const SIMPLIFY_PREFIX: &str = "Name '";
const SIMPLIFY_MIDDLE: &str = "' can be simplified with '";

/// The short name a message from [`simplifiable_name`] suggests.
pub(super) fn simplified_to(message: &str) -> Option<&str> {
    let rest = message.strip_prefix(SIMPLIFY_PREFIX)?;
    let (_, short) = rest.split_once(SIMPLIFY_MIDDLE)?;
    short.strip_suffix('\'')
}

/// A fully qualified class name (`\App\Services\Foo`) that its short name
/// already stands for here: imported with `use`, or in the file's own
/// namespace. The short name has to resolve to the same class, so a clashing
/// import keeps the long form.
fn simplifiable_name(
    text: &str,
    node: Node<'_>,
    file: &crate::php::parse::FileSymbols,
) -> Option<Problem> {
    let raw = &text[node.byte_range()];
    let fqn = raw.strip_prefix('\\')?;
    let short = fqn.rsplit('\\').next().filter(|short| !short.is_empty())?;
    let resolved = resolve_name(short, &file.namespace, &file.uses);
    if !resolved.eq_ignore_ascii_case(fqn) {
        return None;
    }
    Some(problem(
        text,
        node,
        Severity::Hint,
        format!("{SIMPLIFY_PREFIX}\\{fqn}{SIMPLIFY_MIDDLE}{short}'"),
        false,
    ))
}

/// The classes a declaration allows, native type and docblock together,
/// or `None` when it also allows anything that is not a plain class.
fn expected_classes(raw: &[String], file: &FileSymbols) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for declared in raw {
        for part in generics::split_union(declared.trim().trim_start_matches('?')) {
            let part = part.trim().trim_matches(['(', ')']);
            let base = generics::base(part);
            if base.is_empty() || base.ends_with("[]") || part.contains('{') || part.contains('$') {
                return None;
            }
            let lower = base.to_ascii_lowercase();
            if NOT_CLASSES.contains(&lower.as_str())
                || matches!(
                    lower.as_str(),
                    "resource" | "scalar" | "numeric" | "class-string"
                )
            {
                return None;
            }
            let fqn = resolve_name(base, &file.namespace, &file.uses)
                .trim_start_matches('\\')
                .to_owned();
            if !out.iter().any(|known| known.eq_ignore_ascii_case(&fqn)) {
                out.push(fqn);
            }
        }
    }
    (!out.is_empty()).then_some(out)
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
            // A missing token has no width. Mark the character before it so
            // the editor has something to underline.
            let at = node.start_byte().min(text.len());
            let start = text[..at]
                .char_indices()
                .next_back()
                .filter(|&(_, c)| c != '\n')
                .map_or(at, |(i, _)| i);
            let (start_line, start_column, _) = position_of(text, start);
            let (end_line, end_column, _) = position_of(text, at.max(start));
            out.push(Problem {
                start_line,
                start_column,
                end_line,
                end_column,
                severity: Severity::Error,
                message: format!("Syntax error: expected `{}`", node.kind()),
                unnecessary: false,
            });
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

/// Syntax problems of a tree that has errors. A statement missing its `;`
/// makes the parser cascade into bogus errors further down. Such a line is
/// reported as the real cause, then fixed in a scratch copy and parsed again,
/// so unrelated errors after it still show.
fn syntax_problems(root: Node<'_>, text: &str) -> Vec<Problem> {
    let mut causes = Vec::new();
    let mut source = text.to_string();
    let mut found = Vec::new();
    syntax_errors(root, &source, &mut found);
    for _ in 0..MAX_REPAIRS {
        let Some(first) = found.first() else { break };
        let Some(line) = unterminated_line(&source, first.start_line, first.start_column) else {
            break;
        };
        let Some((next, at)) = with_semicolon(&source, line) else {
            break;
        };
        let (start_line, start_column, _) = position_of(&source, at - 1);
        let (end_line, end_column, _) = position_of(&source, at);
        causes.push(Problem {
            start_line,
            start_column,
            end_line,
            end_column,
            severity: Severity::Error,
            message: "Syntax error: expected `;`".into(),
            unnecessary: false,
        });
        found.clear();
        source = next;
        let mut parser = tree_sitter::Parser::new();
        let reparsed = parser
            .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
            .ok()
            .and_then(|()| parser.parse(&source, None));
        match reparsed {
            Some(tree) if tree.root_node().has_error() => {
                syntax_errors(tree.root_node(), &source, &mut found);
            }
            _ => break,
        }
    }
    causes.extend(found);
    causes.sort_by_key(|p| (p.start_line, p.start_column));
    causes.truncate(MAX_PROBLEMS);
    causes
}

/// The 0-based line whose missing `;` explains an error at `line`/`column`.
///
/// Tree-sitter puts the `ERROR` either at the newline after an unterminated
/// call, or at the first token of the statement that follows it. In that
/// second case the culprit is the previous non-blank line.
fn unterminated_line(text: &str, line: u32, column: u32) -> Option<u32> {
    let lines: Vec<&str> = text.lines().collect();
    let here = lines.get(line as usize)?;
    let indent = here.len() - here.trim_start().len();
    let candidate = if (column as usize) <= indent {
        (0..line as usize)
            .rev()
            .find(|&i| !lines[i].trim().is_empty())?
    } else {
        line as usize
    };
    let before = lines[candidate].trim_end();
    let ends_call = before.ends_with(')') || before.ends_with(']');
    let is_comment = before.trim_start().starts_with("//") || before.trim_start().starts_with('#');
    (ends_call && !is_comment).then_some(candidate as u32)
}

/// `text` with a `;` appended to the end of 0-based `line`, and the byte
/// offset just after it. Only that line changes, so positions on every other
/// line stay valid.
fn with_semicolon(text: &str, line: u32) -> Option<(String, usize)> {
    let mut start = 0;
    for _ in 0..line {
        start += text[start..].find('\n')? + 1;
    }
    let end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    let at = start + text[start..end].trim_end().len();
    let mut out = String::with_capacity(text.len() + 1);
    out.push_str(&text[..at]);
    out.push(';');
    out.push_str(&text[at..]);
    Some((out, at + 1))
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
    fn qualified_names_the_file_already_resolves_can_be_simplified() {
        let dir = project();
        let hint = |text: &str| -> Vec<String> {
            run(dir.path(), text)
                .into_iter()
                .filter(|f| f.3.starts_with(SIMPLIFY_PREFIX))
                .map(|f| format!("{}:{} {}", f.0, f.1, f.3))
                .collect()
        };

        // Imported with `use`: the short name is the same class.
        let text = "<?php\nnamespace App\\Services;\n\nuse App\\Models\\User;\n\nclass S\n{\n    public function f() { return new \\App\\Models\\User(); }\n}\n";
        assert_eq!(
            hint(text),
            vec!["7:37 Name '\\App\\Models\\User' can be simplified with 'User'"]
        );

        // The file's own namespace counts as well.
        let text = "<?php\nnamespace App\\Models;\n\nclass S\n{\n    public function f() { return new \\App\\Models\\User(); }\n}\n";
        assert_eq!(hint(text).len(), 1, "{:?}", hint(text));

        // In the global namespace a global class needs no leading slash.
        let text = "<?php\n\nfunction f() { return new \\Exception(); }\n";
        assert_eq!(hint(text).len(), 1, "{:?}", hint(text));

        // A different class with the same short name keeps the long form.
        let text = "<?php\nnamespace App\\Services;\n\nuse Other\\User;\n\nclass S\n{\n    public function f() { return new \\App\\Models\\User(); }\n}\n";
        assert!(hint(text).is_empty(), "{:?}", hint(text));

        // Not imported and in another namespace: the short name would point
        // somewhere else.
        let text = "<?php\nnamespace App\\Services;\n\nclass S\n{\n    public function f() { return new \\App\\Models\\User(); }\n}\n";
        assert!(hint(text).is_empty(), "{:?}", hint(text));
        let text = "<?php\nnamespace App;\n\nclass S\n{\n    public function f() { return new \\Exception(); }\n}\n";
        assert!(hint(text).is_empty(), "{:?}", hint(text));

        // Names already short, and `use` lines, are left alone.
        let text = "<?php\nnamespace App\\Services;\n\nuse App\\Models\\User;\n\nclass S\n{\n    public function f() { return new User(); }\n}\n";
        assert!(hint(text).is_empty(), "{:?}", hint(text));
    }

    #[test]
    fn simplified_name_is_read_back_from_the_message() {
        assert_eq!(
            simplified_to("Name '\\App\\Models\\User' can be simplified with 'User'"),
            Some("User")
        );
        assert_eq!(simplified_to("Unused import `User`"), None);
    }

    #[test]
    fn a_returned_class_the_declared_type_cannot_hold_is_a_warning() {
        let dir = project();
        std::fs::write(
            dir.path().join("app/Models/Post.php"),
            "<?php\nnamespace App\\Models;\n\nclass Post\n{\n}\n",
        )
        .unwrap();
        let wrong = |text: &str| -> Vec<String> {
            run(dir.path(), text)
                .into_iter()
                .filter(|f| f.3.starts_with("Return value"))
                .map(|f| format!("{}: {}", f.0, f.3))
                .collect()
        };
        let head = "<?php\nnamespace App;\n\nuse App\\Models\\Post;\nuse App\\Models\\User;\n\nclass Service\n{\n";

        let text = format!(
            "{head}    public function a(): User\n    {{\n        return new Post();\n    }}\n}}\n"
        );
        assert_eq!(
            wrong(&text),
            vec!["10: Return value of a() is expected to be of type App\\Models\\User"]
        );

        // The docblock widens what is expected, and is named in the message.
        let text = format!(
            "{head}    /**\n     * @return \\App\\Models\\Post\n     */\n    public function a(): User\n    {{\n        return new Post();\n    }}\n}}\n"
        );
        assert!(wrong(&text).is_empty(), "{:?}", wrong(&text));

        // A docblock alone declares the type as well.
        let text = format!(
            "{head}    /**\n     * @return \\App\\Models\\User\n     */\n    public function a()\n    {{\n        return new Post();\n    }}\n}}\n"
        );
        assert_eq!(wrong(&text).len(), 1, "{:?}", wrong(&text));

        // Union, the same class, `$this` and unknown values are fine.
        for body in [
            ("User|Post", "new Post()"),
            ("User", "new User()"),
            ("self", "$this"),
            ("mixed", "new Post()"),
            ("User", "$unknown"),
            ("User", "foo()"),
        ] {
            let text = format!(
                "{head}    public function a(): {}\n    {{\n        return {};\n    }}\n}}\n",
                body.0, body.1
            );
            assert!(wrong(&text).is_empty(), "{body:?}: {:?}", wrong(&text));
        }
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
    fn missing_semicolon_is_reported_at_the_unterminated_statement() {
        let dir = project();
        let text = "<?php\nfunction run() {\n    try {\n        Aset::whereHas()\n        for ($i = 0; $i < 2; $i++) {}\n        DB::commit();\n        return Service::success(\"ok\");\n    } catch (Exception $e) {\n        DB::rollBack();\n    }\n}\n";
        let found = run(dir.path(), text);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, 3, "{found:?}");
        assert!(found[0].3.contains("`;`"), "{found:?}");
    }

    #[test]
    fn a_missing_token_is_marked_over_the_character_before_it() {
        let dir = project();
        let text = "<?php\n$x = $a ? 1 : 2;\n$y = $a ? foo()\n";
        let found = run(dir.path(), text);
        assert!(!found.is_empty(), "{found:?}");
        let mut engine = PhpEngine::new(dir.path());
        let problems = engine.diagnostics(&dir.path().join("app/Service.php"), text);
        let wide = problems
            .iter()
            .filter(|p| p.message.contains("expected"))
            .all(|p| p.end_line > p.start_line || p.end_column > p.start_column);
        assert!(wide, "{problems:?}");
    }

    #[test]
    fn errors_after_a_missing_semicolon_are_kept() {
        let dir = project();
        let text = "<?php\nfunction run() {\n    Aset::whereHas()\n    DB::commit();\n}\ndsdsdsd\n";
        let found = run(dir.path(), text);
        assert!(
            found.iter().any(|f| f.0 == 2 && f.3.contains("`;`")),
            "{found:?}"
        );
        assert!(found.iter().any(|f| f.0 == 5), "{found:?}");
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
