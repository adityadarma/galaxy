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
    Xml,
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
    pub const ALL: [Self; 32] = [
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
        Self::Xml,
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
            Self::Xml => "XML",
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
            | Self::Xml
            | Self::Css
            | Self::Markdown
            | Self::Blade
            | Self::Vue
            | Self::Svelte => None,
            Self::Php => Some("//"),
        }
    }

    /// The comment marker for the file at `path`. Dotenv files share the INI
    /// grammar but comment with `#`; real INI files keep `;`.
    pub fn comment_marker_for(self, path: &str) -> Option<&'static str> {
        if self == Self::Ini && is_dotenv(path) {
            return Some("#");
        }
        self.comment_marker()
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
            Self::Xml => tree_sitter_xml::LANGUAGE_XML.into(),
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
            Self::TypeScript => TYPESCRIPT_HIGHLIGHTS,
            Self::Tsx => tsx_highlights(),
            Self::JavaScript => javascript_highlights(),
            Self::Json => json_highlights(),
            Self::Toml => tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            Self::Yaml => tree_sitter_yaml::HIGHLIGHTS_QUERY,
            // The inline grammar is injected by M3-05; this captures Markdown
            // block nodes now and leaves inline styling as plain text.
            Self::Markdown => tree_sitter_md_025::HIGHLIGHT_QUERY_BLOCK,
            Self::Python => tree_sitter_python::HIGHLIGHTS_QUERY,
            Self::Go => tree_sitter_go::HIGHLIGHTS_QUERY,
            Self::Html | Self::Blade | Self::Vue | Self::Svelte => html_highlights(),
            Self::Xml => tree_sitter_xml::XML_HIGHLIGHT_QUERY,
            Self::Css => css_highlights(),
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
            Self::Ini => ini_highlights(),
            Self::PowerShell => tree_sitter_powershell::HIGHLIGHTS_QUERY,
            Self::Dart => dart_highlights(),
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
            ".env" => Some(Self::Ini),
            "composer.lock" => Some(Self::Json),
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
        if name.starts_with(".env.") {
            return Some(Self::Ini);
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
            "xml" => Some(Self::Xml),
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
            "ini" | "cfg" | "conf" | "env" => Some(Self::Ini),
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
            "php" => Some(Self::Php),
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
/// `.env` and `.env.*` (`.env.local`, `.env.production`), by file name.
fn is_dotenv(path: &str) -> bool {
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase();
    name == ".env" || name.starts_with(".env.")
}

fn php_highlights() -> &'static str {
    static QUERY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    QUERY.get_or_init(|| {
        let base = tree_sitter_php::HIGHLIGHTS_QUERY.replace(
            "(relative_scope) @variable.builtin",
            "(relative_scope) @keyword",
        );
        let builtins = crate::php::builtin_function_names()
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect::<Vec<_>>()
            .join(" ");
        let magic_methods = [
            "__construct",
            "__destruct",
            "__call",
            "__callStatic",
            "__get",
            "__set",
            "__isset",
            "__unset",
            "__sleep",
            "__wakeup",
            "__serialize",
            "__unserialize",
            "__toString",
            "__invoke",
            "__set_state",
            "__clone",
            "__debugInfo",
        ]
        .map(|name| format!("\"{name}\""))
        .join(" ");
        format!(
            r#"{base}
(class_declaration name: (name) @type)
(interface_declaration name: (name) @type)
(trait_declaration name: (name) @type)
(enum_declaration name: (name) @type)
(enum_case name: (name) @constant)
(base_clause [(name) (qualified_name (name))] @type)
(class_interface_clause [(name) (qualified_name (name))] @type)
(use_declaration [(name) (qualified_name (name))] @type)
(attribute [(name) (qualified_name (name))] @type)
(class_constant_access_expression . [(name) (qualified_name (name))] @type)
(class_constant_access_expression (name) @keyword (#eq? @keyword "class"))
(scoped_property_access_expression scope: [(name) (qualified_name (name))] @type)
(nullsafe_member_access_expression name: (name) @property)
(binary_expression operator: "instanceof" right: [(name) (qualified_name (name))] @type)
[(primitive_type) (bottom_type)] @type
((variable_name (name) @_this) @variable.builtin (#eq? @_this "this"))
(function_definition name: (name) @function (#any-of? @function {builtins}))
(unset_statement "unset" @function.builtin)
(function_call_expression function: (name) @function.builtin (#any-of? @function.builtin "empty" "isset" "unset" "eval" "die" "exit" "compact" "extract" {builtins}))
(function_call_expression function: (qualified_name (name) @function.builtin) (#any-of? @function.builtin "empty" "isset" "unset" "eval" "die" "exit" "compact" "extract" {builtins}))
(method_declaration name: (name) @function.builtin (#any-of? @function.builtin {magic_methods}))
(member_call_expression name: (name) @function.builtin (#any-of? @function.builtin {magic_methods}))
(nullsafe_member_call_expression name: (name) @function.builtin (#any-of? @function.builtin {magic_methods}))
(scoped_call_expression name: (name) @function.builtin (#any-of? @function.builtin {magic_methods}))
 ["(" ")" "[" "]" "{{" "}}" "," ";" ":" "?" "=>" "->" "?->" "::" "++" "--"] @punctuation
 ["=" "+" "-" "*" "/" "%" "**" "." "==" "===" "!=" "!==" "<>" ">" ">=" ">>" "<" "<=" "<<" "<=>" "&&" "||" "??" "|>" "!" "&" "|" "^" "~" "@" "+=" "-=" "*=" "/=" "%=" "**=" ".=" "&=" "|=" "^=" "<<=" ">>=" "??="] @operator
"#
        )
    })
}

/// JSON's upstream query colors values but intentionally leaves structural
/// delimiters plain. The editor uses the same punctuation scope for every
/// bracket family, matching JavaScript and TypeScript.
fn json_highlights() -> &'static str {
    static QUERY: OnceLock<String> = OnceLock::new();
    QUERY.get_or_init(|| {
        format!(
            "{}\n(pair key: (string) @property.json)\n[\"[\" \"]\" \"{{\" \"}}\" \",\"] @punctuation\n\":\" @operator\n",
            tree_sitter_json::HIGHLIGHTS_QUERY
        )
    })
}

/// CSS's grammar leaves bare values such as `white`, `bold`, `block` and
/// `inherit` as plain text. They are semantic values, not document text.
fn css_highlights() -> &'static str {
    static QUERY: OnceLock<String> = OnceLock::new();
    QUERY.get_or_init(|| {
        format!(
            "{}\n((plain_value) @constant (#not-match? @constant \"^--\"))\n",
            tree_sitter_css::HIGHLIGHTS_QUERY
        )
    })
}

/// The INI grammar exposes every value as one opaque `setting_value` node and
/// the upstream query leaves it plain. Classify it by its text so `.env` and
/// config files tell numbers, booleans/null and strings apart. The patterns
/// avoid backslashes because the query parser consumes them as escapes.
fn ini_highlights() -> &'static str {
    const NUMBER: &str =
        "^[-+]?(0[xX][0-9a-fA-F]+|[0-9]+([.][0-9]+)?([eE][-+]?[0-9]+)?)[[:space:]]*$";
    const KEYWORD: &str = "(?i)^(true|false|yes|no|on|off|null|nil|none)[[:space:]]*$";
    static QUERY: OnceLock<String> = OnceLock::new();
    QUERY.get_or_init(|| {
        format!(
            "{base}\n\
             ((setting_value) @number (#match? @number \"{NUMBER}\"))\n\
             ((setting_value) @constant (#match? @constant \"{KEYWORD}\"))\n\
             ((setting_value) @string\n  (#not-match? @string \"{NUMBER}\")\n  (#not-match? @string \"{KEYWORD}\"))\n",
            base = tree_sitter_ini::HIGHLIGHTS_QUERY
        )
    })
}

/// The grammar's HTML query, which colors an attribute's value but not the
/// quotes around it. They take the value's color, as in `title="Stock"`.
fn html_highlights() -> &'static str {
    static QUERY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    QUERY.get_or_init(|| {
        format!(
            "{}\n(quoted_attribute_value [\"\\\"\" \"'\"] @string)\n",
            tree_sitter_html::HIGHLIGHTS_QUERY
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

/// The bundled Dart query leaves declaration modifiers unscoped. Add them
/// explicitly so `final` and related modifiers use the keyword color.
fn dart_highlights() -> &'static str {
    static QUERY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    QUERY.get_or_init(|| {
        format!(
            "{}\n[\"final\" \"const\" \"late\" \"var\"] @keyword\n",
            tree_sitter_dart::HIGHLIGHTS_QUERY
        )
    })
}

/// JSX component names (`<UserCard>`, `<Form.Item>`) take the class color.
/// Only TSX and JavaScript have JSX nodes; the plain TypeScript grammar
/// would reject these patterns.
const JSX_COMPONENT_HIGHLIGHTS: &str = r#"
(jsx_opening_element name: (identifier) @type (#match? @type "^[A-Z]"))
(jsx_closing_element name: (identifier) @type (#match? @type "^[A-Z]"))
(jsx_self_closing_element name: (identifier) @type (#match? @type "^[A-Z]"))
(jsx_opening_element name: (member_expression property: (property_identifier) @type))
(jsx_closing_element name: (member_expression property: (property_identifier) @type))
(jsx_self_closing_element name: (member_expression property: (property_identifier) @type))
; `<` and `>` are already operators; the JSX-only `</` and `/>` match them.
["</" "/>"] @operator
"#;

fn tsx_highlights() -> &'static str {
    static QUERY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    QUERY.get_or_init(|| format!("{TYPESCRIPT_HIGHLIGHTS}\n{JSX_COMPONENT_HIGHLIGHTS}"))
}

fn javascript_highlights() -> &'static str {
    static QUERY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    QUERY.get_or_init(|| format!("{JAVASCRIPT_HIGHLIGHTS}\n{JSX_COMPONENT_HIGHLIGHTS}"))
}

// These two grammar crates do not export their upstream query. Keep a small,
// self-contained equivalent here rather than depending on Cargo registry paths.
const TYPESCRIPT_HIGHLIGHTS: &str = r#"
(comment) @comment
(type_identifier) @type
(predefined_type) @type
(class_declaration name: (type_identifier) @type)
(interface_declaration name: (type_identifier) @type)
(type_alias_declaration name: (type_identifier) @type)
(enum_declaration name: (identifier) @type)
(extends_clause (identifier) @type)
(new_expression constructor: (identifier) @type)
((identifier) @type (#any-of? @type "Array" "Boolean" "Date" "Error" "JSON" "Map" "Math" "Number" "Object" "Promise" "Record" "RegExp" "Set" "String" "Symbol" "WeakMap" "WeakSet"))
((member_expression object: (identifier) @type) (#match? @type "^[A-Z]"))
[
  (string)
  (template_string)
  (regex)
] @string
(number) @number
(property_identifier) @property
(private_property_identifier) @property
(member_expression property: (property_identifier) @property)
(optional_chain) @punctuation
(call_expression function: (identifier) @function)
(call_expression function: (identifier) @function.builtin (#eq? @function.builtin "require"))
("require") @function.builtin
(call_expression function: (member_expression property: (property_identifier) @function))
(function_declaration name: (identifier) @function)
(method_definition name: (property_identifier) @function)
(method_signature name: (property_identifier) @function)
(function_expression name: (identifier) @function)
[(this) (this_type)] @variable.builtin
[(true) (false) (null) (undefined)] @constant
(identifier) @variable
; Object shorthand values: `{ type, data }`.
(shorthand_property_identifier) @variable
; Destructured bindings: `({ initialPage, onFlash }: Params)`.
(shorthand_property_identifier_pattern) @variable
[
  "async" "await" "break" "case" "catch" "class" "const" "continue"
  "debugger" "default" "delete" "do" "else" "export" "extends" "finally"
  "for" "from" "function" "if" "import" "in" "instanceof" "let" "new"
  "of" "return" "static" "switch" "throw" "try" "typeof" "var" "void"
  "while" "with" "yield" "as" "interface" "implements" "private"
   "protected" "public" "readonly" "type" "namespace" "declare" "keyof"
    "get" "set" "global" "module" "enum" "satisfies" "is" "asserts" "infer"
    "override" "accessor"
] @keyword
(meta_property ["meta" "target"] @keyword)
(abstract_class_declaration "abstract" @keyword)
("abstract") @keyword
[
   ";" "." "," "(" ")" "[" "]" "{" "}" ":" "?" "..." "=>" "++" "--"
 ] @punctuation
; `${` opens a bracket pair with the substitution's `}`.
(template_substitution "${" @punctuation)
 [
   "=" "+" "-" "*" "/" "%" "**" "==" "===" "!=" "!==" ">" ">=" ">>" ">>>" "<" "<=" "<<"
  "&&" "||" "??" "!" "&" "|" "^" "~" "+=" "-=" "*=" "/=" "%=" "**=" "<<=" ">>=" ">>>=" "&=" "|=" "^=" "&&=" "||=" "??="
 ] @operator
"#;

const JAVASCRIPT_HIGHLIGHTS: &str = r#"
[(comment)] @comment
[(string) (template_string) (regex)] @string
(number) @number
[(true) (false) (null) (undefined)] @constant
(identifier) @variable
(shorthand_property_identifier) @variable
(shorthand_property_identifier_pattern) @variable
(property_identifier) @property
(private_property_identifier) @property
(function_declaration name: (identifier) @function)
(call_expression function: (identifier) @function)
(call_expression function: (identifier) @function.builtin (#eq? @function.builtin "require"))
((import_statement "from" @keyword))
[("const") ("let") ("var") ("function") ("class") ("return") ("if") ("else") ("for") ("while") ("import") ("export") ("default") ("async") ("await") ("new")] @keyword
(meta_property ["meta" "target"] @keyword)
[("=") ("+") ("-") ("*") ("/") ("%") ("**") ("==") ("===") ("!=") ("!==") (">") (">=") (">>") (">>=") (">>>") (">>>=") ("<") ("<=") ("<<") ("<<=") ("&&") ("||") ("??") ("!") ("&") ("|") ("^") ("~") ("+=") ("-=") ("*=") ("/=") ("%=") ("**=") ("&=") ("|=") ("^=") ("&&=") ("||=") ("??=")] @operator
[("(") (")") ("[") ("]") ("{") ("}") (",") (";") (".") (":") ("?") ("=>") ("++") ("--")] @punctuation
(template_substitution "${" @punctuation)
(optional_chain) @punctuation
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
    PropertyJson,
    Punctuation,
    PunctuationBracket1,
    PunctuationBracket2,
    PunctuationBracket3,
    PunctuationBracket4,
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
            "property.json" => return Some(Self::PropertyJson),
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
            "string" | "character" | "text" => Some(Self::String),
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
            Self::PropertyJson => "property.json",
            Self::Punctuation => "punctuation",
            Self::PunctuationBracket1 => "punctuation.bracket.1",
            Self::PunctuationBracket2 => "punctuation.bracket.2",
            Self::PunctuationBracket3 => "punctuation.bracket.3",
            Self::PunctuationBracket4 => "punctuation.bracket.4",
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
    if cached_highlight_query(language).is_none() {
        return Vec::new();
    }
    let mut spans = collect_style_spans(rope, tree, language, start_byte, end_byte);
    // The window starts mid-file, so brackets opened above it still count.
    // Without them a nesting level changes color as the window scrolls.
    let outer = open_brackets_before(rope, tree, language, start_byte);
    rainbow_brackets(rope, &mut spans, &outer);
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

/// Every highlight span of `start_byte..end_byte`, before bracket pairs are
/// colored: the grammar's captures and the overlays of embedded languages.
fn collect_style_spans(
    rope: &Rope,
    tree: &Tree,
    language: SyntaxLanguage,
    start_byte: usize,
    end_byte: usize,
) -> Vec<(usize, usize, HighlightScope)> {
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
    if language == SyntaxLanguage::Blade {
        let mut scan = VueScan {
            rope,
            window: (start_byte, end_byte),
            overlay: crate::php::framework::laravel::blade::Overlay::default(),
            blade: true,
        };
        scan.walk_blocks(tree.root_node());
        spans.extend(scan.overlay.spans.into_iter().filter_map(|(s, e, scope)| {
            let (s, e) = (s.max(start_byte), e.min(end_byte));
            (s < e).then_some((s, e, scope))
        }));
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
    if language == SyntaxLanguage::Php && tree.root_node().has_error() {
        recover_php_errors(rope, tree, start_byte, end_byte, &mut spans);
    }
    if matches!(language, SyntaxLanguage::Tsx | SyntaxLanguage::JavaScript)
        && tree.root_node().has_error()
    {
        let claimed = bare_jsx_ampersand_ranges(rope, tree, start_byte, end_byte);
        if !claimed.is_empty() {
            spans = crate::php::framework::laravel::blade::subtract(spans, &claimed);
        }
    }
    if matches!(language, SyntaxLanguage::Vue | SyntaxLanguage::Svelte) {
        // Component tags (`<UserCard>`, Vue `<user-card>`) take the class
        // color, like JSX components; plain HTML tags keep the tag color.
        for (start, end, scope) in &mut spans {
            if *scope == HighlightScope::Tag
                && text_between(rope, *start, *end)
                    .is_some_and(|name| is_component_tag(language, &name))
            {
                *scope = HighlightScope::Type;
            }
        }
    }
    spans
}

/// `Operations & Automation` between JSX tags. The grammar only accepts
/// `&name;` there and wraps the rest in `ERROR`, whose words then match
/// `@variable` and `@operator`. TypeScript reads it as text, so these ranges
/// drop their captures and render like the text around them. Sorted and
/// non-overlapping, as `subtract` requires.
fn bare_jsx_ampersand_ranges(
    rope: &Rope,
    tree: &Tree,
    window_start: usize,
    window_end: usize,
) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.end_byte() <= window_start || node.start_byte() >= window_end {
            continue;
        }
        if node.is_error() {
            let in_jsx = node
                .parent()
                .is_some_and(|parent| matches!(parent.kind(), "jsx_element" | "jsx_fragment"));
            if in_jsx
                && text_between(rope, node.start_byte(), node.end_byte()).is_some_and(|source| {
                    source.starts_with('&') && !source.contains(['<', '>', '{', '}'])
                })
            {
                ranges.push((node.start_byte(), node.end_byte()));
            }
            continue;
        }
        if node.has_error() {
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
    }
    ranges.sort_unstable();
    ranges
}

/// `<Button>` in any template language, and `<my-card>` in Vue, where a
/// hyphenated name is a registered component.
fn is_component_tag(language: SyntaxLanguage, name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
        || (language == SyntaxLanguage::Vue && name.contains('-'))
}

/// Tree-sitter can put a long valid tail inside `ERROR` after one missing
/// semicolon. Fill the basic lexical scopes in those ranges so strings,
/// variables, calls and control flow do not become plain text.
fn recover_php_errors(
    rope: &Rope,
    tree: &Tree,
    window_start: usize,
    window_end: usize,
    spans: &mut Vec<(usize, usize, HighlightScope)>,
) {
    const KEYWORDS: &[&str] = &[
        "break", "case", "catch", "continue", "default", "do", "else", "elseif", "finally", "for",
        "foreach", "if", "match", "return", "switch", "throw", "try", "while", "yield",
    ];
    // Correct captures that recovery classified as calls first.
    for (start, end, scope) in spans.iter_mut() {
        if !matches!(
            *scope,
            HighlightScope::Function | HighlightScope::FunctionBuiltin
        ) {
            continue;
        }
        let Some(token) = text_between(rope, *start, *end) else {
            continue;
        };
        if KEYWORDS.contains(&token.as_str()) {
            *scope = HighlightScope::Keyword;
        }
    }

    let mut errors = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.is_error() {
            let start = node.start_byte();
            let end = node.end_byte();
            if start < window_end && end > window_start {
                errors.push((start, end));
            }
            continue;
        }
        if node.has_error() {
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
    }
    for (start, end) in errors {
        let Some(text) = text_between(rope, start, end) else {
            continue;
        };
        let mut recovered = Vec::new();
        recover_php_range(&text, start, KEYWORDS, &mut recovered);
        // Pair brackets before clipping to the viewport. Otherwise a closing
        // bracket whose opener scrolled above the window loses its rainbow
        // scope and appears as plain punctuation.
        let _ = rainbow_brackets(rope, &mut recovered, &[]);
        spans.extend(
            recovered
                .into_iter()
                .filter(|(start, end, _)| *start < window_end && *end > window_start),
        );
    }
}

fn recover_php_range(
    text: &str,
    offset: usize,
    keywords: &[&str],
    spans: &mut Vec<(usize, usize, HighlightScope)>,
) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i += text[i..].find('\n').unwrap_or(text.len() - i);
            }
            b'#' => i += text[i..].find('\n').unwrap_or(text.len() - i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += text[i + 2..]
                    .find("*/")
                    .map_or(text.len() - i, |end| end + 4);
            }
            quote @ (b'\'' | b'"') => {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i = (i + 2).min(bytes.len());
                    } else if bytes[i] == quote {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
                spans.push((offset + start, offset + i, HighlightScope::String));
            }
            b'$' if bytes
                .get(i + 1)
                .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_') =>
            {
                let start = i;
                i += 2;
                while bytes
                    .get(i)
                    .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                {
                    i += 1;
                }
                spans.push((offset + start, offset + i, HighlightScope::Variable));
            }
            byte if byte.is_ascii_alphabetic() || byte == b'_' => {
                let start = i;
                i += 1;
                while bytes
                    .get(i)
                    .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                {
                    i += 1;
                }
                let word = &text[start..i];
                let before = text[..start].trim_end();
                let after = text[i..].trim_start();
                let scope = if keywords.contains(&word.to_ascii_lowercase().as_str()) {
                    Some(HighlightScope::Keyword)
                } else if before.ends_with("::") {
                    Some(if after.starts_with('(') {
                        HighlightScope::Function
                    } else {
                        HighlightScope::Constant
                    })
                } else if before.ends_with("->") || before.ends_with("?->") {
                    Some(if after.starts_with('(') {
                        HighlightScope::Function
                    } else {
                        HighlightScope::Property
                    })
                } else if after.starts_with("::") {
                    Some(HighlightScope::Type)
                } else if after.starts_with('(') {
                    Some(HighlightScope::Function)
                } else {
                    None
                };
                if let Some(scope) = scope {
                    spans.push((offset + start, offset + i, scope));
                }
            }
            byte if byte.is_ascii_digit() => {
                let start = i;
                i += 1;
                while bytes
                    .get(i)
                    .is_some_and(|byte| byte.is_ascii_digit() || *byte == b'.')
                {
                    i += 1;
                }
                spans.push((offset + start, offset + i, HighlightScope::Number));
            }
            b'?' if bytes.get(i + 2) == Some(&b'>') && bytes.get(i + 1) == Some(&b'-') => {
                spans.push((offset + i, offset + i + 3, HighlightScope::Punctuation));
                i += 3;
            }
            b'=' if bytes.get(i + 1) == Some(&b'>') => {
                spans.push((offset + i, offset + i + 2, HighlightScope::Punctuation));
                i += 2;
            }
            b'-' if bytes.get(i + 1) == Some(&b'>') => {
                spans.push((offset + i, offset + i + 2, HighlightScope::Punctuation));
                i += 2;
            }
            b':' if bytes.get(i + 1) == Some(&b':') => {
                spans.push((offset + i, offset + i + 2, HighlightScope::Punctuation));
                i += 2;
            }
            b'(' | b')' | b'[' | b']' | b'{' | b'}' | b',' | b';' | b':' | b'?' => {
                spans.push((offset + i, offset + i + 1, HighlightScope::Punctuation));
                i += 1;
            }
            _ => i += 1,
        }
    }
}

/// Color matching bracket pairs by nesting level while leaving every other
/// punctuation token on the theme's ordinary punctuation color.
///
/// `outer` lists the brackets already open where `spans` begin, outermost
/// first, as their closing characters.
///
/// Returns the closing characters of the brackets still open after the last
/// span, outermost first.
fn rainbow_brackets(
    rope: &Rope,
    spans: &mut [(usize, usize, HighlightScope)],
    outer: &[u8],
) -> Vec<u8> {
    spans.sort_by_key(|(start, end, scope)| (*start, *end, highlight_priority(*scope)));
    let mut stack: Vec<(u8, HighlightScope)> = outer
        .iter()
        .enumerate()
        .map(|(depth, marker)| (*marker, rainbow_bracket_scope(depth)))
        .collect();
    let mut index = 0;
    while index < spans.len() {
        let (start, end, _) = spans[index];
        let mut next = index + 1;
        while next < spans.len() && spans[next].0 == start && spans[next].1 == end {
            next += 1;
        }
        // A token can carry several captures (a Lua `{` is also the table
        // constructor), so any plain punctuation capture makes it a bracket.
        let punctuation = spans[index..next]
            .iter()
            .any(|span| span.2 == HighlightScope::Punctuation);
        // The tree may be one edit behind the text (see `viewport`), so its
        // offsets can split a multi-byte character. Skip such a token.
        let token = if punctuation {
            text_between(rope, start, end)
        } else {
            None
        };
        if let Some(token) = token {
            let bracket = match token.as_str() {
                "(" => Some((b')', true)),
                "[" => Some((b']', true)),
                // `${` in a template string closes with `}`; without it the
                // `}` pairs with an outer `{` (JSX `className={`...`}`).
                "{" | "${" => Some((b'}', true)),
                ")" | "]" | "}" => Some((token.as_bytes()[0], false)),
                _ => None,
            };
            if let Some((marker, opening)) = bracket {
                let color = if opening {
                    let color = rainbow_bracket_scope(stack.len());
                    stack.push((marker, color));
                    Some(color)
                } else if stack
                    .last()
                    .is_some_and(|(expected, _)| *expected == marker)
                {
                    stack.pop().map(|(_, color)| color)
                } else {
                    None
                };
                if let Some(color) = color {
                    for span in &mut spans[index..next] {
                        if span.2 == HighlightScope::Punctuation {
                            span.2 = color;
                        }
                    }
                }
            }
        }
        index = next;
    }
    stack.into_iter().map(|(marker, _)| marker).collect()
}

/// Closing characters of the brackets that are open at `byte`, outermost
/// first, so a window that starts mid-file colors them as the whole file
/// would.
///
/// Embedded languages (the script of a Vue or Svelte component) put their
/// brackets in spans that the main tree does not hold, so those files are
/// counted from the first span of the file. Other files read the syntax
/// tree: for each node around `byte`, its direct bracket children that end
/// before `byte` are paired off, and the ones left over are still open.
fn open_brackets_before(
    rope: &Rope,
    tree: &Tree,
    language: SyntaxLanguage,
    byte: usize,
) -> Vec<u8> {
    if byte == 0 {
        return Vec::new();
    }
    let embedded = matches!(
        language,
        SyntaxLanguage::Vue | SyntaxLanguage::Svelte | SyntaxLanguage::Blade
    );
    if embedded && byte <= MAX_TAG_SCAN_BYTES {
        let mut spans = collect_style_spans(rope, tree, language, 0, byte);
        return rainbow_brackets(rope, &mut spans, &[]);
    }
    let mut ancestors = Vec::new();
    let mut node = tree.root_node().descendant_for_byte_range(byte, byte);
    while let Some(current) = node {
        ancestors.push(current);
        node = current.parent();
    }
    let mut open = Vec::new();
    for ancestor in ancestors.into_iter().rev() {
        let mut stack: Vec<u8> = Vec::new();
        let mut cursor = ancestor.walk();
        for child in ancestor.children(&mut cursor) {
            if child.start_byte() >= byte {
                break;
            }
            if child.is_named() {
                continue;
            }
            match child.kind() {
                "(" => stack.push(b')'),
                "[" => stack.push(b']'),
                "{" | "${" => stack.push(b'}'),
                kind @ (")" | "]" | "}") => {
                    if stack.last() == Some(&kind.as_bytes()[0]) {
                        stack.pop();
                    }
                }
                _ => {}
            }
        }
        open.extend(stack);
    }
    open
}

fn rainbow_bracket_scope(depth: usize) -> HighlightScope {
    match depth % 4 {
        0 => HighlightScope::PunctuationBracket1,
        1 => HighlightScope::PunctuationBracket2,
        2 => HighlightScope::PunctuationBracket3,
        _ => HighlightScope::PunctuationBracket4,
    }
}

fn highlight_priority(scope: HighlightScope) -> u8 {
    match scope {
        HighlightScope::Variable => 0,
        HighlightScope::Property => 1,
        HighlightScope::Function | HighlightScope::PropertyJson | HighlightScope::Type => 2,
        HighlightScope::FunctionBuiltin => 3,
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
        blade: false,
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
    scan.tags();
    scan.braced_expressions();
    let mut overlay = scan.overlay;
    // Claims come from tags and braces separately; `subtract` needs them
    // sorted and disjoint.
    overlay.claimed.sort_unstable();
    overlay.claimed.dedup_by(|next, kept| {
        let overlaps = next.0 <= kept.1;
        if overlaps {
            kept.1 = kept.1.max(next.1);
        }
        overlaps
    });
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
    /// Blade: blank out Blade constructs in a block before parsing it.
    blade: bool,
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
        let text = if self.blade {
            crate::php::framework::laravel::blade::mask(&text)
        } else {
            text
        };
        let Some(sub) = cached_parse(language, &text) else {
            return;
        };
        self.captures(language, &text, &sub, from, (from, to));
    }

    /// Blade: only the `<script>` and `<style>` bodies are injected. Blade
    /// has no Vue-style attributes or interpolations to read.
    fn walk_blocks(&mut self, node: tree_sitter::Node) {
        if node.end_byte() <= self.window.0 || node.start_byte() >= self.window.1 {
            return;
        }
        if matches!(node.kind(), "script_element" | "style_element") {
            return self.block(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.walk_blocks(child);
        }
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

/// Largest document whose markup is scanned from the top for tag attributes.
const MAX_TAG_SCAN_BYTES: usize = 512 * 1024;

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

    fn claim(&mut self, from: usize, to: usize, scope: HighlightScope) {
        if from < to && to > self.window.0 && from < self.window.1 {
            self.overlay.claimed.push((from, to));
            self.overlay.spans.push((from, to, scope));
        }
    }

    /// Attribute names and values of every start tag in the window. The HTML
    /// grammar ends a tag at the `>` of `onclick={() => f()}` and splits
    /// `on:x={{ a: 'b' }}` at its spaces, losing the attributes that follow,
    /// so the markup is scanned as text: scripts, styles, comments and
    /// braced expressions are skipped, the rest is read tag by tag.
    fn tags(&mut self) {
        // A tag can start above the window, so read from the top of the file
        // (components are small; huge files fall back to the window).
        let whole = self.rope.len_bytes() <= MAX_TAG_SCAN_BYTES;
        let (base, end) = if whole {
            (0, self.rope.len_bytes())
        } else {
            self.window
        };
        let Some(text) = text_between(self.rope, base, end) else {
            return;
        };
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'{' => {
                    i = find_balanced_close(&text, i + 1, "}").map_or(i + 1, |close| close + 1);
                }
                b'<' if text[i..].starts_with("<!--") => {
                    i = text[i..].find("-->").map_or(bytes.len(), |end| i + end + 3);
                }
                b'<' if bytes.get(i + 1) == Some(&b'/')
                    && bytes.get(i + 2).is_some_and(u8::is_ascii_alphabetic) =>
                {
                    let Some(end) = text[i..].find('>').map(|at| i + at) else {
                        i += 1;
                        continue;
                    };
                    let name_end = text[i + 2..end]
                        .find(char::is_whitespace)
                        .map_or(end, |at| i + 2 + at);
                    self.claim(base + i, base + i + 2, HighlightScope::Punctuation);
                    self.claim(base + i + 2, base + name_end, HighlightScope::Tag);
                    self.claim(base + end, base + end + 1, HighlightScope::Punctuation);
                    i = end + 1;
                }
                b'<' if bytes.get(i + 1).is_some_and(u8::is_ascii_alphabetic) => {
                    let name_end = text[i + 1..]
                        .find(|c: char| c.is_whitespace() || matches!(c, '>' | '/'))
                        .map_or(bytes.len(), |at| i + 1 + at);
                    let name = &text[i + 1..name_end];
                    let (attributes, length) = svelte_tag_attributes(&text[i..]);
                    for (from, to, scope) in attributes {
                        self.claim(base + i + from, base + i + to, scope);
                    }
                    if length > 1 {
                        // The HTML grammar loses names and brackets after a
                        // tag it mis-parses, so they are claimed here too.
                        self.claim(base + i, base + i + 1, HighlightScope::Punctuation);
                        self.claim(base + i + 1, base + name_end, HighlightScope::Tag);
                        let close = base + i + length;
                        let width = if text[..i + length].ends_with("/>") {
                            2
                        } else {
                            1
                        };
                        self.claim(close - width, close, HighlightScope::Punctuation);
                    }
                    i += length.max(1);
                    // Script and style bodies are code, not markup.
                    let closing = if name.eq_ignore_ascii_case("script") {
                        Some("</script")
                    } else if name.eq_ignore_ascii_case("style") {
                        Some("</style")
                    } else {
                        None
                    };
                    if let Some(closing) = closing
                        && length > 1
                        && !text[..i].ends_with("/>")
                    {
                        i = text[i..].find(closing).map_or(bytes.len(), |at| i + at);
                    }
                }
                _ => i += 1,
            }
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
        // The expression follows the block keyword (`@render`, `#if`), so its
        // bytes start `shift` bytes after the opening brace.
        let shift = expression.as_ptr() as usize - expr.as_ptr() as usize;
        if let Some(keyword) = svelte_keyword(&expr) {
            self.overlay.spans.push((
                from + keyword.start,
                from + keyword.end,
                HighlightScope::Keyword,
            ));
        }
        if let Some(each) = svelte_each_parts(&expr) {
            self.each_parts(&expr, from, &each);
        }
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
            (from + shift).saturating_sub(1),
            (from + shift, to),
        );
    }

    /// `as item, index (key)` of an `{#each}` block: the `as` keyword, the
    /// binding pattern (parsed as function parameters) and the key expression.
    fn each_parts(&mut self, expr: &str, from: usize, each: &SvelteEach) {
        self.overlay.spans.push((
            from + each.as_keyword.start,
            from + each.as_keyword.end,
            HighlightScope::Keyword,
        ));
        const PREFIX: &str = "(function(";
        let binding = &expr[each.binding.clone()];
        let wrapped = format!("{PREFIX}{binding}){{}})");
        if let Some(tree) = cached_parse(SyntaxLanguage::JavaScript, &wrapped) {
            let start = from + each.binding.start;
            self.captures(
                SyntaxLanguage::JavaScript,
                &wrapped,
                &tree,
                start.saturating_sub(PREFIX.len()),
                (start, from + each.binding.end),
            );
        }
        if let Some(key) = &each.key
            && let Some(tree) = cached_parse(SyntaxLanguage::JavaScript, &expr[key.clone()])
        {
            self.captures(
                SyntaxLanguage::JavaScript,
                &expr[key.clone()],
                &tree,
                from + key.start,
                (from + key.start, from + key.end),
            );
        }
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

/// Attribute names (`Attribute`) and plain or quoted values (`String`) of one
/// start tag, as byte ranges within `tag`, plus the tag's length. Braced
/// values are left to the template-expression pass. A tag without its closing
/// `>` (half typed, or `a<b` in text) yields nothing and a length of 1.
fn svelte_tag_attributes(tag: &str) -> (Vec<(usize, usize, HighlightScope)>, usize) {
    let bytes = tag.as_bytes();
    let mut out = Vec::new();
    if bytes.first() != Some(&b'<') {
        return (out, 1);
    }
    let is_space = |byte: u8| byte.is_ascii_whitespace();
    let skip_braces = |at: usize| find_balanced_close(tag, at + 1, "}").map(|close| close + 1);
    let mut i = 1;
    while i < bytes.len() && !is_space(bytes[i]) && !matches!(bytes[i], b'>' | b'/') {
        i += 1;
    }
    let mut terminated = false;
    while i < bytes.len() {
        if is_space(bytes[i]) || bytes[i] == b'/' {
            i += 1;
            continue;
        }
        match bytes[i] {
            b'>' => {
                terminated = true;
                i += 1;
                break;
            }
            b'{' => match skip_braces(i) {
                Some(next) => i = next,
                None => break,
            },
            _ => {
                let start = i;
                while i < bytes.len()
                    && !is_space(bytes[i])
                    && !matches!(bytes[i], b'=' | b'>' | b'/' | b'{')
                {
                    i += 1;
                }
                if i == start {
                    i += 1;
                    continue;
                }
                out.push((start, i, HighlightScope::Attribute));
                let mut equals = i;
                while equals < bytes.len() && is_space(bytes[equals]) {
                    equals += 1;
                }
                if bytes.get(equals) != Some(&b'=') {
                    continue;
                }
                i = equals + 1;
                while i < bytes.len() && is_space(bytes[i]) {
                    i += 1;
                }
                match bytes.get(i) {
                    Some(&quote @ (b'"' | b'\'')) => {
                        let Some(close) = tag[i + 1..].find(quote as char) else {
                            break;
                        };
                        let end = i + 1 + close + 1;
                        out.push((i, end, HighlightScope::String));
                        i = end;
                    }
                    Some(b'{') => match skip_braces(i) {
                        Some(next) => i = next,
                        None => break,
                    },
                    Some(_) => {
                        let value = i;
                        while i < bytes.len() && !is_space(bytes[i]) && bytes[i] != b'>' {
                            i += 1;
                        }
                        out.push((value, i, HighlightScope::String));
                    }
                    None => {}
                }
            }
        }
    }
    if terminated {
        (out, i)
    } else {
        (Vec::new(), 1)
    }
}

/// Block keywords that precede the JavaScript of a Svelte braced construct.
/// `:else if` comes before `:else` so the longer form wins.
const SVELTE_KEYWORDS: [&str; 18] = [
    "#each", "#if", "#key", "#snippet", "#await", ":else if", ":else", ":then", ":catch", "/each",
    "/if", "/key", "/snippet", "/await", "@html", "@const", "@render", "@debug",
];

/// Byte range of the block keyword (`@render`, `#if`) that starts `text`.
fn svelte_keyword(text: &str) -> Option<std::ops::Range<usize>> {
    let start = text.len() - text.trim_start().len();
    let rest = &text[start..];
    SVELTE_KEYWORDS
        .iter()
        .find(|keyword| {
            rest.strip_prefix(**keyword)
                .is_some_and(|tail| !tail.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
        })
        .map(|keyword| start..start + keyword.len())
}

/// Returns the JS expression within a Svelte braced construct. Prefixes are
/// markup syntax; the remainder is parsed by the JavaScript grammar.
fn svelte_expression(text: &str) -> &str {
    let Some(keyword) = svelte_keyword(text) else {
        return text.trim_start();
    };
    let rest = text[keyword.end..].trim_start();
    if &text[keyword] == "#each" {
        return rest
            .split_once(" as ")
            .map_or(rest, |(expression, _)| expression.trim_end());
    }
    rest
}

/// Byte ranges, within the braces' text, of the parts after `as` in `{#each}`.
struct SvelteEach {
    as_keyword: std::ops::Range<usize>,
    binding: std::ops::Range<usize>,
    key: Option<std::ops::Range<usize>>,
}

/// Splits `#each items as item, index (key)` into its `as` keyword, binding
/// pattern and parenthesised key. `None` for other constructs or without `as`.
fn svelte_each_parts(expr: &str) -> Option<SvelteEach> {
    let keyword = svelte_keyword(expr)?;
    if &expr[keyword.clone()] != "#each" {
        return None;
    }
    let as_at = expr[keyword.end..].find(" as ")? + keyword.end + 1;
    let tail_start = as_at + 2;
    let tail = expr[tail_start..].trim_end();
    let lead = tail.len() - tail.trim_start().len();
    let binding_start = tail_start + lead;
    let mut binding_end = tail_start + tail.len();
    let mut key = None;
    if tail.ends_with(')') {
        let mut depth = 0usize;
        let open = tail
            .bytes()
            .enumerate()
            .rev()
            .find_map(|(i, byte)| match byte {
                b')' => {
                    depth += 1;
                    None
                }
                b'(' => {
                    depth -= 1;
                    (depth == 0).then_some(i)
                }
                _ => None,
            });
        if let Some(open) = open
            && tail_start + open > binding_start
        {
            key = Some(tail_start + open..tail_start + tail.len());
            binding_end = tail_start + open;
        }
    }
    let binding_end = binding_start + expr[binding_start..binding_end].trim_end().len();
    (binding_start < binding_end).then_some(SvelteEach {
        as_keyword: as_at..as_at + 2,
        binding: binding_start..binding_end,
        key,
    })
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
        Self::with_mask(language, None)
    }

    /// The service for `language`. Blade is parsed as HTML with its own
    /// constructs blanked out (see `blade::mask`), because `->` or `!=` in
    /// `@if (...)` otherwise breaks the HTML tree for the rest of the file.
    pub fn for_syntax(language: SyntaxLanguage) -> Self {
        let mask: Option<TextMask> = (language == SyntaxLanguage::Blade)
            .then_some(crate::php::framework::laravel::blade::mask as TextMask);
        Self::with_mask(language.grammar(), mask)
    }

    fn with_mask(language: Language, mask: Option<TextMask>) -> Self {
        let (request_tx, request_rx) = mpsc::channel::<Request>();
        let (result_tx, result_rx) = mpsc::channel::<SyntaxSnapshot>();
        let latest_requested = Arc::new(AtomicU64::new(0));
        let latest_for_worker = Arc::clone(&latest_requested);
        let worker = thread::Builder::new()
            .name("galaxy-syntax".into())
            .spawn(move || run(language, mask, request_rx, result_tx, latest_for_worker))
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

/// Rewrites document text before it is parsed. It must keep the byte length
/// and line breaks, so tree offsets stay offsets of the document.
type TextMask = fn(&str) -> String;

fn run(
    language: Language,
    mask: Option<TextMask>,
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
        // A masked text differs from the document's in places the edit does
        // not describe, so it is always parsed from scratch.
        let text = match mask {
            Some(mask) => Rope::from_str(&mask(&request.text.to_string())),
            None => request.text,
        };
        let incremental = mask.is_none()
            && matches!(
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

    fn is_bracket_scope(scope: HighlightScope) -> bool {
        matches!(
            scope,
            HighlightScope::PunctuationBracket1
                | HighlightScope::PunctuationBracket2
                | HighlightScope::PunctuationBracket3
                | HighlightScope::PunctuationBracket4
        )
    }

    fn bracket_count(runs: &[StyleRun], source: &str, token: &str) -> usize {
        runs.iter()
            .filter(|run| is_bracket_scope(run.scope) && &source[run.start..run.end] == token)
            .count()
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
            (
                SyntaxLanguage::Xml,
                "<?xml version=\"1.0\"?><main id=\"galaxy\"/>",
            ),
            (SyntaxLanguage::Css, "main { color: red; }"),
            (SyntaxLanguage::Bash, "#!/usr/bin/env bash\necho Galaxy"),
            (
                SyntaxLanguage::Nginx,
                "server {\n    listen 80;\n    location / {\n        root /var/www;\n    }\n}\n",
            ),
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
    fn every_bundled_highlight_query_has_a_theme_scope() {
        for language in SyntaxLanguage::ALL {
            let query = language.compile_highlights().unwrap();
            assert!(
                query
                    .capture_names()
                    .iter()
                    .any(|capture| HighlightScope::from_capture(capture).is_some()),
                "{} has no captures mapped to the shared theme palette",
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
    fn typescript_arrow_function_parentheses_are_punctuation() {
        let language = SyntaxLanguage::TypeScript;
        let source = "const task = async () => {};\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 1);
        assert_eq!(bracket_count(&runs, source, "("), 1);
        assert_eq!(bracket_count(&runs, source, ")"), 1);
        assert!(
            runs.iter().all(|run| run.scope != HighlightScope::Function),
            "an anonymous arrow function must not overwrite its delimiters"
        );
    }

    #[test]
    fn env_values_are_classified_by_type() {
        let language = SyntaxLanguage::Ini;
        let source = "APP_DEBUG=true\nPORT=8080\nRATIO=-1.5\nMODE=OFF\nNAME=\"galaxy app\"\nURL=http://localhost:8000\nEMPTY=\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 7);
        let scopes = |line: usize, token: &str| {
            let text = source.lines().nth(line).unwrap();
            let start = text.find(token).unwrap();
            runs.iter()
                .filter(|run| {
                    run.line == line && run.start == start && run.end == start + token.len()
                })
                .map(|run| run.scope)
                .collect::<Vec<_>>()
        };
        assert_eq!(scopes(0, "true"), vec![HighlightScope::Constant]);
        assert_eq!(scopes(1, "8080"), vec![HighlightScope::Number]);
        assert_eq!(scopes(2, "-1.5"), vec![HighlightScope::Number]);
        assert_eq!(scopes(3, "OFF"), vec![HighlightScope::Constant]);
        assert_eq!(scopes(4, "\"galaxy app\""), vec![HighlightScope::String]);
        assert_eq!(
            scopes(5, "http://localhost:8000"),
            vec![HighlightScope::String]
        );
        // Keys stay properties.
        assert_eq!(scopes(1, "PORT"), vec![HighlightScope::Property]);
    }

    #[test]
    fn css_bare_values_are_constants() {
        let language = SyntaxLanguage::Css;
        let source = ".card { display: block; position: relative; border: none; color: white; font-weight: bold; }\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 1);
        for value in ["block", "relative", "none", "white", "bold"] {
            let start = source.find(value).unwrap();
            assert!(
                runs.iter().any(|run| {
                    run.start == start
                        && run.end == start + value.len()
                        && run.scope == HighlightScope::Constant
                }),
                "{value} is not a CSS constant"
            );
        }
    }

    /// A tree one edit behind the text can hand back offsets inside a
    /// multi-byte character (crash 2026-11-16 10:04, deleting a syntax error).
    #[test]
    fn stale_offsets_inside_a_character_do_not_panic() {
        let rope = Rope::from_str("é(x)");
        // Byte 1 sits inside `é` (bytes 0..2).
        let mut spans = vec![
            (1, 2, HighlightScope::Punctuation),
            (2, 3, HighlightScope::Punctuation),
            (4, 5, HighlightScope::Punctuation),
        ];
        rainbow_brackets(&rope, &mut spans, &[]);
        assert_eq!(
            spans[0].2,
            HighlightScope::Punctuation,
            "split token is left alone"
        );
        assert_eq!(spans[1].2, HighlightScope::PunctuationBracket1);

        // The same edit through the viewport: parse, then change the text
        // without re-parsing so the old tree's offsets are stale.
        let language = SyntaxLanguage::Tsx;
        let old = "const a = f(1));\nconst b = 'é';\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(old, None).unwrap();
        let doc = Document::from_text("const a = f(1);\nconst b = 'éé(';\n");
        let _ = style_runs_for_viewport(&doc, &tree, language, 0, doc.line_count());
    }

    /// Every bracket of a JSX tag has a color; `</` and `/>` used to be plain.
    #[test]
    fn jsx_tag_brackets_are_all_colored() {
        for language in [SyntaxLanguage::Tsx, SyntaxLanguage::JavaScript] {
            let src = "const v = (\n<div>\n<Plus className=\"m\" />\n</div>\n);\n";
            let runs = runs_for(language, src);
            for (line, token) in [(1, "<"), (1, ">"), (2, "<"), (2, "/>"), (3, "</"), (3, ">")] {
                assert!(
                    scopes_of(&runs, src, line, token).contains(&HighlightScope::Operator),
                    "{language:?} line {line} {token}: {runs:?}"
                );
            }
        }
    }

    /// `Operations & Automation` in JSX is text: the words after `&` must
    /// not take the variable or operator color.
    #[test]
    fn bare_ampersand_in_jsx_text_stays_text() {
        for language in [SyntaxLanguage::Tsx, SyntaxLanguage::JavaScript] {
            let src = "const v = (\n<span className=\"x\">\nOperations & Automation\n</span>\n);\n";
            let runs = runs_for(language, src);
            let text = scopes_of(&runs, src, 2, "Operations");
            assert_eq!(scopes_of(&runs, src, 2, "&"), text, "{language:?} {runs:?}");
            assert_eq!(
                scopes_of(&runs, src, 2, "Automation"),
                text,
                "{language:?} {runs:?}"
            );
            // Only the text loses its captures; the tags around it keep theirs.
            assert!(!scopes_of(&runs, src, 1, "span").is_empty(), "{runs:?}");
            assert!(!scopes_of(&runs, src, 3, "span").is_empty(), "{runs:?}");
        }
    }

    /// `className={`a ${x}`}`: `${` pairs with the template's `}`, so the
    /// outer `}` takes the color of the JSX `{`.
    #[test]
    fn template_substitution_brackets_pair_with_their_own_close() {
        for language in [SyntaxLanguage::Tsx, SyntaxLanguage::JavaScript] {
            let src = "const v = <div className={`a b ${\n  s === 'open'\n    ? 'x'\n    : 'y'\n}`}>t</div>;\n";
            let runs = runs_for(language, src);
            let scope_at = |line: usize, start: usize, end: usize| {
                runs.iter()
                    .filter(|r| r.line == line && r.start == start && r.end == end)
                    .map(|r| r.scope)
                    .collect::<Vec<_>>()
            };
            let outer_open = scope_at(0, 25, 26);
            let substitution_open = scope_at(0, 31, 33);
            assert_eq!(
                outer_open,
                [HighlightScope::PunctuationBracket1],
                "{language:?} {runs:?}"
            );
            assert_eq!(
                substitution_open,
                [HighlightScope::PunctuationBracket2],
                "{language:?} {runs:?}"
            );
            assert_eq!(
                scope_at(4, 0, 1),
                substitution_open,
                "{language:?} {runs:?}"
            );
            assert_eq!(scope_at(4, 2, 3), outer_open, "{language:?} {runs:?}");
        }
    }

    fn runs_for(language: SyntaxLanguage, source: &str) -> Vec<StyleRun> {
        let service = SyntaxService::for_syntax(language);
        let doc = Document::from_text(source);
        service.submit_document(&doc);
        let snapshot = wait_for(&service);
        style_runs_for_viewport(&doc, snapshot.tree(), language, 0, doc.line_count())
    }

    #[test]
    fn component_tags_take_the_class_color() {
        let tsx = "const v = <div>\n<UserCard id={1} />\n<Form.Item>x</Form.Item>\n</div>;\n";
        let runs = runs_for(SyntaxLanguage::Tsx, tsx);
        assert!(
            scopes_of(&runs, tsx, 1, "UserCard").contains(&HighlightScope::Type),
            "{runs:?}"
        );
        assert!(
            scopes_of(&runs, tsx, 2, "Item").contains(&HighlightScope::Type),
            "{runs:?}"
        );
        assert!(!scopes_of(&runs, tsx, 0, "div").contains(&HighlightScope::Type));

        let jsx = "const v = (\n<Modal open>\n<span>a</span>\n</Modal>\n);\n";
        let runs = runs_for(SyntaxLanguage::JavaScript, jsx);
        assert!(
            scopes_of(&runs, jsx, 1, "Modal").contains(&HighlightScope::Type),
            "{runs:?}"
        );
        assert!(
            scopes_of(&runs, jsx, 3, "Modal").contains(&HighlightScope::Type),
            "{runs:?}"
        );
        assert!(!scopes_of(&runs, jsx, 2, "span").contains(&HighlightScope::Type));

        let vue = "<template>\n<div>\n<UserCard />\n<user-card></user-card>\n</div>\n</template>\n";
        let runs = runs_for(SyntaxLanguage::Vue, vue);
        assert!(
            scopes_of(&runs, vue, 2, "UserCard").contains(&HighlightScope::Type),
            "{runs:?}"
        );
        assert!(
            scopes_of(&runs, vue, 3, "user-card").contains(&HighlightScope::Type),
            "{runs:?}"
        );
        assert!(scopes_of(&runs, vue, 1, "div").contains(&HighlightScope::Tag));

        let svelte = "<script>\nlet a = 1;\n</script>\n<div>\n<UserCard {a} />\n</div>\n";
        let runs = runs_for(SyntaxLanguage::Svelte, svelte);
        assert!(
            scopes_of(&runs, svelte, 4, "UserCard").contains(&HighlightScope::Type),
            "{runs:?}"
        );
        assert!(scopes_of(&runs, svelte, 3, "div").contains(&HighlightScope::Tag));
    }

    /// Runs of a Blade `source` as the editor sees them: the worker parses the
    /// masked text and the viewport adds the Blade overlay on top.
    fn blade_runs(source: &str) -> Vec<StyleRun> {
        let language = SyntaxLanguage::Blade;
        let service = SyntaxService::for_syntax(language);
        let doc = Document::from_text(source);
        service.submit_document(&doc);
        let snapshot = wait_for(&service);
        style_runs_for_viewport(&doc, snapshot.tree(), language, 0, doc.line_count())
    }

    fn scopes_of(runs: &[StyleRun], source: &str, line: usize, token: &str) -> Vec<HighlightScope> {
        let text = source.lines().nth(line).unwrap();
        let start = text.find(token).unwrap();
        runs.iter()
            .filter(|r| r.line == line && r.start <= start && r.end >= start + token.len())
            .map(|r| r.scope)
            .collect()
    }

    #[test]
    fn blade_conditions_do_not_break_html_below() {
        let source = "<div>\n@if (session('a')->role != 'x' && $b->c != 'y')\n<div class=\"col\">\n<label>Name</label>\n</div>\n@endif\n</div>\n";
        let runs = blade_runs(source);
        assert!(scopes_of(&runs, source, 2, "div").contains(&HighlightScope::Tag));
        assert!(scopes_of(&runs, source, 2, "class").contains(&HighlightScope::Attribute));
        assert!(scopes_of(&runs, source, 3, "label").contains(&HighlightScope::Tag));
    }

    #[test]
    fn blade_style_and_script_blocks_get_injected_styles() {
        let source = "@push('js')\n<style>\n.card { color: #FFF; padding: 0 12px; }\n</style>\n<script>\nconst u = '{{ route(\"a.b\") }}';\nfunction go() { return 1; }\n</script>\n@endpush\n";
        let runs = blade_runs(source);
        assert!(scopes_of(&runs, source, 2, "padding").contains(&HighlightScope::Property));
        assert!(scopes_of(&runs, source, 2, "12px").contains(&HighlightScope::Number));
        assert!(scopes_of(&runs, source, 6, "function").contains(&HighlightScope::Keyword));
        assert!(scopes_of(&runs, source, 6, "1").contains(&HighlightScope::Number));
        // The Blade echo inside the script keeps its Blade colors.
        assert!(scopes_of(&runs, source, 5, "route").contains(&HighlightScope::Function));
    }

    #[test]
    fn php_function_parentheses_are_punctuation() {
        let language = SyntaxLanguage::Php;
        let source = "<?php function noArgs() {} function withArgs($value) {}\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 1);
        assert_eq!(bracket_count(&runs, source, "("), 2);
        assert_eq!(bracket_count(&runs, source, ")"), 2);
    }

    #[test]
    fn php_brackets_and_logical_operators_are_highlighted() {
        let language = SyntaxLanguage::Php;
        let source = "<?php $value = $a instanceof BillingInterval ? $a->value : null; $items = [$a]; $a?->next()->value; C::make(); $i++; --$j; $mask <<= 1; $result ??= $value; if ($a && $b) {}\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 1);
        let scope_at = |token: &str| {
            let start = source.find(token).unwrap();
            runs.iter()
                .filter(|run| run.start == start && run.end == start + token.len())
                .map(|run| run.scope)
                .collect::<Vec<_>>()
        };
        let scope_at_end = |token: &str| {
            let start = source.rfind(token).unwrap();
            runs.iter()
                .filter(|run| run.start == start && run.end == start + token.len())
                .map(|run| run.scope)
                .collect::<Vec<_>>()
        };
        let ternary_question = source.find(" ? ").unwrap() + 1;
        let ternary_colon = source.find(" : ").unwrap() + 1;

        assert!(scope_at("&&").contains(&HighlightScope::Operator));
        assert!(runs.iter().any(|run| {
            run.start == ternary_question
                && run.end == ternary_question + 1
                && run.scope == HighlightScope::Punctuation
        }));
        assert!(runs.iter().any(|run| {
            run.start == ternary_colon
                && run.end == ternary_colon + 1
                && run.scope == HighlightScope::Punctuation
        }));
        assert!(scope_at("[").iter().any(|scope| is_bracket_scope(*scope)));
        assert!(scope_at("]").iter().any(|scope| is_bracket_scope(*scope)));
        assert!(scope_at("?->").contains(&HighlightScope::Punctuation));
        assert!(scope_at_end("->").contains(&HighlightScope::Punctuation));
        assert!(scope_at("::").contains(&HighlightScope::Punctuation));
        assert!(scope_at("++").contains(&HighlightScope::Punctuation));
        assert!(scope_at("--").contains(&HighlightScope::Punctuation));
        assert!(scope_at("BillingInterval").contains(&HighlightScope::Type));
        assert!(scope_at("<<=").contains(&HighlightScope::Operator));
        assert!(scope_at("??=").contains(&HighlightScope::Operator));
    }

    #[test]
    fn php_unset_statement_uses_builtin_function_scope() {
        let language = SyntaxLanguage::Php;
        let source = "<?php unset($items[$offset]);\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 1);

        assert!(runs.iter().any(|run| {
            &source[run.start..run.end] == "unset" && run.scope == HighlightScope::FunctionBuiltin
        }));
    }

    #[test]
    fn php_enum_cases_use_constant_scope() {
        let language = SyntaxLanguage::Php;
        let source = "<?php enum IntervalBoundary { case ClosedOpen; }\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 1);
        let start = source.find("ClosedOpen").unwrap();

        assert!(runs.iter().any(|run| {
            run.start == start
                && run.end == start + "ClosedOpen".len()
                && run.scope == HighlightScope::Constant
        }));
    }

    #[test]
    fn javascript_optional_properties_and_chain_are_highlighted() {
        let language = SyntaxLanguage::JavaScript;
        let source = "const interval = bill?.subscriptionPenaltySettings; bill.subscriptionPenaltySettings;\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 1);
        let scope_at = |token: &str| {
            let start = source.find(token).unwrap();
            runs.iter()
                .filter(|run| run.start == start && run.end == start + token.len())
                .map(|run| run.scope)
                .collect::<Vec<_>>()
        };

        let dot_start = source.rfind('.').unwrap();
        let dot_scopes = runs
            .iter()
            .filter(|run| run.start == dot_start && run.end == dot_start + 1)
            .map(|run| run.scope)
            .collect::<Vec<_>>();
        assert_eq!(scope_at("?."), dot_scopes);
        assert!(scope_at("subscriptionPenaltySettings").contains(&HighlightScope::Property));
    }

    #[test]
    fn javascript_export_default_is_keyword() {
        let language = SyntaxLanguage::JavaScript;
        let source = "export default config;\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 1);
        let start = source.find("default").unwrap();
        assert!(
            runs.iter().any(|run| {
                run.start == start
                    && run.end == start + "default".len()
                    && run.scope == HighlightScope::Keyword
            }),
            "default in export default must use the keyword color"
        );
    }

    #[test]
    fn typescript_and_javascript_operators_and_delimiters_are_scoped() {
        for language in [SyntaxLanguage::TypeScript, SyntaxLanguage::JavaScript] {
            let source = "const value = flag ? left : right; count <<= 1; value ??= fallback; ready &&= true; const map = (item) => item ** 2; require(\"path\");\n";
            let mut parser = Parser::new();
            parser.set_language(&language.grammar()).unwrap();
            let tree = parser.parse(source, None).unwrap();
            let doc = Document::from_text(source);
            let runs = style_runs_for_viewport(&doc, &tree, language, 0, 1);
            let scope_at = |token: &str| {
                let start = source.find(token).unwrap();
                runs.iter()
                    .filter(|run| run.start == start && run.end == start + token.len())
                    .map(|run| run.scope)
                    .collect::<Vec<_>>()
            };

            for token in ["=", "<<=", "??=", "&&=", "**"] {
                assert!(
                    scope_at(token).contains(&HighlightScope::Operator),
                    "{language:?} {token} must be an operator"
                );
            }
            for token in ["?", ":", "=>"] {
                assert!(
                    scope_at(token).contains(&HighlightScope::Punctuation),
                    "{language:?} {token} must be punctuation"
                );
            }
            assert!(scope_at("require").contains(&HighlightScope::FunctionBuiltin));
        }
    }

    #[test]
    fn typescript_import_require_and_type_punctuation_are_scoped() {
        let language = SyntaxLanguage::TypeScript;
        let source = "import path = require(\"path\");\ntype Callback = (...args: any[]) => any;\ntype Value<T> = T extends string ? T : never;\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 3);
        let scope_at = |line: usize, start: usize, token: &str| {
            runs.iter()
                .filter(|run| {
                    run.line == line && run.start == start && run.end == start + token.len()
                })
                .map(|run| run.scope)
                .collect::<Vec<_>>()
        };

        let require = source.lines().next().unwrap().find("require").unwrap();
        assert!(scope_at(0, require, "require").contains(&HighlightScope::FunctionBuiltin));
        for (line, token) in [(1, "..."), (2, "?"), (2, ":")] {
            let start = source.lines().nth(line).unwrap().rfind(token).unwrap();
            assert!(
                scope_at(line, start, token).contains(&HighlightScope::Punctuation),
                "{token} must be punctuation"
            );
        }
    }

    #[test]
    fn rainbow_color_does_not_depend_on_where_the_window_starts() {
        let language = SyntaxLanguage::TypeScript;
        let mut source = String::from("const items = [\n");
        for i in 0..60 {
            source.push_str(&format!("  (item{i}),\n"));
        }
        source.push_str("];\n");
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(&source, None).unwrap();
        let doc = Document::from_text(&source);
        let whole = style_runs_for_viewport(&doc, &tree, language, 0, doc.line_count());
        let brackets = |runs: &[StyleRun], line: usize| {
            runs.iter()
                .filter(|run| run.line == line && matches!(run.start, 2 | 9 | 10))
                .map(|run| (run.start, run.scope))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            brackets(&whole, 40)[0],
            (2, HighlightScope::PunctuationBracket2)
        );
        // This window starts at line 30, below the `[` on line 0.
        let part = style_runs_for_viewport(&doc, &tree, language, 40, 3);
        for line in [30, 40, 41, 42, 43] {
            assert_eq!(brackets(&part, line), brackets(&whole, line), "line {line}");
        }
    }

    /// Rainbow depth is counted from the start of the file, so a line has the
    /// same bracket colors whichever window is asked for.
    #[test]
    fn rainbow_colors_match_the_whole_file_in_every_language() {
        // (language, text before the repeated lines, a repeated line, text after)
        let samples: &[(SyntaxLanguage, &str, &str, &str)] = &[
            (
                SyntaxLanguage::Rust,
                "fn main() {\n    let v = vec![\n",
                "        (a{i}, [b]),\n",
                "    ];\n}\n",
            ),
            (
                SyntaxLanguage::TypeScript,
                "const a = [\n",
                "  (a{i}, [b]),\n",
                "];\n",
            ),
            (
                SyntaxLanguage::Tsx,
                "const a = [\n",
                "  (a{i}, [b]),\n",
                "];\n",
            ),
            (
                SyntaxLanguage::JavaScript,
                "const a = [\n",
                "  (a{i}, [b]),\n",
                "];\n",
            ),
            (
                SyntaxLanguage::Json,
                "{\"a\": [\n",
                "  [{i}, {\"b\": [1]}],\n",
                "  []\n]}\n",
            ),
            (SyntaxLanguage::Toml, "a = [\n", "  [{i}, [2]],\n", "]\n"),
            (SyntaxLanguage::Yaml, "a: [\n", "  [{i}, [2]],\n", "]\n"),
            (
                SyntaxLanguage::Markdown,
                "# T\n\n",
                "text (a{i} [b]) done\n\n",
                "end\n",
            ),
            (
                SyntaxLanguage::Python,
                "x = [\n",
                "    (a{i}, [b]),\n",
                "]\n",
            ),
            (
                SyntaxLanguage::Go,
                "var x = []int{\n",
                "\t(a{i}), [1][0],\n",
                "}\n",
            ),
            (
                SyntaxLanguage::Html,
                "<div>\n",
                "<p title=\"(x)\">(a{i}) [b]</p>\n",
                "</div>\n",
            ),
            (
                SyntaxLanguage::Xml,
                "<a>\n",
                "<b k=\"(x)\">(a{i}) [b]</b>\n",
                "</a>\n",
            ),
            (
                SyntaxLanguage::Css,
                "@media (min-width: 1px) {\n",
                "  a{i} { b: calc((1px) + (2px)); }\n",
                "}\n",
            ),
            (
                SyntaxLanguage::Bash,
                "f() {\n",
                "  (echo a{i}) ; [ -f x ] && { echo; }\n",
                "}\n",
            ),
            (
                SyntaxLanguage::Nginx,
                "http {\n",
                "  server {\n    listen {i};\n  }\n",
                "}\n",
            ),
            (
                SyntaxLanguage::Php,
                "<?php\n$a = [\n",
                "    (a{i}, [b]),\n",
                "];\n",
            ),
            (
                SyntaxLanguage::C,
                "int a[] = {\n",
                "  (a{i}), [1][0],\n",
                "};\n",
            ),
            (
                SyntaxLanguage::Cpp,
                "int a[] = {\n",
                "  (a{i}), [1][0],\n",
                "};\n",
            ),
            (
                SyntaxLanguage::CSharp,
                "class A {\n  int[] a = new int[] {\n",
                "    (a{i}), [1][0],\n",
                "  };\n}\n",
            ),
            (
                SyntaxLanguage::Java,
                "class A {\n  int[] a = new int[] {\n",
                "    (a{i}), [1][0],\n",
                "  };\n}\n",
            ),
            (SyntaxLanguage::Ruby, "a = [\n", "  (a{i}), [b],\n", "]\n"),
            (
                SyntaxLanguage::Lua,
                "local a = {\n",
                "  (a{i}), t[1], f(g(2)),\n",
                "}\n",
            ),
            (
                SyntaxLanguage::Swift,
                "let a = [\n",
                "  (a{i}), [1][0],\n",
                "]\n",
            ),
            (
                SyntaxLanguage::Sql,
                "select (\n",
                "  (a{i}) + (select [1]),\n",
                "  1\n) from t;\n",
            ),
            (
                SyntaxLanguage::Make,
                "a = $(foo\n",
                "  $(bar {i}) $(baz)\n",
                ")\n",
            ),
            (
                SyntaxLanguage::Ini,
                "[sec]\n",
                "k{i} = (a) [b] {c}\n",
                "z = 1\n",
            ),
            (
                SyntaxLanguage::PowerShell,
                "$a = @(\n",
                "  (a{i}), @{k=1},\n",
                ")\n",
            ),
            (
                SyntaxLanguage::Dart,
                "var a = [\n",
                "  (a{i}), [1][0],\n",
                "];\n",
            ),
            (
                SyntaxLanguage::R,
                "a <- c(\n",
                "  (a{i}), list(1)[[1]],\n",
                "  2\n)\n",
            ),
            (
                SyntaxLanguage::Blade,
                "<div>\n@foreach ($items as $i)\n",
                "  <p>{{ ($a{i}) }} (x) [y]</p>\n",
                "@endforeach\n</div>\n",
            ),
            (
                SyntaxLanguage::Vue,
                "<script>\nconst a = [\n",
                "  (a{i}), [1][0],\n",
                "]\n</script>\n",
            ),
            (
                SyntaxLanguage::Svelte,
                "<script>\n  let a = [\n",
                "    (a{i}), [1][0],\n",
                "  ]\n</script>\n",
            ),
        ];
        assert_eq!(
            samples.len(),
            SyntaxLanguage::ALL.len(),
            "a sample for every language"
        );
        let is_bracket_scope = |scope: HighlightScope| {
            matches!(
                scope,
                HighlightScope::Punctuation
                    | HighlightScope::PunctuationBracket1
                    | HighlightScope::PunctuationBracket2
                    | HighlightScope::PunctuationBracket3
                    | HighlightScope::PunctuationBracket4
            )
        };
        let mut failures = Vec::new();
        for (language, head, line, tail) in samples {
            let mut source = String::from(*head);
            for i in 0..80 {
                source.push_str(&line.replace("{i}", &i.to_string()));
            }
            source.push_str(tail);
            let mut parser = Parser::new();
            parser.set_language(&language.grammar()).unwrap();
            let tree = parser.parse(&source, None).unwrap();
            let doc = Document::from_text(&source);
            let line_count = doc.line_count();
            let whole = style_runs_for_viewport(&doc, &tree, *language, 0, line_count);
            let mut checked_brackets = 0;
            for start in (0..line_count).step_by(5) {
                let part = style_runs_for_viewport(&doc, &tree, *language, start, 3);
                let first = start.saturating_sub(VIEWPORT_STYLE_MARGIN_LINES);
                let last = (start + 3 + VIEWPORT_STYLE_MARGIN_LINES).min(line_count);
                let pick = |runs: &[StyleRun]| {
                    runs.iter()
                        .filter(|run| {
                            (first..last).contains(&run.line) && is_bracket_scope(run.scope)
                        })
                        .copied()
                        .collect::<Vec<_>>()
                };
                let (expected, got) = (pick(&whole), pick(&part));
                checked_brackets += expected
                    .iter()
                    .filter(|run| run.scope != HighlightScope::Punctuation)
                    .count();
                if expected != got {
                    let differs = expected.iter().find(|run| !got.contains(run)).map(|run| {
                        let text = doc.rope().line(run.line).to_string();
                        format!(
                            "line {} col {} {:?} in {:?}",
                            run.line, run.start, run.scope, text
                        )
                    });
                    failures.push(format!(
                        "{} (window at line {start}): {differs:?}",
                        language.name()
                    ));
                    break;
                }
            }
            eprintln!(
                "{}: {checked_brackets} colored brackets compared",
                language.name()
            );
        }
        assert!(
            failures.is_empty(),
            "different colors than the whole file: {failures:?}"
        );
    }

    #[test]
    fn nested_brackets_use_matching_rainbow_scopes() {
        let language = SyntaxLanguage::TypeScript;
        let source = "const value = ({ items: [call(\"(plain)\")] });\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 1);
        let brackets = runs
            .iter()
            .filter_map(|run| match &source[run.start..run.end] {
                "(" | ")" | "[" | "]" | "{" | "}" => Some((&source[run.start..run.end], run.scope)),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            brackets,
            vec![
                ("(", HighlightScope::PunctuationBracket1),
                ("{", HighlightScope::PunctuationBracket2),
                ("[", HighlightScope::PunctuationBracket3),
                ("(", HighlightScope::PunctuationBracket4),
                (")", HighlightScope::PunctuationBracket4),
                ("]", HighlightScope::PunctuationBracket3),
                ("}", HighlightScope::PunctuationBracket2),
                (")", HighlightScope::PunctuationBracket1),
            ]
        );
        assert!(runs.iter().all(|run| {
            &source[run.start..run.end] != "(plain)" || run.scope == HighlightScope::String
        }));
    }

    #[test]
    fn json_brackets_and_colons_are_highlighted() {
        let language = SyntaxLanguage::Json;
        let source = "{\"items\": [true]}\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 1);
        let scope_at = |token: &str| {
            let start = source.find(token).unwrap();
            runs.iter()
                .filter(|run| run.start == start && run.end == start + token.len())
                .map(|run| run.scope)
                .collect::<Vec<_>>()
        };

        assert!(scope_at("{").iter().any(|scope| is_bracket_scope(*scope)));
        assert_eq!(
            scope_at("\"items\"").last(),
            Some(&HighlightScope::PropertyJson)
        );
        assert!(scope_at(":").contains(&HighlightScope::Operator));
        assert!(scope_at("[").iter().any(|scope| is_bracket_scope(*scope)));
        assert!(scope_at("]").iter().any(|scope| is_bracket_scope(*scope)));
    }

    #[test]
    fn typescript_regex_is_a_string() {
        let language = SyntaxLanguage::TypeScript;
        let source = "const pattern = /[a-z]+/gi;\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 1);

        assert!(runs.iter().any(|run| {
            &source[run.start..run.end] == "/[a-z]+/gi" && run.scope == HighlightScope::String
        }));
    }

    #[test]
    fn typescript_query_highlights_abstract_as_keyword() {
        let language = SyntaxLanguage::TypeScript;
        let source = "export declare abstract class Chainable {}\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let query = language.compile_highlights().unwrap();
        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());

        assert!(matches.any(|m| m.captures.iter().any(|capture| {
            &source[capture.node.byte_range()] == "abstract"
                && HighlightScope::from_capture(names[capture.index as usize])
                    == Some(HighlightScope::Keyword)
        })));
    }

    #[test]
    fn typescript_query_highlights_abstract_constructor_type_as_keyword() {
        let language = SyntaxLanguage::TypeScript;
        let source =
            "type Constructor<T> = T extends abstract new (...args: any[]) => any ? T : never;\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 1);
        let start = source.find("abstract").unwrap();

        assert!(runs.iter().any(|run| {
            run.start == start
                && run.end == start + "abstract".len()
                && run.scope == HighlightScope::Keyword
        }));
    }

    #[test]
    fn typescript_query_highlights_superclasses_as_types() {
        let language = SyntaxLanguage::TypeScript;
        let source = "class DatabaseQueryBuilder extends Chainable {}\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let query = language.compile_highlights().unwrap();
        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());

        assert!(matches.any(|m| m.captures.iter().any(|capture| {
            &source[capture.node.byte_range()] == "Chainable"
                && HighlightScope::from_capture(names[capture.index as usize])
                    == Some(HighlightScope::Type)
        })));
    }

    /// Mirrors Inertia's `router.d.ts`: destructured parameters and accessor
    /// keywords were left unstyled. A method named `get` stays a function.
    #[test]
    fn typescript_query_highlights_destructured_parameters_and_accessors() {
        let language = SyntaxLanguage::TypeScript;
        let source = "declare class Router {\n    init({ initialPage, onFlash }: P): void;\n    get activePolls(): number;\n    get<T>(url: string): void;\n    set mode(value: string);\n}\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 6);
        let scope = |line: usize, needle: &str| {
            let column = source.lines().nth(line).unwrap().find(needle).unwrap();
            runs.iter()
                .filter(|run| run.line == line && run.start <= column && column < run.end)
                .next_back()
                .map(|run| run.scope)
        };

        assert_eq!(scope(1, "initialPage"), Some(HighlightScope::Variable));
        assert_eq!(scope(1, "onFlash"), Some(HighlightScope::Variable));
        assert_eq!(scope(2, "get"), Some(HighlightScope::Keyword));
        assert_eq!(scope(2, "activePolls"), Some(HighlightScope::Function));
        assert_eq!(scope(3, "get"), Some(HighlightScope::Function));
        assert_eq!(scope(4, "set"), Some(HighlightScope::Keyword));
    }

    /// `title="Stock"`: the quotes take the value's color instead of the plain
    /// foreground, and `declare global` is a keyword like `declare module`.
    #[test]
    fn html_attribute_quotes_and_declare_global_are_colored() {
        let language = SyntaxLanguage::Html;
        let source = "<Head title=\"Stock\" />\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 1);
        let opening = source.find('"').unwrap();
        let closing = source.rfind('"').unwrap();
        for at in [opening, closing] {
            assert!(
                runs.iter().any(|run| run.start <= at
                    && at < run.end
                    && run.scope == HighlightScope::String),
                "quote at {at}: {runs:?}"
            );
        }

        let language = SyntaxLanguage::TypeScript;
        let source = "declare global {\n    var route: string;\n}\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 3);
        let column = source.find("global").unwrap();
        assert!(
            runs.iter().any(|run| run.line == 0
                && run.start <= column
                && column < run.end
                && run.scope == HighlightScope::Keyword),
            "{runs:?}"
        );
    }

    /// `import.meta.env`: `meta` is a keyword like `import`, not plain text.
    #[test]
    fn import_meta_is_highlighted_as_a_keyword() {
        for language in [SyntaxLanguage::TypeScript, SyntaxLanguage::JavaScript] {
            let source = "const a = import.meta.env.VITE;\n";
            let mut parser = Parser::new();
            parser.set_language(&language.grammar()).unwrap();
            let tree = parser.parse(source, None).unwrap();
            let doc = Document::from_text(source);
            let runs = style_runs_for_viewport(&doc, &tree, language, 0, 1);
            let at = source.find("meta").unwrap();
            assert!(
                runs.iter().any(|run| run.start <= at
                    && at < run.end
                    && run.scope == HighlightScope::Keyword),
                "{language:?}: {runs:?}"
            );
        }
    }

    /// Mirrors app/utilities/storage.ts: `new X(...)` colours `X` as a class
    /// and `keyof` as a keyword, matching VS Code.
    #[test]
    fn typescript_query_highlights_constructed_classes_and_keyof() {
        let language = SyntaxLanguage::TypeScript;
        let source = "throw new CustomException('x')\nreturn new Storage(1)\ntype D = { default: keyof K }\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 3);
        let scope = |needle: &str| {
            let offset = source.find(needle).unwrap();
            let prefix = &source[..offset];
            let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
            let column = prefix
                .rsplit_once('\n')
                .map_or(prefix.len(), |(_, tail)| tail.len());
            runs.iter()
                .filter(|run| run.line == line && run.start <= column && column < run.end)
                .next_back()
                .map(|run| run.scope)
        };

        assert_eq!(scope("CustomException"), Some(HighlightScope::Type));
        assert_eq!(scope("Storage"), Some(HighlightScope::Type));
        assert_eq!(scope("keyof"), Some(HighlightScope::Keyword));
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
    fn typescript_final_runs_keep_types_functions_and_properties_colored() {
        let language = SyntaxLanguage::TypeScript;
        let source = "class Session {\n  save(value: string) { return JSON.stringify({ value, active: true }); }\n}\nconst session = new Session();\nsession.save(Object.create(null));\nCdnHelper.generatedUrlsIndexed(session);\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 5);
        let scope_at = |offset: usize| {
            let prefix = &source[..offset];
            let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
            let column = prefix
                .rsplit_once('\n')
                .map_or(prefix.len(), |(_, tail)| tail.len());
            runs.iter()
                .filter(|run| run.line == line && run.start <= column && column < run.end)
                // Renderer applies runs in order; the final overlapping run
                // supplies the visible semantic color.
                .next_back()
                .map(|run| run.scope)
        };
        assert!(runs.iter().any(|run| run.scope == HighlightScope::Type));
        assert!(runs.iter().any(|run| run.scope == HighlightScope::Function));
        assert!(runs.iter().any(|run| run.scope == HighlightScope::Property));
        assert_eq!(
            scope_at(source.find("JSON").unwrap()),
            Some(HighlightScope::Type)
        );
        assert_eq!(
            scope_at(source.find("Object").unwrap()),
            Some(HighlightScope::Type)
        );
        assert_eq!(
            scope_at(source.find("CdnHelper").unwrap()),
            Some(HighlightScope::Type)
        );
    }

    #[test]
    fn typescript_methods_and_boolean_literals_use_semantic_scopes() {
        let language = SyntaxLanguage::TypeScript;
        let source = "Object.defineProperty(target, key, {\n  get() { return true; },\n  set(value) { return false; },\n  serialize() { return true; },\n  hydrate(value) { return false; },\n  enumerable: true,\n  configurable: false,\n});\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 6);
        let scope_at = |needle: &str, occurrence: usize| {
            let offset = source.match_indices(needle).nth(occurrence).unwrap().0;
            let prefix = &source[..offset];
            let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
            let column = prefix
                .rsplit_once('\n')
                .map_or(prefix.len(), |(_, tail)| tail.len());
            runs.iter()
                .filter(|run| run.line == line && run.start <= column && column < run.end)
                .next_back()
                .map(|run| run.scope)
        };

        for (needle, occurrence, expected) in [
            ("serialize", 0, HighlightScope::Function),
            ("hydrate", 0, HighlightScope::Function),
            ("true", 0, HighlightScope::Constant),
            ("false", 0, HighlightScope::Constant),
            ("enumerable", 0, HighlightScope::Property),
        ] {
            assert_eq!(
                scope_at(needle, occurrence),
                Some(expected),
                "{needle}: {runs:?}"
            );
        }
    }

    #[test]
    fn typescript_object_shorthand_values_use_variable_scope() {
        let language = SyntaxLanguage::TypeScript;
        let source = "const trace = {\n  write(type: string, data?: unknown) {\n    return JSON.stringify({ type, data });\n  },\n};\n";
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let doc = Document::from_text(source);
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 5);

        for needle in ["type,", "data }"] {
            let offset = source.find(needle).unwrap();
            let prefix = &source[..offset];
            let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
            let column = prefix
                .rsplit_once('\n')
                .map_or(prefix.len(), |(_, tail)| tail.len());
            let scope = runs
                .iter()
                .filter(|run| run.line == line && run.start <= column && column < run.end)
                .next_back()
                .map(|run| run.scope);
            assert_eq!(scope, Some(HighlightScope::Variable), "{needle}: {runs:?}");
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
            SyntaxLanguage::detect(".env", ""),
            Some(SyntaxLanguage::Ini)
        );
        assert_eq!(
            SyntaxLanguage::detect(".env.production", ""),
            Some(SyntaxLanguage::Ini)
        );
        assert_eq!(
            SyntaxLanguage::detect("phpunit.xml", ""),
            Some(SyntaxLanguage::Xml)
        );
        assert_eq!(
            SyntaxLanguage::detect("composer.lock", ""),
            Some(SyntaxLanguage::Json)
        );
        assert_eq!(
            SyntaxLanguage::detect("artisan", "#!/usr/bin/env php"),
            Some(SyntaxLanguage::Php)
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
    fn dotenv_files_comment_with_a_hash_and_ini_files_with_a_semicolon() {
        let ini = SyntaxLanguage::Ini;
        for path in [
            ".env",
            "/app/.env",
            ".env.local",
            "C:\\app\\.env.production",
        ] {
            assert_eq!(ini.comment_marker_for(path), Some("#"), "{path}");
        }
        for path in ["php.ini", "/etc/php/www.conf", "environment.ini"] {
            assert_eq!(ini.comment_marker_for(path), Some(";"), "{path}");
        }
        assert_eq!(SyntaxLanguage::Rust.comment_marker_for(".env"), Some("//"));
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
    fn php_highlighting_recovers_after_a_missing_semicolon() {
        let source = "<?php\nfunction run($request) {\n    try {\n        Aset::whereHas()\n        for ($i = 0; $i < 2; $i++) {\n            $data = [\n                'available' => $request->available_flag,\n            ];\n            if ($request->desa_id) {\n                $data['provinsi_id'] = $request->provinsi_id;\n            }\n            $aset = Aset::create($data);\n        }\n        DB::commit();\n        return Service::success(\"ok\");\n    } catch (Exception $e) {\n        DB::rollBack();\n    }\n}\n";
        let doc = Document::from_text(source);
        let language = SyntaxLanguage::Php;
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(doc.text(), None).unwrap();
        assert!(tree.root_node().has_error());
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, doc.line_count());
        for (line, token, scope) in [
            (4, "for", HighlightScope::Keyword),
            (6, "'available'", HighlightScope::String),
            (6, "$request", HighlightScope::Variable),
            (6, "available_flag", HighlightScope::Property),
            (8, "if", HighlightScope::Keyword),
            (9, "'provinsi_id'", HighlightScope::String),
            (11, "Aset", HighlightScope::Type),
            (11, "create", HighlightScope::Function),
            (13, "commit", HighlightScope::Function),
            (14, "return", HighlightScope::Keyword),
            (15, "catch", HighlightScope::Keyword),
            (16, "rollBack", HighlightScope::Function),
        ] {
            assert!(
                scopes_of(&runs, source, line, token).contains(&scope),
                "{line}:{token} {runs:?}"
            );
        }
        for (line, token) in [(4, "("), (5, "["), (7, "]"), (8, "{"), (11, "(")] {
            assert!(
                scopes_of(&runs, source, line, token)
                    .iter()
                    .any(|scope| is_bracket_scope(*scope)),
                "{line}:{token} {runs:?}"
            );
        }
        for (line, token) in [(6, "=>"), (7, ";"), (11, ";")] {
            assert!(
                scopes_of(&runs, source, line, token).contains(&HighlightScope::Punctuation),
                "{line}:{token} {runs:?}"
            );
        }

        // Scrolling changes the queried byte window. Recovery must preserve
        // lexer state from the start of the ERROR node, so colors stay stable.
        let scrolled = style_runs_for_viewport(&doc, &tree, language, 16, 2);
        for (line, token, scope) in [
            (6, "'available'", HighlightScope::String),
            (6, "available_flag", HighlightScope::Property),
            (9, "'provinsi_id'", HighlightScope::String),
            (11, "create", HighlightScope::Function),
            (13, "commit", HighlightScope::Function),
            (15, "catch", HighlightScope::Keyword),
            (16, "rollBack", HighlightScope::Function),
        ] {
            assert!(
                scopes_of(&scrolled, source, line, token).contains(&scope),
                "scrolled {line}:{token} {scrolled:?}"
            );
        }
        for (line, token) in [(7, "]"), (8, "{"), (11, "("), (16, "(")] {
            assert!(
                scopes_of(&scrolled, source, line, token)
                    .iter()
                    .any(|scope| is_bracket_scope(*scope)),
                "scrolled {line}:{token} {scrolled:?}"
            );
        }
    }

    #[test]
    fn php_language_constructs_get_builtin_function_scope() {
        let doc = Document::from_text(
            "<?php\nclass C { function __construct() {} }\nif (empty($a) && isset($b)) { strlen($b); \\sprintf('%s', $b); $c->__toString(); C::__debugInfo(); foo(); }\n",
        );
        let language = SyntaxLanguage::Php;
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(doc.text(), None).unwrap();
        let runs = style_runs_for_viewport(&doc, &tree, language, 0, 2);
        let on = |from: usize, to: usize| {
            runs.iter()
                .filter(|r| r.line == 2 && r.start == from && r.end == to)
                .map(|r| r.scope)
                .collect::<Vec<_>>()
        };
        assert!(on(4, 9).contains(&HighlightScope::FunctionBuiltin));
        assert!(on(17, 22).contains(&HighlightScope::FunctionBuiltin));
        assert!(on(30, 36).contains(&HighlightScope::FunctionBuiltin));
        assert!(on(43, 50).contains(&HighlightScope::FunctionBuiltin));
        assert!(on(66, 76).contains(&HighlightScope::FunctionBuiltin));
        assert!(on(83, 94).contains(&HighlightScope::FunctionBuiltin));
        assert!(on(98, 101).contains(&HighlightScope::Function));
        assert!(runs.iter().any(|run| {
            run.line == 1
                && run.start == 19
                && run.end == 30
                && run.scope == HighlightScope::FunctionBuiltin
        }));
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
        assert!(at(2, "@", HighlightScope::Keyword));
        assert!(at(2, "yield", HighlightScope::Keyword));
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
    fn svelte_render_and_html_tags_highlight_keyword_and_expression() {
        let src = "<script>let { children } = $props();</script>\n{@render children()}\n{@html raw}\n{#if ready}x{/if}";
        let doc = Document::from_text(src);
        let mut parser = Parser::new();
        parser
            .set_language(&SyntaxLanguage::Svelte.grammar())
            .unwrap();
        let tree = parser.parse(doc.text(), None).unwrap();
        let runs = style_runs_for_viewport(&doc, &tree, SyntaxLanguage::Svelte, 0, 6);
        let has = |text: &str, nth: usize, scope: HighlightScope| {
            let start = src.match_indices(text).nth(nth).unwrap().0;
            let end = start + text.len();
            runs.iter().any(|run| {
                let line_start = doc.rope().line_to_byte(run.line);
                run.scope == scope && line_start + run.start == start && line_start + run.end == end
            })
        };
        assert!(has("@render", 0, HighlightScope::Keyword), "{runs:?}");
        assert!(has("@html", 0, HighlightScope::Keyword), "{runs:?}");
        assert!(has("#if", 0, HighlightScope::Keyword), "{runs:?}");
        assert!(has("raw", 0, HighlightScope::Variable), "{runs:?}");
        assert!(has("children", 1, HighlightScope::Function), "{runs:?}");
        // No run may start inside the keyword itself.
        assert!(!has("render", 0, HighlightScope::Variable), "{runs:?}");
    }

    #[test]
    fn svelte_each_binding_key_and_closing_tags_are_highlighted() {
        let src = "{#each entries as entry, idx (entry.uid)}\n{#each rows as { label }}{label}{/each}\n{/each}\n{#if a}x{:else if b}y{:else}z{/if}";
        let doc = Document::from_text(src);
        let mut parser = Parser::new();
        parser
            .set_language(&SyntaxLanguage::Svelte.grammar())
            .unwrap();
        let tree = parser.parse(doc.text(), None).unwrap();
        let runs = style_runs_for_viewport(&doc, &tree, SyntaxLanguage::Svelte, 0, 5);
        let at = |text: &str, nth: usize| {
            let start = src.match_indices(text).nth(nth).unwrap().0;
            runs.iter()
                .filter(|run| {
                    let line_start = doc.rope().line_to_byte(run.line);
                    line_start + run.start == start && line_start + run.end == start + text.len()
                })
                .map(|run| run.scope)
                .collect::<Vec<_>>()
        };
        assert!(at(" as ", 0).is_empty());
        assert!(at("as", 0).contains(&HighlightScope::Keyword), "{runs:?}");
        assert!(at("as", 1).contains(&HighlightScope::Keyword), "{runs:?}");
        // Binding, index and key expression are all coloured.
        assert!(
            at("entry", 0).contains(&HighlightScope::Variable),
            "{runs:?}"
        );
        assert!(at("idx", 0).contains(&HighlightScope::Variable), "{runs:?}");
        assert!(
            at("entry", 1).contains(&HighlightScope::Variable),
            "{runs:?}"
        );
        assert!(at("uid", 0).contains(&HighlightScope::Property), "{runs:?}");
        // Destructured binding.
        assert!(
            at("label", 0).contains(&HighlightScope::Variable),
            "{runs:?}"
        );
        // `:else` first matches the start of `:else if`, so take its second hit.
        for (closing, nth) in [("/each", 0), ("/if", 0), (":else if", 0), (":else", 1)] {
            assert!(
                at(closing, nth).contains(&HighlightScope::Keyword),
                "{closing}: {runs:?}"
            );
        }
    }

    #[test]
    fn svelte_attributes_after_braced_objects_keep_their_colors() {
        let src = "<div\n\tin:receive={{ key: 'a b' }}\n\tout:send={{ key: 'a b' }}\n\tclass=\"x y\"\n\taria-label=\"Toggle menu\"\n\tdisabled\n\tid=main\n></div>\n<Languages size={14} />";
        let doc = Document::from_text(src);
        let mut parser = Parser::new();
        parser
            .set_language(&SyntaxLanguage::Svelte.grammar())
            .unwrap();
        let tree = parser.parse(doc.text(), None).unwrap();
        let runs = style_runs_for_viewport(&doc, &tree, SyntaxLanguage::Svelte, 0, 12);
        let at = |text: &str| {
            let start = src.find(text).unwrap();
            runs.iter()
                .filter(|run| {
                    let line_start = doc.rope().line_to_byte(run.line);
                    line_start + run.start == start && line_start + run.end == start + text.len()
                })
                .map(|run| run.scope)
                .collect::<Vec<_>>()
        };
        for name in [
            "in:receive",
            "out:send",
            "class",
            "aria-label",
            "disabled",
            "id",
            "size",
        ] {
            assert!(
                at(name).contains(&HighlightScope::Attribute),
                "{name}: {runs:?}"
            );
        }
        for value in ["\"x y\"", "\"Toggle menu\"", "main"] {
            assert!(
                at(value).contains(&HighlightScope::String),
                "{value}: {runs:?}"
            );
        }
        assert!(at("14").contains(&HighlightScope::Number), "{runs:?}");
        // A component tag takes the class color, not the HTML tag color.
        assert!(at("Languages").contains(&HighlightScope::Type), "{runs:?}");
    }

    #[test]
    fn svelte_attributes_survive_arrow_functions_and_tags_above_the_viewport() {
        let src = "<script lang=\"ts\">\nlet a: Array<string> = []; if (1 < 2) {}\n</script>\n<button\n\tclass=\"md:hidden p-2\"\n\tonclick={() => (open = !open)}\n\taria-label=\"Toggle menu\"\n>\n\tx\n</button>";
        let doc = Document::from_text(src);
        let mut parser = Parser::new();
        parser
            .set_language(&SyntaxLanguage::Svelte.grammar())
            .unwrap();
        let tree = parser.parse(doc.text(), None).unwrap();
        // Viewport on the lines that hold the attributes only.
        let runs = style_runs_for_viewport(&doc, &tree, SyntaxLanguage::Svelte, 4, 3);
        let at = |text: &str| {
            let start = src.find(text).unwrap();
            runs.iter()
                .filter(|run| {
                    let line_start = doc.rope().line_to_byte(run.line);
                    line_start + run.start == start && line_start + run.end == start + text.len()
                })
                .map(|run| run.scope)
                .collect::<Vec<_>>()
        };
        for name in ["class", "onclick", "aria-label"] {
            assert!(
                at(name).contains(&HighlightScope::Attribute),
                "{name}: {runs:?}"
            );
        }
        for value in ["\"md:hidden p-2\"", "\"Toggle menu\""] {
            assert!(
                at(value).contains(&HighlightScope::String),
                "{value}: {runs:?}"
            );
        }
    }

    #[test]
    fn svelte_tag_attributes_skip_braces_and_quotes() {
        let tag =
            "<a href={u} title='x > y' {...rest} data-k = \"v\" on:click={() => f('}')} hidden>";
        let (attributes, length) = svelte_tag_attributes(tag);
        assert_eq!(length, tag.len());
        assert!(svelte_tag_attributes("<p>a<b and more").0.is_empty());
        let found = attributes
            .into_iter()
            .map(|(from, to, scope)| (&tag[from..to], scope))
            .collect::<Vec<_>>();
        assert_eq!(
            found,
            [
                ("href", HighlightScope::Attribute),
                ("title", HighlightScope::Attribute),
                ("'x > y'", HighlightScope::String),
                ("data-k", HighlightScope::Attribute),
                ("\"v\"", HighlightScope::String),
                ("on:click", HighlightScope::Attribute),
                ("hidden", HighlightScope::Attribute),
            ]
        );
    }

    #[test]
    fn svelte_tags_keep_name_and_brackets_after_arrow_attributes() {
        let src = "<button onclick={() => go()}>\n  <Badge\n    a=\"b\"\n  >x</Badge>\n</button>\n<img src={a} />\n";
        let doc = Document::from_text(src);
        let mut parser = Parser::new();
        parser
            .set_language(&SyntaxLanguage::Svelte.grammar())
            .unwrap();
        let tree = parser.parse(doc.text(), None).unwrap();
        let runs = style_runs_for_viewport(&doc, &tree, SyntaxLanguage::Svelte, 0, 8);
        let styled = |line: usize, text: &str, scope: HighlightScope| {
            let lines: Vec<&str> = src.lines().collect();
            let start = lines[line].find(text).unwrap();
            runs.iter().any(|r| {
                r.line == line
                    && r.scope == scope
                    && r.start == start
                    && r.end == start + text.len()
            })
        };
        let tagged = |line: usize, text: &str| styled(line, text, HighlightScope::Tag);
        assert!(tagged(0, "button"));
        // Components take the class color.
        assert!(styled(1, "Badge", HighlightScope::Type));
        assert!(styled(3, "Badge", HighlightScope::Type));
        assert!(tagged(4, "button"));
        assert!(tagged(5, "img"));
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
