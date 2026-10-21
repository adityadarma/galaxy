//! Go build constraints and source file filtering.
//!
//! Decides which `.go` files of a directory take part in a build for a given
//! GOOS/GOARCH: `_test.go` files, GOOS/GOARCH file name suffixes and
//! `//go:build` lines (plus legacy `// +build` lines). Everything is read as
//! text; the Go toolchain is never run.

use std::io::Read;
use std::path::{Path, PathBuf};

const KNOWN_OS: &[&str] = &[
    "aix",
    "android",
    "darwin",
    "dragonfly",
    "freebsd",
    "hurd",
    "illumos",
    "ios",
    "js",
    "linux",
    "nacl",
    "netbsd",
    "openbsd",
    "plan9",
    "solaris",
    "wasip1",
    "windows",
    "zos",
];

const KNOWN_ARCH: &[&str] = &[
    "386",
    "amd64",
    "amd64p32",
    "arm",
    "armbe",
    "arm64",
    "arm64be",
    "loong64",
    "mips",
    "mipsle",
    "mips64",
    "mips64le",
    "mips64p32",
    "mips64p32le",
    "ppc",
    "ppc64",
    "ppc64le",
    "riscv",
    "riscv64",
    "s390",
    "s390x",
    "sparc",
    "sparc64",
    "wasm",
];

/// Systems the `unix` build tag is true for.
const UNIX_OS: &[&str] = &[
    "aix",
    "android",
    "darwin",
    "dragonfly",
    "freebsd",
    "hurd",
    "illumos",
    "ios",
    "linux",
    "netbsd",
    "openbsd",
    "solaris",
];

/// Highest `go1.N` release tag treated as satisfied.
const MAX_RELEASE_TAG: u32 = 24;
const MAX_FILES: usize = 2000;
const HEAD_BYTES: u64 = 4096;
const MAX_DEPTH: usize = 64;

/// The target a build is evaluated for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildContext {
    pub goos: String,
    pub goarch: String,
    /// Extra satisfied tags, e.g. `cgo`, `unix`, or a custom `-tags` value.
    pub tags: Vec<String>,
    pub include_tests: bool,
}

impl BuildContext {
    /// The machine Galaxy runs on, with cgo enabled.
    pub fn host() -> Self {
        Self {
            goos: map_os(std::env::consts::OS).to_owned(),
            goarch: map_arch(std::env::consts::ARCH).to_owned(),
            tags: vec!["cgo".to_owned(), "unix".to_owned()],
            include_tests: false,
        }
    }

    /// Also include `_test.go` files.
    pub fn with_tests(mut self) -> Self {
        self.include_tests = true;
        self
    }

    fn has_tag(&self, tag: &str) -> bool {
        match tag {
            "ignore" => return false,
            "gc" => return true,
            "unix" if UNIX_OS.contains(&self.goos.as_str()) => return true,
            _ => {}
        }
        if tag == self.goos || tag == self.goarch {
            return true;
        }
        // GOOS=android also satisfies `linux`, ios `darwin`, illumos `solaris`.
        let implied = matches!(
            (tag, self.goos.as_str()),
            ("linux", "android") | ("darwin", "ios") | ("solaris", "illumos")
        );
        if implied {
            return true;
        }
        if let Some(n) = tag.strip_prefix("go1.")
            && n.parse::<u32>().is_ok_and(|n| n <= MAX_RELEASE_TAG)
        {
            return true;
        }
        self.tags.iter().any(|t| t == tag)
    }
}

/// Rust `target_os` to GOOS.
fn map_os(os: &str) -> &str {
    match os {
        "macos" => "darwin",
        other => other,
    }
}

/// Rust `target_arch` to GOARCH.
fn map_arch(arch: &str) -> &str {
    match arch {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        "x86" => "386",
        "powerpc" => "ppc",
        "powerpc64" => "ppc64",
        "loongarch64" => "loong64",
        other => other,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok<'a> {
    LParen,
    RParen,
    Not,
    And,
    Or,
    Ident(&'a str),
}

fn tokenize(expr: &str) -> Option<Vec<Tok<'_>>> {
    let bytes = expr.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'(' => {
                toks.push(Tok::LParen);
                i += 1;
            }
            b')' => {
                toks.push(Tok::RParen);
                i += 1;
            }
            b'!' => {
                toks.push(Tok::Not);
                i += 1;
            }
            b'&' if bytes.get(i + 1) == Some(&b'&') => {
                toks.push(Tok::And);
                i += 2;
            }
            b'|' if bytes.get(i + 1) == Some(&b'|') => {
                toks.push(Tok::Or);
                i += 2;
            }
            c if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' => {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.')
                {
                    i += 1;
                }
                toks.push(Tok::Ident(&expr[start..i]));
            }
            _ => return None,
        }
    }
    Some(toks)
}

struct Parser<'a> {
    toks: Vec<Tok<'a>>,
    pos: usize,
    ctx: &'a BuildContext,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<Tok<'a>> {
        self.toks.get(self.pos).copied()
    }

    fn or(&mut self, depth: usize) -> Option<bool> {
        let mut value = self.and(depth)?;
        while self.peek() == Some(Tok::Or) {
            self.pos += 1;
            value |= self.and(depth)?;
        }
        Some(value)
    }

    fn and(&mut self, depth: usize) -> Option<bool> {
        let mut value = self.not(depth)?;
        while self.peek() == Some(Tok::And) {
            self.pos += 1;
            value &= self.not(depth)?;
        }
        Some(value)
    }

    fn not(&mut self, depth: usize) -> Option<bool> {
        if depth > MAX_DEPTH {
            return None;
        }
        let tok = self.peek()?;
        self.pos += 1;
        match tok {
            Tok::Not => Some(!self.not(depth + 1)?),
            Tok::LParen => {
                let value = self.or(depth + 1)?;
                if self.peek() != Some(Tok::RParen) {
                    return None;
                }
                self.pos += 1;
                Some(value)
            }
            Tok::Ident(tag) => Some(self.ctx.has_tag(tag)),
            _ => None,
        }
    }
}

/// Evaluates a `//go:build` expression. An empty expression holds; a
/// malformed one does not.
pub fn eval_constraint(expr: &str, ctx: &BuildContext) -> bool {
    let expr = expr.trim();
    if expr.is_empty() {
        return true;
    }
    let Some(toks) = tokenize(expr) else {
        return false;
    };
    let mut parser = Parser { toks, pos: 0, ctx };
    match parser.or(0) {
        Some(value) if parser.pos == parser.toks.len() => value,
        _ => false,
    }
}

fn go_build_expr(line: &str) -> Option<String> {
    let rest = line.strip_prefix("//go:build")?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = rest.find("//").map_or(rest, |i| &rest[..i]);
    Some(rest.trim().to_owned())
}

/// The build constraint of a file head: the `//go:build` line before the
/// package clause, or the legacy `// +build` lines converted to the same
/// syntax when no `//go:build` line exists.
pub fn constraint_of(head: &str) -> Option<String> {
    let head = head.strip_prefix('\u{feff}').unwrap_or(head);
    let mut in_block = false;
    // Legacy lines only count when their comment group ends in a blank line.
    let mut group: Vec<&str> = Vec::new();
    let mut legacy: Vec<&str> = Vec::new();
    for line in head.lines() {
        let line = line.trim();
        if in_block {
            if let Some(end) = line.find("*/") {
                in_block = false;
                if !line[end + 2..].trim().is_empty() {
                    break;
                }
            }
            continue;
        }
        if line.is_empty() {
            legacy.append(&mut group);
        } else if let Some(body) = line.strip_prefix("//") {
            if let Some(expr) = go_build_expr(line) {
                return Some(expr);
            }
            if let Some(opts) = body.trim_start().strip_prefix("+build")
                && (opts.is_empty() || opts.starts_with(char::is_whitespace))
            {
                group.push(opts.trim());
            }
        } else if let Some(body) = line.strip_prefix("/*") {
            match body.find("*/") {
                Some(end) if !body[end + 2..].trim().is_empty() => break,
                Some(_) => {}
                None => in_block = true,
            }
        } else {
            break;
        }
    }
    convert_legacy(&legacy)
}

/// `a,b c` (a and b, or c) lines, ANDed together, as a `//go:build` expression.
fn convert_legacy(lines: &[&str]) -> Option<String> {
    let parts: Vec<String> = lines
        .iter()
        .filter_map(|line| {
            let options: Vec<String> = line
                .split_whitespace()
                .map(|option| option.split(',').collect::<Vec<_>>().join(" && "))
                .collect();
            match options.len() {
                0 => None,
                1 => Some(options[0].clone()),
                _ if lines.len() > 1 => Some(format!("({})", options.join(" || "))),
                _ => Some(options.join(" || ")),
            }
        })
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" && "))
    }
}

/// File name rules: extension, leading `_`/`.`, `_test`, GOOS/GOARCH suffixes.
fn name_matches(ctx: &BuildContext, name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".go") else {
        return false;
    };
    if name.starts_with('_') || name.starts_with('.') {
        return false;
    }
    let (stem, is_test) = match stem.strip_suffix("_test") {
        Some(stem) => (stem, true),
        None => (stem, false),
    };
    if is_test && !ctx.include_tests {
        return false;
    }
    // Anything before the first `_` is the base name, never a tag.
    let Some(first) = stem.find('_') else {
        return true;
    };
    let parts: Vec<&str> = stem[first..].split('_').collect();
    let n = parts.len();
    if n >= 2 && KNOWN_OS.contains(&parts[n - 2]) && KNOWN_ARCH.contains(&parts[n - 1]) {
        return ctx.has_tag(parts[n - 2]) && ctx.has_tag(parts[n - 1]);
    }
    if KNOWN_OS.contains(&parts[n - 1]) || KNOWN_ARCH.contains(&parts[n - 1]) {
        return ctx.has_tag(parts[n - 1]);
    }
    true
}

/// Whether a file takes part in the build, from its name and the text of its
/// head (the caller reads at most the first 4 KiB).
pub fn file_matches(ctx: &BuildContext, file_name: &str, head: &str) -> bool {
    name_matches(ctx, file_name)
        && constraint_of(head).is_none_or(|expr| eval_constraint(&expr, ctx))
}

fn read_head(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(HEAD_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Sorted `.go` files of `dir` (not recursive) that match `ctx`; at most 2000.
pub fn go_files(dir: &Path, ctx: &BuildContext) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name_matches(ctx, name))
        .collect();
    names.sort();
    let mut out = Vec::new();
    for name in names {
        if out.len() >= MAX_FILES {
            break;
        }
        let path = dir.join(&name);
        if !path.is_file() {
            continue;
        }
        let Some(head) = read_head(&path) else {
            continue;
        };
        if constraint_of(&head).is_none_or(|expr| eval_constraint(&expr, ctx)) {
            out.push(path);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(goos: &str, goarch: &str) -> BuildContext {
        BuildContext {
            goos: goos.to_owned(),
            goarch: goarch.to_owned(),
            tags: vec!["cgo".to_owned(), "unix".to_owned()],
            include_tests: false,
        }
    }

    fn linux() -> BuildContext {
        ctx("linux", "amd64")
    }

    #[test]
    fn host_context_is_mapped() {
        let host = BuildContext::host();
        assert!(!host.goos.is_empty() && !host.goarch.is_empty());
        assert_ne!(host.goos, "macos");
        assert_ne!(host.goarch, "aarch64");
        assert_ne!(host.goarch, "x86_64");
        assert_eq!(host.tags, ["cgo", "unix"]);
        assert!(!host.include_tests);
        assert!(host.with_tests().include_tests);
    }

    #[test]
    fn maps_rust_names_to_go_names() {
        assert_eq!(map_os("macos"), "darwin");
        assert_eq!(map_os("linux"), "linux");
        assert_eq!(map_arch("aarch64"), "arm64");
        assert_eq!(map_arch("x86_64"), "amd64");
        assert_eq!(map_arch("riscv64"), "riscv64");
    }

    #[test]
    fn evaluates_operators_and_precedence() {
        let c = linux();
        assert!(eval_constraint("linux", &c));
        assert!(!eval_constraint("windows", &c));
        assert!(eval_constraint("!windows", &c));
        assert!(eval_constraint("linux && amd64", &c));
        assert!(!eval_constraint("linux && arm64", &c));
        assert!(eval_constraint("windows || linux", &c));
        // && binds tighter than ||: windows || (linux && arm64) is false.
        assert!(!eval_constraint("windows || linux && arm64", &c));
        assert!(eval_constraint("(windows || linux) && amd64", &c));
        assert!(eval_constraint("!(windows || darwin)", &c));
        assert!(eval_constraint("!!linux", &c));
    }

    #[test]
    fn evaluates_special_tags() {
        let c = ctx("darwin", "arm64");
        assert!(eval_constraint("unix", &c));
        assert!(eval_constraint("cgo", &c));
        assert!(eval_constraint("gc", &c));
        assert!(!eval_constraint("ignore", &c));
        assert!(!eval_constraint("gccgo", &c));
        assert!(eval_constraint("go1.18 && go1.24", &c));
        assert!(!eval_constraint("go1.25", &c));
        assert!(!eval_constraint("custom", &c));
        let mut custom = c.clone();
        custom.tags.push("custom".to_owned());
        custom.tags.push("go1.25".to_owned());
        assert!(eval_constraint("custom", &custom));
        assert!(eval_constraint("go1.25", &custom));
        assert!(!eval_constraint("windows", &c));
        let mut windows = ctx("windows", "amd64");
        windows.tags.clear();
        assert!(!eval_constraint("unix", &windows));
    }

    #[test]
    fn implied_operating_systems() {
        assert!(eval_constraint("linux", &ctx("android", "arm64")));
        assert!(eval_constraint("darwin", &ctx("ios", "arm64")));
        assert!(eval_constraint("solaris", &ctx("illumos", "amd64")));
        assert!(!eval_constraint("android", &linux()));
    }

    #[test]
    fn malformed_expressions_do_not_hold() {
        let c = linux();
        for bad in [
            "linux &&",
            "(linux",
            "linux)",
            "linux linux",
            "&& linux",
            "a & b",
            "li-nux",
        ] {
            assert!(!eval_constraint(bad, &c), "{bad}");
        }
        assert!(eval_constraint("", &c));
        let deep = format!("{}linux{}", "(".repeat(5000), ")".repeat(5000));
        assert!(!eval_constraint(&deep, &c));
        assert!(!eval_constraint(&"!".repeat(5000), &c));
    }

    #[test]
    fn finds_go_build_line() {
        let head = "// Copyright\n\n//go:build linux && !cgo\n\npackage x\n";
        assert_eq!(constraint_of(head).as_deref(), Some("linux && !cgo"));
        let crlf = "//go:build linux\r\n\r\npackage x\r\n";
        assert_eq!(constraint_of(crlf).as_deref(), Some("linux"));
        let bom = "\u{feff}//go:build darwin\npackage x\n";
        assert_eq!(constraint_of(bom).as_deref(), Some("darwin"));
        let trailing = "//go:build linux // reason\npackage x\n";
        assert_eq!(constraint_of(trailing).as_deref(), Some("linux"));
    }

    #[test]
    fn ignores_constraint_after_package_clause() {
        let head = "package x\n\n//go:build linux\n";
        assert_eq!(constraint_of(head), None);
        assert_eq!(constraint_of("//go:buildx linux\npackage x\n"), None);
        assert_eq!(constraint_of(""), None);
    }

    #[test]
    fn skips_block_comments_in_header() {
        let head = "/* license\n//go:build windows\n*/\n//go:build linux\npackage x\n";
        assert_eq!(constraint_of(head).as_deref(), Some("linux"));
        assert_eq!(constraint_of("/* a */ package x\n"), None);
    }

    #[test]
    fn converts_legacy_build_lines() {
        let head = "// +build linux,amd64 darwin\n\npackage x\n";
        assert_eq!(
            constraint_of(head).as_deref(),
            Some("linux && amd64 || darwin")
        );
        let two = "// +build linux darwin\n// +build !cgo\n\npackage x\n";
        assert_eq!(
            constraint_of(two).as_deref(),
            Some("(linux || darwin) && !cgo")
        );
        assert_eq!(
            constraint_of("// +build ignore\n\npackage main\n").as_deref(),
            Some("ignore")
        );
    }

    #[test]
    fn legacy_needs_blank_line_and_loses_to_go_build() {
        // Attached to the package clause: documentation, not a constraint.
        assert_eq!(constraint_of("// +build linux\npackage x\n"), None);
        let both = "//go:build darwin\n// +build linux\n\npackage x\n";
        assert_eq!(constraint_of(both).as_deref(), Some("darwin"));
    }

    #[test]
    fn file_name_suffixes() {
        let c = linux();
        assert!(name_matches(&c, "a.go"));
        assert!(name_matches(&c, "a_linux.go"));
        assert!(name_matches(&c, "a_linux_amd64.go"));
        assert!(name_matches(&c, "a_amd64.go"));
        assert!(!name_matches(&c, "a_windows.go"));
        assert!(!name_matches(&c, "a_linux_arm64.go"));
        assert!(!name_matches(&c, "a_arm64.go"));
        assert!(!name_matches(&c, "a_darwin_amd64.go"));
        // Unknown suffixes and bare names are ordinary.
        assert!(name_matches(&c, "a_foo.go"));
        assert!(name_matches(&c, "windows.go"));
        assert!(name_matches(&c, "zos_helper.go"));
    }

    #[test]
    fn skips_hidden_underscore_and_non_go_files() {
        let c = linux();
        assert!(!name_matches(&c, "_a.go"));
        assert!(!name_matches(&c, ".a.go"));
        assert!(!name_matches(&c, "a.txt"));
        assert!(!name_matches(&c, "a.s"));
        assert!(!name_matches(&c, "go"));
    }

    #[test]
    fn test_files_follow_include_tests() {
        let c = linux();
        assert!(!name_matches(&c, "a_test.go"));
        let with = c.with_tests();
        assert!(name_matches(&with, "a_test.go"));
        assert!(name_matches(&with, "a_linux_test.go"));
        assert!(!name_matches(&with, "a_windows_test.go"));
    }

    #[test]
    fn file_matches_combines_name_and_head() {
        let c = linux();
        assert!(file_matches(&c, "a.go", "package a\n"));
        assert!(file_matches(&c, "a.go", "//go:build linux\npackage a\n"));
        assert!(!file_matches(&c, "a.go", "//go:build windows\npackage a\n"));
        assert!(!file_matches(&c, "a_windows.go", "package a\n"));
        assert!(!file_matches(&c, "a.go", "//go:build ignore\npackage a\n"));
        assert!(!file_matches(&c, "a.go", "// +build ignore\n\npackage a\n"));
    }

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    #[test]
    fn lists_matching_files_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write(d, "b.go", "package p\n");
        write(d, "a.go", "package p\n");
        write(d, "a_test.go", "package p\n");
        write(d, "c_windows.go", "package p\n");
        write(d, "d_linux.go", "package p\n");
        write(d, "e.go", "//go:build windows\n\npackage p\n");
        write(d, "f.go", "//go:build !windows\n\npackage p\n");
        write(d, "_g.go", "package p\n");
        write(d, ".h.go", "package p\n");
        write(d, "notes.txt", "x");
        std::fs::create_dir(d.join("sub.go")).unwrap();
        let names = |files: Vec<PathBuf>| -> Vec<String> {
            files
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(
            names(go_files(d, &linux())),
            ["a.go", "b.go", "d_linux.go", "f.go"]
        );
        assert_eq!(
            names(go_files(d, &linux().with_tests())),
            ["a.go", "a_test.go", "b.go", "d_linux.go", "f.go"]
        );
        assert!(go_files(&d.join("missing"), &linux()).is_empty());
    }

    #[test]
    fn go_files_caps_at_two_thousand() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..2100 {
            write(dir.path(), &format!("f{i:04}.go"), "package p\n");
        }
        let files = go_files(dir.path(), &linux());
        assert_eq!(files.len(), 2000);
        assert!(files[0].ends_with("f0000.go"));
    }

    #[test]
    fn reads_only_the_head_of_large_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut text = String::from("//go:build linux\n\npackage p\n");
        text.push_str(&"// filler\n".repeat(10_000));
        write(dir.path(), "big.go", &text);
        assert_eq!(go_files(dir.path(), &linux()).len(), 1);
        assert!(go_files(dir.path(), &ctx("windows", "amd64")).is_empty());
    }
}
