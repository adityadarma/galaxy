//! The calls the editor makes on a Go project, in the shape every built-in
//! language engine shares (see [`crate::lang`]). Lines are 0-based and
//! columns UTF-16, as in the rest of the Go engine.

use std::path::{Path, PathBuf};

use super::engine::GoEngine;
use super::modules::GoProject;
use super::parse::parse;
use super::types::{Symbol, callee_at, resolve_at};
use super::{actions, complete, diag, refs};
use crate::lang::{Action, Completion, HoverResult, Occurrence, Problem, Signature, Target};
use crate::parameter_hints::ParameterHint;

/// The directory a Go file belongs to as a project. A `go.work` root is used
/// only when it declares the nearest module; this keeps unrelated monorepo
/// services from sharing an engine or module-download target.
pub fn project_root(file: &Path) -> PathBuf {
    let start = file.parent().unwrap_or(file);
    if let Some(project) = GoProject::detect(file) {
        if let Some(workspace) = project.root().ancestors().skip(1).find(|dir| {
            dir.join("go.work").is_file()
                && GoProject::detect(dir)
                    .is_some_and(|candidate| candidate.module_for_file(file).is_some())
        }) {
            return workspace.to_path_buf();
        }
        return project.root().to_path_buf();
    }
    start
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(start)
        .to_path_buf()
}

impl GoEngine {
    /// An engine whose index is not built yet. Run [`GoEngine::build`] on a
    /// worker thread and hand the result to [`GoEngine::install`].
    pub fn pending(root: &Path) -> Self {
        let mut engine = Self::with_project(None);
        engine.set_root(root);
        engine
    }

    /// Makes the engine read `text` for `path` instead of the disk, unless
    /// it already holds exactly that text.
    pub fn sync_document(&mut self, path: &Path, text: &str) {
        if self.open_text(path).as_deref() != Some(text) {
            self.open_document(path, text);
        }
    }

    pub fn complete(&mut self, path: &Path, text: &str, line: u32, column: u32) -> Vec<Completion> {
        self.sync_document(path, text);
        let Some((_, tree)) = parse(path, text) else {
            return Vec::new();
        };
        complete::complete(self, path, text, &tree, line, column)
    }

    pub fn definition(&mut self, path: &Path, text: &str, line: u32, column: u32) -> Vec<Target> {
        self.sync_document(path, text);
        let Some(found) = self.symbol_at(path, text, line, column) else {
            return Vec::new();
        };
        let at = |path: &Path, line: u32, column: u32| Target {
            path: path.to_path_buf(),
            line,
            column,
        };
        match &found.symbol {
            Symbol::Local {
                path, line, column, ..
            } => vec![at(path, *line, *column)],
            Symbol::Type { path, pos, .. }
            | Symbol::Func { path, pos, .. }
            | Symbol::Value { path, pos, .. }
            | Symbol::Field { path, pos, .. }
            | Symbol::Method { path, pos, .. } => vec![at(path, pos.line, pos.column)],
            Symbol::Package { import_path } => self
                .package_by_import(import_path)
                .and_then(|pkg| pkg.files.first().map(|f| at(&f.path, 0, 0)))
                .into_iter()
                .collect(),
            Symbol::TypeParam { .. } | Symbol::Builtin { .. } => Vec::new(),
        }
    }

    pub fn hover(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<HoverResult> {
        self.sync_document(path, text);
        let found = self.symbol_at(path, text, line, column)?;
        let head = match &found.symbol {
            Symbol::Package { import_path } => format!("package \"{import_path}\""),
            Symbol::TypeParam { name } => format!("type parameter {name}"),
            Symbol::Builtin { name } if found.signature.is_empty() => name.clone(),
            _ => found.signature.clone(),
        };
        if head.is_empty() {
            return None;
        }
        let mut markdown = format!("```go\n{head}\n```");
        let doc = found.doc.trim();
        if !doc.is_empty() {
            markdown.push_str("\n\n");
            markdown.push_str(doc);
        }
        Some(HoverResult {
            markdown,
            line: found.start_line,
            start_column: found.start_column,
            end_column: found.end_column,
        })
    }

    pub fn signature(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<Signature> {
        self.sync_document(path, text);
        let (_, tree) = parse(path, text)?;
        let byte = byte_at(text, line, column);
        let callee = callee_at(self, path, text, &tree, byte)?;
        Some(Signature {
            label: callee.label,
            documentation: Some(callee.doc).filter(|doc| !doc.trim().is_empty()),
            parameters: callee.params,
            active: callee.active,
        })
    }

    /// Parameter names before the arguments of calls, and the types of
    /// variables declared with `:=`, on lines `first..=last`.
    pub fn parameter_hints(
        &mut self,
        path: &Path,
        text: &str,
        first: u32,
        last: u32,
    ) -> Vec<ParameterHint> {
        self.sync_document(path, text);
        let Some((_, tree)) = parse(path, text) else {
            return Vec::new();
        };
        super::types::parameter_hints(self, path, text, &tree, first, last)
    }

    pub fn actions(&mut self, path: &Path, text: &str, line: u32, column: u32) -> Vec<Action> {
        self.sync_document(path, text);
        actions::actions(self, path, text, line, column)
    }

    pub fn diagnostics(&mut self, path: &Path, text: &str) -> Vec<Problem> {
        self.sync_document(path, text);
        diag::diagnostics(self, path, text)
    }

    /// Every use of the symbol at the cursor, project-wide.
    pub fn references(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Vec<Occurrence> {
        self.sync_document(path, text);
        let Some((_, tree)) = parse(path, text) else {
            return Vec::new();
        };
        refs::references(self, path, text, &tree, line, column)
    }

    /// The uses to rewrite for renaming the symbol at the cursor to
    /// `new_name`, or why it cannot be renamed.
    pub fn rename(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
        new_name: &str,
    ) -> Result<Vec<Occurrence>, String> {
        self.sync_document(path, text);
        let (_, tree) = parse(path, text).ok_or_else(|| "cannot read the file".to_owned())?;
        let query = refs::reference_query(self, path, text, &tree, line, column)
            .ok_or_else(|| "nothing to rename here".to_owned())?;
        refs::can_rename(&query, new_name)?;
        Ok(refs::references(self, path, text, &tree, line, column))
    }

    fn symbol_at(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<super::types::Resolved> {
        let (_, tree) = parse(path, text)?;
        resolve_at(self, path, text, &tree, byte_at(text, line, column))
    }
}

/// Byte offset of a 0-based line and UTF-16 column; past the end of a line
/// or the text it clamps.
fn byte_at(text: &str, line: u32, column: u32) -> usize {
    let mut start = 0;
    for _ in 0..line {
        match text[start..].find('\n') {
            Some(i) => start += i + 1,
            None => return text.len(),
        }
    }
    let rest = &text[start..];
    let end = rest.find('\n').unwrap_or(rest.len());
    let mut units = 0u32;
    for (i, c) in rest[..end].char_indices() {
        if units >= column {
            return start + i;
        }
        units += c.len_utf16() as u32;
    }
    start + end
}

#[cfg(test)]
mod tests;
