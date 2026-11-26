//! UniFFI bridge exposed to Swift.
//!
//! Design rule: the UI calls in with coarse-grained commands and receives
//! batched events through `CoreListener`. No per-character FFI chatter.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use galaxy_core::LanguageIndexStats;
use galaxy_core::analysis::{
    BuiltinStore, CompletionItem as LspCompletionItem, DefinitionLocation as LspDefinitionLocation,
    Diagnostic as LspDiagnostic, DiagnosticSeverity, HoverInfo as LspHoverInfo, Range as LspRange,
    RenameEdit as LspRenameEdit,
};
use galaxy_core::change::{Change, ChangeSet};
use galaxy_core::clipboard;
use galaxy_core::commands::{self, CommandRegistry};
use galaxy_core::completion::{self, Language as SimpleCompletionLanguage};
use galaxy_core::dart::{self, DartEngine};
use galaxy_core::disk::{DiskCheck, DiskStamp};
use galaxy_core::document::{Document, Encoding, LineEnding};
use galaxy_core::edit::{self, IndentConfig};
use galaxy_core::file_finder::FileFinder as CoreFileFinder;
use galaxy_core::find::{FindError, FindOptions, Finder};
use galaxy_core::fold::{self, FoldSet};
use galaxy_core::go::{self, GoEngine};
use galaxy_core::history::{EditKind, Editor};
use galaxy_core::javascript;
use galaxy_core::keymap::{self, Keymap};
use galaxy_core::lang::{
    Action, Completion, HoverResult, OutlineSymbol as LangOutlineSymbol, Problem, Signature, Target,
};
use galaxy_core::movement::{self, Motion};
use galaxy_core::multicursor;
use galaxy_core::navigation::{self, Location, NavHistory};
use galaxy_core::parameter_hints;
use galaxy_core::php::{self, PhpEngine};
use galaxy_core::position::Utf16Position;
use galaxy_core::rust::{self, RustEngine};
use galaxy_core::selection::{Selection, SelectionSet};
use galaxy_core::snippets;
use galaxy_core::syntax::{self, SyntaxLanguage, SyntaxService, SyntaxSnapshot};
use galaxy_core::terminal::TerminalSession;
use galaxy_core::typescript::{DocumentIndex, stdlib::StdLib};
use galaxy_core::viewport;
use galaxy_core::watcher::WorkspaceWatcher;
use galaxy_core::workspace::{
    SearchMatch as CoreSearchMatch, SearchOptions as CoreSearchOptions, Workspace as CoreWorkspace,
};

mod rust_diagnostics_cache;
mod typescript_editor;
mod typescript_globals;
mod typescript_navigation;

use typescript_editor::{
    TypeScriptWorkspaceExports, typescript_completions, typescript_hover_markdown,
    typescript_outline_symbol,
};
use typescript_navigation::{
    TypeScriptNavigationCache, ambient_module_location, module_root_location,
    resolve_typescript_definition_import, resolve_typescript_export,
    resolve_typescript_import_uncached, resolve_typescript_type_export,
    typescript_call_option_target, typescript_decorator_option_target, typescript_import_target,
    typescript_type_member_target, typescript_type_target,
};

uniffi::setup_scaffolding!();

/// Errors surfaced to Swift as a thrown `CoreError`.
#[derive(Debug, uniffi::Error)]
pub enum CoreError {
    Io { message: String },
}

/// Outcome of an explicitly requested Rust generated-source refresh.
#[derive(Debug, Clone, uniffi::Record)]
pub struct RustGeneratedSourceRefresh {
    pub succeeded: bool,
    pub timed_out: bool,
    pub canceled: bool,
    pub compiler_artifacts: u32,
    pub message: String,
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
    Pong {
        message: String,
    },
    GoModulesStarted {
        root: String,
    },
    GoModulesFinished {
        root: String,
    },
    GoModulesFailed {
        root: String,
        message: String,
    },
    WorkspaceIndexStarted {
        root: String,
        total: u64,
    },
    WorkspaceIndexProgress {
        root: String,
        completed: u64,
        total: u64,
    },
    WorkspaceIndexPhase {
        root: String,
        phase: String,
        completed: u64,
        total: u64,
    },
    WorkspaceIndexFinished {
        root: String,
        total: u64,
        changed: u64,
    },
    PhpIndexFinished {
        root: String,
        files: u64,
    },
}

/// Implemented in Swift. Called from core threads, so the Swift side must
/// hop to the main actor before touching UI.
#[uniffi::export(with_foreign)]
pub trait CoreListener: Send + Sync {
    fn on_event(&self, event: CoreEvent);
}

/// Renders Markdown to an HTML fragment for the preview pane.
#[uniffi::export]
pub fn markdown_to_html(source: String) -> String {
    galaxy_core::markdown::to_html(&source)
}

/// Returns the core version.
#[uniffi::export]
pub fn core_version() -> String {
    galaxy_core::version().to_string()
}

/// Definition of a `Log::channel('name')` channel for the Laravel project
/// containing `file` (`config/logging.php`).
#[uniffi::export]
pub fn laravel_log_channel_location(file: String, channel: String) -> Option<DefinitionLocation> {
    let location = galaxy_core::php::framework::laravel::config::log_channel_location(
        Path::new(&file),
        &channel,
    )?;
    Some(DefinitionLocation {
        uri: file_uri(&location.path),
        line: location.line as u64,
        column: location.column_utf16 as u64,
    })
}

/// Initializes file logging in `dir`. Returns `false` if already initialized.
#[uniffi::export]
pub fn init_logging(dir: String, level: String) -> Result<bool, CoreError> {
    Ok(galaxy_core::logging::init(&PathBuf::from(dir), &level)?)
}

/// One keyboard shortcut (M2-10). `key_id` is canonical text such as
/// `shift+cmd+p`; the Swift side builds the same text from key events.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct KeyBindingInfo {
    pub command: String,
    pub key_id: String,
    /// Lowercase character, or a name such as `up`, `tab`, `f5`.
    pub name: String,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub cmd: bool,
}

/// Effective shortcuts: a command's first binding is the one menus show.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct KeymapLoad {
    pub bindings: Vec<KeyBindingInfo>,
    /// One line per skipped entry, or one for a file that does not parse.
    pub problems: Vec<String>,
}

/// Default shortcuts, with the text of the user's `keybindings.json`
/// applied when given.
#[uniffi::export]
pub fn load_keymap(user_file: Option<String>) -> KeymapLoad {
    let registry = commands::builtin();
    let (map, problems) = match user_file {
        Some(text) => Keymap::with_user_file(&text, &registry),
        None => (Keymap::defaults(), Vec::new()),
    };
    KeymapLoad {
        bindings: map
            .bindings()
            .iter()
            .map(|b| KeyBindingInfo {
                command: b.command.clone(),
                key_id: b.key.id(),
                name: b.key.name.clone(),
                ctrl: b.key.ctrl,
                shift: b.key.shift,
                alt: b.key.alt,
                cmd: b.key.cmd,
            })
            .collect(),
        problems,
    }
}

/// Starting content for a new `keybindings.json`.
#[uniffi::export]
pub fn keybindings_template() -> String {
    keymap::user_file_template(&commands::builtin())
}

/// A registered command (M2-09).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CommandInfo {
    pub id: String,
    pub category: String,
    pub title: String,
}

/// Static package metadata for JavaScript and TypeScript project context.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct JavaScriptProjectInfo {
    pub root: String,
    pub name: Option<String>,
    pub frameworks: Vec<String>,
    pub package_manager: String,
}

/// One framework component under a conventional JavaScript project source root.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct JavaScriptComponentInfo {
    pub framework: String,
    pub path: String,
    pub name: String,
    pub symbols: Vec<JavaScriptComponentSymbolInfo>,
}

/// A statically recognized component declaration and directly enumerable props.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct JavaScriptComponentSymbolInfo {
    pub name: String,
    pub props: Vec<String>,
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

/// A definition in the active document's completed Tree-sitter outline.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct OutlineSymbol {
    pub uri: Option<String>,
    pub name: String,
    pub kind: String,
    pub line: u64,
    pub column: u64,
    pub depth: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LspOutlineSymbol {
    pub uri: Option<String>,
    pub name: String,
    pub kind: String,
    pub line: u64,
    pub column: u64,
    pub depth: u64,
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
    /// What the built-in engines last answered, per document.
    analysis: Arc<Mutex<BuiltinStore>>,
    /// Built-in PHP engines, one per project root.
    php: Arc<Mutex<HashMap<PathBuf, Arc<Mutex<PhpEngine>>>>>,
    /// Built-in Go engines, one per project root.
    go: Arc<Mutex<HashMap<PathBuf, Arc<Mutex<GoEngine>>>>>,
    /// Built-in Rust engines, one per Cargo project root.
    rust: Arc<Mutex<HashMap<PathBuf, Arc<Mutex<RustEngine>>>>>,
    /// Built-in Dart engines, one per Flutter/Dart package root.
    dart: Arc<Mutex<HashMap<PathBuf, Arc<Mutex<DartEngine>>>>>,
    /// Roots with a snapshot rebuild in flight. Watcher batches are coalesced
    /// here so completion only reads installed in-memory indexes.
    indexing_roots: Arc<Mutex<HashMap<PathBuf, IndexingState>>>,
    /// Open files handled by built-in analysis, whether or not a language
    /// server serves them.
    php_documents: Mutex<HashMap<String, std::sync::Weak<DocumentHandle>>>,
    /// Per open PHP file: text revision last analysed, when it was first
    /// seen and whether the analysis ran.
    php_checked: Mutex<HashMap<String, (u64, Instant, bool)>>,
    /// Per open TypeScript file: buffer revision last indexed. Analysis is
    /// polled frequently for other engines, but reparsing unchanged buffers
    /// would traverse the whole Tree-sitter tree on every poll.
    typescript_checked: Mutex<HashMap<String, u64>>,
    /// Workspace export generation `typescript_checked` was computed with;
    /// missing-import warnings depend on it, so a change rechecks buffers.
    typescript_checked_exports: Mutex<u64>,
    /// Per open Rust file: buffer revision last checked. Rust diagnostics walk
    /// the parsed source, so repeating that work from the UI's 100 ms poll
    /// makes a newly navigated-to file stall scrolling.
    rust_checked: Mutex<HashMap<String, u64>>,
    /// Per open Dart file: buffer revision last checked for diagnostics.
    dart_checked: Mutex<HashMap<String, u64>>,
    /// Parsed disk documents and successful module resolutions used by TypeScript navigation.
    typescript_navigation: Arc<Mutex<TypeScriptNavigationCache>>,
    /// Parsed direct exports from visible workspace TypeScript sources.
    typescript_workspace_exports: Arc<Mutex<TypeScriptWorkspaceExports>>,
}

#[derive(Default)]
struct IndexingState {
    rerun: bool,
    pending_typescript: Vec<String>,
}

impl Default for Core {
    fn default() -> Self {
        Self {
            listener: RwLock::new(None),
            analysis: Arc::new(Mutex::new(BuiltinStore::default())),
            php: Arc::new(Mutex::new(HashMap::new())),
            go: Arc::new(Mutex::new(HashMap::new())),
            rust: Arc::new(Mutex::new(HashMap::new())),
            dart: Arc::new(Mutex::new(HashMap::new())),
            indexing_roots: Arc::new(Mutex::new(HashMap::new())),
            php_documents: Mutex::new(HashMap::new()),
            php_checked: Mutex::new(HashMap::new()),
            typescript_checked: Mutex::new(HashMap::new()),
            typescript_checked_exports: Mutex::new(0),
            rust_checked: Mutex::new(HashMap::new()),
            dart_checked: Mutex::new(HashMap::new()),
            typescript_navigation: Arc::new(Mutex::new(TypeScriptNavigationCache::default())),
            typescript_workspace_exports: Arc::new(Mutex::new(
                TypeScriptWorkspaceExports::default(),
            )),
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

    /// Reads the nearest `package.json` project for a file or directory.
    /// This is metadata only; semantic requests go to the TypeScript engine.
    pub fn javascript_project(&self, path: String) -> Option<JavaScriptProjectInfo> {
        javascript::Project::detect(Path::new(&path)).map(|project| JavaScriptProjectInfo {
            root: project.root.to_string_lossy().into_owned(),
            name: project.name,
            frameworks: project
                .frameworks
                .into_iter()
                .map(|framework| framework.name().to_owned())
                .collect(),
            package_manager: project.package_manager.name().to_owned(),
        })
    }

    /// Components found for frameworks explicitly declared by the nearest
    /// package manifest. This is an index for navigation/UI, not type analysis.
    pub fn javascript_components(&self, path: String) -> Vec<JavaScriptComponentInfo> {
        let Some(project) = javascript::Project::detect(Path::new(&path)) else {
            return Vec::new();
        };
        javascript::scan_components(&project)
            .into_iter()
            .map(|component| JavaScriptComponentInfo {
                framework: component.framework.name().to_owned(),
                path: component.path.to_string_lossy().into_owned(),
                name: component.name,
                symbols: component
                    .symbols
                    .into_iter()
                    .map(|symbol| JavaScriptComponentSymbolInfo {
                        name: symbol.name,
                        props: symbol.props,
                    })
                    .collect(),
            })
            .collect()
    }

    /// Resolves only a relative JavaScript or TypeScript module specifier.
    /// Bare packages remain for semantic tooling and are never guessed here.
    pub fn resolve_javascript_relative_import(
        &self,
        specifier: String,
        from_path: String,
    ) -> Option<DefinitionLocation> {
        javascript::resolve_relative_import(&specifier, Path::new(&from_path)).map(|path| {
            DefinitionLocation {
                uri: file_uri(&path),
                line: 0,
                column: 0,
            }
        })
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
                .name("galaxy-ping".into())
                .spawn(move || listener.on_event(CoreEvent::Pong { message }))
                .ok();
        }
    }

    /// Runs `go mod download` for a detected Go project. The command is
    /// user-invoked, has fixed arguments and never goes through a shell. A
    /// successful download clears only the Go caches, then rebuilds the
    /// project index without discarding open buffers.
    pub fn download_go_modules(&self, root: String) -> bool {
        let root = PathBuf::from(root);
        let Some(project) = go::GoProject::detect(&root) else {
            return false;
        };
        let root = project.root().to_path_buf();
        let listener = self
            .listener
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let engines = self.go.lock().unwrap_or_else(|e| e.into_inner());
        let engine = engines
            .get(&root)
            .cloned()
            .unwrap_or_else(|| Arc::new(Mutex::new(GoEngine::pending(&root))));
        drop(engines);
        let map = &self.go;
        map.lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(root.clone())
            .or_insert_with(|| engine.clone());
        std::thread::Builder::new()
            .name("go-mod-download".into())
            .spawn(move || {
                if let Some(listener) = &listener {
                    listener.on_event(CoreEvent::GoModulesStarted {
                        root: root.to_string_lossy().into_owned(),
                    });
                }
                let result = Command::new("go")
                    .args(["mod", "download"])
                    .current_dir(&root)
                    .output();
                match result {
                    Ok(output) if output.status.success() => {
                        engine
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .reset_index();
                        spawn_go_index(&engine, &root);
                        if let Some(listener) = &listener {
                            listener.on_event(CoreEvent::GoModulesFinished {
                                root: root.to_string_lossy().into_owned(),
                            });
                        }
                    }
                    Ok(output) => {
                        let mut message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                        if message.is_empty() {
                            message = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                        }
                        if message.is_empty() {
                            message = format!("go mod download exited with {}", output.status);
                        }
                        if let Some(listener) = &listener {
                            listener.on_event(CoreEvent::GoModulesFailed {
                                root: root.to_string_lossy().into_owned(),
                                message: message.chars().take(4000).collect(),
                            });
                        }
                    }
                    Err(error) => {
                        if let Some(listener) = &listener {
                            listener.on_event(CoreEvent::GoModulesFailed {
                                root: root.to_string_lossy().into_owned(),
                                message: error.to_string(),
                            });
                        }
                    }
                }
            })
            .is_ok()
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
        // Core lives for the whole app, while built-in results are scoped to
        // one workspace. Drop old diagnostics before detached index workers
        // start, so Problems never mixes projects after switching folders.
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.php_documents
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.php_checked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.typescript_checked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.rust_checked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.typescript_navigation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.typescript_workspace_exports
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.indexing_roots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(ws.root().to_path_buf(), IndexingState::default());
        spawn_workspace_indexing(
            ws.clone(),
            Arc::clone(&self.analysis),
            Arc::clone(&self.typescript_workspace_exports),
            Arc::clone(&self.go),
            Arc::clone(&self.php),
            Arc::clone(&self.rust),
            Arc::clone(&self.dart),
            Arc::clone(&self.indexing_roots),
            self.listener
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        );
        Ok(Arc::new(Workspace { inner: ws }))
    }

    /// Rebuilds built-in indexes after a workspace watcher batch. The worker
    /// atomically installs a new snapshot while requests use the old one.
    pub fn refresh_workspace_indexes(&self, root: String) {
        let root = PathBuf::from(root);
        {
            let mut indexing = self
                .indexing_roots
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(state) = indexing.get_mut(&root) {
                state.rerun = true;
                return;
            }
            indexing.insert(root.clone(), IndexingState::default());
        }
        let workspace = match CoreWorkspace::open(root.clone()) {
            Ok(workspace) => workspace,
            Err(_) => {
                self.indexing_roots
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&root);
                return;
            }
        };
        spawn_workspace_indexing(
            workspace,
            Arc::clone(&self.analysis),
            Arc::clone(&self.typescript_workspace_exports),
            Arc::clone(&self.go),
            Arc::clone(&self.php),
            Arc::clone(&self.rust),
            Arc::clone(&self.dart),
            Arc::clone(&self.indexing_roots),
            self.listener
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        );
    }

    /// Sets the PHP compatibility target for a workspace. An empty value uses
    /// Composer's platform constraint again.
    pub fn set_php_target_version(&self, root: String, version: String) -> bool {
        let root = PathBuf::from(root);
        let version = if version.is_empty() {
            None
        } else {
            match php::PhpVersion::parse(&version) {
                Some(version) => Some(version),
                None => return false,
            }
        };
        let engines = self.php.lock().unwrap_or_else(|e| e.into_inner());
        let Some(engine) = engines.get(&root) else {
            return true;
        };
        engine
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_php_version(version);
        true
    }

    /// Runs Cargo for a manual workspace refresh or the package containing a
    /// saved source, then rebuilds the Rust engine for fresh OUT_DIR output.
    pub fn refresh_generated_rust_sources(
        &self,
        root: String,
        source: String,
    ) -> RustGeneratedSourceRefresh {
        let root = PathBuf::from(root);
        let source = (!source.is_empty()).then(|| PathBuf::from(source));
        let result = rust::refresh_generated_sources(&root, source.as_deref());
        if !result.canceled {
            let mut analysis = self.analysis.lock().unwrap_or_else(|e| e.into_inner());
            analysis.clear_compiler_diagnostics_under(&file_uri(&root));
            let mut diagnostics_by_uri: HashMap<String, Vec<LspDiagnostic>> = HashMap::new();
            let mut actions_by_uri: HashMap<String, Vec<galaxy_core::analysis::CodeActionInfo>> =
                HashMap::new();
            for diagnostic in &result.diagnostics {
                diagnostics_by_uri
                    .entry(diagnostic.uri.clone())
                    .or_default()
                    .push(LspDiagnostic {
                        uri: diagnostic.uri.clone(),
                        range: diagnostic.range,
                        severity: diagnostic.severity,
                        message: diagnostic.message.clone(),
                        source: Some("rustc".into()),
                        code: diagnostic.code.clone(),
                        unnecessary: false,
                    });
                if !diagnostic.actions.is_empty() {
                    actions_by_uri
                        .entry(diagnostic.uri.clone())
                        .or_default()
                        .extend(diagnostic.actions.clone());
                }
            }
            for (uri, diagnostics) in diagnostics_by_uri {
                analysis.set_compiler_diagnostics(&uri, diagnostics);
                analysis.set_compiler_code_actions(
                    &uri,
                    actions_by_uri.remove(&uri).unwrap_or_default(),
                );
            }
        }
        // A failed check can still regenerate build-script output and must not
        // leave semantic requests pending after an automatic refresh.
        if !result.canceled && !result.timed_out {
            let engine = {
                let mut engines = self.rust.lock().unwrap_or_else(|e| e.into_inner());
                engines
                    .entry(root.clone())
                    .or_insert_with(|| Arc::new(Mutex::new(RustEngine::pending(&root))))
                    .clone()
            };
            let built = build_rust_index(&root);
            engine
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .install(built);
        }
        RustGeneratedSourceRefresh {
            succeeded: result.succeeded,
            timed_out: result.timed_out,
            canceled: result.canceled,
            compiler_artifacts: result.compiler_artifacts,
            message: result.message,
        }
    }

    /// Cancels an explicitly requested Cargo refresh for this workspace.
    pub fn cancel_generated_rust_source_refresh(&self, root: String) -> bool {
        rust::cancel_generated_source_refresh(&PathBuf::from(root))
    }

    /// Applies source changes directly. Only project metadata that can change
    /// module resolution needs a full workspace snapshot rebuild.
    pub fn apply_workspace_changes(&self, root: String, paths: Vec<String>, rescan: bool) {
        let root = PathBuf::from(root);
        // PHP's Composer index includes project classes and `vendor/`. Unlike
        // TypeScript, it has no cheap per-file delta: rebuild it in the
        // background, retaining the old snapshot until the replacement is
        // ready. `vendor/` is deliberately included because Composer writes
        // autoload metadata there after install/dump-autoload.
        let full_reindex = rescan
            || paths
                .iter()
                .any(|path| workspace_index_metadata(Path::new(path)));
        if !full_reindex && paths.iter().any(|path| php_index_input_changed(path)) {
            refresh_php_engines(&self.php, &root);
        }
        if full_reindex {
            self.refresh_workspace_indexes(root.to_string_lossy().into_owned());
            return;
        }
        let paths = paths
            .into_iter()
            .filter(|path| {
                let path = Path::new(path);
                matches!(
                    path.extension().and_then(|extension| extension.to_str()),
                    Some("dart" | "rs")
                ) || is_typescript_path(path)
            })
            .collect::<Vec<_>>();
        if paths.is_empty() {
            return;
        }
        {
            let mut indexing = self
                .indexing_roots
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(state) = indexing.get_mut(&root) {
                if paths.iter().all(|path| is_typescript_path(Path::new(path))) {
                    state.pending_typescript.extend(paths);
                    state.pending_typescript.sort();
                    state.pending_typescript.dedup();
                } else {
                    state.rerun = true;
                }
                return;
            }
        }
        let paths: Vec<PathBuf> = paths.into_iter().map(|path| root.join(path)).collect();
        let dart_paths: Vec<PathBuf> = paths
            .iter()
            .filter(|path| {
                path.extension().and_then(|extension| extension.to_str()) == Some("dart")
            })
            .cloned()
            .collect();
        let rust_paths: Vec<PathBuf> = paths
            .iter()
            .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("rs"))
            .cloned()
            .collect();
        let typescript_paths: Vec<String> = paths
            .iter()
            .filter_map(|path| path.strip_prefix(&root).ok())
            .filter(|path| is_typescript_path(path))
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .collect();
        let dart = Arc::clone(&self.dart);
        let rust = Arc::clone(&self.rust);
        let typescript_exports = Arc::clone(&self.typescript_workspace_exports);
        let _ = std::thread::Builder::new()
            .name("workspace-index-delta".into())
            .spawn(move || {
                let started = Instant::now();
                if !dart_paths.is_empty()
                    && let Some(engine) = dart
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&root)
                        .cloned()
                {
                    let _ = engine
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .apply_disk_changes(&root, &dart_paths);
                }
                let mut rust_by_project: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
                for path in rust_paths {
                    rust_by_project
                        .entry(rust::project_root(&path))
                        .or_default()
                        .push(path);
                }
                for (project, paths) in rust_by_project {
                    if let Some(engine) = rust
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&project)
                        .cloned()
                    {
                        let _ = engine
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .apply_disk_changes(&paths);
                    }
                }
                if !typescript_paths.is_empty() {
                    typescript_exports
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .apply_changes(&root, &typescript_paths);
                }
                tracing::debug!(
                    root = %root.display(),
                    ms = started.elapsed().as_millis() as u64,
                    "applied built-in workspace index delta"
                );
            });
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

    /// Tells the core a document is open so the built-in engines analyse
    /// it. `true` when an engine handles the file.
    pub fn register_document(
        &self,
        document: Arc<DocumentHandle>,
        language: String,
        root: String,
    ) -> bool {
        let _ = (&language, &root);
        let path = document.path();
        if path.is_empty() {
            return false;
        }
        let path = Path::new(&path);
        if !(is_php_path(path)
            || is_blade_path(path)
            || is_go_path(path)
            || is_rust_path(path)
            || is_dart_path(path)
            || is_typescript_path(path)
            || is_component_path(path))
        {
            return false;
        }
        self.php_documents
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(file_uri(path), Arc::downgrade(&document));
        true
    }

    /// Runs the built-in analysis on open documents and hands the problems
    /// to each one. Returns how many documents it looked at.
    pub fn poll_analysis(&self) -> u64 {
        let mut analysis = self.analysis.lock().unwrap_or_else(|e| e.into_inner());
        let documents: HashMap<String, Arc<DocumentHandle>> = {
            let mut open = self.php_documents.lock().unwrap_or_else(|e| e.into_inner());
            let gone: Vec<String> = open
                .iter()
                .filter(|(_, weak)| weak.strong_count() == 0)
                .map(|(uri, _)| uri.clone())
                .collect();
            for uri in gone {
                open.remove(&uri);
                analysis.forget(&uri);
                self.release_go_buffer(&uri);
                self.release_rust_buffer(&uri);
                self.typescript_checked
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&uri);
                self.rust_checked
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&uri);
                self.dart_checked
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&uri);
            }
            open.iter()
                .filter_map(|(uri, weak)| Some((uri.clone(), weak.upgrade()?)))
                .collect()
        };
        self.check_php(&mut analysis, &documents);
        self.check_rust(&mut analysis, &documents);
        self.check_dart(&mut analysis, &documents);
        self.check_typescript(&mut analysis, &documents);
        for (uri, document) in &documents {
            let diagnostics = analysis
                .diagnostics(uri)
                .iter()
                .map(|diagnostic| DiagnosticInfo {
                    start_line: diagnostic.range.start_line,
                    start_character: diagnostic.range.start_character,
                    end_line: diagnostic.range.end_line,
                    end_character: diagnostic.range.end_character,
                    severity: match diagnostic.severity {
                        galaxy_core::analysis::DiagnosticSeverity::Error => 1,
                        galaxy_core::analysis::DiagnosticSeverity::Warning => 2,
                        galaxy_core::analysis::DiagnosticSeverity::Information => 3,
                        galaxy_core::analysis::DiagnosticSeverity::Hint => 4,
                    },
                    message: diagnostic.message.clone(),
                    source: diagnostic.source.clone(),
                    code: diagnostic.code.clone(),
                    unnecessary: diagnostic.unnecessary,
                })
                .collect();
            document.set_diagnostics(diagnostics);
        }
        documents.len() as u64
    }

    pub fn request_hover(&self, document: Arc<DocumentHandle>, line: u64, character: u64) -> bool {
        let path = document.path();
        if path.is_empty() {
            return false;
        }
        let uri = file_uri(Path::new(&path));
        if is_typescript_path(Path::new(&path)) || is_component_path(Path::new(&path)) {
            let text = document.lock().editor.doc.text();
            let Some(source) = component_position_source(&path, &text, line as usize) else {
                self.analysis
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .set_hover(&uri, None);
                return false;
            };
            let index = DocumentIndex::parse(typescript_index_path(&path), &source);
            let position = Utf16Position::new(line as usize, character as usize);
            let hover = index
                .symbol_at(position.line, position.col)
                .and_then(|symbol| {
                    let range = symbol
                        .occurrences
                        .iter()
                        .copied()
                        .find(|range| range.start <= position && position < range.end)?;
                    Some(LspHoverInfo {
                        contents: typescript_hover_markdown(
                            symbol.kind,
                            &symbol.name,
                            Path::new(&path),
                        ),
                        range: Some(typescript_range(range)),
                    })
                })
                .or_else(|| {
                    let (target, occurrences) = typescript_import_target(
                        &self.typescript_navigation,
                        &path,
                        &index,
                        position.line,
                        position.col,
                    )?;
                    let range = occurrences
                        .iter()
                        .copied()
                        .find(|range| range.start <= position && position < range.end)?;
                    Some(LspHoverInfo {
                        contents: format!(
                            "**import**\n\n`{}`",
                            target.uri.strip_prefix("file://").unwrap_or(&target.uri)
                        ),
                        range: Some(typescript_range(range)),
                    })
                });
            let answered = hover.is_some();
            self.analysis
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .set_hover(&uri, hover);
            return answered;
        }
        if is_dart_path(Path::new(&path)) {
            let text = document.lock().editor.doc.text();
            let hover = self
                .dart_engine(Path::new(&path))
                .and_then(|engine| {
                    let engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                    engine.hover(Path::new(&path), &text, line as u32, character as u32)
                })
                .map(|found| LspHoverInfo {
                    contents: found.markdown,
                    range: Some(LspRange {
                        start_line: u64::from(found.line),
                        start_character: u64::from(found.start_column),
                        end_line: u64::from(found.line),
                        end_character: u64::from(found.end_column),
                    }),
                });
            let answered = hover.is_some();
            self.analysis
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .set_hover(&uri, hover);
            return answered;
        }
        let engine = if is_rust_path(Path::new(&path)) {
            self.rust_engine(Path::new(&path)).map(Engine::Rust)
        } else if is_go_path(Path::new(&path)) {
            self.go_engine(Path::new(&path)).map(Engine::Go)
        } else {
            self.php_engine_any(Path::new(&path)).map(Engine::Php)
        };
        if let Some(engine) = engine {
            if matches!(&engine, Engine::Php(engine) if !engine.lock().unwrap_or_else(|e| e.into_inner()).is_ready())
            {
                return false;
            }
            let text = document.lock().editor.doc.text();
            let found = engine.hover(Path::new(&path), &text, line as u32, character as u32);
            let hover = found.map(|hover| LspHoverInfo {
                contents: hover.markdown,
                range: Some(LspRange {
                    start_line: u64::from(hover.line),
                    start_character: u64::from(hover.start_column),
                    end_line: u64::from(hover.line),
                    end_character: u64::from(hover.end_column),
                }),
            });
            let answered = hover.is_some();
            self.analysis
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .set_hover(&uri, hover);
            return answered;
        }
        false
    }

    pub fn hover(&self, document: Arc<DocumentHandle>) -> Option<HoverInfo> {
        let path = document.path();
        if path.is_empty() {
            return None;
        }
        let uri = file_uri(Path::new(&path));
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .hover(&uri)
            .map(|hover| HoverInfo {
                contents: hover.contents.clone(),
                start_line: hover.range.map(|range| range.start_line),
                start_character: hover.range.map(|range| range.start_character),
                end_line: hover.range.map(|range| range.end_line),
                end_character: hover.range.map(|range| range.end_character),
            })
    }

    pub fn request_completion(
        &self,
        document: Arc<DocumentHandle>,
        line: u64,
        character: u64,
    ) -> bool {
        // Completion runs away from the UI thread. Do not let an older request
        // replace the cache after the document has changed underneath it.
        let revision = document.revision();
        let path = document.path();
        if path.is_empty() {
            return false;
        }
        let uri = file_uri(Path::new(&path));
        if is_typescript_path(Path::new(&path)) || is_component_path(Path::new(&path)) {
            // Completion sees source between keystrokes, including incomplete
            // member expressions. Keep every TypeScript preparation step inside
            // the FFI panic boundary so such input cannot crash Swift.
            let completion = catch_unwind(AssertUnwindSafe(|| {
                let text = document.lock().editor.doc.text();
                let source = component_position_source(&path, &text, line as usize)?;
                let script_insertion_line =
                    component_script_insertion_line(&path, &text, line as usize).unwrap_or(0);
                let index_path = if is_component_path(Path::new(&path)) {
                    format!("{path}.ts")
                } else {
                    path.clone()
                };
                let component_extension = is_component_path(Path::new(&path))
                    .then(|| {
                        Path::new(&path)
                            .extension()
                            .and_then(|extension| extension.to_str())
                    })
                    .flatten();
                let exports = self
                    .typescript_workspace_exports
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                Some(typescript_completions(
                    &index_path,
                    &uri,
                    &source,
                    line as usize,
                    character as usize,
                    &exports,
                    &self.typescript_navigation,
                    script_insertion_line,
                    component_extension,
                ))
            }))
            .ok()
            .flatten();
            let Some(items) = completion else {
                return false;
            };
            let answered = !items.is_empty();
            if document.revision() == revision {
                self.analysis
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .set_completion(&uri, items);
            }
            return answered;
        }
        if is_dart_path(Path::new(&path)) {
            let text = document.lock().editor.doc.text();
            let items: Vec<LspCompletionItem> = self
                .dart_engine(Path::new(&path))
                .map(|engine| {
                    let engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                    if engine.is_ready() {
                        engine.complete(Path::new(&path), &text, line as u32, character as u32)
                    } else {
                        Default::default()
                    }
                })
                .unwrap_or_default()
                .into_iter()
                .map(php_completion_item)
                .collect();
            let answered = !items.is_empty();
            if document.revision() == revision {
                self.analysis
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .set_completion(&uri, items);
            }
            return answered;
        }
        if matches!(
            Path::new(&path).extension().and_then(|ext| ext.to_str()),
            Some("py" | "swift")
        ) {
            let language = if path.ends_with(".py") {
                SimpleCompletionLanguage::Python
            } else {
                SimpleCompletionLanguage::Swift
            };
            let text = document.lock().editor.doc.text();
            let items: Vec<LspCompletionItem> =
                completion::complete(language, &text, line as u32, character as u32)
                    .into_iter()
                    .map(php_completion_item)
                    .collect();
            let answered = !items.is_empty();
            if document.revision() == revision {
                self.analysis
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .set_completion(&uri, items);
            }
            return answered;
        }
        let engine = if is_rust_path(Path::new(&path)) {
            self.rust_engine(Path::new(&path)).map(Engine::Rust)
        } else if is_go_path(Path::new(&path)) {
            self.go_engine(Path::new(&path)).map(Engine::Go)
        } else {
            self.php_engine_any(Path::new(&path)).map(Engine::Php)
        };
        if let Some(engine) = engine {
            let text = document.lock().editor.doc.text();
            let found = engine.complete(Path::new(&path), &text, line as u32, character as u32);
            let items: Vec<LspCompletionItem> =
                found.into_iter().map(php_completion_item).collect();
            let answered = !items.is_empty();
            if document.revision() == revision {
                self.analysis
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .set_completion(&uri, items);
            }
            return answered;
        }
        false
    }

    pub fn completions(&self, document: Arc<DocumentHandle>) -> Vec<CompletionInfo> {
        let path = document.path();
        if path.is_empty() {
            return Vec::new();
        }
        let uri = file_uri(Path::new(&path));
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .completions(&uri)
            .iter()
            .map(|item| CompletionInfo {
                label: item.label.clone(),
                detail: item.detail.clone(),
                insert_text: item.insert_text.clone(),
                is_snippet: item.is_snippet,
                additional_edits: item
                    .additional_edits
                    .iter()
                    .map(|edit| TextEditInfo {
                        line: edit.range.start_line,
                        column: edit.range.start_character,
                        text: edit.new_text.clone(),
                    })
                    .collect(),
                replace_from: item.replace_from,
                kind: item.kind,
            })
            .collect()
    }

    pub fn request_signature_help(
        &self,
        document: Arc<DocumentHandle>,
        line: u64,
        character: u64,
    ) -> bool {
        let path = document.path();
        if path.is_empty() {
            return false;
        }
        let uri = file_uri(Path::new(&path));
        let engine = if is_rust_path(Path::new(&path)) {
            self.rust_engine(Path::new(&path)).map(Engine::Rust)
        } else if is_go_path(Path::new(&path)) {
            self.go_engine(Path::new(&path)).map(Engine::Go)
        } else {
            self.php_engine(Path::new(&path)).map(Engine::Php)
        };
        if let Some(engine) = engine {
            let text = document.lock().editor.doc.text();
            let found = engine.signature(Path::new(&path), &text, line as u32, character as u32);
            let signature = found.map(|found| galaxy_core::analysis::SignatureInfo {
                label: found.label,
                documentation: found.documentation,
                parameters: found.parameters,
                active_parameter: u64::from(found.active),
            });
            let answered = signature.is_some();
            self.analysis
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .set_signature(&uri, signature);
            return answered;
        }
        false
    }

    /// Parameter names to show before the call arguments on lines
    /// `first_line..first_line + line_count`, computed from the engines'
    /// own declarations (VS Code's inline-parameters). Calls the engine
    /// cannot resolve get no labels. Never panics into Swift.
    pub fn parameter_hints(
        &self,
        document: Arc<DocumentHandle>,
        first_line: u64,
        line_count: u64,
    ) -> ParameterHints {
        let path = document.path();
        // Text, revision and displayed lines come from one snapshot, so the
        // editor can tell an answer about older text from a current one.
        let doc = document.lock().editor.doc.clone();
        let empty = ParameterHints {
            revision: doc.revision(),
            lines: Vec::new(),
        };
        if path.is_empty() || line_count == 0 {
            return empty;
        }
        let first = u32::try_from(first_line).unwrap_or(u32::MAX);
        let last = first.saturating_add(u32::try_from(line_count - 1).unwrap_or(u32::MAX));
        catch_unwind(AssertUnwindSafe(|| {
            let file = Path::new(&path);
            let text = doc.text();
            let hints = if is_typescript_path(file) || is_component_path(file) {
                self.typescript_parameter_hints(&path, &text, first, last)
            } else if is_rust_path(file) {
                self.rust_engine(file)
                    .map(|engine| {
                        let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                        engine.parameter_hints(file, &text, first, last)
                    })
                    .unwrap_or_default()
            } else if is_dart_path(file) {
                self.dart_engine(file)
                    .map(|engine| {
                        let engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                        if engine.is_ready() {
                            engine.parameter_hints(file, &text, first, last)
                        } else {
                            Vec::new()
                        }
                    })
                    .unwrap_or_default()
            } else if is_go_path(file) {
                self.go_engine(file)
                    .map(|engine| {
                        let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                        if engine.is_ready() {
                            engine.parameter_hints(file, &text, first, last)
                        } else {
                            Vec::new()
                        }
                    })
                    .unwrap_or_default()
            } else if is_php_path(file) {
                self.php_engine(file)
                    .map(|engine| {
                        let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                        if engine.is_ready() {
                            engine.parameter_hints(file, &text, first, last)
                        } else {
                            Vec::new()
                        }
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let mut lines: Vec<ParameterHintLine> = Vec::new();
            for hint in hints {
                let line = hint.line as usize;
                if line >= doc.line_count() {
                    continue;
                }
                if lines
                    .last()
                    .is_none_or(|last| last.line != u64::from(hint.line))
                {
                    lines.push(ParameterHintLine {
                        line: u64::from(hint.line),
                        text: viewport::display_text(&doc, line),
                        hints: Vec::new(),
                    });
                }
                let Some(entry) = lines.last_mut() else {
                    continue;
                };
                // Column 0 has no character to widen; columns past the
                // displayed part of a long line are never drawn.
                let column = u64::from(hint.column);
                if column == 0 || column > entry.text.encode_utf16().count() as u64 {
                    continue;
                }
                entry.hints.push(ParameterHintInfo {
                    line: entry.line,
                    column,
                    label: hint.label,
                    return_type: hint.return_type,
                });
            }
            lines.retain(|line| !line.hints.is_empty());
            ParameterHints {
                revision: doc.revision(),
                lines,
            }
        }))
        .unwrap_or(empty)
    }

    /// Edits that declare the inferred return type of the PHP function whose
    /// parameter list closes at `line`/`column`, with the `use` it needs.
    /// Empty when nothing can be inferred there.
    pub fn return_type_edits(
        &self,
        document: Arc<DocumentHandle>,
        line: u64,
        column: u64,
    ) -> Vec<RenameEdit> {
        let path = document.path();
        let file = Path::new(&path);
        if path.is_empty() || !is_php_path(file) {
            return Vec::new();
        }
        let (Ok(line), Ok(column)) = (u32::try_from(line), u32::try_from(column)) else {
            return Vec::new();
        };
        let text = document.lock().editor.doc.text();
        let uri = file_uri(file);
        catch_unwind(AssertUnwindSafe(|| {
            let Some(engine) = self.php_engine(file) else {
                return Vec::new();
            };
            let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
            if !engine.is_ready() {
                return Vec::new();
            }
            engine
                .return_type_edits(file, &text, line, column)
                .into_iter()
                .map(|edit| RenameEdit {
                    uri: uri.clone(),
                    start_line: u64::from(edit.start_line),
                    start_character: u64::from(edit.start_column),
                    end_line: u64::from(edit.end_line),
                    end_character: u64::from(edit.end_column),
                    new_text: edit.text,
                })
                .collect()
        }))
        .unwrap_or_default()
    }

    pub fn signature_help(&self, document: Arc<DocumentHandle>) -> Option<SignatureInfo> {
        let path = document.path();
        if path.is_empty() {
            return None;
        }
        let uri = file_uri(Path::new(&path));
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .signature_help(&uri)
            .map(|signature| SignatureInfo {
                label: signature.label.clone(),
                documentation: signature.documentation.clone(),
                parameters: signature.parameters.clone(),
                active_parameter: signature.active_parameter,
            })
    }

    pub fn request_definition(
        &self,
        document: Arc<DocumentHandle>,
        line: u64,
        character: u64,
    ) -> bool {
        let path = document.path();
        if path.is_empty() {
            return false;
        }
        let uri = file_uri(Path::new(&path));
        if is_typescript_path(Path::new(&path)) || is_component_path(Path::new(&path)) {
            return self.typescript_definition(&document, &uri, line, character);
        }
        if is_rust_path(Path::new(&path)) {
            return self.rust_definition(&document, &uri, line, character);
        }
        if is_dart_path(Path::new(&path)) {
            return self.dart_definition(&document, &uri, line, character);
        }
        self.php_definition(&document, &uri, line, character)
    }

    /// Uses a distinct UniFFI signature from the other position requests.
    /// This prevents an out-of-date Swift object from dispatching Definition
    /// through another `(DocumentHandle, UInt64, UInt64) -> Bool` entrypoint.
    pub fn request_definition_for_navigation(
        &self,
        document: Arc<DocumentHandle>,
        line: u64,
        character: u64,
        navigation_token: String,
    ) -> bool {
        let _ = navigation_token;
        // Navigation must fail closed. UniFFI turns an escaping Rust panic into
        // an error and the generated Swift wrapper uses `try!`, which crashes
        // the editor. External declaration files are untrusted input.
        catch_unwind(AssertUnwindSafe(|| {
            self.request_definition(document.clone(), line, character)
        }))
        .unwrap_or_else(|_| {
            let uri = file_uri(Path::new(&document.path()));
            self.analysis
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .set_definitions(&uri, Vec::new());
            false
        })
    }

    pub fn definition_locations(&self, document: Arc<DocumentHandle>) -> Vec<DefinitionLocation> {
        let path = document.path();
        if path.is_empty() {
            return Vec::new();
        }
        let uri = file_uri(Path::new(&path));
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .definition_locations(&uri)
            .iter()
            .map(|location| DefinitionLocation {
                uri: location.uri.clone(),
                line: location.range.start_line,
                column: location.range.start_character,
            })
            .collect()
    }

    pub fn request_references(
        &self,
        document: Arc<DocumentHandle>,
        line: u64,
        character: u64,
    ) -> bool {
        let path = document.path();
        if path.is_empty() {
            return false;
        }
        let uri = file_uri(Path::new(&path));
        if is_typescript_path(Path::new(&path)) || is_component_path(Path::new(&path)) {
            return self.typescript_references(&document, &uri, line, character);
        }
        if is_dart_path(Path::new(&path)) {
            return self.dart_references(&document, &uri, line, character);
        }
        self.php_references(&document, &uri, line, character)
    }

    pub fn references(&self, document: Arc<DocumentHandle>) -> Vec<DefinitionLocation> {
        let path = document.path();
        if path.is_empty() {
            return Vec::new();
        }
        let uri = file_uri(Path::new(&path));
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .references(&uri)
            .iter()
            .map(|location| DefinitionLocation {
                uri: location.uri.clone(),
                line: location.range.start_line,
                column: location.range.start_character,
            })
            .collect()
    }

    pub fn request_rename(
        &self,
        document: Arc<DocumentHandle>,
        line: u64,
        character: u64,
        new_name: String,
    ) -> bool {
        let path = document.path();
        if path.is_empty() {
            return false;
        }
        let uri = file_uri(Path::new(&path));
        if is_typescript_path(Path::new(&path)) || is_component_path(Path::new(&path)) {
            return self.typescript_rename(&document, &uri, line, character, &new_name);
        }
        if is_dart_path(Path::new(&path)) {
            return self.dart_rename(&document, &uri, line, character, &new_name);
        }
        self.php_rename(&document, &uri, line, character, &new_name)
    }

    pub fn rename_edits(&self, document: Arc<DocumentHandle>) -> Vec<RenameEdit> {
        let path = document.path();
        if path.is_empty() {
            return Vec::new();
        }
        let uri = file_uri(Path::new(&path));
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .rename_edits(&uri)
            .iter()
            .map(|edit| RenameEdit {
                uri: edit.uri.clone(),
                start_line: edit.range.start_line,
                start_character: edit.range.start_character,
                end_line: edit.range.end_line,
                end_character: edit.range.end_character,
                new_text: edit.new_text.clone(),
            })
            .collect()
    }

    pub fn request_code_actions(
        &self,
        document: Arc<DocumentHandle>,
        line: u64,
        character: u64,
    ) -> bool {
        let path = document.path();
        if path.is_empty() {
            return false;
        }
        let path = Path::new(&path);
        if !(is_rust_path(path) || is_go_path(path) || is_php_path(path) || is_blade_path(path)) {
            return false;
        }
        let uri = file_uri(path);
        let engine = if is_rust_path(path) {
            self.rust_engine(path).map(Engine::Rust)
        } else if is_go_path(path) {
            self.go_engine(path).map(Engine::Go)
        } else {
            self.php_engine_any(path).map(Engine::Php)
        };
        if let Some(engine) = engine {
            let text = document.lock().editor.doc.text();
            let found = engine.actions(path, &text, line as u32, character as u32);
            let actions: Vec<galaxy_core::analysis::CodeActionInfo> = found
                .into_iter()
                .map(|action| galaxy_core::analysis::CodeActionInfo {
                    title: action.title,
                    kind: Some("quickfix".into()),
                    edits: action
                        .edits
                        .into_iter()
                        .map(|edit| LspRenameEdit {
                            uri: uri.clone(),
                            range: LspRange {
                                start_line: u64::from(edit.start_line),
                                start_character: u64::from(edit.start_column),
                                end_line: u64::from(edit.end_line),
                                end_character: u64::from(edit.end_column),
                            },
                            new_text: edit.text,
                        })
                        .chain(action.files.into_iter().map(|change| LspRenameEdit {
                            uri: file_uri(&change.path),
                            range: LspRange {
                                start_line: u64::from(change.line),
                                start_character: u64::from(change.column),
                                end_line: u64::from(change.line),
                                end_character: u64::from(change.column),
                            },
                            new_text: change.text,
                        }))
                        .collect(),
                })
                .collect();
            let mut analysis = self.analysis.lock().unwrap_or_else(|e| e.into_inner());
            let answered = !actions.is_empty() || !analysis.code_actions(&uri).is_empty();
            analysis.set_code_actions(&uri, actions);
            return answered;
        }
        false
    }

    pub fn code_actions(&self, document: Arc<DocumentHandle>) -> Vec<CodeActionInfo> {
        let path = document.path();
        if path.is_empty() {
            return Vec::new();
        }
        let uri = file_uri(Path::new(&path));
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .code_actions(&uri)
            .iter()
            .map(|action| CodeActionInfo {
                title: action.title.clone(),
                kind: action.kind.clone(),
                edits: action.edits.iter().map(rename_edit).collect(),
            })
            .collect()
    }

    pub fn request_document_symbols(&self, document: Arc<DocumentHandle>) -> bool {
        let path = document.path();
        if path.is_empty() {
            return false;
        }
        let uri = file_uri(Path::new(&path));
        if is_typescript_path(Path::new(&path)) || is_component_path(Path::new(&path)) {
            let source = component_virtual_source(&path, &document.lock().editor.doc.text());
            let index = DocumentIndex::parse(typescript_index_path(&path), &source);
            let symbols = index
                .declarations
                .iter()
                .map(|declaration| typescript_outline_symbol(declaration, &uri))
                .collect::<Vec<_>>();
            let answered = !symbols.is_empty();
            self.analysis
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .set_document_symbols(&uri, symbols);
            return answered;
        }
        if is_dart_path(Path::new(&path)) {
            let text = document.lock().editor.doc.text();
            let symbols: Vec<galaxy_core::analysis::DocumentSymbolInfo> = self
                .dart_engine(Path::new(&path))
                .map(|engine| {
                    let engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                    if engine.is_ready() {
                        engine.outline(Path::new(&path), &text)
                    } else {
                        Default::default()
                    }
                })
                .unwrap_or_default()
                .iter()
                .map(|symbol| php_outline_symbol(symbol, &uri))
                .collect();
            let answered = !symbols.is_empty();
            self.analysis
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .set_document_symbols(&uri, symbols);
            return answered;
        }
        let engine = if is_rust_path(Path::new(&path)) {
            self.rust_engine(Path::new(&path)).map(Engine::Rust)
        } else if is_go_path(Path::new(&path)) {
            self.go_engine(Path::new(&path)).map(Engine::Go)
        } else {
            self.php_engine(Path::new(&path)).map(Engine::Php)
        };
        if let Some(engine) = engine {
            let text = document.lock().editor.doc.text();
            if engine.is_ready() {
                let symbols: Vec<_> = engine
                    .outline(Path::new(&path), &text)
                    .iter()
                    .map(|symbol| php_outline_symbol(symbol, &uri))
                    .collect();
                let answered = !symbols.is_empty();
                self.analysis
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .set_document_symbols(&uri, symbols);
                return answered;
            }
        }
        false
    }

    pub fn document_symbols(&self, document: Arc<DocumentHandle>) -> Vec<LspOutlineSymbol> {
        let path = document.path();
        if path.is_empty() {
            return Vec::new();
        }
        let uri = file_uri(Path::new(&path));
        fn flatten(
            source: &[galaxy_core::analysis::DocumentSymbolInfo],
            output: &mut Vec<LspOutlineSymbol>,
            depth: u64,
        ) {
            for symbol in source {
                output.push(LspOutlineSymbol {
                    uri: symbol.uri.clone(),
                    name: symbol.name.clone(),
                    kind: symbol.kind.to_string(),
                    line: symbol.range.start_line,
                    column: symbol.range.start_character,
                    depth,
                });
                flatten(&symbol.children, output, depth + 1);
            }
        }
        let mut output = Vec::new();
        let analysis = self.analysis.lock().unwrap_or_else(|e| e.into_inner());
        flatten(analysis.document_symbols(&uri), &mut output, 0);
        output
    }

    pub fn problems(&self) -> Vec<ProblemInfo> {
        let analysis = self.analysis.lock().unwrap_or_else(|e| e.into_inner());
        analysis
            .problems()
            .into_iter()
            .map(|diagnostic| ProblemInfo {
                uri: diagnostic.uri.clone(),
                line: diagnostic.range.start_line,
                column: diagnostic.range.start_character,
                severity: match diagnostic.severity {
                    galaxy_core::analysis::DiagnosticSeverity::Error => 1,
                    galaxy_core::analysis::DiagnosticSeverity::Warning => 2,
                    galaxy_core::analysis::DiagnosticSeverity::Information => 3,
                    galaxy_core::analysis::DiagnosticSeverity::Hint => 4,
                },
                message: diagnostic.message.clone(),
                source: diagnostic.source.clone(),
                code: diagnostic.code.clone(),
            })
            .collect()
    }
}

/// A watcher path that can affect Composer class resolution.
fn php_index_input_changed(path: &str) -> bool {
    let path = Path::new(path);
    is_php_path(path)
        || path
            .components()
            .next()
            .is_some_and(|component| component.as_os_str() == "vendor")
        || workspace_index_metadata(path)
}

fn file_uri(path: &Path) -> String {
    format!("file://{}", path.to_string_lossy())
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ProblemInfo {
    pub uri: String,
    pub line: u64,
    pub column: u64,
    pub severity: u8,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct HoverInfo {
    pub contents: String,
    pub start_line: Option<u64>,
    pub start_character: Option<u64>,
    pub end_line: Option<u64>,
    pub end_character: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CompletionInfo {
    pub label: String,
    pub detail: Option<String>,
    pub insert_text: String,
    pub is_snippet: bool,
    /// Inserted elsewhere in the file together with the item, e.g. a PHP
    /// `use` import. Positions refer to the text before the completion.
    #[uniffi(default = [])]
    pub additional_edits: Vec<TextEditInfo>,
    /// Column where the text to replace starts, when more than the word
    /// before the cursor is replaced (`app.na` in `config('app.na')`).
    #[uniffi(default = None)]
    pub replace_from: Option<u64>,
    /// LSP `CompletionItemKind`: 2 method, 3 function, 5 field, 6 variable,
    /// 7 class, 8 interface, 10 property, 13 enum, 14 keyword, 15 snippet,
    /// 21 constant, 22 struct.
    #[uniffi(default = None)]
    pub kind: Option<u64>,
}

/// Text inserted at a line and UTF-16 column.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TextEditInfo {
    pub line: u64,
    pub column: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SnippetExpansionInfo {
    pub text: String,
    pub stops: Vec<SnippetStopInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SnippetStopInfo {
    pub index: u32,
    pub range: Utf16Range,
}

#[uniffi::export]
pub fn expand_snippet(
    body: String,
    base_indent: String,
    indent_unit: String,
    filename: String,
    selected_text: String,
    current_year: String,
) -> SnippetExpansionInfo {
    let context = snippets::ExpansionContext {
        filename,
        selected_text,
        current_year,
    };
    let expansion = snippets::expand_with_context(&body, &base_indent, &indent_unit, &context);
    let stops = expansion
        .stops
        .iter()
        .map(|stop| {
            let prefix: String = expansion.text.chars().take(stop.start).collect();
            let text: String = expansion
                .text
                .chars()
                .skip(stop.start)
                .take(stop.end.saturating_sub(stop.start))
                .collect();
            SnippetStopInfo {
                index: stop.index,
                range: Utf16Range {
                    location: prefix.encode_utf16().count() as u64,
                    length: text.encode_utf16().count() as u64,
                },
            }
        })
        .collect();
    SnippetExpansionInfo {
        text: expansion.text,
        stops,
    }
}

/// A parameter name drawn before the argument at `line`/`column` (UTF-16).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ParameterHintInfo {
    pub line: u64,
    pub column: u64,
    pub label: String,
    /// An inferred return type that a double-click can insert.
    pub return_type: bool,
}

/// The hints of one line, with the displayed text they were computed for.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ParameterHintLine {
    pub line: u64,
    pub text: String,
    pub hints: Vec<ParameterHintInfo>,
}

/// A labelled argument at UTF-16 `column` that needs `columns` monospace
/// columns of room for its label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct WrapHintSlot {
    pub column: u64,
    pub columns: u64,
}

/// The label room of one line, keyed by its displayed text.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct WrapHintLine {
    pub text: String,
    pub slots: Vec<WrapHintSlot>,
}

/// Parameter hints for a range of lines at document `revision`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ParameterHints {
    pub revision: u64,
    pub lines: Vec<ParameterHintLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SignatureInfo {
    pub label: String,
    pub documentation: Option<String>,
    pub parameters: Vec<String>,
    pub active_parameter: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DefinitionLocation {
    pub uri: String,
    pub line: u64,
    pub column: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RenameEdit {
    pub uri: String,
    pub start_line: u64,
    pub start_character: u64,
    pub end_line: u64,
    pub end_character: u64,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CodeActionInfo {
    pub title: String,
    pub kind: Option<String>,
    pub edits: Vec<RenameEdit>,
}

fn rename_edit(edit: &galaxy_core::analysis::RenameEdit) -> RenameEdit {
    RenameEdit {
        uri: edit.uri.clone(),
        start_line: edit.range.start_line,
        start_character: edit.range.start_character,
        end_line: edit.range.end_line,
        end_character: edit.range.end_character,
        new_text: edit.new_text.clone(),
    }
}

/// A built-in engine serving one open file, whatever the language.
enum Engine {
    Php(Arc<Mutex<PhpEngine>>),
    Go(Arc<Mutex<GoEngine>>),
    Rust(Arc<Mutex<RustEngine>>),
}

impl Engine {
    fn is_ready(&self) -> bool {
        match self {
            Engine::Php(engine) => engine.lock().unwrap_or_else(|e| e.into_inner()).is_ready(),
            Engine::Go(engine) => engine.lock().unwrap_or_else(|e| e.into_inner()).is_ready(),
            Engine::Rust(engine) => engine.lock().unwrap_or_else(|e| e.into_inner()).is_ready(),
        }
    }

    fn hover(&self, path: &Path, text: &str, line: u32, column: u32) -> Option<HoverResult> {
        match self {
            Engine::Php(engine) => {
                let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                engine
                    .is_ready()
                    .then(|| engine.hover(path, text, line, column))
                    .flatten()
            }
            Engine::Go(engine) => {
                let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                engine
                    .is_ready()
                    .then(|| engine.hover(path, text, line, column))
                    .flatten()
            }
            Engine::Rust(engine) => {
                let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                // Rust standard-library types and members resolve directly
                // from rust-src and must work while workspace indexing runs.
                engine.hover(path, text, line, column)
            }
        }
    }

    fn complete(&self, path: &Path, text: &str, line: u32, column: u32) -> Vec<Completion> {
        match self {
            Engine::Php(engine) => {
                let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                if engine.is_ready() {
                    engine.complete(path, text, line, column)
                } else {
                    Vec::new()
                }
            }
            Engine::Go(engine) => {
                let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                if engine.is_ready() {
                    engine.complete(path, text, line, column)
                } else {
                    Vec::new()
                }
            }
            Engine::Rust(engine) => {
                let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                if engine.is_ready() {
                    engine.complete(path, text, line, column)
                } else {
                    Default::default()
                }
            }
        }
    }

    fn signature(&self, path: &Path, text: &str, line: u32, column: u32) -> Option<Signature> {
        match self {
            Engine::Php(engine) => {
                let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                engine
                    .is_ready()
                    .then(|| engine.signature(path, text, line, column))
                    .flatten()
            }
            Engine::Go(engine) => {
                let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                engine
                    .is_ready()
                    .then(|| engine.signature(path, text, line, column))
                    .flatten()
            }
            Engine::Rust(engine) => {
                let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                engine
                    .is_ready()
                    .then(|| engine.signature(path, text, line, column))
                    .flatten()
            }
        }
    }

    fn definition(&self, path: &Path, text: &str, line: u32, column: u32) -> Vec<Target> {
        match self {
            Engine::Php(engine) => {
                let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                if engine.is_ready() {
                    engine.definition(path, text, line, column)
                } else {
                    Vec::new()
                }
            }
            Engine::Go(engine) => {
                let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                if engine.is_ready() {
                    engine.definition(path, text, line, column)
                } else {
                    Vec::new()
                }
            }
            Engine::Rust(engine) => {
                let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                engine.definition(path, text, line, column)
            }
        }
    }

    fn actions(&self, path: &Path, text: &str, line: u32, column: u32) -> Vec<Action> {
        match self {
            Engine::Php(engine) => {
                let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                if engine.is_ready() {
                    engine.actions(path, text, line, column)
                } else {
                    Vec::new()
                }
            }
            Engine::Go(engine) => {
                let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                if engine.is_ready() {
                    engine.actions(path, text, line, column)
                } else {
                    Vec::new()
                }
            }
            Engine::Rust(engine) => {
                let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                if engine.is_ready() {
                    engine.actions(path, text, line, column)
                } else {
                    Vec::new()
                }
            }
        }
    }

    fn diagnostics(&self, path: &Path, text: &str) -> Vec<Problem> {
        match self {
            Engine::Php(engine) => engine
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .diagnostics(path, text),
            Engine::Go(engine) => engine
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .diagnostics(path, text),
            Engine::Rust(engine) => engine
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .diagnostics(path, text),
        }
    }

    fn outline(&self, path: &Path, text: &str) -> Vec<LangOutlineSymbol> {
        match self {
            Engine::Php(_) => php::outline(path, text),
            Engine::Go(_) => go::outline(path, text),
            Engine::Rust(engine) => engine
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .outline(path, text),
        }
    }
}

impl Core {
    /// The built-in Go engine for the project holding `path`, started on
    /// first use; its index is built on a worker thread.
    fn go_engine(&self, path: &Path) -> Option<Arc<Mutex<GoEngine>>> {
        if !is_go_path(path) {
            return None;
        }
        let root = go::project_root(path);
        let (engine, fresh) = {
            let mut engines = self.go.lock().unwrap_or_else(|e| e.into_inner());
            match engines.get(&root) {
                Some(engine) => (engine.clone(), false),
                None => {
                    let engine = Arc::new(Mutex::new(GoEngine::pending(&root)));
                    engines.insert(root.clone(), engine.clone());
                    (engine, true)
                }
            }
        };
        let stale = engine
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .wants_index(Duration::from_secs(120));
        if fresh || stale {
            spawn_go_index(&engine, &root);
        }
        Some(engine)
    }

    fn rust_engine(&self, path: &Path) -> Option<Arc<Mutex<RustEngine>>> {
        if !is_rust_path(path) {
            return None;
        }
        let root = rust::project_root(path);
        let (engine, fresh) = {
            let mut engines = self.rust.lock().unwrap_or_else(|e| e.into_inner());
            match engines.get(&root) {
                Some(engine) => (engine.clone(), false),
                None => {
                    let engine = Arc::new(Mutex::new(RustEngine::pending(&root)));
                    engines.insert(root.clone(), engine.clone());
                    (engine, true)
                }
            }
        };
        if fresh {
            spawn_rust_index(&engine, &root);
        }
        Some(engine)
    }

    fn dart_engine(&self, path: &Path) -> Option<Arc<Mutex<DartEngine>>> {
        if !is_dart_path(path) {
            return None;
        }
        let root = dart::project_root(path);
        let (engine, fresh) = {
            let mut engines = self.dart.lock().unwrap_or_else(|e| e.into_inner());
            match engines.get(&root) {
                Some(engine) => (engine.clone(), false),
                None => {
                    let engine = Arc::new(Mutex::new(DartEngine::pending(&root)));
                    engines.insert(root.clone(), engine.clone());
                    (engine, true)
                }
            }
        };
        if fresh {
            spawn_dart_index(&engine, &root);
        }
        Some(engine)
    }

    fn release_rust_buffer(&self, uri: &str) {
        let Some(path) = uri.strip_prefix("file://") else {
            return;
        };
        let path = Path::new(path);
        if !is_rust_path(path) {
            return;
        }
        for engine in self.rust.lock().unwrap_or_else(|e| e.into_inner()).values() {
            engine
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .close_document(path);
        }
    }

    fn check_rust(
        &self,
        analysis: &mut BuiltinStore,
        documents: &HashMap<String, Arc<DocumentHandle>>,
    ) {
        for (uri, document) in documents {
            let path = document.path();
            if !is_rust_path(Path::new(&path)) {
                continue;
            }
            let revision = document.revision();
            let checked_revision = self
                .rust_checked
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(uri)
                .copied();
            if checked_revision == Some(revision) {
                continue;
            }
            if checked_revision.is_some() {
                analysis.clear_compiler_diagnostics(uri);
            }
            let Some(engine) = self.rust_engine(Path::new(&path)) else {
                continue;
            };
            let text = document.lock().editor.doc.text();
            let diagnostics = engine
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .diagnostics(Path::new(&path), &text)
                .into_iter()
                .map(|problem| galaxy_core::analysis::Diagnostic {
                    uri: uri.clone(),
                    range: LspRange {
                        start_line: u64::from(problem.start_line),
                        start_character: u64::from(problem.start_column),
                        end_line: u64::from(problem.end_line),
                        end_character: u64::from(problem.end_column),
                    },
                    severity: galaxy_core::analysis::DiagnosticSeverity::Error,
                    message: problem.message,
                    source: Some("galaxy".into()),
                    code: None,
                    unnecessary: false,
                })
                .collect();
            analysis.set_diagnostics(uri, diagnostics);
            self.rust_checked
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(uri.clone(), revision);
        }
    }

    fn check_dart(
        &self,
        analysis: &mut BuiltinStore,
        documents: &HashMap<String, Arc<DocumentHandle>>,
    ) {
        for (uri, document) in documents {
            let path = document.path();
            if !is_dart_path(Path::new(&path)) {
                continue;
            }
            let revision = document.revision();
            if self
                .dart_checked
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(uri)
                .is_some_and(|checked| *checked == revision)
            {
                continue;
            }
            let Some(engine) = self.dart_engine(Path::new(&path)) else {
                continue;
            };
            let text = document.lock().editor.doc.text();
            let diagnostics = engine
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .diagnostics(Path::new(&path), &text)
                .into_iter()
                .map(|problem| galaxy_core::analysis::Diagnostic {
                    uri: uri.clone(),
                    range: LspRange {
                        start_line: u64::from(problem.start_line),
                        start_character: u64::from(problem.start_column),
                        end_line: u64::from(problem.end_line),
                        end_character: u64::from(problem.end_column),
                    },
                    severity: match problem.severity {
                        galaxy_core::lang::Severity::Error => {
                            galaxy_core::analysis::DiagnosticSeverity::Error
                        }
                        galaxy_core::lang::Severity::Warning => {
                            galaxy_core::analysis::DiagnosticSeverity::Warning
                        }
                        galaxy_core::lang::Severity::Hint => {
                            galaxy_core::analysis::DiagnosticSeverity::Hint
                        }
                    },
                    message: problem.message,
                    source: Some("galaxy".into()),
                    code: None,
                    unnecessary: problem.unnecessary,
                })
                .collect();
            analysis.set_diagnostics(uri, diagnostics);
            self.dart_checked
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(uri.clone(), revision);
        }
    }

    /// A closed Go document no longer overrides the file on disk.
    fn release_go_buffer(&self, uri: &str) {
        let Some(path) = uri.strip_prefix("file://") else {
            return;
        };
        let path = Path::new(path);
        if !is_go_path(path) {
            return;
        }
        let engines: Vec<_> = self
            .go
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for engine in engines {
            engine
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .close_document(path);
        }
    }

    /// Uses of the Go symbol at the cursor across the project.
    fn go_usages(
        &self,
        document: &Arc<DocumentHandle>,
        line: u64,
        character: u64,
    ) -> Option<Vec<galaxy_core::lang::Occurrence>> {
        let path = document.path();
        let engine = self.go_engine(Path::new(&path))?;
        let text = document.lock().editor.doc.text();
        let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
        engine
            .is_ready()
            .then(|| engine.references(Path::new(&path), &text, line as u32, character as u32))
    }

    fn rust_usages(
        &self,
        document: &Arc<DocumentHandle>,
        line: u64,
        character: u64,
    ) -> Option<Vec<galaxy_core::lang::Occurrence>> {
        let path = document.path();
        let engine = self.rust_engine(Path::new(&path))?;
        let text = document.lock().editor.doc.text();
        let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
        engine
            .is_ready()
            .then(|| engine.references(Path::new(&path), &text, line as u32, character as u32))
    }

    fn rust_rename(
        &self,
        document: &Arc<DocumentHandle>,
        uri: &str,
        line: u64,
        character: u64,
        new_name: &str,
    ) -> bool {
        let path = document.path();
        let edits = self
            .rust_engine(Path::new(&path))
            .and_then(|engine| {
                let text = document.lock().editor.doc.text();
                let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                engine.is_ready().then(|| {
                    engine
                        .rename(
                            Path::new(&path),
                            &text,
                            line as u32,
                            character as u32,
                            new_name,
                        )
                        .ok()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|usage| LspRenameEdit {
                            uri: file_uri(&usage.path),
                            range: LspRange {
                                start_line: u64::from(usage.line),
                                start_character: u64::from(usage.start_column),
                                end_line: u64::from(usage.line),
                                end_character: u64::from(usage.end_column),
                            },
                            new_text: new_name.to_owned(),
                        })
                        .collect::<Vec<_>>()
                })
            })
            .unwrap_or_default();
        let found = !edits.is_empty();
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_rename_edits(uri, edits);
        found
    }

    /// Renames the Go symbol at the cursor; `false` when the name is not
    /// valid, the symbol is external or unknown.
    fn go_rename(
        &self,
        document: &Arc<DocumentHandle>,
        uri: &str,
        line: u64,
        character: u64,
        new_name: &str,
    ) -> bool {
        let path = document.path();
        let mut edits = Vec::new();
        if let Some(engine) = self.go_engine(Path::new(&path)) {
            let text = document.lock().editor.doc.text();
            let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
            if engine.is_ready()
                && let Ok(found) = engine.rename(
                    Path::new(&path),
                    &text,
                    line as u32,
                    character as u32,
                    new_name.trim(),
                )
            {
                edits = go::rename_edits(&found, new_name.trim())
                    .into_iter()
                    .map(|(path, line, start, end, new_text)| LspRenameEdit {
                        uri: file_uri(&path),
                        range: LspRange {
                            start_line: u64::from(line),
                            start_character: u64::from(start),
                            end_line: u64::from(line),
                            end_character: u64::from(end),
                        },
                        new_text,
                    })
                    .collect();
            }
        }
        let found = !edits.is_empty();
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_rename_edits(uri, edits);
        found
    }

    /// The built-in PHP engine for the project holding `path`, started on
    /// first use. Its index is built on a worker thread and rebuilt when
    /// it grows old, so no request ever walks the file system.
    fn php_engine(&self, path: &Path) -> Option<Arc<Mutex<PhpEngine>>> {
        if !is_php_path(path) {
            return None;
        }
        self.engine_for(path)
    }

    /// Like `php_engine`, and also for Blade templates. Those only get the
    /// Laravel features (names, views, components), never PHP parsing.
    fn php_engine_any(&self, path: &Path) -> Option<Arc<Mutex<PhpEngine>>> {
        if !is_php_path(path) && !is_blade_path(path) {
            return None;
        }
        self.engine_for(path)
    }

    fn engine_for(&self, path: &Path) -> Option<Arc<Mutex<PhpEngine>>> {
        let root = php::project_root(path);
        let (engine, fresh) = {
            let mut engines = self.php.lock().unwrap_or_else(|e| e.into_inner());
            match engines.get(&root) {
                Some(engine) => (engine.clone(), false),
                None => {
                    let engine = Arc::new(Mutex::new(PhpEngine::pending(&root)));
                    engines.insert(root.clone(), engine.clone());
                    (engine, true)
                }
            }
        };
        let stale = engine
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .wants_index(Duration::from_secs(60));
        if fresh || stale {
            spawn_php_index(&engine, &root);
        }
        Some(engine)
    }

    /// Runs the built-in PHP analysis on open files whose text has settled,
    /// so a file is not re-read on every keystroke.
    fn check_php(
        &self,
        analysis: &mut BuiltinStore,
        documents: &HashMap<String, Arc<DocumentHandle>>,
    ) {
        const SETTLE: Duration = Duration::from_millis(300);
        let now = Instant::now();
        let mut checked = self.php_checked.lock().unwrap_or_else(|e| e.into_inner());
        checked.retain(|uri, _| documents.contains_key(uri));
        for (uri, document) in documents {
            let path = document.path();
            if path.is_empty()
                || !(is_php_path(Path::new(&path))
                    || is_blade_path(Path::new(&path))
                    || is_go_path(Path::new(&path)))
            {
                continue;
            }
            let revision = document.revision();
            let entry = checked.entry(uri.clone()).or_insert((u64::MAX, now, true));
            if entry.0 != revision {
                *entry = (revision, now, false);
                continue;
            }
            if entry.2 || now.duration_since(entry.1) < SETTLE {
                continue;
            }
            let engine = if is_go_path(Path::new(&path)) {
                self.go_engine(Path::new(&path)).map(Engine::Go)
            } else {
                self.php_engine_any(Path::new(&path)).map(Engine::Php)
            };
            let Some(engine) = engine else {
                continue;
            };
            if !engine.is_ready() {
                // Try again on the next poll, once the index is built.
                continue;
            }
            let text = document.lock().editor.doc.text();
            let problems = engine.diagnostics(Path::new(&path), &text);
            entry.2 = true;
            let diagnostics = problems
                .into_iter()
                .map(|problem| galaxy_core::analysis::Diagnostic {
                    uri: uri.clone(),
                    range: LspRange {
                        start_line: u64::from(problem.start_line),
                        start_character: u64::from(problem.start_column),
                        end_line: u64::from(problem.end_line),
                        end_character: u64::from(problem.end_column),
                    },
                    severity: match problem.severity {
                        php::Severity::Error => galaxy_core::analysis::DiagnosticSeverity::Error,
                        php::Severity::Warning => {
                            galaxy_core::analysis::DiagnosticSeverity::Warning
                        }
                        php::Severity::Hint => galaxy_core::analysis::DiagnosticSeverity::Hint,
                    },
                    message: problem.message,
                    source: Some("galaxy".into()),
                    code: None,
                    unnecessary: problem.unnecessary,
                })
                .collect();
            analysis.set_diagnostics(uri, diagnostics);
        }
    }

    fn check_typescript(
        &self,
        analysis: &mut BuiltinStore,
        documents: &HashMap<String, Arc<DocumentHandle>>,
    ) {
        let mut checked = self
            .typescript_checked
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Runs on the main thread from `poll_analysis`. A references scan can
        // hold the workspace index for seconds; skip this tick instead of
        // freezing the UI. Unchecked documents are retried on the next poll.
        let exports = match self.typescript_workspace_exports.try_lock() {
            Ok(exports) => exports,
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return,
        };
        {
            let mut generation = self
                .typescript_checked_exports
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if *generation != exports.generation() {
                *generation = exports.generation();
                checked.clear();
            }
        }
        for (uri, document) in documents {
            let path = document.path();
            let component = is_component_path(Path::new(&path));
            if !(is_typescript_path(Path::new(&path)) || component) {
                continue;
            }
            let (revision, text) = {
                let state = document.lock();
                (state.editor.doc.revision(), state.editor.doc.text())
            };
            if checked.get(uri) == Some(&revision) {
                continue;
            }
            let source = component_virtual_source(&path, &text);
            let index = DocumentIndex::parse(typescript_index_path(&path), &source);
            let mut diagnostics: Vec<_> = index
                .diagnostics
                .iter()
                .map(|diagnostic| galaxy_core::analysis::Diagnostic {
                    uri: uri.clone(),
                    range: LspRange {
                        start_line: diagnostic.range.start.line as u64,
                        start_character: diagnostic.range.start.col as u64,
                        end_line: diagnostic.range.end.line as u64,
                        end_character: diagnostic.range.end.col as u64,
                    },
                    severity: galaxy_core::analysis::DiagnosticSeverity::Error,
                    message: diagnostic.message.clone(),
                    source: Some("galaxy".into()),
                    code: None,
                    unnecessary: false,
                })
                .collect();
            diagnostics.extend(index.type_diagnostics.iter().map(|diagnostic| {
                galaxy_core::analysis::Diagnostic {
                    uri: uri.clone(),
                    range: typescript_range(diagnostic.range),
                    severity: galaxy_core::analysis::DiagnosticSeverity::Error,
                    message: diagnostic.message.clone(),
                    source: Some("galaxy".into()),
                    code: Some(diagnostic.code.clone()),
                    unnecessary: false,
                }
            }));
            diagnostics.extend(
                index
                    .imports
                    .iter()
                    .filter(|import| {
                        !typescript_import_is_type_only(import)
                            // Bare package names and tsconfig aliases need a
                            // package/config resolver. Do not report them as
                            // broken merely because local path resolution
                            // cannot prove them.
                            && (import.specifier.starts_with('.') || import.specifier.starts_with('/'))
                            && resolve_typescript_import_uncached(
                                Path::new(&path),
                                &import.specifier,
                            )
                            .is_none()
                    })
                    .map(|import| galaxy_core::analysis::Diagnostic {
                        uri: uri.clone(),
                        range: typescript_range(import.range),
                        severity: galaxy_core::analysis::DiagnosticSeverity::Warning,
                        message: format!("Cannot resolve import '{}'", import.specifier),
                        source: Some("galaxy".into()),
                        code: None,
                        unnecessary: false,
                    }),
            );
            // A name some workspace module exports gets an import suggestion.
            // Any other unknown name is reported only where the project's
            // globals are fully known (see `typescript_globals`). Components
            // are skipped: their build tools may auto-import names
            // (composables, runes) the engine cannot see, and a false report
            // is worse than none.
            let names = index
                .unresolved_references
                .iter()
                .filter(|_| !component)
                .map(|(name, _)| name.as_str())
                .collect::<std::collections::BTreeSet<_>>();
            let suggestions = exports.import_suggestions(Path::new(&path), &names);
            let reports_unknown_names = !component
                && typescript_reports_unknown_names(&path)
                && index.diagnostics.is_empty();
            let stdlib = StdLib::for_file(Path::new(&path));
            diagnostics.extend(index.unresolved_references.iter().filter_map(|(name, range)| {
                // TypeScript's standard library provides these names. Do not
                // mistake an unrelated workspace export for a missing import.
                if stdlib.has_global(name) {
                    return None;
                }
                if let Some(specifier) = suggestions.get(name) {
                    return Some(galaxy_core::analysis::Diagnostic {
                        uri: uri.clone(),
                        range: typescript_range(*range),
                        severity: galaxy_core::analysis::DiagnosticSeverity::Warning,
                        message: format!(
                            "Cannot find name '{name}'. Did you forget to import it from '{specifier}'?"
                        ),
                        source: Some("galaxy".into()),
                        code: Some("TS2304".into()),
                        unnecessary: false,
                    });
                }
                if !reports_unknown_names
                    || name.starts_with('$')
                    || TYPESCRIPT_IMPLICIT_NAMES.contains(&name.as_str())
                {
                    return None;
                }
                exports
                    .is_unknown_global(Path::new(&path), name)
                    .filter(|unknown| *unknown)?;
                Some(galaxy_core::analysis::Diagnostic {
                    uri: uri.clone(),
                    range: typescript_range(*range),
                    severity: galaxy_core::analysis::DiagnosticSeverity::Error,
                    message: format!("Cannot find name '{name}'."),
                    source: Some("galaxy".into()),
                    code: Some("TS2304".into()),
                    unnecessary: false,
                })
            }));
            analysis.set_diagnostics(uri, diagnostics);
            analysis.set_document_symbols(
                uri,
                index
                    .declarations
                    .iter()
                    .map(|declaration| typescript_outline_symbol(declaration, uri))
                    .collect(),
            );
            checked.insert(uri.clone(), revision);
        }
    }

    /// Uses of the PHP symbol at the cursor across the project, read from
    /// open buffers where a file is open. `None` means the PHP engine has no
    /// answer for this position.
    fn php_usages(
        &self,
        document: &Arc<DocumentHandle>,
        line: u64,
        character: u64,
    ) -> Option<(php::RefQuery, Vec<php::Occurrence>)> {
        let path = document.path();
        let engine = self.php_engine(Path::new(&path))?;
        let text = document.lock().editor.doc.text();
        let query = {
            let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
            if !engine.is_ready() {
                return None;
            }
            engine.reference_query(Path::new(&path), &text, line as u32, character as u32)?
        };
        // Reading the project's files happens outside the engine's lock.
        let mut files = query.candidates();
        {
            let open = self.php_documents.lock().unwrap_or_else(|e| e.into_inner());
            for (other, body) in &mut files {
                if let Some(handle) = open
                    .get(&file_uri(other))
                    .and_then(std::sync::Weak::upgrade)
                {
                    *body = handle.lock().editor.doc.text();
                }
            }
        }
        let mut found = Vec::new();
        {
            let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
            found.extend(engine.scan_references(&query, Path::new(&path), &text));
            for (other, body) in &files {
                if other.as_path() != Path::new(&path) {
                    found.extend(engine.scan_references(&query, other, body));
                }
            }
        }
        Some((query, php::finish(found)))
    }

    fn php_references(
        &self,
        document: &Arc<DocumentHandle>,
        uri: &str,
        line: u64,
        character: u64,
    ) -> bool {
        let usages = if is_rust_path(Path::new(&document.path())) {
            self.rust_usages(document, line, character)
        } else if is_go_path(Path::new(&document.path())) {
            self.go_usages(document, line, character)
        } else {
            self.php_usages(document, line, character)
                .map(|(_, found)| found)
        };
        let locations: Vec<LspDefinitionLocation> = usages
            .unwrap_or_default()
            .into_iter()
            .map(|usage| LspDefinitionLocation {
                uri: file_uri(&usage.path),
                range: LspRange {
                    start_line: u64::from(usage.line),
                    start_character: u64::from(usage.start_column),
                    end_line: u64::from(usage.line),
                    end_character: u64::from(usage.end_column),
                },
            })
            .collect();
        let found = !locations.is_empty();
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_references(uri, locations);
        found
    }

    fn typescript_definition(
        &self,
        document: &Arc<DocumentHandle>,
        uri: &str,
        line: u64,
        character: u64,
    ) -> bool {
        let path = document.path();
        let text = document.lock().editor.doc.text();
        // `route('couriers.update')` names a Laravel route, in a script or in
        // a template expression alike.
        if let Some(location) =
            laravel_route_definition(Path::new(&path), &text, line as usize, character as usize)
        {
            self.analysis
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .set_definitions(uri, vec![location]);
            return true;
        }
        let Some(source) = component_position_source(&path, &text, line as usize) else {
            let location = self
                .component_template_definition(&path, &text, uri, line as usize, character as usize)
                .or_else(|| {
                    self.ziggy_route_definition(&path, &text, line as usize, character as usize)
                });
            let found = location.is_some();
            self.analysis
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .set_definitions(uri, location.into_iter().collect());
            return found;
        };
        // Component scripts retain their original line and column offsets, so
        // TypeScript locations can be returned directly to the editor.
        let index = DocumentIndex::parse(typescript_index_path(&path), &source);
        let locations = self.typescript_targets(&path, &text, uri, &index, line, character);
        let found = !locations.is_empty();
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_definitions(uri, locations);
        found
    }

    /// Resolves each call's callee like Go to Definition, then reads the
    /// parameters at the target. Vue and Svelte templates are not analysed.
    fn typescript_parameter_hints(
        &self,
        path: &str,
        text: &str,
        first: u32,
        last: u32,
    ) -> Vec<parameter_hints::ParameterHint> {
        use galaxy_core::typescript::calls;
        let source = component_virtual_source(path, text);
        let index_path = typescript_index_path(path);
        let calls = calls::call_sites(
            Path::new(&index_path),
            &source,
            first as usize,
            last as usize,
        );
        let variables = calls::variable_sites(
            Path::new(&index_path),
            &source,
            first as usize,
            last as usize,
        );
        if calls.is_empty() && variables.is_empty() {
            return Vec::new();
        }
        let uri = file_uri(Path::new(path));
        let index = DocumentIndex::parse(&index_path, &source);
        // Target files read once per request: (parse path, source).
        let mut targets: HashMap<String, Option<(String, String)>> = HashMap::new();
        let mut out = Vec::new();
        for site in variables {
            let label = match site.source {
                calls::VariableType::Literal(name) => name.to_owned(),
                calls::VariableType::Call {
                    line,
                    column,
                    awaited,
                } => {
                    let locations = self.typescript_targets(
                        path,
                        text,
                        &uri,
                        &index,
                        line as u64,
                        column as u64,
                    );
                    // A name with several definitions has no single type.
                    let [location] = locations.as_slice() else {
                        continue;
                    };
                    let Some((target_path, target_source)) = Self::typescript_target_source(
                        &mut targets,
                        location,
                        &uri,
                        &index_path,
                        &source,
                    ) else {
                        continue;
                    };
                    let Some(written) = calls::return_type_at(
                        Path::new(target_path.as_str()),
                        target_source,
                        location.range.start_line as usize,
                        location.range.start_character as usize,
                        awaited,
                    ) else {
                        continue;
                    };
                    written
                }
            };
            out.push(parameter_hints::ParameterHint {
                line: site.line as u32,
                column: site.column as u32,
                label: format!(": {label}"),
                return_type: false,
            });
        }
        for call in calls.into_iter().take(parameter_hints::MAX_CALLS) {
            let locations = self.typescript_targets(
                path,
                text,
                &uri,
                &index,
                call.callee_line as u64,
                call.callee_column as u64,
            );
            let Some(location) = locations.first() else {
                continue;
            };
            let Some((target_path, target_source)) =
                Self::typescript_target_source(&mut targets, location, &uri, &index_path, &source)
            else {
                continue;
            };
            if let Some(parameters) = calls::parameters_at(
                Path::new(target_path.as_str()),
                target_source,
                location.range.start_line as usize,
                location.range.start_character as usize,
            ) {
                out.extend(parameter_hints::hints(&parameters, &call.arguments));
            }
        }
        out.sort_by_key(|hint| (hint.line, hint.column));
        out.dedup_by_key(|hint| (hint.line, hint.column));
        out
    }

    /// Parse path and script source of the file holding `location`, read once
    /// per request into `targets`.
    fn typescript_target_source<'a>(
        targets: &'a mut HashMap<String, Option<(String, String)>>,
        location: &LspDefinitionLocation,
        uri: &str,
        index_path: &str,
        source: &str,
    ) -> Option<&'a (String, String)> {
        targets
            .entry(location.uri.clone())
            .or_insert_with(|| {
                if location.uri == uri {
                    return Some((index_path.to_owned(), source.to_owned()));
                }
                let target = location.uri.strip_prefix("file://")?;
                let target_text = std::fs::read_to_string(target).ok()?;
                Some((
                    typescript_index_path(target),
                    component_virtual_source(target, &target_text),
                ))
            })
            .as_ref()
    }

    /// Go to Definition targets of the script position `line`/`character`,
    /// given the document's already parsed `index`.
    fn typescript_targets(
        &self,
        path: &str,
        text: &str,
        uri: &str,
        index: &DocumentIndex,
        line: u64,
        character: u64,
    ) -> Vec<LspDefinitionLocation> {
        let mut locations = typescript_type_target(
            &self.typescript_navigation,
            path,
            index,
            line as usize,
            character as usize,
        )
        .map(|target| vec![target])
        .unwrap_or_default();
        if locations.is_empty() {
            locations = typescript_import_target(
                &self.typescript_navigation,
                path,
                index,
                line as usize,
                character as usize,
            )
            .map(|(target, _)| vec![target])
            .unwrap_or_default();
        }
        if locations.is_empty()
            && let Some(location) = ecmascript_primitive_type_definition(
                Path::new(path),
                text,
                line as usize,
                character as usize,
            )
        {
            locations.push(location);
        }
        // An imported type/superclass reference (e.g. `extends Macroable`) must
        // follow the package export before falling back to the local import symbol.
        if locations.is_empty()
            && let Some((import, binding)) =
                index.type_import_binding_at(line as usize, character as usize)
            && let Some(imported_name) = binding.imported_name.as_deref()
            && let Some(target) = resolve_typescript_definition_import(
                &self.typescript_navigation,
                Path::new(&path),
                &import.specifier,
            )
        {
            locations.push(
                resolve_typescript_type_export(
                    &self.typescript_navigation,
                    target.clone(),
                    imported_name,
                )
                .or_else(|| {
                    resolve_typescript_export(
                        &self.typescript_navigation,
                        target.clone(),
                        imported_name,
                    )
                })
                .map(|(target, declaration)| LspDefinitionLocation {
                    uri: file_uri(&target),
                    range: typescript_range(declaration.range),
                })
                .or_else(|| {
                    ambient_module_location(
                        &self.typescript_navigation,
                        &target,
                        &import.specifier,
                        imported_name,
                    )
                })
                .unwrap_or_else(|| module_root_location(&target)),
            );
        }
        if locations.is_empty() {
            locations = index
                .symbol_at(line as usize, character as usize)
                .map(|symbol| {
                    vec![LspDefinitionLocation {
                        uri: uri.to_owned(),
                        range: typescript_range(symbol.declaration),
                    }]
                })
                .unwrap_or_default();
        }
        if locations.is_empty()
            && let Some((import, binding, member_name)) =
                index.qualified_type_import_at(line as usize, character as usize)
            && let Some(target) = resolve_typescript_definition_import(
                &self.typescript_navigation,
                Path::new(&path),
                &import.specifier,
            )
            && binding.imported_name.is_some()
            && let Some(target_index) = self
                .typescript_navigation
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .index(&target)
            && let Some(declaration_range) = target_index.unique_type_declaration_range(member_name)
        {
            locations.push(LspDefinitionLocation {
                uri: file_uri(&target),
                range: typescript_range(declaration_range),
            });
        }
        if locations.is_empty() {
            locations = typescript_decorator_option_target(
                &self.typescript_navigation,
                path,
                index,
                line as usize,
                character as usize,
            )
            .or_else(|| {
                typescript_call_option_target(
                    &self.typescript_navigation,
                    path,
                    index,
                    line as usize,
                    character as usize,
                )
            })
            .map(|target| vec![target])
            .unwrap_or_default();
        }
        if locations.is_empty() {
            locations = typescript_type_member_target(
                &self.typescript_navigation,
                path,
                index,
                line as usize,
                character as usize,
            )
            .map(|target| vec![target])
            .unwrap_or_default();
        }
        if locations.is_empty()
            && let Some(export) = index.named_reexport_at(line as usize, character as usize)
            && let (Some(specifier), Some(imported_name)) =
                (export.specifier.as_deref(), export.name.as_deref())
            && let Some(target) = resolve_typescript_definition_import(
                &self.typescript_navigation,
                Path::new(&path),
                specifier,
            )
        {
            let declaration = if export.type_only {
                resolve_typescript_type_export(
                    &self.typescript_navigation,
                    target.clone(),
                    imported_name,
                )
            } else {
                resolve_typescript_export(
                    &self.typescript_navigation,
                    target.clone(),
                    imported_name,
                )
            };
            locations.push(
                declaration
                    .map(|(path, declaration)| LspDefinitionLocation {
                        uri: file_uri(&path),
                        range: typescript_range(declaration.range),
                    })
                    .unwrap_or_else(|| module_root_location(&target)),
            );
        }
        if locations.is_empty()
            // The string literal itself, or the `from` keyword before it
            // (e.g. `export * from './dep.js'`), as VS Code does.
            && let Some(specifier) = index
                .static_module_at(line as usize, character as usize)
                .or_else(|| index.module_keyword_at(line as usize, character as usize))
            && let Some(target) = resolve_typescript_definition_import(
                &self.typescript_navigation,
                Path::new(&path),
                specifier,
            )
        {
            locations.push(module_root_location(&target));
        }
        if locations.is_empty()
            && let Some((import, binding, member)) =
                index.namespace_member_at(line as usize, character as usize)
            && !binding.type_only
            && let Some(target) = resolve_typescript_definition_import(
                &self.typescript_navigation,
                Path::new(&path),
                &import.specifier,
            )
            && is_typescript_path(&target)
            && let Some((target, declaration)) =
                resolve_typescript_export(&self.typescript_navigation, target, &member.member_name)
        {
            locations.push(LspDefinitionLocation {
                uri: file_uri(&target),
                range: typescript_range(declaration.range),
            });
        }
        if locations.is_empty()
            && let Some(import) = index.dynamic_import_at(line as usize, character as usize)
            && let Some(target) = resolve_typescript_definition_import(
                &self.typescript_navigation,
                Path::new(&path),
                &import.specifier,
            )
        {
            locations.push(LspDefinitionLocation {
                uri: file_uri(&target),
                range: LspRange {
                    start_line: 0,
                    start_character: 0,
                    end_line: 0,
                    end_character: 0,
                },
            });
        }
        if locations.is_empty()
            && let Some(location) =
                self.ziggy_route_definition(path, text, line as usize, character as usize)
        {
            locations.push(location);
        }
        if locations.is_empty()
            && let Some(location) =
                vite_import_meta_definition(path, text, line as usize, character as usize)
        {
            locations.push(location);
        }
        if locations.is_empty()
            && let Some(location) =
                svelte_rune_definition(path, text, line as usize, character as usize)
        {
            locations.push(location);
        }
        if locations.is_empty()
            && let Some(location) = ecmascript_builtin_definition(
                Path::new(path),
                text,
                line as usize,
                character as usize,
            )
        {
            locations.push(location);
        }
        locations
    }

    /// A name in a component's template (`@click="deleteOpname(op)"`,
    /// `{{ helper(1) }}`) that one of its scripts declares or imports. Names
    /// the template itself introduces (`v-for` variables) are not in the
    /// scripts and stay unresolved.
    fn component_template_definition(
        &self,
        path: &str,
        text: &str,
        uri: &str,
        line: usize,
        character: usize,
    ) -> Option<LspDefinitionLocation> {
        if !is_component_path(Path::new(path)) {
            return None;
        }
        let line_text = text.lines().nth(line)?;
        let (start, end) = identifier_span_utf16(line_text, character)?;
        if line_text[..start].ends_with('.') {
            return None;
        }
        let name = &line_text[start..end];
        let source = component_virtual_source(path, text);
        let index = DocumentIndex::parse(typescript_index_path(path), &source);
        if let Some(binding) = index
            .imports
            .iter()
            .flat_map(|import| import.bindings.iter())
            .find(|binding| binding.local_name == name)
        {
            return typescript_import_target(
                &self.typescript_navigation,
                path,
                &index,
                binding.range.start.line,
                binding.range.start.col,
            )
            .map(|(target, _)| target);
        }
        let declaration = index.unique_runtime_declaration(name)?;
        Some(LspDefinitionLocation {
            uri: uri.to_owned(),
            range: typescript_range(declaration.range),
        })
    }

    /// The `route` helper under the cursor (`route(...)` with no local
    /// declaration or import), which Laravel apps get from Ziggy: its
    /// declaration in the `ziggy-js` package.
    fn ziggy_route_definition(
        &self,
        path: &str,
        text: &str,
        line: usize,
        character: usize,
    ) -> Option<LspDefinitionLocation> {
        let line_text = text.lines().nth(line)?;
        let (start, end) = identifier_span_utf16(line_text, character)?;
        if &line_text[start..end] != "route" {
            return None;
        }
        let before = line_text[..start].chars().next_back();
        if before.is_some_and(|ch| ch == '.' || ch == '$' || ch.is_alphanumeric() || ch == '_') {
            return None;
        }
        if !line_text[end..].trim_start().starts_with('(') {
            return None;
        }
        let target = resolve_typescript_definition_import(
            &self.typescript_navigation,
            Path::new(path),
            "ziggy-js",
        )?;
        let (target, declaration) =
            resolve_typescript_export(&self.typescript_navigation, target, "route")?;
        Some(LspDefinitionLocation {
            uri: file_uri(&target),
            range: typescript_range(declaration.range),
        })
    }

    fn typescript_references(
        &self,
        document: &Arc<DocumentHandle>,
        uri: &str,
        line: u64,
        character: u64,
    ) -> bool {
        let path = document.path();
        let text = document.lock().editor.doc.text();
        let Some(source) = component_position_source(&path, &text, line as usize) else {
            self.analysis
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .set_references(uri, Vec::new());
            return false;
        };
        let index = DocumentIndex::parse(typescript_index_path(&path), &source);
        let mut locations: Vec<LspDefinitionLocation> = index
            .symbol_at(line as usize, character as usize)
            .map(|symbol| {
                symbol
                    .occurrences
                    .into_iter()
                    .map(|range| LspDefinitionLocation {
                        uri: uri.to_owned(),
                        range: typescript_range(range),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut target = None;
        if locations.is_empty()
            && let Some((import_target, occurrences)) = typescript_import_target(
                &self.typescript_navigation,
                &path,
                &index,
                line as usize,
                character as usize,
            )
        {
            locations.push(import_target.clone());
            target = Some(import_target);
            locations.extend(occurrences.into_iter().map(|range| LspDefinitionLocation {
                uri: uri.to_owned(),
                range: typescript_range(range),
            }));
        }
        if target.is_none()
            && let Some(symbol) = index.symbol_at(line as usize, character as usize)
            && let Some(declaration) = index.declarations.iter().find(|declaration| {
                // The declaration range starts at the keyword for functions and
                // classes (`function createMiniHost`) but at the name for
                // variables, so match by containment, not by start.
                declaration.exported
                    && declaration.name == symbol.name
                    && declaration.range.start <= symbol.declaration.start
                    && symbol.declaration.end <= declaration.range.end
            })
        {
            target = Some(LspDefinitionLocation {
                uri: uri.to_owned(),
                range: typescript_range(declaration.range),
            });
        }
        if let Some(target) = target {
            let (has_workspace_snapshot, workspace_locations) = {
                let mut exports = self
                    .typescript_workspace_exports
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                (
                    exports.has_snapshot_for(Path::new(&path)),
                    exports.references_to(Path::new(&path), &target, &self.typescript_navigation),
                )
            };
            if !has_workspace_snapshot
                && let Some(mut project) =
                    TypeScriptWorkspaceExports::build_nearest_project(Path::new(&path))
            {
                locations.extend(project.references_to(
                    Path::new(&path),
                    &target,
                    &self.typescript_navigation,
                ));
            } else {
                locations.extend(workspace_locations);
            }
        }
        locations.sort_by(|a, b| {
            (&a.uri, a.range.start_line, a.range.start_character).cmp(&(
                &b.uri,
                b.range.start_line,
                b.range.start_character,
            ))
        });
        locations.dedup_by(|a, b| {
            a.uri == b.uri
                && a.range.start_line == b.range.start_line
                && a.range.start_character == b.range.start_character
        });
        let found = !locations.is_empty();
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_references(uri, locations);
        found
    }

    fn dart_references(
        &self,
        document: &Arc<DocumentHandle>,
        uri: &str,
        line: u64,
        character: u64,
    ) -> bool {
        let path = document.path();
        let text = document.lock().editor.doc.text();
        let locations: Vec<LspDefinitionLocation> = self
            .dart_engine(Path::new(&path))
            .map(|engine| {
                let engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                if engine.is_ready() {
                    engine.references(Path::new(&path), &text, line as u32, character as u32)
                } else {
                    Default::default()
                }
            })
            .unwrap_or_default()
            .into_iter()
            .map(|found| LspDefinitionLocation {
                uri: file_uri(&found.path),
                range: LspRange {
                    start_line: u64::from(found.line),
                    start_character: u64::from(found.start_column),
                    end_line: u64::from(found.line),
                    end_character: u64::from(found.end_column),
                },
            })
            .collect();
        let answered = !locations.is_empty();
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_references(uri, locations);
        answered
    }

    fn typescript_rename(
        &self,
        document: &Arc<DocumentHandle>,
        uri: &str,
        line: u64,
        character: u64,
        new_name: &str,
    ) -> bool {
        let path = document.path();
        let text = document.lock().editor.doc.text();
        let source = component_position_source(&path, &text, line as usize);
        let index = source
            .as_deref()
            .map(|source| DocumentIndex::parse(typescript_index_path(&path), source));
        let edits: Vec<LspRenameEdit> = if valid_javascript_identifier(new_name)
            && let Some(index) = &index
        {
            index
                .symbol_at(line as usize, character as usize)
                // The template is not analysed. Renaming a name it uses would
                // leave the markup stale, so such renames are refused.
                .filter(|symbol| {
                    !is_component_path(Path::new(&path))
                        || !component_markup_mentions(&text, &symbol.name)
                })
                .map(|symbol| {
                    symbol
                        .occurrences
                        .into_iter()
                        .map(|range| LspRenameEdit {
                            uri: uri.to_owned(),
                            range: typescript_range(range),
                            new_text: new_name.to_owned(),
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let found = !edits.is_empty();
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_rename_edits(uri, edits);
        found
    }

    fn dart_rename(
        &self,
        document: &Arc<DocumentHandle>,
        uri: &str,
        line: u64,
        character: u64,
        new_name: &str,
    ) -> bool {
        let path = document.path();
        let text = document.lock().editor.doc.text();
        let edits: Vec<LspRenameEdit> = self
            .dart_engine(Path::new(&path))
            .map(|engine| {
                let engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                if engine.is_ready() {
                    engine
                        .rename(
                            Path::new(&path),
                            &text,
                            line as u32,
                            character as u32,
                            new_name,
                        )
                        .unwrap_or_default()
                } else {
                    Default::default()
                }
            })
            .unwrap_or_default()
            .into_iter()
            .map(|found| LspRenameEdit {
                uri: file_uri(&found.path),
                range: LspRange {
                    start_line: u64::from(found.line),
                    start_character: u64::from(found.start_column),
                    end_line: u64::from(found.line),
                    end_character: u64::from(found.end_column),
                },
                new_text: new_name.to_owned(),
            })
            .collect();
        let answered = !edits.is_empty();
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_rename_edits(uri, edits);
        answered
    }

    /// Renames the PHP symbol at the cursor. `false` when the name is not
    /// valid, the symbol lives in `vendor/` or the stubs, or is unknown.
    fn php_rename(
        &self,
        document: &Arc<DocumentHandle>,
        uri: &str,
        line: u64,
        character: u64,
        new_name: &str,
    ) -> bool {
        if is_rust_path(Path::new(&document.path())) {
            return self.rust_rename(document, uri, line, character, new_name);
        }
        if is_go_path(Path::new(&document.path())) {
            return self.go_rename(document, uri, line, character, new_name);
        }
        let bare = new_name.trim().trim_start_matches('$');
        let mut edits = Vec::new();
        if php::valid_name(bare)
            && let Some((query, found)) = self.php_usages(document, line, character)
            && !query.external
        {
            let text = if query.is_variable() {
                format!("${bare}")
            } else {
                bare.to_owned()
            };
            edits = found
                .into_iter()
                .map(|usage| LspRenameEdit {
                    uri: file_uri(&usage.path),
                    range: LspRange {
                        start_line: u64::from(usage.line),
                        start_character: u64::from(usage.start_column),
                        end_line: u64::from(usage.line),
                        end_character: u64::from(usage.end_column),
                    },
                    new_text: if usage.dollar {
                        text.clone()
                    } else {
                        bare.to_owned()
                    },
                })
                .collect();
        }
        let found = !edits.is_empty();
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_rename_edits(uri, edits);
        found
    }

    /// Answers go-to-definition from the PHP engine; `false` means no
    /// definition was found.
    fn php_definition(
        &self,
        document: &Arc<DocumentHandle>,
        uri: &str,
        line: u64,
        character: u64,
    ) -> bool {
        let path = document.path();
        let engine = if is_go_path(Path::new(&path)) {
            self.go_engine(Path::new(&path)).map(Engine::Go)
        } else {
            self.php_engine_any(Path::new(&path)).map(Engine::Php)
        };
        let Some(engine) = engine else {
            self.analysis
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .set_definitions(uri, Vec::new());
            return false;
        };
        let text = document.lock().editor.doc.text();
        let targets = engine.definition(Path::new(&path), &text, line as u32, character as u32);
        let locations: Vec<LspDefinitionLocation> = targets
            .into_iter()
            .map(|target| LspDefinitionLocation {
                uri: file_uri(&target.path),
                range: LspRange {
                    start_line: u64::from(target.line),
                    start_character: u64::from(target.column),
                    end_line: u64::from(target.line),
                    end_character: u64::from(target.column),
                },
            })
            .collect();
        let found = !locations.is_empty();
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_definitions(uri, locations);
        found
    }

    fn rust_definition(
        &self,
        document: &Arc<DocumentHandle>,
        uri: &str,
        line: u64,
        character: u64,
    ) -> bool {
        let path = document.path();
        let Some(engine) = self.rust_engine(Path::new(&path)).map(Engine::Rust) else {
            return false;
        };
        let text = document.lock().editor.doc.text();
        let locations = engine
            .definition(Path::new(&path), &text, line as u32, character as u32)
            .into_iter()
            .map(|target| LspDefinitionLocation {
                uri: file_uri(&target.path),
                range: LspRange {
                    start_line: u64::from(target.line),
                    start_character: u64::from(target.column),
                    end_line: u64::from(target.line),
                    end_character: u64::from(target.column),
                },
            })
            .collect::<Vec<_>>();
        let found = !locations.is_empty();
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_definitions(uri, locations);
        found
    }

    fn dart_definition(
        &self,
        document: &Arc<DocumentHandle>,
        uri: &str,
        line: u64,
        character: u64,
    ) -> bool {
        let path = document.path();
        let Some(engine) = self.dart_engine(Path::new(&path)) else {
            return false;
        };
        let text = document.lock().editor.doc.text();
        let engine = engine.lock().unwrap_or_else(|e| e.into_inner());
        let locations = engine
            .definition(Path::new(&path), &text, line as u32, character as u32)
            .into_iter()
            .map(|target| LspDefinitionLocation {
                uri: file_uri(&target.path),
                range: LspRange {
                    start_line: u64::from(target.line),
                    start_character: u64::from(target.column),
                    end_line: u64::from(target.line),
                    end_character: u64::from(target.column),
                },
            })
            .collect::<Vec<_>>();
        let found = !locations.is_empty();
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_definitions(uri, locations);
        found
    }
}

fn is_php_path(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    name.ends_with(".php") && !name.ends_with(".blade.php")
}

/// Byte span of the identifier touching UTF-16 `character` in `line_text`.
fn identifier_span_utf16(line_text: &str, character: usize) -> Option<(usize, usize)> {
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_' || ch == '$';
    let mut utf16 = 0;
    let mut byte = line_text.len();
    for (index, ch) in line_text.char_indices() {
        if utf16 + ch.len_utf16() > character {
            byte = index;
            break;
        }
        utf16 += ch.len_utf16();
    }
    let at_cursor = line_text[byte..].chars().next().is_some_and(is_word);
    let anchor = if at_cursor {
        byte
    } else {
        let previous = line_text[..byte].char_indices().next_back()?;
        if !is_word(previous.1) {
            return None;
        }
        previous.0
    };
    let start = line_text[..anchor]
        .char_indices()
        .rev()
        .take_while(|(_, ch)| is_word(*ch))
        .last()
        .map_or(anchor, |(index, _)| index);
    let end = line_text[anchor..]
        .char_indices()
        .find(|(_, ch)| !is_word(*ch))
        .map_or(line_text.len(), |(index, _)| anchor + index);
    Some((start, end))
}

/// The Laravel route named by the string literal under the cursor when it is
/// the first argument of `route(`. Reads `routes/*.php` of the nearest
/// ancestor holding an `artisan` script.
fn laravel_route_definition(
    path: &Path,
    text: &str,
    line: usize,
    character: usize,
) -> Option<LspDefinitionLocation> {
    let line_text = text.lines().nth(line)?;
    let name = route_name_at(line_text, character)?;
    let root = path
        .ancestors()
        .skip(1)
        .find(|dir| dir.join("artisan").is_file())?;
    let item = galaxy_core::php::framework::laravel::Index::find_route(root, name)?;
    let loc = item.loc?;
    Some(LspDefinitionLocation {
        uri: file_uri(&loc.path),
        range: LspRange {
            start_line: u64::from(loc.line),
            start_character: u64::from(loc.column),
            end_line: u64::from(loc.line),
            end_character: u64::from(loc.column) + name.encode_utf16().count() as u64,
        },
    })
}

/// Contents of the single-line string literal under UTF-16 `character` when
/// it is the first argument of a bare `route(` call. Scanning from each
/// `route(` keeps `:href="route('a.b')"` working, where the call itself sits
/// inside an attribute's quotes.
fn route_name_at(line_text: &str, character: usize) -> Option<&str> {
    let mut search = 0;
    while let Some(found) = line_text[search..].find("route") {
        let call = search + found;
        search = call + "route".len();
        if line_text[..call]
            .chars()
            .next_back()
            .is_some_and(|ch| ch == '.' || ch == '$' || ch.is_alphanumeric() || ch == '_')
        {
            continue;
        }
        let after = line_text[search..].trim_start();
        let Some(after) = after.strip_prefix('(') else {
            continue;
        };
        let after = after.trim_start();
        let Some(quote) = after
            .chars()
            .next()
            .filter(|ch| matches!(ch, '\'' | '"' | '`'))
        else {
            continue;
        };
        let open = line_text.len() - after.len();
        let Some(length) = after[1..].find(quote) else {
            continue;
        };
        let close = open + 1 + length;
        let name = &line_text[open + 1..close];
        let (from, to) = (
            line_text[..open].encode_utf16().count(),
            line_text[..=close].encode_utf16().count(),
        );
        if character >= from && character <= to && !name.is_empty() && !name.contains("${") {
            return Some(name);
        }
    }
    None
}

/// Svelte 5 runes (`$props`, `$state`, ...) are compiler globals declared as
/// `declare function $name` in the installed `svelte` package's types.
const SVELTE_RUNES: [&str; 7] = [
    "$props",
    "$state",
    "$derived",
    "$effect",
    "$bindable",
    "$inspect",
    "$host",
];

fn svelte_rune_definition(
    path: &str,
    text: &str,
    line: usize,
    character: usize,
) -> Option<LspDefinitionLocation> {
    let file_name = Path::new(path).file_name()?.to_str()?;
    if !(file_name.ends_with(".svelte")
        || file_name.ends_with(".svelte.ts")
        || file_name.ends_with(".svelte.js"))
    {
        return None;
    }
    let line_text = text.lines().nth(line)?;
    let (start, end) = identifier_span_utf16(line_text, character)?;
    let name = &line_text[start..end];
    if !SVELTE_RUNES.contains(&name) {
        return None;
    }
    let types = Path::new(path)
        .ancestors()
        .skip(1)
        .map(|dir| dir.join("node_modules/svelte/types/index.d.ts"))
        .find(|candidate| candidate.is_file())?;
    let contents = std::fs::read_to_string(&types).ok()?;
    let prefix = format!("declare function {name}");
    let (row, entry) = contents.lines().enumerate().find(|(_, entry)| {
        entry
            .strip_prefix(&prefix)
            .is_some_and(|tail| tail.starts_with(['(', '<']))
    })?;
    let column = entry.len() - entry.trim_start().len() + "declare function ".len();
    Some(LspDefinitionLocation {
        uri: file_uri(&types),
        range: LspRange {
            start_line: row as u64,
            start_character: column as u64,
            end_line: row as u64,
            end_character: (column + name.len()) as u64,
        },
    })
}

/// Vite's `import.meta.*`: members resolve to the project's
/// `vite/types/importMeta.d.ts`, and `import.meta.env.VITE_*` keys to the
/// `.env` file that defines them.
fn vite_import_meta_definition(
    path: &str,
    text: &str,
    line: usize,
    character: usize,
) -> Option<LspDefinitionLocation> {
    let line_text = text.lines().nth(line)?;
    let token = token_at_utf16_position(line_text, character)?;
    if token.is_empty() {
        return None;
    }
    let start = token.as_ptr() as usize - line_text.as_ptr() as usize;
    let before = &line_text[..start];
    let interface = if before.ends_with("import.meta.env.") {
        "ImportMetaEnv"
    } else if before.ends_with("import.meta.") {
        "ImportMeta"
    } else {
        return None;
    };
    let ancestors = Path::new(path).ancestors().skip(1).collect::<Vec<_>>();
    let types = ancestors
        .iter()
        .map(|dir| dir.join("node_modules/vite/types/importMeta.d.ts"))
        .find(|candidate| candidate.is_file());
    if let Some(types) = &types
        && let Some(range) = interface_member_range(types, interface, token)
    {
        return Some(LspDefinitionLocation {
            uri: file_uri(types),
            range,
        });
    }

    if interface != "ImportMetaEnv" {
        return None;
    }
    let root = ancestors.iter().find(|dir| {
        dir.join("package.json").is_file()
            || ["js", "ts", "mjs", "mts"]
                .iter()
                .any(|extension| dir.join(format!("vite.config.{extension}")).is_file())
    })?;
    [
        ".env.local",
        ".env",
        ".env.development",
        ".env.production",
        ".env.example",
    ]
    .iter()
    .find_map(|name| {
        let file = root.join(name);
        let contents = std::fs::read_to_string(&file).ok()?;
        let (row, column) = contents.lines().enumerate().find_map(|(row, entry)| {
            let trimmed = entry.trim_start();
            let declaration = trimmed
                .strip_prefix("export ")
                .unwrap_or(trimmed)
                .trim_start();
            let rest = declaration.strip_prefix(token)?;
            rest.trim_start()
                .starts_with('=')
                .then(|| (row, entry.len() - declaration.len()))
        })?;
        Some(LspDefinitionLocation {
            uri: file_uri(&file),
            range: LspRange {
                start_line: row as u64,
                start_character: column as u64,
                end_line: row as u64,
                end_character: (column + token.len()) as u64,
            },
        })
    })
}

/// The range of `member` inside `interface <name> { ... }` of a declaration
/// file, when it is declared exactly once.
fn interface_member_range(file: &Path, interface: &str, member: &str) -> Option<LspRange> {
    let contents = std::fs::read_to_string(file).ok()?;
    let mut inside = false;
    let mut found = Vec::new();
    for (row, entry) in contents.lines().enumerate() {
        let trimmed = entry.trim_start();
        if !inside {
            inside = trimmed.strip_prefix("interface ").is_some_and(|rest| {
                rest.strip_prefix(interface)
                    .is_some_and(|tail| tail.starts_with([' ', '<', '{']))
            });
            continue;
        }
        if entry.starts_with('}') {
            inside = false;
            continue;
        }
        let declaration = trimmed.strip_prefix("readonly ").unwrap_or(trimmed);
        if let Some(tail) = declaration.strip_prefix(member)
            && tail.starts_with(['?', ':', '(', '<'])
        {
            let column = entry.len() - declaration.len();
            found.push((row, column));
        }
    }
    let [(row, column)] = found.as_slice() else {
        return None;
    };
    Some(LspRange {
        start_line: *row as u64,
        start_character: *column as u64,
        end_line: *row as u64,
        end_character: (*column + member.len()) as u64,
    })
}

fn ecmascript_builtin_definition(
    path: &Path,
    text: &str,
    line: usize,
    character: usize,
) -> Option<LspDefinitionLocation> {
    let line_text = text.lines().nth(line)?;
    let byte = line_text
        .char_indices()
        .map(|(byte, ch)| (byte, ch.len_utf16()))
        .scan(0usize, |utf16, (byte, width)| {
            let start = *utf16;
            *utf16 += width;
            Some((byte, start, *utf16))
        })
        .find(|(_, start, end)| *start <= character && character < *end)
        .map(|(byte, _, _)| byte)
        .unwrap_or(line_text.len());
    let prefix_length = line_text[..byte]
        .chars()
        .rev()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>()
        .len();
    let start = byte.saturating_sub(prefix_length);
    let end = line_text[byte..]
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .map(char::len_utf8)
        .sum::<usize>()
        + byte;
    let token = &line_text[start..end];
    let owner = line_text[..start].strip_suffix('.').and_then(|prefix| {
        prefix
            .chars()
            .rev()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>()
            .into()
    });
    let library = StdLib::for_file(path);
    let location = match owner.as_deref() {
        Some(owner) if library.has_global(owner) => library
            .value_member(owner, token)
            .or_else(|| library.type_member(owner, token))?
            .locations
            .into_iter()
            .next()?,
        Some(owner) if ecmascript_builtin_receiver(text, line_text, owner) => {
            let type_name = receiver_library_type(text, owner)?;
            library
                .type_member(type_name, token)?
                .locations
                .into_iter()
                .next()?
        }
        None if ecmascript_primitive_type(token) => library
            .type_locations(ecmascript_primitive_interface(token))
            .into_iter()
            .next()?,
        None => library
            .value_locations(token)
            .into_iter()
            .next()
            .or_else(|| library.type_locations(token).into_iter().next())?,
        _ => return None,
    };
    let source = library
        .source_directory()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../galaxy-core/stubs/typescript-lib")
        })
        .join(&location.file_name);
    Some(LspDefinitionLocation {
        uri: file_uri(&source),
        range: LspRange {
            start_line: location.range.start.line as u64,
            start_character: location.range.start.col as u64,
            end_line: location.range.end.line as u64,
            end_character: location.range.end.col as u64,
        },
    })
}

fn ecmascript_primitive_type_definition(
    path: &Path,
    text: &str,
    line: usize,
    character: usize,
) -> Option<LspDefinitionLocation> {
    let location = ecmascript_builtin_definition(path, text, line, character)?;
    let line_text = text.lines().nth(line)?;
    let token = token_at_utf16_position(line_text, character)?;
    ecmascript_primitive_type(token).then_some(location)
}

fn token_at_utf16_position(line_text: &str, character: usize) -> Option<&str> {
    let byte = line_text
        .char_indices()
        .map(|(byte, ch)| (byte, ch.len_utf16()))
        .scan(0usize, |utf16, (byte, width)| {
            let start = *utf16;
            *utf16 += width;
            Some((byte, start, *utf16))
        })
        .find(|(_, start, end)| *start <= character && character < *end)
        .map(|(byte, _, _)| byte)?;
    let start = line_text[..byte]
        .char_indices()
        .rev()
        .find(|(_, ch)| !ch.is_ascii_alphanumeric() && *ch != '_')
        .map_or(0, |(index, ch)| index + ch.len_utf8());
    let end = line_text[byte..]
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .map(char::len_utf8)
        .sum::<usize>()
        + byte;
    line_text.get(start..end).map(str::trim)
}

fn ecmascript_builtin_receiver(text: &str, _line_text: &str, receiver: &str) -> bool {
    receiver_library_type(text, receiver).is_some()
}

fn receiver_library_type<'a>(text: &str, receiver: &str) -> Option<&'a str> {
    if text.lines().any(|line| {
        line.contains(&format!("{receiver}: ")) && (line.contains("[]") || line.contains("Array<"))
    }) {
        return Some("Array");
    }
    for (annotation, type_name) in [
        ("string", "String"),
        ("number", "Number"),
        ("boolean", "Boolean"),
    ] {
        let annotations = [
            format!("{receiver}: {annotation}"),
            format!("{receiver}?: {annotation}"),
            format!("{receiver}: readonly {annotation}"),
        ];
        if text.lines().any(|line| {
            annotations.iter().any(|written| {
                line.find(written).is_some_and(|start| {
                    line[start + written.len()..]
                        .chars()
                        .next()
                        .is_none_or(|next| !matches!(next, '[' | '<'))
                })
            })
        }) {
            return Some(type_name);
        }
    }
    None
}

fn ecmascript_primitive_type(name: &str) -> bool {
    matches!(name, "string" | "number" | "boolean")
}

fn ecmascript_primitive_interface(name: &str) -> &str {
    match name {
        "string" => "String",
        "number" => "Number",
        "boolean" => "Boolean",
        _ => unreachable!("primitive type was checked before conversion"),
    }
}

fn is_go_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("go"))
}

fn is_dart_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("dart"))
}

fn is_rust_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
}

fn is_typescript_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs")
    )
}

/// Names a TypeScript file may use without declaring them: function-scope
/// and CommonJS bindings that no `lib.*.d.ts` declares.
const TYPESCRIPT_IMPLICIT_NAMES: &[&str] = &[
    "arguments",
    "undefined",
    "require",
    "module",
    "exports",
    "__dirname",
    "__filename",
    "globalThis",
];

/// Files whose unknown names are reported. Declaration files describe names
/// owned elsewhere, and `.svelte.ts` modules use compiler runes.
fn typescript_reports_unknown_names(path: &str) -> bool {
    !path.ends_with(".d.ts")
        && !path.ends_with(".svelte.ts")
        && matches!(
            Path::new(path)
                .extension()
                .and_then(|extension| extension.to_str()),
            Some("ts" | "tsx" | "mts" | "cts")
        )
}

fn is_component_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("vue" | "svelte")
    )
}

/// Returns JavaScript/TypeScript source in a virtual document that preserves
/// the component's original line and UTF-16 column coordinates. Only a cursor
/// inside a `<script>` body produces source, keeping template markup out of
/// definition navigation.
fn component_script_source(path: &str, text: &str, line: usize) -> Option<String> {
    if !is_component_path(Path::new(path)) {
        return Some(text.to_owned());
    }
    let mut search = 0;
    while let Some(relative_start) = text[search..].find("<script") {
        let start = search + relative_start;
        let tag_end = text[start..].find('>')? + start;
        let body_start = tag_end + 1;
        let body_end = text[body_start..].find("</script>")? + body_start;
        let start_line = text[..body_start]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        let end_line = start_line
            + text[body_start..body_end]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count();
        if (start_line..=end_line).contains(&line) {
            let mut virtual_source = String::with_capacity(body_end);
            virtual_source.extend(
                text[..body_start]
                    .chars()
                    .map(|character| if character == '\n' { '\n' } else { ' ' }),
            );
            virtual_source.push_str(&text[body_start..body_end]);
            return Some(virtual_source);
        }
        search = body_end + "</script>".len();
    }
    None
}

/// Path the TypeScript engine parses a document under. Component scripts are
/// parsed as TypeScript, which also accepts plain JavaScript.
fn typescript_index_path(path: &str) -> String {
    if is_component_path(Path::new(path)) {
        format!("{path}.ts")
    } else {
        path.to_owned()
    }
}

/// Byte ranges of every `<script>` body in a Vue or Svelte component, e.g.
/// Vue's `<script>` plus `<script setup>`, or Svelte's module script.
fn component_script_bodies(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut bodies = Vec::new();
    let mut search = 0;
    while let Some(relative_start) = text[search..].find("<script") {
        let start = search + relative_start;
        let Some(tag_end) = text[start..].find('>').map(|end| start + end) else {
            break;
        };
        let body_start = tag_end + 1;
        let Some(body_end) = text[body_start..]
            .find("</script>")
            .map(|end| body_start + end)
        else {
            break;
        };
        bodies.push(body_start..body_end);
        search = body_end + "</script>".len();
    }
    bodies
}

/// Appends `markup` as blanks of the same UTF-16 width, keeping newlines, so
/// positions in the result match the original document.
fn push_blanked(out: &mut String, markup: &str) {
    for character in markup.chars() {
        if character == '\n' {
            out.push('\n');
        } else {
            out.extend(std::iter::repeat_n(' ', character.len_utf16()));
        }
    }
}

/// All scripts of a component as one TypeScript document, with the markup
/// blanked. Lines and UTF-16 columns map 1:1 to the component, so engine
/// locations need no translation. Plain TypeScript files pass through.
fn component_virtual_source(path: &str, text: &str) -> String {
    if !is_component_path(Path::new(path)) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for body in component_script_bodies(text) {
        push_blanked(&mut out, &text[copied..body.start]);
        out.push_str(&text[body.clone()]);
        copied = body.end;
    }
    push_blanked(&mut out, &text[copied..]);
    out
}

/// Source for a position request: `None` when the cursor of a component is
/// outside its scripts, so template markup never resolves to script symbols.
fn component_position_source(path: &str, text: &str, line: usize) -> Option<String> {
    component_script_source(path, text, line)?;
    Some(component_virtual_source(path, text))
}

/// Whether a component's markup (template, styles) mentions `name`, which a
/// script-only rename would leave stale. Covers Svelte's `$store` form and
/// the kebab-case tag of a PascalCase component (`<my-card>` for `MyCard`).
fn component_markup_mentions(text: &str, name: &str) -> bool {
    let mut markup = String::with_capacity(text.len());
    let mut copied = 0;
    for body in component_script_bodies(text) {
        markup.push_str(&text[copied..body.start]);
        copied = body.end;
    }
    markup.push_str(&text[copied..]);
    let mut forms = vec![name.to_owned()];
    if name.chars().next().is_some_and(char::is_uppercase) {
        let mut kebab = String::new();
        for (index, character) in name.char_indices() {
            if character.is_uppercase() && index > 0 {
                kebab.push('-');
            }
            kebab.extend(character.to_lowercase());
        }
        if kebab.contains('-') {
            forms.push(kebab);
        }
    }
    // `$` is not a word character here, so `$count` mentions `count`.
    let word = |c: char| c.is_alphanumeric() || c == '_';
    forms.iter().any(|form| {
        markup.match_indices(form.as_str()).any(|(start, _)| {
            let before = markup[..start].chars().next_back();
            let after = markup[start + form.len()..].chars().next();
            !before.is_some_and(word)
                && !after.is_some_and(|c| word(c) || (c == '-' && form.contains('-')))
        })
    })
}

/// Returns the first body line for the script containing `line`.
fn component_script_insertion_line(path: &str, text: &str, line: usize) -> Option<usize> {
    if !is_component_path(Path::new(path)) {
        return Some(0);
    }
    let mut search = 0;
    while let Some(relative_start) = text[search..].find("<script") {
        let start = search + relative_start;
        let tag_end = text[start..].find('>')? + start;
        let body_start = tag_end + 1;
        let body_end = text[body_start..].find("</script>")? + body_start;
        let start_line = text[..body_start]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        let end_line = start_line
            + text[body_start..body_end]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count();
        if (start_line..=end_line).contains(&line) {
            // A newline directly after the opening tag begins the script body
            // on the next document line, not the tag's line.
            return Some(start_line + usize::from(text[body_start..].starts_with('\n')));
        }
        search = body_end + "</script>".len();
    }
    None
}

/// Detects PHP frameworks and conventional source roots without traversing
/// dependency trees such as `vendor/`.
fn php_workspace_detected(root: &Path) -> bool {
    if root.join("composer.json").is_file()
        || ["artisan", "yii", "spark", "wp-config.php"]
            .into_iter()
            .any(|name| root.join(name).is_file())
        || root.join("bin/console").is_file()
        || root.join("system/CodeIgniter.php").is_file()
    {
        return true;
    }
    [
        "app",
        "src",
        "application",
        "modules",
        "packages",
        "libraries",
        "common",
        "backend",
        "frontend",
        "web/modules",
        "sites/all/modules",
        "wp-content/plugins",
        "wp-content/themes",
    ]
    .into_iter()
    .any(|path| contains_php_source(&root.join(path), 2))
}

fn contains_php_source(dir: &Path, depth: u8) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("php"))
        {
            return true;
        }
        if depth > 0
            && entry.file_type().is_ok_and(|kind| kind.is_dir())
            && contains_php_source(&path, depth - 1)
        {
            return true;
        }
    }
    false
}

fn typescript_range(range: galaxy_core::typescript::Utf16Range) -> LspRange {
    LspRange {
        start_line: range.start.line as u64,
        start_character: range.start.col as u64,
        end_line: range.end.line as u64,
        end_character: range.end.col as u64,
    }
}

fn workspace_index_metadata(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(
            "package.json"
                | "tsconfig.json"
                | "jsconfig.json"
                | "Cargo.toml"
                | "Cargo.lock"
                | "go.mod"
                | "go.sum"
                | "composer.json"
                | "composer.lock"
                | "pubspec.yaml"
                | "pubspec.lock"
        )
    )
}

/// Built-in language engines keyed by project root, shared across threads.
type EngineMap<E> = Arc<Mutex<HashMap<PathBuf, Arc<Mutex<E>>>>>;

#[allow(clippy::too_many_arguments)]
fn spawn_workspace_indexing(
    workspace: CoreWorkspace,
    analysis: Arc<Mutex<BuiltinStore>>,
    typescript_exports: Arc<Mutex<TypeScriptWorkspaceExports>>,
    go_engines: EngineMap<GoEngine>,
    php_engines: EngineMap<PhpEngine>,
    rust_engines: EngineMap<RustEngine>,
    dart_engines: EngineMap<DartEngine>,
    indexing_roots: Arc<Mutex<HashMap<PathBuf, IndexingState>>>,
    listener: Option<Arc<dyn CoreListener>>,
) {
    let root = workspace.root().to_path_buf();
    let _ = std::thread::Builder::new()
        .name("workspace-indexing".into())
        .spawn(move || {
            let started = Instant::now();
            let retry_workspace = workspace.clone();
            let paths = workspace.build_file_index().paths().collect::<Vec<_>>();
            workspace.set_indexed_paths(paths.clone());
            send_workspace_index_started(&listener, &root, paths.len() as u64);
            // Parsing a large TypeScript workspace is expensive. Build without
            // holding the shared snapshot lock so editor requests stay responsive.
            let typescript_started = Instant::now();
            let typescript_snapshot = TypeScriptWorkspaceExports::build_with_progress(
                &root,
                &paths,
                |completed, total| {
                    send_workspace_index_progress(&listener, &root, completed, total)
                },
            );
            log_language_index(
                "typescript",
                &root,
                typescript_snapshot.index_stats(&root),
                typescript_started.elapsed(),
            );
            let typescript_diagnostics = typescript_snapshot.diagnostics();
            typescript_exports
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .install(&root, typescript_snapshot);
            {
                let mut store = analysis.lock().unwrap_or_else(|e| e.into_inner());
                for (uri, diagnostics) in typescript_diagnostics {
                    store.set_diagnostics(&uri, diagnostics);
                }
            }
            publish_workspace_rust_diagnostics(&analysis, &root, &paths, &rust_engines, &listener);
            // Globals are scanned before taking the lock; it reads thousands of files.
            let ambient = typescript_globals::scan_ambient_globals(&root, &paths);
            {
                let mut exports = typescript_exports.lock().unwrap_or_else(|e| e.into_inner());
                exports.set_ambient(&root, ambient);
            }
            let (total, changed) = refresh_workspace_index(&workspace, &paths);
            let mut workers = Vec::new();
            if let Some(project) = go::GoProject::detect(&root) {
                let project_root = project.root().to_path_buf();
                let (engine, _) = pending_go_engine(&go_engines, &project_root);
                workers.push(spawn_go_index(&engine, &project_root));
            }
            if php_workspace_detected(&root) {
                let (engine, _) = pending_php_engine(&php_engines, &root);
                workers.push(spawn_php_index(&engine, &root));
            }
            if root.join("pubspec.yaml").is_file() {
                let (engine, _) = pending_dart_engine(&dart_engines, &root);
                workers.push(spawn_dart_index(&engine, &root));
            }
            for worker in workers.into_iter().flatten() {
                let _ = worker.join();
            }
            send_workspace_index_progress(&listener, &root, total, total);
            send_workspace_index_finished(&listener, &root, total, changed);
            if let Some(engine) = php_engines
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&root)
                .cloned()
            {
                let files = engine
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .index_stats()
                    .files as u64;
                if let Some(listener) = &listener {
                    listener.on_event(CoreEvent::PhpIndexFinished {
                        root: root.to_string_lossy().into_owned(),
                        files,
                    });
                }
            }
            tracing::debug!(
                root = %root.display(),
                files = total,
                changed,
                ms = started.elapsed().as_millis() as u64,
                "rebuilt built-in workspace indexes"
            );
            let state = indexing_roots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&root)
                .unwrap_or_default();
            if !state.pending_typescript.is_empty() {
                typescript_exports
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .apply_changes(&root, &state.pending_typescript);
            }
            if state.rerun {
                indexing_roots
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(root.clone(), IndexingState::default());
                spawn_workspace_indexing(
                    retry_workspace,
                    analysis,
                    typescript_exports,
                    go_engines,
                    php_engines,
                    rust_engines,
                    dart_engines,
                    indexing_roots,
                    listener,
                );
            }
        });
}

/// Publishes diagnostics for all Rust sources in each Cargo project below the
/// workspace. This runs in the workspace worker, never in the 100 ms UI poll.
fn publish_workspace_rust_diagnostics(
    analysis: &Arc<Mutex<BuiltinStore>>,
    root: &Path,
    paths: &[String],
    engines: &EngineMap<RustEngine>,
    listener: &Option<Arc<dyn CoreListener>>,
) {
    const MAX_FILES: usize = 10_000;
    let mut projects: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    for relative in paths
        .iter()
        .filter(|path| is_rust_path(Path::new(path)))
        .take(MAX_FILES)
    {
        let path = root.join(relative);
        projects
            .entry(rust::project_root(&path))
            .or_default()
            .push(path);
    }
    for (project_root, files) in projects {
        let started = Instant::now();
        send_workspace_index_phase(listener, root, "Indexing Rust symbols", 0, 0);
        let engine = {
            let mut engines = engines.lock().unwrap_or_else(|e| e.into_inner());
            engines
                .entry(project_root.clone())
                .or_insert_with(|| Arc::new(Mutex::new(RustEngine::pending(&project_root))))
                .clone()
        };
        let built = build_rust_index(&project_root);
        let snapshot = RustEngine::from_built(built);
        let fingerprint = snapshot.diagnostics_fingerprint();
        let cached = Mutex::new(rust_diagnostics_cache::load(&project_root, fingerprint));
        let diagnostic_total = files.len() as u64;
        send_workspace_index_phase(
            listener,
            root,
            "Checking Rust diagnostics",
            0,
            diagnostic_total,
        );
        // The immutable snapshot can diagnose files in parallel without
        // retaining the live engine mutex or populating its open-file cache.
        let results = galaxy_core::index_scheduler::shared_index_scheduler()
            .map_ordered_with_progress(
                files,
                |path| {
                    let (size, modified_nanos) = file_size_and_mtime(&path)?;
                    let reused = cached
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&path)
                        .filter(|file| file.size == size && file.modified_nanos == modified_nanos);
                    let problems = match reused {
                        Some(file) => file.problems,
                        None => {
                            let text = std::fs::read_to_string(&path).ok()?;
                            snapshot
                                .diagnostics_indexed(&path, &text)
                                .into_iter()
                                .map(rust_diagnostics_cache::CachedProblem::from)
                                .collect()
                        }
                    };
                    let uri = file_uri(&path);
                    let diagnostics = problems
                        .iter()
                        .cloned()
                        .map(|problem| LspDiagnostic {
                            uri: uri.clone(),
                            range: LspRange {
                                start_line: u64::from(problem.start_line),
                                start_character: u64::from(problem.start_column),
                                end_line: u64::from(problem.end_line),
                                end_character: u64::from(problem.end_column),
                            },
                            severity: DiagnosticSeverity::Error,
                            message: problem.message,
                            source: Some("galaxy".into()),
                            code: None,
                            unnecessary: false,
                        })
                        .collect();
                    let entry = rust_diagnostics_cache::CachedFile {
                        size,
                        modified_nanos,
                        problems,
                    };
                    Some((path, uri, diagnostics, entry))
                },
                |completed, total| {
                    send_workspace_index_phase(
                        listener,
                        root,
                        "Checking Rust diagnostics",
                        completed as u64,
                        total as u64,
                    )
                },
            );
        engine
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .install(snapshot.into_built());
        let mut entries = BTreeMap::new();
        {
            let mut analysis = analysis.lock().unwrap_or_else(|e| e.into_inner());
            for (path, uri, diagnostics, entry) in results.into_iter().flatten() {
                analysis.set_diagnostics(&uri, diagnostics);
                entries.insert(path, entry);
            }
        }
        if let Err(error) = rust_diagnostics_cache::save(&project_root, fingerprint, entries) {
            tracing::warn!(root = %project_root.display(), %error, "failed to save Rust diagnostics cache");
        }
        tracing::debug!(
            root = %project_root.display(),
            ms = started.elapsed().as_millis() as u64,
            "indexed Rust workspace diagnostics"
        );
    }
}

fn send_workspace_index_phase(
    listener: &Option<Arc<dyn CoreListener>>,
    root: &Path,
    phase: &str,
    completed: u64,
    total: u64,
) {
    if let Some(listener) = listener {
        listener.on_event(CoreEvent::WorkspaceIndexPhase {
            root: root.to_string_lossy().into_owned(),
            phase: phase.to_owned(),
            completed,
            total,
        });
    }
}

fn send_workspace_index_started(listener: &Option<Arc<dyn CoreListener>>, root: &Path, total: u64) {
    if let Some(listener) = listener {
        listener.on_event(CoreEvent::WorkspaceIndexStarted {
            root: root.to_string_lossy().into_owned(),
            total,
        });
    }
}

fn send_workspace_index_finished(
    listener: &Option<Arc<dyn CoreListener>>,
    root: &Path,
    total: u64,
    changed: u64,
) {
    if let Some(listener) = listener {
        listener.on_event(CoreEvent::WorkspaceIndexFinished {
            root: root.to_string_lossy().into_owned(),
            total,
            changed,
        });
    }
}

fn send_workspace_index_progress(
    listener: &Option<Arc<dyn CoreListener>>,
    root: &Path,
    completed: u64,
    total: u64,
) {
    if let Some(listener) = listener {
        listener.on_event(CoreEvent::WorkspaceIndexProgress {
            root: root.to_string_lossy().into_owned(),
            completed,
            total,
        });
    }
}

/// One manifest is shared by every language service. It records the current
/// workspace file set and freshness, while language engines own their semantic
/// payloads. Reopening a workspace always walks it again and updates this
/// cache incrementally, so additions and removals are never missed.
fn refresh_workspace_index(workspace: &CoreWorkspace, paths: &[String]) -> (u64, u64) {
    let root = workspace.root();
    let total = paths.len() as u64;
    let previous = load_workspace_manifest(root);
    let mut current = BTreeMap::new();
    let mut changed = 0_u64;
    for path in paths {
        if let Some(freshness) = workspace_file_freshness(&root.join(path)) {
            if previous.get(path) != Some(&freshness) {
                changed += 1;
            }
            current.insert(path.clone(), freshness);
        }
    }
    changed += previous
        .keys()
        .filter(|path| !current.contains_key(*path))
        .count() as u64;
    save_workspace_manifest(root, &current);
    (total, changed)
}

fn file_size_and_mtime(path: &Path) -> Option<(u64, u128)> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?;
    Some((metadata.len(), modified.as_nanos()))
}

/// Builds the Rust index of `root`, reparsing only files whose size or
/// modification time changed since the cached build.
fn build_rust_index(root: &Path) -> rust::Built {
    let Some(path) = rust_index_cache_path(root) else {
        return RustEngine::build(root);
    };
    let mut cache = rust::RustIndexCache::load(&path, root);
    let built = RustEngine::build_with_cache(root, &mut cache);
    if let Err(error) = cache.save(&path, root) {
        tracing::warn!(root = %root.display(), %error, "failed to save Rust index cache");
    }
    built
}

fn rust_index_cache_path(root: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut hasher);
    Some(
        PathBuf::from(home)
            .join("Library/Caches/Galaxy/workspace-index")
            .join(format!("{:016x}.rust-index-v1", hasher.finish())),
    )
}

fn workspace_file_freshness(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?;
    Some(format!("{}:{}", metadata.len(), modified.as_nanos()))
}

fn workspace_manifest_path(root: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut hasher);
    Some(
        PathBuf::from(home)
            .join("Library/Caches/Galaxy/workspace-index")
            .join(format!("{:016x}.manifest", hasher.finish())),
    )
}

fn load_workspace_manifest(root: &Path) -> BTreeMap<String, String> {
    let Some(path) = workspace_manifest_path(root) else {
        return BTreeMap::new();
    };
    let Ok(contents) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    contents
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(path, freshness)| (path.to_owned(), freshness.to_owned()))
        .collect()
}

fn save_workspace_manifest(root: &Path, manifest: &BTreeMap<String, String>) {
    let Some(path) = workspace_manifest_path(root) else {
        return;
    };
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let contents = manifest
        .iter()
        .map(|(path, freshness)| format!("{path}\t{freshness}\n"))
        .collect::<String>();
    let temporary = path.with_extension("tmp");
    if std::fs::write(&temporary, contents).is_ok() {
        let _ = std::fs::rename(temporary, path);
    }
}

fn typescript_import_is_type_only(import: &galaxy_core::typescript::Import) -> bool {
    import.type_only
        || (!import.bindings.is_empty() && import.bindings.iter().all(|binding| binding.type_only))
}

fn valid_javascript_identifier(name: &str) -> bool {
    let mut characters = name.bytes();
    matches!(characters.next(), Some(b'$' | b'_') | Some(b'a'..=b'z' | b'A'..=b'Z'))
        && characters.all(
            |character| matches!(character, b'$' | b'_' | b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9'),
        )
        && !matches!(
            name,
            "await"
                | "break"
                | "case"
                | "catch"
                | "class"
                | "const"
                | "continue"
                | "debugger"
                | "default"
                | "delete"
                | "do"
                | "else"
                | "enum"
                | "export"
                | "extends"
                | "false"
                | "finally"
                | "for"
                | "function"
                | "if"
                | "implements"
                | "import"
                | "in"
                | "instanceof"
                | "interface"
                | "let"
                | "new"
                | "null"
                | "package"
                | "private"
                | "protected"
                | "public"
                | "return"
                | "static"
                | "super"
                | "switch"
                | "this"
                | "throw"
                | "true"
                | "try"
                | "typeof"
                | "var"
                | "void"
                | "while"
                | "with"
                | "yield"
        )
}

fn pending_go_engine(
    engines: &Mutex<HashMap<PathBuf, Arc<Mutex<GoEngine>>>>,
    root: &Path,
) -> (Arc<Mutex<GoEngine>>, bool) {
    let mut engines = engines.lock().unwrap_or_else(|e| e.into_inner());
    match engines.get(root) {
        Some(engine) => (engine.clone(), false),
        None => {
            let engine = Arc::new(Mutex::new(GoEngine::pending(root)));
            engines.insert(root.to_path_buf(), engine.clone());
            (engine, true)
        }
    }
}

fn pending_php_engine(
    engines: &Mutex<HashMap<PathBuf, Arc<Mutex<PhpEngine>>>>,
    root: &Path,
) -> (Arc<Mutex<PhpEngine>>, bool) {
    let mut engines = engines.lock().unwrap_or_else(|e| e.into_inner());
    match engines.get(root) {
        Some(engine) => (engine.clone(), false),
        None => {
            let engine = Arc::new(Mutex::new(PhpEngine::pending(root)));
            engines.insert(root.to_path_buf(), engine.clone());
            (engine, true)
        }
    }
}

fn pending_dart_engine(
    engines: &Mutex<HashMap<PathBuf, Arc<Mutex<DartEngine>>>>,
    root: &Path,
) -> (Arc<Mutex<DartEngine>>, bool) {
    let mut engines = engines.lock().unwrap_or_else(|e| e.into_inner());
    match engines.get(root) {
        Some(engine) => (engine.clone(), false),
        None => {
            let engine = Arc::new(Mutex::new(DartEngine::pending(root)));
            engines.insert(root.to_path_buf(), engine.clone());
            (engine, true)
        }
    }
}

fn spawn_go_index(
    engine: &Arc<Mutex<GoEngine>>,
    root: &Path,
) -> Option<std::thread::JoinHandle<()>> {
    let engine = Arc::clone(engine);
    let root = root.to_path_buf();
    std::thread::Builder::new()
        .name("go-index".into())
        .spawn(move || {
            let started = Instant::now();
            let built = GoEngine::build(&root);
            let stats = {
                let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                engine.install(built);
                engine.index_stats()
            };
            log_language_index("go", &root, stats, started.elapsed());
        })
        .ok()
}

fn spawn_rust_index(
    engine: &Arc<Mutex<RustEngine>>,
    root: &Path,
) -> Option<std::thread::JoinHandle<()>> {
    let engine = Arc::clone(engine);
    let root = root.to_path_buf();
    std::thread::Builder::new()
        .name("rust-index".into())
        .spawn(move || {
            let started = Instant::now();
            let built = build_rust_index(&root);
            let stats = {
                let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                engine.install(built);
                engine.index_stats()
            };
            log_language_index("rust", &root, stats, started.elapsed());
        })
        .ok()
}

fn spawn_dart_index(
    engine: &Arc<Mutex<DartEngine>>,
    root: &Path,
) -> Option<std::thread::JoinHandle<()>> {
    let engine = Arc::clone(engine);
    let root = root.to_path_buf();
    std::thread::Builder::new()
        .name("dart-index".into())
        .spawn(move || {
            let started = Instant::now();
            let built = DartEngine::build(&root);
            let stats = {
                let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                engine.install(built);
                engine.index_stats()
            };
            log_language_index("dart", &root, stats, started.elapsed());
        })
        .ok()
}

fn is_blade_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".blade.php"))
}

fn spawn_php_index(
    engine: &Arc<Mutex<PhpEngine>>,
    root: &Path,
) -> Option<std::thread::JoinHandle<()>> {
    let engine = Arc::clone(engine);
    let root = root.to_path_buf();
    std::thread::Builder::new()
        .name("php-index".into())
        .spawn(move || {
            let started = Instant::now();
            let built = php::build(&root);
            let stats = {
                let mut engine = engine.lock().unwrap_or_else(|e| e.into_inner());
                engine.install(built);
                engine.index_stats()
            };
            log_language_index("php", &root, stats, started.elapsed());
        })
        .ok()
}

/// Rebuild every PHP project already active under this workspace. Do not make
/// a watcher event create engines for unrelated folders: opening a PHP file
/// still owns initial engine creation.
fn refresh_php_engines(engines: &EngineMap<PhpEngine>, workspace_root: &Path) {
    let active = engines
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|(root, _)| root.starts_with(workspace_root))
        .map(|(root, engine)| (root.clone(), Arc::clone(engine)))
        .collect::<Vec<_>>();
    for (root, engine) in active {
        let rebuild = engine
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .begin_rebuild();
        if rebuild {
            let _ = spawn_php_index(&engine, &root);
        }
    }
}

fn log_language_index(language: &str, root: &Path, stats: LanguageIndexStats, elapsed: Duration) {
    tracing::info!(
        language,
        root = %root.display(),
        files = stats.files,
        symbols = stats.symbols,
        imports = stats.imports,
        reverse_keys = stats.reverse_keys,
        reverse_sites = stats.reverse_sites,
        source_bytes = stats.source_bytes,
        heap_lower_bound_bytes = stats.heap_lower_bound_bytes,
        build_ms = elapsed.as_millis() as u64,
        "language index ready"
    );
}

/// An outline entry; `kind` uses the LSP `SymbolKind` numbering.
fn php_outline_symbol(
    symbol: &php::OutlineSymbol,
    uri: &str,
) -> galaxy_core::analysis::DocumentSymbolInfo {
    let kind = match symbol.kind {
        "Class" => 5,
        "Method" => 6,
        "Property" => 7,
        "Constructor" => 9,
        "Enum" => 10,
        "Interface" => 11,
        "Function" => 12,
        "Constant" => 14,
        "Variable" => 13,
        "Enum Member" => 22,
        // Traits have no kind of their own; servers report them as classes.
        _ => 5,
    };
    let position = LspRange {
        start_line: u64::from(symbol.line),
        start_character: u64::from(symbol.column),
        end_line: u64::from(symbol.line),
        end_character: u64::from(symbol.column),
    };
    galaxy_core::analysis::DocumentSymbolInfo {
        uri: Some(uri.to_owned()),
        name: symbol.name.clone(),
        kind,
        range: position,
        children: symbol
            .children
            .iter()
            .map(|child| php_outline_symbol(child, uri))
            .collect(),
    }
}

fn php_completion_item(item: php::Completion) -> LspCompletionItem {
    LspCompletionItem {
        label: item.label,
        detail: item.detail,
        insert_text: item.insert_text,
        is_snippet: item.is_snippet,
        sort_text: Some(item.sort),
        replace_from: item.replace_from.map(u64::from),
        kind: Some(match item.kind {
            php::ItemKind::Method => 2,
            php::ItemKind::Function => 3,
            php::ItemKind::Variable => 6,
            php::ItemKind::Class => 7,
            php::ItemKind::Interface => 8,
            php::ItemKind::Property => 10,
            php::ItemKind::Enum => 13,
            php::ItemKind::Keyword => 14,
            php::ItemKind::Constant => 21,
            php::ItemKind::Trait => 22,
        }),
        additional_edits: item
            .extra
            .into_iter()
            .map(|edit| LspRenameEdit {
                uri: String::new(),
                range: LspRange {
                    start_line: u64::from(edit.line),
                    start_character: u64::from(edit.column),
                    end_line: u64::from(edit.line),
                    end_character: u64::from(edit.column),
                },
                new_text: edit.text,
            })
            .collect(),
    }
}

/// One row of the file explorer.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FileEntry {
    pub name: String,
    /// Path relative to the workspace root, `/` separated.
    pub rel_path: String,
    pub is_dir: bool,
    /// Matched by a `.gitignore` rule; the explorer dims it.
    #[uniffi(default = false)]
    pub is_ignored: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct WorkspaceSearchMatch {
    pub rel_path: String,
    pub line: u64,
    pub column: u64,
    pub text: String,
    pub context_before: String,
    pub context_after: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct GitStatusInfo {
    pub branch: Option<String>,
    pub changed_paths: Vec<String>,
    pub changes: Vec<GitChangeInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct GitChangeInfo {
    pub path: String,
    pub staged: bool,
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct GitBranchInfo {
    pub name: String,
    pub current: bool,
    pub remote: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct GitVersionSuggestionInfo {
    pub version: String,
    pub previous_tag: Option<String>,
    pub commit_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum GitRefKindInfo {
    Head,
    Branch,
    Remote,
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct GitRefInfo {
    pub name: String,
    pub kind: GitRefKindInfo,
}

/// One row of the Git Graph panel; see `galaxy_core::git::GitCommit` for
/// what the lane lists mean.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct GitCommitInfo {
    pub hash: String,
    pub author: String,
    pub timestamp: i64,
    pub subject: String,
    pub refs: Vec<GitRefInfo>,
    pub lane: u32,
    pub passing: Vec<u32>,
    pub incoming: Vec<u32>,
    pub outgoing: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum GitResetModeInfo {
    Soft,
    Mixed,
    Hard,
}

/// A file that differs between two commits. `status` is git's letter:
/// A, M, D, R, C or T.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct GitFileDiffInfo {
    pub path: String,
    pub old_path: Option<String>,
    pub status: String,
}

/// The commit behind some lines of a file (inline blame).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct BlameCommitInfo {
    pub sha: String,
    pub author: String,
    pub email: String,
    /// Author time, seconds since the Unix epoch.
    pub time: i64,
    pub summary: String,
    /// Not committed yet (unsaved or uncommitted edits).
    pub uncommitted: bool,
}

/// Blame for a whole file. `line_commit[n]` indexes `commits` for 0-based
/// line `n`; `u32::MAX` means unknown.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct BlameInfo {
    pub commits: Vec<BlameCommitInfo>,
    pub line_commit: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum GitLineChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct GitLineChangeInfo {
    pub line: u64,
    pub kind: GitLineChangeKind,
}

/// A changed region between the committed and the current text, in 0-based
/// lines of each side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct GitDiffHunkInfo {
    pub old_start: u64,
    pub old_len: u64,
    pub new_start: u64,
    pub new_len: u64,
}

/// One frame of the call stack of a stopped program.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DebugFrameInfo {
    pub id: i64,
    pub name: String,
    pub path: Option<String>,
    /// 1-based.
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DebugScopeInfo {
    pub name: String,
    pub variables_reference: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DebugVariableInfo {
    pub name: String,
    pub value: String,
    pub type_name: Option<String>,
    /// Non-zero when the value has children to expand.
    pub variables_reference: i64,
}

/// Breakpoint lines (1-based) of one file.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DebugBreakpointSet {
    pub path: String,
    pub lines: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum DebugEventInfo {
    Initialized,
    Output {
        category: String,
        text: String,
    },
    Stopped {
        thread_id: i64,
        reason: String,
        description: Option<String>,
    },
    Continued,
    Terminated,
    Exited {
        code: i64,
    },
    Frames {
        thread_id: i64,
        frames: Vec<DebugFrameInfo>,
    },
    Variables {
        reference: i64,
        variables: Vec<DebugVariableInfo>,
    },
    Scopes {
        frame_id: i64,
        scopes: Vec<DebugScopeInfo>,
    },
    Evaluated {
        expression: String,
        result: String,
        ok: bool,
    },
    Error {
        message: String,
    },
    AdapterClosed,
}

impl From<galaxy_core::dap::DebugEvent> for DebugEventInfo {
    fn from(event: galaxy_core::dap::DebugEvent) -> Self {
        use galaxy_core::dap::DebugEvent as E;
        match event {
            E::Initialized => Self::Initialized,
            E::Output { category, text } => Self::Output { category, text },
            E::Stopped {
                thread_id,
                reason,
                description,
            } => Self::Stopped {
                thread_id,
                reason,
                description,
            },
            E::Continued => Self::Continued,
            E::Terminated => Self::Terminated,
            E::Exited { code } => Self::Exited { code },
            E::Frames { thread_id, frames } => Self::Frames {
                thread_id,
                frames: frames
                    .into_iter()
                    .map(|f| DebugFrameInfo {
                        id: f.id,
                        name: f.name,
                        path: f.path,
                        line: f.line,
                        column: f.column,
                    })
                    .collect(),
            },
            E::Variables {
                reference,
                variables,
            } => Self::Variables {
                reference,
                variables: variables
                    .into_iter()
                    .map(|v| DebugVariableInfo {
                        name: v.name,
                        value: v.value,
                        type_name: v.type_name,
                        variables_reference: v.variables_reference,
                    })
                    .collect(),
            },
            E::Scopes { frame_id, scopes } => Self::Scopes {
                frame_id,
                scopes: scopes
                    .into_iter()
                    .map(|s| DebugScopeInfo {
                        name: s.name,
                        variables_reference: s.variables_reference,
                    })
                    .collect(),
            },
            E::Evaluated {
                expression,
                result,
                ok,
            } => Self::Evaluated {
                expression,
                result,
                ok,
            },
            E::Error(message) => Self::Error { message },
            E::AdapterClosed => Self::AdapterClosed,
        }
    }
}

/// A running debug session. The app polls it from a timer; nothing blocks.
#[derive(uniffi::Object)]
pub struct DebugHandle {
    session: Mutex<galaxy_core::dap::DebugSession>,
}

/// Starts debugging `program`. An empty `adapter` picks Xcode's `lldb-dap`.
#[uniffi::export]
pub fn start_debug(
    adapter: Vec<String>,
    program: String,
    args: Vec<String>,
    cwd: Option<String>,
    stop_on_entry: bool,
    breakpoints: Vec<DebugBreakpointSet>,
) -> Result<Arc<DebugHandle>, CoreError> {
    let adapter = if adapter.is_empty() {
        let path = galaxy_core::dap::find_lldb_dap().ok_or_else(|| CoreError::Io {
            message: "no debug adapter found: install Xcode (lldb-dap)".to_owned(),
        })?;
        vec![path]
    } else {
        adapter
    };
    let config = galaxy_core::dap::LaunchConfig {
        program,
        args,
        cwd,
        stop_on_entry,
    };
    let breakpoints = breakpoints
        .into_iter()
        .map(|set| (set.path, set.lines))
        .collect();
    let session = galaxy_core::dap::DebugSession::start(&adapter, config, breakpoints)
        .map_err(|message| CoreError::Io { message })?;
    Ok(Arc::new(DebugHandle {
        session: Mutex::new(session),
    }))
}

#[uniffi::export]
impl DebugHandle {
    /// Events since the last call, oldest first.
    pub fn poll(&self) -> Vec<DebugEventInfo> {
        self.lock().poll().into_iter().map(Into::into).collect()
    }

    pub fn set_breakpoints(&self, path: String, lines: Vec<u32>) {
        self.lock().set_breakpoints(&path, &lines);
    }

    pub fn resume(&self) {
        self.lock().continue_();
    }

    pub fn pause(&self) {
        self.lock().pause();
    }

    pub fn step_over(&self) {
        self.lock().step_over();
    }

    pub fn step_in(&self) {
        self.lock().step_in();
    }

    pub fn step_out(&self) {
        self.lock().step_out();
    }

    pub fn request_scopes(&self, frame_id: i64) {
        self.lock().request_scopes(frame_id);
    }

    pub fn request_variables(&self, reference: i64) {
        self.lock().request_variables(reference);
    }

    pub fn evaluate(&self, expression: String, frame_id: Option<i64>) {
        self.lock().evaluate(&expression, frame_id);
    }

    /// Ends the session and the program. Safe to call twice.
    pub fn stop(&self) {
        self.lock().stop();
    }

    pub fn is_running(&self) -> bool {
        self.lock().is_running()
    }
}

impl DebugHandle {
    fn lock(&self) -> std::sync::MutexGuard<'_, galaxy_core::dap::DebugSession> {
        self.session.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[derive(uniffi::Object)]
pub struct TerminalHandle {
    session: TerminalSession,
}

/// A terminal color as the program asked for it; the app maps it to its
/// theme. `Indexed` 0–15 are the ANSI colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TerminalColorInfo {
    Default,
    Indexed { index: u8 },
    Rgb { r: u8, g: u8, b: u8 },
}

impl From<galaxy_core::terminal::TerminalColor> for TerminalColorInfo {
    fn from(color: galaxy_core::terminal::TerminalColor) -> Self {
        use galaxy_core::terminal::TerminalColor;
        match color {
            TerminalColor::Default => Self::Default,
            TerminalColor::Indexed(index) => Self::Indexed { index },
            TerminalColor::Rgb(r, g, b) => Self::Rgb { r, g, b },
        }
    }
}

/// Consecutive cells of one line with the same style.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TerminalRunInfo {
    pub column: u16,
    pub text: String,
    pub foreground: TerminalColorInfo,
    pub background: TerminalColorInfo,
    pub bold: bool,
    pub italic: bool,
    pub dim: bool,
    pub underline: bool,
    pub strikeout: bool,
    pub inverse: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TerminalLineInfo {
    /// Plain text without trailing blanks, for links and copying.
    pub text: String,
    pub runs: Vec<TerminalRunInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct TerminalCursorInfo {
    pub row: u16,
    pub column: u16,
}

/// The visible terminal screen. `version` changes whenever it may have.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TerminalScreenInfo {
    pub version: u64,
    pub columns: u16,
    pub lines: Vec<TerminalLineInfo>,
    pub cursor: Option<TerminalCursorInfo>,
    pub display_offset: u32,
    pub history_size: u32,
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub alt_screen: bool,
    pub mouse_click: bool,
    pub mouse_drag: bool,
    pub mouse_motion: bool,
    pub sgr_mouse: bool,
    pub alternate_scroll: bool,
}

#[uniffi::export]
impl TerminalHandle {
    pub fn poll_output(&self) -> String {
        self.session.take_output()
    }

    /// Cheap: compare with the last screen's `version` before asking for it.
    pub fn version(&self) -> u64 {
        self.session.version()
    }

    pub fn screen(&self) -> TerminalScreenInfo {
        let screen = self.session.screen();
        TerminalScreenInfo {
            version: screen.version,
            columns: screen.columns,
            lines: screen
                .lines
                .into_iter()
                .map(|line| TerminalLineInfo {
                    text: line.text,
                    runs: line
                        .runs
                        .into_iter()
                        .map(|run| TerminalRunInfo {
                            column: run.column,
                            text: run.text,
                            foreground: run.foreground.into(),
                            background: run.background.into(),
                            bold: run.bold,
                            italic: run.italic,
                            dim: run.dim,
                            underline: run.underline,
                            strikeout: run.strikeout,
                            inverse: run.inverse,
                        })
                        .collect(),
                })
                .collect(),
            cursor: screen
                .cursor
                .map(|(row, column)| TerminalCursorInfo { row, column }),
            display_offset: screen.display_offset,
            history_size: screen.history_size,
            app_cursor: screen.app_cursor,
            bracketed_paste: screen.bracketed_paste,
            alt_screen: screen.alt_screen,
            mouse_click: screen.mouse_click,
            mouse_drag: screen.mouse_drag,
            mouse_motion: screen.mouse_motion,
            sgr_mouse: screen.sgr_mouse,
            alternate_scroll: screen.alternate_scroll,
        }
    }

    /// Text the program copied with OSC 52 since the last call.
    pub fn take_clipboard(&self) -> Option<String> {
        self.session.take_clipboard()
    }

    /// The palette the app draws with, as `0xRRGGBB`, so programs that ask
    /// for the terminal's colors (opencode, vim) match the theme. `ansi`
    /// holds the 16 ANSI colors; a different length is ignored.
    pub fn set_colors(&self, ansi: Vec<u32>, foreground: u32, background: u32, cursor: u32) {
        let rgb = |hex: u32| ((hex >> 16) as u8, (hex >> 8) as u8, hex as u8);
        let Ok(ansi) = <[u32; 16]>::try_from(ansi) else {
            return;
        };
        self.session
            .set_colors(galaxy_core::terminal::TerminalPaletteColors {
                ansi: ansi.map(rgb),
                foreground: rgb(foreground),
                background: rgb(background),
                cursor: rgb(cursor),
            });
    }

    pub fn write(&self, input: String) -> Result<(), CoreError> {
        self.session
            .write(input.as_bytes())
            .map_err(CoreError::from)
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), CoreError> {
        self.session.resize(cols, rows).map_err(CoreError::from)
    }

    /// Scrolls through history; positive `lines` goes back.
    pub fn scroll(&self, lines: i32) {
        self.session.scroll(lines);
    }

    /// The shell's name (`zsh`), for the panel's tab list.
    pub fn shell_name(&self) -> String {
        self.session.shell_name().to_owned()
    }

    pub fn is_running(&self) -> bool {
        self.session.is_running()
    }

    pub fn kill(&self) {
        self.session.kill();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct WorkspaceSearchOptions {
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub include: String,
    pub exclude: String,
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

    /// Sets the `files.exclude` globs the explorer hides, relative to the
    /// root. A leading double star and slash matches at any depth. Returns
    /// whether the set changed.
    pub fn set_explorer_excludes(&self, globs: Vec<String>) -> bool {
        self.inner.set_explorer_excludes(globs)
    }

    /// Lists one directory (`""` is the root): folders first, then files.
    /// Shows ignored files and `.git`; only `set_explorer_excludes` hides.
    pub fn list_dir(&self, rel_path: String) -> Result<Vec<FileEntry>, CoreError> {
        Ok(self
            .inner
            .list_dir(&rel_path)?
            .into_iter()
            .map(|e| FileEntry {
                name: e.name,
                rel_path: e.rel_path,
                is_dir: e.is_dir,
                is_ignored: e.is_ignored,
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

    /// Renames `rel_path` within its folder; returns the new relative
    /// path. Never overwrites.
    pub fn rename_entry(&self, rel_path: String, new_name: String) -> Result<String, CoreError> {
        let rel = self.inner.rename(&rel_path, &new_name)?;
        tracing::info!(from = %rel_path, to = %rel, "renamed");
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
                git_changed: batch.git_changed,
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

    pub fn git_status(&self) -> Result<Option<GitStatusInfo>, CoreError> {
        Ok(self.inner.git_status()?.map(|status| GitStatusInfo {
            branch: status.branch,
            changed_paths: status.files.iter().map(|file| file.path.clone()).collect(),
            changes: status
                .files
                .into_iter()
                .map(|file| GitChangeInfo {
                    path: file.path,
                    staged: file.staged,
                    code: file.code,
                })
                .collect(),
        }))
    }

    pub fn git_branches(&self) -> Result<Vec<GitBranchInfo>, CoreError> {
        Ok(galaxy_core::git::branches(self.inner.root())?
            .into_iter()
            .map(|branch| GitBranchInfo {
                name: branch.name,
                current: branch.current,
                remote: branch.remote,
            })
            .collect())
    }

    pub fn git_log_graph(&self, limit: u32) -> Result<Vec<GitCommitInfo>, CoreError> {
        self.git_log_graph_filtered(limit, Vec::new(), Vec::new(), true)
    }

    /// The graph limited to `branches` (empty: all refs) and `authors`
    /// (empty: everyone); `include_remotes` false hides remote branches.
    pub fn git_log_graph_filtered(
        &self,
        limit: u32,
        branches: Vec<String>,
        authors: Vec<String>,
        include_remotes: bool,
    ) -> Result<Vec<GitCommitInfo>, CoreError> {
        use galaxy_core::git::{GitRefKind, LogFilter};
        let filter = LogFilter {
            branches,
            authors,
            include_remotes,
        };
        Ok(
            galaxy_core::git::log_graph_filtered(self.inner.root(), limit as usize, &filter)?
                .into_iter()
                .map(|commit| GitCommitInfo {
                    hash: commit.hash,
                    author: commit.author,
                    timestamp: commit.timestamp,
                    subject: commit.subject,
                    refs: commit
                        .refs
                        .into_iter()
                        .map(|r| GitRefInfo {
                            name: r.name,
                            kind: match r.kind {
                                GitRefKind::Head => GitRefKindInfo::Head,
                                GitRefKind::Branch => GitRefKindInfo::Branch,
                                GitRefKind::Remote => GitRefKindInfo::Remote,
                                GitRefKind::Tag => GitRefKindInfo::Tag,
                            },
                        })
                        .collect(),
                    lane: commit.lane,
                    passing: commit.passing,
                    incoming: commit.incoming,
                    outgoing: commit.outgoing,
                })
                .collect(),
        )
    }

    /// Distinct commit authors, sorted, for the graph's author filter.
    pub fn git_authors(&self) -> Result<Vec<String>, CoreError> {
        Ok(galaxy_core::git::authors(self.inner.root(), 5000)?)
    }

    /// Tags on `origin`; `None` when there is no `origin`. Contacts the
    /// remote, so call it off the main thread.
    pub fn git_remote_tags(&self) -> Result<Option<Vec<String>>, CoreError> {
        Ok(
            galaxy_core::git::remote_tags(self.inner.root())?
                .map(|tags| tags.into_iter().collect()),
        )
    }

    pub fn git_delete_tag(&self, name: String) -> Result<(), CoreError> {
        galaxy_core::git::delete_tag(self.inner.root(), &name)?;
        Ok(())
    }

    pub fn git_delete_remote_tag(&self, name: String) -> Result<(), CoreError> {
        galaxy_core::git::delete_remote_tag(self.inner.root(), &name)?;
        Ok(())
    }

    pub fn git_push_tag(&self, name: String) -> Result<(), CoreError> {
        galaxy_core::git::push_tag(self.inner.root(), &name)?;
        Ok(())
    }

    pub fn git_suggest_version(&self, hash: String) -> Result<GitVersionSuggestionInfo, CoreError> {
        let suggestion = galaxy_core::git::suggest_version(self.inner.root(), &hash)?;
        Ok(GitVersionSuggestionInfo {
            version: suggestion.version,
            previous_tag: suggestion.previous_tag,
            commit_count: suggestion.commit_count,
        })
    }

    pub fn git_cherry_pick(&self, hash: String) -> Result<(), CoreError> {
        galaxy_core::git::cherry_pick(self.inner.root(), &hash)?;
        Ok(())
    }

    pub fn git_revert(&self, hash: String) -> Result<(), CoreError> {
        galaxy_core::git::revert(self.inner.root(), &hash)?;
        Ok(())
    }

    pub fn git_commit_fixup(&self, hash: String) -> Result<(), CoreError> {
        galaxy_core::git::commit_fixup(self.inner.root(), &hash)?;
        Ok(())
    }

    pub fn git_commit_squash(&self, hash: String) -> Result<(), CoreError> {
        galaxy_core::git::commit_squash(self.inner.root(), &hash)?;
        Ok(())
    }

    pub fn git_reset_last_commit_soft(&self) -> Result<(), CoreError> {
        galaxy_core::git::reset_last_commit_soft(self.inner.root())?;
        Ok(())
    }

    pub fn git_reset_to(&self, hash: String, mode: GitResetModeInfo) -> Result<(), CoreError> {
        use galaxy_core::git::GitResetMode;
        let mode = match mode {
            GitResetModeInfo::Soft => GitResetMode::Soft,
            GitResetModeInfo::Mixed => GitResetMode::Mixed,
            GitResetModeInfo::Hard => GitResetMode::Hard,
        };
        galaxy_core::git::reset_to(self.inner.root(), &hash, mode)?;
        Ok(())
    }

    pub fn git_merge(&self, hash: String) -> Result<(), CoreError> {
        galaxy_core::git::merge(self.inner.root(), &hash)?;
        Ok(())
    }

    pub fn git_rebase_onto(&self, hash: String) -> Result<(), CoreError> {
        galaxy_core::git::rebase_onto(self.inner.root(), &hash)?;
        Ok(())
    }

    pub fn git_drop_commit(&self, hash: String) -> Result<(), CoreError> {
        galaxy_core::git::drop_commit(self.inner.root(), &hash)?;
        Ok(())
    }

    /// Whether a remote-tracking branch already holds the commit (local refs
    /// only, no network).
    pub fn git_is_on_remote(&self, hash: String) -> Result<bool, CoreError> {
        Ok(galaxy_core::git::is_on_remote(self.inner.root(), &hash)?)
    }

    pub fn git_compare_files(
        &self,
        from: String,
        to: String,
    ) -> Result<Vec<GitFileDiffInfo>, CoreError> {
        Ok(
            galaxy_core::git::compare_files(self.inner.root(), &from, &to)?
                .into_iter()
                .map(|file| GitFileDiffInfo {
                    path: file.path,
                    old_path: file.old_path,
                    status: file.status.to_string(),
                })
                .collect(),
        )
    }

    pub fn git_compare_diff(
        &self,
        from: String,
        to: String,
        path: String,
        old_path: Option<String>,
    ) -> Result<String, CoreError> {
        Ok(galaxy_core::git::compare_diff(
            self.inner.root(),
            &from,
            &to,
            &path,
            old_path.as_deref(),
        )?)
    }

    pub fn git_checkout_commit(&self, hash: String) -> Result<(), CoreError> {
        galaxy_core::git::checkout_commit(self.inner.root(), &hash)?;
        Ok(())
    }

    pub fn git_create_branch_at(&self, name: String, hash: String) -> Result<(), CoreError> {
        galaxy_core::git::create_branch_at(self.inner.root(), &name, &hash)?;
        Ok(())
    }

    pub fn git_create_tag(&self, name: String, hash: String) -> Result<(), CoreError> {
        galaxy_core::git::create_tag(self.inner.root(), &name, &hash)?;
        Ok(())
    }

    pub fn git_create_annotated_tag(
        &self,
        name: String,
        hash: String,
        message: String,
    ) -> Result<(), CoreError> {
        galaxy_core::git::create_annotated_tag(self.inner.root(), &name, &hash, &message)?;
        Ok(())
    }

    pub fn git_checkout(&self, name: String) -> Result<(), CoreError> {
        galaxy_core::git::checkout(self.inner.root(), &name)?;
        Ok(())
    }

    pub fn git_create_branch(&self, name: String) -> Result<(), CoreError> {
        galaxy_core::git::create_branch(self.inner.root(), &name)?;
        Ok(())
    }

    pub fn git_stage(&self, path: String) -> Result<(), CoreError> {
        galaxy_core::git::stage(self.inner.root(), &path)?;
        Ok(())
    }

    pub fn git_stage_all(&self) -> Result<(), CoreError> {
        galaxy_core::git::stage_all(self.inner.root())?;
        Ok(())
    }

    pub fn git_unstage(&self, path: String) -> Result<(), CoreError> {
        galaxy_core::git::unstage(self.inner.root(), &path)?;
        Ok(())
    }

    pub fn git_unstage_all(&self) -> Result<(), CoreError> {
        galaxy_core::git::unstage_all(self.inner.root())?;
        Ok(())
    }

    pub fn git_commit(&self, message: String) -> Result<(), CoreError> {
        galaxy_core::git::commit(self.inner.root(), &message)?;
        Ok(())
    }

    pub fn terminal(&self, cols: u16, rows: u16) -> Result<Arc<TerminalHandle>, CoreError> {
        Ok(Arc::new(TerminalHandle {
            session: TerminalSession::spawn(self.inner.root(), cols, rows)?,
        }))
    }

    /// The file as committed at `HEAD`, as a document for the left side of a
    /// diff. `None` when HEAD has no such file. Its path is the real one so
    /// the language (and so the highlighting) matches; nothing saves it.
    pub fn git_base_document(
        &self,
        rel_path: String,
    ) -> Result<Option<Arc<DocumentHandle>>, CoreError> {
        let Some(bytes) = galaxy_core::git::head_blob(self.inner.root(), &rel_path)? else {
            return Ok(None);
        };
        let doc = Document::from_bytes(&bytes)?;
        let path = self.inner.root().join(&rel_path);
        Ok(Some(Arc::new(DocumentHandle::new(doc, path))))
    }

    /// The staged version of a file, or `None` when it is absent from the index.
    pub fn git_index_document(
        &self,
        rel_path: String,
    ) -> Result<Option<Arc<DocumentHandle>>, CoreError> {
        let Some(bytes) = galaxy_core::git::index_blob(self.inner.root(), &rel_path)? else {
            return Ok(None);
        };
        let doc = Document::from_bytes(&bytes)?;
        let path = self.inner.root().join(&rel_path);
        Ok(Some(Arc::new(DocumentHandle::new(doc, path))))
    }

    /// Changed regions between `HEAD` and `text` for `rel_path`. A file that
    /// is not in HEAD is one inserted region.
    pub fn git_diff_hunks(
        &self,
        rel_path: String,
        text: String,
    ) -> Result<Vec<GitDiffHunkInfo>, CoreError> {
        let base = galaxy_core::git::head_blob(self.inner.root(), &rel_path)?.unwrap_or_default();
        let base = String::from_utf8_lossy(&base);
        Ok(galaxy_core::git::diff_hunks(&base, &text)
            .into_iter()
            .map(|h| GitDiffHunkInfo {
                old_start: h.old_start as u64,
                old_len: h.old_len as u64,
                new_start: h.new_start as u64,
                new_len: h.new_len as u64,
            })
            .collect())
    }

    /// Diff two snapshots supplied by the caller (HEAD/index/working tree).
    pub fn git_diff_hunks_between(
        &self,
        old_text: String,
        new_text: String,
    ) -> Vec<GitDiffHunkInfo> {
        galaxy_core::git::diff_hunks(&old_text, &new_text)
            .into_iter()
            .map(|h| GitDiffHunkInfo {
                old_start: h.old_start as u64,
                old_len: h.old_len as u64,
                new_start: h.new_start as u64,
                new_len: h.new_len as u64,
            })
            .collect()
    }

    /// Stages the region of `rel_path` that starts at `old_start` in `HEAD`
    /// (see `git_diff_hunks`), with `text` as the working content. The other
    /// changes stay unstaged. Does nothing when that region is gone.
    pub fn git_stage_hunk(
        &self,
        rel_path: String,
        text: String,
        old_start: u64,
    ) -> Result<(), CoreError> {
        let base = galaxy_core::git::head_blob(self.inner.root(), &rel_path)?.unwrap_or_default();
        let base = String::from_utf8_lossy(&base);
        let Some(hunk) = galaxy_core::git::diff_hunks(&base, &text)
            .into_iter()
            .find(|h| h.old_start as u64 == old_start)
        else {
            return Ok(());
        };
        galaxy_core::git::stage_hunk(self.inner.root(), &rel_path, &text, hunk)?;
        Ok(())
    }

    /// Stage a hunk relative to the current index, not HEAD.
    pub fn git_stage_index_hunk(
        &self,
        rel_path: String,
        text: String,
        old_start: u64,
    ) -> Result<(), CoreError> {
        let base = galaxy_core::git::index_blob(self.inner.root(), &rel_path)?.unwrap_or_default();
        let base = String::from_utf8_lossy(&base);
        let Some(hunk) = galaxy_core::git::diff_hunks(&base, &text)
            .into_iter()
            .find(|h| h.old_start as u64 == old_start)
        else {
            return Ok(());
        };
        galaxy_core::git::stage_index_hunk(self.inner.root(), &rel_path, &base, &text, hunk)?;
        Ok(())
    }

    /// Who last changed each line of `rel_path`, counting `text` as the
    /// current content. `None` for untracked files and outside a repository.
    pub fn git_blame(
        &self,
        rel_path: String,
        text: String,
    ) -> Result<Option<BlameInfo>, CoreError> {
        Ok(
            galaxy_core::git::blame(self.inner.root(), &rel_path, &text)?.map(|blame| BlameInfo {
                commits: blame
                    .commits
                    .into_iter()
                    .map(|c| BlameCommitInfo {
                        sha: c.sha,
                        author: c.author,
                        email: c.email,
                        time: c.time,
                        summary: c.summary,
                        uncommitted: c.uncommitted,
                    })
                    .collect(),
                line_commit: blame.line_commit,
            }),
        )
    }

    pub fn git_diff_lines(
        &self,
        rel_path: String,
        text: String,
    ) -> Result<Vec<GitLineChangeInfo>, CoreError> {
        Ok(
            galaxy_core::git::diff_lines(self.inner.root(), &rel_path, &text)?
                .into_iter()
                .map(|change| GitLineChangeInfo {
                    line: change.line as u64,
                    kind: match change.kind {
                        galaxy_core::git::GitLineChangeKind::Added => GitLineChangeKind::Added,
                        galaxy_core::git::GitLineChangeKind::Modified => {
                            GitLineChangeKind::Modified
                        }
                        galaxy_core::git::GitLineChangeKind::Deleted => GitLineChangeKind::Deleted,
                    },
                })
                .collect(),
        )
    }

    pub fn search(
        &self,
        query: String,
        options: WorkspaceSearchOptions,
        limit: u64,
    ) -> Result<Vec<WorkspaceSearchMatch>, CoreError> {
        Ok(self
            .inner
            .search(
                &query,
                &CoreSearchOptions {
                    regex: options.regex,
                    case_sensitive: options.case_sensitive,
                    whole_word: options.whole_word,
                    include: options.include,
                    exclude: options.exclude,
                },
                limit as usize,
            )?
            .into_iter()
            .map(|m: CoreSearchMatch| WorkspaceSearchMatch {
                rel_path: m.rel_path,
                line: m.line as u64,
                column: m.column as u64,
                text: m.text,
                context_before: m.context_before,
                context_after: m.context_after,
            })
            .collect())
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
    /// Repository metadata changed; refresh branch, upstream, and status.
    pub git_changed: bool,
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

/// On-disk text encoding, mirrored from `galaxy_core::document::Encoding`.
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

/// Line ending style, mirrored from `galaxy_core::document::LineEnding`.
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

/// A visual row: a logical line and the row within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct RowPosition {
    pub line: u64,
    /// Row within the line, 0 for its first.
    pub row: u64,
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
    ToggleComment,
    MoveLinesUp,
    MoveLinesDown,
    CopyLinesUp,
    CopyLinesDown,
    DeleteLines,
    JoinLines,
    UpperCase,
    LowerCase,
    TrimTrailingWhitespace,
    SortLinesAscending,
    SortLinesDescending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct LineSpan {
    pub start: u64,
    pub end: u64,
    pub includes_eol: bool,
}

/// A diagnostic with a UTF-16 range, supplied by the built-in engines to a
/// document.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DiagnosticInfo {
    pub start_line: u64,
    pub start_character: u64,
    pub end_line: u64,
    pub end_character: u64,
    /// LSP severity: 1 error, 2 warning, 3 information, 4 hint.
    pub severity: u8,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
    /// LSP `Unnecessary` tag: unused code, drawn faded.
    pub unnecessary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DiagnosticSpan {
    pub start: u64,
    pub end: u64,
    pub severity: u8,
    pub message: String,
    pub unnecessary: bool,
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
    /// Other places the identifier at the caret (or the selected one)
    /// appears on this line, UTF-16 columns. Empty with several selections.
    pub occurrences: Vec<LineSpan>,
    /// `text` is only the start of a very long line (see `display_line`).
    pub truncated: bool,
    /// Tab-expanded leading indentation columns for vertical guides.
    pub indent_guides: Vec<u64>,
    /// Matching bracket cells on this line, when the primary caret is beside one.
    pub brackets: Vec<LineSpan>,
    pub diagnostics: Vec<DiagnosticSpan>,
    /// A fold marker belongs in the gutter (set for folded lines too).
    pub foldable: bool,
    /// The lines below are folded away.
    pub folded: bool,
}

/// What the gutter shows for a line's fold marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FoldState {
    /// Nothing to fold here.
    Plain,
    /// Can be folded; currently open.
    Expanded,
    /// Folded; its body is hidden.
    Folded,
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
    fn from_summary(sum: galaxy_core::find::Summary) -> Self {
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

fn cursor_info(c: viewport::CursorInfo) -> CursorInfo {
    CursorInfo {
        line: c.line as u64,
        col: c.col as u64,
        col_utf16: c.col_utf16 as u64,
    }
}

/// Cursor and document metadata without any line content.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct StatusSnapshot {
    pub revision: u64,
    pub line_count: u64,
    pub primary: CursorInfo,
    pub selection_count: u64,
}

/// One semantic syntax style range on a document line. Columns are UTF-16,
/// matching `ViewportLine.text`; `scope` is a stable theme key (M3-04).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct StyleRun {
    pub line: u64,
    pub start: u64,
    pub end: u64,
    pub scope: String,
}

/// Everything the editor view needs to draw one frame. Columns are UTF-16.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ViewportSnapshot {
    pub revision: u64,
    pub line_count: u64,
    pub lines: Vec<ViewportLine>,
    pub primary: CursorInfo,
    pub selection_count: u64,
    /// Visible syntax plus a small scroll margin. Empty until M3-06 attaches
    /// a language parser to each document.
    pub styles: Vec<StyleRun>,
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
                    occurrences: Vec::new(),
                    truncated: l.truncated,
                    indent_guides: l.indent_guides.into_iter().map(|c| c as u64).collect(),
                    brackets: l
                        .brackets
                        .into_iter()
                        .map(|sp| LineSpan {
                            start: sp.start as u64,
                            end: sp.end as u64,
                            includes_eol: sp.includes_eol,
                        })
                        .collect(),
                    diagnostics: l
                        .diagnostics
                        .into_iter()
                        .map(|span| DiagnosticSpan {
                            start: span.start as u64,
                            end: span.end as u64,
                            severity: span.severity,
                            message: String::new(),
                            unnecessary: span.unnecessary,
                        })
                        .collect(),
                    foldable: false,
                    folded: false,
                })
                .collect(),
            primary: cursor_info(s.primary),
            selection_count: s.selection_count as u64,
            styles: s
                .styles
                .into_iter()
                .map(|run| StyleRun {
                    line: run.line as u64,
                    start: run.start as u64,
                    end: run.end as u64,
                    scope: run.scope.name().to_owned(),
                })
                .collect(),
        }
    }
}

struct State {
    editor: Editor,
    path: PathBuf,
    indent: IndentConfig,
    /// Add the closing tag when `>` or `</` is typed (HTML, PHP, JSX).
    auto_close_tags: bool,
    /// Rename a tag's partner when its name is edited.
    auto_rename_tags: bool,
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
    syntax: Option<SyntaxState>,
    diagnostics: Vec<DiagnosticInfo>,
    diagnostics_revision: u64,
    /// Widest line in columns and the revision it was measured at.
    max_columns: Option<(u64, usize)>,
    /// Soft wrap width in columns; `None` while wrap is off.
    wrap_columns: Option<usize>,
    /// Row table for `wrap_columns`, rebuilt when the text or width changes.
    wrap_map: Option<galaxy_core::wrap::WrapMap>,
    /// Room the inline parameter labels take in some lines; rows leave it free.
    wrap_extras: galaxy_core::wrap::Extras,
    /// Folded line ranges; their lines take no rows.
    folds: FoldSet,
}

struct SyntaxState {
    language: SyntaxLanguage,
    service: SyntaxService,
    snapshot: Option<SyntaxSnapshot>,
    /// Last document revision handed to the worker.
    submitted: u64,
}

impl SyntaxState {
    fn new(language: SyntaxLanguage, doc: &Document) -> Self {
        let service = SyntaxService::for_syntax(language);
        service.submit_document(doc);
        Self {
            language,
            service,
            snapshot: None,
            submitted: doc.revision(),
        }
    }

    fn submit(&mut self, doc: &Document) {
        self.service.submit_document(doc);
        self.submitted = doc.revision();
    }

    /// Submits the document if any edit path (undo, paste, IME, ...) changed
    /// it without telling the worker. Cheap: Rope clones are O(1).
    fn ensure_submitted(&mut self, doc: &Document) {
        if self.submitted != doc.revision() {
            self.submit(doc);
        }
    }

    fn drain(&mut self) {
        if let Some(snapshot) = self.service.poll() {
            self.snapshot = Some(snapshot);
        }
    }

    /// Tree for exactly `doc`'s revision.
    fn poll(&mut self, doc: &Document) -> Option<&SyntaxSnapshot> {
        self.ensure_submitted(doc);
        self.drain();
        let revision = doc.revision();
        self.snapshot
            .as_ref()
            .filter(|snapshot| snapshot.revision() == revision)
    }

    /// Newest tree, possibly one edit behind. Drawing with it keeps colors
    /// on screen while the worker reparses, instead of a plain-text frame
    /// on every keystroke.
    fn latest(&mut self, doc: &Document) -> Option<&SyntaxSnapshot> {
        self.ensure_submitted(doc);
        self.drain();
        self.snapshot.as_ref()
    }
}

impl State {
    /// Moves folds along with edits and unfolds any that hides a caret.
    fn fold_sync(&mut self) {
        self.folds.sync(&self.editor.doc);
        if self.folds.is_empty() {
            return;
        }
        let doc = &self.editor.doc;
        let stuck: Vec<usize> = self
            .editor
            .sel
            .iter()
            .map(|sel| doc.char_to_line(sel.head))
            .filter(|&line| self.folds.is_hidden(line))
            .collect();
        for line in stuck {
            self.folds.reveal(line);
        }
    }

    /// The row table for the current text, wrap width and folds, rebuilt if
    /// stale. `None` while lines are neither wrapped nor folded.
    fn wrap(&mut self) -> Option<&galaxy_core::wrap::WrapMap> {
        self.fold_sync();
        let columns = self.wrap_columns;
        if columns.is_none() && self.folds.is_empty() {
            self.wrap_map = None;
            return None;
        }
        let stale = self
            .wrap_map
            .as_ref()
            .is_none_or(|m| !m.is_current(&self.editor.doc, columns, self.folds.hidden_ranges()));
        if stale {
            let map = galaxy_core::wrap::WrapMap::build_with_extras(
                &self.editor.doc,
                columns,
                self.folds.hidden_ranges().to_vec(),
                &self.wrap_extras,
            );
            self.wrap_map = Some(map);
        }
        self.wrap_map.as_ref()
    }

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

    /// Detected Tree-sitter language, if this is a supported non-large file.
    pub fn syntax_language(&self) -> Option<String> {
        self.lock()
            .syntax
            .as_ref()
            .map(|syntax| syntax.language.name().to_owned())
    }

    /// Built-in snippets for this document's file type. The UI filters these
    /// by the word immediately before the caret and merges them with engine
    /// completions.
    pub fn snippet_completions(&self) -> Vec<CompletionInfo> {
        let path = self.path();
        snippets::snippets_for_path(&path)
            .into_iter()
            .map(|snippet| CompletionInfo {
                label: snippet.prefix.to_owned(),
                detail: Some(format!("Snippet: {}", snippet.description)),
                insert_text: snippet.body.to_owned(),
                is_snippet: true,
                additional_edits: Vec::new(),
                replace_from: None,
                kind: Some(15),
            })
            .collect()
    }

    /// Polls the background parser without blocking. `true` means styles for
    /// the current document revision are ready for the next viewport draw.
    pub fn poll_syntax(&self) -> bool {
        let st = &mut *self.lock();
        st.syntax
            .as_mut()
            .and_then(|syntax| syntax.poll(&st.editor.doc))
            .is_some()
    }

    /// Stops the parser worker and releases its completed tree. Document text,
    /// selections and history remain resident; `resume_syntax` recreates it.
    pub fn suspend_syntax(&self) -> bool {
        self.lock().syntax.take().is_some()
    }

    /// Recreates syntax parsing for a supported non-large document. Returns
    /// whether a worker is active after the call.
    pub fn resume_syntax(&self) -> bool {
        let mut st = self.lock();
        if st.syntax.is_none() {
            st.syntax = syntax_for(&st.path, &st.editor.doc);
        }
        st.syntax.is_some()
    }

    /// Whether this handle currently owns a syntax worker and completed tree.
    pub fn syntax_active(&self) -> bool {
        self.lock().syntax.is_some()
    }

    /// Called for every document on each analysis poll; bumps
    /// [`Self::diagnostics_revision`] only when the list actually changed.
    pub fn set_diagnostics(&self, diagnostics: Vec<DiagnosticInfo>) {
        let mut st = self.lock();
        if st.diagnostics != diagnostics {
            st.diagnostics = diagnostics;
            st.diagnostics_revision += 1;
        }
    }

    /// Changes whenever diagnostics change, so the UI can skip redraws.
    pub fn diagnostics_revision(&self) -> u64 {
        self.lock().diagnostics_revision
    }

    /// Returns the diagnostic covering this UTF-16 position, if any.
    pub fn diagnostic_at(&self, line: u64, character: u64) -> Option<DiagnosticInfo> {
        self.lock()
            .diagnostics
            .iter()
            .find(|diagnostic| {
                (diagnostic.start_line, diagnostic.start_character) <= (line, character)
                    && (line, character) <= (diagnostic.end_line, diagnostic.end_character)
            })
            .cloned()
    }

    /// Definitions from the current completed syntax tree. Parsing stays on
    /// the syntax worker; callers receive an empty list while it catches up.
    pub fn outline(&self) -> Vec<OutlineSymbol> {
        let st = &mut *self.lock();
        let (tree, language) = {
            let Some(syntax) = st.syntax.as_mut() else {
                return Vec::new();
            };
            let Some(snapshot) = syntax.poll(&st.editor.doc) else {
                return Vec::new();
            };
            (snapshot.tree().clone(), syntax.language)
        };
        syntax::outline_symbols(&st.editor.doc, &tree, language)
            .into_iter()
            .map(|symbol| OutlineSymbol {
                uri: None,
                name: symbol.name,
                kind: symbol.kind,
                line: symbol.line as u64,
                column: symbol.column_utf16 as u64,
                depth: 0,
            })
            .collect()
    }

    /// Whether the position is plain text between tags (JSX, HTML or PHP
    /// inline HTML) instead of code, using the newest completed syntax tree.
    /// `false` while no tree exists yet.
    pub fn is_markup_text(&self, line: u64, col_utf16: u64) -> bool {
        let st = &mut *self.lock();
        let pos = to_char(&st.editor.doc, TextPos { line, col_utf16 });
        let Some(syntax) = st.syntax.as_mut() else {
            return false;
        };
        let language = syntax.language;
        let Some(snapshot) = syntax.latest(&st.editor.doc) else {
            return false;
        };
        let tree = snapshot.tree().clone();
        syntax::is_markup_text(&st.editor.doc, &tree, language, pos)
    }

    /// Plain lines without selection info.
    pub fn lines(&self, first: u64, count: u64) -> Vec<String> {
        self.lock()
            .editor
            .doc
            .lines(clamp_usize(first), clamp_usize(count))
    }

    /// Primary caret only. Cheap: no lines, styles or diagnostics are built,
    /// so use this instead of `viewport(0, 0)` when only the cursor matters.
    pub fn primary_cursor(&self) -> CursorInfo {
        cursor_info(self.lock().editor.primary_cursor())
    }

    /// Cursor plus document metadata for the status bar and scroll geometry,
    /// without building a viewport snapshot.
    pub fn status_snapshot(&self) -> StatusSnapshot {
        let st = self.lock();
        StatusSnapshot {
            revision: st.editor.doc.revision(),
            line_count: st.editor.doc.line_count() as u64,
            primary: cursor_info(st.editor.primary_cursor()),
            selection_count: st.editor.sel.len() as u64,
        }
    }

    /// One snapshot per frame: visible lines, selections, carets, cursor.
    pub fn viewport(&self, first_line: u64, count: u64) -> ViewportSnapshot {
        let st = &mut *self.lock();
        st.fold_sync();
        let first_line = clamp_usize(first_line);
        let count = clamp_usize(count);
        // A tree one edit behind is fine for colors: offsets are clipped to
        // the current text and the fresh tree arrives a frame or two later.
        let syntax = st.syntax.as_mut().and_then(|syntax| {
            let language = syntax.language;
            syntax
                .latest(&st.editor.doc)
                .map(|snapshot| (language, snapshot.tree().clone()))
        });
        // Folded lines are skipped, not built and thrown away: a folded class
        // can hide thousands of lines between two visible rows.
        let end = first_line.saturating_add(count);
        let mut segments: Vec<(usize, usize)> = Vec::new();
        let mut at = first_line;
        for &(hidden_first, hidden_last) in st.folds.hidden_ranges() {
            if hidden_last < at {
                continue;
            }
            if hidden_first >= end {
                break;
            }
            if hidden_first > at {
                segments.push((at, hidden_first - at));
            }
            at = hidden_last + 1;
        }
        if at < end {
            segments.push((at, end - at));
        }
        let mut merged: Option<ViewportSnapshot> = None;
        for (start, len) in segments {
            let part: ViewportSnapshot = match &syntax {
                Some((language, tree)) => st
                    .editor
                    .viewport_with_syntax(start, len, tree, *language)
                    .into(),
                None => st.editor.viewport(start, len).into(),
            };
            match merged.as_mut() {
                None => merged = Some(part),
                Some(all) => {
                    all.lines.extend(part.lines);
                    all.styles.extend(part.styles);
                }
            }
        }
        let mut snap = merged.unwrap_or_else(|| st.editor.viewport(first_line, 0).into());
        // Same identifier elsewhere, marked while the caret rests on one.
        // Not while searching (find highlights already say "these") or with
        // several carets, where it would only add noise.
        if st.finder.is_none() && st.editor.sel.len() == 1 {
            let key = movement::symbol_at(&st.editor.doc, st.editor.sel.primary());
            if let Some(key) = key {
                for line in &mut snap.lines {
                    line.occurrences = movement::symbol_spans(&line.text, &key)
                        .into_iter()
                        .map(|(start, end)| LineSpan {
                            start: start as u64,
                            end: end as u64,
                            includes_eol: false,
                        })
                        .collect();
                }
            }
        }
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
        for line in &mut snap.lines {
            for diagnostic in &st.diagnostics {
                if line.line < diagnostic.start_line || line.line > diagnostic.end_line {
                    continue;
                }
                let mut start = if line.line == diagnostic.start_line {
                    diagnostic.start_character
                } else {
                    0
                };
                let mut end = if line.line == diagnostic.end_line {
                    diagnostic.end_character
                } else {
                    line.text.encode_utf16().count() as u64
                };
                // A missing token (`;` left out) has no width. Mark the
                // character at it, or the one before it at the end of the line.
                if start == end
                    && diagnostic.start_line == diagnostic.end_line
                    && diagnostic.start_character == diagnostic.end_character
                {
                    if start < line.text.encode_utf16().count() as u64 {
                        end = start + 1;
                    } else {
                        start = start.saturating_sub(1);
                    }
                }
                if start < end {
                    line.diagnostics.push(DiagnosticSpan {
                        start,
                        end,
                        severity: diagnostic.severity,
                        message: diagnostic.message.clone(),
                        unnecessary: diagnostic.unnecessary,
                    });
                }
            }
        }
        let doc = &st.editor.doc;
        let folds = &st.folds;
        for line in &mut snap.lines {
            let at = line.line as usize;
            line.folded = folds.is_folded(at);
            line.foldable = line.folded || fold::is_foldable(doc, at);
        }
        snap
    }

    /// Large-file mode (> 20 MB): the UI shows a banner and skips live
    /// find counts.
    pub fn is_large(&self) -> bool {
        self.lock().editor.doc.is_large()
    }

    /// Columns the widest line takes, so the editor can size its horizontal
    /// scroll range before long lines are drawn. Measured once per
    /// revision. `0` for a document too big to scan.
    pub fn max_line_columns(&self) -> u64 {
        let mut st = self.lock();
        let revision = st.editor.doc.revision();
        if let Some((measured, columns)) = st.max_columns
            && measured == revision
        {
            return columns as u64;
        }
        let columns = galaxy_core::wrap::max_line_columns(&st.editor.doc).unwrap_or(0);
        st.max_columns = Some((revision, columns));
        columns as u64
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

    pub fn text(&self) -> String {
        self.lock().editor.doc.text()
    }

    /// `true` when the text differs from what was last loaded or saved.
    /// Undoing (or redoing) back to the saved text clears it again.
    pub fn is_modified(&self) -> bool {
        let st = self.lock();
        st.deleted_on_disk || st.has_unsaved_edits()
    }

    /// `true` when the text itself has unsaved edits. Unlike `is_modified`,
    /// a file that was only deleted on disk does not count.
    pub fn has_unsaved_text(&self) -> bool {
        self.lock().has_unsaved_edits()
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
        st.syntax = syntax_for(&st.path, &st.editor.doc);
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

    /// Turns the HTML/JSX tag helpers on or off (`editor.autoClosingTags`,
    /// `editor.linkedEditing`).
    pub fn set_tag_editing(&self, auto_close: bool, auto_rename: bool) {
        let mut st = self.lock();
        st.auto_close_tags = auto_close;
        st.auto_rename_tags = auto_rename;
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
        let path = st.path.to_string_lossy().into_owned();
        let comment_marker = st
            .syntax
            .as_ref()
            .and_then(|syntax| syntax.language.comment_marker_for(&path));
        let flavor = st
            .syntax
            .as_ref()
            .and_then(|syntax| galaxy_core::tags::Flavor::for_language(syntax.language));
        let close_flavor = flavor.filter(|_| st.auto_close_tags);
        let rename_flavor = flavor.filter(|_| st.auto_rename_tags);
        {
            let ed = &mut st.editor;
            let (doc, sel) = (&ed.doc, &ed.sel);
            let rename = |tx: edit::Transaction| match rename_flavor {
                Some(flavor) => edit::with_linked_rename(doc, sel, tx, flavor),
                None => tx,
            };
            let (tx, kind) = match &command {
                EditCommand::InsertText { text } => {
                    let character =
                        (text.chars().count() == 1).then(|| text.chars().next().unwrap());
                    let tag_tx = close_flavor.and_then(|flavor| match character {
                        Some('>') => edit::close_tag_gt(doc, sel, flavor),
                        Some('/') => edit::close_tag_slash(doc, sel, flavor),
                        _ => None,
                    });
                    if let Some(tx) = tag_tx {
                        (tx, EditKind::Other)
                    } else if let Some(open) = character.filter(|c| edit::pair_for(*c).is_some()) {
                        (edit::insert_pair(doc, sel, open), EditKind::Other)
                    } else if let Some(close) =
                        character.filter(|c| matches!(c, ')' | ']' | '}' | '\'' | '"'))
                    {
                        (
                            edit::skip_closing(doc, sel, close)
                                .unwrap_or_else(|| edit::insert_text(doc, sel, text)),
                            insert_kind(text),
                        )
                    } else {
                        (rename(edit::insert_text(doc, sel, text)), insert_kind(text))
                    }
                }
                EditCommand::Paste { text } => (edit::paste(doc, sel, text), EditKind::Other),
                EditCommand::Backspace => {
                    (rename(edit::backspace(doc, sel, &cfg)), EditKind::Delete)
                }
                EditCommand::DeleteForward => {
                    (rename(edit::delete_forward(doc, sel)), EditKind::Delete)
                }
                EditCommand::Newline => (edit::newline(doc, sel, &cfg), EditKind::Other),
                EditCommand::Indent => (edit::indent(doc, sel, &cfg), EditKind::Other),
                EditCommand::Outdent => (edit::outdent(doc, sel, &cfg), EditKind::Other),
                EditCommand::ToggleComment => match comment_marker {
                    Some(marker) => (edit::toggle_comment(doc, sel, marker), EditKind::Other),
                    None => return,
                },
                EditCommand::MoveLinesUp => (edit::move_lines(doc, sel, true), EditKind::Other),
                EditCommand::MoveLinesDown => (edit::move_lines(doc, sel, false), EditKind::Other),
                EditCommand::CopyLinesUp => (edit::copy_lines(doc, sel, true), EditKind::Other),
                EditCommand::CopyLinesDown => (edit::copy_lines(doc, sel, false), EditKind::Other),
                EditCommand::DeleteLines => (edit::delete_lines(doc, sel), EditKind::Other),
                EditCommand::JoinLines => (edit::join_lines(doc, sel), EditKind::Other),
                EditCommand::UpperCase => (edit::transform_case(doc, sel, true), EditKind::Other),
                EditCommand::LowerCase => (edit::transform_case(doc, sel, false), EditKind::Other),
                EditCommand::TrimTrailingWhitespace => {
                    (edit::trim_trailing_whitespace(doc, sel), EditKind::Other)
                }
                EditCommand::SortLinesAscending => {
                    (edit::sort_lines(doc, sel, false), EditKind::Other)
                }
                EditCommand::SortLinesDescending => {
                    (edit::sort_lines(doc, sel, true), EditKind::Other)
                }
            };
            ed.apply(tx, kind, Instant::now());
        }
        let st = &mut *st;
        if let Some(syntax) = st.syntax.as_mut() {
            syntax.submit(&st.editor.doc);
        }
    }

    // --- Navigation and selection (M1-11) ---

    /// Arrow keys, Home/End, Page Up/Down. `extend` is the Shift key.
    pub fn move_cursors(&self, motion: CursorMotion, extend: bool) {
        let mut st = self.lock();
        st.fold_sync();
        let columns = st.wrap_columns;
        let st = &mut *st;
        let ed = &mut st.editor;
        let mut next = match columns {
            Some(columns) => movement::move_selections_wrapped_with(
                &ed.doc,
                &ed.sel,
                motion.into(),
                extend,
                columns,
                &st.wrap_extras,
            ),
            None => movement::move_selections(&ed.doc, &ed.sel, motion.into(), extend),
        };
        if !st.folds.is_empty() {
            // Up and Down step over folded lines instead of opening them.
            let doc = &ed.doc;
            let folds = &st.folds;
            let down = matches!(motion, CursorMotion::Down | CursorMotion::PageDown { .. });
            next.transform(doc, |sel| {
                let line = doc.char_to_line(sel.head);
                let Some((first, last)) = folds.hidden_at(line) else {
                    return sel;
                };
                let target = if down {
                    (last + 1 < doc.line_count()).then_some(last + 1)
                } else {
                    first.checked_sub(1)
                };
                let Some(target) = target else { return sel };
                let column = sel.head - doc.line_to_char(line);
                let head = (doc.line_to_char(target) + column).min(doc.line_end_char(target));
                if extend {
                    sel.extend_to(head)
                } else {
                    Selection::caret(head)
                }
            });
        }
        ed.set_selections(next);
        ed.history.break_group();
    }

    /// Turns soft wrap on at `columns` monospace columns, or off with
    /// `None`. Row queries below answer in logical lines while it is off.
    pub fn set_wrap_columns(&self, columns: Option<u32>) {
        let mut st = self.lock();
        let columns = columns.map(|c| (c as usize).max(galaxy_core::wrap::MIN_COLUMNS));
        if st.wrap_columns != columns {
            st.wrap_columns = columns;
            st.wrap_map = None;
        }
    }

    /// Room the inline labels take, so rows wrap with them: for each line
    /// (by displayed text) the UTF-16 columns of the labelled arguments and
    /// the monospace columns each label needs. Replaces the previous set.
    pub fn set_wrap_hints(&self, lines: Vec<WrapHintLine>) {
        let extras = galaxy_core::wrap::Extras::new(lines.into_iter().map(|line| {
            let at = line
                .slots
                .iter()
                .map(|slot| (clamp_usize(slot.column), clamp_usize(slot.columns)))
                .collect();
            (line.text, at)
        }));
        let mut st = self.lock();
        if st.wrap_extras != extras {
            st.wrap_extras = extras;
            st.wrap_map = None;
        }
    }

    /// Visual rows in the document: `line_count` while wrap is off.
    pub fn total_rows(&self) -> u64 {
        let mut st = self.lock();
        let lines = st.editor.doc.line_count() as u64;
        st.wrap().map_or(lines, |m| m.total_rows())
    }

    /// First visual row of `line`.
    pub fn row_of_line(&self, line: u64) -> u64 {
        let mut st = self.lock();
        let line = clamp_usize(line);
        st.wrap().map_or(line as u64, |m| m.row_of_line(line))
    }

    /// Visual rows `line` takes.
    pub fn rows_of_line(&self, line: u64) -> u64 {
        let mut st = self.lock();
        let line = clamp_usize(line);
        st.wrap().map_or(1, |m| m.rows_of_line(line))
    }

    /// The line and row within it that a global visual row falls on.
    pub fn line_at_row(&self, row: u64) -> RowPosition {
        let mut st = self.lock();
        let last = st.editor.doc.line_count().saturating_sub(1) as u64;
        match st.wrap() {
            Some(m) => {
                let (line, in_line) = m.line_at_row(row);
                RowPosition {
                    line: line as u64,
                    row: in_line as u64,
                }
            }
            None => RowPosition {
                line: row.min(last),
                row: 0,
            },
        }
    }

    /// UTF-16 columns, within `line`, where rows after its first start.
    /// Empty while wrap is off or when the line fits one row.
    pub fn line_breaks(&self, line: u64) -> Vec<u64> {
        let st = self.lock();
        let Some(columns) = st.wrap_columns else {
            return Vec::new();
        };
        let doc = &st.editor.doc;
        let line = clamp_usize(line).min(doc.line_count().saturating_sub(1));
        let start = doc.line_to_char(line);
        let base = doc.char_to_utf16_offset(start);
        galaxy_core::wrap::breaks_for_line_with(doc, line, columns, &st.wrap_extras)
            .into_iter()
            .map(|at| (doc.char_to_utf16_offset(start + at) - base) as u64)
            .collect()
    }

    /// Folds `line`, or unfolds it if it is folded. `false` when the line
    /// has nothing to fold. A caret inside the folded lines moves to the
    /// header.
    pub fn toggle_fold(&self, line: u64) -> bool {
        let mut st = self.lock();
        let st = &mut *st;
        let line = clamp_usize(line);
        if !st.folds.toggle(&st.editor.doc, line) {
            return false;
        }
        let doc = &st.editor.doc;
        let folds = &st.folds;
        if folds.is_empty() {
            return true;
        }
        let header_end = |at: usize| doc.line_end_char(at);
        let mut next = st.editor.sel.clone();
        next.transform(doc, |sel| {
            let moved = |pos: usize| {
                let at = doc.char_to_line(pos);
                match folds.hidden_at(at) {
                    Some((first, _)) => header_end(first - 1),
                    None => pos,
                }
            };
            let (anchor, head) = (moved(sel.anchor), moved(sel.head));
            if (anchor, head) == (sel.anchor, sel.head) {
                sel
            } else {
                Selection::new(anchor, head)
            }
        });
        st.editor.set_selections(next);
        true
    }

    /// Unfolds everything, for views that need one row per line.
    pub fn unfold_all(&self) {
        self.lock().folds.clear();
    }

    /// Header lines of the blocks around `line`, outermost first: what a
    /// sticky bar can pin while `line` is just below it.
    pub fn sticky_scopes(&self, line: u64, max: u64) -> Vec<u64> {
        let st = self.lock();
        fold::enclosing_headers(&st.editor.doc, clamp_usize(line), clamp_usize(max))
            .into_iter()
            .map(|l| l as u64)
            .collect()
    }

    /// Fold marker state of `line`.
    pub fn fold_state(&self, line: u64) -> FoldState {
        let mut st = self.lock();
        st.fold_sync();
        let line = clamp_usize(line);
        if st.folds.is_folded(line) {
            FoldState::Folded
        } else if fold::is_foldable(&st.editor.doc, line) {
            FoldState::Expanded
        } else {
            FoldState::Plain
        }
    }

    /// Any folded range exists. Row geometry differs from plain lines then.
    pub fn has_folds(&self) -> bool {
        let mut st = self.lock();
        st.fold_sync();
        !st.folds.is_empty()
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

    pub fn apply_rename_edits(&self, edits: Vec<RenameEdit>) -> bool {
        let mut st = self.lock();
        let doc = &st.editor.doc;
        let doc_len = doc.len_chars();
        let changes = edits
            .into_iter()
            .map(|edit| {
                let start = doc.utf16_to_char(galaxy_core::position::Utf16Position::new(
                    edit.start_line as usize,
                    edit.start_character as usize,
                ));
                let end = doc.utf16_to_char(galaxy_core::position::Utf16Position::new(
                    edit.end_line as usize,
                    edit.end_character as usize,
                ));
                Change::new(start, end, edit.new_text)
            })
            .collect::<Vec<_>>();
        let tx = ChangeSet::new(changes, doc_len);
        if tx.is_empty() {
            return false;
        }
        let transaction = galaxy_core::edit::from_changes(tx, doc);
        st.editor
            .apply(transaction, EditKind::Other, Instant::now());
        let st = &mut *st;
        if let Some(syntax) = st.syntax.as_mut() {
            syntax.submit(&st.editor.doc);
        }
        true
    }

    pub fn apply_formatting_edits(&self, edits: Vec<RenameEdit>) -> bool {
        self.apply_rename_edits(edits)
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

    /// The file was renamed or moved (by the explorer): follow it. The
    /// content is unchanged, so the disk stamp still applies.
    pub fn set_path(&self, path: String) {
        let mut st = self.lock();
        st.path = PathBuf::from(path);
        st.deleted_on_disk = false;
        st.syntax = syntax_for(&st.path, &st.editor.doc);
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
        st.syntax = syntax_for(&st.path, &st.editor.doc);
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
        let syntax = syntax_for(&path, &doc);
        Self {
            state: Mutex::new(State {
                disk: None,
                deleted_on_disk: false,
                saved_state: 0,
                editor: Editor::new(doc),
                path,
                indent: IndentConfig::default(),
                auto_close_tags: true,
                auto_rename_tags: true,
                finder: None,
                find_error: None,
                syntax,
                diagnostics: Vec::new(),
                diagnostics_revision: 0,
                max_columns: None,
                wrap_columns: None,
                wrap_map: None,
                wrap_extras: galaxy_core::wrap::Extras::default(),
                folds: FoldSet::default(),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn syntax_for(path: &Path, doc: &Document) -> Option<SyntaxState> {
    if doc.is_large() {
        return None;
    }
    let first_line = doc.lines(0, 1).into_iter().next().unwrap_or_default();
    SyntaxLanguage::detect(&path.to_string_lossy(), &first_line)
        .map(|language| SyntaxState::new(language, doc))
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
mod typescript_integration_tests;

#[cfg(test)]
mod typescript_navigation_integration_tests;

#[cfg(test)]
mod php_integration_tests;

#[cfg(test)]
mod engine_integration_tests;

#[cfg(test)]
mod document_editor_integration_tests;

#[cfg(test)]
mod workspace_integration_tests;

#[cfg(test)]
mod core_integration_tests;
