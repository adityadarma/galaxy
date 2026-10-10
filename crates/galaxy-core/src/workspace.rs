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

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use ignore::{DirEntry as WalkEntry, WalkBuilder};
use regex::RegexBuilder;

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

        // Each unique name is stored twice: in `components` and as its own
        // key in `component_ids`.
        let component_bytes = 2 * self
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

/// How the Go to File index changes after some paths changed on disk.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IndexDelta {
    /// Drop these paths and everything under them (`p` and `p/...`).
    pub removed: Vec<String>,
    /// Then add these files. Sorted, unique.
    pub added: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    pub rel_path: String,
    pub line: usize,
    pub column: usize,
    pub text: String,
    pub context_before: String,
    pub context_after: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchOptions {
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub include: String,
    pub exclude: String,
}

fn split_globs(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|glob| !glob.is_empty())
        .map(str::to_owned)
        .collect()
}

fn glob_matches(glob: &str, path: &str) -> bool {
    let mut pattern = String::from("^");
    let mut chars = glob.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    chars.next();
                    pattern.push_str("(?:.*/)?");
                } else {
                    pattern.push_str(".*");
                }
            }
            '*' => pattern.push_str("[^/]*"),
            '?' => pattern.push_str("[^/]"),
            _ => pattern.push_str(&regex::escape(&ch.to_string())),
        }
    }
    pattern.push('$');
    RegexBuilder::new(&pattern)
        .build()
        .is_ok_and(|regex| regex.is_match(path))
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

    /// Creates an empty file `name` inside folder `parent` (both relative)
    /// and returns its relative path. `name` may contain `/` to create
    /// missing folders on the way, as in VSCode. Never overwrites.
    pub fn create_file(&self, parent: &str, name: &str) -> io::Result<String> {
        let (rel, path) = self.new_entry_path(parent, name)?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(rel)
    }

    /// Creates folder `name` (and missing parents) inside `parent`.
    /// Fails if it already exists.
    pub fn create_dir(&self, parent: &str, name: &str) -> io::Result<String> {
        let (rel, path) = self.new_entry_path(parent, name)?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::create_dir(&path)?;
        Ok(rel)
    }

    /// Renames `rel` to `new_name` in the same folder (`/` in the name
    /// moves it into subfolders, created as needed) and returns the new
    /// relative path. Never overwrites another file; a change of letter
    /// case only is allowed on case-insensitive volumes.
    pub fn rename(&self, rel: &str, new_name: &str) -> io::Result<String> {
        let from = self.resolve(rel)?;
        if rel.split('/').all(|p| p.is_empty() || p == ".") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the workspace folder cannot be renamed here",
            ));
        }
        fs::symlink_metadata(&from)?;
        let parent = rel.rsplit_once('/').map_or("", |(p, _)| p);
        let (new_rel, to) = self.new_entry_path(parent, new_name)?;
        if new_rel == rel {
            return Ok(new_rel);
        }
        if new_rel.starts_with(&format!("{rel}/")) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a folder cannot be moved into itself",
            ));
        }
        if let Ok(existing) = fs::symlink_metadata(&to) {
            let same_file = {
                use std::os::unix::fs::MetadataExt;
                let src = fs::symlink_metadata(&from)?;
                src.dev() == existing.dev() && src.ino() == existing.ino()
            };
            if !same_file {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{new_rel} already exists"),
                ));
            }
        }
        if let Some(dir) = to.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::rename(&from, &to)?;
        Ok(new_rel)
    }

    /// Validates a new name and returns `(relative path, absolute path)`.
    fn new_entry_path(&self, parent: &str, name: &str) -> io::Result<(String, PathBuf)> {
        let invalid = |why: &str| io::Error::new(io::ErrorKind::InvalidInput, why.to_string());
        let name = name.trim();
        if name.is_empty() {
            return Err(invalid("a name is required"));
        }
        if name.contains('\0') {
            return Err(invalid("names cannot contain NUL"));
        }
        if name.starts_with('/') {
            return Err(invalid("names cannot start with /"));
        }
        let mut parts: Vec<&str> = parent.split('/').filter(|p| !p.is_empty()).collect();
        for part in name.split('/').filter(|p| !p.is_empty()) {
            if part == "." || part == ".." {
                return Err(invalid("names cannot contain . or .. folders"));
            }
            parts.push(part);
        }
        let rel = parts.join("/");
        let path = self.resolve(&rel)?;
        if !fs::metadata(self.resolve(parent)?)?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "not a directory",
            ));
        }
        Ok((rel, path))
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

    /// Searches readable UTF-8 files while walking with the same ignore rules
    /// as the explorer. Results are bounded so a broad query cannot flood the
    /// UI or retain an unbounded snapshot.
    pub fn search(
        &self,
        query: &str,
        options: &SearchOptions,
        limit: usize,
    ) -> io::Result<Vec<SearchMatch>> {
        if query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let pattern = if options.regex {
            query.to_string()
        } else {
            regex::escape(query)
        };
        let pattern = if options.whole_word {
            format!(r"\b(?:{pattern})\b")
        } else {
            pattern
        };
        let regex = RegexBuilder::new(&pattern)
            .case_insensitive(!options.case_sensitive)
            .build()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        let includes = split_globs(&options.include);
        let excludes = split_globs(&options.exclude);
        let walker = WalkBuilder::new(&self.root)
            .hidden(false)
            .ignore(true)
            .git_ignore(true)
            .git_global(false)
            .git_exclude(false)
            .require_git(false)
            .follow_links(false)
            .filter_entry(|e| e.depth() == 0 || !is_default_excluded(e))
            .build();
        let mut results = Vec::new();
        for entry in walker
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        {
            if results.len() >= limit {
                break;
            }
            let path = entry.path();
            let Ok(bytes) = fs::read(path) else { continue };
            let Ok(text) = String::from_utf8(bytes) else {
                continue;
            };
            let Some(relative) = path.strip_prefix(&self.root).ok() else {
                continue;
            };
            let rel_path = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            if !includes.is_empty() && !includes.iter().any(|glob| glob_matches(glob, &rel_path)) {
                continue;
            }
            if excludes.iter().any(|glob| glob_matches(glob, &rel_path)) {
                continue;
            }
            let lines: Vec<&str> = text.lines().collect();
            for (line, content) in lines.iter().enumerate() {
                for found in regex.find_iter(content) {
                    if results.len() >= limit {
                        break;
                    }
                    let byte_column = found.start();
                    let column = content[..byte_column].encode_utf16().count();
                    results.push(SearchMatch {
                        rel_path: rel_path.clone(),
                        line,
                        column,
                        text: (*content).to_string(),
                        context_before: line
                            .checked_sub(1)
                            .and_then(|index| lines.get(index))
                            .map_or_else(String::new, |line| (*line).to_string()),
                        context_after: lines
                            .get(line + 1)
                            .map_or_else(String::new, |line| (*line).to_string()),
                    });
                }
                if results.len() >= limit {
                    break;
                }
            }
        }
        Ok(results)
    }

    /// Works out how the file index changes after `changed` (relative
    /// paths reported by the watcher) changed on disk. Every changed path
    /// is removed with everything under it, then whatever exists now and
    /// is visible (not ignored, not under an ignored folder) is added back:
    /// a file as itself, a folder with all its files.
    pub fn index_delta(&self, changed: &[String]) -> IndexDelta {
        // Directory listings, cached: one `list_dir` per parent.
        let mut listings: HashMap<String, Option<HashSet<String>>> = HashMap::new();
        let mut visible = |rel: &str| -> bool {
            let mut parent = String::new();
            for part in rel.split('/') {
                let names = listings.entry(parent.clone()).or_insert_with(|| {
                    self.list_dir(&parent)
                        .ok()
                        .map(|list| list.into_iter().map(|e| e.name).collect())
                });
                if !names.as_ref().is_some_and(|n| n.contains(part)) {
                    return false;
                }
                if !parent.is_empty() {
                    parent.push('/');
                }
                parent.push_str(part);
            }
            true
        };

        let mut removed: Vec<String> = Vec::new();
        let mut added = BTreeSet::new();
        // A changed folder covers its own changed children.
        let changed: BTreeSet<&str> = changed.iter().map(String::as_str).collect();
        for rel in changed.iter().copied() {
            let covered = removed.last().is_some_and(|r| {
                rel.strip_prefix(r.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
            });
            if covered || rel.is_empty() {
                continue;
            }
            removed.push(rel.to_owned());
            let Ok(path) = self.resolve(rel) else {
                continue;
            };
            // `symlink_metadata`: a link to a folder is not walked, as in
            // `build_file_index`.
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue; // gone
            };
            if !visible(rel) {
                continue;
            }
            if meta.is_dir() {
                added.extend(self.files_under(&path));
            } else if !(meta.file_type().is_symlink() && path.is_dir()) {
                added.insert(rel.to_owned());
            }
        }
        IndexDelta {
            removed,
            added: added.into_iter().collect(),
        }
    }

    /// Relative paths of the files under `dir`, with the same rules as
    /// `build_file_index`.
    fn files_under(&self, dir: &Path) -> Vec<String> {
        let walker = WalkBuilder::new(dir)
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
        walker
            .filter_map(Result::ok)
            .filter(|e| e.depth() > 0 && !e.file_type().is_some_and(|k| k.is_dir()))
            .filter(|e| !(e.path_is_symlink() && e.path().is_dir()))
            .filter_map(|e| {
                let rel = e.path().strip_prefix(&self.root).ok()?;
                Some(
                    rel.components()
                        .map(|c| c.as_os_str().to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/"),
                )
            })
            .collect()
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
            // Links are not followed, so a link to a folder arrives here as
            // a non-directory entry. It is not a file: leave it out.
            if entry.path_is_symlink() && entry.path().is_dir() {
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
    fn search_returns_line_columns_and_skips_ignored_or_binary_files() {
        let (dir, ws) = tree();
        fs::write(
            dir.path().join("src/main.rs"),
            "first needle\n😀 second needle",
        )
        .unwrap();
        fs::write(dir.path().join("node_modules/pkg/ignored.js"), "needle").unwrap();
        fs::write(dir.path().join("binary.bin"), [0, 159, 146, 150]).unwrap();

        let matches = ws.search("needle", &SearchOptions::default(), 20).unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].rel_path, "src/main.rs");
        assert_eq!((matches[0].line, matches[0].column), (0, 6));
        assert_eq!(matches[0].context_before, "");
        assert_eq!(matches[0].context_after, "😀 second needle");
        assert_eq!((matches[1].line, matches[1].column), (1, 10));
        assert_eq!(matches[1].context_before, "first needle");
        assert_eq!(matches[1].context_after, "");
    }

    #[test]
    fn search_respects_result_limit_and_empty_query() {
        let (_dir, ws) = tree();
        assert!(ws.search("x", &SearchOptions::default(), 1).unwrap().len() <= 1);
        assert!(
            ws.search("", &SearchOptions::default(), 20)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn search_supports_regex_case_word_and_globs() {
        let (dir, ws) = tree();
        fs::write(dir.path().join("src/main.rs"), "Needle needle\nneedle2").unwrap();
        let options = SearchOptions {
            regex: true,
            case_sensitive: true,
            whole_word: true,
            include: "src/**/*.rs".into(),
            ..SearchOptions::default()
        };
        let matches = ws.search("[Nn]eedle", &options, 20).unwrap();
        assert_eq!(matches.len(), 2);
        assert!(
            ws.search(
                "needle",
                &SearchOptions {
                    exclude: "src/**".into(),
                    ..SearchOptions::default()
                },
                20
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn creates_files_and_folders_inside_the_workspace() {
        let (dir, ws) = tree();
        assert_eq!(ws.create_file("src", "lib.rs").unwrap(), "src/lib.rs");
        assert!(dir.path().join("src/lib.rs").is_file());
        assert_eq!(
            ws.create_file("", " notes.md ").unwrap(),
            "notes.md",
            "trimmed"
        );
        assert_eq!(ws.create_dir("src", "net").unwrap(), "src/net");
        assert!(dir.path().join("src/net").is_dir());
        // Slashes create the folders on the way, as in VSCode.
        assert_eq!(ws.create_file("", "a/b/c.txt").unwrap(), "a/b/c.txt");
        assert!(dir.path().join("a/b/c.txt").is_file());
    }

    #[test]
    fn rename_moves_within_the_folder_and_never_overwrites() {
        let (dir, ws) = tree();
        let r = dir.path();
        fs::write(r.join("src/main.rs"), "main").unwrap();
        fs::write(r.join("src/other.rs"), "other").unwrap();
        assert_eq!(ws.rename("src/main.rs", "app.rs").unwrap(), "src/app.rs");
        assert_eq!(fs::read_to_string(r.join("src/app.rs")).unwrap(), "main");
        assert!(!r.join("src/main.rs").exists());

        let err = ws.rename("src/app.rs", "other.rs").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(r.join("src/other.rs")).unwrap(), "other");

        // Slashes move it into (new) subfolders of the same folder.
        assert_eq!(
            ws.rename("src/app.rs", "bin/app.rs").unwrap(),
            "src/bin/app.rs"
        );
        // Folders move with their contents.
        assert_eq!(ws.rename("src/bin", "cli").unwrap(), "src/cli");
        assert!(r.join("src/cli/app.rs").is_file());
        assert_eq!(
            ws.rename("src", "src/inner").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        for bad in ["", "../x", "/x", "a/../b"] {
            assert!(ws.rename("src/other.rs", bad).is_err(), "{bad:?}");
        }
        assert!(ws.rename("", "x").is_err());
        assert_eq!(
            ws.rename("missing.txt", "x").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn rename_can_change_only_the_letter_case() {
        let (dir, ws) = tree();
        fs::write(dir.path().join("readme.txt"), "x").unwrap();
        assert_eq!(ws.rename("readme.txt", "README.txt").unwrap(), "README.txt");
        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"README.txt".to_string()), "{names:?}");
    }

    #[test]
    fn create_never_overwrites_and_rejects_bad_names() {
        let (dir, ws) = tree();
        fs::write(dir.path().join("src/main.rs"), "keep").unwrap();
        let err = ws.create_file("src", "main.rs").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
            "keep"
        );
        assert_eq!(
            ws.create_dir("", "src").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        for bad in ["", "  ", "../x", "a/../b", "/etc/x", "./x", "a\0b"] {
            assert!(ws.create_file("", bad).is_err(), "{bad:?} must be refused");
        }
        assert!(ws.create_file("../", "x").is_err(), "parent outside");
        assert!(
            ws.create_file("README.md", "x").is_err(),
            "parent is a file"
        );
        assert!(!dir.path().parent().unwrap().join("x").exists());
    }

    #[test]
    fn index_delta_adds_visible_files_and_removes_gone_ones() {
        let (dir, ws) = tree();
        let r = dir.path();
        fs::write(r.join(".gitignore"), "*.log\nout/\n").unwrap();
        fs::write(r.join("src/new.rs"), "x").unwrap();
        fs::write(r.join("debug.log"), "x").unwrap();
        fs::create_dir_all(r.join("out")).unwrap();
        fs::write(r.join("out/a.txt"), "x").unwrap();
        fs::remove_file(r.join("README.md")).unwrap();
        let changed: Vec<String> = ["src/new.rs", "debug.log", "out/a.txt", "README.md"]
            .map(String::from)
            .to_vec();
        let delta = ws.index_delta(&changed);
        assert_eq!(
            delta.added,
            ["src/new.rs"],
            "ignored files and files in ignored folders stay out"
        );
        assert_eq!(
            delta.removed,
            ["README.md", "debug.log", "out/a.txt", "src/new.rs"],
            "sorted"
        );
    }

    #[test]
    fn index_delta_walks_new_folders_and_drops_renamed_ones() {
        let (dir, ws) = tree();
        let r = dir.path();
        // `git mv src lib`: the old folder is gone, the new one appears.
        fs::rename(r.join("src"), r.join("lib")).unwrap();
        let delta = ws.index_delta(&["src".into(), "lib".into(), "lib/main.rs".into()]);
        assert_eq!(
            delta.removed,
            ["lib", "src"],
            "lib/main.rs is covered by lib"
        );
        assert_eq!(delta.added, ["lib/deep/mod.rs", "lib/main.rs"]);
        assert!(
            ws.index_delta(&["node_modules/pkg/index.js".into()])
                .added
                .is_empty()
        );
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
        // Every distinct name (`src`, `other`, `main.rs`, ...) is stored once.
        let unique: std::collections::HashSet<&str> =
            paths.iter().flat_map(|path| path.split('/')).collect();
        assert_eq!(index.component_count(), unique.len());
    }

    #[cfg(unix)]
    #[test]
    fn file_index_skips_links_to_folders_but_keeps_links_to_files() {
        let (dir, ws) = tree();
        std::os::unix::fs::symlink(dir.path().join("src"), dir.path().join("src-link")).unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("src/main.rs"),
            dir.path().join("main-link.rs"),
        )
        .unwrap();
        let index = ws.build_file_index();
        let paths: Vec<String> = (0..index.len()).map(|i| index.path(i).unwrap()).collect();
        assert!(!paths.iter().any(|p| p == "src-link"), "{paths:?}");
        assert!(
            !paths.iter().any(|p| p.starts_with("src-link/")),
            "links are not followed"
        );
        assert!(paths.iter().any(|p| p == "main-link.rs"));
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
