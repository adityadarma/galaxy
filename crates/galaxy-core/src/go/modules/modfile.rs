//! Parsing of `go.mod`, `go.work`, `go.sum` and `vendor/modules.txt` text.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use super::paths::normalize;

/// `go.mod` files larger than this are read only up to the cap.
pub(super) const MAX_MOD_BYTES: u64 = 1024 * 1024;

pub(super) const MAX_SUM_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(super) enum Target {
    Dir(PathBuf),
    Module(String, String),
}

#[derive(Debug, Clone)]
pub(super) struct Replace {
    pub(super) old: String,
    pub(super) old_version: Option<String>,
    pub(super) target: Target,
}

#[derive(Debug, Clone)]
pub(super) struct Vendor {
    pub(super) dir: PathBuf,
    /// Package paths listed in `vendor/modules.txt`; `None` without a manifest.
    pub(super) packages: Option<HashSet<String>>,
}

/// What the directives of one `go.mod` or `go.work` say.
#[derive(Debug, Default)]
pub(super) struct ModFile {
    pub(super) module: Option<String>,
    pub(super) go: Option<String>,
    pub(super) requires: Vec<(String, String)>,
    pub(super) replaces: Vec<Replace>,
    pub(super) uses: Vec<PathBuf>,
}

/// Reads a file as text, at most `cap` bytes, cut at the last full line.
pub(super) fn read_capped(path: &Path, cap: u64) -> Option<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(cap)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 >= cap
        && let Some(end) = bytes.iter().rposition(|b| *b == b'\n')
    {
        bytes.truncate(end + 1);
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

pub(super) fn merge_max(map: &mut HashMap<String, String>, path: String, version: String) {
    match map.get(&path) {
        Some(old) if cmp_version(old, &version) != Ordering::Less => {}
        _ => {
            map.insert(path, version);
        }
    }
}

/// Splits a directive line into fields: quotes removed, `//` comments cut.
pub(super) fn tokens(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '/' && line_has_comment(&mut chars.clone()) {
            break;
        } else if c == '"' || c == '`' {
            chars.next();
            let mut token = String::new();
            while let Some(n) = chars.next() {
                if n == c {
                    break;
                }
                if n == '\\' && c == '"' {
                    if let Some(escaped) = chars.next() {
                        token.push(escaped);
                    }
                } else {
                    token.push(n);
                }
            }
            out.push(token);
        } else {
            let mut token = String::new();
            while let Some(&n) = chars.peek() {
                if n.is_whitespace() || (n == '/' && line_has_comment(&mut chars.clone())) {
                    break;
                }
                token.push(n);
                chars.next();
            }
            out.push(token);
        }
    }
    out
}

pub(super) fn line_has_comment(chars: &mut impl Iterator<Item = char>) -> bool {
    chars.next() == Some('/') && chars.next() == Some('/')
}

pub(super) fn is_local_path(s: &str) -> bool {
    s == "."
        || s == ".."
        || s.starts_with("./")
        || s.starts_with("../")
        || Path::new(s).is_absolute()
}

pub(super) fn read_mod_file(path: &Path, base: &Path) -> ModFile {
    let Some(text) = read_capped(path, MAX_MOD_BYTES) else {
        return ModFile::default();
    };
    parse_mod(&text, base)
}

/// Parses `go.mod` or `go.work` text; relative paths resolve against `base`.
pub(super) fn parse_mod(text: &str, base: &Path) -> ModFile {
    let mut file = ModFile::default();
    let mut block: Option<String> = None;
    for line in text.lines() {
        let toks = tokens(line);
        let Some(first) = toks.first() else {
            continue;
        };
        if let Some(verb) = &block {
            if first == ")" {
                block = None;
            } else {
                apply_directive(&mut file, verb, &toks, base);
            }
        } else if toks.get(1).is_some_and(|t| t == "(") {
            block = Some(first.clone());
        } else if first != ")" {
            apply_directive(&mut file, first, &toks[1..], base);
        }
    }
    file
}

pub(super) fn apply_directive(file: &mut ModFile, verb: &str, args: &[String], base: &Path) {
    match (verb, args) {
        ("module", [path, ..]) => {
            file.module.get_or_insert_with(|| path.clone());
        }
        ("go", [version, ..]) => {
            file.go.get_or_insert_with(|| version.clone());
        }
        ("require", [path, version, ..]) => {
            file.requires.push((path.clone(), version.clone()));
        }
        ("use", [dir, ..]) => file.uses.push(normalize(&base.join(dir))),
        ("replace", _) => {
            let Some(arrow) = args.iter().position(|t| t == "=>") else {
                return;
            };
            let (left, right) = (&args[..arrow], &args[arrow + 1..]);
            let (Some(old), Some(new)) = (left.first(), right.first()) else {
                return;
            };
            let target = if is_local_path(new) {
                Target::Dir(normalize(&base.join(new)))
            } else if let Some(version) = right.get(1) {
                Target::Module(new.clone(), version.clone())
            } else {
                return;
            };
            file.replaces.push(Replace {
                old: old.clone(),
                old_version: left.get(1).cloned(),
                target,
            });
        }
        _ => {}
    }
}

/// `module version h1:hash` lines of a `go.sum`; `/go.mod` hashes skipped.
pub(super) fn read_sums(path: &Path, out: &mut HashMap<String, String>) {
    let Some(text) = read_capped(path, MAX_SUM_BYTES) else {
        return;
    };
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        if let (Some(module), Some(version)) = (fields.next(), fields.next())
            && !version.ends_with("/go.mod")
        {
            merge_max(out, module.to_owned(), version.to_owned());
        }
    }
}

pub(super) fn read_vendor(load_dir: &Path) -> Option<Vendor> {
    let dir = load_dir.join("vendor");
    if !dir.is_dir() {
        return None;
    }
    let packages = read_capped(&dir.join("modules.txt"), MAX_SUM_BYTES).map(|text| {
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_owned)
            .collect()
    });
    Some(Vendor { dir, packages })
}

pub(super) fn cmp_version(a: &str, b: &str) -> Ordering {
    fn split(v: &str) -> (Vec<u64>, Option<&str>) {
        let v = v.strip_prefix('v').unwrap_or(v);
        let v = v.split('+').next().unwrap_or(v);
        let (core, pre) = match v.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (v, None),
        };
        (
            core.split('.').map(|p| p.parse().unwrap_or(0)).collect(),
            pre,
        )
    }
    fn cmp_pre(x: &str, y: &str) -> Ordering {
        let mut xs = x.split('.');
        let mut ys = y.split('.');
        loop {
            match (xs.next(), ys.next()) {
                (None, None) => return Ordering::Equal,
                (None, Some(_)) => return Ordering::Less,
                (Some(_), None) => return Ordering::Greater,
                (Some(p), Some(q)) => {
                    let order = match (p.parse::<u64>(), q.parse::<u64>()) {
                        (Ok(p), Ok(q)) => p.cmp(&q),
                        (Ok(_), Err(_)) => Ordering::Less,
                        (Err(_), Ok(_)) => Ordering::Greater,
                        (Err(_), Err(_)) => p.cmp(q),
                    };
                    if order != Ordering::Equal {
                        return order;
                    }
                }
            }
        }
    }
    let (ac, ap) = split(a);
    let (bc, bp) = split(b);
    ac.cmp(&bc).then_with(|| match (ap, bp) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => cmp_pre(x, y),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_handles_quotes_and_comments() {
        assert_eq!(
            tokens(r#"replace "a b" v1 => ./x // c"#),
            ["replace", "a b", "v1", "=>", "./x"]
        );
        assert_eq!(tokens("`raw/path` v1"), ["raw/path", "v1"]);
        assert_eq!(tokens("  // only comment"), Vec::<String>::new());
        assert_eq!(tokens("a.com/x//y v1"), ["a.com/x"]);
    }

    #[test]
    fn compares_versions() {
        assert_eq!(cmp_version("v1.10.0", "v1.9.0"), Ordering::Greater);
        assert_eq!(cmp_version("v1.0.0-rc.1", "v1.0.0"), Ordering::Less);
        assert_eq!(cmp_version("v1.0.0-rc.2", "v1.0.0-rc.10"), Ordering::Less);
        assert_eq!(
            cmp_version("v2.0.0+incompatible", "v2.0.0"),
            Ordering::Equal
        );
        assert_eq!(
            cmp_version("v0.0.0-20160411212932-81ebce5c23df", "v0.1.0"),
            Ordering::Less
        );
    }
}
