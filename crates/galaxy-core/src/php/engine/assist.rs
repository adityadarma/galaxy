//! Signature help, the outline of a file and quick fixes.

use std::path::Path;

use super::{
    Ctx, PhpEngine, byte_offset, clean_doc, import_edit, is_ident, member_operator, position_of,
    receiver_start, word_start,
};
use crate::lang::{Action, Edit, OutlineSymbol, Signature};
use crate::parameter_hints::{self, Argument, Parameter, ParameterHint};
use crate::php::framework;
use crate::php::parse::{ClassKind, FileSymbols, MemberKind, parse_file, parse_with_tree};

const MAX_IMPORT_CHOICES: usize = 8;

/// Words that are followed by `(` without being calls.
const NOT_CALLS: &[&str] = &[
    "if", "elseif", "while", "for", "foreach", "switch", "catch", "function", "fn", "array",
    "list", "isset", "unset", "empty", "match", "return", "echo", "print", "use", "declare",
];

/// The `(` of the call that surrounds `byte` and the commas between it and
/// the cursor. A text scan, so typing outside any call costs nothing.
fn open_call(text: &str, byte: usize) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let floor = byte.saturating_sub(3000);
    let mut depth = 0i32;
    let mut commas = 0usize;
    let mut i = byte.min(bytes.len());
    while i > floor {
        i -= 1;
        match bytes[i] {
            b')' | b']' | b'}' => depth += 1,
            b'(' => {
                if depth == 0 {
                    return Some((i, commas));
                }
                depth -= 1;
            }
            // An array literal inside the call: its commas are not arguments.
            b'[' => {
                if depth == 0 {
                    commas = 0;
                } else {
                    depth -= 1;
                }
            }
            b'{' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
            }
            b';' if depth == 0 => return None,
            b',' if depth == 0 => commas += 1,
            _ => {}
        }
    }
    None
}

fn ends_with_word(text: &str, word: &str) -> bool {
    let text = text.trim_end();
    text.len() >= word.len()
        && text[text.len() - word.len()..].eq_ignore_ascii_case(word)
        && text[..text.len() - word.len()]
            .bytes()
            .next_back()
            .is_none_or(|c| !is_ident(c) && c != b'$')
}

/// The parameters inside the leading parentheses of `detail`.
fn parameters_of(detail: &str) -> Vec<String> {
    let Some(rest) = detail.strip_prefix('(') else {
        return Vec::new();
    };
    let mut depth = 0i32;
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, c) in rest.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' if depth == 0 => {
                let last = rest[start..i].trim();
                if !last.is_empty() {
                    parts.push(last.to_owned());
                }
                return parts;
            }
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(rest[start..i].trim().to_owned());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts
}

/// The first paragraph of a docblock, before any `@tag`.
fn summary(doc: &str) -> Option<String> {
    let text = clean_doc(doc)
        .lines()
        .take_while(|line| !line.starts_with('@'))
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn signature_of(name: &str, detail: &str, doc: Option<&str>, commas: usize) -> Signature {
    Signature {
        label: format!("{name}{detail}"),
        documentation: doc.and_then(summary),
        parameters: parameters_of(detail),
        active: commas as u32,
    }
}

/// The called name before the `(` at `paren` and the text before that
/// name; `None` when the parenthesis does not open a call.
fn call_name(text: &str, paren: usize) -> Option<(&str, &str)> {
    let before = text[..paren].trim_end();
    let name_start = before
        .char_indices()
        .rev()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '\\') || !c.is_ascii()))
        .map_or(0, |(i, c)| i + c.len_utf8());
    let name = &before[name_start..];
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let head = &before[..name_start];
    if member_operator(head).is_none()
        && (NOT_CALLS.contains(&name.to_ascii_lowercase().as_str())
            || ends_with_word(head, "function")
            || ends_with_word(head, "fn")
            || ends_with_word(head, "class"))
    {
        return None;
    }
    Some((name, head))
}

/// `$name` from a declared parameter such as `?string $name = null`.
fn php_parameter(declared: &str) -> Parameter {
    let declared = declared.split('=').next().unwrap_or("").trim();
    let rest = declared.contains("...");
    let name = declared
        .split_whitespace()
        .last()
        .map(|word| word.trim_start_matches(['&', '.']))
        .filter(|word| word.starts_with('$'))
        .unwrap_or("");
    Parameter {
        name: name.trim_start_matches('$').to_owned(),
        rest,
    }
}

/// Calls on lines `first..=last` with arguments: the byte of each `(` and
/// the arguments it passes.
fn php_call_sites(
    text: &str,
    root: tree_sitter::Node<'_>,
    first: u32,
    last: u32,
) -> Vec<(usize, Vec<Argument>)> {
    let (first, last) = (first as usize, last as usize);
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.end_position().row < first || node.start_position().row > last {
            continue;
        }
        if out.len() < parameter_hints::MAX_CALLS
            && matches!(
                node.kind(),
                "function_call_expression"
                    | "member_call_expression"
                    | "nullsafe_member_call_expression"
                    | "scoped_call_expression"
                    | "object_creation_expression"
            )
            && let Some(list) = node.child_by_field_name("arguments").or_else(|| {
                let mut cursor = node.walk();
                node.named_children(&mut cursor)
                    .find(|child| child.kind() == "arguments")
            })
        {
            let mut cursor = list.walk();
            let arguments: Vec<_> = list
                .named_children(&mut cursor)
                .filter(|argument| argument.kind() == "argument")
                .map(|argument| php_argument(text, argument))
                .collect();
            if !arguments.is_empty() {
                out.push((list.start_byte(), arguments));
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    out
}

fn php_argument(text: &str, argument: tree_sitter::Node<'_>) -> Argument {
    let (line, column, _) = position_of(text, argument.start_byte());
    let mut cursor = argument.walk();
    let value = argument.named_children(&mut cursor).last();
    let spread = value.is_some_and(|value| value.kind() == "variadic_unpacking");
    let name = value.and_then(|value| {
        let named = match value.kind() {
            "variable_name" => value,
            "member_access_expression" | "nullsafe_member_access_expression" => {
                value.child_by_field_name("name")?
            }
            _ => return None,
        };
        named.utf8_text(text.as_bytes()).ok().map(str::to_owned)
    });
    Argument {
        line,
        column,
        name,
        spread,
        named: argument.child_by_field_name("name").is_some(),
    }
}

impl PhpEngine {
    /// The signature of the call the cursor is in, with the argument being
    /// typed. `None` outside a call or for a callee it cannot find.
    pub fn signature(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<Signature> {
        let byte = byte_offset(text, line, column)?;
        let (paren, commas) = open_call(text, byte)?;
        let previous = self.register_current(path, text);
        let out = self.signature_inner(path, text, paren, commas);
        self.restore_current(path, previous);
        out
    }

    fn signature_inner(
        &mut self,
        path: &Path,
        text: &str,
        paren: usize,
        commas: usize,
    ) -> Option<Signature> {
        // Cheap text check first: typing outside a call never parses.
        call_name(text, paren)?;
        let (file, tree) = parse_with_tree(path, text)?;
        let (name, detail, doc) = self.callee(text, &file, &tree, paren)?;
        Some(signature_of(&name, &detail, doc.as_deref(), commas))
    }

    /// Parameter-name hints for the calls on lines `first..=last`.
    pub fn parameter_hints(
        &mut self,
        path: &Path,
        text: &str,
        first: u32,
        last: u32,
    ) -> Vec<ParameterHint> {
        let previous = self.register_current(path, text);
        let mut out = Vec::new();
        if let Some((file, tree)) = parse_with_tree(path, text) {
            for (paren, arguments) in php_call_sites(text, tree.root_node(), first, last) {
                if let Some((_, detail, _)) = self.callee(text, &file, &tree, paren) {
                    let parameters: Vec<_> = parameters_of(&detail)
                        .iter()
                        .map(|parameter| php_parameter(parameter))
                        .collect();
                    out.extend(parameter_hints::hints(&parameters, &arguments));
                }
            }
        }
        self.restore_current(path, previous);
        out.sort_by_key(|hint| (hint.line, hint.column));
        out
    }

    /// Name, parameter detail and docblock of the callee whose argument
    /// list opens at `paren`.
    fn callee(
        &mut self,
        text: &str,
        file: &FileSymbols,
        tree: &tree_sitter::Tree,
        paren: usize,
    ) -> Option<(String, String, Option<String>)> {
        let (name, head) = call_name(text, paren)?;
        let operator = member_operator(head);
        let row = position_of(text, paren).0;
        let class = file
            .classes
            .iter()
            .find(|c| c.start_line <= row && row <= c.end_line)
            .cloned();
        let ctx = Ctx {
            src: text,
            tree,
            file,
            byte: paren,
            class,
        };

        if let Some((receiver_end, _)) = operator {
            let receiver = &head[receiver_start(head, receiver_end)..receiver_end];
            let fqn = self.eval(&ctx, receiver, 0)?;
            let class = self.class(&fqn)?;
            let (_, member) = self.find_member(&class, name, true)?;
            return Some((
                member.name.clone(),
                member.detail.clone(),
                member.doc.clone(),
            ));
        }
        if ends_with_word(head, "new") {
            let fqn = self.class_ref(&ctx, name)?;
            let class = self.class(&fqn)?;
            let (_, constructor) = self.find_member(&class, "__construct", true)?;
            return Some((
                class.name.clone(),
                constructor.detail.clone(),
                constructor.doc.clone(),
            ));
        }
        let namespaced = (!file.namespace.is_empty() && !name.starts_with('\\'))
            .then(|| format!("{}\\{name}", file.namespace));
        let function = namespaced
            .as_deref()
            .and_then(|fqn| self.function(fqn))
            .or_else(|| self.function(name))?;
        Some((
            function.name.clone(),
            function.detail.clone(),
            function.doc.clone(),
        ))
    }

    /// Quick fixes at `(line, column)`: imports for the class name under the
    /// cursor, and removal of unused imports.
    pub fn actions(&mut self, path: &Path, text: &str, line: u32, column: u32) -> Vec<Action> {
        let previous = self.register_current(path, text);
        let mut out = Vec::new();
        let template = framework::is_template(path);
        if let Some(byte) = byte_offset(text, line, column) {
            if !template {
                self.import_actions(path, text, byte, &mut out);
            }
            let frameworks = self.frameworks.clone();
            for fw in &frameworks {
                fw.actions(self, path, text, byte, &mut out);
            }
        }
        if !template {
            self.unused_import_actions(path, text, line, &mut out);
        }
        self.restore_current(path, previous);
        out
    }

    fn import_actions(&mut self, path: &Path, text: &str, byte: usize, out: &mut Vec<Action>) {
        let bytes = text.as_bytes();
        let start = word_start(text, byte);
        let mut end = byte;
        while end < bytes.len() && is_ident(bytes[end]) {
            end += 1;
        }
        let word = &text[start..end];
        if !word.starts_with(|c: char| c.is_ascii_uppercase()) {
            return;
        }
        let file = parse_file(path, text);
        let mut choices: Vec<(bool, String)> = self
            .class_names()
            .iter()
            .filter(|name| name.short == word)
            .map(|name| (name.vendor, name.fqn.clone()))
            .collect();
        for open in self.open.values() {
            for class in &open.classes {
                if class.name == word && !choices.iter().any(|(_, fqn)| *fqn == class.fqn) {
                    choices.push((false, class.fqn.clone()));
                }
            }
        }
        choices.sort();
        choices.dedup();
        for (_, fqn) in choices {
            if out.len() >= MAX_IMPORT_CHOICES {
                break;
            }
            let Some(insertion) = import_edit(&file, &fqn) else {
                continue;
            };
            out.push(Action {
                files: Vec::new(),
                title: format!("Import `{fqn}`"),
                edits: vec![Edit {
                    start_line: insertion.line,
                    start_column: insertion.column,
                    end_line: insertion.line,
                    end_column: insertion.column,
                    text: insertion.text,
                }],
            });
        }
    }

    fn unused_import_actions(&mut self, path: &Path, text: &str, line: u32, out: &mut Vec<Action>) {
        let lines: Vec<&str> = text.lines().collect();
        let removable: Vec<u32> = self
            .diagnostics(path, text)
            .into_iter()
            .filter(|p| p.message.starts_with("Unused import"))
            .map(|p| p.start_line)
            .filter(|row| {
                lines.get(*row as usize).is_some_and(|l| {
                    let l = l.trim();
                    l.starts_with("use ")
                        && l.ends_with(';')
                        && !l.contains(',')
                        && !l.contains('{')
                })
            })
            .collect();
        let delete = |row: u32| Edit {
            start_line: row,
            start_column: 0,
            end_line: row + 1,
            end_column: 0,
            text: String::new(),
        };
        if removable.contains(&line) {
            out.push(Action {
                files: Vec::new(),
                title: "Remove unused import".into(),
                edits: vec![delete(line)],
            });
        }
        if removable.len() > 1 {
            out.push(Action {
                files: Vec::new(),
                title: "Remove all unused imports".into(),
                edits: removable.iter().map(|row| delete(*row)).collect(),
            });
        }
    }
}

/// Classes, functions and their members, in source order.
pub fn outline(path: &Path, text: &str) -> Vec<OutlineSymbol> {
    let file = parse_file(path, text);
    let mut out: Vec<OutlineSymbol> = Vec::new();
    for class in &file.classes {
        let mut children: Vec<OutlineSymbol> = class
            .members
            .iter()
            // Members described by docblock tags are not declarations.
            .filter(|m| !m.in_docblock)
            .map(|member| OutlineSymbol {
                name: member.name.clone(),
                kind: match member.kind {
                    MemberKind::Method if member.name.eq_ignore_ascii_case("__construct") => {
                        "Constructor"
                    }
                    MemberKind::Method => "Method",
                    MemberKind::Property => "Property",
                    MemberKind::Constant => "Constant",
                    MemberKind::Case => "Enum Member",
                },
                line: member.line,
                column: member.column,
                children: Vec::new(),
            })
            .collect();
        children.sort_by_key(|s| (s.line, s.column));
        out.push(OutlineSymbol {
            name: class.name.clone(),
            kind: match class.kind {
                ClassKind::Class => "Class",
                ClassKind::Interface => "Interface",
                ClassKind::Trait => "Trait",
                ClassKind::Enum => "Enum",
            },
            line: class.line,
            column: class.column,
            children,
        });
    }
    for function in &file.functions {
        out.push(OutlineSymbol {
            name: function.name.clone(),
            kind: "Function",
            line: function.line,
            column: function.column,
            children: Vec::new(),
        });
    }
    out.sort_by_key(|s| (s.line, s.column));
    out
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
            root.join("app/Models/Order.php"),
            "<?php\nnamespace App\\Models;\n\nclass Order\n{\n    const MAX = 3;\n    public $count = 0;\n\n    public function __construct(int $id, string $name = '') {}\n\n    /**\n     * Adds two numbers.\n     *\n     * @param int $a first\n     */\n    public function add(int $a, string $b = 'x'): int\n    {\n        return 1;\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("app/Models/Customer.php"),
            "<?php\nnamespace App\\Models;\n\nclass Customer\n{\n}\n",
        )
        .unwrap();
        dir
    }

    /// `text` with `|` where the cursor is; the class is closed after it.
    fn signature_at(text: &str) -> Option<Signature> {
        let dir = project();
        let mut engine = PhpEngine::new(dir.path());
        let marker = text.find('|').expect("cursor marker");
        let before = &text[..marker];
        let line = before.matches('\n').count() as u32;
        let column = before.rsplit('\n').next().unwrap().encode_utf16().count() as u32;
        let source = text.replacen('|', "", 1);
        engine.signature(&dir.path().join("app/Report.php"), &source, line, column)
    }

    const HEAD: &str = "<?php\nnamespace App;\n\nuse App\\Models\\Order;\n\nclass Report\n{\n    public function run(Order $order)\n    {\n";
    const TAIL: &str = "\n    }\n}\n";

    #[test]
    fn signature_of_a_method_follows_the_argument() {
        let text = format!("{HEAD}        $order->add(1, |{TAIL}");
        let found = signature_at(&text).expect("signature");
        assert_eq!(found.label, "add(int $a, string $b = 'x'): int");
        assert_eq!(found.parameters, vec!["int $a", "string $b = 'x'"]);
        assert_eq!(found.active, 1);
        assert_eq!(found.documentation.as_deref(), Some("Adds two numbers."));

        // Commas inside an array literal belong to one argument.
        let text = format!("{HEAD}        $order->add([1, 2, |{TAIL}");
        assert_eq!(signature_at(&text).unwrap().active, 0);
        // A finished call is not one the cursor is in.
        let text = format!("{HEAD}        $order->add(1, 'a'); |{TAIL}");
        assert_eq!(signature_at(&text), None);
    }

    #[test]
    fn signature_of_constructors_and_functions() {
        let text = format!("{HEAD}        $o = new Order(1, |{TAIL}");
        let found = signature_at(&text).expect("constructor");
        assert_eq!(found.label, "Order(int $id, string $name = '')");
        assert_eq!(found.active, 1);

        let text = format!("{HEAD}        str_replace('a', |{TAIL}");
        let found = signature_at(&text).expect("built-in function");
        assert!(found.label.starts_with("str_replace("), "{}", found.label);
        assert_eq!(found.active, 1);
        assert!(found.parameters.len() >= 3);

        // Keywords and declarations are not calls.
        assert_eq!(signature_at(&format!("{HEAD}        if (|{TAIL}")), None);
        assert_eq!(signature_at("<?php\nfunction add(|\n{}\n"), None);
    }

    #[test]
    fn parameter_hints_label_methods_constructors_and_functions() {
        let dir = project();
        let mut engine = PhpEngine::new(dir.path());
        let path = dir.path().join("app/Report.php");
        let body = "        $order->add(1, $b);\n        $o = new Order(7, name: 'x');\n        str_replace('a', 'b', $subject);\n        $order->add(...$all);";
        let text = format!("{HEAD}{body}{TAIL}");
        let found = engine.parameter_hints(&path, &text, 0, 40);
        let labels: Vec<(u32, u32, &str)> = found
            .iter()
            .map(|hint| (hint.line, hint.column, hint.label.as_str()))
            .collect();
        // `$b` repeats its parameter; `name:` is already named; a spread
        // hides every position.
        assert!(labels.contains(&(9, 20, "a:")), "{labels:?}");
        assert!(
            !labels.iter().any(|(_, _, label)| *label == "b:"),
            "{labels:?}"
        );
        assert!(labels.contains(&(10, 23, "id:")), "{labels:?}");
        assert!(
            !labels.iter().any(|(_, _, label)| *label == "name:"),
            "{labels:?}"
        );
        assert!(
            labels
                .iter()
                .any(|(line, _, label)| *line == 11 && *label == "search:"),
            "{labels:?}"
        );
        assert!(!labels.iter().any(|(line, _, _)| *line == 12), "{labels:?}");
        assert_eq!(
            php_parameter("?string &...$rest = null"),
            Parameter {
                name: "rest".into(),
                rest: true
            }
        );
    }

    #[test]
    fn outline_lists_members_in_order() {
        let text = "<?php\nnamespace App;\n\n/**\n * @property string $label\n */\nclass Item\n{\n    const A = 1;\n    public $x;\n\n    public function __construct() {}\n\n    public function go() {}\n}\n\nfunction helper() {}\n";
        let symbols = outline(Path::new("Item.php"), text);
        let shape: Vec<(String, &str, usize)> = symbols
            .iter()
            .map(|s| (s.name.clone(), s.kind, s.children.len()))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("Item".into(), "Class", 4),
                ("helper".into(), "Function", 0)
            ]
        );
        let members: Vec<(&str, &str, u32)> = symbols[0]
            .children
            .iter()
            .map(|s| (s.name.as_str(), s.kind, s.line))
            .collect();
        assert_eq!(
            members,
            vec![
                ("A", "Constant", 8),
                ("x", "Property", 9),
                ("__construct", "Constructor", 11),
                ("go", "Method", 13),
            ],
            "the docblock's `label` is not a member"
        );
    }

    #[test]
    fn actions_import_classes_and_drop_unused_imports() {
        let dir = project();
        let mut engine = PhpEngine::new(dir.path());
        let path = dir.path().join("app/Services/S.php");
        let text = "<?php\nnamespace App\\Services;\n\nuse App\\Models\\Customer;\n\nclass S\n{\n    public function f(Order $o) {}\n}\n";

        // On `Order`: offer the import, placed after the last `use`.
        let found = engine.actions(&path, text, 7, 24);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].title, "Import `App\\Models\\Order`");
        assert_eq!(
            found[0].edits,
            vec![Edit {
                start_line: 4,
                start_column: 0,
                end_line: 4,
                end_column: 0,
                text: "use App\\Models\\Order;\n".into(),
            }]
        );

        // On the unused `use` line: offer to remove it.
        let found = engine.actions(&path, text, 3, 5);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].title, "Remove unused import");
        assert_eq!(
            found[0].edits,
            vec![Edit {
                start_line: 3,
                start_column: 0,
                end_line: 4,
                end_column: 0,
                text: String::new(),
            }]
        );

        // Nothing to do on plain code.
        assert!(engine.actions(&path, text, 6, 2).is_empty());
    }
}
