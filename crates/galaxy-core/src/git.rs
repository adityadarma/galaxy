//! Read-only Git state for workspace UI (M6-01).

use std::io;
use std::path::{Path, PathBuf};

use gix::bstr::ByteSlice;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitFileStatus {
    pub path: String,
    pub kind: GitFileStatusKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitFileStatusKind {
    Changed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatus {
    pub root: PathBuf,
    pub branch: Option<String>,
    pub files: Vec<GitFileStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitLineChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitLineChange {
    pub line: usize,
    pub kind: GitLineChangeKind,
}

pub fn diff_lines(
    root: &Path,
    rel_path: &str,
    working_text: &str,
) -> io::Result<Vec<GitLineChange>> {
    let repo = gix::discover(root).map_err(io::Error::other)?;
    let head = match repo.head_tree() {
        Ok(tree) => tree,
        Err(_) => {
            return Ok((1..=working_text.lines().count().max(1))
                .map(|line| GitLineChange {
                    line,
                    kind: GitLineChangeKind::Added,
                })
                .collect());
        }
    };
    let old = head
        .lookup_entry_by_path(rel_path)
        .map_err(io::Error::other)?
        .and_then(|entry| entry.object().ok())
        .and_then(|object| object.try_into_blob().ok())
        .map(|mut blob| blob.take_data())
        .unwrap_or_default();
    let old = String::from_utf8_lossy(&old);
    let old_lines: Vec<_> = old.lines().collect();
    let new_lines: Vec<_> = working_text.lines().collect();
    if old_lines.len().max(new_lines.len()) > 2_000 {
        return Ok(Vec::new());
    }
    let mut lcs = vec![vec![0u16; new_lines.len() + 1]; old_lines.len() + 1];
    for old in (0..old_lines.len()).rev() {
        for new in (0..new_lines.len()).rev() {
            lcs[old][new] = if old_lines[old] == new_lines[new] {
                lcs[old + 1][new + 1] + 1
            } else {
                lcs[old + 1][new].max(lcs[old][new + 1])
            };
        }
    }
    let mut changes = Vec::new();
    let (mut old, mut new) = (0, 0);
    while old < old_lines.len() || new < new_lines.len() {
        if old < old_lines.len() && new < new_lines.len() && old_lines[old] == new_lines[new] {
            old += 1;
            new += 1;
        } else if new < new_lines.len()
            && (old == old_lines.len() || lcs[old][new + 1] >= lcs[old + 1][new])
        {
            changes.push(GitLineChange {
                line: new + 1,
                kind: GitLineChangeKind::Added,
            });
            new += 1;
        } else {
            changes.push(GitLineChange {
                line: new.max(1),
                kind: GitLineChangeKind::Deleted,
            });
            old += 1;
        }
    }
    // A replacement is rendered as one modified marker instead of adjacent
    // added/deleted stripes.
    for index in 0..changes.len().saturating_sub(1) {
        if changes[index].kind == GitLineChangeKind::Deleted
            && changes[index + 1].kind == GitLineChangeKind::Added
            && changes[index].line == changes[index + 1].line
        {
            changes[index].kind = GitLineChangeKind::Modified;
            changes.remove(index + 1);
            break;
        }
    }
    Ok(changes)
}

pub fn read_status(root: &Path) -> io::Result<Option<GitStatus>> {
    let repo = match gix::discover(root) {
        Ok(repo) => repo,
        Err(_) => return Ok(None),
    };
    let branch = repo
        .head_name()
        .map_err(io::Error::other)?
        .map(|name| name.shorten().to_string());
    let mut files = Vec::new();
    let platform = repo
        .status(gix::progress::Discard)
        .map_err(io::Error::other)?;
    let mut iter = platform
        .untracked_files(gix::status::UntrackedFiles::Files)
        .into_iter(std::iter::empty::<gix::bstr::BString>())
        .map_err(io::Error::other)?;
    while let Some(item) = iter.next().transpose().map_err(io::Error::other)? {
        let path = item.location().to_str_lossy().into_owned();
        files.push(GitFileStatus {
            path,
            kind: GitFileStatusKind::Changed,
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files.dedup_by(|a, b| a.path == b.path);
    let root = repo
        .workdir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root.to_path_buf());
    Ok(Some(GitStatus {
        root,
        branch,
        files,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    #[test]
    fn reads_branch_and_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), ["init", "-b", "main"]);
        fs::write(dir.path().join("tracked.txt"), "one\n").unwrap();
        run(dir.path(), ["add", "tracked.txt"]);
        run(
            dir.path(),
            [
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );
        fs::write(dir.path().join("tracked.txt"), "two\n").unwrap();
        fs::write(dir.path().join("new.txt"), "new\n").unwrap();
        let status = read_status(dir.path()).unwrap().unwrap();
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(
            status
                .files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            ["new.txt", "tracked.txt"]
        );
    }

    #[test]
    fn diff_lines_reports_insert_replace_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), ["init", "-b", "main"]);
        fs::write(dir.path().join("tracked.txt"), "one\ntwo\nthree\n").unwrap();
        run(dir.path(), ["add", "tracked.txt"]);
        run(
            dir.path(),
            [
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );
        let changes = diff_lines(dir.path(), "tracked.txt", "one\nchanged\ntwo\n").unwrap();
        assert!(!changes.is_empty());
        assert!(changes.iter().any(|change| change.line == 2));
        assert!(changes.iter().all(|change| matches!(
            change.kind,
            GitLineChangeKind::Added | GitLineChangeKind::Modified | GitLineChangeKind::Deleted
        )));

        let inserted = diff_lines(dir.path(), "tracked.txt", "one\ntwo\nthree\nfour\n").unwrap();
        assert!(
            inserted
                .iter()
                .any(|change| change.line == 4 && change.kind == GitLineChangeKind::Added)
        );
        let deleted = diff_lines(dir.path(), "tracked.txt", "one\ntwo\n").unwrap();
        assert!(
            deleted
                .iter()
                .any(|change| change.kind == GitLineChangeKind::Deleted)
        );
    }

    fn run<const N: usize>(dir: &Path, args: [&str; N]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
