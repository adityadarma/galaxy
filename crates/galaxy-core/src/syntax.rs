//! Background Tree-sitter parsing (M3-01).
//!
//! A [`SyntaxService`] belongs to one document and owns one parser on one
//! worker thread. Editing never waits for parsing: it clones Ropey's cheap
//! rope handle, tags it with `Document::revision`, and polls completed trees
//! on the UI side. A tree for an older revision is discarded.
//!
//! M3-02 adds `InputEdit` and the previous tree for incremental parsing;
//! M3-03 supplies the bundled grammars. This layer intentionally knows
//! neither language names nor highlight queries.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc::{self, Receiver, Sender, TryRecvError},
};
use std::thread::{self, JoinHandle};

use ropey::Rope;
use tree_sitter::{
    InputEdit, Language, Parser, Point, Query, QueryCursor, StreamingIterator, Tree,
};

use crate::document::Document;

/// Grammars statically linked into Galaxy (M3-03). Path and shebang matching
/// deliberately remains M3-06; callers choose the language explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyntaxLanguage {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Json,
    Toml,
    Yaml,
    Markdown,
    Python,
    Go,
    Html,
    Css,
    Bash,
}

impl SyntaxLanguage {
    pub const ALL: [Self; 13] = [
        Self::Rust,
        Self::TypeScript,
        Self::Tsx,
        Self::JavaScript,
        Self::Json,
        Self::Toml,
        Self::Yaml,
        Self::Markdown,
        Self::Python,
        Self::Go,
        Self::Html,
        Self::Css,
        Self::Bash,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::TypeScript => "TypeScript",
            Self::Tsx => "TSX",
            Self::JavaScript => "JavaScript",
            Self::Json => "JSON",
            Self::Toml => "TOML",
            Self::Yaml => "YAML",
            Self::Markdown => "Markdown",
            Self::Python => "Python",
            Self::Go => "Go",
            Self::Html => "HTML",
            Self::Css => "CSS",
            Self::Bash => "Bash",
        }
    }

    /// Preferred line-wise comment marker. Block-only languages intentionally
    /// return none: inserting only an opening marker would create invalid code.
    pub fn comment_marker(self) -> Option<&'static str> {
        match self {
            Self::Rust | Self::TypeScript | Self::Tsx | Self::JavaScript | Self::Go => Some("//"),
            Self::Toml | Self::Yaml | Self::Python | Self::Bash => Some("#"),
            Self::Json | Self::Html | Self::Css | Self::Markdown => None,
        }
    }

    pub fn grammar(self) -> Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::Json => tree_sitter_json::LANGUAGE.into(),
            Self::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
            Self::Yaml => tree_sitter_yaml::LANGUAGE.into(),
            Self::Markdown => tree_sitter_md_025::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::Html => tree_sitter_html::LANGUAGE.into(),
            Self::Css => tree_sitter_css::LANGUAGE.into(),
            Self::Bash => tree_sitter_bash::LANGUAGE.into(),
        }
    }

    /// Upstream highlights.scm bundled with the matching grammar version.
    pub fn highlight_query(self) -> &'static str {
        match self {
            Self::Rust => tree_sitter_rust::HIGHLIGHTS_QUERY,
            Self::TypeScript | Self::Tsx => tree_sitter_typescript::HIGHLIGHTS_QUERY,
            Self::JavaScript => JAVASCRIPT_HIGHLIGHTS,
            Self::Json => tree_sitter_json::HIGHLIGHTS_QUERY,
            Self::Toml => tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            Self::Yaml => tree_sitter_yaml::HIGHLIGHTS_QUERY,
            // The inline grammar is injected by M3-05; this captures Markdown
            // block nodes now and leaves inline styling as plain text.
            Self::Markdown => tree_sitter_md_025::HIGHLIGHT_QUERY_BLOCK,
            Self::Python => tree_sitter_python::HIGHLIGHTS_QUERY,
            Self::Go => tree_sitter_go::HIGHLIGHTS_QUERY,
            Self::Html => tree_sitter_html::HIGHLIGHTS_QUERY,
            Self::Css => tree_sitter_css::HIGHLIGHTS_QUERY,
            Self::Bash => BASH_HIGHLIGHTS,
        }
    }

    /// Upstream symbol query where the grammar ships one. Languages without
    /// a stable tag query deliberately have no outline rather than guesses.
    pub fn tags_query(self) -> Option<&'static str> {
        match self {
            Self::Rust => Some(tree_sitter_rust::TAGS_QUERY),
            Self::TypeScript | Self::Tsx => Some(tree_sitter_typescript::TAGS_QUERY),
            Self::JavaScript => Some(tree_sitter_javascript::TAGS_QUERY),
            Self::Python => Some(tree_sitter_python::TAGS_QUERY),
            Self::Go => Some(tree_sitter_go::TAGS_QUERY),
            _ => None,
        }
    }

    /// Compiles the grammar's query against exactly its matching grammar.
    pub fn compile_highlights(self) -> Result<Query, tree_sitter::QueryError> {
        Query::new(&self.grammar(), self.highlight_query())
    }

    /// Detects bundled languages by filename, extension, then an interpreter
    /// shebang. The first source line is enough; callers need not copy a file.
    pub fn detect(path: &str, first_line: &str) -> Option<Self> {
        let name = path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(path)
            .to_ascii_lowercase();
        let by_name = match name.as_str() {
            "dockerfile" => None,
            "cargo.toml" => Some(Self::Toml),
            _ => None,
        };
        if by_name.is_some() {
            return by_name;
        }
        let extension = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
        let by_extension = match extension {
            "rs" => Some(Self::Rust),
            "ts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            "js" | "mjs" | "cjs" => Some(Self::JavaScript),
            "json" | "jsonc" => Some(Self::Json),
            "toml" => Some(Self::Toml),
            "yaml" | "yml" => Some(Self::Yaml),
            "md" | "markdown" => Some(Self::Markdown),
            "py" | "pyw" => Some(Self::Python),
            "go" => Some(Self::Go),
            "html" | "htm" => Some(Self::Html),
            "css" => Some(Self::Css),
            "sh" | "bash" | "zsh" => Some(Self::Bash),
            _ => None,
        };
        if by_extension.is_some() {
            return by_extension;
        }
        let interpreter = first_line
            .strip_prefix("#!")?
            .split_whitespace()
            .last()?
            .rsplit('/')
            .next()?;
        match interpreter {
            "bash" | "sh" | "zsh" => Some(Self::Bash),
            "python" | "python3" => Some(Self::Python),
            "node" | "deno" => Some(Self::JavaScript),
            _ => None,
        }
    }
}

/// A definition captured from a completed Tree-sitter tag query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineSymbol {
    pub name: String,
    pub kind: String,
    pub line: usize,
    pub column_utf16: usize,
}

/// Extracts definitions from a completed tree. This never parses or blocks.
pub fn outline_symbols(
    doc: &Document,
    tree: &Tree,
    language: SyntaxLanguage,
) -> Vec<OutlineSymbol> {
    let Some(source) = language.tags_query() else {
        return Vec::new();
    };
    let Ok(query) = Query::new(&language.grammar(), source) else {
        return Vec::new();
    };
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let rope = doc.rope();
    let source = rope.to_string();
    let mut out = Vec::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    while let Some(m) = matches.next() {
        let mut name = None;
        let mut kind = None;
        for capture in m.captures {
            let capture_name = names[capture.index as usize];
            if capture_name == "name" {
                name = Some(capture.node);
            }
            if let Some(definition) = capture_name.strip_prefix("definition.") {
                kind = Some(definition);
            }
        }
        let (Some(name), Some(kind)) = (name, kind) else {
            continue;
        };
        let start = rope.byte_to_char(name.start_byte());
        let end = rope.byte_to_char(name.end_byte());
        let pos = doc.char_to_utf16(start);
        out.push(OutlineSymbol {
            name: rope.slice(start..end).to_string(),
            kind: kind.to_owned(),
            line: pos.line,
            column_utf16: pos.col,
        });
    }
    out.sort_by_key(|symbol| (symbol.line, symbol.column_utf16));
    out.dedup_by_key(|symbol| (symbol.line, symbol.column_utf16));
    out
}

// These two grammar crates do not export their upstream query. Keep a small,
// self-contained equivalent here rather than depending on Cargo registry paths.
const JAVASCRIPT_HIGHLIGHTS: &str = r#"
[(comment)] @comment
[(string) (template_string) (regex)] @string
(number) @number
[(true) (false) (null) (undefined)] @constant
(identifier) @variable
(property_identifier) @property
(function_declaration name: (identifier) @function)
(call_expression function: (identifier) @function)
[("const") ("let") ("var") ("function") ("class") ("return") ("if") ("else") ("for") ("while") ("import") ("export") ("async") ("await") ("new")] @keyword
[("+") ("-") ("*") ("/") ("=") ("==") ("===") ("=>") ("!") ("&&") ("||")] @operator
[("(") (")") ("[") ("]") ("{") ("}") (",") (";") (".")] @punctuation
"#;

const BASH_HIGHLIGHTS: &str = r#"
(comment) @comment
[(string) (raw_string) (heredoc_body)] @string
(number) @number
(variable_name) @variable
(command_name) @function
[("if") ("then") ("else") ("elif") ("fi") ("for") ("while") ("do") ("done") ("case") ("esac") ("function") ("in")] @keyword
[("|") ("||") ("&&") (";") ("&")] @operator
[("(") (")") ("{") ("}") (";")] @punctuation
"#;

/// Semantic color categories. Themes map these scopes to concrete colors in
/// M3-07. The string spelling follows upstream Tree-sitter/Helix captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HighlightScope {
    Attribute,
    Comment,
    Constant,
    Constructor,
    Function,
    Keyword,
    Label,
    Number,
    Operator,
    Property,
    Punctuation,
    String,
    Tag,
    Type,
    Variable,
}

impl HighlightScope {
    /// Maps a capture name, including modifiers like `function.builtin`, to
    /// the stable base scope consumed by themes and viewport style runs.
    pub fn from_capture(capture: &str) -> Option<Self> {
        match capture.split('.').next()? {
            "attribute" => Some(Self::Attribute),
            "comment" => Some(Self::Comment),
            "constant" | "boolean" => Some(Self::Constant),
            "constructor" => Some(Self::Constructor),
            "function" | "method" => Some(Self::Function),
            "keyword" => Some(Self::Keyword),
            "label" => Some(Self::Label),
            "number" | "float" => Some(Self::Number),
            "operator" => Some(Self::Operator),
            "property" | "field" => Some(Self::Property),
            "punctuation" | "delimiter" => Some(Self::Punctuation),
            "string" | "character" => Some(Self::String),
            "tag" => Some(Self::Tag),
            "type" | "namespace" => Some(Self::Type),
            "variable" | "parameter" => Some(Self::Variable),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Attribute => "attribute",
            Self::Comment => "comment",
            Self::Constant => "constant",
            Self::Constructor => "constructor",
            Self::Function => "function",
            Self::Keyword => "keyword",
            Self::Label => "label",
            Self::Number => "number",
            Self::Operator => "operator",
            Self::Property => "property",
            Self::Punctuation => "punctuation",
            Self::String => "string",
            Self::Tag => "tag",
            Self::Type => "type",
            Self::Variable => "variable",
        }
    }
}

/// A semantic style range on one document line, in UTF-16 columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyleRun {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub scope: HighlightScope,
}

/// Extra lines queried either side of the visible viewport. They make a
/// future style cache useful immediately after a short scroll without ever
/// querying a whole document on a frame.
pub const VIEWPORT_STYLE_MARGIN_LINES: usize = 10;

/// Returns semantic style runs for `first_line..first_line + count`, expanded
/// by [`VIEWPORT_STYLE_MARGIN_LINES`]. Ranges are clipped to displayed text so
/// renderer columns stay valid even for minified lines cut by the viewport.
pub fn style_runs_for_viewport(
    doc: &Document,
    tree: &Tree,
    language: SyntaxLanguage,
    first_line: usize,
    count: usize,
) -> Vec<StyleRun> {
    let line_count = doc.line_count();
    let first = first_line.saturating_sub(VIEWPORT_STYLE_MARGIN_LINES);
    let last = first_line
        .saturating_add(count)
        .saturating_add(VIEWPORT_STYLE_MARGIN_LINES)
        .min(line_count);
    if first >= last {
        return Vec::new();
    }
    let rope = doc.rope();
    let start_byte = rope.line_to_byte(first);
    let end_byte = if last == line_count {
        rope.len_bytes()
    } else {
        rope.line_to_byte(last)
    };
    let query = match language.compile_highlights() {
        Ok(query) => query,
        Err(_) => return Vec::new(), // grammar queries are validated in tests
    };
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(start_byte..end_byte);
    let text = rope.to_string();
    let mut runs = Vec::new();
    let mut matches = cursor.matches(&query, tree.root_node(), text.as_bytes());
    while let Some(m) = matches.next() {
        for capture in m.captures {
            let Some(scope) = HighlightScope::from_capture(names[capture.index as usize]) else {
                continue;
            };
            let node = capture.node;
            let start = node.start_byte().max(start_byte);
            let end = node.end_byte().min(end_byte);
            if start >= end {
                continue;
            }
            let start_char = rope.byte_to_char(start);
            let end_char = rope.byte_to_char(end);
            let start_line = rope.char_to_line(start_char);
            let end_line = rope.char_to_line(end_char);
            for line in start_line..=end_line.min(last - 1) {
                let line_start = rope.line_to_char(line);
                let line_end = doc
                    .line_end_char(line)
                    .min(line_start + crate::viewport::MAX_LINE_DISPLAY_CHARS);
                let from = start_char.max(line_start);
                let to = end_char.min(line_end);
                if from < to {
                    let base = rope.char_to_utf16_cu(line_start);
                    runs.push(StyleRun {
                        line,
                        start: rope.char_to_utf16_cu(from) - base,
                        end: rope.char_to_utf16_cu(to) - base,
                        scope,
                    });
                }
            }
        }
    }
    runs.sort_by_key(|run| (run.line, run.start, run.end));
    runs
}

struct Request {
    revision: u64,
    text: Rope,
    /// The revision that `edit` transforms, when this is an edit request.
    base_revision: Option<u64>,
    edit: Option<SyntaxEdit>,
}

/// One normalized text mutation expressed in Tree-sitter byte coordinates.
///
/// `from_byte_ranges` computes the row/byte-column points from Rope chunks.
/// The byte positions must be UTF-8 character boundaries; editor changes
/// already satisfy that invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxEdit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start_position: Point,
    pub old_end_position: Point,
    pub new_end_position: Point,
}

impl SyntaxEdit {
    pub fn from_byte_ranges(
        old: &Rope,
        new: &Rope,
        start_byte: usize,
        old_end_byte: usize,
        new_end_byte: usize,
    ) -> Self {
        Self {
            start_byte,
            old_end_byte,
            new_end_byte,
            start_position: point_at_byte(old, start_byte),
            old_end_position: point_at_byte(old, old_end_byte),
            new_end_position: point_at_byte(new, new_end_byte),
        }
    }

    fn input_edit(self) -> InputEdit {
        InputEdit {
            start_byte: self.start_byte,
            old_end_byte: self.old_end_byte,
            new_end_byte: self.new_end_byte,
            start_position: self.start_position,
            old_end_position: self.old_end_position,
            new_end_position: self.new_end_position,
        }
    }
}

fn point_at_byte(text: &Rope, byte: usize) -> Point {
    let byte = byte.min(text.len_bytes());
    let char_idx = text.byte_to_char(byte);
    let row = text.char_to_line(char_idx);
    Point {
        row,
        column: byte - text.line_to_byte(row),
    }
}

/// A parsed concrete syntax tree, tied to the document revision it parsed.
#[derive(Debug)]
pub struct SyntaxSnapshot {
    revision: u64,
    tree: Tree,
    incremental: bool,
}

impl SyntaxSnapshot {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    /// Whether this tree reused an immediately previous parsed tree.
    pub fn is_incremental(&self) -> bool {
        self.incremental
    }
}

/// One background parser for one document.
///
/// `Language` is injected so this module remains independent of the grammar
/// bundle. Send a document after loading and after each text change, then
/// poll from the UI event loop without blocking.
pub struct SyntaxService {
    requests: Option<Sender<Request>>,
    results: Receiver<SyntaxSnapshot>,
    latest_requested: Arc<AtomicU64>,
    worker: Option<JoinHandle<()>>,
}

impl SyntaxService {
    pub fn new(language: Language) -> Self {
        let (request_tx, request_rx) = mpsc::channel::<Request>();
        let (result_tx, result_rx) = mpsc::channel::<SyntaxSnapshot>();
        let latest_requested = Arc::new(AtomicU64::new(0));
        let latest_for_worker = Arc::clone(&latest_requested);
        let worker = thread::Builder::new()
            .name("galaxy-syntax".into())
            .spawn(move || run(language, request_rx, result_tx, latest_for_worker))
            .expect("creating syntax worker thread");
        Self {
            requests: Some(request_tx),
            results: result_rx,
            latest_requested,
            worker: Some(worker),
        }
    }

    /// Queues the document's current rope. Cloning a Rope is O(1): text is
    /// still read by Tree-sitter through rope chunks on the worker.
    pub fn submit_document(&self, doc: &Document) {
        self.submit(doc.revision(), doc.rope().clone());
    }

    pub fn submit(&self, revision: u64, text: Rope) {
        self.send(Request {
            revision,
            text,
            base_revision: None,
            edit: None,
        });
    }

    /// Queues an edit from `base_revision` to `revision`. The worker uses the
    /// old tree only when it has parsed exactly `base_revision`; otherwise it
    /// parses the submitted rope in full, preserving correctness under load.
    pub fn submit_edit(&self, base_revision: u64, revision: u64, text: Rope, edit: SyntaxEdit) {
        self.send(Request {
            revision,
            text,
            base_revision: Some(base_revision),
            edit: Some(edit),
        });
    }

    fn send(&self, request: Request) {
        let revision = request.revision;
        self.latest_requested.store(revision, Ordering::Release);
        // The receiver only disappears while Drop is joining the worker.
        let _ = self
            .requests
            .as_ref()
            .expect("syntax service is alive")
            .send(request);
    }

    /// Returns the newest completed tree, if any. Older results are ignored
    /// even if a request raced with worker completion.
    pub fn poll(&self) -> Option<SyntaxSnapshot> {
        let mut newest = None;
        loop {
            match self.results.try_recv() {
                Ok(snapshot)
                    if snapshot.revision == self.latest_requested.load(Ordering::Acquire) =>
                {
                    newest = Some(snapshot);
                }
                Ok(_) => {}
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return newest,
            }
        }
    }
}

impl Drop for SyntaxService {
    fn drop(&mut self) {
        // Closing the sender lets the worker finish its current parse then
        // exit. Joining avoids a parser thread outliving its document.
        self.requests.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run(
    language: Language,
    requests: Receiver<Request>,
    results: Sender<SyntaxSnapshot>,
    latest: Arc<AtomicU64>,
) {
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .expect("valid Tree-sitter language");
    let mut previous = None::<(u64, Tree)>;
    while let Ok(request) = requests.recv() {
        let text = request.text;
        let incremental = matches!(
            (request.base_revision, request.edit, previous.as_ref()),
            (Some(base), Some(_), Some((revision, _))) if base == *revision
        );
        let old_tree = if incremental {
            let (_, tree) = previous.as_mut().expect("matched above");
            tree.edit(&request.edit.expect("matched above").input_edit());
            Some(tree.clone())
        } else {
            None
        };
        let tree = parser.parse_with_options(
            &mut |byte, _| {
                if byte >= text.len_bytes() {
                    return "";
                }
                let (chunk, chunk_start, _, _) = text.chunk_at_byte(byte);
                &chunk[byte - chunk_start..]
            },
            old_tree.as_ref(),
            None,
        );
        let Some(tree) = tree else {
            previous = None;
            continue;
        };
        previous = Some((request.revision, tree.clone()));
        // A newer document was submitted while parsing. Do not make an old
        // tree observable; the next request will replace parser state later.
        if request.revision != latest.load(Ordering::Acquire) {
            continue;
        }
        let _ = results.send(SyntaxSnapshot {
            revision: request.revision,
            tree,
            incremental,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use tree_sitter::{QueryCursor, StreamingIterator};

    fn wait_for(service: &SyntaxService) -> SyntaxSnapshot {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(snapshot) = service.poll() {
                return snapshot;
            }
            assert!(Instant::now() < deadline, "syntax worker did not finish");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn worker_parses_rope_chunks_without_blocking_submitter() {
        let service = SyntaxService::new(tree_sitter_json::LANGUAGE.into());
        let doc = Document::from_text("{\"answer\": 42}");
        service.submit_document(&doc);

        let snapshot = wait_for(&service);
        assert_eq!(snapshot.revision(), doc.revision());
        assert_eq!(snapshot.tree().root_node().kind(), "document");
        assert!(!snapshot.tree().root_node().has_error());
    }

    #[test]
    fn a_stale_tree_is_not_observable() {
        let service = SyntaxService::new(tree_sitter_json::LANGUAGE.into());
        // A large first request makes it very likely the second arrives while
        // the worker is still parsing; correctness does not depend on timing.
        service.submit(1, Rope::from_str(&format!("[{}]", "0,".repeat(100_000))));
        service.submit(2, Rope::from_str("[true]"));

        let snapshot = wait_for(&service);
        assert_eq!(snapshot.revision(), 2);
        assert_eq!(snapshot.tree().root_node().kind(), "document");
    }

    #[test]
    fn parses_an_edit_incrementally_with_utf8_and_newlines() {
        let service = SyntaxService::new(tree_sitter_json::LANGUAGE.into());
        let before = Rope::from_str("{\"name\": \"world\"}");
        service.submit(10, before.clone());
        let first = wait_for(&service);
        assert!(!first.is_incremental());

        let after = Rope::from_str("{\n  \"name\": \"世界\"\n}");
        let edit = SyntaxEdit::from_byte_ranges(
            &before,
            &after,
            1,
            before.len_bytes() - 1,
            after.len_bytes() - 1,
        );
        assert_eq!(edit.start_position, Point { row: 0, column: 1 });
        assert_eq!(edit.new_end_position, Point { row: 2, column: 0 });
        service.submit_edit(10, 11, after, edit);

        let second = wait_for(&service);
        assert_eq!(second.revision(), 11);
        assert!(second.is_incremental());
        assert!(!second.tree().root_node().has_error());
    }

    #[test]
    fn non_contiguous_edit_falls_back_to_full_parse() {
        let service = SyntaxService::new(tree_sitter_json::LANGUAGE.into());
        let first = Rope::from_str("[1]");
        service.submit(1, first.clone());
        let _ = wait_for(&service);

        let second = Rope::from_str("[2]");
        let edit = SyntaxEdit::from_byte_ranges(&first, &second, 1, 2, 2);
        // Revision 7 was never parsed, so using its edit would be invalid.
        service.submit_edit(7, 8, second, edit);
        let snapshot = wait_for(&service);
        assert_eq!(snapshot.revision(), 8);
        assert!(!snapshot.is_incremental());
        assert!(!snapshot.tree().root_node().has_error());
    }

    #[test]
    fn every_bundled_grammar_loads_and_parses_its_language() {
        let samples = [
            (SyntaxLanguage::Rust, "fn main() { println!(\"hi\"); }"),
            (SyntaxLanguage::TypeScript, "const n: number = 1;"),
            (SyntaxLanguage::Tsx, "const node = <main>hi</main>;"),
            (SyntaxLanguage::JavaScript, "const n = 1;"),
            (SyntaxLanguage::Json, "{\"n\": 1}"),
            (SyntaxLanguage::Toml, "title = \"Galaxy\""),
            (SyntaxLanguage::Yaml, "name: Galaxy"),
            (SyntaxLanguage::Markdown, "# Galaxy\n\nText."),
            (SyntaxLanguage::Python, "def f():\n    return 1"),
            (SyntaxLanguage::Go, "package main\nfunc main() {}"),
            (SyntaxLanguage::Html, "<main>Galaxy</main>"),
            (SyntaxLanguage::Css, "main { color: red; }"),
            (SyntaxLanguage::Bash, "#!/usr/bin/env bash\necho Galaxy"),
        ];
        assert_eq!(SyntaxLanguage::ALL.len(), samples.len());
        for (language, source) in samples {
            let mut parser = Parser::new();
            parser.set_language(&language.grammar()).unwrap();
            let tree = parser.parse(source, None).unwrap();
            assert!(
                !tree.root_node().has_error(),
                "{} did not parse its sample: {}",
                language.name(),
                tree.root_node().to_sexp()
            );
        }
    }

    #[test]
    fn every_bundled_highlight_query_compiles() {
        for language in SyntaxLanguage::ALL {
            let query = language.compile_highlights().unwrap_or_else(|error| {
                panic!(
                    "{} highlight query did not compile: {error}",
                    language.name()
                )
            });
            assert!(
                !query.capture_names().is_empty(),
                "{} has no highlight captures",
                language.name()
            );
        }
    }

    #[test]
    fn capture_names_map_to_stable_theme_scopes() {
        assert_eq!(
            HighlightScope::from_capture("keyword.control"),
            Some(HighlightScope::Keyword)
        );
        assert_eq!(
            HighlightScope::from_capture("function.builtin"),
            Some(HighlightScope::Function)
        );
        assert_eq!(
            HighlightScope::from_capture("punctuation.bracket"),
            Some(HighlightScope::Punctuation)
        );
        assert_eq!(
            HighlightScope::from_capture("string.special.path"),
            Some(HighlightScope::String)
        );
        assert_eq!(HighlightScope::from_capture("embedded"), None);
    }

    #[test]
    fn rust_query_emits_keyword_string_and_comment_scopes() {
        let language = SyntaxLanguage::Rust;
        let source = "// note\nfn main() { let name = \"Galaxy\"; }";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let query = language.compile_highlights().unwrap();
        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
        let mut scopes = Vec::new();
        while let Some(m) = matches.next() {
            scopes.extend(
                m.captures.iter().filter_map(|capture| {
                    HighlightScope::from_capture(names[capture.index as usize])
                }),
            );
        }
        assert!(scopes.contains(&HighlightScope::Comment));
        assert!(scopes.contains(&HighlightScope::Keyword));
        assert!(scopes.contains(&HighlightScope::String));
    }

    #[test]
    fn detects_extensions_special_names_and_shebangs() {
        assert_eq!(
            SyntaxLanguage::detect("/w/main.rs", ""),
            Some(SyntaxLanguage::Rust)
        );
        assert_eq!(
            SyntaxLanguage::detect("Cargo.toml", ""),
            Some(SyntaxLanguage::Toml)
        );
        assert_eq!(
            SyntaxLanguage::detect("script", "#!/usr/bin/env python3"),
            Some(SyntaxLanguage::Python)
        );
        assert_eq!(
            SyntaxLanguage::detect("script", "#!/bin/bash"),
            Some(SyntaxLanguage::Bash)
        );
        assert_eq!(SyntaxLanguage::detect("notes.txt", "hello"), None);
    }

    #[test]
    fn comment_markers_only_include_line_comment_languages() {
        assert_eq!(SyntaxLanguage::Rust.comment_marker(), Some("//"));
        assert_eq!(SyntaxLanguage::Python.comment_marker(), Some("#"));
        assert_eq!(SyntaxLanguage::Json.comment_marker(), None);
        assert_eq!(SyntaxLanguage::Html.comment_marker(), None);
    }

    #[test]
    fn extracts_rust_outline_from_completed_tree() {
        let source = "struct Account;\nimpl Account { fn deposit(&self) {} }\nfn main() {}\n";
        let doc = Document::from_text(source);
        let mut parser = Parser::new();
        parser
            .set_language(&SyntaxLanguage::Rust.grammar())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let symbols = outline_symbols(&doc, &tree, SyntaxLanguage::Rust);
        assert_eq!(
            symbols
                .iter()
                .map(|symbol| symbol.name.as_str())
                .collect::<Vec<_>>(),
            ["Account", "deposit", "main"]
        );
        assert_eq!(symbols[0].kind, "class");
        assert_eq!(symbols[1].kind, "method");
    }

    #[test]
    fn styles_are_utf16_bounded_and_include_the_scroll_margin() {
        let doc = Document::from_text("// ���\nfn main() {\n  let name = \"Galaxy\";\n}\nend");
        let language = SyntaxLanguage::Rust;
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(doc.text(), None).unwrap();
        // View line 2; line 0 is included because the 10-line margin reaches it.
        let runs = style_runs_for_viewport(&doc, &tree, language, 2, 1);
        assert!(
            runs.iter()
                .any(|run| run.line == 0 && run.scope == HighlightScope::Comment)
        );
        assert!(
            runs.iter()
                .any(|run| run.line == 2 && run.scope == HighlightScope::String)
        );
        for run in runs {
            let text = crate::viewport::display_text(&doc, run.line);
            assert!(run.start < run.end);
            assert!(run.end <= text.encode_utf16().count());
        }
    }
}
