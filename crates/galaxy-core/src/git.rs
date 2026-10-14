//! Read-only Git state for workspace UI (M6-01).

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitFileStatus {
    pub path: String,
    pub kind: GitFileStatusKind,
    pub staged: bool,
    pub code: String,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitBranch {
    pub name: String,
    pub current: bool,
    pub remote: bool,
}

pub fn branches(root: &Path) -> io::Result<Vec<GitBranch>> {
    let output = Command::new("git")
        .args([
            "-C",
            &root.to_string_lossy(),
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads",
            "refs/remotes",
        ])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    let current = read_status(root)?
        .and_then(|status| status.branch)
        .unwrap_or_default();
    let mut result = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|name| !name.ends_with("/HEAD"))
        .map(|name| {
            let remote = name.starts_with("origin/") || name.contains('/');
            GitBranch {
                name: name.to_owned(),
                current: name == current,
                remote,
            }
        })
        .collect::<Vec<_>>();
    result.sort_by_key(|branch| (!branch.current, branch.remote, branch.name.clone()));
    result.dedup_by(|a, b| a.name == b.name);
    Ok(result)
}

pub fn checkout(root: &Path, name: &str) -> io::Result<()> {
    if name.is_empty() || name.contains('\n') || name.contains('\0') {
        return Err(io::Error::other("invalid branch name"));
    }
    let status = Command::new("git")
        .args(["-C", &root.to_string_lossy(), "checkout", name])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("git checkout failed for {name}")))
    }
}

pub fn create_branch(root: &Path, name: &str) -> io::Result<()> {
    if name.is_empty() || name.contains('\n') || name.contains('\0') {
        return Err(io::Error::other("invalid branch name"));
    }
    let status = Command::new("git")
        .args(["-C", &root.to_string_lossy(), "switch", "-c", name])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "git branch creation failed for {name}"
        )))
    }
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

/// One changed region between two texts, in 0-based line numbers. A pure
/// insertion has `old_len == 0`, a pure deletion `new_len == 0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffHunk {
    pub old_start: usize,
    pub old_len: usize,
    pub new_start: usize,
    pub new_len: usize,
}

/// Longest middle section (old lines x new lines) compared line by line.
/// Beyond it the whole middle is reported as one replaced hunk.
const MAX_DIFF_CELLS: usize = 4_000_000;

/// Changed regions turning `old` into `new`. Common leading and trailing
/// lines are skipped first, so the quadratic part only sees what changed.
pub fn diff_hunks(old: &str, new: &str) -> Vec<DiffHunk> {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let a = &old[prefix..old.len() - suffix];
    let b = &new[prefix..new.len() - suffix];
    if a.is_empty() && b.is_empty() {
        return Vec::new();
    }
    let whole = DiffHunk {
        old_start: prefix,
        old_len: a.len(),
        new_start: prefix,
        new_len: b.len(),
    };
    if a.is_empty() || b.is_empty() || a.len().saturating_mul(b.len()) > MAX_DIFF_CELLS {
        return vec![whole];
    }

    let width = b.len() + 1;
    let mut lcs = vec![0u16; (a.len() + 1) * width];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i * width + j] = if a[i] == b[j] {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }

    let mut hunks = Vec::new();
    let (mut i, mut j) = (0, 0);
    let mut open: Option<(usize, usize)> = None;
    let close = |hunks: &mut Vec<DiffHunk>, (os, ns): (usize, usize), i: usize, j: usize| {
        hunks.push(DiffHunk {
            old_start: prefix + os,
            old_len: i - os,
            new_start: prefix + ns,
            new_len: j - ns,
        });
    };
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            if let Some(start) = open.take() {
                close(&mut hunks, start, i, j);
            }
            i += 1;
            j += 1;
        } else {
            open.get_or_insert((i, j));
            let take_new =
                j < b.len() && (i == a.len() || lcs[i * width + j + 1] >= lcs[(i + 1) * width + j]);
            if take_new {
                j += 1;
            } else {
                i += 1;
            }
        }
    }
    if let Some(start) = open.take() {
        close(&mut hunks, start, i, j);
    }
    hunks
}

/// The file's content at `HEAD`, or `None` when HEAD has no such file (new,
/// untracked, or an unborn branch).
pub fn head_blob(root: &Path, rel_path: &str) -> io::Result<Option<Vec<u8>>> {
    let repo = gix::discover(root).map_err(io::Error::other)?;
    let Ok(head) = repo.head_tree() else {
        return Ok(None);
    };
    Ok(head
        .lookup_entry_by_path(rel_path)
        .map_err(io::Error::other)?
        .and_then(|entry| entry.object().ok())
        .and_then(|object| object.try_into_blob().ok())
        .map(|mut blob| blob.take_data()))
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
    let output = Command::new("git")
        .args([
            "-C",
            &root.to_string_lossy(),
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
        ])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    let mut files = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if line.len() < 3 {
            continue;
        }
        let code = line[..2].to_owned();
        let path = line[3..]
            .strip_prefix('"')
            .unwrap_or(&line[3..])
            .rsplit(" -> ")
            .next()
            .unwrap_or(&line[3..])
            .to_owned();
        files.push(GitFileStatus {
            path,
            kind: GitFileStatusKind::Changed,
            staged: code.as_bytes()[0] != b' ' && code.as_bytes()[0] != b'?',
            code,
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

pub fn stage(root: &Path, path: &str) -> io::Result<()> {
    git_path_command(root, ["add", "--", path])
}

pub fn stage_all(root: &Path) -> io::Result<()> {
    git_path_command(root, ["add", "-A"])
}

pub fn unstage(root: &Path, path: &str) -> io::Result<()> {
    git_path_command(root, ["restore", "--staged", "--", path])
}

pub fn unstage_all(root: &Path) -> io::Result<()> {
    git_path_command(root, ["restore", "--staged", "."])
}

pub fn commit(root: &Path, message: &str) -> io::Result<()> {
    if message.trim().is_empty() {
        return Err(io::Error::other("commit message is empty"));
    }
    let status = Command::new("git")
        .args(["-C", &root.to_string_lossy(), "commit", "-m", message])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("git commit failed"))
    }
}

fn git_path_command<const N: usize>(root: &Path, args: [&str; N]) -> io::Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("git operation failed"))
    }
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

    fn hunks(old: &str, new: &str) -> Vec<(usize, usize, usize, usize)> {
        diff_hunks(old, new)
            .iter()
            .map(|h| (h.old_start, h.old_len, h.new_start, h.new_len))
            .collect()
    }

    #[test]
    fn diff_hunks_reports_each_changed_region() {
        assert!(hunks("a\nb\n", "a\nb\n").is_empty());
        assert!(hunks("", "").is_empty());
        // Replacement in the middle.
        assert_eq!(hunks("a\nb\nc\n", "a\nX\nc\n"), [(1, 1, 1, 1)]);
        // Pure insertion and pure deletion.
        assert_eq!(hunks("a\nc\n", "a\nb\nc\n"), [(1, 0, 1, 1)]);
        assert_eq!(hunks("a\nb\nc\n", "a\nc\n"), [(1, 1, 1, 0)]);
        // Two separate regions keep their own offsets on both sides.
        assert_eq!(
            hunks("a\nb\nc\nd\ne\n", "a\nB\nc\nd\nE\nF\n"),
            [(1, 1, 1, 1), (4, 1, 4, 2)]
        );
        // New file and emptied file.
        assert_eq!(hunks("", "a\nb\n"), [(0, 0, 0, 2)]);
        assert_eq!(hunks("a\nb\n", ""), [(0, 2, 0, 0)]);
    }

    #[test]
    fn diff_hunks_gives_up_on_huge_middles_with_one_hunk() {
        let old: String = (0..3000).map(|i| format!("old {i}\n")).collect();
        let new: String = (0..3000).map(|i| format!("new {i}\n")).collect();
        assert_eq!(hunks(&old, &new), [(0, 3000, 0, 3000)]);
    }

    #[test]
    fn head_blob_reads_committed_content_only() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), ["init", "-b", "main"]);
        assert_eq!(
            head_blob(dir.path(), "tracked.txt").unwrap(),
            None,
            "unborn branch"
        );
        fs::write(dir.path().join("tracked.txt"), "one\n").unwrap();
        run(dir.path(), ["add", "tracked.txt"]);
        run(
            dir.path(),
            [
                "-c",
                "user.name=Test",
                "-c",
                "user.email=t@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );
        fs::write(dir.path().join("tracked.txt"), "two\n").unwrap();
        fs::write(dir.path().join("new.txt"), "new\n").unwrap();
        assert_eq!(
            head_blob(dir.path(), "tracked.txt").unwrap(),
            Some(b"one\n".to_vec())
        );
        assert_eq!(head_blob(dir.path(), "new.txt").unwrap(), None, "untracked");
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
