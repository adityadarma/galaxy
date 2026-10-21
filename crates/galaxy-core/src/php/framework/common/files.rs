//! File and config helpers shared by every framework: bounded reads and
//! directory walks, positions and locations, `use` resolution, bracket
//! matching and flattening of PHP config files, plus the [`Item`] and
//! [`Loc`] types an index is made of.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use tree_sitter::{Node, Parser};

const MAX_FILES: usize = 4000;
const MAX_BYTES: u64 = 2 * 1024 * 1024;

/// Where a name is defined. Line and UTF-16 column are 0-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loc {
    pub path: PathBuf,
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Item {
    pub name: String,
    /// A value, a signature or a short description.
    pub detail: String,
    pub loc: Option<Loc>,
    /// Fully qualified class behind the name, when there is one.
    pub class: Option<String>,
}

pub(in crate::php) fn read(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return None;
    }
    fs::read_to_string(path).ok()
}

/// Files below `dir`, sorted, skipping hidden entries.
pub(in crate::php) fn walk(dir: &Path) -> Vec<PathBuf> {
    fn go(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth == 0 || out.len() >= MAX_FILES {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                go(&path, depth - 1, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    go(dir, 8, &mut out);
    out
}

pub(in crate::php) fn stem(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.split('.').next())
        .unwrap_or("")
        .to_owned()
}

/// Line and UTF-16 column of byte `at`.
pub(in crate::php) fn position(text: &str, at: usize) -> (u32, u32) {
    let before = &text[..at.min(text.len())];
    let line = before.matches('\n').count() as u32;
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    (
        line,
        text[start..before.len()].encode_utf16().count() as u32,
    )
}

pub(in crate::php) fn at(path: &Path, text: &str, byte: usize) -> Option<Loc> {
    let (line, column) = position(text, byte);
    Some(Loc {
        path: path.to_path_buf(),
        line,
        column,
    })
}

pub(in crate::php) fn file_loc(path: &Path) -> Option<Loc> {
    Some(Loc {
        path: path.to_path_buf(),
        line: 0,
        column: 0,
    })
}

/// `use` statements of a file: lowercase alias to class.
pub(in crate::php) fn uses(text: &str) -> Vec<(String, String)> {
    static USE: OnceLock<Regex> = OnceLock::new();
    let re =
        USE.get_or_init(|| Regex::new(r"(?m)^use\s+([\w\\]+?)(?:\s+as\s+(\w+))?;").expect("valid"));
    re.captures_iter(text)
        .map(|c| {
            let class = c[1].to_owned();
            let alias = c.get(2).map_or_else(
                || class.rsplit('\\').next().unwrap_or("").to_owned(),
                |m| m.as_str().to_owned(),
            );
            (alias.to_ascii_lowercase(), class)
        })
        .collect()
}

pub(in crate::php) fn resolve(class: &str, uses: &[(String, String)]) -> String {
    if let Some(rest) = class.strip_prefix('\\') {
        return rest.to_owned();
    }
    let (first, rest) = class
        .split_once('\\')
        .map_or((class, None), |(a, b)| (a, Some(b)));
    match uses
        .iter()
        .find(|(alias, _)| *alias == first.to_ascii_lowercase())
    {
        Some((_, full)) => match rest {
            Some(rest) => format!("{full}\\{rest}"),
            None => full.clone(),
        },
        None => class.to_owned(),
    }
}

/// Byte offset of the bracket that closes the one at `open`.
pub(in crate::php) fn close_of(text: &str, open: usize) -> usize {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            q @ (b'\'' | b'"') => {
                i += 1;
                while i < bytes.len() && bytes[i] != q {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    bytes.len()
}

/// One value of a file that returns an array (`config/*.php`, `lang/*/*.php`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigEntry {
    /// Dotted path of the key, e.g. `connections.mysql.host`.
    pub key: String,
    pub line: usize,
    pub column_utf16: usize,
    /// The string itself, or the source text of other values, shortened.
    pub value: String,
    pub is_array: bool,
}

const MAX_CONFIG_ENTRIES: usize = 5000;
const MAX_CONFIG_DEPTH: usize = 8;

/// Every keyed value of the array a file returns, nested arrays included.
/// Elements without a string key are skipped.
pub fn flatten_config(source: &str) -> Vec<ConfigEntry> {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
        .is_err()
    {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(array) = returned_array(tree.root_node()) {
        collect_entries(array, "", source, 0, &mut out);
    }
    out
}

fn collect_entries(
    array: Node<'_>,
    prefix: &str,
    source: &str,
    depth: usize,
    out: &mut Vec<ConfigEntry>,
) {
    let bytes = source.as_bytes();
    let mut cursor = array.walk();
    let elements: Vec<Node<'_>> = array
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "array_element_initializer")
        .collect();
    for element in elements {
        if out.len() >= MAX_CONFIG_ENTRIES {
            return;
        }
        let mut children = element.walk();
        if !element.children(&mut children).any(|n| n.kind() == "=>") {
            continue;
        }
        let mut named = element.walk();
        let mut parts = element.named_children(&mut named);
        let (Some(key_node), Some(value)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Some(name) = string_value(key_node, bytes) else {
            continue;
        };
        let key = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}.{name}")
        };
        let point = key_node.start_position();
        let line_start = key_node.start_byte() - point.column;
        let column_utf16 = source[line_start..key_node.start_byte()]
            .encode_utf16()
            .count();
        let is_array = value.kind() == "array_creation_expression";
        let preview = match string_value(value, bytes) {
            Some(text) => text.to_owned(),
            None if is_array => String::new(),
            None => {
                let text = value.utf8_text(bytes).unwrap_or("");
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if text.chars().count() > 60 {
                    text.chars().take(60).collect::<String>() + "…"
                } else {
                    text
                }
            }
        };
        out.push(ConfigEntry {
            key: key.clone(),
            line: point.row,
            column_utf16,
            value: preview,
            is_array,
        });
        if is_array && depth < MAX_CONFIG_DEPTH {
            collect_entries(value, &key, source, depth + 1, out);
        }
    }
}

/// The array literal in the file's top-level `return [...]`.
pub(in crate::php) fn returned_array(program: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = program.walk();
    let returns: Vec<Node<'_>> = program
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "return_statement")
        .collect();
    returns.into_iter().find_map(|ret| {
        let mut inner = ret.walk();
        let mut children = ret.named_children(&mut inner);
        children.find(|node| node.kind() == "array_creation_expression")
    })
}

/// Contents of a plain string literal (no interpolation handling needed for
/// config keys).
pub(in crate::php) fn string_value<'s>(node: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    if !matches!(node.kind(), "string" | "encapsed_string") {
        return None;
    }
    let text = node.utf8_text(source).ok()?;
    text.strip_prefix(['\'', '"'])?.strip_suffix(['\'', '"'])
}
