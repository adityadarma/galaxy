//! Workspace: lazy, gitignore-aware directory listing (M2-01, M2-04).
//!
//! The explorer never scans the whole tree. Each call lists exactly one
//! directory, so opening a 100k-file repository costs one `readdir`.
//!
//! Paths crossing the API are **relative to the workspace root**, `/`
//! separated, with `""` meaning the root. `resolve` rejects anything that
//! would leave the root (`..`, absolute paths), so a stale or hostile path
//! from the UI cannot read outside the workspace.
//!
//! Ignore rules: `.gitignore` and `.ignore` files apply even outside a git
//! repository, and ancestors of the listed directory are consulted so the
//! root's rules reach nested folders. Dotfiles are shown (like VSCode).
//! The built-in excludes from the PRD (`.git`, `node_modules`, `target`,
//! `dist`, `build`) are hidden when they are directories.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use ignore::{DirEntry as WalkEntry, WalkBuilder};

/// Directories hidden regardless of ignore files (PRD 7.1).
pub const DEFAULT_EXCLUDES: &[&str] = &[".git", "node_modules", "target", "dist", "build"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    /// Path relative to the workspace root, `/` separated.
    pub rel_path: String,
    /// `true` for directories, including symlinks that point to one.
    pub is_dir: bool,
}

/// Compact identifier for one path stored in a [`PathArena`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PathId(usize);

/// Interns path components once and represents paths as slices of component
/// IDs. A source tree repeats names such as `src`, `tests`, and `mod.rs`
/// heavily; this avoids one `String` allocation per occurrence in a large
/// workspace index.
#[derive(Debug, Default, Clone)]
pub struct PathArena {
    components: Vec<Box<str>>,
    component_ids: HashMap<Box<str>, u32>,
    paths: Vec<Box<[u32]>>,
}

impl PathArena {
    fn intern(&mut self, path: &Path) -> PathId {
        let mut ids = Vec::new();
        for component in path.components() {
            let Component::Normal(part) = component else {
                continue;
            };
            let name: Box<str> = part.to_string_lossy().into_owned().into_boxed_str();
            let id = if let Some(&id) = self.component_ids.get(name.as_ref()) {
                id
            } else {
                let id =
                    u32::try_from(self.components.len()).expect("too many unique path components");
                self.component_ids.insert(name.clone(), id);
                self.components.push(name);
                id
            };
            ids.push(id);
        }
        let id = PathId(self.paths.len());
        self.paths.push(ids.into_boxed_slice());
        id
    }

    fn display(&self, id: PathId) -> Option<String> {
        let path = self.paths.get(id.0)?;
        Some(
            path.iter()
                .map(|&part| self.components[part as usize].as_ref())
                .collect::<Vec<_>>()
                .join("/"),
        )
    }

    /// Number of unique filename/path components retained by the arena.
    pub fn component_count(&self) -> usize {
        self.components.len()
    }
}

/// File-only index of a workspace. Directories stay out of the index because
/// the explorer loads them lazily; consumers such as Go to File need only
/// candidate files.
#[derive(Debug, Default, Clone)]
pub struct WorkspaceIndex {
    arena: PathArena,
    files: Vec<PathId>,
}

impl WorkspaceIndex {
    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Relative `/`-separated path for a file ID in index order.
    pub fn path(&self, index: usize) -> Option<String> {
        self.files.get(index).and_then(|&id| self.arena.display(id))
    }

    pub fn component_count(&self) -> usize {
        self.arena.component_count()
    }

    /// Conservative size of allocations directly owned by this index. It
    /// intentionally excludes allocator bookkeeping and the source tree's
    /// own filesystem caches, so benchmark output remains comparable across
    /// runs.
    pub fn estimated_memory_bytes(&self) -> usize {
        use std::mem::size_of;

        let component_bytes = self
            .arena
            .components
            .iter()
            .map(|component| component.len())
            .sum::<usize>();
        let path_component_bytes = self
            .arena
            .paths
            .iter()
            .map(|path| path.len() * size_of::<u32>())
            .sum::<usize>();
        component_bytes
            + path_component_bytes
            + self.arena.components.capacity() * size_of::<Box<str>>()
            + self.arena.paths.capacity() * size_of::<Box<[u32]>>()
            + self.files.capacity() * size_of::<PathId>()
            + self.arena.component_ids.capacity() * size_of::<(Box<str>, u32)>()
    }

    fn push(&mut self, rel: &Path) {
        let id = self.arena.intern(rel);
        self.files.push(id);
    }
}

#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        if !fs::metadata(&root)?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "not a directory",
            ));
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Folder name for titles; falls back to the full path for `/`.
    pub fn name(&self) -> String {
        self.root.file_name().map_or_else(
            || self.root.to_string_lossy().into_owned(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    /// Absolute path of `rel`, refusing to leave the workspace.
    pub fn resolve(&self, rel: &str) -> io::Result<PathBuf> {
        let mut out = self.root.clone();
        for component in Path::new(rel).components() {
            match component {
                Component::Normal(part) => out.push(part),
                Component::CurDir => {}
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "path escapes the workspace",
                    ));
                }
            }
        }
        Ok(out)
    }

    /// Lists one directory: folders first, then files, each sorted
    /// case-insensitively. Unreadable entries are skipped.
    pub fn list_dir(&self, rel: &str) -> io::Result<Vec<DirEntry>> {
        let dir = self.resolve(rel)?;
        if !fs::metadata(&dir)?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "not a directory",
            ));
        }

        let walker = WalkBuilder::new(&dir)
            .max_depth(Some(1))
            .hidden(false)
            .ignore(true)
            .git_ignore(true)
            .git_global(false)
            .git_exclude(false)
            .require_git(false)
            .parents(true)
            .follow_links(false)
            .filter_entry(|e| e.depth() == 0 || !is_default_excluded(e))
            .build();

        let mut entries: Vec<DirEntry> = walker
            .filter_map(Result::ok)
            .filter(|e| e.depth() == 1)
            .filter_map(|e| {
                let path = e.path();
                let rel_path = path.strip_prefix(&self.root).ok()?;
                Some(DirEntry {
                    name: e.file_name().to_string_lossy().into_owned(),
                    rel_path: rel_path
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/"),
                    // `is_dir` follows symlinks, so linked folders expand.
                    is_dir: path.is_dir(),
                })
            })
            .collect();

        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(entries)
    }

    /// Walks the workspace once and builds a compact, gitignore-aware file
    /// index. This is intentionally explicit rather than part of `open`: the
    /// explorer can remain instant for large repositories, while Go to File
    /// can request the index when it needs it.
    pub fn build_file_index(&self) -> WorkspaceIndex {
        let walker = WalkBuilder::new(&self.root)
            .hidden(false)
            .ignore(true)
            .git_ignore(true)
            .git_global(false)
            .git_exclude(false)
            .require_git(false)
            .parents(true)
            .follow_links(false)
            .filter_entry(|e| e.depth() == 0 || !is_default_excluded(e))
            .build();

        let mut index = WorkspaceIndex::default();
        for entry in walker.filter_map(Result::ok) {
            if entry.depth() == 0 || entry.file_type().is_some_and(|kind| kind.is_dir()) {
                continue;
            }
            if let Ok(rel) = entry.path().strip_prefix(&self.root) {
                index.push(rel);
            }
        }
        index
    }
}

fn is_default_excluded(entry: &WalkEntry) -> bool {
    let name = entry.file_name().to_string_lossy();
    if name == ".git" {
        // Also a file in worktrees and submodules.
        return true;
    }
    DEFAULT_EXCLUDES.contains(&name.as_ref()) && entry.file_type().is_some_and(|t| t.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        for d in [
            "src/deep",
            "Zeta",
            "alpha",
            "node_modules/pkg",
            "target/debug",
            ".git",
            "dist",
            ".github",
        ] {
            fs::create_dir_all(r.join(d)).unwrap();
        }
        for f in [
            "README.md",
            "Cargo.toml",
            ".gitignore",
            "src/main.rs",
            "src/deep/mod.rs",
            "node_modules/pkg/index.js",
            "build", // a FILE named like an excluded directory stays visible
        ] {
            fs::write(r.join(f), "x").unwrap();
        }
        let ws = Workspace::open(r).unwrap();
        (dir, ws)
    }

    fn names(entries: &[DirEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn folders_first_sorted_case_insensitively_with_dotfiles() {
        let (_dir, ws) = tree();
        let entries = ws.list_dir("").unwrap();
        assert_eq!(
            names(&entries),
            vec![
                ".github",
                "alpha",
                "src",
                "Zeta",
                ".gitignore",
                "build",
                "Cargo.toml",
                "README.md"
            ]
        );
        assert!(entries[..4].iter().all(|e| e.is_dir));
        assert!(entries[4..].iter().all(|e| !e.is_dir));
    }

    #[test]
    fn default_excludes_hide_directories_only() {
        let (_dir, ws) = tree();
        let listed = ws.list_dir("").unwrap();
        let n = names(&listed);
        for hidden in [".git", "node_modules", "target", "dist"] {
            assert!(!n.contains(&hidden), "{hidden} should be hidden");
        }
        assert!(n.contains(&"build"), "a file named build is a normal file");
    }

    #[test]
    fn lists_one_directory_with_relative_paths() {
        let (_dir, ws) = tree();
        let entries = ws.list_dir("src").unwrap();
        let got: Vec<_> = entries
            .iter()
            .map(|e| (e.rel_path.as_str(), e.is_dir))
            .collect();
        assert_eq!(got, vec![("src/deep", true), ("src/main.rs", false)]);
        // Nested listing composes with the returned relative path.
        let deep = ws.list_dir(&entries[0].rel_path).unwrap();
        assert_eq!(deep[0].rel_path, "src/deep/mod.rs");
        assert_eq!(ws.list_dir("./src").unwrap().len(), 2);
    }

    #[test]
    fn gitignore_applies_without_a_git_repo_and_reaches_subdirectories() {
        let (dir, ws) = tree();
        fs::write(dir.path().join(".gitignore"), "*.log\n/alpha/\n*.tmp\n").unwrap();
        fs::write(dir.path().join("debug.log"), "x").unwrap();
        fs::write(dir.path().join("src/scratch.tmp"), "x").unwrap();

        let root = ws.list_dir("").unwrap();
        let n = names(&root);
        assert!(!n.contains(&"debug.log"));
        assert!(!n.contains(&"alpha"));
        // The root's rules apply when listing a nested directory.
        assert!(!names(&ws.list_dir("src").unwrap()).contains(&"scratch.tmp"));
    }

    #[test]
    fn nested_gitignore_and_ignore_files_apply() {
        let (dir, ws) = tree();
        fs::write(dir.path().join("src/.gitignore"), "generated.rs\n").unwrap();
        fs::write(dir.path().join("src/generated.rs"), "x").unwrap();
        fs::write(dir.path().join(".ignore"), "Cargo.toml\n").unwrap();

        assert!(!names(&ws.list_dir("src").unwrap()).contains(&"generated.rs"));
        assert!(!names(&ws.list_dir("").unwrap()).contains(&"Cargo.toml"));
    }

    #[test]
    fn paths_cannot_escape_the_workspace() {
        let (_dir, ws) = tree();
        for bad in ["..", "../x", "src/../..", "/etc", "src/../../.."] {
            let err = ws.list_dir(bad).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{bad}");
            assert_eq!(
                ws.resolve(bad).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        assert_eq!(ws.resolve("").unwrap(), ws.root());
        assert_eq!(
            ws.resolve("src/main.rs").unwrap(),
            ws.root().join("src/main.rs")
        );
    }

    #[test]
    fn errors_for_missing_and_non_directories() {
        let (dir, ws) = tree();
        assert_eq!(
            ws.list_dir("nope").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            ws.list_dir("README.md").unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        assert!(Workspace::open(dir.path().join("README.md")).is_err());
        assert!(Workspace::open(dir.path().join("missing")).is_err());
    }

    #[test]
    fn file_index_respects_ignores_and_interns_components() {
        let (dir, ws) = tree();
        fs::create_dir_all(dir.path().join("src/other")).unwrap();
        fs::write(dir.path().join("src/other/main.rs"), "x").unwrap();
        fs::write(dir.path().join("src/other/mod.rs"), "x").unwrap();
        fs::write(dir.path().join("ignored.log"), "x").unwrap();
        fs::write(dir.path().join(".gitignore"), "*.log\n").unwrap();

        let index = ws.build_file_index();
        let mut paths = (0..index.len())
            .map(|id| index.path(id).unwrap())
            .collect::<Vec<_>>();
        paths.sort();
        assert!(paths.contains(&"src/main.rs".to_string()));
        assert!(paths.contains(&"src/other/main.rs".to_string()));
        assert!(paths.contains(&"src/other/mod.rs".to_string()));
        assert!(!paths.iter().any(|path| path.starts_with("node_modules/")));
        assert!(!paths.iter().any(|path| path == "ignored.log"));
        // `src`, `other`, and `main.rs` are each stored only once.
        assert!(index.component_count() < paths.iter().map(|path| path.split('/').count()).sum());
    }

    #[test]
    fn root_named_like_an_excluded_dir_still_lists_its_children() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("build");
        fs::create_dir_all(root.join("out")).unwrap();
        fs::write(root.join("a.txt"), "x").unwrap();
        let ws = Workspace::open(&root).unwrap();
        assert_eq!(names(&ws.list_dir("").unwrap()), vec!["out", "a.txt"]);
        assert_eq!(ws.name(), "build");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_folders_are_shown_as_folders() {
        let (dir, ws) = tree();
        std::os::unix::fs::symlink(dir.path().join("src"), dir.path().join("link")).unwrap();
        let entries = ws.list_dir("").unwrap();
        let link = entries.iter().find(|e| e.name == "link").unwrap();
        assert!(link.is_dir);
        assert_eq!(ws.list_dir("link").unwrap().len(), 2);
    }

    #[test]
    fn large_directory_lists_quickly_and_completely() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5_000 {
            fs::write(dir.path().join(format!("f{i:05}.txt")), "").unwrap();
        }
        let ws = Workspace::open(dir.path()).unwrap();
        let start = std::time::Instant::now();
        let entries = ws.list_dir("").unwrap();
        assert_eq!(entries.len(), 5_000);
        assert_eq!(entries[0].name, "f00000.txt");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
    }
}
