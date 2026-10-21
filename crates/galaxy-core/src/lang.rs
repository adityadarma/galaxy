//! Result types every built-in language engine returns to the editor.
//!
//! Lines are 0-based and columns are UTF-16 code units, which is what the
//! editor and the FFI expect. An engine for another language fills the same
//! types, so the FFI and the UI stay language-neutral.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Class,
    Interface,
    Trait,
    Enum,
    Method,
    Property,
    Constant,
    Variable,
    Function,
    Keyword,
}

/// Text to insert at a position of the original document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Insertion {
    pub line: u32,
    pub column: u32,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub label: String,
    pub detail: Option<String>,
    pub insert_text: String,
    pub is_snippet: bool,
    pub kind: ItemKind,
    pub sort: String,
    /// Extra edits elsewhere in the file, such as a `use` import.
    pub extra: Vec<Insertion>,
    /// UTF-16 column where the text to replace starts, when it is not just
    /// the word before the cursor (`app.na` in `config('app.na')`).
    pub replace_from: Option<u32>,
}

/// Where a symbol is declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub path: PathBuf,
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverResult {
    pub markdown: String,
    pub line: u32,
    pub start_column: u32,
    pub end_column: u32,
}

/// The call the cursor is inside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub label: String,
    pub documentation: Option<String>,
    pub parameters: Vec<String>,
    /// Index of the argument being typed.
    pub active: u32,
}

/// A name of a file's outline, with the members of a class below it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineSymbol {
    pub name: String,
    pub kind: &'static str,
    pub line: u32,
    pub column: u32,
    pub children: Vec<OutlineSymbol>,
}

/// A change to the text; columns are UTF-16.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub text: String,
}

/// Text inserted into another file, which is created when it is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: std::path::PathBuf,
    pub line: u32,
    pub column: u32,
    pub text: String,
}

/// A quick fix and the edits that make it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub title: String,
    pub edits: Vec<Edit>,
    /// Changes to other files.
    pub files: Vec<FileChange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Hint,
}

/// A problem in the text. Columns are UTF-16.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub severity: Severity,
    pub message: String,
    /// Unused code, drawn faded rather than underlined.
    pub unnecessary: bool,
}

/// One use of a symbol. Columns are UTF-16 on a single line.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Occurrence {
    pub path: PathBuf,
    pub line: u32,
    pub start_column: u32,
    pub end_column: u32,
    /// The range includes the `$` of a variable.
    pub dollar: bool,
}
