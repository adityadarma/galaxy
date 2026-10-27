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
    Nginx,
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
    /// `.blade.php`: HTML grammar plus the [`crate::php::framework::laravel::blade`] overlay.
    Blade,
    /// `.vue`: HTML grammar plus `<script>`/`<style>` injection.
    Vue,
    /// `.svelte`: HTML grammar plus `<script>`/`<style>` and template expression injection.
    Svelte,
}

impl SyntaxLanguage {
    pub const ALL: [Self; 31] = [
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
        Self::Nginx,
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
        Self::Vue,
        Self::Svelte,
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
            Self::Nginx => "Nginx",
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
            Self::Vue => "Vue",
            Self::Svelte => "Svelte",
        }
    }

    /// Preferred line-wise comment marker. Block-only languages intentionally
    /// return none: inserting only an opening marker would create invalid code.
    pub fn comment_marker(self) -> Option<&'static str> {
        match self {
            Self::Rust | Self::TypeScript | Self::Tsx | Self::JavaScript | Self::Go => Some("//"),
            Self::Toml | Self::Yaml | Self::Python | Self::Bash | Self::Nginx => Some("#"),
            Self::Ruby | Self::Make | Self::PowerShell | Self::R => Some("#"),
            Self::C | Self::Cpp | Self::CSharp | Self::Java | Self::Swift | Self::Dart => {
                Some("//")
            }
            Self::Lua | Self::Sql => Some("--"),
            Self::Ini => Some(";"),
            Self::Json
            | Self::Html
            | Self::Css
            | Self::Markdown
            | Self::Blade
            | Self::Vue
            | Self::Svelte => None,
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
            Self::Html | Self::Blade | Self::Vue | Self::Svelte => {
                tree_sitter_html::LANGUAGE.into()
            }
            Self::Css => tree_sitter_css::LANGUAGE.into(),
            Self::Bash => tree_sitter_bash::LANGUAGE.into(),
            Self::Nginx => tree_sitter_nginx::LANGUAGE.into(),
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
            Self::Rust => rust_highlights(),
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
            Self::Html | Self::Blade | Self::Vue | Self::Svelte => {
                tree_sitter_html::HIGHLIGHTS_QUERY
            }
            Self::Css => tree_sitter_css::HIGHLIGHTS_QUERY,
            Self::Bash => BASH_HIGHLIGHTS,
            Self::Nginx => NGINX_HIGHLIGHTS,
            Self::Php => php_highlights(),
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
            "dockerfile" => Some(Self::Bash),
            "www.conf" => Some(Self::Ini),
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
        if name.starts_with("dockerfile.") {
            return Some(Self::Bash);
        }
        if name == "nginx.conf" {
            return Some(Self::Nginx);
        }
        let extension = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
        let by_extension = match extension {
            "rs" => Some(Self::Rust),
            "ts" | "mts" | "cts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            "js" | "mjs" | "cjs" | "jsx" => Some(Self::JavaScript),
            "json" | "jsonc" => Some(Self::Json),
            "toml" => Some(Self::Toml),
            "yaml" | "yml" => Some(Self::Yaml),
            "md" | "markdown" => Some(Self::Markdown),
            "py" | "pyw" => Some(Self::Python),
            "go" => Some(Self::Go),
            "html" | "htm" => Some(Self::Html),
            "vue" => Some(Self::Vue),
            "svelte" => Some(Self::Svelte),
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

/// Whether char offset `pos` lies in markup rather than code: text between
/// tags, HTML tag and attribute names, and JSX/TSX intrinsic tags (`<div>`)
/// and attribute names. Such words have no definition, so Cmd+click must not
/// offer them as links. Components (`<Button>`, Vue `<my-card>`) stay code,
/// as do `{{ }}` interpolations in Vue.
pub fn is_markup_text(doc: &Document, tree: &Tree, language: SyntaxLanguage, pos: usize) -> bool {
    let html = matches!(
        language,
        SyntaxLanguage::Html
            | SyntaxLanguage::Php
            | SyntaxLanguage::Blade
            | SyntaxLanguage::Vue
            | SyntaxLanguage::Svelte
    );
    let jsx = matches!(language, SyntaxLanguage::Tsx | SyntaxLanguage::JavaScript);
    if !html && !jsx {
        return false;
    }
    let rope = doc.rope();
    let byte = rope.char_to_byte(pos.min(rope.len_chars()));
    // A one-byte range skips the node that merely ends where this one starts
    // (the `<` before a tag name).
    let Some(node) = tree
        .root_node()
        .descendant_for_byte_range(byte, (byte + 1).min(rope.len_bytes()))
    else {
        return false;
    };
    let text = |n: tree_sitter::Node| text_between(rope, n.start_byte(), n.end_byte());
    let is_component = |name: &str| {
        name.chars().next().is_some_and(char::is_uppercase)
            || (language == SyntaxLanguage::Vue && name.contains('-'))
    };
    match node.kind() {
        "jsx_text" => jsx,
        "text" if html => language != SyntaxLanguage::Vue || !in_interpolation(rope, node, byte),
        "tag_name" | "erroneous_end_tag_name" if html => {
            !(language == SyntaxLanguage::Vue && text(node).is_some_and(|n| is_component(&n)))
        }
        "attribute_name" if html => true,
        "identifier" if jsx => {
            node.parent().is_some_and(|p| {
                matches!(
                    p.kind(),
                    "jsx_opening_element" | "jsx_closing_element" | "jsx_self_closing_element"
                )
            }) && text(node).is_some_and(|n| !is_component(&n))
        }
        "property_identifier" if jsx => node.parent().is_some_and(|p| p.kind() == "jsx_attribute"),
        _ => false,
    }
}

/// Whether `byte` of the Vue `text` node sits between `{{` and `}}`.
fn in_interpolation(rope: &Rope, node: tree_sitter::Node, byte: usize) -> bool {
    let Some(text) = text_between(rope, node.start_byte(), node.end_byte()) else {
        return false;
    };
    let at = byte.saturating_sub(node.start_byte()).min(text.len());
    let before = text.get(..at).unwrap_or("");
    let after = text.get(at..).unwrap_or("");
    match (before.rfind("{{"), before.rfind("}}")) {
        (Some(open), close) if close.is_none_or(|c| c < open) => after.contains("}}"),
        _ => false,
    }
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

/// The grammar's PHP query, with `static::`, `self::` and `parent::` styled
/// as keywords rather than variables, and the class names it leaves out:
/// declarations, `extends`/`implements`, trait uses, attributes, `X::class`
/// and `X::$property`.
fn php_highlights() -> &'static str {
    static QUERY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    QUERY.get_or_init(|| {
        let base = tree_sitter_php::HIGHLIGHTS_QUERY.replace(
            "(relative_scope) @variable.builtin",
            "(relative_scope) @keyword",
        );
        format!(
            r#"{base}
(class_declaration name: (name) @type)
(interface_declaration name: (name) @type)
(trait_declaration name: (name) @type)
(enum_declaration name: (name) @type)
(base_clause [(name) (qualified_name (name))] @type)
(class_interface_clause [(name) (qualified_name (name))] @type)
(use_declaration [(name) (qualified_name (name))] @type)
(attribute [(name) (qualified_name (name))] @type)
(class_constant_access_expression . [(name) (qualified_name (name))] @type)
(class_constant_access_expression (name) @keyword (#eq? @keyword "class"))
(scoped_property_access_expression scope: [(name) (qualified_name (name))] @type)
((variable_name (name) @_this) @variable.builtin (#eq? @_this "this"))
(function_call_expression function: (name) @function.builtin (#any-of? @function.builtin "empty" "isset" "unset" "eval" "die" "exit" "compact" "extract"))
"#
        )
    })
}

/// The upstream Rust query styles attribute names but leaves `#[...]`
/// delimiters unscoped, causing `#` to fall back to the plain foreground.
fn rust_highlights() -> &'static str {
    static QUERY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    QUERY.get_or_init(|| {
        format!(
            "(identifier) @variable\n{}\n[\"#\" \"[\" \"]\"] @punctuation\n(scoped_identifier path: (identifier) @type name: (identifier) @constant)\n",
            tree_sitter_rust::HIGHLIGHTS_QUERY
        )
    })
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
(private_property_identifier) @property
(call_expression function: (identifier) @function)
(call_expression function: (member_expression property: (property_identifier) @function))
(function_declaration name: (identifier) @function)
(method_definition name: (property_identifier) @function)
(function_expression name: (identifier) @function)
(arrow_function) @function
[(this) (this_type)] @variable.builtin
(undefined) @constant
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
(private_property_identifier) @property
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

const NGINX_HIGHLIGHTS: &str = r#"
(comment) @comment
(value) @variable
(attribute (keyword) @attribute)
[(location_modifier) "="] @operator
[(keyword) "location"] @keyword
["if" "map"] @keyword.conditional
(directive (keyword) @constant)
(boolean) @boolean
[(auto) (constant) (level) (connection_method) (var) condition: (condition)] @variable.builtin
[(string_literal) (quoted_string_literal) (file) (mask)] @string
(directive (variable) @variable.parameter)
(directive (variable (keyword) @variable.parameter))
(location_route) @string.special
";" @punctuation.delimiter
[(numeric_literal) (time) (size) (cpumask)] @number
["{" "}"] @punctuation.bracket
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
    /// Language built-ins (`function.builtin` captures); themes may color
    /// them apart from user functions and fall back to `function`.
    FunctionBuiltin,
    Keyword,
    Label,
    Number,
    Operator,
    Property,
    Punctuation,
    String,
    Tag,
    Type,
    /// `$this`, `self`-like receivers (`variable.builtin` captures); themes
    /// may color them apart from ordinary variables.
    VariableBuiltin,
    Variable,
}

impl HighlightScope {
    /// Maps a capture name, including modifiers like `function.builtin`, to
    /// the stable base scope consumed by themes and viewport style runs.
    pub fn from_capture(capture: &str) -> Option<Self> {
        match capture {
            "function.builtin" => return Some(Self::FunctionBuiltin),
            "variable.builtin" => return Some(Self::VariableBuiltin),
            _ => {}
        }
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
            Self::FunctionBuiltin => "function.builtin",
            Self::Keyword => "keyword",
            Self::Label => "label",
            Self::Number => "number",
            Self::Operator => "operator",
            Self::Property => "property",
            Self::Punctuation => "punctuation",
            Self::String => "string",
            Self::Tag => "tag",
            Self::Type => "type",
            Self::VariableBuiltin => "variable.builtin",
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
        let overlay = crate::php::framework::laravel::blade::scan(&text);
        spans = crate::php::framework::laravel::blade::subtract(
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
    if language == SyntaxLanguage::Vue {
        let overlay = vue_overlay(rope, tree, start_byte, end_byte);
        spans = crate::php::framework::laravel::blade::subtract(spans, &overlay.claimed);
        spans.extend(overlay.spans);
    }
    if language == SyntaxLanguage::Svelte {
        let overlay = svelte_overlay(rope, tree, start_byte, end_byte);
        spans = crate::php::framework::laravel::blade::subtract(spans, &overlay.claimed);
        spans.extend(overlay.spans);
    }
    // Tree-sitter does not guarantee that a broad fallback capture precedes a
    // semantic capture of the same token. Apply the generic variable color
    // first so the renderer lets the more specific scope win.
    spans.sort_by_key(|(start, end, scope)| (*start, *end, highlight_priority(*scope)));
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

fn highlight_priority(scope: HighlightScope) -> u8 {
    match scope {
        HighlightScope::Variable => 0,
        _ => 1,
    }
}

/// Cached parses of injected code (script/style bodies and template
/// expressions), keyed by language and text, so scrolling re-parses nothing
/// and typing re-parses only what changed.
const INJECTION_CACHE_SLOTS: usize = 64;

thread_local! {
    static INJECTION_CACHE: std::cell::RefCell<Vec<(SyntaxLanguage, String, Tree)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn cached_parse(language: SyntaxLanguage, text: &str) -> Option<Tree> {
    INJECTION_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(i) = cache
            .iter()
            .position(|(l, t, _)| *l == language && t == text)
        {
            let entry = cache.remove(i);
            let tree = entry.2.clone();
            cache.push(entry);
            return Some(tree);
        }
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).ok()?;
        let parsed = parser.parse(text, None)?;
        if cache.len() >= INJECTION_CACHE_SLOTS {
            cache.remove(0);
        }
        cache.push((language, text.to_owned(), parsed.clone()));
        Some(parsed)
    })
}

/// Language of a Vue `<script>` or `<style>` start tag from its `lang`
/// attribute. Unknown values fall back to nothing (left unstyled).
fn vue_block_language(element: &str, start_tag: &str) -> Option<SyntaxLanguage> {
    let lang = ["lang=\"", "lang='"].iter().find_map(|prefix| {
        let at = start_tag.find(prefix)? + prefix.len();
        start_tag[at..].split(['"', '\'']).next()
    });
    match (element, lang) {
        ("script_element", Some("ts")) => Some(SyntaxLanguage::TypeScript),
        ("script_element", Some("tsx")) => Some(SyntaxLanguage::Tsx),
        ("script_element", Some("js" | "jsx") | None) => Some(SyntaxLanguage::JavaScript),
        ("style_element", Some("css") | None) => Some(SyntaxLanguage::Css),
        _ => None,
    }
}

/// Text of `start..end`, or `None` when a (possibly stale) tree offset is out
/// of range or splits a character.
fn text_between(rope: &Rope, start: usize, end: usize) -> Option<String> {
    let len = rope.len_bytes();
    let (start, end) = (start.min(len), end.min(len));
    if start > end {
        return None;
    }
    let on_boundary = |b: usize| rope.char_to_byte(rope.byte_to_char(b)) == b;
    (on_boundary(start) && on_boundary(end)).then(|| rope.byte_slice(start..end).to_string())
}

/// Styles for the embedded languages of a Vue single-file component:
/// `<script>`/`<style>` bodies, `{{ }}` interpolations and the values of
/// `v-*`, `:prop` and `@event` attributes. `claimed` lists the attribute
/// values whose flat HTML string color must be replaced. Only the part of the
/// document in `start_byte..end_byte` is styled.
fn vue_overlay(
    rope: &Rope,
    tree: &Tree,
    start_byte: usize,
    end_byte: usize,
) -> crate::php::framework::laravel::blade::Overlay {
    let mut scan = VueScan {
        rope,
        window: (start_byte, end_byte),
        overlay: crate::php::framework::laravel::blade::Overlay::default(),
    };
    scan.walk(tree.root_node());
    let mut overlay = scan.overlay;
    overlay.spans.retain_mut(|span| {
        span.0 = span.0.max(start_byte);
        span.1 = span.1.min(end_byte);
        span.0 < span.1
    });
    overlay
}

/// Styles the embedded languages and braced template expressions of a Svelte
/// component using the bundled HTML, JavaScript/TypeScript, and CSS grammars.
fn svelte_overlay(
    rope: &Rope,
    tree: &Tree,
    start_byte: usize,
    end_byte: usize,
) -> crate::php::framework::laravel::blade::Overlay {
    let mut scan = SvelteScan {
        rope,
        window: (start_byte, end_byte),
        overlay: crate::php::framework::laravel::blade::Overlay::default(),
        blocks: Vec::new(),
    };
    scan.walk(tree.root_node());
    scan.braced_expressions();
    let mut overlay = scan.overlay;
    overlay.spans.retain_mut(|span| {
        span.0 = span.0.max(start_byte);
        span.1 = span.1.min(end_byte);
        span.0 < span.1
    });
    overlay
}

struct VueScan<'a> {
    rope: &'a Rope,
    window: (usize, usize),
    overlay: crate::php::framework::laravel::blade::Overlay,
}

impl VueScan<'_> {
    fn walk(&mut self, node: tree_sitter::Node) {
        if node.end_byte() <= self.window.0 || node.start_byte() >= self.window.1 {
            return;
        }
        match node.kind() {
            "script_element" | "style_element" => return self.block(node),
            "attribute" => return self.attribute(node),
            "text" => return self.interpolations(node),
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.walk(child);
        }
    }

    fn block(&mut self, element: tree_sitter::Node) {
        let mut cursor = element.walk();
        let mut tag = None;
        let mut raw = None;
        for child in element.children(&mut cursor) {
            match child.kind() {
                "start_tag" => tag = Some(child),
                "raw_text" => raw = Some(child),
                _ => {}
            }
        }
        let (Some(tag), Some(raw)) = (tag, raw) else {
            return;
        };
        let Some(tag_text) = text_between(self.rope, tag.start_byte(), tag.end_byte()) else {
            return;
        };
        let Some(language) = vue_block_language(element.kind(), &tag_text) else {
            return;
        };
        let (from, to) = (raw.start_byte(), raw.end_byte());
        if from >= to || to <= self.window.0 || from >= self.window.1 {
            return;
        }
        let Some(text) = text_between(self.rope, from, to) else {
            return;
        };
        let Some(sub) = cached_parse(language, &text) else {
            return;
        };
        self.captures(language, &text, &sub, from, (from, to));
    }

    fn attribute(&mut self, attribute: tree_sitter::Node) {
        let mut cursor = attribute.walk();
        let mut name = None;
        let mut value = None;
        for child in attribute.children(&mut cursor) {
            match child.kind() {
                "attribute_name" => name = Some(child),
                "attribute_value" => value = Some(child),
                "quoted_attribute_value" => {
                    let mut inner = child.walk();
                    value = child
                        .children(&mut inner)
                        .find(|n| n.kind() == "attribute_value");
                }
                _ => {}
            }
        }
        let (Some(name), Some(value)) = (name, value) else {
            return;
        };
        let Some(name) = text_between(self.rope, name.start_byte(), name.end_byte()) else {
            return;
        };
        if !(name.starts_with("v-") || name.starts_with(':') || name.starts_with('@')) {
            return;
        }
        let (from, to) = (value.start_byte(), value.end_byte());
        if from >= to {
            return;
        }
        self.overlay.claimed.push((from, to));
        self.expression(from, to);
    }

    fn interpolations(&mut self, text_node: tree_sitter::Node) {
        let base = text_node.start_byte();
        let Some(text) = text_between(self.rope, base, text_node.end_byte()) else {
            return;
        };
        let mut at = 0;
        while let Some(open) = text[at..].find("{{").map(|i| i + at) {
            let Some(close) = find_balanced_close(&text, open + 2, "}}") else {
                break;
            };
            let scope = HighlightScope::Punctuation;
            self.overlay
                .spans
                .push((base + open, base + open + 2, scope));
            self.overlay
                .spans
                .push((base + close, base + close + 2, scope));
            self.expression(base + open + 2, base + close);
            at = close + 2;
        }
    }

    /// Styles `from..to` as one JavaScript expression. It is parsed inside
    /// parentheses so `{ a: b }` is an object and not a block; when that does
    /// not parse (`a = 1; b = 2`) it is parsed as plain statements.
    fn expression(&mut self, from: usize, to: usize) {
        if to <= self.window.0 || from >= self.window.1 {
            return;
        }
        let Some(expr) = text_between(self.rope, from, to) else {
            return;
        };
        let language = SyntaxLanguage::JavaScript;
        let wrapped = format!("({expr})");
        let (text, lead, sub) = match cached_parse(language, &wrapped) {
            Some(tree) if !tree.root_node().has_error() => (wrapped, 1, tree),
            _ => match cached_parse(language, &expr) {
                Some(tree) => (expr, 0, tree),
                None => return,
            },
        };
        self.captures(language, &text, &sub, from - lead, (from, to));
    }

    /// Adds the highlight captures of `tree` (parsed from `text`, which sits
    /// at byte `offset` of the document) that fall inside `keep`.
    fn captures(
        &mut self,
        language: SyntaxLanguage,
        text: &str,
        tree: &Tree,
        offset: usize,
        keep: (usize, usize),
    ) {
        let Some(query) = cached_highlight_query(language) else {
            return;
        };
        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(
            self.window.0.saturating_sub(offset)
                ..self.window.1.saturating_sub(offset).min(text.len()),
        );
        let mut matches = cursor.matches(query, tree.root_node(), text.as_bytes());
        while let Some(m) = matches.next() {
            for capture in m.captures {
                let Some(scope) = HighlightScope::from_capture(names[capture.index as usize])
                else {
                    continue;
                };
                let from = (capture.node.start_byte() + offset).max(keep.0);
                let to = (capture.node.end_byte() + offset).min(keep.1);
                if from < to {
                    self.overlay.spans.push((from, to, scope));
                }
            }
        }
    }
}

struct SvelteScan<'a> {
    rope: &'a Rope,
    window: (usize, usize),
    overlay: crate::php::framework::laravel::blade::Overlay,
    blocks: Vec<(usize, usize)>,
}

impl SvelteScan<'_> {
    fn walk(&mut self, node: tree_sitter::Node) {
        if node.end_byte() <= self.window.0 || node.start_byte() >= self.window.1 {
            return;
        }
        match node.kind() {
            "script_element" | "style_element" => return self.block(node),
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.walk(child);
        }
    }

    fn block(&mut self, element: tree_sitter::Node) {
        let mut cursor = element.walk();
        let mut tag = None;
        let mut raw = None;
        for child in element.children(&mut cursor) {
            match child.kind() {
                "start_tag" => tag = Some(child),
                "raw_text" => raw = Some(child),
                _ => {}
            }
        }
        let (Some(tag), Some(raw)) = (tag, raw) else {
            return;
        };
        let Some(tag_text) = text_between(self.rope, tag.start_byte(), tag.end_byte()) else {
            return;
        };
        let Some(language) = vue_block_language(element.kind(), &tag_text) else {
            return;
        };
        self.blocks.push((raw.start_byte(), raw.end_byte()));
        self.inject(language, raw.start_byte(), raw.end_byte());
    }

    fn inject(&mut self, language: SyntaxLanguage, from: usize, to: usize) {
        if from >= to || to <= self.window.0 || from >= self.window.1 {
            return;
        }
        let Some(text) = text_between(self.rope, from, to) else {
            return;
        };
        let Some(tree) = cached_parse(language, &text) else {
            return;
        };
        self.captures(language, &text, &tree, from, (from, to));
    }

    fn braced_expressions(&mut self) {
        let Some(text) = text_between(self.rope, self.window.0, self.window.1) else {
            return;
        };
        let mut at = 0;
        while let Some(open) = text[at..].find('{').map(|i| i + at) {
            let Some(close) = find_balanced_close(&text, open + 1, "}") else {
                break;
            };
            let from = self.window.0 + open;
            let to = self.window.0 + close + 1;
            if self
                .blocks
                .iter()
                .any(|&(start, end)| from >= start && from < end)
            {
                at = close + 1;
                continue;
            }
            self.overlay.claimed.push((from, to));
            self.overlay
                .spans
                .push((from, from + 1, HighlightScope::Punctuation));
            self.overlay
                .spans
                .push((to - 1, to, HighlightScope::Punctuation));
            self.expression(from + 1, to - 1);
            at = close + 1;
        }
    }

    fn expression(&mut self, from: usize, to: usize) {
        let Some(expr) = text_between(self.rope, from, to) else {
            return;
        };
        let expression = svelte_expression(&expr);
        if expression.trim().is_empty() {
            return;
        }
        let wrapped = format!("({expression})");
        let Some(tree) = cached_parse(SyntaxLanguage::JavaScript, &wrapped) else {
            return;
        };
        self.captures(
            SyntaxLanguage::JavaScript,
            &wrapped,
            &tree,
            from.saturating_sub(1),
            (from, to),
        );
    }

    fn captures(
        &mut self,
        language: SyntaxLanguage,
        text: &str,
        tree: &Tree,
        offset: usize,
        keep: (usize, usize),
    ) {
        let Some(query) = cached_highlight_query(language) else {
            return;
        };
        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), text.as_bytes());
        while let Some(m) = matches.next() {
            for capture in m.captures {
                let Some(scope) = HighlightScope::from_capture(names[capture.index as usize])
                else {
                    continue;
                };
                let from = (capture.node.start_byte() + offset).max(keep.0);
                let to = (capture.node.end_byte() + offset).min(keep.1);
                if from < to {
                    self.overlay.spans.push((from, to, scope));
                }
            }
        }
    }
}

/// Returns the JS expression within a Svelte braced construct. Prefixes are
/// markup syntax; the remainder is parsed by the JavaScript grammar.
fn svelte_expression(text: &str) -> &str {
    let text = text.trim_start();
    if let Some(rest) = text.strip_prefix("#each") {
        return rest.trim_start().split_once(" as ").map_or_else(
            || rest.trim_start(),
            |(expression, _)| expression.trim_end(),
        );
    }
    for prefix in [
        "#if", ":else if", "#await", ":then", ":catch", "@html", "@const",
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            return rest.trim_start();
        }
    }
    text
}

/// Finds a delimiter outside JavaScript strings, comments, template literals,
/// and nested braces. Unterminated lexical sections return `None` safely.
fn find_balanced_close(text: &str, start: usize, close: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = start;
    let mut depth = 0usize;
    let mut quote = None;
    let mut line_comment = false;
    let mut block_comment = false;
    while i < bytes.len() {
        if line_comment {
            if bytes[i] == b'\n' {
                line_comment = false;
            }
            i += 1;
            continue;
        }
        if block_comment {
            if bytes.get(i..i + 2) == Some(b"*/") {
                block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if let Some(q) = quote {
            if bytes[i] == b'\\' {
                i += 2;
                continue;
            }
            if bytes[i] == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if bytes.get(i..i + 2) == Some(b"//") {
            line_comment = true;
            i += 2;
            continue;
        }
        if bytes.get(i..i + 2) == Some(b"/*") {
            block_comment = true;
            i += 2;
            continue;
        }
        if matches!(bytes[i], b'\'' | b'\"' | b'`') {
            quote = Some(bytes[i]);
            i += 1;
            continue;
        }
        if bytes[i] == b'{' {
            depth += 1;
            i += 1;
            continue;
        }
        if bytes[i] == b'}' {
            if depth == 0 && text[i..].starts_with(close) {
                return Some(i);
            }
            depth = depth.saturating_sub(1);
        }
        i += 1;
    }
    None
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
            [true, true, true, false, false, false],
        );
    }

    #[test]
    fn tag_and_attribute_names_are_markup_but_components_and_expressions_are_not() {
        assert_eq!(
            markup_flags(
                SyntaxLanguage::Html,
                "<section class=\"a\"><p>x</p></section>",
                &["section", "class", "p>x", "/section"],
            ),
            [true, true, true, false],
        );
        let jsx = "const v = <div className=\"a\"><Button label=\"b\" /></div>;";
        assert_eq!(
            markup_flags(
                SyntaxLanguage::Tsx,
                jsx,
                &["div className", "className", "Button", "label", "v ="],
            ),
            [true, true, false, true, false],
        );
        let vue = "<template><my-card :id=\"id\">{{ total }} word</my-card><Item /></template>";
        assert_eq!(
            markup_flags(
                SyntaxLanguage::Vue,
                vue,
                &["template", "my-card", ":id", "total", "word", "Item"],
            ),
            [true, false, true, false, true, false],
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
            [true, true]
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
            (
                SyntaxLanguage::Vue,
                "<template><p>{{ a }}</p></template>\n<script setup lang=\"ts\">\nconst a = 1\n</script>\n",
            ),
            (
                SyntaxLanguage::Svelte,
                "<script>let a = 1;</script>\n<p>{a}</p>\n<style>p { color: red; }</style>\n",
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
            Some(HighlightScope::FunctionBuiltin)
        );
        assert_eq!(
            HighlightScope::from_capture("function.method"),
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
        let source = "// note\nenum ClassKind { Interface }\nfn main() { let name = \"Galaxy\"; let _ = ClassKind::Interface; let value = name; }";
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
        assert!(scopes.contains(&HighlightScope::Type));
        assert!(scopes.contains(&HighlightScope::Constant));
        assert!(scopes.contains(&HighlightScope::Variable));

        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 3);
        assert!(runs.iter().any(|run| run.scope == HighlightScope::Type));
        assert!(runs.iter().any(|run| run.scope == HighlightScope::Constant));
    }

    #[test]
    fn typescript_query_emits_editor_token_scopes() {
        let language = SyntaxLanguage::TypeScript;
        let source = "import Database from 'better-sqlite3';\nexport async function backup(db: Database.Database, destination: string): Promise<void> {\n  const count: number | undefined = 42;\n  return db.backup(destination);\n}\n";
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
            HighlightScope::Constant,
        ] {
            assert!(
                scopes.contains(&expected),
                "missing TypeScript scope {expected:?}"
            );
        }
    }

    #[test]
    fn typescript_query_colours_private_class_fields_as_properties() {
        let language = SyntaxLanguage::TypeScript;
        let source = "class Backup {\n  #path = '';\n  read() { return this.#path; }\n}\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let query = language.compile_highlights().unwrap();
        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
        let mut private_fields = 0;
        while let Some(m) = matches.next() {
            for capture in m.captures {
                if &source[capture.node.byte_range()] == "#path"
                    && HighlightScope::from_capture(names[capture.index as usize])
                        == Some(HighlightScope::Property)
                {
                    private_fields += 1;
                }
            }
        }

        assert_eq!(private_fields, 2);
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
        assert_eq!(
            SyntaxLanguage::detect("src/App.vue", ""),
            Some(SyntaxLanguage::Vue)
        );
        assert_eq!(
            SyntaxLanguage::detect("src/App.svelte", ""),
            Some(SyntaxLanguage::Svelte)
        );
        assert_eq!(
            SyntaxLanguage::detect("node_modules/vue/dist/vue.d.mts", ""),
            Some(SyntaxLanguage::TypeScript)
        );
        assert_eq!(
            SyntaxLanguage::detect("Dockerfile", ""),
            Some(SyntaxLanguage::Bash)
        );
        assert_eq!(
            SyntaxLanguage::detect("Dockerfile.production", ""),
            Some(SyntaxLanguage::Bash)
        );
        assert_eq!(
            SyntaxLanguage::detect(".docker/nginx-logrotate.conf", ""),
            Some(SyntaxLanguage::Ini)
        );
        assert_eq!(
            SyntaxLanguage::detect(".docker/nginx.conf", ""),
            Some(SyntaxLanguage::Nginx)
        );
        assert_eq!(
            SyntaxLanguage::detect("php-fpm/www.conf", ""),
            Some(SyntaxLanguage::Ini)
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
    fn php_language_constructs_get_builtin_function_scope() {
        let doc = Document::from_text("<?php\nif (empty($a)) { foo(); }\n");
        let language = SyntaxLanguage::Php;
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(doc.text(), None).unwrap();
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 2);
        let on = |from: usize, to: usize| {
            runs.iter()
                .filter(|r| r.line == 1 && r.start == from && r.end == to)
                .map(|r| r.scope)
                .collect::<Vec<_>>()
        };
        assert!(on(4, 9).contains(&HighlightScope::FunctionBuiltin));
        assert!(on(17, 20).contains(&HighlightScope::Function));
    }

    #[test]
    fn php_this_gets_builtin_variable_scope() {
        let doc = Document::from_text("<?php\n$this->a = $b;\n");
        let language = SyntaxLanguage::Php;
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(doc.text(), None).unwrap();
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 2);
        let last = |from: usize, to: usize| {
            runs.iter()
                .filter(|r| r.line == 1 && r.start == from && r.end == to)
                .map(|r| r.scope)
                .next_back()
        };
        assert_eq!(last(0, 5), Some(HighlightScope::VariableBuiltin));
        assert_eq!(last(11, 13), Some(HighlightScope::Variable));
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
    fn vue_script_and_style_blocks_get_injected_styles() {
        let src = "<template>\n  <p class=\"a\">{{ n }}</p>\n</template>\n<script setup lang=\"ts\">\nconst n: number = 1\n</script>\n<style scoped>\n.a { color: red; }\n</style>\n";
        let doc = Document::from_text(src);
        let language = SyntaxLanguage::detect("src/App.vue", "").unwrap();
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(doc.text(), None).unwrap();
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 9);
        let at = |line: usize, text: &str, scope: HighlightScope| {
            let line_text = crate::viewport::display_text(&doc, line);
            let start = line_text[..line_text.find(text).unwrap()]
                .encode_utf16()
                .count();
            let end = start + text.encode_utf16().count();
            runs.iter()
                .any(|r| r.line == line && r.start == start && r.end == end && r.scope == scope)
        };
        assert!(at(1, "p", HighlightScope::Tag));
        assert!(at(4, "const", HighlightScope::Keyword), "{runs:?}");
        assert!(at(4, "number", HighlightScope::Type), "{runs:?}");
        assert!(at(7, "color", HighlightScope::Property), "{runs:?}");
    }

    #[test]
    fn vue_template_expressions_are_styled_as_javascript() {
        let src = "<template>\n  <p :class=\"{ active: on }\" @click=\"count++\">{{ user.name + 1 }}</p>\n</template>\n";
        let doc = Document::from_text(src);
        let language = SyntaxLanguage::Vue;
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(doc.text(), None).unwrap();
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 4);
        let find = |text: &str| {
            let line_text = crate::viewport::display_text(&doc, 1);
            let start = line_text[..line_text.find(text).unwrap()]
                .encode_utf16()
                .count();
            (start, start + text.encode_utf16().count())
        };
        let at = |text: &str, scope: HighlightScope| {
            let (start, end) = find(text);
            runs.iter()
                .any(|r| r.line == 1 && r.start == start && r.end == end && r.scope == scope)
        };
        assert!(at("active", HighlightScope::Property), "{runs:?}");
        assert!(at("on", HighlightScope::Variable), "{runs:?}");
        assert!(at("count", HighlightScope::Variable), "{runs:?}");
        assert!(at("user", HighlightScope::Variable), "{runs:?}");
        assert!(at("name", HighlightScope::Property), "{runs:?}");
        assert!(at("1", HighlightScope::Number), "{runs:?}");
        assert!(at("{{", HighlightScope::Punctuation), "{runs:?}");
        // The flat HTML string color no longer covers a directive value.
        let (from, to) = find("{ active: on }");
        assert!(
            !runs.iter().any(|r| r.line == 1
                && r.scope == HighlightScope::String
                && r.start < to
                && r.end > from),
            "{runs:?}"
        );
        // A plain attribute keeps its string color.
        let plain = Document::from_text("<p class=\"a\"></p>");
        let tree = parser.parse(plain.text(), None).unwrap();
        let runs = style_runs_for_viewport(&plain, &tree, language, 0, 1);
        assert!(runs.iter().any(|r| r.scope == HighlightScope::String));
    }

    #[test]
    fn vue_interpolation_ignores_close_markers_in_javascript_strings() {
        let src = "<p>{{ value + '}}' }}</p>";
        let doc = Document::from_text(src);
        let mut parser = Parser::new();
        parser.set_language(&SyntaxLanguage::Vue.grammar()).unwrap();
        let tree = parser.parse(doc.text(), None).unwrap();
        let runs = style_runs_for_viewport(&doc, &tree, SyntaxLanguage::Vue, 0, 1);
        assert!(runs.iter().any(|run| run.scope == HighlightScope::Variable));
        let close = src.rfind("}}").unwrap();
        let close_utf16 = src[..close].encode_utf16().count();
        assert!(runs.iter().any(|run| {
            run.scope == HighlightScope::Punctuation
                && run.start == close_utf16
                && run.end == close_utf16 + 2
        }));
    }

    #[test]
    fn svelte_injects_blocks_and_balanced_template_expressions() {
        let src = "<script lang=\"ts\">const total: number = 1;</script>\n<style>.item { color: red; }</style>\n{#if total + ` ${value}`}{total}{:else if ready}{#each items as item}{item.name}{/each}{/if}\n{#await load()}{:then value}{@html value}{:catch error}{@const label = value}\n<button on:click={count++} class:active={ready}>{count}</button>";
        let doc = Document::from_text(src);
        let mut parser = Parser::new();
        parser
            .set_language(&SyntaxLanguage::Svelte.grammar())
            .unwrap();
        let tree = parser.parse(doc.text(), None).unwrap();
        let runs = style_runs_for_viewport(&doc, &tree, SyntaxLanguage::Svelte, 0, 5);
        let has = |text: &str, scope: HighlightScope| {
            let start = src.find(text).unwrap();
            let end = start + text.len();
            runs.iter().any(|run| {
                let line_start = doc.rope().line_to_byte(run.line);
                run.scope == scope && line_start + run.start == start && line_start + run.end == end
            })
        };
        assert!(has("const", HighlightScope::Keyword), "{runs:?}");
        assert!(has("number", HighlightScope::Type), "{runs:?}");
        assert!(has("color", HighlightScope::Property), "{runs:?}");
        assert!(has("total", HighlightScope::Variable), "{runs:?}");
        assert!(has("count", HighlightScope::Variable), "{runs:?}");
    }

    #[test]
    fn vue_block_language_follows_lang_attribute() {
        assert_eq!(
            vue_block_language("script_element", "<script setup lang=\"ts\">"),
            Some(SyntaxLanguage::TypeScript)
        );
        assert_eq!(
            vue_block_language("script_element", "<script>"),
            Some(SyntaxLanguage::JavaScript)
        );
        assert_eq!(
            vue_block_language("style_element", "<style lang='scss'>"),
            None
        );
        assert_eq!(
            vue_block_language("style_element", "<style scoped>"),
            Some(SyntaxLanguage::Css)
        );
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

    #[test]
    fn php_query_colours_static_scopes_and_class_names() {
        let language = SyntaxLanguage::Php;
        let source = "<?php\nclass Pay extends Base implements Contract {\n    protected static function booted(): void {\n        static::creating(fn (Pay $p) => self::make());\n        $m = [Method::class, Status::$all];\n    }\n}\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let query = language.compile_highlights().unwrap();
        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
        let mut found = std::collections::HashSet::new();
        while let Some(m) = matches.next() {
            for capture in m.captures {
                if let Some(scope) = HighlightScope::from_capture(names[capture.index as usize]) {
                    found.insert((source[capture.node.byte_range()].to_owned(), scope));
                }
            }
        }
        let has = |text: &str, scope| found.contains(&(text.to_owned(), scope));
        for word in ["static", "self", "class"] {
            assert!(
                has(word, HighlightScope::Keyword),
                "{word} is not a keyword"
            );
        }
        for name in ["Pay", "Base", "Contract", "Method", "Status"] {
            assert!(has(name, HighlightScope::Type), "{name} is not a type");
        }
        assert!(!has("static", HighlightScope::Variable));
    }
}
