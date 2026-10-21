//! Directory walks that list Go package import paths.

use std::path::Path;

pub(super) const MAX_WALK_DEPTH: usize = 24;

pub(super) struct Walk<'a> {
    /// Directory names skipped directly below the walk root.
    pub(super) skip_top: &'a [&'a str],
    pub(super) skip_internal: bool,
    /// Stop at subdirectories that have their own `go.mod`.
    pub(super) skip_nested: bool,
    pub(super) cap: usize,
}

pub(super) fn is_source_name(name: &str) -> bool {
    name.ends_with(".go")
        && !name.ends_with("_test.go")
        && !name.starts_with('_')
        && !name.starts_with('.')
}

pub(super) fn skip_dir(name: &str, depth: usize, walk: &Walk) -> bool {
    name.starts_with(['.', '_'])
        || name == "testdata"
        || name == "vendor"
        || (walk.skip_internal && name == "internal")
        || (depth == 0 && walk.skip_top.contains(&name))
}

/// Import paths (`prefix` plus the relative directory) of every directory
/// below `dir` that holds non-test Go files. Result is sorted.
pub(super) fn collect_packages(dir: &Path, prefix: &str, walk: &Walk, out: &mut Vec<String>) {
    let mut stack = vec![(dir.to_path_buf(), prefix.to_owned(), 0usize)];
    while let Some((current, path, depth)) = stack.pop() {
        if out.len() >= walk.cap {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        let (mut subdirs, mut has_go, mut has_mod) = (Vec::new(), false, false);
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if !skip_dir(&name, depth, walk) {
                    subdirs.push(name);
                }
            } else if name == "go.mod" {
                has_mod = true;
            } else if is_source_name(&name) {
                has_go = true;
            }
        }
        if depth > 0 && has_mod && walk.skip_nested {
            continue;
        }
        if has_go && !path.is_empty() {
            out.push(path.clone());
        }
        if depth >= MAX_WALK_DEPTH {
            continue;
        }
        subdirs.sort();
        for name in subdirs.into_iter().rev() {
            let child = if path.is_empty() {
                name.clone()
            } else {
                format!("{path}/{name}")
            };
            stack.push((current.join(&name), child, depth + 1));
        }
    }
    out.sort();
    out.dedup();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn package_walk_honours_cap() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..10 {
            put(tmp.path(), &format!("p{i}/f.go"), "package p\n");
        }
        let walk = Walk {
            skip_top: &[],
            skip_internal: false,
            skip_nested: true,
            cap: 3,
        };
        let mut out = Vec::new();
        collect_packages(tmp.path(), "m", &walk, &mut out);
        assert_eq!(out, ["m/p0", "m/p1", "m/p2"]);
    }
}
