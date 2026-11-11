//! Bounded native Rust navigation and analysis.
//!
//! This module is the Rust semantic engine. It does not start or depend on an
//! external language server. Type inference, macro expansion, and Cargo
//! feature selection are implemented here as the native engine grows.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use tree_sitter::{Node, Parser, Tree};

use crate::lang::{
    Action, Completion, Edit, HoverResult, ItemKind, Occurrence, OutlineSymbol, Problem, Severity,
    Signature, Target,
};
use crate::parameter_hints::{self, Argument, Parameter, ParameterHint};

const MAX_FILES: usize = 4_000;
const MAX_FILE_BYTES: u64 = 2 << 20;
const MAX_ITEMS: usize = 200;
const MAX_PROBLEMS: usize = 200;
const MAX_OCCURRENCES: usize = 2_000;
const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];

#[derive(Debug, Clone)]
struct Declaration {
    name: String,
    module: Vec<String>,
    kind: ItemKind,
    path: PathBuf,
    line: u32,
    column: u32,
    scope_depth: usize,
    signature: String,
}

#[derive(Debug, Clone)]
struct UseBinding {
    local: String,
    target: Vec<String>,
    glob: bool,
}

/// Project declarations built on a worker thread.
#[derive(Debug, Default)]
pub struct Built {
    declarations: Vec<Declaration>,
    files: Vec<PathBuf>,
    uses: HashMap<PathBuf, Vec<UseBinding>>,
    source_roots: Vec<PathBuf>,
}

/// Static index for one Cargo project or repository.
#[derive(Debug)]
pub struct RustEngine {
    declarations: Vec<Declaration>,
    files: Vec<PathBuf>,
    uses: HashMap<PathBuf, Vec<UseBinding>>,
    source_roots: Vec<PathBuf>,
    open: HashMap<PathBuf, (Vec<Declaration>, Vec<UseBinding>, String)>,
    ready: bool,
}

impl RustEngine {
    pub fn index_stats(&self) -> crate::LanguageIndexStats {
        let imports = self.uses.values().map(Vec::len).sum();
        let source_bytes = self.open.values().map(|(_, _, text)| text.len()).sum();
        crate::LanguageIndexStats {
            files: self.files.len(),
            symbols: self.declarations.len(),
            imports,
            source_bytes,
            heap_lower_bound_bytes: self.files.len() * std::mem::size_of::<PathBuf>()
                + self.declarations.len() * std::mem::size_of::<Declaration>()
                + imports * std::mem::size_of::<UseBinding>()
                + source_bytes,
            ..Default::default()
        }
    }

    pub fn pending(_root: &Path) -> Self {
        Self {
            declarations: Vec::new(),
            files: Vec::new(),
            uses: HashMap::new(),
            source_roots: Vec::new(),
            open: HashMap::new(),
            ready: false,
        }
    }

    pub fn build(root: &Path) -> Built {
        Self::build_with_scheduler(root, crate::index_scheduler::shared_index_scheduler())
    }

    pub fn build_with_scheduler(
        root: &Path,
        scheduler: &crate::index_scheduler::IndexScheduler,
    ) -> Built {
        let mut declarations = Vec::new();
        let mut files = Vec::new();
        let mut uses = HashMap::new();
        let roots = cargo_source_roots(root);
        let mut candidates = Vec::new();
        for source_root in &roots {
            let mut walker = WalkBuilder::new(source_root);
            walker
                .hidden(false)
                .git_ignore(true)
                .git_global(true)
                .parents(true);
            for entry in walker.build().flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("rs")
                    || entry
                        .metadata()
                        .is_ok_and(|metadata| metadata.len() > MAX_FILE_BYTES)
                {
                    continue;
                }
                candidates.push((path.to_path_buf(), source_root.clone()));
            }
        }
        candidates.sort_by(|left, right| left.0.cmp(&right.0));
        candidates.dedup_by(|left, right| left.0 == right.0);
        candidates.truncate(MAX_FILES);
        let indexed = scheduler.map_ordered(candidates, |(path, source_root)| {
            let text = std::fs::read_to_string(&path).ok()?;
            Some((
                path.clone(),
                parse_declarations(&path, &text, &source_root),
                parse_use_bindings(&text),
            ))
        });
        for (path, parsed_declarations, parsed_uses) in indexed.into_iter().flatten() {
            files.push(path.clone());
            declarations.extend(parsed_declarations);
            uses.insert(path, parsed_uses);
        }
        declarations.sort_by(|left, right| {
            (&left.name, &left.path, left.line).cmp(&(&right.name, &right.path, right.line))
        });
        Built {
            declarations,
            files,
            uses,
            source_roots: roots,
        }
    }

    pub fn install(&mut self, built: Built) {
        self.declarations = built.declarations;
        self.files = built.files;
        self.uses = built.uses;
        self.source_roots = built.source_roots;
        self.ready = true;
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Applies source-file watcher changes without rediscovering Cargo roots.
    /// Cargo metadata changes can alter every module and require `build`.
    pub fn apply_disk_changes(&mut self, paths: &[PathBuf]) -> bool {
        if !self.ready
            || paths.iter().any(|path| {
                matches!(
                    path.file_name().and_then(|name| name.to_str()),
                    Some("Cargo.toml" | "Cargo.lock")
                )
            })
        {
            return false;
        }
        for path in paths {
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                continue;
            }
            self.open.remove(path);
            self.declarations.retain(|item| item.path != *path);
            self.uses.remove(path);
            self.files.retain(|file| file != path);
            if let Ok(text) = std::fs::read_to_string(path)
                && text.len() as u64 <= MAX_FILE_BYTES
            {
                let source_root = self.source_root_for(path);
                self.declarations
                    .extend(parse_declarations(path, &text, &source_root));
                self.uses.insert(path.clone(), parse_use_bindings(&text));
                self.files.push(path.clone());
            }
        }
        self.declarations.sort_by(|left, right| {
            (&left.name, &left.path, left.line).cmp(&(&right.name, &right.path, right.line))
        });
        true
    }

    pub fn sync_document(&mut self, path: &Path, text: &str) {
        self.open.insert(
            path.to_path_buf(),
            (
                parse_declarations(path, text, &self.source_root_for(path)),
                parse_use_bindings(text),
                text.to_owned(),
            ),
        );
    }

    pub fn close_document(&mut self, path: &Path) {
        self.open.remove(path);
    }

    pub fn complete(&mut self, path: &Path, text: &str, line: u32, column: u32) -> Vec<Completion> {
        self.sync_document(path, text);
        let prefix = word_before(text, line, column);
        if member_access_before(text, line, column) {
            return self
                .member_completions(text, line, column)
                .into_iter()
                .filter(|item| item.label.starts_with(prefix))
                .take(MAX_ITEMS)
                .collect();
        }
        let mut candidates = self.visible_declarations(path);
        candidates.extend(local_declarations(
            path,
            text,
            cursor_byte(text, line, column),
        ));
        candidates.retain(|item| item.name.starts_with(prefix));
        candidates
            .sort_by(|left, right| left.name.cmp(&right.name).then(left.path.cmp(&right.path)));
        candidates.dedup_by(|left, right| left.name == right.name && left.kind == right.kind);
        candidates
            .into_iter()
            .take(MAX_ITEMS)
            .map(|item| Completion {
                label: item.name.clone(),
                detail: Some(item.signature.clone()),
                insert_text: item.name.clone(),
                is_snippet: false,
                kind: item.kind,
                sort: item.name.clone(),
                extra: Vec::new(),
                replace_from: None,
            })
            .collect()
    }

    pub fn signature(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<Signature> {
        self.sync_document(path, text);
        let byte = cursor_byte(text, line, column);
        let (name, active) = call_at(text, byte)?;
        let declaration = self
            .resolve(path, &name, byte)
            .into_iter()
            .find(|item| item.kind == ItemKind::Function)?;
        let parameters = signature_parameters(&declaration.signature);
        Some(Signature {
            label: declaration.signature,
            documentation: None,
            parameters,
            active,
        })
    }

    /// Parameter-name hints for the calls on lines `first..=last`. Free and
    /// associated functions only: method calls would need the receiver's
    /// type, which the engine looks up by reading the project.
    pub fn parameter_hints(
        &mut self,
        path: &Path,
        text: &str,
        first: u32,
        last: u32,
    ) -> Vec<ParameterHint> {
        self.sync_document(path, text);
        let Some(tree) = parse_tree(text) else {
            return Vec::new();
        };
        let mut sources: HashMap<PathBuf, Option<String>> = HashMap::new();
        let mut out = Vec::new();
        for (name, associated, arguments) in rust_call_sites(text, tree.root_node(), first, last) {
            let declarations: Vec<_> = self
                .resolve_nonlocal(path, &name)
                .into_iter()
                .filter(|item| item.kind == ItemKind::Function)
                .collect();
            let [declaration] = declarations.as_slice() else {
                continue;
            };
            let Some(source) = self.declaration_source(&mut sources, path, text, declaration)
            else {
                continue;
            };
            if let Some(parameters) =
                rust_parameters_at(source, declaration.line, declaration.column, associated)
            {
                out.extend(parameter_hints::hints(&parameters, &arguments));
            }
        }
        self.type_hints(
            path,
            text,
            tree.root_node(),
            (first, last),
            &mut sources,
            &mut out,
        );
        out.sort_by_key(|hint| (hint.line, hint.column));
        out
    }

    /// Source text of the file that holds `declaration`: the open buffer, the
    /// text being analyzed, or the file on disk. Cached in `sources`.
    fn declaration_source<'a>(
        &self,
        sources: &'a mut HashMap<PathBuf, Option<String>>,
        path: &Path,
        text: &str,
        declaration: &Declaration,
    ) -> Option<&'a str> {
        sources
            .entry(declaration.path.clone())
            .or_insert_with(|| {
                if declaration.path == path {
                    Some(text.to_owned())
                } else {
                    self.document(&declaration.path)
                        .map(|(_, text)| text.to_owned())
                        .or_else(|| std::fs::read_to_string(&declaration.path).ok())
                }
            })
            .as_deref()
    }

    /// `: type` labels after the name in `let` statements without a type, on
    /// lines `rows.0..=rows.1`. Only types that can be named with certainty
    /// get a label: literals, struct literals, and calls of one resolved
    /// function with a written return type.
    fn type_hints(
        &self,
        path: &Path,
        text: &str,
        root: Node<'_>,
        rows: (u32, u32),
        sources: &mut HashMap<PathBuf, Option<String>>,
        out: &mut Vec<ParameterHint>,
    ) {
        let (first, last) = (rows.0 as usize, rows.1 as usize);
        let mut found = 0;
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.end_position().row < first || node.start_position().row > last {
                continue;
            }
            if node.kind() == "let_declaration"
                && found < parameter_hints::MAX_CALLS
                && let Some(hint) = self.let_type_hint(path, text, node, sources)
            {
                found += 1;
                out.push(hint);
            }
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
        }
    }

    fn let_type_hint(
        &self,
        path: &Path,
        text: &str,
        declaration: Node<'_>,
        sources: &mut HashMap<PathBuf, Option<String>>,
    ) -> Option<ParameterHint> {
        if declaration.child_by_field_name("type").is_some() {
            return None;
        }
        let mut pattern = declaration.child_by_field_name("pattern")?;
        if pattern.kind() == "mut_pattern" {
            let mut cursor = pattern.walk();
            pattern = pattern.named_children(&mut cursor).last()?;
        }
        if pattern.kind() != "identifier" {
            return None;
        }
        let name = pattern.utf8_text(text.as_bytes()).ok()?;
        let value = declaration.child_by_field_name("value")?;
        let label = match value.kind() {
            "string_literal" | "raw_string_literal" => "&str".to_owned(),
            "boolean_literal" => "bool".to_owned(),
            "char_literal" => "char".to_owned(),
            "integer_literal" | "float_literal" | "unary_expression" | "negative_literal" => {
                numeric_literal_type(text, declaration, value, name)?
            }
            "struct_expression" => {
                let type_name = value.child_by_field_name("name")?;
                if type_name.kind() != "type_identifier" {
                    return None;
                }
                let type_name = type_name.utf8_text(text.as_bytes()).ok()?;
                if type_name == "Self" {
                    return None;
                }
                type_name.to_owned()
            }
            "call_expression" => self.call_result_type(path, text, value, sources)?,
            _ => return None,
        };
        Some(ParameterHint {
            line: pattern.end_position().row as u32,
            column: utf16_column(text, pattern.end_byte()),
            label: format!(": {label}"),
            return_type: false,
        })
    }

    /// The written return type of the single function that `call` names.
    fn call_result_type(
        &self,
        path: &Path,
        text: &str,
        call: Node<'_>,
        sources: &mut HashMap<PathBuf, Option<String>>,
    ) -> Option<String> {
        let function = call.child_by_field_name("function")?;
        let (name, owner) = match function.kind() {
            "identifier" => (function, None),
            "scoped_identifier" => {
                let owner = function
                    .child_by_field_name("path")?
                    .utf8_text(text.as_bytes())
                    .ok()?
                    .rsplit("::")
                    .next()?;
                (function.child_by_field_name("name")?, Some(owner))
            }
            _ => return None,
        };
        let name = name.utf8_text(text.as_bytes()).ok()?;
        if name.starts_with(|c: char| c.is_uppercase()) {
            return None;
        }
        let declarations: Vec<_> = self
            .resolve_nonlocal(path, name)
            .into_iter()
            .filter(|item| item.kind == ItemKind::Function)
            .collect();
        let [declaration] = declarations.as_slice() else {
            return None;
        };
        let source = self.declaration_source(sources, path, text, declaration)?;
        rust_return_type_at(source, declaration.line, declaration.column, owner)
    }

    pub fn definition(&mut self, path: &Path, text: &str, line: u32, column: u32) -> Vec<Target> {
        self.sync_document(path, text);
        let Some(name) = identifier_at(text, line, column) else {
            return Vec::new();
        };
        self.resolve(path, &name, cursor_byte(text, line, column))
            .into_iter()
            .map(|item| Target {
                path: item.path,
                line: item.line,
                column: item.column,
            })
            .take(MAX_ITEMS)
            .collect()
    }

    pub fn hover(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<HoverResult> {
        self.sync_document(path, text);
        let (name, start, end) = identifier_range(text, line, column)?;
        let declaration = self
            .resolve(path, &name, cursor_byte(text, line, column))
            .into_iter()
            .next()?;
        Some(HoverResult {
            markdown: format!("```rust\n{}\n```", declaration.signature),
            line,
            start_column: start,
            end_column: end,
        })
    }

    pub fn diagnostics(&mut self, path: &Path, text: &str) -> Vec<Problem> {
        self.sync_document(path, text);
        let Some(tree) = parse_tree(text) else {
            return Vec::new();
        };
        let mut errors = Vec::new();
        collect_errors(tree.root_node(), text, &mut errors);
        errors.sort_by_key(|problem| (problem.start_line, problem.start_column));
        errors.truncate(MAX_PROBLEMS);
        errors
    }

    pub fn actions(&mut self, path: &Path, text: &str, line: u32, _column: u32) -> Vec<Action> {
        self.sync_document(path, text);
        let Some(source) = text.lines().nth(line as usize) else {
            return Vec::new();
        };
        let Some(binding) = parse_use_bindings(source)
            .into_iter()
            .find(|binding| !binding.glob)
        else {
            return Vec::new();
        };
        if identifier_count(text, &binding.local) != 1 {
            return Vec::new();
        }
        vec![Action {
            title: format!("Remove unused import `{}`", binding.local),
            edits: vec![Edit {
                start_line: line,
                start_column: 0,
                end_line: line + 1,
                end_column: 0,
                text: String::new(),
            }],
            files: Vec::new(),
        }]
    }

    /// Conservative native formatting for valid Rust syntax. It changes only
    /// indentation and trailing whitespace, preserving token spacing.
    pub fn format(&self, text: &str) -> Option<String> {
        let tree = parse_tree(text)?;
        if tree.root_node().has_error() {
            return None;
        }
        let mut depth = 0usize;
        let mut out = String::new();
        for line in text.lines() {
            let content = line.trim();
            if content.starts_with('}') {
                depth = depth.saturating_sub(1);
            }
            if !content.is_empty() {
                out.push_str(&"    ".repeat(depth));
                out.push_str(content);
            }
            out.push('\n');
            let (opens, closes) = structural_braces(content);
            depth += opens;
            depth = depth.saturating_sub(closes);
        }
        Some(out)
    }

    /// Finds identifier occurrences when the name has exactly one top-level
    /// declaration in this project. Ambiguous names require richer resolution.
    pub fn references(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Vec<Occurrence> {
        self.sync_document(path, text);
        let Some(name) = identifier_at(text, line, column) else {
            return Vec::new();
        };
        let declarations = self.resolve(path, &name, cursor_byte(text, line, column));
        if declarations.len() != 1 {
            return Vec::new();
        }
        if declarations[0].kind == ItemKind::Variable {
            return local_occurrences(path, text, &name, &declarations[0]);
        }
        let target = &declarations[0];
        self.sources()
            .into_iter()
            .flat_map(|(candidate_path, source)| {
                item_occurrences(self, &candidate_path, &source, &name, target)
            })
            .take(MAX_OCCURRENCES)
            .collect()
    }

    /// Validates that a local rename is safe enough for the current static
    /// resolver, then returns all edits as UTF-16 occurrences.
    pub fn rename(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
        new_name: &str,
    ) -> Result<Vec<Occurrence>, String> {
        if !valid_name(new_name) {
            return Err("invalid Rust identifier".to_owned());
        }
        let found = self.references(path, text, line, column);
        if found.is_empty() {
            return Err("symbol is ambiguous or unknown".to_owned());
        }
        Ok(found)
    }

    pub fn outline(&self, path: &Path, text: &str) -> Vec<OutlineSymbol> {
        parse_declarations(path, text, &self.source_root_for(path))
            .into_iter()
            .map(|item| OutlineSymbol {
                name: item.name,
                kind: item_kind_name(item.kind),
                line: item.line,
                column: item.column,
                children: Vec::new(),
            })
            .collect()
    }

    fn all_declarations(&self) -> Vec<Declaration> {
        let mut all = self.declarations.clone();
        all.retain(|item| !self.open.contains_key(&item.path));
        all.extend(
            self.open
                .values()
                .flat_map(|(declarations, _, _)| declarations)
                .cloned(),
        );
        all
    }

    fn visible_declarations(&self, path: &Path) -> Vec<Declaration> {
        let mut visible = self.all_declarations();
        let aliases = self.use_bindings(path);
        for binding in aliases {
            let targets = if binding.glob {
                self.declarations_in_module(path, &binding.target)
            } else {
                self.resolve_path(path, &binding.target)
            };
            for target in targets {
                let mut alias = target.clone();
                if !binding.glob {
                    alias.name = binding.local.clone();
                }
                visible.push(alias);
            }
        }
        visible
    }

    fn member_completions(&self, text: &str, line: u32, column: u32) -> Vec<Completion> {
        let byte = cursor_byte(text, line, column);
        let before = &text[..byte];
        let Some(receiver) = before
            .rsplit_once('.')
            .map(|(left, _)| left.trim_end())
            .and_then(|left| left.rsplit(|character: char| !is_ident(character)).next())
        else {
            return Vec::new();
        };
        let Some(ty) = receiver_type(before, receiver) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (_, source) in self.sources() {
            out.extend(struct_fields(&source, &ty));
            out.extend(impl_methods(&source, &ty));
        }
        out.sort_by(|left, right| left.label.cmp(&right.label));
        out.dedup_by(|left, right| left.label == right.label);
        out
    }

    fn resolve(&self, path: &Path, name: &str, at: usize) -> Vec<Declaration> {
        if let Some((_, text)) = self.document(path)
            && let Some(members) = self.resolve_member(text, name, at)
        {
            return members;
        }
        if let Some((_, text)) = self.document(path)
            && let Some(local) = local_declaration(path, text, name, at)
        {
            return vec![local];
        }
        self.resolve_nonlocal(path, name)
    }

    /// Resolve `value.member` after the lightweight local type inference used
    /// by member completion. Do not fall back to a name-only match: a method
    /// name is frequently shared by unrelated types.
    fn resolve_member(&self, text: &str, name: &str, at: usize) -> Option<Vec<Declaration>> {
        let tree = parse_tree(text)?;
        let ast_receiver = member_access_at(tree.root_node(), at).and_then(|member| {
            member.child_by_field_name("value").and_then(|receiver| {
                receiver
                    .utf8_text(text.as_bytes())
                    .ok()
                    .map(|name| (name, member.start_byte()))
            })
        });
        let (receiver, before) = if let Some((receiver, start)) = ast_receiver {
            (receiver, &text[..start])
        } else {
            let before = &text[..at];
            let (left, _) = before.rsplit_once('.')?;
            let receiver = left
                .trim_end()
                .rsplit(|character: char| !is_ident(character))
                .next()?;
            (receiver, left)
        };
        if !is_ident_text(receiver) {
            return None;
        }
        let ty = receiver_type(before, receiver)?;
        let mut matches = Vec::new();
        for (path, source) in self.sources() {
            matches.extend(member_declarations(&path, &source, &ty, name));
        }
        matches.sort_by(|left, right| {
            (&left.path, left.line, left.column).cmp(&(&right.path, right.line, right.column))
        });
        matches.dedup_by(|left, right| {
            left.path == right.path && left.line == right.line && left.column == right.column
        });
        (matches.len() == 1).then_some(matches)
    }

    /// Resolves project and import declarations without reparsing local scopes.
    /// Workspace diagnostics call this after collecting every local binding from
    /// their already-parsed syntax tree.
    fn resolve_nonlocal(&self, path: &Path, name: &str) -> Vec<Declaration> {
        let all = self.all_declarations();
        let local: Vec<_> = all
            .iter()
            .filter(|item| item.path == path && item.name == name)
            .cloned()
            .collect();
        if !local.is_empty() {
            return local;
        }
        if let Some(binding) = self
            .use_bindings(path)
            .iter()
            .find(|binding| binding.local == name)
            && !binding.glob
            && let Some(target) = self.resolve_path(path, &binding.target).into_iter().next()
        {
            return vec![target];
        }
        let globbed: Vec<_> = self
            .use_bindings(path)
            .into_iter()
            .filter(|binding| binding.glob)
            .flat_map(|binding| self.declarations_in_module(path, &binding.target))
            .filter(|item| item.name == name)
            .collect();
        if globbed.len() == 1 {
            return globbed;
        }
        let matching: Vec<_> = all.into_iter().filter(|item| item.name == name).collect();
        if matching.len() == 1 {
            matching
        } else {
            Vec::new()
        }
    }

    fn resolve_path(&self, path: &Path, target: &[String]) -> Vec<Declaration> {
        let Some(name) = target.last() else {
            return Vec::new();
        };
        let current = self.module_for(path);
        let module = match target.first().map(String::as_str) {
            Some("crate") => target[1..target.len().saturating_sub(1)].to_vec(),
            Some("self") => {
                let mut module = current;
                module.extend_from_slice(&target[1..target.len().saturating_sub(1)]);
                module
            }
            Some("super") => {
                let mut module = current;
                let parents = target
                    .iter()
                    .take_while(|segment| segment.as_str() == "super")
                    .count();
                for _ in 0..parents {
                    module.pop();
                }
                module.extend_from_slice(&target[parents..target.len().saturating_sub(1)]);
                module
            }
            _ if target.len() > 1 => target[..target.len() - 1].to_vec(),
            _ => Vec::new(),
        };
        let matches: Vec<_> = self
            .all_declarations()
            .into_iter()
            .filter(|item| item.name == *name && item.module == module)
            .collect();
        if matches.len() == 1 {
            matches
        } else {
            Vec::new()
        }
    }

    fn declarations_in_module(&self, path: &Path, target: &[String]) -> Vec<Declaration> {
        let module = self.module_target(path, target);
        self.all_declarations()
            .into_iter()
            .filter(|item| item.module == module)
            .collect()
    }

    fn module_target(&self, path: &Path, target: &[String]) -> Vec<String> {
        let current = self.module_for(path);
        match target.first().map(String::as_str) {
            Some("crate") => target[1..].to_vec(),
            Some("self") => {
                let mut module = current;
                module.extend_from_slice(&target[1..]);
                module
            }
            Some("super") => {
                let parents = target
                    .iter()
                    .take_while(|segment| segment.as_str() == "super")
                    .count();
                let mut module = current;
                for _ in 0..parents {
                    module.pop();
                }
                module.extend_from_slice(&target[parents..]);
                module
            }
            _ => target.to_vec(),
        }
    }

    fn source_root_for(&self, path: &Path) -> PathBuf {
        self.source_roots
            .iter()
            .find(|root| path.starts_with(root))
            .cloned()
            .or_else(|| path.parent().map(Path::to_path_buf))
            .unwrap_or_default()
    }

    fn module_for(&self, path: &Path) -> Vec<String> {
        module_path(path, &self.source_root_for(path))
    }

    fn use_bindings(&self, path: &Path) -> Vec<UseBinding> {
        self.open
            .get(path)
            .map(|(_, uses, _)| uses.clone())
            .or_else(|| self.uses.get(path).cloned())
            .unwrap_or_default()
    }

    fn sources(&self) -> Vec<(PathBuf, String)> {
        let mut paths = self.files.clone();
        paths.extend(self.open.keys().cloned());
        paths.sort();
        paths.dedup();
        paths
            .into_iter()
            .filter_map(|path| {
                self.open
                    .get(&path)
                    .map(|(_, _, text)| (path.clone(), text.clone()))
                    .or_else(|| std::fs::read_to_string(&path).ok().map(|text| (path, text)))
            })
            .collect()
    }

    fn document(&self, path: &Path) -> Option<(PathBuf, &str)> {
        self.open
            .get(path)
            .map(|(_, _, text)| (path.to_path_buf(), text.as_str()))
    }
}

fn member_access_at(node: Node<'_>, at: usize) -> Option<Node<'_>> {
    if node.start_byte() > at || node.end_byte() < at {
        return None;
    }
    if node.kind() == "field_expression"
        && node
            .child_by_field_name("field")
            .is_some_and(|field| field.start_byte() <= at && at <= field.end_byte())
    {
        return Some(node);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find_map(|child| member_access_at(child, at))
}

fn structural_braces(line: &str) -> (usize, usize) {
    let mut quoted = false;
    let mut escaped = false;
    let mut opens = 0;
    let mut closes = 0;
    let mut characters = line.chars().peekable();
    while let Some(character) = characters.next() {
        if !quoted && character == '/' && characters.peek() == Some(&'/') {
            break;
        }
        if character == '"' && !escaped {
            quoted = !quoted;
        }
        if !quoted {
            match character {
                '{' => opens += 1,
                '}' => closes += 1,
                _ => {}
            }
        }
        escaped = character == '\\' && !escaped;
        if character != '\\' {
            escaped = false;
        }
    }
    (opens, closes)
}

/// The nearest Cargo workspace root, then `.git`, then the file directory.
pub fn project_root(file: &Path) -> PathBuf {
    let start = file.parent().unwrap_or(file);
    start
        .ancestors()
        .find(|dir| dir.join("Cargo.toml").is_file())
        .or_else(|| start.ancestors().find(|dir| dir.join(".git").exists()))
        .unwrap_or(start)
        .to_path_buf()
}

/// Source directories configured by the nearest Cargo manifest. A normal
/// package uses `src`; custom `path` targets add their containing directory.
/// This parser intentionally reads only local manifest paths and never runs
/// Cargo or resolves dependencies.
fn cargo_source_roots(root: &Path) -> Vec<PathBuf> {
    let manifest = root.join("Cargo.toml");
    let mut roots = vec![root.join("src")];
    let Ok(contents) = std::fs::read_to_string(manifest) else {
        return roots;
    };
    for line in contents.lines().map(str::trim) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "path" {
            continue;
        }
        let value = value.trim().trim_matches('"');
        let path = root.join(value);
        if path.extension().is_some_and(|extension| extension == "rs")
            && let Some(parent) = path.parent()
        {
            roots.push(parent.to_path_buf());
        }
    }
    roots.retain(|path| path.is_dir());
    roots.sort();
    roots.dedup();
    roots
}

fn module_path(path: &Path, source_root: &Path) -> Vec<String> {
    let Ok(relative) = path.strip_prefix(source_root) else {
        return Vec::new();
    };
    let mut parts: Vec<_> = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .map(str::to_owned)
        .collect();
    let Some(file) = parts.pop() else {
        return Vec::new();
    };
    let stem = Path::new(&file)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("");
    if !matches!(stem, "lib" | "main" | "mod") && !stem.is_empty() {
        parts.push(stem.to_owned());
    }
    parts
}

fn parse_tree(text: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .ok()?;
    parser.parse(text, None)
}

fn parse_declarations(path: &Path, text: &str, source_root: &Path) -> Vec<Declaration> {
    let Some(tree) = parse_tree(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect_declarations(
        path,
        text,
        tree.root_node(),
        &module_path(path, source_root),
        &mut out,
    );
    out
}

fn collect_declarations(
    path: &Path,
    text: &str,
    node: Node<'_>,
    module: &[String],
    out: &mut Vec<Declaration>,
) {
    collect_declaration(path, text, node, module, out);
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let mut child_module = module.to_vec();
        if node.kind() == "mod_item"
            && child.kind() == "declaration_list"
            && let Some(name) = node.child_by_field_name("name")
            && let Ok(name) = name.utf8_text(text.as_bytes())
        {
            child_module.push(name.to_owned());
        }
        collect_declarations(path, text, child, &child_module, out);
    }
}

fn parse_use_bindings(text: &str) -> Vec<UseBinding> {
    let Some(tree) = parse_tree(text) else {
        return Vec::new();
    };
    let mut bindings = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "use_declaration" {
            let source = node.utf8_text(text.as_bytes()).unwrap_or("");
            bindings.extend(use_bindings(source));
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    bindings
}

fn use_bindings(source: &str) -> Vec<UseBinding> {
    let source = source
        .trim()
        .trim_start_matches("pub ")
        .trim_start_matches("use ")
        .trim_end_matches(';')
        .trim();
    let mut out = Vec::new();
    collect_use_bindings(source, "", &mut out);
    out
}

fn collect_use_bindings(source: &str, prefix: &str, out: &mut Vec<UseBinding>) {
    for item in split_use_items(source) {
        let item = item.trim();
        if let Some(open) = item.find('{') {
            let before = item[..open].trim_end_matches("::").trim();
            let close = item.rfind('}').unwrap_or(item.len());
            let next_prefix = if before.is_empty() {
                prefix.to_owned()
            } else {
                format!("{prefix}{before}::")
            };
            collect_use_bindings(&item[open + 1..close], &next_prefix, out);
            continue;
        }
        let (path, alias) = item.rsplit_once(" as ").unwrap_or((item, ""));
        if path.trim_end().ends_with("::*") {
            let target: Vec<String> =
                format!("{prefix}{}", path.trim_end().trim_end_matches("::*"))
                    .split("::")
                    .map(str::trim)
                    .filter(|segment| !segment.is_empty())
                    .map(str::to_owned)
                    .collect();
            if !target.is_empty() && target.iter().all(|segment| is_ident_text(segment)) {
                out.push(UseBinding {
                    local: "*".to_owned(),
                    target,
                    glob: true,
                });
            }
            continue;
        }
        let target: Vec<String> = format!("{prefix}{path}")
            .split("::")
            .map(str::trim)
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect();
        let local = if alias.is_empty() {
            target.last().map(String::as_str).unwrap_or("")
        } else {
            alias.trim()
        };
        if !target.is_empty()
            && target.iter().all(|segment| is_ident_text(segment))
            && is_ident_text(local)
        {
            out.push(UseBinding {
                local: local.to_owned(),
                target,
                glob: false,
            });
        }
    }
}

fn split_use_items(source: &str) -> Vec<&str> {
    let mut depth = 0;
    let mut start = 0;
    let mut out = Vec::new();
    for (index, character) in source.char_indices() {
        match character {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&source[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    out.push(&source[start..]);
    out
}

fn collect_declaration(
    path: &Path,
    text: &str,
    node: Node<'_>,
    module: &[String],
    out: &mut Vec<Declaration>,
) {
    let kind = match node.kind() {
        "function_item" => ItemKind::Function,
        "struct_item" => ItemKind::Class,
        "enum_item" => ItemKind::Enum,
        "trait_item" => ItemKind::Interface,
        "type_item" => ItemKind::Class,
        "const_item" | "static_item" => ItemKind::Constant,
        "mod_item" => ItemKind::Class,
        _ => return,
    };
    let Some(name) = node.child_by_field_name("name") else {
        return;
    };
    let name_text = name.utf8_text(text.as_bytes()).unwrap_or("");
    if name_text.is_empty() {
        return;
    }
    let point = name.start_position();
    let signature = text[node.start_byte()..node.end_byte()]
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    out.push(Declaration {
        name: name_text.to_owned(),
        module: module.to_vec(),
        kind,
        path: path.to_path_buf(),
        line: point.row as u32,
        column: utf16_column(text, name.start_byte()),
        scope_depth: 0,
        signature,
    });
}

fn local_declaration(path: &Path, text: &str, name: &str, at: usize) -> Option<Declaration> {
    local_declarations(path, text, at)
        .into_iter()
        .filter(|binding| binding.name == name)
        .max_by_key(|binding| (binding.scope_depth, binding.line, binding.column))
}

fn local_declarations(path: &Path, text: &str, at: usize) -> Vec<Declaration> {
    let Some(tree) = parse_tree(text) else {
        return Vec::new();
    };
    let mut bindings = Vec::new();
    collect_local_bindings(path, text, tree.root_node(), at, 0, &mut bindings);
    bindings
}

fn collect_local_bindings(
    path: &Path,
    text: &str,
    node: Node<'_>,
    at: usize,
    depth: usize,
    out: &mut Vec<Declaration>,
) {
    if node.start_byte() > at || node.end_byte() < at {
        return;
    }
    if node.kind() == "function_item"
        && let Some(parameters) = node.child_by_field_name("parameters")
    {
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            collect_pattern_bindings(path, text, parameter, depth, out);
        }
    }
    if node.kind() == "parameters" {
        let mut cursor = node.walk();
        for parameter in node.named_children(&mut cursor) {
            collect_pattern_bindings(path, text, parameter, depth, out);
        }
    }
    if node.kind() == "closure_parameters" {
        let mut cursor = node.walk();
        for parameter in node.named_children(&mut cursor) {
            collect_pattern_bindings(path, text, parameter, depth, out);
        }
    }
    if node.kind() == "let_declaration"
        && node.start_byte() < at
        && let Some(pattern) = node
            .child_by_field_name("pattern")
            .or_else(|| node.named_child(0))
    {
        collect_pattern_bindings(path, text, pattern, depth, out);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if node.kind() == "block"
            && child.kind() == "let_declaration"
            && child.start_byte() < at
            && let Some(pattern) = child
                .child_by_field_name("pattern")
                .or_else(|| child.named_child(0))
        {
            collect_pattern_bindings(path, text, pattern, depth + 1, out);
        }
        if child.start_byte() <= at && child.end_byte() >= at {
            collect_local_bindings(path, text, child, at, depth + 1, out);
        }
    }
}

fn collect_pattern_bindings(
    path: &Path,
    text: &str,
    node: Node<'_>,
    depth: usize,
    out: &mut Vec<Declaration>,
) {
    if node.kind() == "identifier" {
        let name = node.utf8_text(text.as_bytes()).unwrap_or("");
        if !name.is_empty() {
            let point = node.start_position();
            out.push(Declaration {
                name: name.to_owned(),
                module: Vec::new(),
                kind: ItemKind::Variable,
                path: path.to_path_buf(),
                line: point.row as u32,
                column: utf16_column(text, node.start_byte()),
                scope_depth: depth,
                signature: format!("let {name}"),
            });
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_pattern_bindings(path, text, child, depth, out);
    }
}

fn collect_errors(node: Node<'_>, text: &str, out: &mut Vec<Problem>) {
    if out.len() >= MAX_PROBLEMS || !node.has_error() {
        return;
    }
    if node.is_error() || node.is_missing() {
        let start = node.start_position();
        let end = node.end_position();
        out.push(Problem {
            start_line: start.row as u32,
            start_column: utf16_column(text, node.start_byte()),
            end_line: end.row as u32,
            end_column: utf16_column(text, node.end_byte()),
            severity: Severity::Error,
            message: "Syntax error".to_owned(),
            unnecessary: false,
        });
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_errors(child, text, out);
    }
}

#[allow(dead_code)]
fn collect_unresolved(
    engine: &RustEngine,
    path: &Path,
    text: &str,
    node: Node<'_>,
    local_bindings: &HashSet<String>,
    out: &mut Vec<Problem>,
) {
    if out.len() >= MAX_PROBLEMS || node.has_error() {
        return;
    }
    if node.kind() == "identifier"
        && identifier_is_reference(node)
        && is_call_target(node)
        && !is_attribute_argument(node)
        && let Ok(name) = node.utf8_text(text.as_bytes())
        && !is_prelude_name(name)
        && !is_field_shorthand(node)
        && !is_lifetime_name(node, text)
        && !is_member_name(node, text)
        && !is_closure_binding(node, text)
        && !engine
            .use_bindings(path)
            .iter()
            .any(|binding| binding.local == name)
        && !is_match_binding(node, text)
        && !local_bindings.contains(name)
        && engine.resolve_nonlocal(path, name).is_empty()
    {
        let point = node.start_position();
        out.push(Problem {
            start_line: point.row as u32,
            start_column: utf16_column(text, node.start_byte()),
            end_line: point.row as u32,
            end_column: utf16_column(text, node.end_byte()),
            severity: Severity::Error,
            message: format!("cannot resolve `{name}`"),
            unnecessary: false,
        });
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_unresolved(engine, path, text, child, local_bindings, out);
    }
}

#[allow(dead_code)]
fn all_local_binding_names(node: Node<'_>, text: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    collect_all_local_binding_names(node, text, &mut names);
    names
}

#[allow(dead_code)]
fn collect_all_local_binding_names(node: Node<'_>, text: &str, names: &mut HashSet<String>) {
    match node.kind() {
        "let_declaration" => {
            if let Some(pattern) = node
                .child_by_field_name("pattern")
                .or_else(|| node.named_child(0))
            {
                collect_pattern_binding_names(pattern, text, names);
            }
        }
        "match_arm" => {
            if let Some(pattern) = node
                .child_by_field_name("pattern")
                .or_else(|| node.named_child(0))
            {
                collect_pattern_binding_names(pattern, text, names);
            }
        }
        "parameters" | "closure_parameters" => {
            let mut cursor = node.walk();
            for parameter in node.named_children(&mut cursor) {
                collect_pattern_binding_names(parameter, text, names);
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_all_local_binding_names(child, text, names);
    }
}

#[allow(dead_code)]
fn collect_pattern_binding_names(node: Node<'_>, text: &str, names: &mut HashSet<String>) {
    if node.kind() == "identifier" {
        if let Ok(name) = node.utf8_text(text.as_bytes()) {
            names.insert(name.to_owned());
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_pattern_binding_names(child, text, names);
    }
}

#[allow(dead_code)]
fn is_match_binding(node: Node<'_>, text: &str) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "match_arm" {
            let arm = &text[parent.start_byte()..parent.end_byte()];
            return current.start_byte() - parent.start_byte() < arm.find("=>").unwrap_or(0);
        }
        current = parent;
    }
    false
}

/// The native resolver does not model every Rust expression and pattern form.
/// Restrict unresolved diagnostics to direct calls, where a missing name is
/// actionable, instead of flooding Problems with pattern and metadata names.
#[allow(dead_code)]
fn is_call_target(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    parent.kind() == "call_expression"
        && parent
            .child_by_field_name("function")
            .map(|function| function.id())
            == Some(node.id())
}

/// Names supplied by Rust's standard prelude have no local declaration for the
/// lightweight resolver to find. Keeping this list to prelude items preserves
/// unresolved diagnostics for ordinary project symbols.
#[allow(dead_code)]
fn is_prelude_name(name: &str) -> bool {
    matches!(
        name,
        "Some"
            | "None"
            | "Ok"
            | "Err"
            | "Option"
            | "Result"
            | "String"
            | "Vec"
            | "Box"
            | "Default"
            | "Clone"
            | "Copy"
            | "Drop"
            | "Eq"
            | "Ord"
            | "PartialEq"
            | "PartialOrd"
            | "Send"
            | "Sized"
            | "Sync"
            | "assert"
            | "assert_eq"
            | "assert_ne"
            | "debug_assert"
            | "debug_assert_eq"
            | "debug_assert_ne"
            | "panic"
            | "todo"
            | "unimplemented"
            | "unreachable"
            | "format"
            | "println"
            | "eprintln"
            | "print"
            | "eprint"
            | "vec"
            | "drop"
    )
}

/// `Entry { uri }` reuses the surrounding `uri` binding; Tree-sitter exposes
/// its only identifier as a field initializer, not as a local declaration.
#[allow(dead_code)]
fn is_field_shorthand(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    parent.kind() == "field_initializer" && parent.named_child_count() == 1
}

#[allow(dead_code)]
fn is_lifetime_name(node: Node<'_>, text: &str) -> bool {
    node.start_byte() > 0 && text.as_bytes().get(node.start_byte() - 1) == Some(&b'\'')
}

/// Member names are resolved from a receiver type, which is intentionally
/// outside this name-only resolver. Their receiver remains checked normally.
#[allow(dead_code)]
fn is_member_name(node: Node<'_>, text: &str) -> bool {
    text.as_bytes().get(node.start_byte().saturating_sub(1)) == Some(&b'.')
}

/// Closure parameters are lexical bindings. The body identifier has a normal
/// expression parent, so find the enclosing closure and compare its parameters.
#[allow(dead_code)]
fn is_closure_binding(node: Node<'_>, text: &str) -> bool {
    let Ok(name) = node.utf8_text(text.as_bytes()) else {
        return false;
    };
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "closure_expression" {
            let mut cursor = parent.walk();
            for child in parent.named_children(&mut cursor) {
                if child.kind() == "closure_parameters" && contains_identifier(child, text, name) {
                    return true;
                }
            }
            return false;
        }
        current = parent.parent();
    }
    false
}

#[allow(dead_code)]
fn contains_identifier(node: Node<'_>, text: &str, name: &str) -> bool {
    if node.kind() == "identifier" && node.utf8_text(text.as_bytes()).ok() == Some(name) {
        return true;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .any(|child| contains_identifier(child, text, name))
}

/// Attribute names and arguments may be procedural macros or compiler-builtins.
/// The local resolver cannot expand them, so reporting them as unresolved floods
/// the editor with false positives for common `#[derive(...)]` declarations.
#[allow(dead_code)]
fn is_attribute_argument(node: Node<'_>) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "attribute_item" {
            return true;
        }
        current = parent.parent();
    }
    false
}

#[allow(dead_code)]
fn identifier_is_reference(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    !matches!(
        parent.kind(),
        "function_item"
            | "struct_item"
            | "enum_item"
            | "trait_item"
            | "type_item"
            | "mod_item"
            | "field_declaration"
            | "parameter"
            | "closure_parameters"
            | "let_declaration"
            | "use_as_clause"
            | "scoped_identifier"
    ) && parent.child_by_field_name("name").map(|name| name.id()) != Some(node.id())
}

fn identifier_count(text: &str, name: &str) -> usize {
    let Some(tree) = parse_tree(text) else {
        return 0;
    };
    let mut count = 0;
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier" && node.utf8_text(text.as_bytes()).ok() == Some(name) {
            count += 1;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    count
}

fn identifier_at(text: &str, line: u32, column: u32) -> Option<String> {
    identifier_range(text, line, column).map(|range| range.0)
}

fn identifier_range(text: &str, line: u32, column: u32) -> Option<(String, u32, u32)> {
    let start = line_start(text, line)?;
    let end = text[start..]
        .find('\n')
        .map_or(text.len(), |offset| start + offset);
    let byte = byte_at_column(&text[start..end], column) + start;
    let left = text[..byte]
        .char_indices()
        .rev()
        .find(|(_, c)| !is_ident(*c))
        .map_or(0, |(at, c)| at + c.len_utf8());
    let right = text[byte..]
        .char_indices()
        .find(|(_, c)| !is_ident(*c))
        .map_or(text.len(), |(at, _)| byte + at);
    (left < right).then(|| {
        (
            text[left..right].to_owned(),
            utf16_column(text, left),
            utf16_column(text, right),
        )
    })
}

fn word_before(text: &str, line: u32, column: u32) -> &str {
    let Some(start) = line_start(text, line) else {
        return "";
    };
    let end = text[start..]
        .find('\n')
        .map_or(text.len(), |offset| start + offset);
    let byte = start + byte_at_column(&text[start..end], column);
    let word_start = text[..byte]
        .char_indices()
        .rev()
        .find(|(_, c)| !is_ident(*c))
        .map_or(0, |(at, c)| at + c.len_utf8());
    &text[word_start..byte]
}

fn member_access_before(text: &str, line: u32, column: u32) -> bool {
    let byte = cursor_byte(text, line, column);
    text[..byte].trim_end().ends_with('.')
        || text[..byte]
            .rsplit_once('.')
            .is_some_and(|(_, right)| right.chars().all(is_ident))
}

fn receiver_type(before: &str, receiver: &str) -> Option<String> {
    let binding = format!("let {receiver}:");
    if let Some((_, after)) = before.rsplit_once(&binding) {
        return after
            .trim_start()
            .split(|character: char| !is_ident(character))
            .next()
            .filter(|ty| !ty.is_empty())
            .map(str::to_owned);
    }
    let construction = format!("let {receiver} = ");
    before
        .rsplit_once(&construction)
        .and_then(|(_, after)| {
            after
                .split(|character: char| !is_ident(character))
                .next()
                .filter(|ty| !ty.is_empty())
                .map(str::to_owned)
        })
        .or_else(|| parameter_type(before, receiver))
}

fn parameter_type(before: &str, receiver: &str) -> Option<String> {
    let function = before.rsplit_once("fn ")?.1;
    let parameters = function.split_once('(')?.1.split_once(')')?.0;
    parameters.split(',').find_map(|parameter| {
        let (name, ty) = parameter.trim().split_once(':')?;
        (name.trim().trim_start_matches("mut ") == receiver)
            .then(|| {
                ty.trim()
                    .trim_start_matches('&')
                    .trim_start_matches("mut ")
                    .split(|character: char| !is_ident(character))
                    .next()
                    .unwrap_or("")
                    .to_owned()
            })
            .filter(|ty| !ty.is_empty())
    })
}

fn struct_fields(text: &str, ty: &str) -> Vec<Completion> {
    let Some((_, rest)) = text.split_once(&format!("struct {ty}")) else {
        return Vec::new();
    };
    let Some(body) = rest
        .split_once('{')
        .and_then(|(_, after)| after.split_once('}').map(|(body, _)| body))
    else {
        return Vec::new();
    };
    body.lines()
        .filter_map(|line| {
            line.trim()
                .split_once(':')
                .map(|(name, value)| completion(name.trim(), value.trim(), ItemKind::Variable))
        })
        .collect()
}

fn impl_methods(text: &str, ty: &str) -> Vec<Completion> {
    let Some((_, rest)) = text.split_once(&format!("impl {ty}")) else {
        return Vec::new();
    };
    let Some(body) = rest
        .split_once('{')
        .and_then(|(_, after)| after.split_once('}').map(|(body, _)| body))
    else {
        return Vec::new();
    };
    body.lines()
        .filter_map(|line| {
            let line = line.trim();
            let name = line
                .strip_prefix("pub fn ")
                .or_else(|| line.strip_prefix("fn "))?
                .split('(')
                .next()?;
            Some(completion(name, line, ItemKind::Function))
        })
        .collect()
}

fn member_declarations(path: &Path, text: &str, ty: &str, name: &str) -> Vec<Declaration> {
    let Some(tree) = parse_tree(text) else {
        return Vec::new();
    };
    let mut members = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "field_declaration" => {
                if struct_for_member(node, text).as_deref() != Some(ty) {
                    continue;
                }
                push_member_declaration(path, text, node, name, ItemKind::Variable, &mut members);
            }
            "function_item" if impl_for_member(node, text).as_deref() == Some(ty) => {
                push_member_declaration(path, text, node, name, ItemKind::Function, &mut members);
            }
            _ => {}
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    members.extend(struct_field_declarations(path, text, ty, name));
    members
}

fn struct_field_declarations(path: &Path, text: &str, ty: &str, name: &str) -> Vec<Declaration> {
    let Some((_, rest)) = text.split_once(&format!("struct {ty}")) else {
        return Vec::new();
    };
    let Some((_, body)) = rest.split_once('{') else {
        return Vec::new();
    };
    let Some((body, _)) = body.split_once('}') else {
        return Vec::new();
    };
    let Some(offset) = body
        .find(name)
        .filter(|offset| body[*offset + name.len()..].trim_start().starts_with(':'))
    else {
        return Vec::new();
    };
    let start = text.find(body).unwrap_or(0) + offset;
    let point = text[..start].lines().count().saturating_sub(1) as u32;
    vec![Declaration {
        name: name.to_owned(),
        module: Vec::new(),
        kind: ItemKind::Variable,
        path: path.to_path_buf(),
        line: point,
        column: utf16_column(text, start),
        scope_depth: 0,
        signature: body.trim().to_owned(),
    }]
}

fn push_member_declaration(
    path: &Path,
    text: &str,
    node: Node<'_>,
    name: &str,
    kind: ItemKind,
    out: &mut Vec<Declaration>,
) {
    let Some(member_name) = node.child_by_field_name("name").or_else(|| {
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .find(|child| matches!(child.kind(), "identifier" | "field_identifier"))
    }) else {
        return;
    };
    if member_name.utf8_text(text.as_bytes()).ok() != Some(name) {
        return;
    }
    let point = member_name.start_position();
    out.push(Declaration {
        name: name.to_owned(),
        module: Vec::new(),
        kind,
        path: path.to_path_buf(),
        line: point.row as u32,
        column: utf16_column(text, member_name.start_byte()),
        scope_depth: 0,
        signature: text[node.start_byte()..node.end_byte()]
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_owned(),
    });
}

fn struct_for_member(node: Node<'_>, text: &str) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "struct_item" {
            return parent
                .child_by_field_name("name")?
                .utf8_text(text.as_bytes())
                .ok()
                .map(str::to_owned);
        }
        current = parent.parent();
    }
    None
}

fn impl_for_member(node: Node<'_>, text: &str) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "impl_item" {
            let ty = parent.child_by_field_name("type")?;
            return ty.utf8_text(text.as_bytes()).ok().map(str::trim).map(|ty| {
                ty.trim_start_matches('&')
                    .trim_start_matches("mut ")
                    .to_owned()
            });
        }
        current = parent.parent();
    }
    None
}

fn completion(name: &str, detail: &str, kind: ItemKind) -> Completion {
    Completion {
        label: name.to_owned(),
        detail: Some(detail.to_owned()),
        insert_text: name.to_owned(),
        is_snippet: false,
        kind,
        sort: name.to_owned(),
        extra: Vec::new(),
        replace_from: None,
    }
}

fn call_at(text: &str, byte: usize) -> Option<(String, u32)> {
    let tree = parse_tree(text)?;
    let call = deepest_call(tree.root_node(), byte)?;
    let function = call.child_by_field_name("function")?;
    let name = function
        .utf8_text(text.as_bytes())
        .ok()?
        .rsplit("::")
        .next()?
        .trim();
    if name.is_empty() {
        return None;
    }
    let arguments = call.child_by_field_name("arguments")?;
    let mut active = 0;
    let mut cursor = arguments.walk();
    for argument in arguments.named_children(&mut cursor) {
        if argument.end_byte() < byte {
            active += 1;
        }
    }
    Some((name.to_owned(), active))
}

/// Calls on lines `first..=last`: the called name, whether it is called
/// by path (`Type::f(x)`, where `self` is an argument), and the arguments.
fn rust_call_sites(
    text: &str,
    root: Node<'_>,
    first: u32,
    last: u32,
) -> Vec<(String, bool, Vec<Argument>)> {
    let (first, last) = (first as usize, last as usize);
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.end_position().row < first || node.start_position().row > last {
            continue;
        }
        if node.kind() == "call_expression"
            && out.len() < parameter_hints::MAX_CALLS
            && let Some(call) = rust_call_site(text, node)
        {
            out.push(call);
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    out
}

fn rust_call_site(text: &str, call: Node<'_>) -> Option<(String, bool, Vec<Argument>)> {
    let mut function = call.child_by_field_name("function")?;
    if function.kind() == "generic_function" {
        function = function.child_by_field_name("function")?;
    }
    let (name, associated) = match function.kind() {
        "identifier" => (function, false),
        "scoped_identifier" => (function.child_by_field_name("name")?, true),
        _ => return None,
    };
    let name = name.utf8_text(text.as_bytes()).ok()?;
    // Tuple structs and enum variants (`Some(x)`) take no named parameters.
    if name.starts_with(|c: char| c.is_uppercase()) {
        return None;
    }
    let list = call.child_by_field_name("arguments")?;
    let mut cursor = list.walk();
    let arguments: Vec<_> = list
        .named_children(&mut cursor)
        .filter(|argument| {
            !matches!(
                argument.kind(),
                "attribute_item" | "line_comment" | "block_comment"
            )
        })
        .map(|argument| {
            let value = match argument.kind() {
                "reference_expression" => argument.child_by_field_name("value").unwrap_or(argument),
                _ => argument,
            };
            let named = match value.kind() {
                "identifier" => Some(value),
                "field_expression" => value.child_by_field_name("field"),
                _ => None,
            };
            Argument {
                line: argument.start_position().row as u32,
                column: utf16_column(text, argument.start_byte()),
                name: named
                    .and_then(|node| node.utf8_text(text.as_bytes()).ok())
                    .map(str::to_owned),
                spread: false,
                named: false,
            }
        })
        .collect();
    (!arguments.is_empty()).then(|| (name.to_owned(), associated, arguments))
}

/// Parameters of the function whose name is at `line`/`column` of `text`.
/// `self` counts only when the function is called by path.
fn rust_parameters_at(
    text: &str,
    line: u32,
    column: u32,
    associated: bool,
) -> Option<Vec<Parameter>> {
    let tree = parse_tree(text)?;
    let byte = cursor_byte(text, line, column);
    let mut node = tree.root_node().descendant_for_byte_range(byte, byte)?;
    while !matches!(node.kind(), "function_item" | "function_signature_item") {
        node = node.parent()?;
    }
    let list = node.child_by_field_name("parameters")?;
    let mut cursor = list.walk();
    let mut out = Vec::new();
    for parameter in list.named_children(&mut cursor) {
        match parameter.kind() {
            "self_parameter" if associated => out.push(Parameter {
                name: "self".to_owned(),
                rest: false,
            }),
            "parameter" => {
                let pattern = parameter.child_by_field_name("pattern")?;
                let pattern = if pattern.kind() == "mut_pattern" {
                    let mut inner = pattern.walk();
                    pattern.named_children(&mut inner).last().unwrap_or(pattern)
                } else {
                    pattern
                };
                let name = if pattern.kind() == "identifier" {
                    pattern
                        .utf8_text(text.as_bytes())
                        .ok()?
                        .trim_start_matches('_')
                } else {
                    ""
                };
                out.push(Parameter {
                    name: name.to_owned(),
                    rest: false,
                });
            }
            _ => {}
        }
    }
    Some(out)
}

/// Type of a number written in a `let`. A suffix (`89u64`) names the type.
/// Without one the type is only the default (`i32`, `f64`) when nothing
/// later in the block could pick another: any use of the variable outside a
/// formatting macro may.
fn numeric_literal_type(
    text: &str,
    declaration: Node<'_>,
    value: Node<'_>,
    name: &str,
) -> Option<String> {
    let mut literal = value;
    if matches!(value.kind(), "unary_expression" | "negative_literal") {
        if !text[value.start_byte()..value.end_byte()].starts_with('-') {
            return None;
        }
        let mut cursor = value.walk();
        literal = value
            .named_children(&mut cursor)
            .find(|child| matches!(child.kind(), "integer_literal" | "float_literal"))?;
    }
    let source = literal.utf8_text(text.as_bytes()).ok()?;
    const INTEGERS: [&str; 12] = [
        "u128", "i128", "usize", "isize", "u16", "u32", "u64", "i16", "i32", "i64", "u8", "i8",
    ];
    if let Some(suffix) = INTEGERS.iter().find(|suffix| source.ends_with(**suffix)) {
        return Some((*suffix).to_owned());
    }
    let hex = source.starts_with("0x") || source.starts_with("0X");
    if !hex && (source.ends_with("f32") || source.ends_with("f64")) {
        return Some(source[source.len() - 3..].to_owned());
    }
    if !only_printed_after(text, declaration, name) {
        return None;
    }
    let float = literal.kind() == "float_literal";
    Some(if float { "f64" } else { "i32" }.to_owned())
}

/// True when every later use of `name` in the enclosing block is an argument
/// of a macro that only formats it.
fn only_printed_after(text: &str, declaration: Node<'_>, name: &str) -> bool {
    const FORMATTING: [&str; 8] = [
        "println", "print", "eprintln", "eprint", "format", "dbg", "write", "writeln",
    ];
    let Some(block) = declaration.parent() else {
        return false;
    };
    let mut stack = vec![block];
    while let Some(node) = stack.pop() {
        if node.end_byte() <= declaration.end_byte() {
            continue;
        }
        if node.kind() == "identifier" && node.utf8_text(text.as_bytes()).ok() == Some(name) {
            let mut ancestor = node.parent();
            while ancestor.is_some_and(|parent| parent.kind() == "token_tree") {
                ancestor = ancestor.and_then(|parent| parent.parent());
            }
            let formatted = ancestor
                .filter(|parent| parent.kind() == "macro_invocation")
                .and_then(|parent| parent.child_by_field_name("macro"))
                .and_then(|mac| mac.utf8_text(text.as_bytes()).ok())
                .and_then(|mac| mac.rsplit("::").next())
                .is_some_and(|mac| FORMATTING.contains(&mac));
            if !formatted {
                return false;
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    true
}

/// The return type written on the function whose name is at `line`/`column`.
/// `owner` replaces `Self` for associated calls (`Coffee::new()`). Generic,
/// `async` and `impl Trait` signatures give no answer.
fn rust_return_type_at(text: &str, line: u32, column: u32, owner: Option<&str>) -> Option<String> {
    let tree = parse_tree(text)?;
    let byte = cursor_byte(text, line, column);
    let mut node = tree.root_node().descendant_for_byte_range(byte, byte)?;
    while node.kind() != "function_item" {
        node = node.parent()?;
    }
    if node.child_by_field_name("type_parameters").is_some() {
        return None;
    }
    let mut cursor = node.walk();
    let asynchronous = node.named_children(&mut cursor).any(|child| {
        child.kind() == "function_modifiers"
            && child
                .utf8_text(text.as_bytes())
                .is_ok_and(|m| m.contains("async"))
    });
    if asynchronous {
        return None;
    }
    let written = node
        .child_by_field_name("return_type")?
        .utf8_text(text.as_bytes())
        .ok()?
        .trim();
    if written.contains("impl ") || written.contains("dyn ") || written.contains('\'') {
        return None;
    }
    if !written
        .split(|c: char| !is_ident(c))
        .any(|word| word == "Self")
    {
        return Some(written.to_owned());
    }
    let owner =
        owner.filter(|owner| owner.starts_with(|c: char| c.is_uppercase()) && *owner != "Self")?;
    let mut out = String::new();
    let mut word = String::new();
    for character in written.chars().chain(std::iter::once(' ')) {
        if is_ident(character) {
            word.push(character);
            continue;
        }
        out.push_str(if word == "Self" { owner } else { &word });
        word.clear();
        out.push(character);
    }
    out.pop();
    Some(out)
}

fn deepest_call(node: Node<'_>, byte: usize) -> Option<Node<'_>> {
    if node.start_byte() > byte || node.end_byte() < byte {
        return None;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(call) = deepest_call(child, byte) {
            return Some(call);
        }
    }
    (node.kind() == "call_expression").then_some(node)
}

fn signature_parameters(signature: &str) -> Vec<String> {
    signature
        .split_once('(')
        .and_then(|(_, after)| after.split_once(')').map(|(parameters, _)| parameters))
        .map(|parameters| {
            parameters
                .split(',')
                .map(str::trim)
                .filter(|parameter| !parameter.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn line_start(text: &str, line: u32) -> Option<usize> {
    let mut start = 0;
    for _ in 0..line {
        start += text[start..].find('\n')? + 1;
    }
    Some(start)
}

fn byte_at_column(line: &str, column: u32) -> usize {
    let mut units = 0;
    for (offset, character) in line.char_indices() {
        if units >= column {
            return offset;
        }
        units += character.len_utf16() as u32;
    }
    line.len()
}

fn cursor_byte(text: &str, line: u32, column: u32) -> usize {
    line_start(text, line).map_or(text.len(), |start| {
        let end = text[start..]
            .find('\n')
            .map_or(text.len(), |offset| start + offset);
        start + byte_at_column(&text[start..end], column)
    })
}

fn utf16_column(text: &str, byte: usize) -> u32 {
    let line_start = text[..byte].rfind('\n').map_or(0, |offset| offset + 1);
    text[line_start..byte].encode_utf16().count() as u32
}

fn is_ident(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

fn is_ident_text(text: &str) -> bool {
    let mut characters = text.chars();
    matches!(characters.next(), Some(character) if is_ident(character)) && characters.all(is_ident)
}

fn item_kind_name(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Class => "Class",
        ItemKind::Interface => "Interface",
        ItemKind::Enum => "Enum",
        ItemKind::Function => "Function",
        ItemKind::Constant => "Constant",
        _ => "Variable",
    }
}

fn valid_name(name: &str) -> bool {
    let mut characters = name.chars();
    matches!(characters.next(), Some(character) if character == '_' || character.is_alphabetic())
        && characters.all(|character| character == '_' || character.is_alphanumeric())
        && !KEYWORDS.contains(&name)
}

fn item_occurrences(
    engine: &RustEngine,
    path: &Path,
    text: &str,
    name: &str,
    target: &Declaration,
) -> Vec<Occurrence> {
    let Some(tree) = parse_tree(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier"
            && node.utf8_text(text.as_bytes()).ok() == Some(name)
            && engine
                .resolve(path, name, node.start_byte())
                .first()
                .is_some_and(|found| {
                    found.path == target.path
                        && found.line == target.line
                        && found.column == target.column
                })
        {
            let point = node.start_position();
            out.push(Occurrence {
                path: path.to_path_buf(),
                line: point.row as u32,
                start_column: utf16_column(text, node.start_byte()),
                end_column: utf16_column(text, node.end_byte()),
                dollar: false,
            });
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    out.sort();
    out
}

fn local_occurrences(
    path: &Path,
    text: &str,
    name: &str,
    declaration: &Declaration,
) -> Vec<Occurrence> {
    let Some(tree) = parse_tree(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier"
            && node.utf8_text(text.as_bytes()).ok() == Some(name)
            && (utf16_column(text, node.start_byte()) == declaration.column
                && node.start_position().row as u32 == declaration.line
                || local_declaration(path, text, name, node.start_byte()).is_some_and(|found| {
                    found.line == declaration.line && found.column == declaration.column
                }))
        {
            let point = node.start_position();
            out.push(Occurrence {
                path: path.to_path_buf(),
                line: point.row as u32,
                start_column: utf16_column(text, node.start_byte()),
                end_column: utf16_column(text, node.end_byte()),
                dollar: false,
            });
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_buffer_provides_completion_definition_hover_and_outline() {
        let path = Path::new("/tmp/lib.rs");
        let text = "pub struct Coffee;\npub fn brew() {}\nfn main() { brew }";
        let column = text.lines().nth(2).unwrap().find("brew").unwrap() as u32 + 2;
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        assert_eq!(engine.complete(path, text, 2, column + 2)[0].label, "brew");
        assert_eq!(engine.definition(path, text, 2, column)[0].line, 1);
        assert!(
            engine
                .hover(path, text, 2, column)
                .unwrap()
                .markdown
                .contains("fn brew")
        );
        assert_eq!(engine.outline(path, text).len(), 3);
    }

    #[test]
    fn reports_rust_syntax_errors() {
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        assert!(
            !engine
                .diagnostics(Path::new("/tmp/lib.rs"), "fn broken(")
                .is_empty()
        );
    }

    #[test]
    fn disk_delta_replaces_and_removes_file_declarations() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("src");
        std::fs::create_dir_all(&source).unwrap();
        let path = source.join("lib.rs");
        std::fs::write(&path, "pub fn old_name() {}\n").unwrap();
        let mut engine = RustEngine::pending(root.path());
        engine.install(RustEngine::build(root.path()));
        std::fs::write(&path, "pub fn new_name() {}\n").unwrap();
        assert!(engine.apply_disk_changes(std::slice::from_ref(&path)));
        assert!(
            engine
                .declarations
                .iter()
                .any(|item| item.name == "new_name")
        );
        assert!(
            !engine
                .declarations
                .iter()
                .any(|item| item.name == "old_name")
        );
        std::fs::remove_file(&path).unwrap();
        assert!(engine.apply_disk_changes(std::slice::from_ref(&path)));
        assert!(!engine.declarations.iter().any(|item| item.path == path));
    }

    #[test]
    fn references_and_rename_require_a_unique_top_level_declaration() {
        let path = Path::new("/tmp/lib.rs");
        let text = "pub fn brew() {}\nfn main() { brew(); }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        let uses = engine.references(path, text, 1, 13);
        assert_eq!(uses.len(), 2);
        assert_eq!(
            engine
                .rename(path, text, 1, 13, "make_coffee")
                .unwrap()
                .len(),
            2
        );
        assert!(engine.rename(path, text, 1, 13, "fn").is_err());
    }

    #[test]
    fn imported_alias_resolves_to_its_unique_declaration() {
        let definition = PathBuf::from("/tmp/models.rs");
        let importer = Path::new("/tmp/lib.rs");
        let source = "use crate::models::Coffee as Drink;\nfn main() { Drink; }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built {
            declarations: vec![Declaration {
                name: "Coffee".to_owned(),
                module: vec!["models".to_owned()],
                kind: ItemKind::Class,
                path: definition.clone(),
                line: 0,
                column: 11,
                scope_depth: 0,
                signature: "pub struct Coffee;".to_owned(),
            }],
            files: Vec::new(),
            uses: HashMap::new(),
            source_roots: Vec::new(),
        });
        assert_eq!(
            engine.definition(importer, source, 1, 14)[0].path,
            definition
        );
        assert!(
            engine
                .hover(importer, source, 1, 14)
                .unwrap()
                .markdown
                .contains("Coffee")
        );
    }

    #[test]
    fn module_paths_distinguish_same_named_items() {
        let root = Path::new("/project/src");
        let mut engine = RustEngine::pending(Path::new("/project"));
        engine.install(Built {
            declarations: vec![
                Declaration {
                    name: "Config".to_owned(),
                    module: vec!["client".to_owned()],
                    kind: ItemKind::Class,
                    path: PathBuf::from("/project/src/client.rs"),
                    line: 0,
                    column: 11,
                    scope_depth: 0,
                    signature: "pub struct Config;".to_owned(),
                },
                Declaration {
                    name: "Config".to_owned(),
                    module: vec!["server".to_owned()],
                    kind: ItemKind::Class,
                    path: PathBuf::from("/project/src/server.rs"),
                    line: 0,
                    column: 11,
                    scope_depth: 0,
                    signature: "pub struct Config;".to_owned(),
                },
            ],
            files: Vec::new(),
            uses: HashMap::new(),
            source_roots: vec![root.to_path_buf()],
        });
        let source = "use crate::server::Config;\nfn main() { Config; }";
        assert_eq!(
            engine.definition(Path::new("/project/src/lib.rs"), source, 1, 14)[0].path,
            PathBuf::from("/project/src/server.rs")
        );
    }

    #[test]
    fn lexical_scope_prefers_the_nearest_binding() {
        let path = Path::new("/tmp/lib.rs");
        let source =
            "fn brew(coffee: u8) {\n let coffee = 2;\n { let coffee = 3; coffee; }\n coffee;\n}";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        assert_eq!(engine.definition(path, source, 2, 22)[0].line, 2);
        assert_eq!(engine.definition(path, source, 3, 2)[0].line, 1);
    }

    #[test]
    fn grouped_imports_and_repeated_super_resolve_the_full_path() {
        let root = Path::new("/project/src");
        let mut engine = RustEngine::pending(Path::new("/project"));
        engine.install(Built {
            declarations: vec![Declaration {
                name: "Coffee".to_owned(),
                module: vec!["models".to_owned(), "drink".to_owned()],
                kind: ItemKind::Class,
                path: PathBuf::from("/project/src/models/drink.rs"),
                line: 0,
                column: 11,
                scope_depth: 0,
                signature: "pub struct Coffee;".to_owned(),
            }],
            files: Vec::new(),
            uses: HashMap::new(),
            source_roots: vec![root.to_path_buf()],
        });
        let source = "use crate::models::{drink::{Coffee as Bean}};\nfn main() { Bean; }";
        assert_eq!(
            engine.definition(Path::new("/project/src/lib.rs"), source, 1, 14)[0].path,
            PathBuf::from("/project/src/models/drink.rs")
        );
        let nested = Path::new("/project/src/models/drink/nested.rs");
        let source = "use super::super::drink::Coffee;\nfn main() { Coffee; }";
        assert_eq!(
            engine.definition(nested, source, 1, 14)[0].path,
            PathBuf::from("/project/src/models/drink.rs")
        );
    }

    #[test]
    fn rename_local_binding_excludes_shadowed_bindings() {
        let path = Path::new("/tmp/lib.rs");
        let source = "fn main() {\n let coffee = 1;\n { let coffee = 2; coffee; }\n coffee;\n}";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        let references = engine.references(path, source, 2, 21);
        assert_eq!(references.len(), 2);
        assert_eq!(references[0].line, 2);
        assert_eq!(references[1].line, 2);
        assert_eq!(
            engine
                .rename(path, source, 2, 21, "espresso")
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn glob_import_and_item_references_follow_the_resolved_declaration() {
        let root = Path::new("/project/src");
        let target = PathBuf::from("/project/src/models.rs");
        let source_path = Path::new("/project/src/lib.rs");
        let mut engine = RustEngine::pending(Path::new("/project"));
        engine.install(Built {
            declarations: vec![Declaration {
                name: "Coffee".to_owned(),
                module: vec!["models".to_owned()],
                kind: ItemKind::Class,
                path: target.clone(),
                line: 0,
                column: 11,
                scope_depth: 0,
                signature: "pub struct Coffee;".to_owned(),
            }],
            files: vec![target],
            uses: HashMap::new(),
            source_roots: vec![root.to_path_buf()],
        });
        let source = "use crate::models::*;\nfn main() { Coffee; }";
        assert_eq!(
            engine.definition(source_path, source, 1, 14)[0].path,
            PathBuf::from("/project/src/models.rs")
        );
    }

    #[test]
    fn member_completion_and_signature_use_local_type_annotations() {
        let path = Path::new("/tmp/lib.rs");
        let source = "struct Coffee { beans: u8 }\nimpl Coffee { fn brew(&self, minutes: u8) {} }\nfn main() { let coffee: Coffee = Coffee { beans: 1 }; coffee.br; brew(1, 2); }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        assert!(
            engine
                .complete(path, source, 2, 65)
                .iter()
                .any(|item| item.label == "brew")
        );
        let argument = source.lines().nth(2).unwrap().rfind('2').unwrap() as u32;
        let signature = engine.signature(path, source, 2, argument).unwrap();
        assert!(signature.label.contains("fn brew"));
        assert_eq!(signature.active, 1);
    }

    #[test]
    fn parameter_hints_name_function_arguments() {
        let path = Path::new("/tmp/hints.rs");
        let source = "fn brew(\n    minutes: u8,\n    mut cups: HashMap<u8, u8>,\n) {}\nstruct Pot;\nimpl Pot { fn fill(&self, litres: u8) {} }\nfn main() { brew(5, map); brew(minutes, &cups); Pot::fill(&pot, 2); Some(1); }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        let found = engine.parameter_hints(path, source, 6, 6);
        let line = source.lines().nth(6).unwrap();
        let labels: Vec<(u32, &str)> = found
            .iter()
            .map(|hint| (hint.column, hint.label.as_str()))
            .collect();
        assert_eq!(
            labels,
            vec![
                (line.find("5,").unwrap() as u32, "minutes:"),
                (line.find("map").unwrap() as u32, "cups:"),
                (line.find("&pot").unwrap() as u32, "self:"),
                (line.find("2)").unwrap() as u32, "litres:"),
            ]
        );
    }

    #[test]
    fn let_bindings_without_a_type_get_one() {
        let path = Path::new("/tmp/lets.rs");
        let source = concat!(
            "struct Coffee;\n",
            "impl Coffee { fn new() -> Self { Coffee } fn cups() -> Vec<Self> { vec![] } }\n",
            "fn count() -> usize { 3 }\n",
            "fn pick<T>(item: T) -> T { item }\n",
            "fn main() {\n",
            "    let a = 89;\n",
            "    let b = 89u64;\n",
            "    let c = -1.5;\n",
            "    let d = \"hi\";\n",
            "    let mut e = true;\n",
            "    let f = Coffee;\n",
            "    let g = count();\n",
            "    let h = Coffee::new();\n",
            "    let i = Coffee::cups();\n",
            "    let j = pick(1);\n",
            "    let k: u8 = 4;\n",
            "    let (l, m) = (1, 2);\n",
            "    let n = 5;\n",
            "    takes(n);\n",
            "    println!(\"{a} {}\", a);\n",
            "}\n",
        );
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        let found = engine.parameter_hints(path, source, 0, 40);
        let labels: Vec<(u32, &str)> = found
            .iter()
            .filter(|hint| hint.label.starts_with(':'))
            .map(|hint| (hint.line, hint.label.as_str()))
            .collect();
        assert_eq!(
            labels,
            vec![
                (5, ": i32"),
                (6, ": u64"),
                (7, ": f64"),
                (8, ": &str"),
                (9, ": bool"),
                (11, ": usize"),
                (12, ": Coffee"),
                (13, ": Vec<Coffee>"),
            ]
        );
        let line = source.lines().nth(5).unwrap();
        assert_eq!(
            found.iter().find(|hint| hint.line == 5).unwrap().column as usize,
            line.find(" =").unwrap()
        );
    }

    #[test]
    fn member_completion_uses_workspace_definitions() {
        let definition = PathBuf::from("/tmp/models.rs");
        let path = Path::new("/tmp/lib.rs");
        let source = "fn main() { let coffee: Coffee = todo!(); coffee.br }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built {
            declarations: Vec::new(),
            files: vec![definition.clone()],
            uses: HashMap::new(),
            source_roots: vec![PathBuf::from("/tmp")],
        });
        engine.sync_document(
            &definition,
            "pub struct Coffee { pub beans: u8 }\nimpl Coffee { pub fn brew(&self) {} }",
        );
        assert!(
            engine
                .complete(path, source, 0, 55)
                .iter()
                .any(|item| item.label == "brew")
        );
    }

    #[test]
    fn member_definition_resolves_methods_and_fields_in_another_buffer() {
        let definition = PathBuf::from("/tmp/models.rs");
        let path = Path::new("/tmp/lib.rs");
        let source = "fn main() { let coffee: Coffee = todo!(); coffee.brew(); coffee.beans; }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built {
            declarations: Vec::new(),
            files: vec![definition.clone()],
            uses: HashMap::new(),
            source_roots: vec![PathBuf::from("/tmp")],
        });
        engine.sync_document(
            &definition,
            "pub struct Coffee { pub beans: u8 }\nimpl Coffee {\n    pub fn brew(&self) {}\n}",
        );

        let method_column = source.find("brew").unwrap() as u32 + 1;
        let method = engine.definition(path, source, 0, method_column);
        assert_eq!(method.len(), 1);
        assert_eq!(method[0].path, definition);
        assert_eq!((method[0].line, method[0].column), (2, 11));

        let field_column = source.rfind("beans").unwrap() as u32 + 1;
        let field = engine.definition(path, source, 0, field_column);
        assert_eq!(field.len(), 1);
        assert_eq!(field[0].path, definition);
        assert_eq!((field[0].line, field[0].column), (0, 24));
    }

    #[test]
    fn member_completion_infers_receiver_type_from_function_parameter() {
        let path = Path::new("/tmp/lib.rs");
        let source = "struct Coffee { beans: u8 }\nimpl Coffee { fn brew(&self) {} }\nfn serve(coffee: &Coffee) { coffee.br }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        assert!(
            engine
                .complete(path, source, 2, 40)
                .iter()
                .any(|item| item.label == "brew")
        );
    }

    #[test]
    fn signature_help_tracks_the_innermost_nested_call() {
        let path = Path::new("/tmp/lib.rs");
        let source = "fn brew(minutes: u8, temperature: u8) {}\nfn grind(beans: u8, setting: u8) {}\nfn main() { brew(grind(1, 2), 3); }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        let inner = engine.signature(path, source, 2, 26).unwrap();
        assert!(inner.label.contains("fn grind"));
        assert_eq!(inner.active, 1);
        let outer = engine.signature(path, source, 2, 31).unwrap();
        assert!(outer.label.contains("fn brew"));
        assert_eq!(outer.active, 1);
    }

    #[test]
    fn offers_unused_import_removal_without_name_heuristic_diagnostics() {
        let path = Path::new("/tmp/lib.rs");
        let source = "use crate::models::Coffee;\nfn main() { missing(); }\n";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        assert!(engine.diagnostics(path, source).is_empty());
        let actions = engine.actions(path, source, 0, 0);
        assert_eq!(actions[0].title, "Remove unused import `Coffee`");
    }

    #[test]
    fn does_not_report_builtin_derive_attributes_as_unresolved() {
        let path = Path::new("/tmp/lib.rs");
        let source = "#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub struct Range;\n";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        assert!(engine.diagnostics(path, source).is_empty());
    }

    #[test]
    fn does_not_report_prelude_closure_or_field_shorthand_as_unresolved() {
        let path = Path::new("/tmp/lib.rs");
        let source = "struct Entry<'a> { uri: &'a str }\nfn main() {\n    let uri = \"file:///tmp\";\n    let _entry = Entry { uri };\n    let value = Some(uri);\n    assert!(value.is_some());\n    [1].iter().map(|d| d + 1).for_each(drop);\n}\n";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        let diagnostics = engine.diagnostics(path, source);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn does_not_report_imported_or_match_bound_names_as_unresolved() {
        let path = Path::new("/tmp/lib.rs");
        let source = "use tree_sitter::Node;\nfn inspect(value: &str) {\n    Node;\n    match value {\n        comment => drop(comment),\n    }\n}\n";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        let diagnostics = engine.diagnostics(path, source);
        assert!(
            diagnostics
                .iter()
                .all(|problem| !problem.message.contains("Node")
                    && !problem.message.contains("comment")),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn diagnostics_do_not_report_pattern_or_unknown_names() {
        let path = Path::new("/tmp/lib.rs");
        let source = "fn inspect(project: Option<&str>) {\n    if let Some(comment) = project {\n        for child in comment.chars() {\n            let _ = child;\n        }\n    } else {\n        missing();\n    }\n}\n";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        let diagnostics = engine.diagnostics(path, source);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn formatter_only_normalizes_indentation_for_valid_syntax() {
        let engine = RustEngine::pending(Path::new("/tmp"));
        assert_eq!(
            engine.format("fn main() {\nlet coffee = 1;  \n}\n"),
            Some("fn main() {\n    let coffee = 1;\n}\n".to_owned())
        );
        assert_eq!(
            structural_braces("let label = \"{not a block}\"; // }"),
            (0, 0)
        );
        assert_eq!(engine.format("fn main("), None);
    }
}
