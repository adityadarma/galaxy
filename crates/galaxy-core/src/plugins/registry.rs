//! Plugin registries: a git repository (or local folder) with an
//! `index.json` listing installable plugins.
//!
//! ```json
//! { "plugins": [
//!   { "id": "laravel", "name": "Laravel", "description": "...", "version": "0.0.32",
//!     "languages": ["php"], "ref": "v0.0.32", "path": "plugins/laravel",
//!     "sha256": "<tree hash>" }
//! ] }
//! ```
//!
//! An entry without `url` lives in the registry repository itself.

use std::fs;
use std::path::Path;

use serde_json::Value;

use super::install::{Source, checkout, plugin_root};
use super::{safe_relative, validate_id, validate_sha256};

pub const INDEX_FILE: &str = "index.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub languages: Vec<String>,
    pub source: Source,
    /// Expected tree hash of the plugin folder; checked on install.
    pub sha256: Option<String>,
    /// Registry it came from.
    pub registry: String,
}

/// Reads the index of `registry` (a git spec as for [`Source::parse`], or an
/// absolute folder holding `index.json`).
pub fn fetch_entries(registry: &str) -> Result<Vec<Entry>, String> {
    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let (root, own) = if registry.starts_with('/') && !registry.contains('#') {
        (
            Path::new(registry).to_path_buf(),
            Source::parse(&format!("file://{registry}"))?,
        )
    } else {
        let own = Source::parse(registry)?;
        checkout(&own, &temp.path().join("registry"))?;
        let root = plugin_root(&temp.path().join("registry"), own.subdir.as_deref())?;
        (root, own)
    };
    let text = fs::read_to_string(root.join(INDEX_FILE))
        .map_err(|e| format!("{registry}: {INDEX_FILE}: {e}"))?;
    parse_index(&text, &own, registry)
}

pub fn parse_index(text: &str, own: &Source, registry: &str) -> Result<Vec<Entry>, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("{INDEX_FILE}: {e}"))?;
    let list = value
        .get("plugins")
        .and_then(Value::as_array)
        .ok_or("index.json needs a plugins array")?;
    let mut entries = Vec::new();
    for item in list {
        let text = |key: &str| item.get(key).and_then(Value::as_str);
        let id = text("id").ok_or("index entry needs an id")?;
        validate_id(id)?;
        let path = text("path");
        if let Some(path) = path {
            safe_relative(path)?;
        }
        let source = Source {
            url: text("url").unwrap_or(&own.url).to_owned(),
            rev: text("ref").map(str::to_owned).or_else(|| {
                // Same repository: stay on the registry's own revision.
                text("url").is_none().then(|| own.rev.clone()).flatten()
            }),
            subdir: path.map(str::to_owned),
        };
        source.validate().map_err(|e| format!("{id}: {e}"))?;
        let sha256 = text("sha256").map(str::to_owned);
        if let Some(hash) = &sha256 {
            validate_sha256(hash)?;
        }
        entries.push(Entry {
            id: id.to_owned(),
            name: text("name").unwrap_or(id).to_owned(),
            description: text("description").unwrap_or_default().to_owned(),
            version: text("version").unwrap_or_default().to_owned(),
            languages: item
                .get("languages")
                .and_then(Value::as_array)
                .map(|l| {
                    l.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            source,
            sha256,
            registry: registry.to_owned(),
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_without_url_live_in_the_registry_repo() {
        let own = Source::parse("https://example.com/plugins.git#main").unwrap();
        let text = r#"{"plugins":[
            {"id":"a","path":"plugins/a","version":"1"},
            {"id":"b","url":"https://other.example/b.git","ref":"v2"}]}"#;
        let entries = parse_index(text, &own, "reg").unwrap();
        assert_eq!(entries[0].source.url, "https://example.com/plugins.git");
        assert_eq!(entries[0].source.rev.as_deref(), Some("main"));
        assert_eq!(entries[0].source.subdir.as_deref(), Some("plugins/a"));
        assert_eq!(entries[1].source.url, "https://other.example/b.git");
        assert_eq!(entries[1].source.rev.as_deref(), Some("v2"));
    }

    #[test]
    fn bad_entries_are_rejected() {
        let own = Source::parse("https://example.com/p.git").unwrap();
        for bad in [
            r#"{"plugins":[{"id":"../x"}]}"#,
            r#"{"plugins":[{"id":"a","path":"../x"}]}"#,
            r#"{"plugins":[{"id":"a","url":"http://x/y.git"}]}"#,
            r#"{"plugins":[{"id":"a","sha256":"zz"}]}"#,
            r#"{}"#,
        ] {
            assert!(parse_index(bad, &own, "r").is_err(), "{bad}");
        }
    }
}
