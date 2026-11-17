//! Workspace file watcher (M2-02).
//!
//! FSEvents (through `notify`) reports every created, changed, renamed and
//! deleted path under the workspace root. Events are batched: a batch is
//! sent once the stream has been quiet for `QUIET`, or at the latest
//! `MAX_DELAY` after its first event, so `git checkout` or a build produces
//! a few batches instead of thousands of callbacks.
//!
//! Paths in a batch are relative to the root, `/` separated, sorted and
//! deduplicated. Changes inside `.git` are reduced to a `git_changed` signal;
//! changes inside built-in excluded folders (`node_modules`, `target`, ...)
//! are dropped here. `.gitignore` rules are applied later by
//! `Workspace::index_delta`, which checks the disk anyway. When events may
//! have been lost (FSEvents asks for a
//! rescan, the root itself changed, or a batch grows past
//! `MAX_BATCH_PATHS`), the batch carries `rescan` instead of paths.

use std::collections::BTreeSet;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::workspace::DEFAULT_EXCLUDES;

/// A batch is sent after this long without new events.
pub const QUIET: Duration = Duration::from_millis(60);
/// ... or this long after its first event, whichever comes first.
pub const MAX_DELAY: Duration = Duration::from_millis(300);
/// Above this many paths a batch becomes a rescan.
pub const MAX_BATCH_PATHS: usize = 1_000;

/// Changed paths since the previous batch.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChangeBatch {
    /// Relative, `/`-separated, sorted, without duplicates. Empty when
    /// `rescan` is set.
    pub paths: Vec<String>,
    /// Anything may have changed: re-read everything.
    pub rescan: bool,
    /// Git metadata changed without exposing `.git` paths to workspace consumers.
    pub git_changed: bool,
}

/// Collects events into one batch. Separate from the thread so it can be
/// tested without FSEvents.
#[derive(Debug)]
pub(crate) struct Batcher {
    /// The root as given and, if different, its canonical form: FSEvents
    /// reports `/private/var/...` for a root opened as `/var/...`.
    roots: Vec<PathBuf>,
    paths: BTreeSet<String>,
    rescan: bool,
    git_changed: bool,
}

impl Batcher {
    pub(crate) fn new(root: &Path) -> Self {
        let mut roots = vec![root.to_path_buf()];
        if let Ok(canonical) = root.canonicalize()
            && canonical != root
        {
            roots.push(canonical);
        }
        Self {
            roots,
            paths: BTreeSet::new(),
            rescan: false,
            git_changed: false,
        }
    }

    pub(crate) fn add(&mut self, event: notify::Result<notify::Event>) {
        match event {
            Err(e) => {
                tracing::warn!(error = %e, "file watcher error; rescanning");
                self.set_rescan();
            }
            Ok(event) if event.need_rescan() => self.set_rescan(),
            // Reads do not change anything.
            Ok(event) if matches!(event.kind, EventKind::Access(_)) => {}
            Ok(event) => {
                for path in &event.paths {
                    self.push(path);
                }
            }
        }
    }

    pub(crate) fn push(&mut self, path: &Path) {
        if self.rescan {
            return;
        }
        let Some(rel) = self.relative(path) else {
            return;
        };
        if rel.is_empty() {
            // The root itself was renamed, deleted or replaced.
            return self.set_rescan();
        }
        if rel == ".git" || rel.starts_with(".git/") {
            self.git_changed = true;
            return;
        }
        if !is_relevant(&rel) {
            return;
        }
        self.paths.insert(rel);
        if self.paths.len() > MAX_BATCH_PATHS {
            self.set_rescan();
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        !self.rescan && self.paths.is_empty() && !self.git_changed
    }

    pub(crate) fn take(&mut self) -> ChangeBatch {
        ChangeBatch {
            paths: std::mem::take(&mut self.paths).into_iter().collect(),
            rescan: std::mem::take(&mut self.rescan),
            git_changed: std::mem::take(&mut self.git_changed),
        }
    }

    fn set_rescan(&mut self) {
        self.rescan = true;
        self.paths.clear();
    }

    fn relative(&self, path: &Path) -> Option<String> {
        let rel = self.roots.iter().find_map(|r| path.strip_prefix(r).ok())?;
        let mut parts = Vec::new();
        for c in rel.components() {
            match c {
                Component::Normal(p) => parts.push(p.to_string_lossy()),
                Component::CurDir => {}
                _ => return None,
            }
        }
        Some(parts.join("/"))
    }
}

/// `false` for paths the workspace never shows: anything in `.git`, inside
/// an excluded folder, or one of our own temp files from an atomic save.
fn is_relevant(rel: &str) -> bool {
    let mut parts = rel.split('/').peekable();
    while let Some(part) = parts.next() {
        let last = parts.peek().is_none();
        if part == ".git" {
            return false;
        }
        if !last && DEFAULT_EXCLUDES.contains(&part) {
            return false;
        }
        if last && part.starts_with(".galaxy-") && part.ends_with(".tmp") {
            return false;
        }
    }
    true
}

/// Watches one workspace until dropped. `sink` runs on the watcher's own
/// thread, once per batch.
pub struct WorkspaceWatcher {
    _watcher: RecommendedWatcher,
    stopped: Arc<AtomicBool>,
}

impl WorkspaceWatcher {
    pub fn start(
        root: &Path,
        mut sink: impl FnMut(ChangeBatch) + Send + 'static,
    ) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher = notify::recommended_watcher(move |event| {
            // The batching thread is gone once the watcher stops.
            let _ = tx.send(event);
        })
        .map_err(io::Error::other)?;
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(io::Error::other)?;

        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let mut batcher = Batcher::new(root);
        thread::Builder::new()
            .name("galaxy-watcher".into())
            .spawn(move || {
                while let Ok(first) = rx.recv() {
                    batcher.add(first);
                    let start = Instant::now();
                    loop {
                        let left = MAX_DELAY.saturating_sub(start.elapsed());
                        if left.is_zero() {
                            break;
                        }
                        match rx.recv_timeout(QUIET.min(left)) {
                            Ok(event) => batcher.add(event),
                            Err(RecvTimeoutError::Timeout) => break,
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    if stop.load(Ordering::Acquire) {
                        return;
                    }
                    if !batcher.is_empty() {
                        let batch = batcher.take();
                        tracing::debug!(
                            paths = batch.paths.len(),
                            rescan = batch.rescan,
                            "workspace changed"
                        );
                        sink(batch);
                    }
                }
            })?;
        tracing::info!(root = %root.display(), "watching workspace");
        Ok(Self {
            _watcher: watcher,
            stopped,
        })
    }
}

impl Drop for WorkspaceWatcher {
    fn drop(&mut self) {
        // No more batches, even one already being collected.
        self.stopped.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::mpsc::Receiver;

    fn batcher() -> Batcher {
        Batcher::new(Path::new("/w"))
    }

    #[test]
    fn paths_become_relative_sorted_and_unique() {
        let mut b = batcher();
        for p in ["/w/src/b.rs", "/w/a.txt", "/w/src/b.rs", "/elsewhere/x"] {
            b.push(Path::new(p));
        }
        assert_eq!(
            b.take(),
            ChangeBatch {
                paths: vec!["a.txt".into(), "src/b.rs".into()],
                rescan: false,
                git_changed: false,
            }
        );
        assert!(b.is_empty(), "take starts a new batch");
    }

    #[test]
    fn git_changes_are_signalled_while_excluded_paths_are_dropped() {
        let mut b = batcher();
        for p in [
            "/w/.git/index",
            "/w/.git",
            "/w/node_modules/pkg/index.js",
            "/w/target/debug/app",
            "/w/src/.galaxy-ab12.tmp",
        ] {
            b.push(Path::new(p));
        }
        let batch = b.take();
        assert!(batch.paths.is_empty());
        assert!(batch.git_changed);
        // A file named like an excluded folder is still a file.
        b.push(Path::new("/w/build"));
        b.push(Path::new("/w/src/.galaxy-notes.md"));
        assert_eq!(b.take().paths, ["build", "src/.galaxy-notes.md"]);
    }

    #[test]
    fn lost_events_root_changes_and_huge_batches_rescan() {
        let mut b = batcher();
        b.push(Path::new("/w/a"));
        b.add(Err(notify::Error::generic("overflow")));
        assert_eq!(
            b.take(),
            ChangeBatch {
                paths: vec![],
                rescan: true,
                git_changed: false,
            }
        );

        b.push(Path::new("/w"));
        assert!(b.take().rescan, "the root itself changed");

        for i in 0..=MAX_BATCH_PATHS {
            b.push(Path::new(&format!("/w/f{i}")));
        }
        let batch = b.take();
        assert!(batch.rescan);
        assert!(batch.paths.is_empty());
    }

    #[test]
    fn reads_are_ignored() {
        let mut b = batcher();
        let read = notify::Event::new(EventKind::Access(notify::event::AccessKind::Any))
            .add_path("/w/a".into());
        b.add(Ok(read));
        assert!(b.is_empty());
        let write = notify::Event::new(EventKind::Modify(notify::event::ModifyKind::Any))
            .add_path("/w/a".into());
        b.add(Ok(write));
        assert_eq!(b.take().paths, ["a"]);
    }

    fn start(root: &Path) -> (WorkspaceWatcher, Receiver<ChangeBatch>) {
        let (tx, rx) = mpsc::channel();
        let w = WorkspaceWatcher::start(root, move |b| {
            let _ = tx.send(b);
        })
        .unwrap();
        // Let FSEvents start the stream before the test touches files. CI
        // VMs start it slowly, so wait longer than a local run needs, then
        // drop late events from the test's own setup.
        thread::sleep(STARTUP);
        while rx.try_recv().is_ok() {}
        (w, rx)
    }

    /// How long FSEvents gets to start, and to deliver a test's events.
    const STARTUP: Duration = Duration::from_secs(1);
    const DELIVERY: Duration = Duration::from_secs(15);

    /// Collects batches until `done` holds or `DELIVERY` passes.
    fn collect(
        rx: &Receiver<ChangeBatch>,
        done: impl Fn(&BTreeSet<String>) -> bool,
    ) -> (BTreeSet<String>, usize) {
        let mut seen = BTreeSet::new();
        let mut batches = 0;
        let deadline = Instant::now() + DELIVERY;
        while !done(&seen) {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(b) => {
                    batches += 1;
                    assert!(!b.rescan, "unexpected rescan");
                    seen.extend(b.paths);
                }
                Err(_) => break,
            }
        }
        (seen, batches)
    }

    #[test]
    fn reports_created_changed_and_deleted_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("old.txt"), "x").unwrap();
        let (_w, rx) = start(dir.path());

        fs::write(dir.path().join("src/new.rs"), "fn main() {}").unwrap();
        fs::remove_file(dir.path().join("old.txt")).unwrap();
        let (seen, _) = collect(&rx, |s| s.contains("src/new.rs") && s.contains("old.txt"));
        assert!(seen.contains("src/new.rs"), "{seen:?}");
        assert!(seen.contains("old.txt"), "{seen:?}");
    }

    #[test]
    fn reports_git_metadata_without_exposing_git_paths() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        let (_w, rx) = start(dir.path());

        fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let batch = rx.recv_timeout(DELIVERY).unwrap();
        assert!(batch.git_changed);
        assert!(batch.paths.is_empty());
        assert!(!batch.rescan);
    }

    #[test]
    fn a_burst_of_writes_arrives_in_few_batches() {
        let dir = tempfile::tempdir().unwrap();
        let (_w, rx) = start(dir.path());
        for i in 0..200 {
            fs::write(dir.path().join(format!("f{i:03}.txt")), "x").unwrap();
        }
        let (seen, batches) = collect(&rx, |s| s.len() >= 200);
        assert_eq!(seen.len(), 200);
        assert!(batches <= 5, "{batches} batches for one burst");
    }

    #[test]
    fn excluded_folders_stay_quiet_and_drop_stops_the_watcher() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        let (w, rx) = start(dir.path());
        fs::write(dir.path().join("node_modules/pkg/a.js"), "x").unwrap();
        fs::write(dir.path().join("seen.txt"), "x").unwrap();
        let (seen, _) = collect(&rx, |s| s.contains("seen.txt"));
        assert_eq!(seen.into_iter().collect::<Vec<_>>(), ["seen.txt"]);

        drop(w);
        fs::write(dir.path().join("after.txt"), "x").unwrap();
        assert!(
            rx.recv_timeout(Duration::from_millis(800)).is_err(),
            "no batches after drop"
        );
    }
}
