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

use std::sync::OnceLock;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
    mpsc::{self, Receiver, Sender, TryRecvError},
};
use std::thread::{self, JoinHandle};

use ropey::Rope;
use tree_sitter::{
    InputEdit, Language, Parser, Point, Query, QueryCursor, StreamingIterator, TextProvider, Tree,
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
    Php,
    C,
    Cpp,
    CSharp,
    Java,
    Ruby,
    Lua,
    Swift,
    Sql,
    Make,
    Ini,
    PowerShell,
    Dart,
    R,
    /// `.blade.php`: HTML grammar plus the [`crate::blade`] overlay.
    Blade,
}

impl SyntaxLanguage {
    pub const ALL: [Self; 28] = [
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
        Self::Php,
        Self::C,
        Self::Cpp,
        Self::CSharp,
        Self::Java,
        Self::Ruby,
        Self::Lua,
        Self::Swift,
        Self::Sql,
        Self::Make,
        Self::Ini,
        Self::PowerShell,
        Self::Dart,
        Self::R,
        Self::Blade,
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
            Self::Php => "PHP",
            Self::C => "C",
            Self::Cpp => "C++",
            Self::CSharp => "C#",
            Self::Java => "Java",
            Self::Ruby => "Ruby",
            Self::Lua => "Lua",
            Self::Swift => "Swift",
            Self::Sql => "SQL",
            Self::Make => "Makefile",
            Self::Ini => "INI",
            Self::PowerShell => "PowerShell",
            Self::Dart => "Dart",
            Self::R => "R",
            Self::Blade => "Blade",
        }
    }

    /// Preferred line-wise comment marker. Block-only languages intentionally
    /// return none: inserting only an opening marker would create invalid code.
    pub fn comment_marker(self) -> Option<&'static str> {
        match self {
            Self::Rust | Self::TypeScript | Self::Tsx | Self::JavaScript | Self::Go => Some("//"),
            Self::Toml | Self::Yaml | Self::Python | Self::Bash => Some("#"),
            Self::Ruby | Self::Make | Self::PowerShell | Self::R => Some("#"),
            Self::C | Self::Cpp | Self::CSharp | Self::Java | Self::Swift | Self::Dart => {
                Some("//")
            }
            Self::Lua | Self::Sql => Some("--"),
            Self::Ini => Some(";"),
            Self::Json | Self::Html | Self::Css | Self::Markdown | Self::Blade => None,
            Self::Php => Some("//"),
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
            Self::Html | Self::Blade => tree_sitter_html::LANGUAGE.into(),
            Self::Css => tree_sitter_css::LANGUAGE.into(),
            Self::Bash => tree_sitter_bash::LANGUAGE.into(),
            Self::Php => tree_sitter_php::LANGUAGE_PHP.into(),
            Self::C => tree_sitter_c::LANGUAGE.into(),
            Self::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Self::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Self::Java => tree_sitter_java::LANGUAGE.into(),
            Self::Ruby => tree_sitter_ruby::LANGUAGE.into(),
            Self::Lua => tree_sitter_lua::LANGUAGE.into(),
            Self::Swift => tree_sitter_swift::LANGUAGE.into(),
            Self::Sql => tree_sitter_sequel::LANGUAGE.into(),
            Self::Make => tree_sitter_make::LANGUAGE.into(),
            Self::Ini => tree_sitter_ini::LANGUAGE.into(),
            Self::PowerShell => tree_sitter_powershell::LANGUAGE.into(),
            Self::Dart => tree_sitter_dart::LANGUAGE.into(),
            Self::R => tree_sitter_r::LANGUAGE.into(),
        }
    }

    /// Upstream highlights.scm bundled with the matching grammar version.
    pub fn highlight_query(self) -> &'static str {
        match self {
            Self::Rust => tree_sitter_rust::HIGHLIGHTS_QUERY,
            Self::TypeScript | Self::Tsx => TYPESCRIPT_HIGHLIGHTS,
            Self::JavaScript => JAVASCRIPT_HIGHLIGHTS,
            Self::Json => tree_sitter_json::HIGHLIGHTS_QUERY,
            Self::Toml => tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            Self::Yaml => tree_sitter_yaml::HIGHLIGHTS_QUERY,
            // The inline grammar is injected by M3-05; this captures Markdown
            // block nodes now and leaves inline styling as plain text.
            Self::Markdown => tree_sitter_md_025::HIGHLIGHT_QUERY_BLOCK,
            Self::Python => tree_sitter_python::HIGHLIGHTS_QUERY,
            Self::Go => tree_sitter_go::HIGHLIGHTS_QUERY,
            Self::Html | Self::Blade => tree_sitter_html::HIGHLIGHTS_QUERY,
            Self::Css => tree_sitter_css::HIGHLIGHTS_QUERY,
            Self::Bash => BASH_HIGHLIGHTS,
            Self::Php => tree_sitter_php::HIGHLIGHTS_QUERY,
            Self::C => tree_sitter_c::HIGHLIGHT_QUERY,
            Self::Cpp => tree_sitter_cpp::HIGHLIGHT_QUERY,
            Self::CSharp => tree_sitter_c_sharp::HIGHLIGHTS_QUERY,
            Self::Java => tree_sitter_java::HIGHLIGHTS_QUERY,
            Self::Ruby => tree_sitter_ruby::HIGHLIGHTS_QUERY,
            Self::Lua => tree_sitter_lua::HIGHLIGHTS_QUERY,
            Self::Swift => tree_sitter_swift::HIGHLIGHTS_QUERY,
            Self::Sql => tree_sitter_sequel::HIGHLIGHTS_QUERY,
            Self::Make => tree_sitter_make::HIGHLIGHTS_QUERY,
            Self::Ini => tree_sitter_ini::HIGHLIGHTS_QUERY,
            Self::PowerShell => tree_sitter_powershell::HIGHLIGHTS_QUERY,
            Self::Dart => tree_sitter_dart::HIGHLIGHTS_QUERY,
            Self::R => tree_sitter_r::HIGHLIGHTS_QUERY,
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
            Self::C => Some(tree_sitter_c::TAGS_QUERY),
            Self::Cpp => Some(tree_sitter_cpp::TAGS_QUERY),
            Self::CSharp => Some(tree_sitter_c_sharp::TAGS_QUERY),
            Self::Java => Some(tree_sitter_java::TAGS_QUERY),
            Self::Ruby => Some(tree_sitter_ruby::TAGS_QUERY),
            Self::Lua => Some(tree_sitter_lua::TAGS_QUERY),
            Self::Swift => Some(tree_sitter_swift::TAGS_QUERY),
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
            "makefile" | "gnumakefile" => Some(Self::Make),
            "gemfile" | "rakefile" => Some(Self::Ruby),
            _ => None,
        };
        if by_name.is_some() {
            return by_name;
        }
        if name.ends_with(".blade.php") {
            return Some(Self::Blade);
        }
        let extension = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
        let by_extension = match extension {
            "rs" => Some(Self::Rust),
            "ts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            "js" | "mjs" | "cjs" | "jsx" => Some(Self::JavaScript),
            "json" | "jsonc" => Some(Self::Json),
            "toml" => Some(Self::Toml),
            "yaml" | "yml" => Some(Self::Yaml),
            "md" | "markdown" => Some(Self::Markdown),
            "py" | "pyw" => Some(Self::Python),
            "go" => Some(Self::Go),
            "html" | "htm" => Some(Self::Html),
            "css" => Some(Self::Css),
            "sh" | "bash" | "zsh" => Some(Self::Bash),
            "php" => Some(Self::Php),
            "c" | "h" => Some(Self::C),
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => Some(Self::Cpp),
            "cs" => Some(Self::CSharp),
            "java" => Some(Self::Java),
            "rb" | "rake" | "gemspec" => Some(Self::Ruby),
            "lua" => Some(Self::Lua),
            "swift" => Some(Self::Swift),
            "sql" => Some(Self::Sql),
            "mk" | "mak" => Some(Self::Make),
            "ini" | "cfg" | "conf" => Some(Self::Ini),
            "ps1" | "psm1" | "psd1" => Some(Self::PowerShell),
            "dart" => Some(Self::Dart),
            "r" => Some(Self::R),
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
            "ruby" => Some(Self::Ruby),
            "lua" => Some(Self::Lua),
            "pwsh" => Some(Self::PowerShell),
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

/// Whether char offset `pos` lies in plain text between tags (JSX text,
/// HTML text, or PHP's inline HTML) rather than in code. Such words have no
/// definition, so Cmd+click must not offer them as links.
pub fn is_markup_text(doc: &Document, tree: &Tree, language: SyntaxLanguage, pos: usize) -> bool {
    let kind = match language {
        SyntaxLanguage::Tsx | SyntaxLanguage::JavaScript => "jsx_text",
        SyntaxLanguage::Html | SyntaxLanguage::Php | SyntaxLanguage::Blade => "text",
        _ => return false,
    };
    let rope = doc.rope();
    let byte = rope.char_to_byte(pos.min(rope.len_chars()));
    tree.root_node()
        .descendant_for_byte_range(byte, byte)
        .is_some_and(|node| node.kind() == kind)
}

/// Extracts definitions from a completed tree. This never parses or blocks.
pub fn outline_symbols(
    doc: &Document,
    tree: &Tree,
    language: SyntaxLanguage,
) -> Vec<OutlineSymbol> {
    let Some(query) = cached_tags_query(language) else {
        return Vec::new();
    };
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let rope = doc.rope();
    let len = rope.len_bytes();
    let mut out = Vec::new();
    let mut matches = cursor.matches(query, tree.root_node(), RopeTextProvider(rope));
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
        // Tree may lag the rope by one edit; clamp instead of panicking.
        let start = rope.byte_to_char(name.start_byte().min(len));
        let end = rope.byte_to_char(name.end_byte().min(len));
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
const TYPESCRIPT_HIGHLIGHTS: &str = r#"
(comment) @comment
(type_identifier) @type
(predefined_type) @type
[
  (string)
  (template_string)
] @string
(number) @number
(property_identifier) @property
(call_expression function: (identifier) @function)
(call_expression function: (member_expression property: (property_identifier) @function))
(function_declaration name: (identifier) @function)
(method_definition name: (property_identifier) @function)
(function_expression name: (identifier) @function)
(arrow_function) @function
(identifier) @variable
[
  "async" "await" "break" "case" "catch" "class" "const" "continue"
  "debugger" "default" "delete" "do" "else" "export" "extends" "finally"
  "for" "from" "function" "if" "import" "in" "instanceof" "let" "new"
  "of" "return" "static" "switch" "throw" "try" "typeof" "var" "void"
  "while" "with" "yield" "as" "interface" "implements" "private"
  "protected" "public" "readonly" "type" "namespace" "declare"
] @keyword
[
  ";" "." "," "(" ")" "[" "]" "{" "}" ":"
] @punctuation
[
  "=" "+" "-" "*" "/" "%" "==" "===" "!=" "!==" "=>" ">" ">=" "<" "<="
  "&&" "||" "??" "!" "&" "|" "^" "~" "?"
] @operator
"#;

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
    let Some(query) = cached_highlight_query(language) else {
        return Vec::new();
    };
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(start_byte..end_byte);
    // Predicates read node text through Rope chunks; no full-document copy.
    let mut spans: Vec<(usize, usize, HighlightScope)> = Vec::new();
    let mut matches = cursor.matches(query, tree.root_node(), RopeTextProvider(rope));
    while let Some(m) = matches.next() {
        for capture in m.captures {
            let Some(scope) = HighlightScope::from_capture(names[capture.index as usize]) else {
                continue;
            };
            let start = capture.node.start_byte().max(start_byte);
            let end = capture.node.end_byte().min(end_byte);
            if start < end {
                spans.push((start, end, scope));
            }
        }
    }
    if language == SyntaxLanguage::Blade {
        // Blade syntax is scanned from the first requested line, so a
        // directive or comment opened further up the file is not seen.
        let text = rope.byte_slice(start_byte..end_byte).to_string();
        let overlay = crate::blade::scan(&text);
        spans = crate::blade::subtract(
            spans,
            &overlay
                .claimed
                .iter()
                .map(|&(s, e)| (s + start_byte, e + start_byte))
                .collect::<Vec<_>>(),
        );
        spans.extend(
            overlay
                .spans
                .into_iter()
                .map(|(s, e, scope)| (s + start_byte, e + start_byte, scope)),
        );
    }
    let mut runs = Vec::new();
    for (start, end, scope) in spans {
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
    runs.sort_by_key(|run| (run.line, run.start, run.end));
    runs
}

/// Feeds Tree-sitter query predicates straight from Rope chunks, so a query
/// never copies the whole document. Node ranges past the end of the rope
/// (the tree may be one edit behind while the worker reparses) are clipped;
/// affected runs are clipped to the viewport later, so this never reaches
/// the screen.
struct RopeTextProvider<'a>(&'a Rope);

impl<'a> TextProvider<&'a [u8]> for RopeTextProvider<'a> {
    type I = RopeByteChunks<'a>;

    fn text(&mut self, node: tree_sitter::Node) -> Self::I {
        let len = self.0.len_bytes();
        let end = node.end_byte().min(len);
        let start = node.start_byte().min(end);
        let (chunks, chunk_start, _, _) = self.0.chunks_at_byte(start);
        RopeByteChunks {
            chunks,
            chunk_start,
            start,
            end,
        }
    }
}

struct RopeByteChunks<'a> {
    chunks: ropey::iter::Chunks<'a>,
    chunk_start: usize,
    start: usize,
    end: usize,
}

impl<'a> Iterator for RopeByteChunks<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        if self.start >= self.end {
            return None;
        }
        let chunk = self.chunks.next()?.as_bytes();
        let chunk_start = self.chunk_start;
        let chunk_end = chunk_start + chunk.len();
        self.chunk_start = chunk_end;
        let from = self.start.saturating_sub(chunk_start);
        let to = self.end.min(chunk_end) - chunk_start;
        self.start = chunk_end;
        Some(&chunk[from..to])
    }
}

fn cached_tags_query(language: SyntaxLanguage) -> Option<&'static Query> {
    const N: usize = SyntaxLanguage::ALL.len();
    static QUERIES: [OnceLock<Option<Query>>; N] = [const { OnceLock::new() }; N];
    let slot = &QUERIES[language as usize];
    slot.get_or_init(|| {
        language
            .tags_query()
            .and_then(|source| Query::new(&language.grammar(), source).ok())
    })
    .as_ref()
}

fn cached_highlight_query(language: SyntaxLanguage) -> Option<&'static Query> {
    const N: usize = SyntaxLanguage::ALL.len();
    static QUERIES: [OnceLock<Option<Query>>; N] = [const { OnceLock::new() }; N];
    let slot = &QUERIES[language as usize];
    slot.get_or_init(|| language.compile_highlights().ok())
        .as_ref()
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
    /// Last rope handed to the worker (a shared-node clone). The next
    /// `submit_document` diffs against it so the worker can reparse
    /// incrementally; new trees then share unchanged subtrees with the old
    /// one instead of each being a full copy.
    last_submitted: Mutex<Option<(u64, Rope)>>,
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
            last_submitted: Mutex::new(None),
        }
    }

    /// Queues the document's current rope. Cloning a Rope is O(1): text is
    /// still read by Tree-sitter through rope chunks on the worker.
    ///
    /// After the first submit this sends an edit computed by diffing against
    /// the previous rope, so every edit path (typing, paste, undo, IME,
    /// reload) gets an incremental reparse without reporting its changes.
    pub fn submit_document(&self, doc: &Document) {
        let revision = doc.revision();
        let text = doc.rope().clone();
        let edit = self
            .last_submitted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(base, old)| {
                let change = crate::rope_diff::changed_bytes(old, &text).unwrap_or(
                    crate::rope_diff::ByteChange {
                        start: 0,
                        old_end: 0,
                        new_end: 0,
                    },
                );
                (
                    *base,
                    SyntaxEdit::from_byte_ranges(
                        old,
                        &text,
                        change.start,
                        change.old_end,
                        change.new_end,
                    ),
                )
            });
        match edit {
            Some((base, edit)) => self.submit_edit(base, revision, text, edit),
            None => self.submit(revision, text),
        }
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
        *self
            .last_submitted
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some((revision, request.text.clone()));
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

    /// Parses `text` as `language` and reports `is_markup_text` at the first
    /// char of each of `words`.
    fn markup_flags(language: SyntaxLanguage, text: &str, words: &[&str]) -> Vec<bool> {
        let service = SyntaxService::new(language.grammar());
        let doc = Document::from_text(text);
        service.submit_document(&doc);
        let snapshot = wait_for(&service);
        words
            .iter()
            .map(|word| {
                let byte = text
                    .find(word)
                    .unwrap_or_else(|| panic!("{word} not in text"));
                let pos = text[..byte].chars().count();
                is_markup_text(&doc, snapshot.tree(), language, pos)
            })
            .collect()
    }

    #[test]
    fn words_between_jsx_tags_are_markup_text_but_code_is_not() {
        let text =
            "const total = 1;\nconst view = <p className=\"cls\">Recorded admin {total}</p>;\n";
        assert_eq!(
            markup_flags(
                SyntaxLanguage::Tsx,
                text,
                &[
                    "Recorded",
                    "admin",
                    "className",
                    "total}",
                    "view",
                    "const view"
                ],
            ),
            [true, true, false, false, false, false],
        );
    }

    #[test]
    fn html_text_is_markup_and_plain_languages_never_are() {
        assert_eq!(
            markup_flags(
                SyntaxLanguage::Html,
                "<p class=\"a\">Hello world</p>",
                &["Hello", "class"]
            ),
            [true, false]
        );
        assert_eq!(
            markup_flags(
                SyntaxLanguage::Rust,
                "fn main() { let word = 1; }",
                &["word", "main"]
            ),
            [false, false]
        );
        assert_eq!(
            markup_flags(
                SyntaxLanguage::Php,
                "<?php $a = 1; ?>\n<p>Plain words</p>",
                &["Plain", "$a"]
            ),
            [true, false]
        );
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
    fn submit_document_diffs_and_reparses_incrementally() {
        let language: Language = tree_sitter_rust::LANGUAGE.into();
        let service = SyntaxService::new(language.clone());
        let mut doc = Document::from_text("fn a() { let x = 1; }\nfn b() {}\n");
        service.submit_document(&doc);
        let first = wait_for(&service);
        assert!(!first.is_incremental());

        // Edits reach the service without any change description, as from
        // undo or paste.
        for (at, text) in [(17, "42 + "), (0, "// 😀 note\n"), (5, "")] {
            let change = crate::change::ChangeSet::new(
                vec![crate::change::Change::new(
                    at,
                    at + usize::from(text.is_empty()),
                    text,
                )],
                doc.len_chars(),
            );
            doc.apply(&change);
            service.submit_document(&doc);
            let snapshot = wait_for(&service);
            assert!(snapshot.is_incremental(), "edit at {at} reparsed in full");
            let mut parser = Parser::new();
            parser.set_language(&language).unwrap();
            let full = parser.parse(doc.text(), None).unwrap();
            assert_eq!(
                snapshot.tree().root_node().to_sexp(),
                full.root_node().to_sexp(),
                "incremental tree differs from a full parse"
            );
        }
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
            (
                SyntaxLanguage::Php,
                "<?php function hello(string $name): string { return $name; }",
            ),
            (SyntaxLanguage::C, "int main(void) { return 0; }"),
            (
                SyntaxLanguage::Cpp,
                "class A { public: int f() { return 1; } };",
            ),
            (SyntaxLanguage::CSharp, "class A { int F() { return 1; } }"),
            (SyntaxLanguage::Java, "class A { int f() { return 1; } }"),
            (SyntaxLanguage::Ruby, "def f\n  1\nend"),
            (SyntaxLanguage::Lua, "local n = 1"),
            (SyntaxLanguage::Swift, "func f() -> Int { return 1 }"),
            (SyntaxLanguage::Sql, "SELECT id FROM users;"),
            (SyntaxLanguage::Make, "all:\n\techo hi\n"),
            (SyntaxLanguage::Ini, "[core]\nname = galaxy\n"),
            (SyntaxLanguage::PowerShell, "$n = 1"),
            (SyntaxLanguage::Dart, "int f() { return 1; }"),
            (SyntaxLanguage::R, "x <- 1"),
            (
                SyntaxLanguage::Blade,
                "<p>{{ $name }}</p>\n@if ($a)\n@endif",
            ),
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
    fn typescript_query_emits_editor_token_scopes() {
        let language = SyntaxLanguage::TypeScript;
        let source = "import Database from 'better-sqlite3';\nexport async function backup(db: Database.Database, destination: string): Promise<void> {\n  const count: number = 42;\n  return db.backup(destination);\n}\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let query = language.compile_highlights().unwrap();
        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
        let mut scopes = std::collections::HashSet::new();
        while let Some(m) = matches.next() {
            scopes.extend(
                m.captures.iter().filter_map(|capture| {
                    HighlightScope::from_capture(names[capture.index as usize])
                }),
            );
        }
        for expected in [
            HighlightScope::Keyword,
            HighlightScope::Function,
            HighlightScope::String,
            HighlightScope::Type,
            HighlightScope::Number,
            HighlightScope::Property,
        ] {
            assert!(
                scopes.contains(&expected),
                "missing TypeScript scope {expected:?}"
            );
        }
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
        assert_eq!(
            SyntaxLanguage::detect("app/Console/Kernel.php", "<?php"),
            Some(SyntaxLanguage::Php)
        );
        assert_eq!(
            SyntaxLanguage::detect("resources/views/welcome.blade.php", "<?php"),
            Some(SyntaxLanguage::Blade)
        );
        for (path, expected) in [
            ("main.c", SyntaxLanguage::C),
            ("main.cpp", SyntaxLanguage::Cpp),
            ("Program.cs", SyntaxLanguage::CSharp),
            ("Main.java", SyntaxLanguage::Java),
            ("app.rb", SyntaxLanguage::Ruby),
            ("Gemfile", SyntaxLanguage::Ruby),
            ("init.lua", SyntaxLanguage::Lua),
            ("App.swift", SyntaxLanguage::Swift),
            ("query.sql", SyntaxLanguage::Sql),
            ("Makefile", SyntaxLanguage::Make),
            ("setup.ini", SyntaxLanguage::Ini),
            ("run.ps1", SyntaxLanguage::PowerShell),
            ("main.dart", SyntaxLanguage::Dart),
            ("plot.R", SyntaxLanguage::R),
        ] {
            assert_eq!(SyntaxLanguage::detect(path, ""), Some(expected), "{path}");
        }
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
    fn every_tags_query_compiles_and_outlines_its_sample() {
        let samples = [
            (SyntaxLanguage::Rust, "fn alpha() {}\n", "alpha"),
            (
                SyntaxLanguage::C,
                "int alpha(void) { return 0; }\n",
                "alpha",
            ),
            (
                SyntaxLanguage::Cpp,
                "class Alpha { public: int f() { return 1; } };\n",
                "Alpha",
            ),
            (
                SyntaxLanguage::CSharp,
                "class Alpha { int F() { return 1; } }\n",
                "Alpha",
            ),
            (
                SyntaxLanguage::Java,
                "class Alpha { int f() { return 1; } }\n",
                "Alpha",
            ),
            (SyntaxLanguage::Ruby, "def alpha\n  1\nend\n", "alpha"),
            (SyntaxLanguage::Lua, "function alpha() end\n", "alpha"),
            (
                SyntaxLanguage::Swift,
                "func alpha() -> Int { return 1 }\n",
                "alpha",
            ),
        ];
        for (language, source, expected) in samples {
            let tags = language.tags_query().expect("tags query");
            Query::new(&language.grammar(), tags).unwrap_or_else(|error| {
                panic!("{} tags query did not compile: {error}", language.name())
            });
            let doc = Document::from_text(source);
            let mut parser = Parser::new();
            parser.set_language(&language.grammar()).unwrap();
            let tree = parser.parse(source, None).unwrap();
            let symbols = outline_symbols(&doc, &tree, language);
            assert!(
                symbols.iter().any(|symbol| symbol.name == expected),
                "{} outline missing {expected}: {symbols:?}",
                language.name()
            );
        }
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

    #[test]
    fn php_styles_include_keywords_variables_and_strings() {
        let doc = Document::from_text(
            "<?php\nclass User { public function name() { return \"Ada\"; } }\n",
        );
        let language = SyntaxLanguage::Php;
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(doc.text(), None).unwrap();
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 2);
        assert!(runs.iter().any(|run| run.scope == HighlightScope::Keyword));
        assert!(runs.iter().any(|run| run.scope == HighlightScope::String));
    }

    #[test]
    fn blade_views_get_html_and_blade_styles() {
        let src = "<title>{{ $title ?? 'Good' }}</title>\n<link href=\"{{ asset('a.css') }}\">\n@yield('content')\n";
        let doc = Document::from_text(src);
        let language = SyntaxLanguage::detect("resources/views/auth.blade.php", "").unwrap();
        assert_eq!(language, SyntaxLanguage::Blade);
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(doc.text(), None).unwrap();
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 3);
        let at = |line: usize, text: &str, scope: HighlightScope| {
            let line_text = crate::viewport::display_text(&doc, line);
            let start = line_text[..line_text.find(text).unwrap()]
                .encode_utf16()
                .count();
            let end = start + text.encode_utf16().count();
            runs.iter()
                .any(|r| r.line == line && r.start == start && r.end == end && r.scope == scope)
        };
        // HTML still works around the Blade syntax.
        assert!(at(0, "title", HighlightScope::Tag));
        assert!(at(1, "href", HighlightScope::Attribute));
        // Blade echoes and directives.
        assert!(at(0, "$title", HighlightScope::Variable));
        assert!(at(0, "'Good'", HighlightScope::String));
        assert!(at(1, "asset", HighlightScope::Function));
        assert!(at(2, "@yield", HighlightScope::Keyword));
        assert!(at(2, "'content'", HighlightScope::String));
        // The HTML attribute value is not painted over the Blade expression.
        assert!(
            !runs.iter().any(|r| r.line == 1
                && r.scope == HighlightScope::String
                && r.start <= 12
                && r.end >= 30),
            "{runs:?}"
        );
        for run in &runs {
            let text = crate::viewport::display_text(&doc, run.line);
            assert!(run.start < run.end && run.end <= text.encode_utf16().count());
        }
    }

    #[test]
    fn stale_tree_after_deletion_styles_shorter_text_without_panicking() {
        // The UI draws with the previous tree while the worker reparses.
        let before = Document::from_text("<?php\n$name = \"Ada\";\n$age = 36;\n");
        let language = SyntaxLanguage::Php;
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(before.text(), None).unwrap();
        let after = Document::from_text("<?php\n$n");
        let runs = style_runs_for_viewport(&after, &tree, language, 0, 10);
        for run in runs {
            let text = crate::viewport::display_text(&after, run.line);
            assert!(run.line < after.line_count());
            assert!(run.end <= text.encode_utf16().count());
        }
    }
}
