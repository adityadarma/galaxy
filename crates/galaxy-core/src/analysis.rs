//! Result types shared by the built-in language engines and the editor:
//! positions are UTF-16 columns, as the editor and the FFI expect.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start_line: u64,
    pub start_character: u64,
    pub end_line: u64,
    pub end_character: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub uri: String,
    pub range: Range,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
    /// LSP `DiagnosticTag.Unnecessary` (1): unused code, shown faded rather
    /// than underlined.
    pub unnecessary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverInfo {
    pub contents: String,
    pub range: Option<Range>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    pub detail: Option<String>,
    pub insert_text: String,
    pub is_snippet: bool,
    pub sort_text: Option<String>,
    /// Edits elsewhere in the file applied with the item (a `use` import).
    pub additional_edits: Vec<RenameEdit>,
    /// UTF-16 column on the cursor's line where the text to replace starts,
    /// when it is more than the word before the cursor.
    pub replace_from: Option<u64>,
    /// LSP `CompletionItemKind` (2 method, 3 function, 7 class, ...).
    pub kind: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureInfo {
    pub label: String,
    pub documentation: Option<String>,
    pub parameters: Vec<String>,
    pub active_parameter: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionLocation {
    pub uri: String,
    pub range: Range,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameEdit {
    pub uri: String,
    pub range: Range,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeActionInfo {
    pub title: String,
    pub kind: Option<String>,
    pub edits: Vec<RenameEdit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentSymbolInfo {
    pub uri: Option<String>,
    pub name: String,
    pub kind: u64,
    pub range: Range,
    pub children: Vec<DocumentSymbolInfo>,
}

/// What the built-in engines last answered for each document (by URI). The
/// editor reads these back after asking an engine, so a request never waits
/// on the UI thread.
#[derive(Debug, Default)]
pub struct BuiltinStore {
    diagnostics: std::collections::HashMap<String, Vec<Diagnostic>>,
    hover: std::collections::HashMap<String, HoverInfo>,
    completion: std::collections::HashMap<String, Vec<CompletionItem>>,
    definitions: std::collections::HashMap<String, Vec<DefinitionLocation>>,
    references: std::collections::HashMap<String, Vec<DefinitionLocation>>,
    rename_edits: std::collections::HashMap<String, Vec<RenameEdit>>,
    signatures: std::collections::HashMap<String, SignatureInfo>,
    code_actions: std::collections::HashMap<String, Vec<CodeActionInfo>>,
    document_symbols: std::collections::HashMap<String, Vec<DocumentSymbolInfo>>,
}

impl BuiltinStore {
    /// A workspace switch invalidates every document-scoped response. Keeping
    /// them would show diagnostics and navigation results from the old root.
    pub fn clear(&mut self) {
        self.diagnostics.clear();
        self.hover.clear();
        self.completion.clear();
        self.definitions.clear();
        self.references.clear();
        self.rename_edits.clear();
        self.signatures.clear();
        self.code_actions.clear();
        self.document_symbols.clear();
    }

    pub fn diagnostics(&self, uri: &str) -> &[Diagnostic] {
        self.diagnostics.get(uri).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Every problem in every document, ordered by file, position, severity.
    pub fn problems(&self) -> Vec<&Diagnostic> {
        let mut out: Vec<_> = self.diagnostics.values().flatten().collect();
        out.sort_by_key(|d| {
            (
                &d.uri,
                d.range.start_line,
                d.range.start_character,
                d.severity,
            )
        });
        out
    }

    /// Replaces the problems of `uri` (none clears them). `false` when
    /// nothing changed.
    pub fn set_diagnostics(&mut self, uri: &str, diagnostics: Vec<Diagnostic>) -> bool {
        if diagnostics.is_empty() {
            return self.diagnostics.remove(uri).is_some();
        }
        if self.diagnostics.get(uri) == Some(&diagnostics) {
            return false;
        }
        self.diagnostics.insert(uri.to_owned(), diagnostics);
        true
    }

    /// Drops diagnostics matching a workspace URI prefix. Workspace scanners
    /// use this to replace provisional open-document results atomically.
    pub fn clear_diagnostics_under(&mut self, uri_prefix: &str) {
        self.diagnostics
            .retain(|uri, _| !uri.starts_with(uri_prefix));
    }

    pub fn hover(&self, uri: &str) -> Option<&HoverInfo> {
        self.hover.get(uri)
    }

    pub fn set_hover(&mut self, uri: &str, hover: Option<HoverInfo>) {
        match hover {
            Some(hover) => self.hover.insert(uri.to_owned(), hover),
            None => self.hover.remove(uri),
        };
    }

    pub fn completions(&self, uri: &str) -> &[CompletionItem] {
        self.completion.get(uri).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn set_completion(&mut self, uri: &str, items: Vec<CompletionItem>) {
        self.completion.insert(uri.to_owned(), items);
    }

    pub fn definition_locations(&self, uri: &str) -> &[DefinitionLocation] {
        self.definitions.get(uri).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn set_definitions(&mut self, uri: &str, locations: Vec<DefinitionLocation>) {
        self.definitions.insert(uri.to_owned(), locations);
    }

    pub fn references(&self, uri: &str) -> &[DefinitionLocation] {
        self.references.get(uri).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn set_references(&mut self, uri: &str, locations: Vec<DefinitionLocation>) {
        self.references.insert(uri.to_owned(), locations);
    }

    pub fn rename_edits(&self, uri: &str) -> &[RenameEdit] {
        self.rename_edits.get(uri).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn set_rename_edits(&mut self, uri: &str, edits: Vec<RenameEdit>) {
        self.rename_edits.insert(uri.to_owned(), edits);
    }

    pub fn signature_help(&self, uri: &str) -> Option<&SignatureInfo> {
        self.signatures.get(uri)
    }

    pub fn set_signature(&mut self, uri: &str, signature: Option<SignatureInfo>) {
        match signature {
            Some(signature) => self.signatures.insert(uri.to_owned(), signature),
            None => self.signatures.remove(uri),
        };
    }

    pub fn code_actions(&self, uri: &str) -> &[CodeActionInfo] {
        self.code_actions.get(uri).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn set_code_actions(&mut self, uri: &str, actions: Vec<CodeActionInfo>) {
        self.code_actions.insert(uri.to_owned(), actions);
    }

    pub fn document_symbols(&self, uri: &str) -> &[DocumentSymbolInfo] {
        self.document_symbols
            .get(uri)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn set_document_symbols(&mut self, uri: &str, symbols: Vec<DocumentSymbolInfo>) {
        self.document_symbols.insert(uri.to_owned(), symbols);
    }

    /// Forgets everything stored for `uri` (the document was closed).
    pub fn forget(&mut self, uri: &str) {
        self.diagnostics.remove(uri);
        self.hover.remove(uri);
        self.completion.remove(uri);
        self.definitions.remove(uri);
        self.references.remove(uri);
        self.rename_edits.remove(uri);
        self.signatures.remove(uri);
        self.code_actions.remove(uri);
        self.document_symbols.remove(uri);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn problem(message: &str) -> Diagnostic {
        Diagnostic {
            uri: "file:///a.php".into(),
            range: Range {
                start_line: 1,
                start_character: 0,
                end_line: 1,
                end_character: 3,
            },
            severity: DiagnosticSeverity::Warning,
            message: message.into(),
            source: Some("galaxy".into()),
            code: None,
            unnecessary: false,
        }
    }

    #[test]
    fn diagnostics_report_whether_anything_changed() {
        let mut store = BuiltinStore::default();
        assert!(!store.set_diagnostics("file:///a.php", Vec::new()));
        assert!(store.set_diagnostics("file:///a.php", vec![problem("one")]));
        assert!(!store.set_diagnostics("file:///a.php", vec![problem("one")]));
        assert!(store.set_diagnostics("file:///a.php", vec![problem("two")]));
        assert_eq!(store.problems().len(), 1);
        assert!(store.set_diagnostics("file:///a.php", Vec::new()));
        assert!(store.diagnostics("file:///a.php").is_empty());
    }

    #[test]
    fn clear_drops_results_from_the_previous_workspace() {
        let mut store = BuiltinStore::default();
        store.set_diagnostics("file:///old/stub.php", vec![problem("old")]);
        store.set_hover(
            "file:///old/stub.php",
            Some(HoverInfo {
                contents: "old hover".into(),
                range: None,
            }),
        );
        store.clear();
        assert!(store.problems().is_empty());
        assert!(store.hover("file:///old/stub.php").is_none());
    }

    #[test]
    fn forget_drops_every_answer_for_a_document() {
        let mut store = BuiltinStore::default();
        store.set_hover(
            "file:///a.php",
            Some(HoverInfo {
                contents: "x".into(),
                range: None,
            }),
        );
        store.set_diagnostics("file:///a.php", vec![problem("one")]);
        store.forget("file:///a.php");
        assert!(store.hover("file:///a.php").is_none());
        assert!(store.problems().is_empty());
    }
}
