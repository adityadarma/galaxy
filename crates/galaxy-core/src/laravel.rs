//! Laravel lookups that read the project's config files with Tree-sitter
//! instead of running PHP, such as `Log::channel('queue')` jumping to its
//! definition in `config/logging.php`.

use std::path::{Path, PathBuf};

use tree_sitter::{Node, Parser};

/// A key in a config file: 0-based line and UTF-16 column of its string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigKeyLocation {
    pub path: PathBuf,
    pub line: usize,
    pub column_utf16: usize,
}

/// The Laravel project containing `file`: the nearest ancestor directory
/// with an `artisan` script. Works when the workspace root is a parent of
/// the project (monorepos, `src/backend`, ...).
pub fn project_root(file: &Path) -> Option<&Path> {
    file.ancestors()
        .skip(1)
        .find(|dir| dir.join("artisan").is_file())
}

/// Where `channel` is defined for the project containing `file`, i.e. the
/// `'queue'` key in `'channels' => ['queue' => [...]]` of
/// `config/logging.php`.
pub fn log_channel_location(file: &Path, channel: &str) -> Option<ConfigKeyLocation> {
    let path = project_root(file)?.join("config").join("logging.php");
    let source = std::fs::read_to_string(&path).ok()?;
    let (line, column_utf16) = config_key_position(&source, &["channels", channel])?;
    Some(ConfigKeyLocation {
        path,
        line,
        column_utf16,
    })
}

/// Position of the last key in `keys`, following nested arrays from the
/// array a config file returns. `None` if any key is missing.
pub fn config_key_position(source: &str, keys: &[&str]) -> Option<(usize, usize)> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
        .ok()?;
    let tree = parser.parse(source, None)?;
    let bytes = source.as_bytes();
    let (last, parents) = keys.split_last()?;
    let mut array = returned_array(tree.root_node())?;
    for key in parents {
        array = entry(array, key, bytes)?
            .1
            .filter(|value| value.kind() == "array_creation_expression")?;
    }
    let (key_node, _) = entry(array, last, bytes)?;
    let point = key_node.start_position();
    let line_start = key_node.start_byte() - point.column;
    let column_utf16 = source[line_start..key_node.start_byte()]
        .encode_utf16()
        .count();
    Some((point.row, column_utf16))
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
fn returned_array(program: Node<'_>) -> Option<Node<'_>> {
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

/// Key and value nodes of the `'key' => value` element of `array`.
fn entry<'t>(array: Node<'t>, key: &str, source: &[u8]) -> Option<(Node<'t>, Option<Node<'t>>)> {
    let mut cursor = array.walk();
    let elements: Vec<Node<'t>> = array
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "array_element_initializer")
        .collect();
    elements.into_iter().find_map(|element| {
        let mut children = element.walk();
        if !element
            .children(&mut children)
            .any(|node| node.kind() == "=>")
        {
            return None;
        }
        let mut named = element.walk();
        let mut parts = element.named_children(&mut named);
        let key_node = parts.next()?;
        let value = parts.next();
        (string_value(key_node, source)? == key).then_some((key_node, value))
    })
}

/// Contents of a plain string literal (no interpolation handling needed for
/// config keys).
fn string_value<'s>(node: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    if !matches!(node.kind(), "string" | "encapsed_string") {
        return None;
    }
    let text = node.utf8_text(source).ok()?;
    text.strip_prefix(['\'', '"'])?.strip_suffix(['\'', '"'])
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOGGING: &str = r#"<?php

use Monolog\Handler\StreamHandler;

return [
    'default' => env('LOG_CHANNEL', 'stack'),
    // 'channels' in a comment must not match
    'channels' => [
        'stack' => [
            'driver' => 'stack',
            'channels' => ['single'],
        ],
        "queue" => [
            'driver' => 'daily',
            'path' => storage_path('logs/queue.log'),
        ],
    ],
];
"#;

    #[test]
    fn finds_nested_keys() {
        assert_eq!(
            config_key_position(LOGGING, &["channels", "queue"]),
            Some((12, 8))
        );
        assert_eq!(
            config_key_position(LOGGING, &["channels", "stack"]),
            Some((8, 8))
        );
        assert_eq!(
            config_key_position(LOGGING, &["channels", "stack", "driver"]),
            Some((9, 12))
        );
        assert_eq!(config_key_position(LOGGING, &["default"]), Some((5, 4)));
    }

    #[test]
    fn missing_or_non_array_keys_are_none() {
        assert_eq!(config_key_position(LOGGING, &["channels", "missing"]), None);
        assert_eq!(config_key_position(LOGGING, &["default", "stack"]), None);
        assert_eq!(config_key_position("<?php echo 1;", &["channels"]), None);
        assert_eq!(config_key_position("not php", &["channels"]), None);
    }

    #[test]
    fn columns_are_utf16() {
        let source = "<?php\nreturn [\n    'é' => [], 'b' => [],\n];\n";
        assert_eq!(config_key_position(source, &["b"]), Some((2, 15)));
    }

    #[test]
    fn finds_the_project_from_a_nested_file() {
        let workspace = std::env::temp_dir().join(format!("galaxy-laravel-{}", std::process::id()));
        // The Laravel app lives below the workspace root.
        let root = workspace.join("backend");
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::create_dir_all(root.join("app/Providers")).unwrap();
        std::fs::write(root.join("artisan"), "#!/usr/bin/env php\n").unwrap();
        std::fs::write(root.join("config/logging.php"), LOGGING).unwrap();
        let file = root.join("app/Providers/AppServiceProvider.php");

        assert_eq!(project_root(&file), Some(root.as_path()));
        let location = log_channel_location(&file, "queue").unwrap();
        assert_eq!(location.path, root.join("config/logging.php"));
        assert_eq!((location.line, location.column_utf16), (12, 8));
        assert!(log_channel_location(&file, "nope").is_none());
        assert!(log_channel_location(&workspace.join("other.php"), "queue").is_none());
        let _ = std::fs::remove_dir_all(&workspace);
    }
}
