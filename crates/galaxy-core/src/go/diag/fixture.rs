//! Test helpers shared by the diagnostics and actions tests.

use std::path::{Path, PathBuf};

use crate::go::{GoEngine, GoProject};
use crate::lang::Edit;

pub fn put(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A module `example.com/app` with a fake GOROOT (fmt, os, io, strings,
/// net/http, encoding/json, math/rand, crypto/rand, errors) and a module
/// cache holding `example.com/dep@v1.0.0` (packages `dep` and `dep/util`).
pub struct Fx {
    /// Keeps the directory alive.
    _tmp: tempfile::TempDir,
    pub app: PathBuf,
    pub engine: GoEngine,
}

impl Fx {
    pub fn new(files: &[(&str, &str)]) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let goroot = tmp.path().join("goroot");
        for (dir, name) in [
            ("fmt", "fmt"),
            ("os", "os"),
            ("io", "io"),
            ("strings", "strings"),
            ("errors", "errors"),
            ("net/http", "http"),
            ("encoding/json", "json"),
            ("math/rand", "rand"),
            ("crypto/rand", "rand"),
            ("sort", "sort"),
        ] {
            put(
                &goroot,
                &format!("src/{dir}/{name}.go"),
                &format!("package {name}\n"),
            );
        }
        let cache = tmp.path().join("mod");
        put(&cache, "example.com/dep@v1.0.0/dep.go", "package dep\n");
        put(
            &cache,
            "example.com/dep@v1.0.0/util/util.go",
            "package util\n",
        );
        put(
            &cache,
            "example.com/other@v2.0.0/http/h.go",
            "package http\n",
        );
        let app = tmp.path().join("app");
        put(
            &app,
            "go.mod",
            "module example.com/app\n\ngo 1.22\n\nrequire (\n\texample.com/dep v1.0.0\n\texample.com/other v2.0.0\n)\n",
        );
        put(&app, "lib/lib.go", "package lib\n\nfunc Do() {}\n");
        put(&app, "lib/http/h.go", "package http\n");
        for (rel, text) in files {
            put(&app, rel, text);
        }
        let project = GoProject::detect_with(&app, Some(goroot), Some(cache));
        assert!(project.is_some());
        Fx {
            _tmp: tmp,
            app,
            engine: GoEngine::with_project(project),
        }
    }

    pub fn main(&self) -> PathBuf {
        self.app.join("main.go")
    }
}

/// Applies edits (UTF-16 columns) to `src`, last position first.
pub fn apply(src: &str, edits: &[Edit]) -> String {
    let offset = |line: u32, column: u32| -> usize {
        let mut start = 0;
        for _ in 0..line {
            match src[start..].find('\n') {
                Some(i) => start += i + 1,
                None => return src.len(),
            }
        }
        let rest = &src[start..];
        let end = rest.find('\n').unwrap_or(rest.len());
        let mut units = 0;
        for (i, c) in rest[..end].char_indices() {
            if units >= column {
                return start + i;
            }
            units += c.len_utf16() as u32;
        }
        start + end
    };
    let mut ranges: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|e| {
            (
                offset(e.start_line, e.start_column),
                offset(e.end_line, e.end_column),
                e.text.as_str(),
            )
        })
        .collect();
    ranges.sort_by_key(|r| std::cmp::Reverse(r.0));
    let mut out = src.to_owned();
    for (from, to, text) in ranges {
        out.replace_range(from..to, text);
    }
    out
}
