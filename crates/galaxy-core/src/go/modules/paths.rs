//! Path helpers shared by the module resolver.

use std::path::{Component, Path, PathBuf};

/// Module cache case encoding: an uppercase letter `X` is stored as `!x`.
pub fn escape_module_path(p: &str) -> String {
    let mut out = String::with_capacity(p.len() + 4);
    for c in p.chars() {
        if c.is_ascii_uppercase() {
            out.push('!');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

pub(super) fn valid_import_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', '\0'])
        && path
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

/// The part of `import` below `module`; empty when they are equal.
pub(super) fn rest_after<'a>(import: &'a str, module: &str) -> Option<&'a str> {
    if import == module {
        Some("")
    } else {
        import.strip_prefix(module)?.strip_prefix('/')
    }
}

pub(super) fn join_rest(dir: &Path, rest: &str) -> PathBuf {
    if rest.is_empty() {
        dir.to_path_buf()
    } else {
        dir.join(rest)
    }
}

pub(super) fn relative_slashes(rel: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str()?),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

/// Resolves `.` and `..` without touching the disk.
pub(super) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub(super) fn unescape_module_path(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    let mut chars = p.chars();
    while let Some(c) = chars.next() {
        if c == '!'
            && let Some(next) = chars.clone().next()
            && next.is_ascii_lowercase()
        {
            out.push(next.to_ascii_uppercase());
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_and_unescapes_module_paths() {
        assert_eq!(
            escape_module_path("github.com/BurntSushi/toml"),
            "github.com/!burnt!sushi/toml"
        );
        assert_eq!(escape_module_path("example.com/lower"), "example.com/lower");
        assert_eq!(
            unescape_module_path("github.com/!burnt!sushi/toml"),
            "github.com/BurntSushi/toml"
        );
        assert_eq!(unescape_module_path("a!"), "a!");
    }
}
