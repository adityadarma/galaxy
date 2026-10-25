//! Read-only Git state for workspace UI (M6-01).

use std::collections::BTreeSet;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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
            "--format=%(refname)",
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
        .filter_map(|full| {
            // `refs/heads/a/b` is a local branch even though it has a slash.
            let (name, remote) = match full.strip_prefix("refs/heads/") {
                Some(name) => (name, false),
                None => (full.strip_prefix("refs/remotes/")?, true),
            };
            Some(GitBranch {
                name: name.to_owned(),
                current: !remote && name == current,
                remote,
            })
        })
        .collect::<Vec<_>>();
    result.sort_by_key(|branch| (!branch.current, branch.remote, branch.name.clone()));
    result.dedup_by(|a, b| a.name == b.name);
    Ok(result)
}

/// A ref pointing at a commit, as shown next to it in the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRef {
    pub name: String,
    pub kind: GitRefKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitRefKind {
    /// `HEAD -> branch`: the checked-out branch, or a detached `HEAD`.
    Head,
    Branch,
    Remote,
    Tag,
}

/// One row of the commit graph. Lanes are 0-based columns. Drawing a row:
/// `passing` lanes run the full row height, `incoming` lanes run from the
/// row top into the node, `outgoing` lanes run from the node to the row
/// bottom. A lane equal to `lane` is a straight segment, any other bends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCommit {
    pub hash: String,
    pub parents: Vec<String>,
    pub author: String,
    /// Unix seconds, author date.
    pub timestamp: i64,
    pub subject: String,
    pub refs: Vec<GitRef>,
    pub lane: u32,
    pub passing: Vec<u32>,
    pub incoming: Vec<u32>,
    pub outgoing: Vec<u32>,
}

/// Which commits the graph shows. The default shows every ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFilter {
    /// Branch or tag names to start from; empty means every ref.
    pub branches: Vec<String>,
    /// Author names to keep, matched exactly; empty means everyone.
    pub authors: Vec<String>,
    /// Show remote-tracking branches and the commits only they reach.
    pub include_remotes: bool,
}

impl Default for LogFilter {
    fn default() -> Self {
        Self {
            branches: Vec::new(),
            authors: Vec::new(),
            include_remotes: true,
        }
    }
}

/// Newest `limit` commits of every ref, in date order, with the lane
/// layout for drawing them (the Git Graph panel).
pub fn log_graph(root: &Path, limit: usize) -> io::Result<Vec<GitCommit>> {
    log_graph_filtered(root, limit, &LogFilter::default())
}

/// `log_graph` limited by `filter`. With an author filter, parents that
/// were filtered out are dropped so no line is left hanging.
pub fn log_graph_filtered(
    root: &Path,
    limit: usize,
    filter: &LogFilter,
) -> io::Result<Vec<GitCommit>> {
    if let Some(bad) = filter.branches.iter().find(|name| !valid_ref_name(name)) {
        return Err(io::Error::other(format!("invalid branch name: {bad}")));
    }
    let mut cmd = Command::new("git");
    cmd.args(["-C", &root.to_string_lossy(), "log"]);
    cmd.args(["--date-order", "-n", &limit.to_string()]);
    if !filter.authors.is_empty() {
        // The header reads `Name <email>`; anchor so "Bob" skips "Rob Bob".
        for author in &filter.authors {
            cmd.arg(format!("--author=^{} <", escape_regex(author)));
        }
    }
    cmd.arg("--format=%H%x1f%P%x1f%an%x1f%at%x1f%D%x1f%s%x1e");
    if !filter.branches.is_empty() {
        cmd.args(&filter.branches);
    } else if filter.include_remotes {
        cmd.arg("--all");
    } else {
        cmd.args(["--branches", "--tags", "HEAD"]);
    }
    // Names were checked above, but keep them from ever reading as paths.
    cmd.arg("--");
    let output = output_within(cmd, LOCAL_TIMEOUT)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("does not have any commits") {
            return Ok(Vec::new());
        }
        return Err(io::Error::other(stderr.to_string()));
    }
    let mut commits = parse_log(&String::from_utf8_lossy(&output.stdout));
    if !filter.include_remotes {
        for commit in &mut commits {
            commit.refs.retain(|r| r.kind != GitRefKind::Remote);
        }
    }
    if !filter.authors.is_empty() {
        let kept: BTreeSet<String> = commits.iter().map(|c| c.hash.clone()).collect();
        for commit in &mut commits {
            commit.parents.retain(|p| kept.contains(p));
        }
    }
    layout_lanes(&mut commits);
    Ok(commits)
}

/// Escapes `text` for git's basic regular expressions.
fn escape_regex(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '\\' | '.' | '[' | ']' | '*' | '^' | '$') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Distinct author names in the newest `limit` commits of every ref,
/// sorted.
pub fn authors(root: &Path, limit: usize) -> io::Result<Vec<String>> {
    let mut cmd = Command::new("git");
    cmd.args([
        "-C",
        &root.to_string_lossy(),
        "log",
        "--all",
        "-n",
        &limit.to_string(),
        "--format=%an",
    ]);
    let output = output_within(cmd, LOCAL_TIMEOUT)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("does not have any commits") {
            return Ok(Vec::new());
        }
        return Err(io::Error::other(stderr.to_string()));
    }
    let names: BTreeSet<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect();
    Ok(names.into_iter().collect())
}

fn parse_log(text: &str) -> Vec<GitCommit> {
    text.split('\x1e')
        .filter_map(|record| {
            let mut fields = record.trim_start_matches('\n').split('\x1f');
            let hash = fields.next().filter(|h| !h.is_empty())?.to_owned();
            let parents = fields
                .next()?
                .split_whitespace()
                .map(str::to_owned)
                .collect();
            let author = fields.next()?.to_owned();
            let timestamp = fields.next()?.parse().unwrap_or(0);
            let refs = parse_refs(fields.next()?);
            let subject = fields.next()?.to_owned();
            Some(GitCommit {
                hash,
                parents,
                author,
                timestamp,
                subject,
                refs,
                lane: 0,
                passing: Vec::new(),
                incoming: Vec::new(),
                outgoing: Vec::new(),
            })
        })
        .collect()
}

/// Parses `%D`: `HEAD -> main, origin/main, tag: v1`.
fn parse_refs(decoration: &str) -> Vec<GitRef> {
    decoration
        .split(", ")
        .filter(|part| !part.is_empty() && !part.ends_with("/HEAD"))
        .map(|part| {
            if let Some(tag) = part.strip_prefix("tag: ") {
                GitRef {
                    name: tag.to_owned(),
                    kind: GitRefKind::Tag,
                }
            } else if let Some(branch) = part.strip_prefix("HEAD -> ") {
                GitRef {
                    name: branch.to_owned(),
                    kind: GitRefKind::Head,
                }
            } else if part == "HEAD" {
                GitRef {
                    name: "HEAD".to_owned(),
                    kind: GitRefKind::Head,
                }
            } else if part.contains('/') {
                GitRef {
                    name: part.to_owned(),
                    kind: GitRefKind::Remote,
                }
            } else {
                GitRef {
                    name: part.to_owned(),
                    kind: GitRefKind::Branch,
                }
            }
        })
        .collect()
}

/// Assigns each commit a lane and the line segments around it. `lanes[i]`
/// holds the hash the line in lane `i` is waiting to reach.
fn layout_lanes(commits: &mut [GitCommit]) {
    let mut lanes: Vec<Option<String>> = Vec::new();
    for commit in commits {
        let matching: Vec<usize> = lanes
            .iter()
            .enumerate()
            .filter(|(_, hash)| hash.as_deref() == Some(commit.hash.as_str()))
            .map(|(index, _)| index)
            .collect();
        let lane = matching
            .first()
            .copied()
            .unwrap_or_else(|| free_lane(&mut lanes));
        commit.lane = lane as u32;
        commit.incoming = matching.iter().map(|&i| i as u32).collect();
        commit.passing = lanes
            .iter()
            .enumerate()
            .filter(|(index, hash)| hash.is_some() && !matching.contains(index))
            .map(|(index, _)| index as u32)
            .collect();
        for &index in &matching {
            lanes[index] = None;
        }
        let mut outgoing = Vec::new();
        for (n, parent) in commit.parents.iter().enumerate() {
            let target = if n == 0 {
                lanes[lane] = Some(parent.clone());
                lane
            } else if let Some(existing) = lanes
                .iter()
                .position(|hash| hash.as_deref() == Some(parent.as_str()))
            {
                existing
            } else {
                let index = free_lane(&mut lanes);
                lanes[index] = Some(parent.clone());
                index
            };
            if !outgoing.contains(&(target as u32)) {
                outgoing.push(target as u32);
            }
        }
        commit.outgoing = outgoing;
        while lanes.last().is_some_and(Option::is_none) {
            lanes.pop();
        }
    }
}

fn free_lane(lanes: &mut Vec<Option<String>>) -> usize {
    lanes.iter().position(Option::is_none).unwrap_or_else(|| {
        lanes.push(None);
        lanes.len() - 1
    })
}

/// A branch or tag name safe to pass as a git argument.
fn valid_ref_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.contains(['\n', '\0', ' ', '~', '^', ':', '?', '*', '[', '\\'])
        && !name.contains("..")
}

/// A commit id as printed by `git log`: hex only, so it cannot be an option.
fn valid_commit_id(rev: &str) -> bool {
    (4..=64).contains(&rev.len()) && rev.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Local git operations finish quickly; this only stops a stuck one.
const LOCAL_TIMEOUT: Duration = Duration::from_secs(60);
/// Remote operations give up sooner: offline must not hang the panel.
const NETWORK_TIMEOUT: Duration = Duration::from_secs(20);
/// Blame walks the file's whole history; a huge file may take a while.
const BLAME_TIMEOUT: Duration = Duration::from_secs(15);
/// The remote whose tags the graph compares against.
const REMOTE: &str = "origin";

/// Runs `cmd` without a terminal (no credential prompt can block it) and
/// kills it after `limit`.
fn output_within(mut cmd: Command, limit: Duration) -> io::Result<Output> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        // Nothing here can answer an editor: take the default message.
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("GIT_MERGE_AUTOEDIT", "no");
    let mut child = cmd.spawn()?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("no stdout"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("no stderr"))?;
    let out = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let err = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            // The reader threads are left to end on their own: a helper
            // process may still hold the pipes open.
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "git timed out"));
        }
        thread::sleep(Duration::from_millis(20));
    };
    Ok(Output {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}

fn git_output(root: &Path, args: &[&str], limit: Duration) -> io::Result<Output> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).args(args);
    output_within(cmd, limit)
}

fn run_git(root: &Path, args: &[&str], what: &str, limit: Duration) -> io::Result<()> {
    let output = git_output(root, args, limit)
        .map_err(|e| io::Error::new(e.kind(), format!("{what} failed: {e}")))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let reason = stderr.lines().next().unwrap_or("").trim();
    Err(io::Error::other(if reason.is_empty() {
        format!("{what} failed")
    } else {
        format!("{what} failed: {reason}")
    }))
}

fn has_remote(root: &Path) -> bool {
    git_output(root, &["remote"], LOCAL_TIMEOUT)
        .map(|o| {
            o.status.success()
                && String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .any(|name| name == REMOTE)
        })
        .unwrap_or(false)
}

/// Names of the tags on `origin`, or `None` when there is no `origin`.
/// Contacts the remote, so it can be slow or fail while offline.
pub fn remote_tags(root: &Path) -> io::Result<Option<BTreeSet<String>>> {
    if !has_remote(root) {
        return Ok(None);
    }
    let output = git_output(
        root,
        &["ls-remote", "--tags", "--refs", REMOTE],
        NETWORK_TIMEOUT,
    )?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(
            stderr
                .lines()
                .next()
                .unwrap_or("ls-remote failed")
                .trim()
                .to_owned(),
        ));
    }
    Ok(Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.split('\t').nth(1))
            .filter_map(|name| name.strip_prefix("refs/tags/"))
            .map(str::to_owned)
            .collect(),
    ))
}

/// Deletes the local tag `name`.
pub fn delete_tag(root: &Path, name: &str) -> io::Result<()> {
    if !valid_ref_name(name) {
        return Err(io::Error::other("invalid tag name"));
    }
    run_git(root, &["tag", "-d", name], "deleting tag", LOCAL_TIMEOUT)
}

/// Deletes tag `name` on `origin`. The local tag stays.
pub fn delete_remote_tag(root: &Path, name: &str) -> io::Result<()> {
    if !valid_ref_name(name) {
        return Err(io::Error::other("invalid tag name"));
    }
    let target = format!("refs/tags/{name}");
    run_git(
        root,
        &["push", REMOTE, "--delete", &target],
        "deleting remote tag",
        NETWORK_TIMEOUT,
    )
}

/// Pushes the local tag `name` to `origin`.
pub fn push_tag(root: &Path, name: &str) -> io::Result<()> {
    if !valid_ref_name(name) {
        return Err(io::Error::other("invalid tag name"));
    }
    let target = format!("refs/tags/{name}");
    run_git(
        root,
        &["push", REMOTE, &target],
        "pushing tag",
        NETWORK_TIMEOUT,
    )
}

/// An unfinished cherry-pick, revert, merge or rebase: its name and the
/// arguments that abort it.
fn in_progress(root: &Path) -> Option<(&'static str, &'static [&'static str])> {
    let repo = gix::discover(root).ok()?;
    let dir = repo.git_dir();
    let states: [(&str, &str, &'static [&'static str]); 5] = [
        ("rebase-merge", "rebase", &["rebase", "--abort"]),
        ("rebase-apply", "rebase", &["rebase", "--abort"]),
        ("MERGE_HEAD", "merge", &["merge", "--abort"]),
        (
            "CHERRY_PICK_HEAD",
            "cherry-pick",
            &["cherry-pick", "--abort"],
        ),
        ("REVERT_HEAD", "revert", &["revert", "--abort"]),
    ];
    states
        .iter()
        .find(|(file, _, _)| dir.join(file).exists())
        .map(|&(_, name, abort)| (name, abort))
}

fn ensure_idle(root: &Path) -> io::Result<()> {
    match in_progress(root) {
        Some((name, _)) => Err(io::Error::other(format!(
            "a {name} is already in progress; finish or abort it in the terminal first"
        ))),
        None => Ok(()),
    }
}

fn checked_commit_id(rev: &str) -> io::Result<()> {
    if valid_commit_id(rev) {
        Ok(())
    } else {
        Err(io::Error::other("invalid commit id"))
    }
}

fn parent_count(root: &Path, rev: &str) -> io::Result<usize> {
    let output = git_output(
        root,
        &["rev-list", "--parents", "-n", "1", rev],
        LOCAL_TIMEOUT,
    )?;
    if !output.status.success() {
        return Err(io::Error::other("unknown commit"));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .count()
        .saturating_sub(1))
}

/// Runs an operation that can stop on conflicts. If it does, it is aborted
/// so the repository is left exactly as it was.
fn run_abortable(root: &Path, args: &[&str], what: &str) -> io::Result<()> {
    ensure_idle(root)?;
    match run_git(root, args, what, LOCAL_TIMEOUT) {
        Ok(()) => Ok(()),
        Err(error) => {
            if let Some((name, abort)) = in_progress(root) {
                let _ = run_git(root, abort, "abort", LOCAL_TIMEOUT);
                return Err(io::Error::other(format!(
                    "{what} stopped on conflicts ({name}) and was aborted; nothing changed"
                )));
            }
            Err(error)
        }
    }
}

/// Applies the changes of commit `rev` on top of HEAD.
pub fn cherry_pick(root: &Path, rev: &str) -> io::Result<()> {
    checked_commit_id(rev)?;
    if parent_count(root, rev)? > 1 {
        return Err(io::Error::other("a merge commit cannot be cherry-picked"));
    }
    run_abortable(root, &["cherry-pick", rev], "cherry-pick")
}

/// Adds a commit that undoes commit `rev`.
pub fn revert(root: &Path, rev: &str) -> io::Result<()> {
    checked_commit_id(rev)?;
    if parent_count(root, rev)? > 1 {
        return Err(io::Error::other("a merge commit cannot be reverted"));
    }
    run_abortable(root, &["revert", "--no-edit", rev], "revert")
}

fn commit_staged_for(root: &Path, flag: &str, rev: &str, what: &str) -> io::Result<()> {
    checked_commit_id(rev)?;
    ensure_idle(root)?;
    // `diff --cached --quiet` exits 1 when something is staged.
    let staged = git_output(root, &["diff", "--cached", "--quiet"], LOCAL_TIMEOUT)?;
    if staged.status.code() != Some(1) {
        return Err(io::Error::other(format!(
            "{what} failed: nothing is staged; stage the changes first"
        )));
    }
    run_git(
        root,
        &["commit", &format!("{flag}={rev}")],
        what,
        LOCAL_TIMEOUT,
    )
}

/// Commits the staged changes as `fixup! <subject of rev>`.
pub fn commit_fixup(root: &Path, rev: &str) -> io::Result<()> {
    commit_staged_for(root, "--fixup", rev, "fixup commit")
}

/// Commits the staged changes as `squash! <subject of rev>`.
pub fn commit_squash(root: &Path, rev: &str) -> io::Result<()> {
    commit_staged_for(root, "--squash", rev, "squash commit")
}

/// Undoes the last commit and keeps its changes staged.
pub fn reset_last_commit_soft(root: &Path) -> io::Result<()> {
    ensure_idle(root)?;
    let parent = git_output(
        root,
        &["rev-parse", "--verify", "--quiet", "HEAD~1"],
        LOCAL_TIMEOUT,
    )?;
    if !parent.status.success() {
        return Err(io::Error::other(
            "HEAD is the first commit; there is nothing to undo",
        ));
    }
    run_git(root, &["reset", "--soft", "HEAD~1"], "reset", LOCAL_TIMEOUT)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitResetMode {
    /// Move the branch; keep the index and working tree.
    Soft,
    /// Move the branch and reset the index; keep the working tree.
    Mixed,
    /// Move the branch and discard every uncommitted change.
    Hard,
}

/// Moves the current branch to commit `rev`.
pub fn reset_to(root: &Path, rev: &str, mode: GitResetMode) -> io::Result<()> {
    checked_commit_id(rev)?;
    ensure_idle(root)?;
    let flag = match mode {
        GitResetMode::Soft => "--soft",
        GitResetMode::Mixed => "--mixed",
        GitResetMode::Hard => "--hard",
    };
    run_git(root, &["reset", flag, rev], "reset", LOCAL_TIMEOUT)
}

/// Merges commit `rev` into the current branch.
pub fn merge(root: &Path, rev: &str) -> io::Result<()> {
    checked_commit_id(rev)?;
    run_abortable(root, &["merge", "--no-edit", rev], "merge")
}

/// Replays the current branch on top of commit `rev`.
pub fn rebase_onto(root: &Path, rev: &str) -> io::Result<()> {
    checked_commit_id(rev)?;
    run_abortable(root, &["rebase", "--rebase-merges", rev], "rebase")
}

/// Removes commit `rev` from the current branch, keeping the rest. This
/// rewrites every commit after it.
pub fn drop_commit(root: &Path, rev: &str) -> io::Result<()> {
    checked_commit_id(rev)?;
    if parent_count(root, rev)? != 1 {
        return Err(io::Error::other(
            "only a commit with exactly one parent can be dropped",
        ));
    }
    // `rebase --onto rev^ rev` replays rev..HEAD; for a commit outside this
    // branch that would replay unrelated history.
    let ancestor = git_output(
        root,
        &["merge-base", "--is-ancestor", rev, "HEAD"],
        LOCAL_TIMEOUT,
    )?;
    match ancestor.status.code() {
        Some(0) => {}
        Some(1) => {
            return Err(io::Error::other(
                "that commit is not part of the current branch",
            ));
        }
        _ => return Err(io::Error::other("could not check the commit against HEAD")),
    }
    run_abortable(
        root,
        &[
            "rebase",
            "--rebase-merges",
            "--onto",
            &format!("{rev}^"),
            rev,
        ],
        "drop",
    )
}

/// Whether some remote-tracking branch already contains `rev`, so
/// rewriting it would diverge from what was pushed. Uses local refs only.
pub fn is_on_remote(root: &Path, rev: &str) -> io::Result<bool> {
    checked_commit_id(rev)?;
    let output = git_output(
        root,
        &[
            "for-each-ref",
            "--count=1",
            "--contains",
            rev,
            "refs/remotes",
        ],
        LOCAL_TIMEOUT,
    )?;
    if !output.status.success() {
        return Err(io::Error::other("could not read remote branches"));
    }
    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

/// A file that differs between two commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitFileDiff {
    pub path: String,
    /// Where a renamed or copied file came from.
    pub old_path: Option<String>,
    /// `A`dded, `M`odified, `D`eleted, `R`enamed, `C`opied, `T`ype changed.
    pub status: char,
}

/// Files that differ between commits `from` and `to`.
pub fn compare_files(root: &Path, from: &str, to: &str) -> io::Result<Vec<GitFileDiff>> {
    checked_commit_id(from)?;
    checked_commit_id(to)?;
    let output = git_output(
        root,
        &["diff", "--name-status", "-z", "--find-renames", from, to],
        LOCAL_TIMEOUT,
    )?;
    if !output.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut fields = text.split('\0').filter(|field| !field.is_empty());
    let mut files = Vec::new();
    while let Some(status) = fields.next() {
        let letter = status.chars().next().unwrap_or('M');
        let Some(first) = fields.next() else { break };
        if matches!(letter, 'R' | 'C') {
            let Some(second) = fields.next() else { break };
            files.push(GitFileDiff {
                path: second.to_owned(),
                old_path: Some(first.to_owned()),
                status: letter,
            });
        } else {
            files.push(GitFileDiff {
                path: first.to_owned(),
                old_path: None,
                status: letter,
            });
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// The most diff text shown for one file.
const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;

/// Unified diff of one file between commits `from` and `to`.
pub fn compare_diff(
    root: &Path,
    from: &str,
    to: &str,
    path: &str,
    old_path: Option<&str>,
) -> io::Result<String> {
    checked_commit_id(from)?;
    checked_commit_id(to)?;
    if path.is_empty() || path.contains('\0') || old_path.is_some_and(|p| p.contains('\0')) {
        return Err(io::Error::other("invalid path"));
    }
    let mut args = vec![
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--find-renames",
        from,
        to,
        "--",
    ];
    if let Some(old) = old_path {
        args.push(old);
    }
    args.push(path);
    let output = git_output(root, &args, LOCAL_TIMEOUT)?;
    if !output.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let truncated = output.stdout.len() > MAX_DIFF_BYTES;
    let bytes = &output.stdout[..output.stdout.len().min(MAX_DIFF_BYTES)];
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    if truncated {
        text.push_str("\n… diff truncated …\n");
    }
    Ok(text)
}

/// Creates `name` at commit `rev` and switches to it.
pub fn create_branch_at(root: &Path, name: &str, rev: &str) -> io::Result<()> {
    if !valid_ref_name(name) {
        return Err(io::Error::other("invalid branch name"));
    }
    if !valid_commit_id(rev) {
        return Err(io::Error::other("invalid commit id"));
    }
    run_git(
        root,
        &["switch", "-c", name, rev],
        "creating branch",
        LOCAL_TIMEOUT,
    )
}

/// Creates a lightweight tag `name` at commit `rev`.
pub fn create_tag(root: &Path, name: &str, rev: &str) -> io::Result<()> {
    if !valid_ref_name(name) {
        return Err(io::Error::other("invalid tag name"));
    }
    if !valid_commit_id(rev) {
        return Err(io::Error::other("invalid commit id"));
    }
    run_git(root, &["tag", name, rev], "creating tag", LOCAL_TIMEOUT)
}

/// Checks out a commit, leaving HEAD detached.
pub fn checkout_commit(root: &Path, rev: &str) -> io::Result<()> {
    if !valid_commit_id(rev) {
        return Err(io::Error::other("invalid commit id"));
    }
    run_git(
        root,
        &["checkout", "--detach", rev],
        "checkout",
        LOCAL_TIMEOUT,
    )
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
        .map(|mut blob| blob.take_data());
    // Dependency trees are third-party code, not changes to the workspace.
    // They are commonly ignored and absent from HEAD, which otherwise makes
    // every line look newly added in the gutter.
    if old.is_none()
        && rel_path
            .split('/')
            .any(|part| matches!(part, "node_modules" | "vendor"))
    {
        return Ok(Vec::new());
    }
    let old = old.unwrap_or_default();
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

/// The commit that last touched some lines of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameCommit {
    pub sha: String,
    pub author: String,
    pub email: String,
    /// Author time, seconds since the Unix epoch.
    pub time: i64,
    pub summary: String,
    /// Lines not yet committed (edited, or staged but not committed).
    pub uncommitted: bool,
}

/// Blame for a whole file: `line_commit[n]` indexes `commits` for 0-based
/// line `n`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Blame {
    pub commits: Vec<BlameCommit>,
    pub line_commit: Vec<u32>,
}

/// Blame for `rel_path` as if the file held `working_text`, so unsaved edits
/// show as uncommitted. `None` when git cannot blame the file (untracked, new
/// repository, outside a repository).
pub fn blame(root: &Path, rel_path: &str, working_text: &str) -> io::Result<Option<Blame>> {
    use std::collections::HashMap;
    use std::io::Write;

    let mut contents = tempfile::NamedTempFile::new()?;
    contents.write_all(working_text.as_bytes())?;
    contents.flush()?;
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .args(["blame", "--porcelain", "--contents"])
        .arg(contents.path())
        .args(["--", rel_path]);
    let output = output_within(cmd, BLAME_TIMEOUT)?;
    if !output.status.success() {
        return Ok(None);
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut blame = Blame::default();
    let mut index: HashMap<&str, u32> = HashMap::new();
    let mut current: Option<u32> = None;
    let mut final_line = 0usize;
    for line in text.split('\n') {
        if line.starts_with('\t') {
            // Content line: the group header before it said which commit.
            if let Some(commit) = current {
                if blame.line_commit.len() < final_line {
                    blame.line_commit.resize(final_line, u32::MAX);
                }
                blame.line_commit[final_line - 1] = commit;
            }
            continue;
        }
        let mut parts = line.split(' ');
        let (Some(sha), Some(orig), Some(fin)) = (parts.next(), parts.next(), parts.next()) else {
            // A header line such as `summary ...`: attach it to the commit.
            if let (Some(commit), Some((key, value))) = (current, line.split_once(' ')) {
                set_blame_field(&mut blame.commits[commit as usize], key, value);
            }
            continue;
        };
        let is_group_header = (40..=64).contains(&sha.len())
            && sha.bytes().all(|b| b.is_ascii_hexdigit())
            && orig.parse::<usize>().is_ok();
        if is_group_header {
            let Ok(fin) = fin.parse::<usize>() else {
                continue;
            };
            final_line = fin;
            current = Some(*index.entry(sha).or_insert_with(|| {
                blame.commits.push(BlameCommit {
                    sha: sha.to_string(),
                    author: String::new(),
                    email: String::new(),
                    time: 0,
                    summary: String::new(),
                    uncommitted: sha.bytes().all(|b| b == b'0'),
                });
                (blame.commits.len() - 1) as u32
            }));
        } else if let (Some(commit), Some((key, value))) = (current, line.split_once(' ')) {
            set_blame_field(&mut blame.commits[commit as usize], key, value);
        }
    }
    Ok(Some(blame))
}

fn set_blame_field(commit: &mut BlameCommit, key: &str, value: &str) {
    match key {
        "author" => commit.author = value.to_string(),
        "author-mail" => commit.email = value.trim_matches(['<', '>']).to_string(),
        "author-time" => commit.time = value.parse().unwrap_or(0),
        "summary" => commit.summary = value.to_string(),
        _ => {}
    }
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

    #[test]
    fn diff_lines_ignores_untracked_dependency_files() {
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

        assert!(
            diff_lines(dir.path(), "node_modules/pkg/index.ts", "export {};\n")
                .unwrap()
                .is_empty()
        );
        assert!(
            diff_lines(dir.path(), "vendor/pkg/src/File.php", "<?php\n")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn log_graph_lays_out_a_merge() {
        let dir = tempfile::tempdir().unwrap();
        let id = ["-c", "user.name=Test", "-c", "user.email=t@example.com"];
        run(dir.path(), ["init", "-b", "main"]);
        fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        run(dir.path(), ["add", "a.txt"]);
        run(
            dir.path(),
            [id[0], id[1], id[2], id[3], "commit", "-m", "base"],
        );
        run(dir.path(), ["switch", "-c", "feature"]);
        fs::write(dir.path().join("b.txt"), "b\n").unwrap();
        run(dir.path(), ["add", "b.txt"]);
        run(
            dir.path(),
            [id[0], id[1], id[2], id[3], "commit", "-m", "on feature"],
        );
        run(dir.path(), ["switch", "main"]);
        fs::write(dir.path().join("c.txt"), "c\n").unwrap();
        run(dir.path(), ["add", "c.txt"]);
        run(
            dir.path(),
            [id[0], id[1], id[2], id[3], "commit", "-m", "on main"],
        );
        run(
            dir.path(),
            [
                id[0], id[1], id[2], id[3], "merge", "--no-ff", "feature", "-m", "merge",
            ],
        );

        let commits = log_graph(dir.path(), 50).unwrap();
        let subjects: Vec<_> = commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects.len(), 4);
        assert_eq!(subjects[0], "merge");
        assert_eq!(subjects[3], "base");
        let merge = &commits[0];
        assert_eq!(merge.parents.len(), 2);
        assert_eq!(merge.lane, 0);
        assert_eq!(
            merge.outgoing,
            [0, 1],
            "first parent stays, second branches"
        );
        assert!(
            merge
                .refs
                .iter()
                .any(|r| r.kind == GitRefKind::Head && r.name == "main")
        );
        let base = commits.last().unwrap();
        assert_eq!(base.lane, 0);
        assert_eq!(base.incoming, [0, 1], "both lines end at the root commit");
        assert!(base.outgoing.is_empty());
    }

    fn commit_file_as(dir: &Path, author: &str, file: &str, message: &str) {
        fs::write(dir.join(file), message).unwrap();
        run(dir, ["add", file]);
        let name = format!("user.name={author}");
        run(
            dir,
            [
                "-c",
                name.as_str(),
                "-c",
                "user.email=t@example.com",
                "commit",
                "-m",
                message,
            ],
        );
    }

    #[test]
    fn log_graph_filters_by_branch_author_and_remote() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), ["init", "-b", "main"]);
        commit_file_as(dir.path(), "Ann", "a.txt", "one");
        run(dir.path(), ["switch", "-c", "feature"]);
        commit_file_as(dir.path(), "Bob", "b.txt", "two");
        run(dir.path(), ["switch", "main"]);
        commit_file_as(dir.path(), "Ann", "c.txt", "three");
        // A remote-tracking ref without a remote.
        run(
            dir.path(),
            ["update-ref", "refs/remotes/origin/feature", "feature"],
        );

        let all = log_graph_filtered(dir.path(), 50, &LogFilter::default()).unwrap();
        assert_eq!(all.len(), 3);
        assert!(
            all.iter()
                .flat_map(|c| &c.refs)
                .any(|r| r.kind == GitRefKind::Remote)
        );

        let main_only = LogFilter {
            branches: vec!["main".into()],
            ..LogFilter::default()
        };
        let subjects: Vec<_> = log_graph_filtered(dir.path(), 50, &main_only)
            .unwrap()
            .into_iter()
            .map(|c| c.subject)
            .collect();
        assert_eq!(subjects, ["three", "one"]);

        let bob = LogFilter {
            authors: vec!["Bob".into()],
            ..LogFilter::default()
        };
        let commits = log_graph_filtered(dir.path(), 50, &bob).unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "two");
        assert!(commits[0].parents.is_empty(), "filtered parent is dropped");
        assert!(commits[0].outgoing.is_empty());

        let no_remotes = LogFilter {
            include_remotes: false,
            ..LogFilter::default()
        };
        let commits = log_graph_filtered(dir.path(), 50, &no_remotes).unwrap();
        assert_eq!(commits.len(), 3);
        assert!(
            !commits
                .iter()
                .flat_map(|c| &c.refs)
                .any(|r| r.kind == GitRefKind::Remote)
        );
    }

    #[test]
    fn log_graph_rejects_option_like_branch_names() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), ["init", "-b", "main"]);
        let filter = LogFilter {
            branches: vec!["--output=/tmp/x".into()],
            ..LogFilter::default()
        };
        assert!(log_graph_filtered(dir.path(), 5, &filter).is_err());
    }

    #[test]
    fn authors_lists_distinct_names_sorted() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), ["init", "-b", "main"]);
        commit_file_as(dir.path(), "Bob", "a.txt", "one");
        commit_file_as(dir.path(), "Ann", "b.txt", "two");
        commit_file_as(dir.path(), "Bob", "c.txt", "three");
        assert_eq!(authors(dir.path(), 50).unwrap(), ["Ann", "Bob"]);
    }

    #[test]
    fn branches_with_a_slash_are_local() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), ["init", "-b", "main"]);
        commit_file_as(dir.path(), "Ann", "a.txt", "one");
        run(dir.path(), ["branch", "feature/x"]);
        run(
            dir.path(),
            ["update-ref", "refs/remotes/origin/main", "main"],
        );
        let list = branches(dir.path()).unwrap();
        let find = |name: &str| list.iter().find(|b| b.name == name).unwrap().clone();
        assert!(!find("feature/x").remote);
        assert!(find("origin/main").remote);
        assert!(find("main").current);
    }

    #[test]
    fn log_graph_of_an_empty_repository_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), ["init", "-b", "main"]);
        assert!(log_graph(dir.path(), 10).unwrap().is_empty());
    }

    #[test]
    fn parse_refs_classifies_decorations() {
        let refs = parse_refs("HEAD -> main, origin/main, origin/HEAD, tag: v1, feature");
        let kinds: Vec<_> = refs.iter().map(|r| (r.name.as_str(), r.kind)).collect();
        assert_eq!(
            kinds,
            [
                ("main", GitRefKind::Head),
                ("origin/main", GitRefKind::Remote),
                ("v1", GitRefKind::Tag),
                ("feature", GitRefKind::Branch),
            ]
        );
    }

    #[test]
    fn creates_branch_and_tag_at_a_commit_and_rejects_bad_names() {
        let dir = tempfile::tempdir().unwrap();
        let id = ["-c", "user.name=Test", "-c", "user.email=t@example.com"];
        run(dir.path(), ["init", "-b", "main"]);
        fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        run(dir.path(), ["add", "a.txt"]);
        run(
            dir.path(),
            [id[0], id[1], id[2], id[3], "commit", "-m", "one"],
        );
        fs::write(dir.path().join("a.txt"), "b\n").unwrap();
        run(
            dir.path(),
            [id[0], id[1], id[2], id[3], "commit", "-am", "two"],
        );
        let commits = log_graph(dir.path(), 10).unwrap();
        let first = commits[1].hash.clone();

        create_tag(dir.path(), "v1", &first).unwrap();
        create_branch_at(dir.path(), "old", &first).unwrap();
        let status = read_status(dir.path()).unwrap().unwrap();
        assert_eq!(status.branch.as_deref(), Some("old"));
        let refs = log_graph(dir.path(), 10).unwrap();
        let on_first = refs.iter().find(|c| c.hash == first).unwrap();
        assert!(
            on_first
                .refs
                .iter()
                .any(|r| r.kind == GitRefKind::Tag && r.name == "v1")
        );

        checkout_commit(dir.path(), &commits[0].hash).unwrap();
        assert_eq!(
            read_status(dir.path()).unwrap().unwrap().branch,
            None,
            "detached"
        );

        assert!(create_branch_at(dir.path(), "-bad", &first).is_err());
        assert!(create_branch_at(dir.path(), "has space", &first).is_err());
        assert!(create_tag(dir.path(), "v1", &first).is_err(), "tag exists");
        assert!(checkout_commit(dir.path(), "--help").is_err());
        assert!(
            create_tag(dir.path(), "x", "main").is_err(),
            "only commit ids"
        );
    }

    #[test]
    fn compares_local_tags_with_origin_and_deletes_them() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        let remote = dir.path().join("remote.git");
        fs::create_dir(&work).unwrap();
        run(dir.path(), ["init", "--bare", "-b", "main", "remote.git"]);
        let id = ["-c", "user.name=Test", "-c", "user.email=t@example.com"];
        run(&work, ["init", "-b", "main"]);
        assert_eq!(remote_tags(&work).unwrap(), None, "no origin yet");
        run(&work, ["remote", "add", "origin", remote.to_str().unwrap()]);
        fs::write(work.join("a.txt"), "a\n").unwrap();
        run(&work, ["add", "a.txt"]);
        run(&work, [id[0], id[1], id[2], id[3], "commit", "-m", "one"]);
        let hash = log_graph(&work, 5).unwrap()[0].hash.clone();

        create_tag(&work, "v1", &hash).unwrap();
        create_tag(&work, "v2", &hash).unwrap();
        push_tag(&work, "v1").unwrap();
        let names = |tags: Option<BTreeSet<String>>| tags.unwrap().into_iter().collect::<Vec<_>>();
        assert_eq!(
            names(remote_tags(&work).unwrap()),
            ["v1"],
            "v2 is local only"
        );

        delete_remote_tag(&work, "v1").unwrap();
        assert!(names(remote_tags(&work).unwrap()).is_empty());
        let local = log_graph(&work, 5).unwrap();
        assert!(
            local[0].refs.iter().any(|r| r.name == "v1"),
            "deleting on origin keeps the local tag"
        );

        delete_tag(&work, "v1").unwrap();
        let after = log_graph(&work, 5).unwrap();
        assert!(!after[0].refs.iter().any(|r| r.name == "v1"));
        assert!(delete_tag(&work, "v1").is_err(), "already gone");
        assert!(delete_tag(&work, "-d").is_err());
        assert!(delete_remote_tag(&work, "a b").is_err());
    }

    #[test]
    fn remote_tags_fails_instead_of_hanging_on_an_unreachable_origin() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), ["init", "-b", "main"]);
        let missing = dir.path().join("does-not-exist.git");
        run(
            dir.path(),
            ["remote", "add", "origin", missing.to_str().unwrap()],
        );
        assert!(remote_tags(dir.path()).is_err());
    }

    /// A repository with a committer identity, so history-changing
    /// operations can run.
    fn identified_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), ["init", "-b", "main"]);
        run(dir.path(), ["config", "user.name", "Test"]);
        run(dir.path(), ["config", "user.email", "t@example.com"]);
        dir
    }

    fn commit_file(dir: &Path, name: &str, content: &str, message: &str) -> String {
        fs::write(dir.join(name), content).unwrap();
        run(dir, ["add", "-A"]);
        run(dir, ["commit", "-m", message]);
        head(dir)
    }

    fn head(dir: &Path) -> String {
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn subjects(dir: &Path) -> Vec<String> {
        log_graph(dir, 50)
            .unwrap()
            .into_iter()
            .map(|c| c.subject)
            .collect()
    }

    fn is_clean(dir: &Path) -> bool {
        read_status(dir).unwrap().unwrap().files.is_empty()
    }

    #[test]
    fn cherry_pick_and_revert_apply_one_commit() {
        let repo = identified_repo();
        let d = repo.path();
        commit_file(d, "a.txt", "a\n", "base");
        run(d, ["switch", "-c", "feature"]);
        let picked = commit_file(d, "f.txt", "f\n", "add f");
        run(d, ["switch", "main"]);
        assert!(!d.join("f.txt").exists());

        cherry_pick(d, &picked).unwrap();
        assert!(d.join("f.txt").exists());
        assert_eq!(subjects(d)[0], "add f");

        revert(d, &head(d)).unwrap();
        assert!(!d.join("f.txt").exists());
        assert!(subjects(d)[0].starts_with("Revert"));
        assert!(is_clean(d));
    }

    #[test]
    fn conflicts_are_aborted_and_leave_the_repository_untouched() {
        let repo = identified_repo();
        let d = repo.path();
        commit_file(d, "a.txt", "base\n", "base");
        run(d, ["switch", "-c", "feature"]);
        let theirs = commit_file(d, "a.txt", "theirs\n", "theirs");
        run(d, ["switch", "main"]);
        let ours = commit_file(d, "a.txt", "ours\n", "ours");

        for result in [
            cherry_pick(d, &theirs),
            merge(d, &theirs),
            rebase_onto(d, &theirs),
        ] {
            let error = result.unwrap_err().to_string();
            assert!(error.contains("aborted"), "{error}");
            assert_eq!(in_progress(d), None);
            assert_eq!(head(d), ours, "HEAD unchanged");
            assert!(is_clean(d));
            assert_eq!(fs::read_to_string(d.join("a.txt")).unwrap(), "ours\n");
        }
    }

    #[test]
    fn refuses_to_start_while_another_operation_is_unfinished() {
        let repo = identified_repo();
        let d = repo.path();
        commit_file(d, "a.txt", "base\n", "base");
        run(d, ["switch", "-c", "feature"]);
        let theirs = commit_file(d, "a.txt", "theirs\n", "theirs");
        run(d, ["switch", "main"]);
        commit_file(d, "a.txt", "ours\n", "ours");
        // Leave a conflicted cherry-pick behind, as a terminal user could.
        let _ = Command::new("git")
            .args(["cherry-pick", &theirs])
            .current_dir(d)
            .output();
        assert!(in_progress(d).is_some());
        let error = merge(d, &theirs).unwrap_err().to_string();
        assert!(error.contains("already in progress"), "{error}");
        assert!(
            in_progress(d).is_some(),
            "the user's operation is not aborted"
        );
    }

    #[test]
    fn merge_commits_cannot_be_cherry_picked_reverted_or_dropped() {
        let repo = identified_repo();
        let d = repo.path();
        commit_file(d, "a.txt", "a\n", "base");
        run(d, ["switch", "-c", "feature"]);
        commit_file(d, "f.txt", "f\n", "on feature");
        run(d, ["switch", "main"]);
        commit_file(d, "m.txt", "m\n", "on main");
        run(d, ["merge", "--no-ff", "feature", "-m", "merge"]);
        let merged = head(d);
        assert!(cherry_pick(d, &merged).is_err());
        assert!(revert(d, &merged).is_err());
        assert!(drop_commit(d, &merged).is_err());
        let root = log_graph(d, 50).unwrap().pop().unwrap().hash;
        assert!(
            drop_commit(d, &root).is_err(),
            "a root commit has no parent"
        );
    }

    #[test]
    fn fixup_and_squash_commit_staged_changes_only() {
        let repo = identified_repo();
        let d = repo.path();
        let target = commit_file(d, "a.txt", "a\n", "target");
        commit_file(d, "b.txt", "b\n", "later");

        let error = commit_fixup(d, &target).unwrap_err().to_string();
        assert!(error.contains("nothing is staged"), "{error}");

        fs::write(d.join("a.txt"), "a2\n").unwrap();
        run(d, ["add", "a.txt"]);
        commit_fixup(d, &target).unwrap();
        assert_eq!(subjects(d)[0], "fixup! target");

        fs::write(d.join("a.txt"), "a3\n").unwrap();
        run(d, ["add", "a.txt"]);
        commit_squash(d, &target).unwrap();
        assert_eq!(subjects(d)[0], "squash! target");
    }

    #[test]
    fn reset_moves_the_branch_and_keeps_or_discards_changes() {
        let repo = identified_repo();
        let d = repo.path();
        let first = commit_file(d, "a.txt", "one\n", "one");
        commit_file(d, "a.txt", "two\n", "two");

        reset_last_commit_soft(d).unwrap();
        assert_eq!(head(d), first);
        let status = read_status(d).unwrap().unwrap();
        assert!(
            status.files.iter().any(|f| f.path == "a.txt" && f.staged),
            "kept staged"
        );

        run(d, ["commit", "-am", "two again"]);
        reset_to(d, &first, GitResetMode::Mixed).unwrap();
        let status = read_status(d).unwrap().unwrap();
        assert!(
            status.files.iter().any(|f| f.path == "a.txt" && !f.staged),
            "kept unstaged"
        );
        assert_eq!(fs::read_to_string(d.join("a.txt")).unwrap(), "two\n");

        reset_to(d, &first, GitResetMode::Hard).unwrap();
        assert!(is_clean(d));
        assert_eq!(fs::read_to_string(d.join("a.txt")).unwrap(), "one\n");

        let only = log_graph(d, 5).unwrap().pop().unwrap().hash;
        reset_to(d, &only, GitResetMode::Soft).unwrap();
        assert!(
            reset_last_commit_soft(d).is_err(),
            "first commit has no parent"
        );
    }

    #[test]
    fn merge_rebase_and_drop_rewrite_the_current_branch() {
        let repo = identified_repo();
        let d = repo.path();
        commit_file(d, "a.txt", "a\n", "base");
        run(d, ["switch", "-c", "feature"]);
        let feature = commit_file(d, "f.txt", "f\n", "feature work");
        run(d, ["switch", "main"]);
        let main_tip = commit_file(d, "m.txt", "m\n", "main work");

        merge(d, &feature).unwrap();
        assert!(d.join("f.txt").exists() && d.join("m.txt").exists());
        reset_to(d, &main_tip, GitResetMode::Hard).unwrap();

        run(d, ["switch", "feature"]);
        rebase_onto(d, &main_tip).unwrap();
        assert!(d.join("m.txt").exists(), "feature now sits on main");
        let list = subjects(d);
        assert_eq!(&list[..2], ["feature work", "main work"]);

        let middle = commit_file(d, "g.txt", "g\n", "middle");
        commit_file(d, "h.txt", "h\n", "top");
        drop_commit(d, &middle).unwrap();
        assert!(!d.join("g.txt").exists(), "dropped commit's change is gone");
        assert!(d.join("h.txt").exists(), "later commits stay");
        assert!(!subjects(d).contains(&"middle".to_owned()));
        assert!(is_clean(d));

        // The tip can be dropped too.
        let tip_before = head(d);
        drop_commit(d, &tip_before).unwrap();
        assert_ne!(head(d), tip_before);
        assert!(!d.join("h.txt").exists());

        // A commit on another branch is refused, and nothing changes.
        run(d, ["switch", "main"]);
        let before = head(d);
        let error = drop_commit(d, &feature).unwrap_err().to_string();
        assert!(error.contains("not part of the current branch"), "{error}");
        assert_eq!(head(d), before);
        assert_eq!(in_progress(d), None);
    }

    #[test]
    fn dirty_tree_stops_a_rebase_without_leaving_state() {
        let repo = identified_repo();
        let d = repo.path();
        let base = commit_file(d, "a.txt", "a\n", "base");
        commit_file(d, "b.txt", "b\n", "next");
        fs::write(d.join("a.txt"), "dirty\n").unwrap();
        assert!(rebase_onto(d, &base).is_err());
        assert_eq!(in_progress(d), None);
        assert_eq!(fs::read_to_string(d.join("a.txt")).unwrap(), "dirty\n");
    }

    #[test]
    fn detects_whether_a_commit_was_pushed() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        fs::create_dir(&work).unwrap();
        run(dir.path(), ["init", "--bare", "-b", "main", "remote.git"]);
        run(&work, ["init", "-b", "main"]);
        run(&work, ["config", "user.name", "Test"]);
        run(&work, ["config", "user.email", "t@example.com"]);
        run(
            &work,
            [
                "remote",
                "add",
                "origin",
                dir.path().join("remote.git").to_str().unwrap(),
            ],
        );
        let pushed = commit_file(&work, "a.txt", "a\n", "pushed");
        assert!(!is_on_remote(&work, &pushed).unwrap(), "nothing pushed yet");
        run(&work, ["push", "-u", "origin", "main"]);
        let local = commit_file(&work, "b.txt", "b\n", "local");
        assert!(is_on_remote(&work, &pushed).unwrap());
        assert!(!is_on_remote(&work, &local).unwrap());
    }

    #[test]
    fn compares_two_commits_file_by_file() {
        let repo = identified_repo();
        let d = repo.path();
        let from = commit_file(d, "keep.txt", "k\n", "one");
        fs::write(d.join("old.txt"), "same content\nline 2\nline 3\n").unwrap();
        run(d, ["add", "-A"]);
        run(d, ["commit", "-m", "two"]);
        let mid = head(d);
        fs::write(d.join("keep.txt"), "k changed\n").unwrap();
        run(d, ["mv", "old.txt", "new.txt"]);
        fs::write(d.join("added.txt"), "x\n").unwrap();
        run(d, ["add", "-A"]);
        run(d, ["commit", "-m", "three"]);
        let to = head(d);

        let files = compare_files(d, &mid, &to).unwrap();
        let summary: Vec<_> = files
            .iter()
            .map(|f| (f.path.as_str(), f.old_path.as_deref(), f.status))
            .collect();
        assert_eq!(
            summary,
            [
                ("added.txt", None, 'A'),
                ("keep.txt", None, 'M'),
                ("new.txt", Some("old.txt"), 'R'),
            ]
        );
        let diff = compare_diff(d, &mid, &to, "keep.txt", None).unwrap();
        assert!(
            diff.contains("-k\n") && diff.contains("+k changed"),
            "{diff}"
        );
        let renamed = compare_diff(d, &mid, &to, "new.txt", Some("old.txt")).unwrap();
        assert!(renamed.contains("rename from old.txt"), "{renamed}");
        assert!(compare_files(d, &from, "--help").is_err());
        assert!(compare_diff(d, &from, &to, "", None).is_err());
    }

    fn commit_as(dir: &Path, name: &str, message: &str) {
        run(
            dir,
            [
                "-c",
                &format!("user.name={name}"),
                "-c",
                &format!("user.email={}@example.com", name.to_lowercase()),
                "commit",
                "-m",
                message,
            ],
        );
    }

    #[test]
    fn blame_names_the_author_of_each_line_and_flags_unsaved_edits() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), ["init", "-b", "main"]);
        fs::write(dir.path().join("f.txt"), "one\ntwo\n").unwrap();
        run(dir.path(), ["add", "f.txt"]);
        commit_as(dir.path(), "Alice", "first");
        fs::write(dir.path().join("f.txt"), "one\nTWO\nthree\n").unwrap();
        run(dir.path(), ["add", "f.txt"]);
        commit_as(dir.path(), "Bob", "shout two");

        let blame = blame(dir.path(), "f.txt", "one\nTWO\nthree\nfour\n")
            .unwrap()
            .unwrap();
        assert_eq!(blame.line_commit.len(), 4);
        let at = |line: usize| &blame.commits[blame.line_commit[line] as usize];
        assert_eq!(at(0).author, "Alice");
        assert_eq!(at(0).email, "alice@example.com");
        assert_eq!(at(0).summary, "first");
        assert_eq!(at(1).author, "Bob");
        assert_eq!(at(1).summary, "shout two");
        assert!(at(1).time > 0 && !at(1).uncommitted);
        assert_eq!(at(1).sha.len(), 40);
        // Bob wrote line 3 too, in the same commit.
        assert_eq!(blame.line_commit[1], blame.line_commit[2]);
        // The unsaved fourth line belongs to no commit yet.
        assert!(at(3).uncommitted);
    }

    #[test]
    fn blame_is_none_for_untracked_files() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), ["init", "-b", "main"]);
        fs::write(dir.path().join("new.txt"), "x\n").unwrap();
        assert_eq!(blame(dir.path(), "new.txt", "x\n").unwrap(), None);
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
