//! UniFFI bridge exposed to Swift.
//!
//! Design rule: the UI calls in with coarse-grained commands and receives
//! batched events through `CoreListener`. No per-character FFI chatter.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use rcode_core::clipboard;
use rcode_core::commands::{self, CommandRegistry};
use rcode_core::disk::{DiskCheck, DiskStamp};
use rcode_core::document::{Document, Encoding, LineEnding};
use rcode_core::edit::{self, IndentConfig};
use rcode_core::file_finder::FileFinder as CoreFileFinder;
use rcode_core::find::{FindError, FindOptions, Finder};
use rcode_core::history::{EditKind, Editor};
use rcode_core::movement::{self, Motion};
use rcode_core::multicursor;
use rcode_core::navigation::{self, Location, NavHistory};
use rcode_core::position::Utf16Position;
use rcode_core::selection::{Selection, SelectionSet};
use rcode_core::viewport;
use rcode_core::watcher::WorkspaceWatcher;
use rcode_core::workspace::Workspace as CoreWorkspace;

uniffi::setup_scaffolding!();

/// Errors surfaced to Swift as a thrown `CoreError`.
#[derive(Debug, uniffi::Error)]
pub enum CoreError {
    Io { message: String },
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoreError::Io { message } => write!(f, "I/O error: {message}"),
        }
    }
}

impl std::error::Error for CoreError {}

impl From<std::io::Error> for CoreError {
    fn from(e: std::io::Error) -> Self {
        CoreError::Io {
            message: e.to_string(),
        }
    }
}

/// Events pushed from the core to the UI.
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum CoreEvent {
    Pong { message: String },
}

/// Implemented in Swift. Called from core threads, so the Swift side must
/// hop to the main actor before touching UI.
#[uniffi::export(with_foreign)]
pub trait CoreListener: Send + Sync {
    fn on_event(&self, event: CoreEvent);
}

/// Returns the core version.
#[uniffi::export]
pub fn core_version() -> String {
    rcode_core::version().to_string()
}

/// Initializes file logging in `dir`. Returns `false` if already initialized.
#[uniffi::export]
pub fn init_logging(dir: String, level: String) -> Result<bool, CoreError> {
    Ok(rcode_core::logging::init(&PathBuf::from(dir), &level)?)
}

/// A registered command (M2-09).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CommandInfo {
    pub id: String,
    pub category: String,
    pub title: String,
}

/// One command palette row.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CommandMatch {
    pub id: String,
    /// `Category: Title`.
    pub label: String,
    /// Matched characters, UTF-16 ranges within `label`.
    pub highlights: Vec<Utf16Range>,
    pub is_recent: bool,
}

/// The command registry with the built-in commands, and the palette's
/// search over it.
#[derive(uniffi::Object)]
pub struct CommandPalette {
    inner: Mutex<CommandRegistry>,
}

#[uniffi::export]
impl CommandPalette {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(commands::builtin()),
        })
    }

    pub fn commands(&self) -> Vec<CommandInfo> {
        self.lock()
            .commands()
            .iter()
            .map(|c| CommandInfo {
                id: c.id.clone(),
                category: c.category.clone(),
                title: c.title.clone(),
            })
            .collect()
    }

    /// Matches for `query`, limited to `available` ids if given.
    pub fn search(&self, query: String, available: Option<Vec<String>>) -> Vec<CommandMatch> {
        let available: Option<std::collections::HashSet<String>> =
            available.map(|ids| ids.into_iter().collect());
        self.lock()
            .search(&query, available.as_ref())
            .into_iter()
            .map(|m| CommandMatch {
                id: m.id,
                label: m.label,
                highlights: m.highlights.into_iter().map(Utf16Range::from).collect(),
                is_recent: m.is_recent,
            })
            .collect()
    }

    pub fn note_used(&self, id: String) {
        self.lock().note_used(&id);
    }

    pub fn recent(&self) -> Vec<String> {
        self.lock().recent().to_vec()
    }
}

impl CommandPalette {
    fn lock(&self) -> std::sync::MutexGuard<'_, CommandRegistry> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Where Go to Line goes: zero-based line, optional zero-based char column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct LineTarget {
    pub line: u64,
    pub col: Option<u64>,
}

/// Parses Go to Line input (`42`, `42:7`, `-1`). `None` if it is not one.
#[uniffi::export]
pub fn parse_line_target(input: String, line_count: u64) -> Option<LineTarget> {
    navigation::parse_line_target(&input, clamp_usize(line_count)).map(|t| LineTarget {
        line: t.line as u64,
        col: t.col.map(|c| c as u64),
    })
}

/// A cursor position in a document for back/forward navigation. `key`
/// identifies the document (resolved path, or an id for Untitled);
/// `col` counts chars.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NavLocation {
    pub key: String,
    pub line: u64,
    pub col: u64,
}

impl From<NavLocation> for Location {
    fn from(l: NavLocation) -> Self {
        Location::new(l.key, clamp_usize(l.line), clamp_usize(l.col))
    }
}

impl From<Location> for NavLocation {
    fn from(l: Location) -> Self {
        NavLocation {
            key: l.key,
            line: l.line as u64,
            col: l.col as u64,
        }
    }
}

/// Back/forward history for one window (M2-08).
#[derive(uniffi::Object, Default)]
pub struct NavigationHistory {
    inner: Mutex<NavHistory>,
}

#[uniffi::export]
impl NavigationHistory {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Records `from` as a place to come back to.
    pub fn record(&self, from: NavLocation) {
        self.lock().record(from.into());
    }

    /// Records `from` only if `to` is in another document or far away.
    pub fn record_jump(&self, from: NavLocation, to: NavLocation) -> bool {
        self.lock().record_jump(from.into(), &to.into())
    }

    pub fn back(&self, current: Option<NavLocation>) -> Option<NavLocation> {
        self.lock().back(current.map(Into::into)).map(Into::into)
    }

    pub fn forward(&self, current: Option<NavLocation>) -> Option<NavLocation> {
        self.lock().forward(current.map(Into::into)).map(Into::into)
    }

    pub fn forget(&self, key: String) {
        self.lock().forget(&key);
    }

    pub fn can_go_back(&self) -> bool {
        self.lock().can_go_back()
    }

    pub fn can_go_forward(&self) -> bool {
        self.lock().can_go_forward()
    }
}

impl NavigationHistory {
    fn lock(&self) -> std::sync::MutexGuard<'_, NavHistory> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Root object owned by the app for its whole lifetime.
#[derive(uniffi::Object)]
pub struct Core {
    listener: RwLock<Option<Arc<dyn CoreListener>>>,
}

impl Default for Core {
    fn default() -> Self {
        Self {
            listener: RwLock::new(None),
        }
    }
}

#[uniffi::export]
impl Core {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn set_listener(&self, listener: Arc<dyn CoreListener>) {
        *self.listener.write().unwrap_or_else(|e| e.into_inner()) = Some(listener);
    }

    /// Round-trip check for the callback path. Emits `Pong` from a
    /// background thread to mirror how real events will arrive.
    pub fn ping(&self, message: String) {
        let listener = self
            .listener
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(listener) = listener {
            std::thread::Builder::new()
                .name("rcode-ping".into())
                .spawn(move || listener.on_event(CoreEvent::Pong { message }))
                .ok();
        }
    }

    /// An empty, unsaved document ("Untitled"). It has no path until the
    /// first `save_as`.
    pub fn new_document(&self) -> Arc<DocumentHandle> {
        Arc::new(DocumentHandle::new(Document::from_text(""), PathBuf::new()))
    }

    /// Opens a folder as a workspace. Nothing is scanned yet; the explorer
    /// lists one directory at a time through `Workspace::list_dir`.
    pub fn open_workspace(&self, path: String) -> Result<Arc<Workspace>, CoreError> {
        let ws = CoreWorkspace::open(PathBuf::from(&path))?;
        tracing::info!(path = %ws.root().display(), "opened workspace");
        Ok(Arc::new(Workspace { inner: ws }))
    }

    pub fn open_document(&self, path: String) -> Result<Arc<DocumentHandle>, CoreError> {
        let path = PathBuf::from(path);
        let handle = DocumentHandle::load(path)?;
        {
            let st = handle.lock();
            tracing::info!(
                path = %st.path.display(),
                lines = st.editor.doc.line_count(),
                encoding = ?st.editor.doc.encoding(),
                line_ending = ?st.editor.doc.line_ending(),
                "opened document"
            );
        }
        Ok(Arc::new(handle))
    }
}

/// One row of the file explorer.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FileEntry {
    pub name: String,
    /// Path relative to the workspace root, `/` separated.
    pub rel_path: String,
    pub is_dir: bool,
}

/// A folder opened in the explorer.
#[derive(uniffi::Object)]
pub struct Workspace {
    inner: CoreWorkspace,
}

#[uniffi::export]
impl Workspace {
    pub fn root(&self) -> String {
        self.inner.root().to_string_lossy().into_owned()
    }

    /// Folder name for the window title.
    pub fn name(&self) -> String {
        self.inner.name()
    }

    /// Lists one directory (`""` is the root): folders first, then files,
    /// honoring `.gitignore` and the built-in excludes.
    pub fn list_dir(&self, rel_path: String) -> Result<Vec<FileEntry>, CoreError> {
        Ok(self
            .inner
            .list_dir(&rel_path)?
            .into_iter()
            .map(|e| FileEntry {
                name: e.name,
                rel_path: e.rel_path,
                is_dir: e.is_dir,
            })
            .collect())
    }

    /// Creates an empty file `name` in folder `parent_rel_path` and returns
    /// its relative path. `name` may contain `/`. Never overwrites.
    pub fn create_file(&self, parent_rel_path: String, name: String) -> Result<String, CoreError> {
        let rel = self.inner.create_file(&parent_rel_path, &name)?;
        tracing::info!(path = %rel, "created file");
        Ok(rel)
    }

    /// Creates folder `name` in `parent_rel_path`; returns its relative path.
    pub fn create_folder(
        &self,
        parent_rel_path: String,
        name: String,
    ) -> Result<String, CoreError> {
        let rel = self.inner.create_dir(&parent_rel_path, &name)?;
        tracing::info!(path = %rel, "created folder");
        Ok(rel)
    }

    /// Starts watching the workspace for changes on disk (M2-02). Batches
    /// arrive on a background thread until the returned handle is stopped
    /// or dropped.
    pub fn watch(
        &self,
        listener: Arc<dyn WorkspaceListener>,
    ) -> Result<Arc<WorkspaceWatch>, CoreError> {
        let watcher = WorkspaceWatcher::start(self.inner.root(), move |batch| {
            listener.on_changes(WatchBatch {
                paths: batch.paths,
                rescan: batch.rescan,
            });
        })?;
        Ok(Arc::new(WorkspaceWatch {
            inner: Mutex::new(Some(watcher)),
        }))
    }

    /// Walks the whole workspace and returns a Go to File searcher. Takes
    /// about 0.6 s on 100k files: call it off the main thread.
    pub fn build_file_finder(&self) -> Arc<FileFinder> {
        let start = Instant::now();
        let finder = CoreFileFinder::from_index(&self.inner.build_file_index());
        tracing::info!(
            files = finder.len(),
            ms = start.elapsed().as_millis() as u64,
            "built file finder"
        );
        Arc::new(FileFinder {
            inner: Mutex::new(finder),
        })
    }

    /// Opens a file inside the workspace. Paths that would leave the
    /// workspace are refused.
    pub fn open_file(&self, rel_path: String) -> Result<Arc<DocumentHandle>, CoreError> {
        let path = self.inner.resolve(&rel_path)?;
        let handle = DocumentHandle::load(path)?;
        {
            let st = handle.lock();
            tracing::info!(path = %st.path.display(), lines = st.editor.doc.line_count(), "opened workspace file");
        }
        Ok(Arc::new(handle))
    }
}

/// A document's file compared with what it last loaded or saved (M2-03).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DiskStatus {
    Unchanged,
    /// Another program changed the content.
    Changed,
    /// The file is gone (deleted, or renamed away).
    Deleted,
}

/// Paths that changed on disk since the previous batch (M2-02).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct WatchBatch {
    /// Relative, `/`-separated, sorted. Empty when `rescan` is set.
    pub paths: Vec<String>,
    /// Events were lost: treat everything as changed.
    pub rescan: bool,
}

/// Implemented in Swift. Called on the watcher thread.
#[uniffi::export(with_foreign)]
pub trait WorkspaceListener: Send + Sync {
    fn on_changes(&self, batch: WatchBatch);
}

/// A running watcher; stops when stopped or dropped.
#[derive(uniffi::Object)]
pub struct WorkspaceWatch {
    inner: Mutex<Option<WorkspaceWatcher>>,
}

#[uniffi::export]
impl WorkspaceWatch {
    pub fn stop(&self) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

/// One Go to File result.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FileMatch {
    /// Relative, `/`-separated path.
    pub rel_path: String,
    /// UTF-16 offset of the file name within `rel_path`.
    pub name_start: u64,
    /// Matched characters, UTF-16 ranges within `rel_path`.
    pub highlights: Vec<Utf16Range>,
    pub is_recent: bool,
}

/// Go to File searcher for one workspace (M2-07). A snapshot: files
/// created later appear after it is rebuilt.
#[derive(uniffi::Object)]
pub struct FileFinder {
    inner: Mutex<CoreFileFinder>,
}

#[uniffi::export]
impl FileFinder {
    pub fn file_count(&self) -> u64 {
        self.lock().len() as u64
    }

    /// `current`: the open file's relative path, if any. `recent`: recently
    /// opened relative paths, most recent first.
    pub fn set_context(&self, current: Option<String>, recent: Vec<String>) {
        self.lock().set_context(current.as_deref(), &recent);
    }

    /// Updates the index for paths the watcher reported. Checks the disk,
    /// so call it off the main thread. Returns `true` if the file list
    /// changed.
    pub fn apply_changes(&self, workspace: Arc<Workspace>, changed: Vec<String>) -> bool {
        let start = Instant::now();
        let delta = workspace.inner.index_delta(&changed);
        let updated = self.lock().apply_delta(&delta);
        tracing::debug!(
            changed = changed.len(),
            added = delta.added.len(),
            removed = delta.removed.len(),
            ms = start.elapsed().as_millis() as u64,
            "updated file finder"
        );
        updated
    }

    /// Best `limit` files for `query`; recent files for an empty query.
    pub fn search(&self, query: String, limit: u32) -> Vec<FileMatch> {
        self.lock()
            .search(&query, limit as usize)
            .into_iter()
            .map(|m| FileMatch {
                rel_path: m.path,
                name_start: m.name_start_utf16 as u64,
                highlights: m.highlights.into_iter().map(Utf16Range::from).collect(),
                is_recent: m.is_recent,
            })
            .collect()
    }
}

impl FileFinder {
    fn lock(&self) -> std::sync::MutexGuard<'_, CoreFileFinder> {
        // A panic mid-search leaves nothing half-written worth refusing.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// On-disk text encoding, mirrored from `rcode_core::document::Encoding`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

impl From<Encoding> for TextEncoding {
    fn from(e: Encoding) -> Self {
        match e {
            Encoding::Utf8 => TextEncoding::Utf8,
            Encoding::Utf16Le => TextEncoding::Utf16Le,
            Encoding::Utf16Be => TextEncoding::Utf16Be,
        }
    }
}

impl From<TextEncoding> for Encoding {
    fn from(e: TextEncoding) -> Self {
        match e {
            TextEncoding::Utf8 => Encoding::Utf8,
            TextEncoding::Utf16Le => Encoding::Utf16Le,
            TextEncoding::Utf16Be => Encoding::Utf16Be,
        }
    }
}

/// Line ending style, mirrored from `rcode_core::document::LineEnding`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum LineEndingKind {
    Lf,
    CrLf,
    Cr,
}

impl From<LineEnding> for LineEndingKind {
    fn from(e: LineEnding) -> Self {
        match e {
            LineEnding::Lf => LineEndingKind::Lf,
            LineEnding::CrLf => LineEndingKind::CrLf,
            LineEnding::Cr => LineEndingKind::Cr,
        }
    }
}

impl From<LineEndingKind> for LineEnding {
    fn from(e: LineEndingKind) -> Self {
        match e {
            LineEndingKind::Lf => LineEnding::Lf,
            LineEndingKind::CrLf => LineEnding::CrLf,
            LineEndingKind::Cr => LineEnding::Cr,
        }
    }
}

/// A position from the UI: line plus UTF-16 column (what `NSString` and
/// Core Text use). Out-of-range values are clamped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct TextPos {
    pub line: u64,
    pub col_utf16: u64,
}

/// A selection from the UI. `anchor == head` is a caret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct TextSelection {
    pub anchor: TextPos,
    pub head: TextPos,
}

/// Cursor motions for arrow keys and friends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CursorMotion {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    DocStart,
    DocEnd,
    PageUp { lines: u32 },
    PageDown { lines: u32 },
}

impl From<CursorMotion> for Motion {
    fn from(m: CursorMotion) -> Self {
        match m {
            CursorMotion::Left => Motion::Left,
            CursorMotion::Right => Motion::Right,
            CursorMotion::Up => Motion::Up,
            CursorMotion::Down => Motion::Down,
            CursorMotion::WordLeft => Motion::WordLeft,
            CursorMotion::WordRight => Motion::WordRight,
            CursorMotion::LineStart => Motion::LineStart,
            CursorMotion::LineEnd => Motion::LineEnd,
            CursorMotion::DocStart => Motion::DocStart,
            CursorMotion::DocEnd => Motion::DocEnd,
            CursorMotion::PageUp { lines } => Motion::PageUp(lines as usize),
            CursorMotion::PageDown { lines } => Motion::PageDown(lines as usize),
        }
    }
}

/// Text taken by copy or cut. `linewise` is set when there was no
/// selection and whole lines were taken; the UI keeps that flag on the
/// pasteboard so a later paste goes above the caret's line.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ClipboardText {
    pub text: String,
    pub linewise: bool,
}

impl From<clipboard::Copied> for ClipboardText {
    fn from(c: clipboard::Copied) -> Self {
        Self {
            text: c.text,
            linewise: c.linewise,
        }
    }
}

/// Edit commands. Each applies to every selection at once and is one call
/// per keystroke, regardless of cursor count.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum EditCommand {
    InsertText { text: String },
    Paste { text: String },
    Backspace,
    DeleteForward,
    Newline,
    Indent,
    Outdent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct LineSpan {
    pub start: u64,
    pub end: u64,
    pub includes_eol: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ViewportLine {
    pub line: u64,
    pub text: String,
    pub selections: Vec<LineSpan>,
    pub carets: Vec<u64>,
    /// IME marked text on this line, drawn underlined.
    pub marked: Option<LineSpan>,
    /// Find matches on this line (UTF-16 columns), empty when not searching.
    pub matches: Vec<LineSpan>,
    /// `text` is only the start of a very long line (see `display_line`).
    pub truncated: bool,
}

/// Indentation used by Tab, Enter and Outdent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct IndentSettings {
    pub use_tabs: bool,
    pub tab_size: u32,
}

/// Options of the find bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct FindConfig {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

/// What the find bar shows next to the text field.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FindStatus {
    /// Why the pattern is unusable (an invalid regex), if it is.
    pub error: Option<String>,
    pub total: u64,
    /// 1-based index of the match that is currently selected.
    pub current: Option<u64>,
    /// More matches exist than are counted; `total` is a lower bound.
    pub truncated: bool,
    /// `false` when the document is too big to count on the calling
    /// thread; `total` and `current` are then meaningless. Get the real
    /// numbers from `DocumentHandle::start_find_count`.
    pub counted: bool,
}

/// Above this size the find bar counts matches on a background thread.
/// A full count costs roughly 5 ms per MB in a release build.
pub const SYNC_FIND_COUNT_BYTES: u64 = 1 << 20;

impl FindStatus {
    fn from_summary(sum: rcode_core::find::Summary) -> Self {
        Self {
            error: None,
            total: sum.total as u64,
            current: sum.current.map(|c| c as u64),
            truncated: sum.truncated,
            counted: true,
        }
    }
}

/// A match count over a snapshot of the document, meant to run off the
/// main thread. The snapshot is a rope clone, which shares its storage, so
/// creating a job is cheap and edits made meanwhile do not affect it.
#[derive(uniffi::Object)]
pub struct FindCountJob {
    finder: Finder,
    doc: Document,
    sel: SelectionSet,
    cancel: AtomicBool,
}

#[uniffi::export]
impl FindCountJob {
    /// Text revision the count describes. Compare with
    /// `DocumentHandle::revision` before showing the result.
    pub fn revision(&self) -> u64 {
        self.doc.revision()
    }

    /// Makes a running `run` return `None` soon.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Counts. Blocks; call it from a background queue. `None` if cancelled.
    pub fn run(&self) -> Option<FindStatus> {
        self.finder
            .summary_cancellable(&self.doc, &self.sel, &self.cancel)
            .map(FindStatus::from_summary)
    }
}

/// A range in document-wide UTF-16 code units, like `NSRange`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct Utf16Range {
    pub location: u64,
    pub length: u64,
}

impl From<(usize, usize)> for Utf16Range {
    fn from((location, length): (usize, usize)) -> Self {
        Self {
            location: location as u64,
            length: length as u64,
        }
    }
}

fn range_pair(r: Utf16Range) -> (usize, usize) {
    (clamp_usize(r.location), clamp_usize(r.length))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct CursorInfo {
    pub line: u64,
    pub col: u64,
    pub col_utf16: u64,
}

/// Everything the editor view needs to draw one frame. Columns are UTF-16.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ViewportSnapshot {
    pub revision: u64,
    pub line_count: u64,
    pub lines: Vec<ViewportLine>,
    pub primary: CursorInfo,
    pub selection_count: u64,
}

impl From<viewport::ViewportSnapshot> for ViewportSnapshot {
    fn from(s: viewport::ViewportSnapshot) -> Self {
        Self {
            revision: s.revision,
            line_count: s.line_count as u64,
            lines: s
                .lines
                .into_iter()
                .map(|l| ViewportLine {
                    line: l.line as u64,
                    text: l.text,
                    selections: l
                        .selections
                        .into_iter()
                        .map(|sp| LineSpan {
                            start: sp.start as u64,
                            end: sp.end as u64,
                            includes_eol: sp.includes_eol,
                        })
                        .collect(),
                    carets: l.carets.into_iter().map(|c| c as u64).collect(),
                    marked: l.marked.map(|sp| LineSpan {
                        start: sp.start as u64,
                        end: sp.end as u64,
                        includes_eol: sp.includes_eol,
                    }),
                    matches: Vec::new(),
                    truncated: l.truncated,
                })
                .collect(),
            primary: CursorInfo {
                line: s.primary.line as u64,
                col: s.primary.col as u64,
                col_utf16: s.primary.col_utf16 as u64,
            },
            selection_count: s.selection_count as u64,
        }
    }
}

struct State {
    editor: Editor,
    path: PathBuf,
    indent: IndentConfig,
    /// `History::state_id` at the last load or save, for `is_modified`.
    saved_state: u64,
    /// What the file was when last loaded or saved (M2-03). `None` for
    /// an Untitled buffer.
    disk: Option<DiskStamp>,
    /// The file is gone from disk; the buffer counts as unsaved.
    deleted_on_disk: bool,
    /// Active search, if the find bar has a usable pattern.
    finder: Option<Finder>,
    /// Why the current pattern is unusable.
    find_error: Option<String>,
}

impl State {
    /// Edits since the last load or save, ignoring the file's state.
    fn has_unsaved_edits(&self) -> bool {
        // Marked IME text is in the buffer but not yet in the history.
        self.editor.has_marked_text() || self.editor.history.state_id() != self.saved_state
    }

    fn find_status(&self) -> FindStatus {
        let ed = &self.editor;
        match &self.finder {
            // Counting scans the whole file; above this size it belongs on
            // a background thread (`start_find_count`).
            Some(_) if ed.doc.len_bytes() as u64 > SYNC_FIND_COUNT_BYTES => FindStatus {
                error: None,
                total: 0,
                current: None,
                truncated: false,
                counted: false,
            },
            Some(f) => FindStatus::from_summary(f.summary(&ed.doc, &ed.sel)),
            None => FindStatus {
                error: self.find_error.clone(),
                total: 0,
                current: None,
                truncated: false,
                counted: true,
            },
        }
    }

    /// Selects `range` and ends the typing group, like any cursor move.
    fn select_match(&mut self, range: std::ops::Range<usize>) {
        let ed = &mut self.editor;
        ed.set_selections(SelectionSet::single(Selection::new(range.start, range.end)));
        ed.history.break_group();
    }
}

/// A document opened by the UI: text, selections, and undo history.
#[derive(uniffi::Object)]
pub struct DocumentHandle {
    state: Mutex<State>,
}

#[uniffi::export]
impl DocumentHandle {
    pub fn line_count(&self) -> u64 {
        self.lock().editor.doc.line_count() as u64
    }

    pub fn len_bytes(&self) -> u64 {
        self.lock().editor.doc.len_bytes() as u64
    }

    pub fn revision(&self) -> u64 {
        self.lock().editor.doc.revision()
    }

    /// Plain lines without selection info.
    pub fn lines(&self, first: u64, count: u64) -> Vec<String> {
        self.lock()
            .editor
            .doc
            .lines(clamp_usize(first), clamp_usize(count))
    }

    /// One snapshot per frame: visible lines, selections, carets, cursor.
    pub fn viewport(&self, first_line: u64, count: u64) -> ViewportSnapshot {
        let st = self.lock();
        let mut snap: ViewportSnapshot = st
            .editor
            .viewport(clamp_usize(first_line), clamp_usize(count))
            .into();
        if let Some(finder) = &st.finder {
            for line in &mut snap.lines {
                // Only the displayed part, so a minified line stays cheap.
                line.matches = finder
                    .columns_in_text(&line.text)
                    .into_iter()
                    .map(|r| LineSpan {
                        start: r.start as u64,
                        end: r.end as u64,
                        includes_eol: false,
                    })
                    .collect();
            }
        }
        snap
    }

    /// Large-file mode (> 20 MB): the UI shows a banner and skips live
    /// find counts.
    pub fn is_large(&self) -> bool {
        self.lock().editor.doc.is_large()
    }

    /// The displayed text of `line`: the whole line, or the first
    /// `MAX_LINE_DISPLAY_CHARS` chars of a very long one. Use it for
    /// hit-testing so clicks agree with what was drawn.
    pub fn display_line(&self, line: u64) -> String {
        let st = self.lock();
        let line = clamp_usize(line).min(st.editor.doc.line_count() - 1);
        viewport::display_text(&st.editor.doc, line)
    }

    pub fn path(&self) -> String {
        self.lock().path.to_string_lossy().into_owned()
    }

    /// `true` when the text differs from what was last loaded or saved.
    /// Undoing (or redoing) back to the saved text clears it again.
    pub fn is_modified(&self) -> bool {
        let st = self.lock();
        st.deleted_on_disk || st.has_unsaved_edits()
    }

    /// Compares the file on disk with what was last loaded or saved
    /// (M2-03). One `stat` when nothing changed. Errors other than "not
    /// found" (permissions) report `Unchanged`.
    pub fn check_disk(&self) -> DiskStatus {
        let mut st = self.lock();
        if st.path.as_os_str().is_empty() {
            return DiskStatus::Unchanged;
        }
        let path = st.path.clone();
        let Some(stamp) = st.disk.as_mut() else {
            // Saved never and loaded never: nothing to compare with.
            return DiskStatus::Unchanged;
        };
        match stamp.check(&path) {
            Ok(DiskCheck::Unchanged) => {
                st.deleted_on_disk = false;
                DiskStatus::Unchanged
            }
            Ok(DiskCheck::Changed(_)) => {
                st.deleted_on_disk = false;
                DiskStatus::Changed
            }
            Ok(DiskCheck::Deleted) => {
                st.deleted_on_disk = true;
                DiskStatus::Deleted
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "could not check file on disk");
                DiskStatus::Unchanged
            }
        }
    }

    /// Replaces the buffer with the file on disk as one undoable step,
    /// changing only the part that differs so carets elsewhere stay put.
    /// The result counts as saved. Returns `true` if the text changed.
    pub fn reload_from_disk(&self) -> Result<bool, CoreError> {
        let mut st = self.lock();
        let (stamp, bytes) = DiskStamp::read(&st.path)?;
        let disk = Document::from_bytes(&bytes)?;
        let changed = st.editor.reload(&disk, Instant::now());
        st.saved_state = st.editor.history.state_id();
        st.disk = Some(stamp);
        st.deleted_on_disk = false;
        tracing::info!(path = %st.path.display(), changed, "reloaded document from disk");
        Ok(changed)
    }

    /// Keeps the buffer after the file changed on disk: the change is not
    /// reported again, and the buffer stays unsaved until it is saved over
    /// the file.
    pub fn keep_buffer_over_disk(&self) {
        let mut st = self.lock();
        if let Ok((stamp, _)) = DiskStamp::read(&st.path) {
            st.disk = Some(stamp);
        }
        // No history state matches the file any more.
        st.saved_state = u64::MAX;
    }

    pub fn encoding(&self) -> TextEncoding {
        self.lock().editor.doc.encoding().into()
    }

    pub fn set_encoding(&self, encoding: TextEncoding) {
        self.lock().editor.doc.set_encoding(encoding.into());
    }

    pub fn has_bom(&self) -> bool {
        self.lock().editor.doc.has_bom()
    }

    pub fn line_ending(&self) -> LineEndingKind {
        self.lock().editor.doc.line_ending().into()
    }

    pub fn set_line_ending(&self, line_ending: LineEndingKind) {
        self.lock().editor.doc.set_line_ending(line_ending.into());
    }

    /// Current indentation settings, for the status bar.
    pub fn indent_settings(&self) -> IndentSettings {
        let cfg = self.lock().indent;
        IndentSettings {
            use_tabs: cfg.use_tabs,
            tab_size: cfg.tab_size as u32,
        }
    }

    pub fn set_indent(&self, use_tabs: bool, tab_size: u32) {
        self.lock().indent = IndentConfig {
            use_tabs,
            tab_size: tab_size as usize,
        };
    }

    /// Replaces all selections (click, arrow keys, Opt+Click). `primary`
    /// indexes into `selections`. An empty list puts a caret at 0.
    pub fn set_selections(&self, selections: Vec<TextSelection>, primary: u64) {
        let mut st = self.lock();
        let ed = &mut st.editor;
        let ranges = selections
            .iter()
            .map(|s| Selection::new(to_char(&ed.doc, s.anchor), to_char(&ed.doc, s.head)))
            .collect();
        let set = SelectionSet::new(ranges, clamp_usize(primary), &ed.doc);
        ed.set_selections(set);
    }

    /// Applies an edit to every selection and records it for undo.
    pub fn edit(&self, command: EditCommand) {
        let mut st = self.lock();
        let cfg = st.indent;
        let ed = &mut st.editor;
        let (doc, sel) = (&ed.doc, &ed.sel);
        let (tx, kind) = match &command {
            EditCommand::InsertText { text } => {
                (edit::insert_text(doc, sel, text), insert_kind(text))
            }
            EditCommand::Paste { text } => (edit::paste(doc, sel, text), EditKind::Other),
            EditCommand::Backspace => (edit::backspace(doc, sel, &cfg), EditKind::Delete),
            EditCommand::DeleteForward => (edit::delete_forward(doc, sel), EditKind::Delete),
            EditCommand::Newline => (edit::newline(doc, sel, &cfg), EditKind::Other),
            EditCommand::Indent => (edit::indent(doc, sel, &cfg), EditKind::Other),
            EditCommand::Outdent => (edit::outdent(doc, sel, &cfg), EditKind::Other),
        };
        ed.apply(tx, kind, Instant::now());
    }

    // --- Navigation and selection (M1-11) ---

    /// Arrow keys, Home/End, Page Up/Down. `extend` is the Shift key.
    pub fn move_cursors(&self, motion: CursorMotion, extend: bool) {
        let mut st = self.lock();
        let ed = &mut st.editor;
        let next = movement::move_selections(&ed.doc, &ed.sel, motion.into(), extend);
        ed.set_selections(next);
        ed.history.break_group();
    }

    pub fn select_all(&self) {
        let mut st = self.lock();
        let ed = &mut st.editor;
        let next = movement::select_all(&ed.doc);
        ed.set_selections(next);
    }

    /// Plain click: one caret at `pos`.
    pub fn set_caret(&self, pos: TextPos) {
        let mut st = self.lock();
        let ed = &mut st.editor;
        let next = movement::set_caret(&ed.doc, to_char(&ed.doc, pos));
        ed.set_selections(next);
        ed.history.break_group();
    }

    /// Option-click: adds a caret.
    pub fn add_caret(&self, pos: TextPos) {
        let mut st = self.lock();
        let ed = &mut st.editor;
        let next = movement::add_caret(&ed.doc, &ed.sel, to_char(&ed.doc, pos));
        ed.set_selections(next);
    }

    /// Shift-click or mouse drag: moves the primary head to `pos`.
    pub fn extend_selection_to(&self, pos: TextPos) {
        let mut st = self.lock();
        let ed = &mut st.editor;
        let next = movement::extend_primary_to(&ed.doc, &ed.sel, to_char(&ed.doc, pos));
        ed.set_selections(next);
    }

    /// Double-click: selects the word at `pos`.
    pub fn select_word_at(&self, pos: TextPos) {
        let mut st = self.lock();
        let ed = &mut st.editor;
        let range = movement::word_at(&ed.doc, to_char(&ed.doc, pos));
        ed.set_selections(SelectionSet::single(range));
        ed.history.break_group();
    }

    /// Triple-click: selects the whole line at `pos`.
    pub fn select_line_at(&self, pos: TextPos) {
        let mut st = self.lock();
        let ed = &mut st.editor;
        let range = movement::line_at(&ed.doc, to_char(&ed.doc, pos));
        ed.set_selections(SelectionSet::single(range));
        ed.history.break_group();
    }

    /// Go to Line and history navigation: one caret at `line` (clamped),
    /// at char column `col` (clamped to the line), or at the first
    /// non-blank char when `col` is `None`.
    pub fn go_to_line(&self, line: u64, col: Option<u64>) {
        let mut st = self.lock();
        let ed = &mut st.editor;
        let line = clamp_usize(line).min(ed.doc.line_count() - 1);
        let start = ed.doc.line_to_char(line);
        let len = ed.doc.line_len_chars(line);
        let offset = match col {
            Some(c) => clamp_usize(c).min(len),
            None => navigation::first_non_blank_col(&ed.doc, line),
        };
        let next = movement::set_caret(&ed.doc, start + offset);
        ed.set_selections(next);
        ed.history.break_group();
    }

    /// Primary caret as `(line, char column)`, for the history.
    pub fn caret_location(&self, key: String) -> NavLocation {
        let st = self.lock();
        let pos = st.editor.doc.char_to_position(st.editor.sel.primary().head);
        NavLocation {
            key,
            line: pos.line as u64,
            col: pos.col as u64,
        }
    }

    // --- Find and replace (M1-13) ---

    /// Sets the search. An empty pattern clears it. An invalid regex keeps
    /// no search and reports the reason in `FindStatus::error`.
    pub fn set_find(&self, pattern: String, config: FindConfig) -> FindStatus {
        let mut st = self.lock();
        let options = FindOptions {
            case_sensitive: config.case_sensitive,
            whole_word: config.whole_word,
            regex: config.regex,
        };
        match Finder::new(&pattern, options) {
            Ok(f) => {
                st.finder = Some(f);
                st.find_error = None;
            }
            Err(FindError::Empty) => {
                st.finder = None;
                st.find_error = None;
            }
            Err(FindError::Invalid(message)) => {
                st.finder = None;
                st.find_error = Some(message);
            }
        }
        st.find_status()
    }

    pub fn clear_find(&self) {
        let mut st = self.lock();
        st.finder = None;
        st.find_error = None;
    }

    pub fn find_status(&self) -> FindStatus {
        self.lock().find_status()
    }

    /// Snapshot for counting matches off the main thread, or `None` when
    /// there is no search.
    pub fn start_find_count(&self) -> Option<Arc<FindCountJob>> {
        let st = self.lock();
        let finder = st.finder.clone()?;
        Some(Arc::new(FindCountJob {
            finder,
            doc: st.editor.doc.clone(),
            sel: st.editor.sel.clone(),
            cancel: AtomicBool::new(false),
        }))
    }

    /// Selects the first match at or after the start of the current
    /// selection. This is what typing in the find field uses, so the
    /// selected match does not jump ahead while the pattern is refined.
    pub fn find_nearest(&self) -> bool {
        let mut st = self.lock();
        let hit = st
            .finder
            .as_ref()
            .and_then(|f| f.find_next(&st.editor.doc, st.editor.sel.primary().start(), true));
        hit.map(|r| st.select_match(r)).is_some()
    }

    /// Enter / Cmd+G: the next match after the current one, wrapping.
    pub fn find_next(&self) -> bool {
        let mut st = self.lock();
        let hit = st.finder.as_ref().and_then(|f| {
            let p = st.editor.sel.primary();
            f.find_next(&st.editor.doc, p.end(), true)
        });
        hit.map(|r| st.select_match(r)).is_some()
    }

    /// Shift+Enter / Shift+Cmd+G: the previous match, wrapping.
    pub fn find_previous(&self) -> bool {
        let mut st = self.lock();
        let hit = st.finder.as_ref().and_then(|f| {
            let p = st.editor.sel.primary();
            f.find_prev(&st.editor.doc, p.start(), true)
        });
        hit.map(|r| st.select_match(r)).is_some()
    }

    /// Selects every match (Alt+Enter), ready for multi-cursor editing.
    pub fn select_all_matches(&self) -> bool {
        let mut st = self.lock();
        let found = st
            .finder
            .as_ref()
            .and_then(|f| f.select_all(&st.editor.doc, st.editor.sel.primary().start()));
        let Some(sel) = found else { return false };
        st.editor.set_selections(sel);
        st.editor.history.break_group();
        true
    }

    /// Replaces the selected match (if the selection is exactly one) and
    /// moves on to the next. With nothing selected it just selects the next
    /// match, so repeated Enter walks through them. Returns `true` if a
    /// replacement was made.
    pub fn replace_current(&self, replacement: String) -> bool {
        let mut st = self.lock();
        let tx = st
            .finder
            .as_ref()
            .and_then(|f| f.replace_current(&st.editor.doc, &st.editor.sel, &replacement));
        match tx {
            Some(tx) => {
                st.editor.apply(tx, EditKind::Other, Instant::now());
                let next = st
                    .finder
                    .as_ref()
                    .and_then(|f| f.find_next(&st.editor.doc, st.editor.sel.primary().end(), true));
                if let Some(r) = next {
                    st.select_match(r);
                }
                true
            }
            None => {
                let next = st.finder.as_ref().and_then(|f| {
                    f.find_next(&st.editor.doc, st.editor.sel.primary().start(), true)
                });
                if let Some(r) = next {
                    st.select_match(r);
                }
                false
            }
        }
    }

    /// Replaces every match in one undo step. Returns how many.
    pub fn replace_all(&self, replacement: String) -> u64 {
        let mut st = self.lock();
        let done = st
            .finder
            .as_ref()
            .and_then(|f| f.replace_all(&st.editor.doc, &st.editor.sel, &replacement));
        let Some((tx, count)) = done else { return 0 };
        st.editor.apply(tx, EditKind::Other, Instant::now());
        count as u64
    }

    // --- Multi-cursor (M1-12) ---

    /// Cmd+D: selects the word at the caret, then adds the next match.
    pub fn select_next_occurrence(&self) {
        let mut st = self.lock();
        let ed = &mut st.editor;
        let next = multicursor::select_next_occurrence(&ed.doc, &ed.sel);
        ed.set_selections(next);
        ed.history.break_group();
    }

    /// Shift+Cmd+L: selects every occurrence of the current selection.
    pub fn select_all_occurrences(&self) {
        let mut st = self.lock();
        let ed = &mut st.editor;
        let next = multicursor::select_all_occurrences(&ed.doc, &ed.sel);
        ed.set_selections(next);
        ed.history.break_group();
    }

    /// Opt+Cmd+Up (`above`) or Opt+Cmd+Down: adds a caret on the next line.
    pub fn add_cursor_vertical(&self, above: bool) {
        let mut st = self.lock();
        let ed = &mut st.editor;
        let delta = if above { -1 } else { 1 };
        let next = multicursor::add_cursor_vertical(&ed.doc, &ed.sel, delta);
        ed.set_selections(next);
        ed.history.break_group();
    }

    /// Opt+Shift+drag: one selection per line between `anchor` and `head`.
    /// Columns are the chars from each position's own line start.
    pub fn column_select(&self, anchor: TextPos, head: TextPos) {
        let mut st = self.lock();
        let ed = &mut st.editor;
        let next = multicursor::column_selection(
            &ed.doc,
            virtual_column(&ed.doc, anchor),
            virtual_column(&ed.doc, head),
        );
        ed.set_selections(next);
        ed.history.break_group();
    }

    // --- Clipboard ---

    pub fn copy(&self) -> ClipboardText {
        let st = self.lock();
        clipboard::copy(&st.editor.doc, &st.editor.sel).into()
    }

    pub fn cut(&self) -> ClipboardText {
        let mut st = self.lock();
        let ed = &mut st.editor;
        let (copied, tx) = clipboard::cut(&ed.doc, &ed.sel);
        ed.apply(tx, EditKind::Other, Instant::now());
        copied.into()
    }

    /// Pastes text from the pasteboard. `linewise` comes from the flag
    /// stored by the matching copy.
    pub fn paste_clipboard(&self, text: String, linewise: bool) {
        let mut st = self.lock();
        let ed = &mut st.editor;
        let tx = if linewise {
            clipboard::paste_linewise(&ed.doc, &ed.sel, &text)
        } else {
            edit::paste(&ed.doc, &ed.sel, &text)
        };
        ed.apply(tx, EditKind::Other, Instant::now());
    }

    // --- IME (NSTextInputClient). All ranges are document-wide UTF-16. ---

    pub fn has_marked_text(&self) -> bool {
        self.lock().editor.has_marked_text()
    }

    pub fn marked_range(&self) -> Option<Utf16Range> {
        self.lock().editor.marked_range_utf16().map(Into::into)
    }

    pub fn selected_range(&self) -> Utf16Range {
        self.lock().editor.selected_range_utf16().into()
    }

    /// Length of the document in UTF-16 units (`NSRange` upper bound).
    pub fn len_utf16(&self) -> u64 {
        self.lock().editor.doc.len_utf16() as u64
    }

    /// Document-wide UTF-16 offset of a line/column position (clamped).
    pub fn utf16_offset(&self, pos: TextPos) -> u64 {
        let st = self.lock();
        let doc = &st.editor.doc;
        doc.char_to_utf16_offset(to_char(doc, pos)) as u64
    }

    /// Line/column of a document-wide UTF-16 offset (clamped).
    pub fn position_at(&self, offset: u64) -> TextPos {
        let st = self.lock();
        let doc = &st.editor.doc;
        let p = doc.char_to_utf16(doc.utf16_offset_to_char(clamp_usize(offset)));
        TextPos {
            line: p.line as u64,
            col_utf16: p.col as u64,
        }
    }

    pub fn text_in_range(&self, range: Utf16Range) -> String {
        let (location, length) = range_pair(range);
        self.lock().editor.text_utf16(location, length)
    }

    /// `setMarkedText`: `selected` is the caret/selection inside `text`.
    pub fn set_marked_text(
        &self,
        text: String,
        selected: Utf16Range,
        replacement: Option<Utf16Range>,
    ) {
        self.lock().editor.set_marked_text(
            &text,
            range_pair(selected),
            replacement.map(range_pair),
        );
    }

    /// `insertText(_:replacementRange:)` for composed text or a replaced
    /// range. Ends any composition as a single undo step.
    pub fn commit_text(&self, text: String, replacement: Option<Utf16Range>) {
        self.lock()
            .editor
            .commit_text(&text, replacement.map(range_pair));
    }

    pub fn unmark_text(&self) {
        self.lock().editor.unmark_text();
    }

    pub fn undo(&self) -> bool {
        self.lock().editor.undo()
    }

    pub fn redo(&self) -> bool {
        self.lock().editor.redo()
    }

    pub fn can_undo(&self) -> bool {
        self.lock().editor.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.lock().editor.history.can_redo()
    }

    /// Ends the current typing group (focus loss, explicit commands).
    pub fn break_undo_group(&self) {
        self.lock().editor.history.break_group();
    }

    /// Saves atomically to the document's current path.
    pub fn save(&self) -> Result<(), CoreError> {
        let mut st = self.lock();
        if st.path.as_os_str().is_empty() {
            return Err(CoreError::Io {
                message: "document has no path yet; use save as".into(),
            });
        }
        let stamp = st.editor.doc.save_stamped(&st.path)?;
        st.disk = Some(stamp);
        st.deleted_on_disk = false;
        st.editor.history.break_group();
        st.saved_state = st.editor.history.state_id();
        tracing::info!(path = %st.path.display(), "saved document");
        Ok(())
    }

    /// Saves atomically to `path` and makes it the document's path.
    pub fn save_as(&self, path: String) -> Result<(), CoreError> {
        let path = PathBuf::from(path);
        let mut st = self.lock();
        let stamp = st.editor.doc.save_stamped(&path)?;
        st.disk = Some(stamp);
        st.deleted_on_disk = false;
        st.editor.history.break_group();
        tracing::info!(path = %path.display(), "saved document as");
        st.saved_state = st.editor.history.state_id();
        st.path = path;
        Ok(())
    }
}

impl DocumentHandle {
    /// Loads `path` and remembers what was on disk.
    fn load(path: PathBuf) -> Result<Self, CoreError> {
        let (stamp, bytes) = DiskStamp::read(&path)?;
        let handle = Self::new(Document::from_bytes(&bytes)?, path);
        handle.lock().disk = Some(stamp);
        Ok(handle)
    }

    fn new(doc: Document, path: PathBuf) -> Self {
        Self {
            state: Mutex::new(State {
                disk: None,
                deleted_on_disk: false,
                saved_state: 0,
                editor: Editor::new(doc),
                path,
                indent: IndentConfig::default(),
                finder: None,
                find_error: None,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Typing a space or punctuation still groups with surrounding typing in
/// VSCode; only explicit breaks (pause, cursor move) split. Multi-char
/// inserts (IME commit, auto-close) are their own step.
fn insert_kind(text: &str) -> EditKind {
    if text.chars().count() == 1 && text != "\n" {
        EditKind::Insert
    } else {
        EditKind::Other
    }
}

fn to_char(doc: &Document, p: TextPos) -> usize {
    doc.utf16_to_char(Utf16Position::new(
        clamp_usize(p.line),
        clamp_usize(p.col_utf16),
    ))
}

/// `(line, char column)` for a UTF-16 position, keeping columns that lie
/// past the end of a short line. Column selection needs them: dragging
/// right of a short line must still describe the same box on longer lines.
fn virtual_column(doc: &Document, p: TextPos) -> (usize, usize) {
    let line = clamp_usize(p.line).min(doc.line_count() - 1);
    let col = clamp_usize(p.col_utf16);
    let line_start = doc.line_to_char(line);
    let line_utf16 = doc.char_to_utf16(doc.line_end_char(line)).col;
    if col > line_utf16 {
        let line_chars = doc.line_len_chars(line);
        (line, line_chars.saturating_add(col - line_utf16))
    } else {
        let at = doc.utf16_to_char(Utf16Position::new(line, col));
        (line, at - line_start)
    }
}

fn clamp_usize(v: u64) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    struct ChannelListener(Mutex<mpsc::Sender<CoreEvent>>);

    impl CoreListener for ChannelListener {
        fn on_event(&self, event: CoreEvent) {
            let _ = self.0.lock().unwrap().send(event);
        }
    }

    #[test]
    fn command_palette_searches_builtin_commands() {
        let p = CommandPalette::new();
        assert!(p.commands().len() > 30);
        let found = p.search("save as".into(), None);
        assert_eq!(found[0].id, "workbench.action.files.saveAs");
        assert_eq!(found[0].label, "File: Save As…");
        assert!(!found[0].highlights.is_empty());
        p.note_used("workbench.action.gotoLine".into());
        let only = p.search(
            String::new(),
            Some(vec!["workbench.action.gotoLine".into()]),
        );
        assert_eq!(only.len(), 1);
        assert!(only[0].is_recent);
        assert_eq!(p.recent(), ["workbench.action.gotoLine"]);
    }

    #[test]
    fn go_to_line_and_history() {
        let doc = Core::new().new_document();
        doc.edit(EditCommand::InsertText {
            text: "one\n    two\nthree".into(),
        });
        assert_eq!(
            parse_line_target("2:3".into(), doc.line_count()),
            Some(LineTarget {
                line: 1,
                col: Some(2)
            })
        );
        doc.go_to_line(1, None);
        let here = doc.caret_location("k".into());
        assert_eq!((here.line, here.col), (1, 4), "first non-blank char");
        doc.go_to_line(99, Some(99));
        let end = doc.caret_location("k".into());
        assert_eq!((end.line, end.col), (2, 5), "clamped");

        let h = NavigationHistory::new();
        h.record(here.clone());
        assert!(h.can_go_back());
        assert_eq!(
            h.back(Some(NavLocation {
                key: "j".into(),
                ..end
            })),
            Some(here)
        );
        assert!(h.can_go_forward());
    }

    #[test]
    fn file_finder_searches_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "").unwrap();
        std::fs::write(dir.path().join("README.md"), "").unwrap();
        let ws = Core::new()
            .open_workspace(dir.path().to_string_lossy().into_owned())
            .unwrap();
        let finder = ws.build_file_finder();
        assert_eq!(finder.file_count(), 2);
        let found = finder.search("main".into(), 10);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].rel_path, "src/main.rs");
        assert_eq!(found[0].name_start, 4);
        assert_eq!(
            found[0].highlights,
            [Utf16Range {
                location: 4,
                length: 4
            }]
        );
        finder.set_context(None, vec!["README.md".into()]);
        let recent = finder.search(String::new(), 10);
        assert_eq!(recent[0].rel_path, "README.md");
        assert!(recent[0].is_recent);
    }

    #[test]
    fn ping_delivers_pong_via_listener() {
        let (tx, rx) = mpsc::channel();
        let core = Core::new();
        core.set_listener(Arc::new(ChannelListener(Mutex::new(tx))));
        core.ping("hi".into());
        let event = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(
            event,
            CoreEvent::Pong {
                message: "hi".into()
            }
        );
    }

    #[test]
    fn open_document_reads_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "fn main() {\n}\n").unwrap();

        let doc = Core::new()
            .open_document(path.to_string_lossy().into_owned())
            .unwrap();
        assert_eq!(doc.line_count(), 3);
        assert_eq!(doc.lines(0, 2), vec!["fn main() {", "}"]);
    }

    #[test]
    fn save_preserves_crlf_and_utf16() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.txt");
        let mut src = vec![0xFF, 0xFE];
        for u in "a\r\nb\r\n".encode_utf16() {
            src.extend(u.to_le_bytes());
        }
        std::fs::write(&path, &src).unwrap();

        let doc = Core::new()
            .open_document(path.to_string_lossy().into_owned())
            .unwrap();
        assert_eq!(doc.encoding(), TextEncoding::Utf16Le);
        assert_eq!(doc.line_ending(), LineEndingKind::CrLf);
        assert!(doc.has_bom());
        doc.save().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), src);

        let other = dir.path().join("out.txt");
        doc.set_encoding(TextEncoding::Utf8);
        doc.set_line_ending(LineEndingKind::Lf);
        doc.save_as(other.to_string_lossy().into_owned()).unwrap();
        assert_eq!(std::fs::read(&other).unwrap(), b"\xEF\xBB\xBFa\nb\n");
        assert_eq!(doc.path(), other.to_string_lossy());
    }

    fn pos(line: u64, col: u64) -> TextPos {
        TextPos {
            line,
            col_utf16: col,
        }
    }

    fn caret(line: u64, col: u64) -> TextSelection {
        TextSelection {
            anchor: pos(line, col),
            head: pos(line, col),
        }
    }

    #[test]
    fn edit_viewport_undo_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("e.txt");
        std::fs::write(&path, "a😀\nb\n").unwrap();
        let doc = Core::new()
            .open_document(path.to_string_lossy().into_owned())
            .unwrap();

        // Carets after the emoji (UTF-16 col 3) and at end of line 1.
        doc.set_selections(vec![caret(0, 3), caret(1, 1)], 0);
        doc.edit(EditCommand::InsertText { text: "!".into() });
        let snap = doc.viewport(0, 10);
        assert_eq!(snap.lines[0].text, "a😀!");
        assert_eq!(snap.lines[0].carets, vec![4]);
        assert_eq!(snap.lines[1].carets, vec![2]);
        assert_eq!(snap.selection_count, 2);
        assert_eq!(snap.primary.col, 3);
        assert_eq!(snap.primary.col_utf16, 4);

        assert!(doc.undo());
        assert_eq!(doc.lines(0, 2), vec!["a😀", "b"]);
        assert!(doc.redo());
        assert_eq!(doc.lines(0, 2), vec!["a😀!", "b!"]);
    }

    #[test]
    fn selection_is_reported_in_utf16() {
        let doc = DocumentHandle::new(Document::from_text("😀ab\ncd"), PathBuf::new());
        doc.set_selections(
            vec![TextSelection {
                anchor: pos(0, 2),
                head: pos(1, 1),
            }],
            0,
        );
        let snap = doc.viewport(0, 2);
        assert_eq!(
            snap.lines[0].selections,
            vec![LineSpan {
                start: 2,
                end: 4,
                includes_eol: true
            }]
        );
        assert_eq!(
            snap.lines[1].selections,
            vec![LineSpan {
                start: 0,
                end: 1,
                includes_eol: false
            }]
        );
        // Mid-surrogate column rounds down to the char start.
        doc.set_selections(vec![caret(0, 1)], 0);
        assert_eq!(doc.viewport(0, 1).primary.col_utf16, 0);
    }

    #[test]
    fn ime_composition_round_trip_over_ffi() {
        let doc = DocumentHandle::new(Document::from_text("😀"), PathBuf::new());
        doc.set_selections(vec![caret(0, 2)], 0);

        let range = |location, length| Utf16Range { location, length };
        for preedit in ["k", "ka", "か"] {
            let n = preedit.encode_utf16().count() as u64;
            doc.set_marked_text(preedit.into(), range(n, 0), None);
            assert!(doc.has_marked_text());
        }
        // Marked range is document-wide: the emoji occupies units 0..2.
        assert_eq!(doc.marked_range(), Some(range(2, 1)));
        assert_eq!(doc.selected_range(), range(3, 0));
        assert_eq!(doc.text_in_range(range(0, 99)), "😀か");
        assert_eq!(doc.utf16_offset(pos(0, 3)), 3);
        assert_eq!(doc.position_at(3), pos(0, 3));
        assert_eq!(doc.position_at(1), pos(0, 0)); // inside the surrogate pair
        let snap = doc.viewport(0, 1);
        let marked = snap.lines[0].marked.unwrap();
        assert_eq!((marked.start, marked.end), (2, 3));

        doc.commit_text("漢".into(), None);
        assert!(!doc.has_marked_text());
        assert_eq!(doc.lines(0, 1), vec!["😀漢"]);
        assert!(doc.viewport(0, 1).lines[0].marked.is_none());
        assert!(doc.undo());
        assert_eq!(doc.lines(0, 1), vec!["😀"]);
    }

    #[test]
    fn new_document_is_empty_editable_and_needs_save_as() {
        let doc = Core::new().new_document();
        assert_eq!(doc.line_count(), 1);
        assert_eq!(doc.path(), "");
        doc.edit(EditCommand::InsertText { text: "hi".into() });
        assert_eq!(doc.lines(0, 1), vec!["hi"]);
        assert!(matches!(doc.save(), Err(CoreError::Io { .. })));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.txt");
        doc.save_as(path.to_string_lossy().into_owned()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hi");
        doc.save().unwrap();
    }

    #[test]
    fn workspace_lists_lazily_and_opens_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("README.md"), "hi").unwrap();

        let ws = Core::new()
            .open_workspace(dir.path().to_string_lossy().into_owned())
            .unwrap();
        let root: Vec<_> = ws
            .list_dir(String::new())
            .unwrap()
            .into_iter()
            .map(|e| (e.name, e.is_dir))
            .collect();
        assert_eq!(
            root,
            vec![("src".into(), true), ("README.md".into(), false)]
        );

        let src = ws.list_dir("src".into()).unwrap();
        assert_eq!(src[0].rel_path, "src/main.rs");
        let doc = ws.open_file(src[0].rel_path.clone()).unwrap();
        assert_eq!(doc.lines(0, 1), vec!["fn main() {}"]);
        assert!(doc.path().ends_with("src/main.rs"));

        // The UI can never read outside the folder.
        assert!(ws.list_dir("..".into()).is_err());
        assert!(ws.open_file("../outside".into()).is_err());
        assert!(ws.open_file("/etc/passwd".into()).is_err());
        assert!(
            Core::new()
                .open_workspace("/nonexistent/dir".into())
                .is_err()
        );
    }

    /// Opens a real file; the second write uses a later mtime so the size
    /// and time check sees it even within one clock tick.
    fn file_doc(text: &str) -> (tempfile::TempDir, PathBuf, Arc<DocumentHandle>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, text).unwrap();
        let doc = Core::new()
            .open_document(path.to_string_lossy().into_owned())
            .unwrap();
        (dir, path, doc)
    }

    fn write_later(path: &std::path::Path, text: &str) {
        std::fs::write(path, text).unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(later)
            .unwrap();
    }

    fn all_text(doc: &DocumentHandle) -> String {
        doc.lines(0, doc.line_count()).join("\n")
    }

    #[test]
    fn external_changes_reload_as_one_undo_step() {
        let (_dir, path, doc) = file_doc("one\ntwo\n");
        assert_eq!(doc.check_disk(), DiskStatus::Unchanged);
        write_later(&path, "one\nTWO\n");
        assert_eq!(doc.check_disk(), DiskStatus::Changed);
        assert!(doc.reload_from_disk().unwrap());
        assert_eq!(all_text(&doc), "one\nTWO\n");
        assert!(!doc.is_modified(), "reloaded text counts as saved");
        assert_eq!(doc.check_disk(), DiskStatus::Unchanged);
        assert!(doc.undo());
        assert_eq!(all_text(&doc), "one\ntwo\n");
        assert!(doc.is_modified(), "undoing the reload differs from disk");
    }

    #[test]
    fn our_own_save_is_not_an_external_change() {
        let (_dir, _path, doc) = file_doc("a");
        doc.edit(EditCommand::InsertText { text: "b".into() });
        doc.save().unwrap();
        assert_eq!(doc.check_disk(), DiskStatus::Unchanged);
    }

    #[test]
    fn keeping_the_buffer_stops_reporting_and_stays_unsaved() {
        let (_dir, path, doc) = file_doc("a");
        doc.edit(EditCommand::InsertText {
            text: "mine ".into(),
        });
        write_later(&path, "theirs");
        assert_eq!(doc.check_disk(), DiskStatus::Changed);
        doc.keep_buffer_over_disk();
        assert_eq!(doc.check_disk(), DiskStatus::Unchanged);
        assert!(doc.undo());
        assert!(
            doc.is_modified(),
            "even undone to the old text, the file differs"
        );
        doc.save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a");
        assert!(!doc.is_modified());
    }

    #[test]
    fn a_deleted_file_keeps_its_text_and_counts_as_unsaved() {
        let (_dir, path, doc) = file_doc("keep me");
        std::fs::remove_file(&path).unwrap();
        assert_eq!(doc.check_disk(), DiskStatus::Deleted);
        assert!(doc.is_modified());
        assert_eq!(all_text(&doc), "keep me");
        // Restored with the same content (git checkout): clean again.
        std::fs::write(&path, "keep me").unwrap();
        assert_eq!(doc.check_disk(), DiskStatus::Unchanged);
        assert!(!doc.is_modified());
        // Deleted, then saved: recreated.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(doc.check_disk(), DiskStatus::Deleted);
        doc.save().unwrap();
        assert!(!doc.is_modified());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep me");
    }

    fn doc_with(text: &str) -> Arc<DocumentHandle> {
        Arc::new(DocumentHandle::new(
            Document::from_text(text),
            PathBuf::new(),
        ))
    }

    fn primary(doc: &DocumentHandle) -> (u64, u64) {
        let r = doc.selected_range();
        (r.location, r.length)
    }

    #[test]
    fn navigation_selection_and_mouse() {
        let doc = doc_with("hello world\nfoo");
        doc.set_caret(pos(0, 6));
        doc.move_cursors(CursorMotion::Right, true);
        assert_eq!(primary(&doc), (6, 1));
        doc.move_cursors(CursorMotion::Left, false);
        assert_eq!(primary(&doc), (6, 0));
        doc.move_cursors(CursorMotion::Down, false);
        assert_eq!(
            primary(&doc),
            (15, 0),
            "column 6 clamps to the end of 'foo'"
        );
        doc.move_cursors(CursorMotion::DocStart, false);
        doc.move_cursors(CursorMotion::WordRight, true);
        assert_eq!(primary(&doc), (0, 5));

        doc.select_word_at(pos(0, 8));
        assert_eq!(primary(&doc), (6, 5));
        doc.select_line_at(pos(0, 3));
        assert_eq!(primary(&doc), (0, 12), "line plus its break");
        doc.select_all();
        assert_eq!(primary(&doc), (0, 15));

        doc.set_caret(pos(0, 1));
        doc.add_caret(pos(1, 1));
        assert_eq!(doc.viewport(0, 2).selection_count, 2);
        doc.extend_selection_to(pos(1, 3));
        doc.set_caret(pos(0, 0));
        assert_eq!(
            doc.viewport(0, 2).selection_count,
            1,
            "a click resets to one caret"
        );
        doc.extend_selection_to(pos(0, 4));
        assert_eq!(primary(&doc), (0, 4));
    }

    #[test]
    fn multi_cursor_commands_edit_every_cursor() {
        let doc = doc_with("foo bar\nfoo baz\nqux");
        doc.set_caret(pos(0, 1));
        doc.select_next_occurrence();
        assert_eq!(primary(&doc), (0, 3), "first press selects the word");
        doc.select_next_occurrence();
        assert_eq!(doc.viewport(0, 3).selection_count, 2);
        doc.edit(EditCommand::InsertText { text: "X".into() });
        assert_eq!(doc.lines(0, 3), vec!["X bar", "X baz", "qux"]);
        assert!(doc.undo());
        assert_eq!(
            doc.lines(0, 3),
            vec!["foo bar", "foo baz", "qux"],
            "one undo step for both"
        );

        doc.select_all_occurrences();
        assert_eq!(doc.viewport(0, 3).selection_count, 2);

        doc.set_caret(pos(0, 2));
        doc.add_cursor_vertical(false);
        doc.add_cursor_vertical(false);
        assert_eq!(doc.viewport(0, 3).selection_count, 3);
        doc.add_cursor_vertical(false);
        assert_eq!(
            doc.viewport(0, 3).selection_count,
            3,
            "no line below the last"
        );
        doc.add_cursor_vertical(true);
        assert_eq!(doc.viewport(0, 3).selection_count, 3);

        doc.column_select(pos(0, 4), pos(2, 6));
        assert_eq!(doc.viewport(0, 3).selection_count, 3);
        doc.edit(EditCommand::InsertText { text: "|".into() });
        assert_eq!(
            doc.lines(0, 3),
            vec!["foo |r", "foo |z", "qux|"],
            "the box is columns 4..6; the short line only gets a caret at its end"
        );
    }

    #[test]
    fn column_select_keeps_columns_past_short_lines_and_counts_utf16() {
        // Dragging from the short last line still spans the same columns
        // on the long lines above it.
        let doc = doc_with("abcdef\nabcdef\nab");
        doc.column_select(pos(2, 5), pos(0, 1));
        assert_eq!(doc.viewport(0, 3).selection_count, 3);
        doc.edit(EditCommand::InsertText { text: "|".into() });
        // Columns 1..5 cover "b" on the short line, which is replaced too.
        assert_eq!(doc.lines(0, 3), vec!["a|f", "a|f", "a|"]);

        // An astral char is two UTF-16 units but one column.
        let doc = doc_with("a\u{1F600}bc\nabcd");
        doc.column_select(pos(0, 3), pos(1, 3));
        doc.edit(EditCommand::InsertText { text: "|".into() });
        assert_eq!(doc.lines(0, 2), vec!["a\u{1F600}|c", "ab|d"]);
    }

    fn cfg(case_sensitive: bool, whole_word: bool, regex: bool) -> FindConfig {
        FindConfig {
            case_sensitive,
            whole_word,
            regex,
        }
    }

    #[test]
    fn find_walks_matches_and_reports_status() {
        let doc = doc_with("cat Cat concat\ncat");
        let st = doc.set_find("cat".into(), cfg(false, true, false));
        assert_eq!((st.total, st.current, st.error.clone()), (3, None, None));

        assert!(doc.find_nearest());
        assert_eq!(primary(&doc), (0, 3));
        assert_eq!(doc.find_status().current, Some(1));
        assert!(doc.find_next());
        assert_eq!(primary(&doc), (4, 3));
        assert!(doc.find_next());
        assert_eq!(primary(&doc), (15, 3));
        assert!(doc.find_next());
        assert_eq!(primary(&doc), (0, 3), "wraps around");
        assert!(doc.find_previous());
        assert_eq!(primary(&doc), (15, 3), "and backwards");
        assert_eq!(doc.find_status().current, Some(3));

        // Options change the result set.
        assert_eq!(doc.set_find("cat".into(), cfg(true, false, false)).total, 3);
        assert_eq!(doc.set_find("Cat".into(), cfg(true, false, false)).total, 1);
    }

    #[test]
    fn find_matches_are_part_of_the_viewport_in_utf16_columns() {
        let doc = doc_with("\u{1F600}ab ab\nxx");
        doc.set_find("ab".into(), cfg(true, false, false));
        let snap = doc.viewport(0, 2);
        let cols: Vec<_> = snap.lines[0]
            .matches
            .iter()
            .map(|m| (m.start, m.end))
            .collect();
        assert_eq!(cols, vec![(2, 4), (5, 7)]);
        assert!(snap.lines[1].matches.is_empty());
        doc.clear_find();
        assert!(doc.viewport(0, 2).lines[0].matches.is_empty());
        assert!(!doc.find_next(), "no search, nothing to find");
    }

    #[test]
    fn invalid_regex_reports_an_error_and_keeps_no_search() {
        let doc = doc_with("a(b");
        let st = doc.set_find("(".into(), cfg(true, false, true));
        assert!(st.error.as_deref().is_some_and(|e| e.contains("unclosed")));
        assert_eq!(st.total, 0);
        assert!(!doc.find_next());
        // A valid literal recovers; an empty pattern clears without an error.
        assert_eq!(doc.set_find("(".into(), cfg(true, false, false)).total, 1);
        let cleared = doc.set_find(String::new(), cfg(true, false, false));
        assert_eq!((cleared.total, cleared.error), (0, None));
    }

    #[test]
    fn replace_one_then_all_are_single_undo_steps() {
        let doc = doc_with("a1 a2 a3");
        doc.set_find(r"a(\d)".into(), cfg(true, false, true));
        assert!(
            !doc.replace_current("<$1>".into()),
            "nothing selected yet: just selects"
        );
        assert_eq!(primary(&doc), (0, 2));
        assert!(doc.replace_current("<$1>".into()));
        assert_eq!(doc.lines(0, 1), vec!["<1> a2 a3"]);
        assert_eq!(primary(&doc), (4, 2), "moved on to the next match");

        assert_eq!(doc.replace_all("[$1]".into()), 2);
        assert_eq!(doc.lines(0, 1), vec!["<1> [2] [3]"]);
        assert!(doc.undo());
        assert_eq!(
            doc.lines(0, 1),
            vec!["<1> a2 a3"],
            "replace all is one step"
        );
        assert!(doc.undo());
        assert_eq!(doc.lines(0, 1), vec!["a1 a2 a3"]);
        // Undoing back to the loaded text is clean again.
        assert!(!doc.is_modified());
        assert_eq!(doc.replace_all("x".into()), 3);
        doc.clear_find();
        assert_eq!(doc.replace_all("y".into()), 0, "no search, no replacement");
    }

    #[test]
    fn long_lines_are_cut_and_matches_come_from_the_shown_part() {
        let cut = viewport::MAX_LINE_DISPLAY_CHARS;
        let long = format!("{}ab{}", "x".repeat(10), "y".repeat(cut + 100));
        let doc = doc_with(&format!("{long}ab\nab"));
        doc.set_find("ab".into(), cfg(true, false, false));
        let snap = doc.viewport(0, 2);
        assert!(snap.lines[0].truncated);
        assert_eq!(snap.lines[0].text.chars().count(), cut);
        let shown: Vec<_> = snap.lines[0].matches.iter().map(|m| m.start).collect();
        assert_eq!(shown, vec![10], "the match past the cut is not drawn");
        assert!(!snap.lines[1].truncated);
        assert_eq!(doc.display_line(0), snap.lines[0].text);
        assert_eq!(doc.display_line(99), "ab", "clamped to the last line");
        // Search itself still sees the whole line.
        assert_eq!(doc.find_status().total, 3);
    }

    #[test]
    fn big_documents_count_in_a_background_job() {
        // Over the sync limit, with few enough matches to check exactly:
        // "ab" starts every 100th line.
        let filler = "x".repeat(40);
        let lines = SYNC_FIND_COUNT_BYTES as usize / 40 + 100;
        let text: String = (0..lines)
            .map(|i| {
                if i % 100 == 0 {
                    format!("ab{filler}\n")
                } else {
                    format!("{filler}\n")
                }
            })
            .collect();
        let expected = lines.div_ceil(100) as u64;
        let doc = doc_with(&text);
        let st = doc.set_find("ab".into(), cfg(true, false, false));
        assert!(!st.counted, "too big to count on the caller's thread");
        assert!(doc.find_nearest());
        assert_eq!(primary(&doc), (0, 2), "finding still works right away");

        let job = doc.start_find_count().unwrap();
        assert_eq!(job.revision(), doc.revision());
        let counted = std::thread::spawn(move || job.run())
            .join()
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                counted.total,
                counted.current,
                counted.counted,
                counted.truncated
            ),
            (expected, Some(1), true, false)
        );

        // Small documents still count synchronously.
        assert!(
            doc_with("ab")
                .set_find("ab".into(), cfg(true, false, false))
                .counted
        );
        // No search, no job.
        doc.clear_find();
        assert!(doc.start_find_count().is_none());
    }

    #[test]
    fn a_count_job_is_a_snapshot_and_can_be_cancelled() {
        let doc = doc_with("ab ab");
        doc.set_find("ab".into(), cfg(true, false, false));
        let job = doc.start_find_count().unwrap();
        doc.edit(EditCommand::InsertText { text: "ab ".into() });
        assert_ne!(
            job.revision(),
            doc.revision(),
            "the UI can tell the result is stale"
        );
        assert_eq!(
            job.run().unwrap().total,
            2,
            "counts the text it was created from"
        );
        assert_eq!(doc.find_status().total, 3);

        let job = doc.start_find_count().unwrap();
        job.cancel();
        assert!(job.run().is_none());
    }

    #[test]
    fn indent_settings_round_trip() {
        let doc = doc_with("x");
        assert_eq!(
            doc.indent_settings(),
            IndentSettings {
                use_tabs: false,
                tab_size: 4
            }
        );
        doc.set_indent(true, 8);
        assert_eq!(
            doc.indent_settings(),
            IndentSettings {
                use_tabs: true,
                tab_size: 8
            }
        );
    }

    #[test]
    fn select_all_matches_allows_multi_cursor_editing() {
        let doc = doc_with("ab x ab\nab");
        doc.set_find("ab".into(), cfg(true, false, false));
        assert!(doc.select_all_matches());
        assert_eq!(doc.viewport(0, 2).selection_count, 3);
        doc.edit(EditCommand::InsertText { text: "Z".into() });
        assert_eq!(doc.lines(0, 2), vec!["Z x Z", "Z"]);
    }

    #[test]
    fn clipboard_round_trip_including_linewise() {
        let doc = doc_with("one\ntwo\nthree");
        doc.set_caret(pos(1, 1));
        let copied = doc.copy();
        assert_eq!(
            copied,
            ClipboardText {
                text: "two\n".into(),
                linewise: true
            }
        );
        assert_eq!(
            doc.lines(0, 5).join("|"),
            "one|two|three",
            "copy does not edit"
        );
        assert!(!doc.is_modified());

        doc.paste_clipboard(copied.text.clone(), copied.linewise);
        assert_eq!(doc.lines(0, 5).join("|"), "one|two|two|three");

        let cut = doc.cut();
        assert!(cut.linewise);
        assert_eq!(doc.lines(0, 5).join("|"), "one|two|three");
        assert!(doc.undo());
        assert_eq!(doc.lines(0, 5).join("|"), "one|two|two|three");

        doc.select_word_at(pos(0, 1));
        let word = doc.cut();
        assert_eq!(
            word,
            ClipboardText {
                text: "one".into(),
                linewise: false
            }
        );
        doc.paste_clipboard("1".into(), false);
        assert_eq!(doc.lines(0, 1), vec!["1"]);
    }

    #[test]
    fn is_modified_tracks_edits_and_saves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "hi").unwrap();
        let doc = Core::new()
            .open_document(path.to_string_lossy().into_owned())
            .unwrap();
        assert!(!doc.is_modified());

        doc.move_cursors(CursorMotion::DocEnd, false);
        assert!(!doc.is_modified(), "moving the cursor is not an edit");
        doc.edit(EditCommand::InsertText { text: "!".into() });
        assert!(doc.is_modified());
        doc.save().unwrap();
        assert!(!doc.is_modified());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hi!");

        doc.undo();
        assert!(doc.is_modified(), "undo past the save differs from disk");
        doc.redo();
        assert!(!doc.is_modified(), "redo back to the saved text is clean");
        doc.undo();
        doc.select_all();
        assert!(doc.is_modified());
        let other = dir.path().join("b.txt");
        doc.save_as(other.to_string_lossy().into_owned()).unwrap();
        assert!(!doc.is_modified());
        assert_eq!(doc.path(), other.to_string_lossy());

        // A new Untitled buffer starts clean, then becomes modified.
        let fresh = Core::new().new_document();
        assert!(!fresh.is_modified());
        fresh.edit(EditCommand::InsertText { text: "x".into() });
        assert!(fresh.is_modified());
    }

    #[test]
    fn undoing_back_to_the_saved_text_is_not_modified() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "hi").unwrap();
        let doc = Core::new()
            .open_document(path.to_string_lossy().into_owned())
            .unwrap();
        doc.move_cursors(CursorMotion::DocEnd, false);
        doc.edit(EditCommand::InsertText { text: "!".into() });
        doc.edit(EditCommand::Backspace);
        assert!(doc.is_modified(), "same text by editing is still an edit");
        while doc.undo() {}
        assert!(!doc.is_modified(), "undo back to the loaded text is clean");
        doc.redo();
        assert!(doc.is_modified());
        doc.undo();
        assert!(!doc.is_modified());

        // Marked IME text counts until it is committed or cancelled.
        doc.set_marked_text(
            "k".into(),
            Utf16Range {
                location: 1,
                length: 0,
            },
            None,
        );
        assert!(doc.is_modified());
        doc.unmark_text();
        assert!(doc.is_modified(), "committed text is an edit");
        doc.undo();
        assert!(!doc.is_modified());
    }

    struct Collect(std::sync::mpsc::Sender<WatchBatch>);
    impl WorkspaceListener for Collect {
        fn on_changes(&self, batch: WatchBatch) {
            let _ = self.0.send(batch);
        }
    }

    #[test]
    fn watching_feeds_the_file_finder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "x").unwrap();
        let ws = Core::new()
            .open_workspace(dir.path().to_string_lossy().into_owned())
            .unwrap();
        let finder = ws.build_file_finder();
        assert_eq!(finder.file_count(), 1);
        let (tx, rx) = std::sync::mpsc::channel();
        let watch = ws.watch(Arc::new(Collect(tx))).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));

        std::fs::write(dir.path().join("b.rs"), "x").unwrap();
        let mut paths = Vec::new();
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while !paths.iter().any(|p: &String| p == "b.rs") && Instant::now() < deadline {
            if let Ok(b) = rx.recv_timeout(std::time::Duration::from_millis(500)) {
                paths.extend(b.paths);
            }
        }
        assert!(finder.apply_changes(Arc::clone(&ws), paths));
        assert_eq!(finder.file_count(), 2);
        assert_eq!(finder.search("b.rs".into(), 5)[0].rel_path, "b.rs");

        watch.stop();
        std::fs::write(dir.path().join("c.rs"), "x").unwrap();
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(800))
                .is_err()
        );
    }

    #[test]
    fn open_missing_document_errors() {
        let err = Core::new().open_document("/nonexistent/x".into());
        assert!(matches!(err, Err(CoreError::Io { .. })));
    }
}
