//! Bounded, native Dart and Flutter project navigation.
//!
//! This is intentionally a lexical resolver. It indexes only the application
//! package, never follows SDK or pub-cache imports, and declines ambiguous
//! answers rather than guessing.

mod flutter;
mod hints;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use tree_sitter::{Node, Parser};

use crate::lang::{
    Completion, HoverResult, Insertion, ItemKind, Occurrence, OutlineSymbol, Problem, Severity,
    Target,
};

const MAX_FILES: usize = 4_000;
const MAX_FILE_BYTES: u64 = 2 << 20;
const MAX_ITEMS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Declaration {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) line: u32,
    pub(crate) column: u32,
    pub(crate) kind: ItemKind,
    pub(crate) signature: String,
    pub(crate) owner: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct Import {
    path: PathBuf,
    prefix: Option<String>,
    show: Option<HashSet<String>>,
    hide: HashSet<String>,
}

#[derive(Debug, Clone, Default)]
struct FileIndex {
    declarations: Vec<Declaration>,
    imports: Vec<Import>,
    parts: Vec<PathBuf>,
    part_of: Option<String>,
}

#[derive(Debug, Default)]
pub struct Built {
    files: HashMap<PathBuf, FileIndex>,
    sources: HashMap<PathBuf, String>,
    packages: HashMap<String, PathBuf>,
}

#[derive(Debug, Default)]
pub struct DartEngine {
    files: HashMap<PathBuf, FileIndex>,
    sources: HashMap<PathBuf, String>,
    packages: HashMap<String, PathBuf>,
    ready: bool,
}

impl DartEngine {
    pub fn index_stats(&self) -> crate::LanguageIndexStats {
        let symbols = self
            .files
            .values()
            .map(|file| file.declarations.len())
            .sum();
        let imports = self.files.values().map(|file| file.imports.len()).sum();
        let source_bytes = self.sources.values().map(String::len).sum();
        crate::LanguageIndexStats {
            files: self.files.len(),
            symbols,
            imports,
            source_bytes,
            heap_lower_bound_bytes: self.files.len()
                * (std::mem::size_of::<PathBuf>() + std::mem::size_of::<FileIndex>())
                + source_bytes,
            ..Default::default()
        }
    }

    pub fn pending(root: &Path) -> Self {
        Self {
            packages: package_roots(root, package_name(root).as_deref()),
            ..Self::default()
        }
    }

    pub fn build(root: &Path) -> Built {
        Self::build_with_scheduler(root, crate::index_scheduler::shared_index_scheduler())
    }

    pub fn build_with_scheduler(
        root: &Path,
        scheduler: &crate::index_scheduler::IndexScheduler,
    ) -> Built {
        let package = package_name(root);
        let mut built = Built {
            packages: package_roots(root, package.as_deref()),
            ..Built::default()
        };
        let mut roots = vec![root.join("lib")];
        roots.extend(built.packages.values().cloned());
        roots.sort();
        roots.dedup();
        let mut candidates = Vec::new();
        for source_root in roots {
            let mut walker = WalkBuilder::new(&source_root);
            walker
                .hidden(false)
                .git_ignore(true)
                .git_global(true)
                .parents(true);
            for entry in walker.build().flatten() {
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("dart")
                    || entry
                        .metadata()
                        .is_ok_and(|metadata| metadata.len() > MAX_FILE_BYTES)
                {
                    continue;
                }
                candidates.push(path.to_path_buf());
            }
        }
        candidates.sort();
        candidates.dedup();
        candidates.truncate(MAX_FILES);
        let indexed = scheduler.map_ordered(candidates, |path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let index = index_file(&path, &text, root, &built.packages);
            Some((path, index, text))
        });
        for (path, index, text) in indexed.into_iter().flatten() {
            built.files.insert(path.clone(), index);
            built.sources.insert(path, text);
        }
        built
    }

    pub fn install(&mut self, built: Built) {
        self.files = built.files;
        self.sources = built.sources;
        self.packages = built.packages;
        self.ready = true;
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Applies a bounded watcher delta without walking the package again.
    /// Metadata changes affect package resolution and must use `build` instead.
    pub fn apply_disk_changes(&mut self, root: &Path, paths: &[PathBuf]) -> bool {
        if !self.ready
            || paths
                .iter()
                .any(|path| path.file_name().and_then(|name| name.to_str()) == Some("pubspec.yaml"))
        {
            return false;
        }
        for path in paths {
            if path.extension().and_then(|value| value.to_str()) != Some("dart") {
                continue;
            }
            match std::fs::read_to_string(path) {
                Ok(text) if text.len() as u64 <= MAX_FILE_BYTES => {
                    self.files
                        .insert(path.clone(), index_file(path, &text, root, &self.packages));
                    self.sources.insert(path.clone(), text);
                }
                _ => {
                    self.files.remove(path);
                    self.sources.remove(path);
                }
            }
        }
        true
    }

    pub fn definition(&self, path: &Path, text: &str, line: u32, column: u32) -> Vec<Target> {
        if let Some(target) = self.import_target(path, text, line, column) {
            return vec![target];
        }
        if let Some(route) = flutter::route_target(path, text, line, column) {
            return vec![target(route)];
        }
        let Some((name, _, _)) = identifier_range(text, line, column) else {
            return Vec::new();
        };
        self.resolve(path, text, line, column, &name)
            .into_iter()
            .map(target)
            .collect()
    }

    pub fn complete(&self, path: &Path, text: &str, line: u32, column: u32) -> Vec<Completion> {
        let prefix = word_before(text, line, column);
        let qualified = qualifier_before(text, line, column);
        let file = self.file_for(path, text);
        // After `p.` for an import prefix only that import's names apply.
        let import_prefix = qualified
            .as_deref()
            .filter(|name| self.is_import_prefix(&file, name));
        let visible = match import_prefix {
            Some(prefix) => self.prefixed_exports(&file, prefix),
            None => self.visible(path, text, None),
        };
        let mut candidates: Vec<(Declaration, Vec<Insertion>)> = visible
            .iter()
            .cloned()
            .map(|item| (item, Vec::new()))
            .collect();
        // Cross-file auto-imports can cover the whole workspace. Wait until
        // the user supplied enough prefix to avoid ranking a huge list after
        // every initial keystroke.
        if qualified.is_none() && prefix.chars().count() >= 2 {
            for (candidate_path, candidate_file) in &self.files {
                // Already imported (possibly with a prefix): never offer a
                // second, unprefixed import of the same file.
                if candidate_path == path
                    || self.library_files(path, &file).contains(candidate_path)
                    || file
                        .imports
                        .iter()
                        .any(|import| &import.path == candidate_path)
                {
                    continue;
                }
                let extra = if let Some(part_of) = candidate_file.part_of.as_deref() {
                    part_of_matches(candidate_path, part_of, path)
                        .then(|| part_insertion(text, candidate_path, path))
                } else {
                    Some(Insertion {
                        line: 0,
                        column: 0,
                        text: format!("import '{}';\n", relative_uri(path, candidate_path)),
                    })
                };
                let Some(extra) = extra else { continue };
                for item in &candidate_file.declarations {
                    if !item.name.starts_with('_')
                        && !visible
                            .iter()
                            .any(|shown| shown.name == item.name && shown.owner == item.owner)
                    {
                        candidates.push((item.clone(), vec![extra.clone()]));
                    }
                }
            }
        }
        if let Some(prefix_name) = qualified.as_ref().filter(|_| import_prefix.is_none()) {
            candidates.retain(|(item, _)| {
                item.owner.as_deref() == Some(prefix_name.as_str())
                    || (item.kind == ItemKind::Class && &item.name == prefix_name)
            });
        }
        candidates.retain(|(item, _)| item.name.starts_with(&prefix));
        candidates
            .sort_by(|(a, _), (b, _)| a.name.cmp(&b.name).then(a.signature.cmp(&b.signature)));
        candidates.dedup_by(|(a, _), (b, _)| a.name == b.name && a.owner == b.owner);
        candidates
            .into_iter()
            .take(MAX_ITEMS)
            .map(|(item, extra)| Completion {
                label: item.name.clone(),
                detail: Some(item.signature.clone()),
                insert_text: item.name.clone(),
                is_snippet: false,
                kind: item.kind,
                sort: item.name.clone(),
                extra,
                replace_from: None,
            })
            .collect()
    }

    pub fn hover(&self, path: &Path, text: &str, line: u32, column: u32) -> Option<HoverResult> {
        if let Some(target) = self.import_target(path, text, line, column) {
            let source = text.lines().nth(line as usize)?;
            let (_, start, end) = uri_at(source, column)?;
            return Some(HoverResult {
                markdown: format!("**import**\n\n`{}`", target.path.display()),
                line,
                start_column: start,
                end_column: end,
            });
        }
        let (name, start, end) = identifier_range(text, line, column)?;
        let item = self
            .resolve(path, text, line, column, &name)
            .into_iter()
            .next()?;
        Some(HoverResult {
            markdown: format!("```dart\n{}\n```", item.signature),
            line,
            start_column: start,
            end_column: end,
        })
    }

    pub fn outline(&self, path: &Path, text: &str) -> Vec<OutlineSymbol> {
        let file = index_file(
            path,
            text,
            &project_root(path),
            &package_roots(
                &project_root(path),
                package_name(&project_root(path)).as_deref(),
            ),
        );
        let mut top = Vec::new();
        for item in &file.declarations {
            if item.owner.is_none() {
                let children = file
                    .declarations
                    .iter()
                    .filter(|child| child.owner.as_deref() == Some(&item.name))
                    .map(outline_symbol)
                    .collect();
                top.push(OutlineSymbol {
                    name: item.name.clone(),
                    kind: kind_name(item.kind),
                    line: item.line,
                    column: item.column,
                    children,
                });
            }
        }
        top
    }

    pub fn references(&self, path: &Path, text: &str, line: u32, column: u32) -> Vec<Occurrence> {
        let Some((name, _, _)) = identifier_range(text, line, column) else {
            return Vec::new();
        };
        let resolved = self.resolve(path, text, line, column, &name);
        if resolved.len() != 1 {
            return Vec::new();
        }
        let target = &resolved[0];
        let mut found = Vec::new();
        for (candidate_path, source) in self.all_sources(path, text) {
            for (at_line, start, end) in occurrences(&source, &name) {
                if self
                    .resolve(&candidate_path, &source, at_line, start, &name)
                    .as_slice()
                    == [target.clone()]
                {
                    found.push(Occurrence {
                        path: candidate_path.clone(),
                        line: at_line,
                        start_column: start,
                        end_column: end,
                        dollar: false,
                    });
                }
            }
        }
        found.sort();
        found
    }

    pub fn rename(
        &self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
        new_name: &str,
    ) -> Result<Vec<Occurrence>, String> {
        if !valid_identifier(new_name) {
            return Err("invalid Dart identifier".to_owned());
        }
        let occurrences = self.references(path, text, line, column);
        if occurrences.is_empty() {
            return Err("symbol is ambiguous or unknown".to_owned());
        }
        Ok(occurrences)
    }

    pub fn diagnostics(&self, path: &Path, text: &str) -> Vec<Problem> {
        let root = project_root(path);
        let file = index_file(path, text, &root, &self.packages);
        let mut problems = Vec::new();
        for import in &file.imports {
            if !import.path.is_file() {
                let line = text
                    .lines()
                    .position(|line| {
                        line.contains("import ")
                            && line.contains(
                                import
                                    .path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or(""),
                            )
                    })
                    .unwrap_or(0) as u32;
                problems.push(problem(line, 0, "import target is not in this package"));
            }
        }
        if let Some(tree) = parse_tree(text) {
            collect_syntax_errors(tree.root_node(), text, &mut problems);
        }
        problems.sort_by_key(|item| (item.start_line, item.start_column));
        problems
    }

    fn resolve(
        &self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
        name: &str,
    ) -> Vec<Declaration> {
        if let Some(local) = flutter::semantic_target(path, text, line, name) {
            return vec![local];
        }
        let file = self.file_for(path, text);
        let qualifier = qualifier_at(text, line, column);
        let owner = qualifier
            .as_deref()
            .filter(|q| !self.is_import_prefix(&file, q));
        let mut matches: Vec<_> = self
            .visible(path, text, qualifier.as_deref())
            .into_iter()
            .filter(|item| item.name == name)
            .collect();
        if let Some(owner) = owner {
            matches.retain(|item| item.owner.as_deref() == Some(owner));
        }
        if let Some(prefix) = qualifier.filter(|q| self.is_import_prefix(&file, q)) {
            matches.retain(|item| self.import_allows(&file, &prefix, item));
        }
        matches.sort_by(|a, b| (&a.path, a.line, a.column).cmp(&(&b.path, b.line, b.column)));
        matches.dedup();
        if matches.len() == 1 {
            matches
        } else {
            Vec::new()
        }
    }

    /// Declarations reachable from `path`. Names of a prefixed import
    /// (`import 'x.dart' as p;`) are only reachable through `p.`, as in
    /// Dart, so they are included only when `prefix` names that import.
    fn visible(&self, path: &Path, text: &str, prefix: Option<&str>) -> Vec<Declaration> {
        let file = self.file_for(path, text);
        let mut out = file.declarations.clone();
        for sibling in self.library_files(path, &file) {
            if let Some(other) = self.files.get(&sibling) {
                out.extend(other.declarations.clone());
            }
        }
        for import in &file.imports {
            if import.prefix.is_none() || import.prefix.as_deref() == prefix {
                out.extend(self.exported(&import.path, import));
            }
        }
        out
    }

    /// Top-level names a prefixed import exposes after `prefix.`.
    fn prefixed_exports(&self, file: &FileIndex, prefix: &str) -> Vec<Declaration> {
        file.imports
            .iter()
            .filter(|import| import.prefix.as_deref() == Some(prefix))
            .flat_map(|import| self.exported(&import.path, import))
            .filter(|item| item.owner.is_none())
            .collect()
    }

    fn exported(&self, path: &Path, import: &Import) -> Vec<Declaration> {
        self.files
            .get(path)
            .into_iter()
            .flat_map(|file| file.declarations.iter())
            .filter(|item| {
                !item.name.starts_with('_')
                    && import
                        .show
                        .as_ref()
                        .is_none_or(|show| show.contains(&item.name))
                    && !import.hide.contains(&item.name)
            })
            .cloned()
            .collect()
    }

    fn file_for(&self, path: &Path, text: &str) -> FileIndex {
        self.files
            .get(path)
            .filter(|_| self.sources.get(path).is_some_and(|source| source == text))
            .cloned()
            .unwrap_or_else(|| {
                let root = project_root(path);
                index_file(path, text, &root, &self.packages)
            })
    }

    fn library_files(&self, path: &Path, file: &FileIndex) -> Vec<PathBuf> {
        let mut out = file.parts.clone();
        for (candidate, other) in &self.files {
            if other.parts.iter().any(|part| part == path) {
                out.push(candidate.clone());
            }
        }
        out
    }

    fn is_import_prefix(&self, file: &FileIndex, name: &str) -> bool {
        file.imports
            .iter()
            .any(|item| item.prefix.as_deref() == Some(name))
    }
    fn import_allows(&self, file: &FileIndex, prefix: &str, item: &Declaration) -> bool {
        file.imports.iter().any(|import| {
            import.prefix.as_deref() == Some(prefix)
                && import.path == item.path
                && import
                    .show
                    .as_ref()
                    .is_none_or(|show| show.contains(&item.name))
                && !import.hide.contains(&item.name)
        })
    }
    fn all_sources(&self, path: &Path, text: &str) -> Vec<(PathBuf, String)> {
        let mut out: Vec<_> = self
            .sources
            .iter()
            .map(|(path, source)| (path.clone(), source.clone()))
            .collect();
        if let Some(item) = out.iter_mut().find(|(candidate, _)| candidate == path) {
            item.1 = text.to_owned();
        } else {
            out.push((path.to_path_buf(), text.to_owned()));
        }
        out
    }

    fn import_target(&self, path: &Path, text: &str, line: u32, column: u32) -> Option<Target> {
        let source = text.lines().nth(line as usize)?;
        let (uri, _, _) = uri_at(source, column)?;
        let directive = source.trim_start();
        if !(directive.starts_with("import ")
            || directive.starts_with("export ")
            || directive.starts_with("part "))
        {
            return None;
        }
        let target = resolve_uri(path, &uri, &project_root(path), &self.packages)?;
        target.is_file().then_some(Target {
            path: target,
            line: 0,
            column: 0,
        })
    }
}

fn relative_uri(from: &Path, target: &Path) -> String {
    let base = from.parent().unwrap_or(from);
    let base_parts: Vec<_> = base.components().collect();
    let target_parts: Vec<_> = target.components().collect();
    let shared = base_parts
        .iter()
        .zip(&target_parts)
        .take_while(|(a, b)| a == b)
        .count();
    let mut parts = vec!["..".to_owned(); base_parts.len().saturating_sub(shared)];
    parts.extend(
        target_parts[shared..]
            .iter()
            .map(|part| part.as_os_str().to_string_lossy().into_owned()),
    );
    if parts.is_empty() {
        target
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    } else {
        parts.join("/")
    }
}

fn part_of_matches(candidate: &Path, part_of: &str, library: &Path) -> bool {
    let trimmed = part_of.trim_matches(['\'', '"']);
    candidate
        .parent()
        .is_some_and(|parent| parent.join(trimmed) == library)
}

fn part_insertion(text: &str, candidate: &Path, library: &Path) -> Insertion {
    let last_part = text
        .lines()
        .enumerate()
        .filter_map(|(line, source)| {
            strip_comment(source)
                .trim_start()
                .starts_with("part ")
                .then_some(line as u32)
        })
        .last();
    let uri = relative_uri(library, candidate);
    match last_part {
        Some(line) => Insertion {
            line: line + 1,
            column: 0,
            text: format!("part '{uri}';\n"),
        },
        None => part_after_imports(text, uri),
    }
}

fn part_after_imports(text: &str, uri: String) -> Insertion {
    let last_import = text
        .lines()
        .enumerate()
        .filter_map(|(line, source)| {
            strip_comment(source)
                .trim_start()
                .starts_with("import ")
                .then_some(line as u32)
        })
        .last();
    match last_import {
        Some(line) => Insertion {
            line: line + 1,
            column: 0,
            text: format!("\npart '{uri}';\n"),
        },
        None => Insertion {
            line: 0,
            column: 0,
            text: format!("part '{uri}';\n"),
        },
    }
}

fn parse_tree(text: &str) -> Option<tree_sitter::Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_dart::LANGUAGE.into())
        .ok()?;
    parser.parse(text, None)
}

fn collect_syntax_errors(node: Node<'_>, text: &str, out: &mut Vec<Problem>) {
    if !node.has_error() {
        return;
    }
    if node.is_error() || node.is_missing() {
        let point = node.start_position();
        let start = text[..node.start_byte()].rfind('\n').map_or(0, |at| at + 1);
        out.push(problem(
            point.row as u32,
            utf16(&text[start..node.start_byte()]),
            "syntax error",
        ));
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_syntax_errors(child, text, out);
    }
}

pub fn project_root(file: &Path) -> PathBuf {
    let start = file.parent().unwrap_or(file);
    start
        .ancestors()
        .find(|dir| dir.join("pubspec.yaml").is_file())
        .or_else(|| start.ancestors().find(|dir| dir.join(".git").exists()))
        .unwrap_or(start)
        .to_path_buf()
}

fn index_file(
    path: &Path,
    text: &str,
    root: &Path,
    packages: &HashMap<String, PathBuf>,
) -> FileIndex {
    let mut out = FileIndex::default();
    let mut owners: Vec<(String, usize)> = Vec::new();
    let mut depth = 0usize;
    for (line, source) in text.lines().enumerate() {
        let code = strip_comment(source).trim_start();
        if let Some(import) = parse_import(path, code, root, packages) {
            out.imports.push(import);
        }
        if let Some(part) = parse_uri(code, "part ") {
            out.parts.push(
                resolve_uri(path, &part, root, packages)
                    .unwrap_or_else(|| path.parent().unwrap_or(root).join(part)),
            );
        }
        if let Some(value) = parse_uri(code, "part of ") {
            out.part_of = Some(value);
        }
        while owners.last().is_some_and(|(_, level)| *level > depth) {
            owners.pop();
        }
        let owner = owners.last().map(|(name, _)| name.clone());
        if let Some((name, kind, signature, opens_owner)) = declaration(code, owner.as_deref()) {
            let column = utf16(&source[..source.find(&name).unwrap_or(0)]);
            out.declarations.push(Declaration {
                name: name.clone(),
                path: path.to_path_buf(),
                line: line as u32,
                column,
                kind,
                signature,
                owner: owner.clone(),
            });
            if opens_owner {
                owners.push((name, depth + code.chars().filter(|c| *c == '{').count()));
            }
        }
        depth = depth
            .saturating_add(code.chars().filter(|c| *c == '{').count())
            .saturating_sub(code.chars().filter(|c| *c == '}').count());
        while owners.last().is_some_and(|(_, level)| *level > depth) {
            owners.pop();
        }
    }
    out
}

fn declaration(code: &str, owner: Option<&str>) -> Option<(String, ItemKind, String, bool)> {
    let code = code.trim_start_matches(|c: char| c == '@' || c.is_whitespace());
    for (prefix, kind) in [
        ("abstract class ", ItemKind::Class),
        ("class ", ItemKind::Class),
        ("mixin ", ItemKind::Trait),
        ("enum ", ItemKind::Enum),
        ("extension ", ItemKind::Trait),
        ("typedef ", ItemKind::Function),
    ] {
        if let Some(rest) = code.strip_prefix(prefix) {
            let name = first_ident(rest)?;
            return Some((
                name,
                kind,
                code.trim_end_matches('{').trim().to_owned(),
                true,
            ));
        }
    }
    let owner = owner?;
    if let Some(rest) = code
        .strip_prefix(owner)
        .and_then(|rest| rest.strip_prefix('.'))
    {
        let name = first_ident(rest)?;
        return Some((
            name.clone(),
            ItemKind::Method,
            format!("{owner}.{name}{}", rest[name.len()..].trim()),
            false,
        ));
    }
    if code.contains('(') && (code.contains(')') || code.contains("=>")) {
        let before = code.split('(').next()?.trim();
        let name = before.rsplit(|c: char| !is_ident(c)).next()?;
        // The unnamed constructor (`const TransactionEvent()`) belongs to
        // its class; indexing it separately makes type navigation ambiguous.
        if name == owner {
            return None;
        }
        if !name.is_empty() && !matches!(name, "if" | "for" | "while" | "switch") {
            return Some((
                name.to_owned(),
                ItemKind::Method,
                code.trim_end_matches('{').trim().to_owned(),
                false,
            ));
        }
    }
    if code.ends_with(';') && !code.contains('(') && !code.starts_with("return ") {
        let before = code.trim_end_matches(';').split('=').next()?.trim();
        let name = before.rsplit(|c: char| !is_ident(c)).next()?;
        if !name.is_empty() {
            return Some((name.to_owned(), ItemKind::Property, code.to_owned(), false));
        }
    }
    None
}

fn parse_import(
    path: &Path,
    code: &str,
    root: &Path,
    packages: &HashMap<String, PathBuf>,
) -> Option<Import> {
    let uri = parse_uri(code, "import ")?;
    let path = resolve_uri(path, &uri, root, packages)?;
    let prefix = code
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "as")
        .map(|w| w[1].trim_end_matches(';').to_owned());
    let names = |word: &str| {
        code.split_once(word).map(|(_, rest)| {
            rest.trim()
                .trim_start_matches(['{', ' '])
                .split(|c: char| !is_ident(c))
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
    };
    Some(Import {
        path,
        prefix,
        show: names(" show "),
        hide: names(" hide ").unwrap_or_default(),
    })
}

fn parse_uri(code: &str, prefix: &str) -> Option<String> {
    let rest = code.strip_prefix(prefix)?.trim_start();
    let quote = rest.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    rest[1..].find(quote).map(|end| rest[1..end + 1].to_owned())
}

fn uri_at(source: &str, column: u32) -> Option<(String, u32, u32)> {
    let byte = utf8_at(source, column);
    let before = &source[..byte];
    let start = before
        .char_indices()
        .rev()
        .find_map(|(at, c)| matches!(c, '\'' | '"').then_some(at))?;
    let quote = source[start..].chars().next()?;
    let end = source[byte..].find(quote)? + byte;
    (start < byte && byte <= end).then(|| {
        (
            source[start + quote.len_utf8()..end].to_owned(),
            utf16(&source[..start + quote.len_utf8()]),
            utf16(&source[..end]),
        )
    })
}

fn resolve_uri(
    path: &Path,
    uri: &str,
    root: &Path,
    packages: &HashMap<String, PathBuf>,
) -> Option<PathBuf> {
    if let Some(rest) = uri.strip_prefix("package:") {
        let (name, file) = rest.split_once('/')?;
        packages.get(name).map(|root| root.join(file))
    } else {
        (!uri.contains(':')).then(|| path.parent().unwrap_or(root).join(uri))
    }
}

fn package_roots(root: &Path, package: Option<&str>) -> HashMap<String, PathBuf> {
    let mut packages = HashMap::new();
    if let Some(name) = package {
        packages.insert(name.to_owned(), root.join("lib"));
    }
    if let Ok(config) = std::fs::read_to_string(root.join(".dart_tool/package_config.json"))
        && let Ok(config) = serde_json::from_str::<serde_json::Value>(&config)
        && let Some(entries) = config.get("packages").and_then(serde_json::Value::as_array)
    {
        for entry in entries {
            let (Some(name), Some(uri)) = (
                entry.get("name").and_then(serde_json::Value::as_str),
                entry.get("rootUri").and_then(serde_json::Value::as_str),
            ) else {
                continue;
            };
            let base = uri
                .strip_prefix("file://")
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join(".dart_tool").join(uri));
            let package_root = if base.is_absolute() {
                base
            } else {
                root.join(base)
            };
            let package_uri = entry
                .get("packageUri")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("lib/");
            packages.insert(name.to_owned(), package_root.join(package_uri));
        }
    }
    if let Some(flutter_root) = std::env::var_os("FLUTTER_ROOT") {
        packages
            .entry("flutter".to_owned())
            .or_insert_with(|| PathBuf::from(flutter_root).join("packages/flutter/lib"));
    }
    packages
}

fn package_name(root: &Path) -> Option<String> {
    std::fs::read_to_string(root.join("pubspec.yaml"))
        .ok()?
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("name:")
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
        })
}
fn first_ident(text: &str) -> Option<String> {
    let name: String = text.chars().take_while(|c| is_ident(*c)).collect();
    (!name.is_empty()).then_some(name)
}
fn strip_comment(text: &str) -> &str {
    text.split_once("//").map_or(text, |(code, _)| code)
}
fn is_ident(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}
fn valid_identifier(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .next()
            .is_some_and(|c| c == '_' || c.is_alphabetic())
        && text.chars().all(is_ident)
}
fn utf16(text: &str) -> u32 {
    text.encode_utf16().count() as u32
}
fn identifier_range(text: &str, line: u32, column: u32) -> Option<(String, u32, u32)> {
    let source = text.lines().nth(line as usize)?;
    let mut byte = source.len();
    let mut units = 0;
    for (index, c) in source.char_indices() {
        if units >= column {
            byte = index;
            break;
        }
        units += c.len_utf16() as u32;
    }
    let start = source[..byte]
        .rfind(|c: char| !is_ident(c))
        .map_or(0, |n| n + 1);
    let end = source[byte..]
        .find(|c: char| !is_ident(c))
        .map_or(source.len(), |n| byte + n);
    (start < end).then(|| {
        (
            source[start..end].to_owned(),
            utf16(&source[..start]),
            utf16(&source[..end]),
        )
    })
}
fn word_before(text: &str, line: u32, column: u32) -> String {
    identifier_range(text, line, column)
        .map(|(name, _, _)| name)
        .unwrap_or_default()
}
fn qualifier_at(text: &str, line: u32, column: u32) -> Option<String> {
    let source = text.lines().nth(line as usize)?;
    let (_, start, _) = identifier_range(text, line, column)?;
    let left = source[..utf8_at(source, start)].trim_end();
    left.strip_suffix('.')
        .and_then(|s| s.rsplit(|c: char| !is_ident(c)).next())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}
fn qualifier_before(text: &str, line: u32, column: u32) -> Option<String> {
    let source = text.lines().nth(line as usize)?;
    let prefix = &source[..utf8_at(source, column)];
    prefix
        .trim_end()
        .strip_suffix('.')
        .and_then(|s| s.rsplit(|c: char| !is_ident(c)).next())
        .map(str::to_owned)
}
fn utf8_at(source: &str, column: u32) -> usize {
    let mut units = 0;
    for (index, c) in source.char_indices() {
        if units >= column {
            return index;
        }
        units += c.len_utf16() as u32;
    }
    source.len()
}
fn occurrences(text: &str, name: &str) -> Vec<(u32, u32, u32)> {
    text.lines()
        .enumerate()
        .flat_map(|(line, source)| {
            let mut out = Vec::new();
            let mut offset = 0;
            while let Some(found) = source[offset..].find(name) {
                let start = offset + found;
                let end = start + name.len();
                if !source[..start].chars().last().is_some_and(is_ident)
                    && !source[end..].chars().next().is_some_and(is_ident)
                {
                    out.push((line as u32, utf16(&source[..start]), utf16(&source[..end])));
                }
                offset = end;
            }
            out
        })
        .collect()
}
fn target(item: Declaration) -> Target {
    Target {
        path: item.path,
        line: item.line,
        column: item.column,
    }
}
fn outline_symbol(item: &Declaration) -> OutlineSymbol {
    OutlineSymbol {
        name: item.name.clone(),
        kind: kind_name(item.kind),
        line: item.line,
        column: item.column,
        children: Vec::new(),
    }
}
fn kind_name(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Class => "class",
        ItemKind::Trait => "mixin",
        ItemKind::Enum => "enum",
        ItemKind::Method => "method",
        ItemKind::Property => "field",
        ItemKind::Function => "function",
        _ => "symbol",
    }
}
fn problem(line: u32, column: u32, message: &str) -> Problem {
    Problem {
        start_line: line,
        start_column: column,
        end_line: line,
        end_column: column + 1,
        severity: Severity::Error,
        message: message.to_owned(),
        unnecessary: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn engine(files: &[(&str, &str)]) -> (tempfile::TempDir, DartEngine) {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("lib")).unwrap();
        std::fs::write(root.path().join("pubspec.yaml"), "name: coffee_app\n").unwrap();
        for (name, text) in files {
            let path = root.path().join("lib").join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let mut engine = DartEngine::pending(root.path());
        engine.install(DartEngine::build(root.path()));
        (root, engine)
    }

    #[test]
    fn disk_delta_replaces_and_removes_a_file_index() {
        let (root, mut engine) = engine(&[("state.dart", "class OldState {}\n")]);
        let path = root.path().join("lib/state.dart");
        std::fs::write(&path, "class NewState {}\n").unwrap();
        assert!(engine.apply_disk_changes(root.path(), std::slice::from_ref(&path)));
        assert!(
            engine.files[&path]
                .declarations
                .iter()
                .any(|item| item.name == "NewState")
        );
        assert!(
            !engine.files[&path]
                .declarations
                .iter()
                .any(|item| item.name == "OldState")
        );
        std::fs::remove_file(&path).unwrap();
        assert!(engine.apply_disk_changes(root.path(), std::slice::from_ref(&path)));
        assert!(!engine.files.contains_key(&path));
    }
    #[test]
    fn imports_aliases_and_show_hide_resolve() {
        let (root, engine) = engine(&[
            ("card.dart", "class Card {} class Hidden {}\n"),
            (
                "main.dart",
                "import 'card.dart' as ui show Card;\nvoid f() { ui.Card(); }\n",
            ),
        ]);
        let source = std::fs::read_to_string(root.path().join("lib/main.dart")).unwrap();
        let found = engine.definition(&root.path().join("lib/main.dart"), &source, 1, 16);
        assert_eq!(found[0].path, root.path().join("lib/card.dart"));
        assert_eq!(
            engine
                .complete(&root.path().join("lib/main.dart"), &source, 1, 16)
                .iter()
                .filter(|item| item.label == "Hidden")
                .count(),
            0
        );
    }

    #[test]
    fn prefixed_import_names_need_their_prefix() {
        let (root, engine) = engine(&[
            ("card.dart", "class Card {}\n"),
            (
                "main.dart",
                "import 'card.dart' as ui;\nvoid f() { Card(); ui.Card(); Ca }\n",
            ),
        ]);
        let path = root.path().join("lib/main.dart");
        let source = std::fs::read_to_string(&path).unwrap();
        let line = source.lines().nth(1).unwrap();
        let bare = line.find("Card()").unwrap() as u32 + 1;
        let qualified = line.find("ui.Card").unwrap() as u32 + 4;
        let typed = line.find("Ca }").unwrap() as u32 + 2;

        assert!(engine.definition(&path, &source, 1, bare).is_empty());
        assert_eq!(
            engine.definition(&path, &source, 1, qualified)[0].path,
            root.path().join("lib/card.dart")
        );
        // Unqualified completion neither offers `Card` as reachable nor
        // proposes importing `card.dart` a second time.
        assert!(
            engine
                .complete(&path, &source, 1, typed)
                .iter()
                .all(|item| item.label != "Card")
        );
        let after_prefix = line.find("ui.Card").unwrap() as u32 + 3;
        assert!(
            engine
                .complete(&path, &source, 1, after_prefix)
                .iter()
                .any(|item| item.label == "Card" && item.extra.is_empty())
        );
    }

    #[test]
    fn completion_offers_unimported_symbols_with_import_or_part_edit() {
        let (root, engine) = engine(&[
            ("login_state.dart", "class LoginState {}\n"),
            (
                "library.dart",
                "import '../helpers/helpers_wrapper.dart';\n\npart 'existing.dart';\nclass Library { LibrarySt value; }\n",
            ),
            (
                "library_state.dart",
                "part of 'library.dart';\nclass LibraryState {}\n",
            ),
            (
                "login_bloc.dart",
                "class LoginBloc extends Bloc<LoginEvent, LoginSt> {}\n",
            ),
        ]);
        let path = root.path().join("lib/login_bloc.dart");
        let source = std::fs::read_to_string(&path).unwrap();
        let items = engine.complete(
            &path,
            &source,
            0,
            source.find("LoginSt").unwrap() as u32 + 2,
        );
        let login = items
            .iter()
            .find(|item| item.label == "LoginState")
            .unwrap();
        assert_eq!(login.extra[0].text, "import 'login_state.dart';\n");
        let path = root.path().join("lib/library.dart");
        let source = std::fs::read_to_string(&path).unwrap();
        let items = engine.complete(
            &path,
            &source,
            3,
            source.lines().nth(3).unwrap().find("LibrarySt").unwrap() as u32 + 2,
        );
        let part = items
            .iter()
            .find(|item| item.label == "LibraryState")
            .unwrap();
        assert_eq!(part.extra[0].line, 3);
        assert_eq!(part.extra[0].text, "part 'library_state.dart';\n");
    }

    #[test]
    fn import_uri_and_own_class_have_navigation_and_hover_before_indexing() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("lib")).unwrap();
        std::fs::write(root.path().join("pubspec.yaml"), "name: coffee_app\n").unwrap();
        let card = root.path().join("lib/card.dart");
        std::fs::write(&card, "class Card {}\n").unwrap();
        let main = root.path().join("lib/main.dart");
        let source = "import 'card.dart';\nclass Main {}\n";
        std::fs::write(&main, source).unwrap();
        let engine = DartEngine::pending(root.path());
        let import = engine.definition(&main, source, 0, 10);
        assert_eq!(import[0].path, card);
        assert!(engine.hover(&main, source, 0, 10).is_some());
        assert_eq!(engine.definition(&main, source, 1, 7)[0].line, 1);
    }
    #[test]
    fn parts_named_constructors_and_members_are_indexed() {
        let (root, engine) = engine(&[
            (
                "model.dart",
                "part 'model_part.dart';\nclass Coffee {\n  Coffee.named();\n  String title;\n  void brew() {}\n}\n",
            ),
            (
                "model_part.dart",
                "part of 'model.dart';\nextension Taste on Coffee {\n  String describe() => 'x';\n}\n",
            ),
            (
                "main.dart",
                "import 'model.dart';\nvoid f() { Coffee.named(); Coffee().brew(); }\n",
            ),
        ]);
        let main = root.path().join("lib/main.dart");
        let source = std::fs::read_to_string(&main).unwrap();
        assert_eq!(engine.definition(&main, &source, 1, 20)[0].line, 2);
        assert!(
            engine.outline(
                &root.path().join("lib/model.dart"),
                &std::fs::read_to_string(root.path().join("lib/model.dart")).unwrap()
            )[0]
            .children
            .iter()
            .any(|item| item.name == "brew")
        );
    }

    #[test]
    fn abstract_class_in_part_resolves_from_its_library() {
        let (root, engine) = engine(&[
            (
                "blocs/transaction/transaction_bloc.dart",
                "part 'transaction_event.dart';\npart 'transaction_state.dart';\n\nclass TransactionBloc extends Bloc<TransactionEvent, TransactionState> {}\n",
            ),
            (
                "blocs/transaction/transaction_event.dart",
                "part of 'transaction_bloc.dart';\n\nabstract class TransactionEvent extends Equatable {\n  const TransactionEvent();\n}\n",
            ),
            (
                "blocs/transaction/transaction_state.dart",
                "part of 'transaction_bloc.dart';\n\nclass TransactionState {}\n",
            ),
        ]);
        let path = root
            .path()
            .join("lib/blocs/transaction/transaction_bloc.dart");
        let source = std::fs::read_to_string(&path).unwrap();
        let line = source.lines().nth(3).unwrap();
        let column = line.find("TransactionEvent").unwrap() as u32 + 1;
        let found = engine.definition(&path, &source, 3, column);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].path,
            root.path()
                .join("lib/blocs/transaction/transaction_event.dart")
        );
        assert_eq!(found[0].line, 2);
    }
    #[test]
    fn references_rename_hover_and_diagnostics_are_conservative() {
        let (root, engine) = engine(&[
            ("a.dart", "class Bean {}\n"),
            (
                "main.dart",
                "import 'a.dart';\nvoid f() { Bean(); Bean(); }\n",
            ),
        ]);
        let main = root.path().join("lib/main.dart");
        let source = std::fs::read_to_string(&main).unwrap();
        assert_eq!(engine.references(&main, &source, 1, 11).len(), 3);
        assert!(engine.rename(&main, &source, 1, 11, "Roast").is_ok());
        assert!(
            engine
                .hover(&main, &source, 1, 11)
                .unwrap()
                .markdown
                .contains("class Bean")
        );
        assert!(!engine.diagnostics(&main, "void f( {").is_empty());
    }

    #[test]
    fn package_config_dependency_resolves_to_installed_package_source() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("lib")).unwrap();
        std::fs::create_dir_all(root.path().join(".dart_tool")).unwrap();
        let dependency = root.path().join("packages/theme/lib");
        std::fs::create_dir_all(&dependency).unwrap();
        std::fs::write(root.path().join("pubspec.yaml"), "name: coffee_app\n").unwrap();
        std::fs::write(dependency.join("theme.dart"), "class CoffeeTheme {}\n").unwrap();
        std::fs::write(root.path().join(".dart_tool/package_config.json"), format!("{{\"packages\":[{{\"name\":\"theme\",\"rootUri\":\"{}\",\"packageUri\":\"lib/\"}}]}}", root.path().join("packages/theme").display())).unwrap();
        let main = root.path().join("lib/main.dart");
        let source = "import 'package:theme/theme.dart';\nvoid f() { CoffeeTheme(); }\n";
        std::fs::write(&main, source).unwrap();
        let mut engine = DartEngine::pending(root.path());
        engine.install(DartEngine::build(root.path()));
        let found = engine.definition(&main, source, 1, 12);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, dependency.join("theme.dart"));
    }

    #[test]
    fn flutter_context_widget_and_named_route_resolve_locally() {
        let (root, engine) = engine(&[(
            "main.dart",
            "class DetailsPage {}\nclass HomePage {\n  void open(BuildContext context) {\n    Navigator.pushNamed(context, '/details');\n  }\n}\nclass _HomeState extends State<HomePage> {\n  void build() { widget; }\n}\nfinal app = MaterialApp(routes: {'/details': (_) => DetailsPage()});\n",
        )]);
        let main = root.path().join("lib/main.dart");
        let source = std::fs::read_to_string(&main).unwrap();
        let context = source.lines().nth(3).unwrap().find("context").unwrap() as u32 + 1;
        assert_eq!(engine.definition(&main, &source, 3, context)[0].line, 2);
        let widget = source.lines().nth(7).unwrap().find("widget").unwrap() as u32 + 1;
        assert_eq!(engine.definition(&main, &source, 7, widget)[0].line, 6);
        let route = source.lines().nth(3).unwrap().find("details").unwrap() as u32 + 2;
        assert_eq!(engine.definition(&main, &source, 3, route)[0].line, 0);
    }

    #[test]
    fn provider_and_bloc_reads_resolve_to_the_static_provider_type() {
        let (root, engine) = engine(&[(
            "main.dart",
            "class AuthBloc {}\nclass App {\n  Widget build() => BlocProvider<AuthBloc>(create: (_) => AuthBloc(), child: Child());\n}\nclass Child {\n  void load(BuildContext context) { context.read<AuthBloc>(); }\n}\n",
        )]);
        let main = root.path().join("lib/main.dart");
        let source = std::fs::read_to_string(&main).unwrap();
        let column = source.lines().nth(5).unwrap().rfind("AuthBloc").unwrap() as u32 + 1;
        let found = engine.definition(&main, &source, 5, column);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 0);
    }

    #[test]
    fn hints_name_arguments_and_type_variables() {
        let main = "import 'brew.dart';\nvoid main() {\n  var a = 89;\n  final b = 'x';\n  const c = 1.5;\n  var d = true;\n  int e = 2;\n  var f = Brew.make();\n  var g = Brew.generic(1);\n  Brew.greet(a, 'x', loud: true);\n  var h = -3;\n}\n";
        let brew = "class Brew {\n  static String make() => 'x';\n  static T generic<T>(T item) => item;\n  static void greet(String name, int times, {bool loud = false}) {}\n}\n";
        let (root, engine) = engine(&[("main.dart", main), ("brew.dart", brew)]);
        let path = root.path().join("lib/main.dart");
        let found = engine.parameter_hints(&path, main, 0, 20);
        let seen: Vec<(u32, u32, &str)> = found
            .iter()
            .map(|hint| (hint.line, hint.column, hint.label.as_str()))
            .collect();
        assert_eq!(
            seen,
            vec![
                (2, 7, ": int"),
                (3, 9, ": String"),
                (4, 9, ": double"),
                (5, 7, ": bool"),
                (7, 7, ": String"),
                (9, 13, "name:"),
                (9, 16, "times:"),
                (10, 7, ": int"),
            ],
            "{seen:?}"
        );
    }
}
