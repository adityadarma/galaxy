//! UniFFI bridge exposed to Swift.
//!
//! Design rule: the UI calls in with coarse-grained commands and receives
//! batched events through `CoreListener`. No per-character FFI chatter.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use galaxy_core::analysis::{
    BuiltinStore, CompletionItem as LspCompletionItem, DefinitionLocation as LspDefinitionLocation,
    HoverInfo as LspHoverInfo, Range as LspRange, RenameEdit as LspRenameEdit,
};
use galaxy_core::change::{Change, ChangeSet};
use galaxy_core::clipboard;
use galaxy_core::commands::{self, CommandRegistry};
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
use galaxy_core::php::{self, PhpEngine};
use galaxy_core::position::Utf16Position;
use galaxy_core::selection::{Selection, SelectionSet};
use galaxy_core::snippets;
use galaxy_core::syntax::{self, SyntaxLanguage, SyntaxService, SyntaxSnapshot};
use galaxy_core::terminal::TerminalSession;
use galaxy_core::typescript::{
    DeclarationKind, DocumentIndex, ImportBinding, ImportKind, TypeMemberTarget,
};
use galaxy_core::viewport;
use galaxy_core::watcher::WorkspaceWatcher;
use galaxy_core::workspace::{
    SearchMatch as CoreSearchMatch, SearchOptions as CoreSearchOptions, Workspace as CoreWorkspace,
};

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
    WorkspaceIndexFinished {
        root: String,
        total: u64,
        changed: u64,
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
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LspOutlineSymbol {
    pub uri: Option<String>,
    pub name: String,
    pub kind: String,
    pub line: u64,
    pub column: u64,
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
    analysis: Mutex<BuiltinStore>,
    /// Built-in PHP engines, one per project root.
    php: Arc<Mutex<HashMap<PathBuf, Arc<Mutex<PhpEngine>>>>>,
    /// Built-in Go engines, one per project root.
    go: Arc<Mutex<HashMap<PathBuf, Arc<Mutex<GoEngine>>>>>,
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
    /// Parsed disk documents and successful module resolutions used by TypeScript navigation.
    typescript_navigation: Arc<Mutex<TypeScriptNavigationCache>>,
}

impl Default for Core {
    fn default() -> Self {
        Self {
            listener: RwLock::new(None),
            analysis: Mutex::new(BuiltinStore::default()),
            php: Arc::new(Mutex::new(HashMap::new())),
            go: Arc::new(Mutex::new(HashMap::new())),
            php_documents: Mutex::new(HashMap::new()),
            php_checked: Mutex::new(HashMap::new()),
            typescript_checked: Mutex::new(HashMap::new()),
            typescript_navigation: Arc::new(Mutex::new(TypeScriptNavigationCache::default())),
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
    /// This is metadata only; TypeScript semantic requests remain LSP-owned.
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
        spawn_workspace_indexing(
            ws.clone(),
            Arc::clone(&self.typescript_navigation),
            Arc::clone(&self.go),
            Arc::clone(&self.php),
            self.listener
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        );
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
            || is_typescript_path(path))
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
                self.typescript_checked
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&uri);
            }
            open.iter()
                .filter_map(|(uri, weak)| Some((uri.clone(), weak.upgrade()?)))
                .collect()
        };
        self.check_php(&mut analysis, &documents);
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
        if is_typescript_path(Path::new(&path)) {
            let index = DocumentIndex::parse(&path, &document.lock().editor.doc.text());
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
        let engine = if is_go_path(Path::new(&path)) {
            self.go_engine(Path::new(&path)).map(Engine::Go)
        } else {
            self.php_engine_any(Path::new(&path)).map(Engine::Php)
        };
        if let Some(engine) = engine {
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
        let path = document.path();
        if path.is_empty() {
            return false;
        }
        let uri = file_uri(Path::new(&path));
        if is_typescript_path(Path::new(&path)) {
            let text = document.lock().editor.doc.text();
            let items = typescript_completions(&path, &text, line as usize, character as usize);
            let answered = !items.is_empty();
            self.analysis
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .set_completion(&uri, items);
            return answered;
        }
        let engine = if is_go_path(Path::new(&path)) {
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
            self.analysis
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .set_completion(&uri, items);
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
        let engine = if is_go_path(Path::new(&path)) {
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
        if is_typescript_path(Path::new(&path)) {
            return self.typescript_definition(&document, &uri, line, character);
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
        if is_typescript_path(Path::new(&path)) {
            return self.typescript_references(&document, &uri, line, character);
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
        if is_typescript_path(Path::new(&path)) {
            return self.typescript_rename(&document, &uri, line, character, &new_name);
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
        if !(is_go_path(path) || is_php_path(path) || is_blade_path(path)) {
            return false;
        }
        let uri = file_uri(path);
        let engine = if is_go_path(path) {
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
            let answered = !actions.is_empty();
            self.analysis
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .set_code_actions(&uri, actions);
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
        if is_typescript_path(Path::new(&path)) {
            let index = DocumentIndex::parse(&path, &document.lock().editor.doc.text());
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
        let engine = if is_go_path(Path::new(&path)) {
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
        ) {
            for symbol in source {
                output.push(LspOutlineSymbol {
                    uri: symbol.uri.clone(),
                    name: symbol.name.clone(),
                    kind: symbol.kind.to_string(),
                    line: symbol.range.start_line,
                    column: symbol.range.start_character,
                });
                flatten(&symbol.children, output);
            }
        }
        let mut output = Vec::new();
        let analysis = self.analysis.lock().unwrap_or_else(|e| e.into_inner());
        flatten(analysis.document_symbols(&uri), &mut output);
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
}

impl Engine {
    fn is_ready(&self) -> bool {
        match self {
            Engine::Php(engine) => engine.lock().unwrap_or_else(|e| e.into_inner()).is_ready(),
            Engine::Go(engine) => engine.lock().unwrap_or_else(|e| e.into_inner()).is_ready(),
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
        }
    }

    fn outline(&self, path: &Path, text: &str) -> Vec<LangOutlineSymbol> {
        match self {
            Engine::Php(_) => php::outline(path, text),
            Engine::Go(_) => go::outline(path, text),
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
        for (uri, document) in documents {
            let path = document.path();
            if !is_typescript_path(Path::new(&path)) {
                continue;
            }
            let (revision, text) = {
                let state = document.lock();
                (state.editor.doc.revision(), state.editor.doc.text())
            };
            if checked.get(uri) == Some(&revision) {
                continue;
            }
            let index = DocumentIndex::parse(&path, &text);
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
    /// open buffers where a file is open. `None` leaves the request to the
    /// language servers.
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
        let usages = if is_go_path(Path::new(&document.path())) {
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
        let index = DocumentIndex::parse(&path, &document.lock().editor.doc.text());
        let mut locations = typescript_type_target(
            &self.typescript_navigation,
            &path,
            &index,
            line as usize,
            character as usize,
        )
        .map(|target| vec![target])
        .unwrap_or_default();
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
        if locations.is_empty() {
            locations = typescript_import_target(
                &self.typescript_navigation,
                &path,
                &index,
                line as usize,
                character as usize,
            )
            .map(|(target, _)| vec![target])
            .unwrap_or_default();
        }
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
                .unwrap_or_else(|| module_root_location(&target)),
            );
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
            locations = typescript_type_member_target(
                &self.typescript_navigation,
                &path,
                &index,
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
            && let Some(specifier) = index.static_module_at(line as usize, character as usize)
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
        let found = !locations.is_empty();
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_definitions(uri, locations);
        found
    }

    fn typescript_references(
        &self,
        document: &Arc<DocumentHandle>,
        uri: &str,
        line: u64,
        character: u64,
    ) -> bool {
        let path = document.path();
        let index = DocumentIndex::parse(&path, &document.lock().editor.doc.text());
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
        if locations.is_empty()
            && let Some((target, occurrences)) = typescript_import_target(
                &self.typescript_navigation,
                &path,
                &index,
                line as usize,
                character as usize,
            )
        {
            locations.push(target);
            locations.extend(occurrences.into_iter().map(|range| LspDefinitionLocation {
                uri: uri.to_owned(),
                range: typescript_range(range),
            }));
        }
        let found = !locations.is_empty();
        self.analysis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_references(uri, locations);
        found
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
        let index = DocumentIndex::parse(&path, &document.lock().editor.doc.text());
        let edits: Vec<LspRenameEdit> = if valid_javascript_identifier(new_name) {
            index
                .symbol_at(line as usize, character as usize)
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

    /// Answers go-to-definition from the PHP engine; `false` leaves the
    /// request to the language servers.
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
}

fn is_php_path(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    name.ends_with(".php") && !name.ends_with(".blade.php")
}

fn is_go_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("go"))
}

fn is_typescript_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs")
    )
}

fn typescript_range(range: galaxy_core::typescript::Utf16Range) -> LspRange {
    LspRange {
        start_line: range.start.line as u64,
        start_character: range.start.col as u64,
        end_line: range.end.line as u64,
        end_character: range.end.col as u64,
    }
}

const MAX_TYPESCRIPT_NAVIGATION_CACHE_ENTRIES: usize = 256;
const MAX_TYPESCRIPT_WARM_SOURCE_CANDIDATES: usize = 5000;
const MAX_TYPESCRIPT_WARM_IMPORT_TARGETS: usize = 512;
const TYPESCRIPT_WARM_EXCLUDED_DIRECTORIES: &[&str] = &[
    "node_modules",
    ".git",
    ".hg",
    ".svn",
    "target",
    "dist",
    "build",
    ".next",
    "coverage",
    "vendor",
];

#[derive(Clone, PartialEq, Eq)]
struct FileFreshness {
    modified: Option<SystemTime>,
    len: u64,
}

impl FileFreshness {
    fn read(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(Self {
            modified: metadata.modified().ok(),
            len: metadata.len(),
        })
    }
}

#[derive(Clone)]
struct CachedTypeScriptIndex {
    freshness: FileFreshness,
    index: Arc<DocumentIndex>,
}

#[derive(Clone)]
struct CachedTypeScriptResolution {
    target: PathBuf,
    dependencies: Vec<(PathBuf, Option<FileFreshness>)>,
}

#[derive(Default)]
struct TypeScriptNavigationCache {
    indexes: HashMap<PathBuf, CachedTypeScriptIndex>,
    resolutions: HashMap<(PathBuf, String), CachedTypeScriptResolution>,
}

impl TypeScriptNavigationCache {
    fn index(&mut self, path: &Path) -> Option<Arc<DocumentIndex>> {
        let path = module_identity(path);
        let freshness = FileFreshness::read(&path)?;
        if let Some(cached) = self.indexes.get(&path)
            && cached.freshness == freshness
        {
            return Some(Arc::clone(&cached.index));
        }
        let text = std::fs::read_to_string(&path).ok()?;
        let index = Arc::new(DocumentIndex::parse(&path, &text));
        if self.indexes.len() >= MAX_TYPESCRIPT_NAVIGATION_CACHE_ENTRIES {
            self.indexes.clear();
        }
        self.indexes.insert(
            path,
            CachedTypeScriptIndex {
                freshness,
                index: Arc::clone(&index),
            },
        );
        Some(index)
    }

    fn resolve(&mut self, importer: &Path, specifier: &str) -> Option<PathBuf> {
        let importer = module_identity(importer);
        let key = (importer.clone(), specifier.to_owned());
        if let Some(cached) = self.resolutions.get(&key)
            && cached.target.is_file()
            && cached
                .dependencies
                .iter()
                .all(|(path, freshness)| FileFreshness::read(path) == *freshness)
        {
            return Some(cached.target.clone());
        }
        self.resolutions.remove(&key);
        let target = resolve_typescript_import_uncached(&importer, specifier)?;
        let target = prefer_typescript_declaration(target);
        if !target.is_file() {
            return None;
        }
        let dependencies = typescript_resolution_dependencies(&importer, specifier);
        if self.resolutions.len() >= MAX_TYPESCRIPT_NAVIGATION_CACHE_ENTRIES {
            self.resolutions.clear();
        }
        self.resolutions.insert(
            key,
            CachedTypeScriptResolution {
                target: target.clone(),
                dependencies,
            },
        );
        Some(target)
    }
}

fn spawn_workspace_indexing(
    workspace: CoreWorkspace,
    cache: Arc<Mutex<TypeScriptNavigationCache>>,
    go_engines: Arc<Mutex<HashMap<PathBuf, Arc<Mutex<GoEngine>>>>>,
    php_engines: Arc<Mutex<HashMap<PathBuf, Arc<Mutex<PhpEngine>>>>>,
    listener: Option<Arc<dyn CoreListener>>,
) {
    let root = workspace.root().to_path_buf();
    let _ = std::thread::Builder::new()
        .name("workspace-indexing".into())
        .spawn(move || {
            send_workspace_index_started(&listener, &root);
            let (total, changed) = refresh_workspace_index(&workspace, &listener);
            let mut workers = Vec::new();
            if typescript_workspace_detected(&root) {
                workers.push(spawn_typescript_navigation_warmup(&root, cache));
            }
            if let Some(project) = go::GoProject::detect(&root) {
                let project_root = project.root().to_path_buf();
                let (engine, fresh) = pending_go_engine(&go_engines, &project_root);
                if fresh {
                    workers.push(spawn_go_index(&engine, &project_root));
                }
            }
            if root.join("composer.json").is_file() {
                let (engine, fresh) = pending_php_engine(&php_engines, &root);
                if fresh {
                    workers.push(spawn_php_index(&engine, &root));
                }
            }
            for worker in workers.into_iter().flatten() {
                let _ = worker.join();
            }
            send_workspace_index_finished(&listener, &root, total, changed);
        });
}

fn send_workspace_index_started(listener: &Option<Arc<dyn CoreListener>>, root: &Path) {
    if let Some(listener) = listener {
        listener.on_event(CoreEvent::WorkspaceIndexStarted {
            root: root.to_string_lossy().into_owned(),
            total: 0,
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

/// One manifest is shared by every language service. It records the current
/// workspace file set and freshness, while language engines own their semantic
/// payloads. Reopening a workspace always walks it again and updates this
/// cache incrementally, so additions and removals are never missed.
fn refresh_workspace_index(
    workspace: &CoreWorkspace,
    listener: &Option<Arc<dyn CoreListener>>,
) -> (u64, u64) {
    let root = workspace.root();
    let root_text = root.to_string_lossy().into_owned();
    let paths = workspace.build_file_index().paths().collect::<Vec<_>>();
    let total = paths.len() as u64;
    if let Some(listener) = listener {
        listener.on_event(CoreEvent::WorkspaceIndexProgress {
            root: root_text.clone(),
            completed: 0,
            total,
        });
    }
    let previous = load_workspace_manifest(root);
    let mut current = BTreeMap::new();
    let mut changed = 0_u64;
    for (offset, path) in paths.iter().enumerate() {
        let Some(freshness) = workspace_file_freshness(&root.join(path)) else {
            continue;
        };
        if previous.get(path) != Some(&freshness) {
            changed += 1;
        }
        current.insert(path.clone(), freshness);
        let completed = (offset + 1) as u64;
        if (completed == total || completed.is_multiple_of(32))
            && let Some(listener) = listener
        {
            listener.on_event(CoreEvent::WorkspaceIndexProgress {
                root: root_text.clone(),
                completed,
                total,
            });
        }
    }
    changed += previous
        .keys()
        .filter(|path| !current.contains_key(*path))
        .count() as u64;
    save_workspace_manifest(root, &current);
    (total, changed)
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

/// A JavaScript package or an explicit JS/TS project configuration opts the
/// workspace into the JavaScript/TypeScript navigation warm-up.
fn typescript_workspace_detected(root: &Path) -> bool {
    ["package.json", "tsconfig.json", "jsconfig.json"]
        .into_iter()
        .any(|name| root.join(name).is_file())
}

fn spawn_typescript_navigation_warmup(
    root: &Path,
    cache: Arc<Mutex<TypeScriptNavigationCache>>,
) -> Option<std::thread::JoinHandle<()>> {
    let root = root.to_path_buf();
    std::thread::Builder::new()
        .name("typescript-navigation-warmup".into())
        // Keep opening a workspace instant while bounding work in large repositories.
        .spawn(move || warm_typescript_navigation(&root, &cache))
        .ok()
}

fn warm_typescript_navigation(root: &Path, cache: &Mutex<TypeScriptNavigationCache>) {
    let mut sources = vec![root.to_path_buf()];
    let mut seen_imports = HashSet::new();
    let mut candidates = 0;
    let mut warmed = 0;

    while let Some(path) = sources.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if !typescript_warm_excluded_directory(&path)
                    && sources.len() < MAX_TYPESCRIPT_WARM_SOURCE_CANDIDATES
                {
                    sources.push(path);
                }
                continue;
            }
            if !file_type.is_file() || !is_typescript_path(&path) {
                continue;
            }
            candidates += 1;
            if candidates > MAX_TYPESCRIPT_WARM_SOURCE_CANDIDATES {
                return;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let index = DocumentIndex::parse(&path, &text);
            let specifiers = index
                .imports
                .iter()
                .filter(|import| index.import_is_syntax_valid(import))
                .map(|import| import.specifier.as_str())
                .chain(
                    index
                        .exports
                        .iter()
                        .filter_map(|export| export.specifier.as_deref()),
                );
            for specifier in specifiers {
                if warmed == MAX_TYPESCRIPT_WARM_IMPORT_TARGETS {
                    return;
                }
                let importer = module_identity(&path);
                if !seen_imports.insert((importer, specifier.to_owned())) {
                    continue;
                }
                let Some(target) = resolve_typescript_definition_import(cache, &path, specifier)
                else {
                    continue;
                };
                if !is_typescript_path(&target) {
                    continue;
                }
                warmed += 1;
                warm_typescript_declaration_chain(
                    cache,
                    target,
                    0,
                    &mut warmed,
                    &mut HashSet::new(),
                );
            }
        }
    }
}

fn typescript_warm_excluded_directory(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| TYPESCRIPT_WARM_EXCLUDED_DIRECTORIES.contains(&name))
}

fn warm_typescript_declaration_chain(
    cache: &Mutex<TypeScriptNavigationCache>,
    path: PathBuf,
    depth: usize,
    warmed: &mut usize,
    visited: &mut HashSet<PathBuf>,
) {
    if depth == MAX_TYPESCRIPT_REEXPORT_DEPTH
        || *warmed == MAX_TYPESCRIPT_WARM_IMPORT_TARGETS
        || !visited.insert(module_identity(&path))
    {
        return;
    }
    let Some(index) = cache.lock().unwrap_or_else(|e| e.into_inner()).index(&path) else {
        return;
    };
    let specifiers = index
        .exports
        .iter()
        .filter_map(|export| export.specifier.as_deref())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    for specifier in specifiers {
        if *warmed == MAX_TYPESCRIPT_WARM_IMPORT_TARGETS {
            return;
        }
        if let Some(target) = resolve_typescript_definition_import(cache, &path, &specifier) {
            if !is_typescript_path(&target) {
                continue;
            }
            *warmed += 1;
            warm_typescript_declaration_chain(cache, target, depth + 1, warmed, visited);
        }
    }
}

fn typescript_resolution_dependencies(
    importer: &Path,
    specifier: &str,
) -> Vec<(PathBuf, Option<FileFreshness>)> {
    let mut dependencies = vec![(importer.to_path_buf(), FileFreshness::read(importer))];
    let mut directory = importer.parent();
    while let Some(path) = directory {
        for name in ["package.json", "tsconfig.json"] {
            let dependency = path.join(name);
            dependencies.push((dependency.clone(), FileFreshness::read(&dependency)));
        }
        directory = path.parent();
    }
    if !javascript::is_relative_typescript_specifier(specifier) && !specifier.starts_with("node:") {
        let package = specifier
            .split('/')
            .take(if specifier.starts_with('@') { 2 } else { 1 })
            .collect::<Vec<_>>()
            .join("/");
        let mut directory = importer.parent();
        while let Some(path) = directory {
            let manifest = path
                .join("node_modules")
                .join(&package)
                .join("package.json");
            dependencies.push((manifest.clone(), FileFreshness::read(&manifest)));
            directory = path.parent();
        }
    }
    dependencies
}

fn prefer_typescript_declaration(target: PathBuf) -> PathBuf {
    let declaration = target.with_extension("d.ts");
    if target
        .extension()
        .is_some_and(|extension| extension == "js")
        && declaration.is_file()
    {
        declaration
    } else {
        javascript::package_types_declaration(&target).unwrap_or(target)
    }
}

fn typescript_import_target(
    cache: &Mutex<TypeScriptNavigationCache>,
    importer_path: &str,
    index: &DocumentIndex,
    line: usize,
    character: usize,
) -> Option<(
    LspDefinitionLocation,
    Vec<galaxy_core::typescript::Utf16Range>,
)> {
    let (import, binding, occurrences) = index.imports.iter().find_map(|import| {
        index
            .import_is_syntax_valid(import)
            .then_some(import)
            .and_then(|import| {
                import.bindings.iter().find_map(|binding| {
                    let position = Utf16Position::new(line, character);
                    let occurrences = index.import_binding_occurrences(binding).unwrap_or(
                        galaxy_core::typescript::SymbolOccurrences {
                            name: binding.local_name.clone(),
                            kind: DeclarationKind::Variable,
                            declaration: binding.range,
                            occurrences: vec![binding.range],
                        },
                    );
                    (binding.range.start <= position && position < binding.range.end
                        || occurrences
                            .occurrences
                            .iter()
                            .any(|range| range.start <= position && position < range.end))
                    .then_some((import, binding, occurrences))
                })
            })
    })?;
    let imported_name = direct_imported_name(binding)?;
    let importer = Path::new(importer_path);
    let target_path = resolve_typescript_definition_import(cache, importer, &import.specifier)?;
    if !is_typescript_path(&target_path) {
        return None;
    }
    let location = resolve_typescript_export(cache, target_path.clone(), imported_name)
        .map(|(path, declaration)| LspDefinitionLocation {
            uri: file_uri(&path),
            range: typescript_range(declaration.range),
        })
        .unwrap_or_else(|| module_root_location(&target_path));
    Some((location, occurrences.occurrences))
}

fn typescript_type_target(
    cache: &Mutex<TypeScriptNavigationCache>,
    importer_path: &str,
    index: &DocumentIndex,
    line: usize,
    character: usize,
) -> Option<LspDefinitionLocation> {
    let position = Utf16Position::new(line, character);
    let (import, binding) = index.imports.iter().find_map(|import| {
        index
            .import_is_syntax_valid(import)
            .then_some(import)
            .and_then(|import| {
                import
                    .bindings
                    .iter()
                    .find(|binding| {
                        binding.type_only
                            && (binding.range.start <= position && position < binding.range.end
                                || index.import_binding_occurrences(binding).is_some_and(
                                    |occurrences| {
                                        occurrences.occurrences.iter().any(|range| {
                                            range.start <= position && position < range.end
                                        })
                                    },
                                ))
                    })
                    .map(|binding| (import, binding))
            })
    })?;
    let imported_name = binding.imported_name.as_deref()?;
    if !binding.type_only || !matches!(binding.kind, ImportKind::Named | ImportKind::Default) {
        return None;
    }
    let target =
        resolve_typescript_definition_import(cache, Path::new(importer_path), &import.specifier)?;
    Some(
        resolve_typescript_type_export(cache, target.clone(), imported_name)
            .or_else(|| resolve_typescript_export(cache, target.clone(), imported_name))
            .map(|(path, declaration)| LspDefinitionLocation {
                uri: file_uri(&path),
                range: typescript_range(declaration.range),
            })
            .unwrap_or_else(|| module_root_location(&target)),
    )
}

fn typescript_type_member_target(
    cache: &Mutex<TypeScriptNavigationCache>,
    importer_path: &str,
    index: &DocumentIndex,
    line: usize,
    character: usize,
) -> Option<LspDefinitionLocation> {
    let usage = index.type_member_at(line, character)?;
    resolve_typescript_type_member_target(
        cache,
        importer_path,
        index,
        &usage.target,
        &usage.member_name,
    )
}

fn resolve_typescript_type_member_target(
    cache: &Mutex<TypeScriptNavigationCache>,
    importer_path: &str,
    index: &DocumentIndex,
    target: &TypeMemberTarget,
    member_name: &str,
) -> Option<LspDefinitionLocation> {
    match target {
        TypeMemberTarget::Inline(members) => {
            unique_type_member_location(members, member_name, Path::new(importer_path))
        }
        TypeMemberTarget::Named(range) => {
            let member = index
                .type_declaration_at(*range)?
                .iter()
                .filter(|member| member.name == member_name)
                .collect::<Vec<_>>();
            let [member] = member.as_slice() else {
                return None;
            };
            Some(LspDefinitionLocation {
                uri: file_uri(Path::new(importer_path)),
                range: typescript_range(member.range),
            })
        }
        TypeMemberTarget::Imported(range) => {
            let (import, binding) = index.import_binding_by_range(*range)?;
            if !binding.type_only
                || !matches!(binding.kind, ImportKind::Named | ImportKind::Default)
            {
                return None;
            }
            let imported_name = binding.imported_name.as_deref()?;
            let target = resolve_typescript_definition_import(
                cache,
                Path::new(importer_path),
                &import.specifier,
            )?;
            let (path, declaration) = resolve_typescript_type_export(cache, target, imported_name)?;
            let target_index = cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .index(&path)?;
            let member = target_index.type_member(&declaration.name, member_name)?;
            Some(LspDefinitionLocation {
                uri: file_uri(&path),
                range: typescript_range(member.range),
            })
        }
        TypeMemberTarget::SuperImported(range) => {
            let (import, binding) = index.import_binding_by_range(*range)?;
            if binding.type_only || !matches!(binding.kind, ImportKind::Named | ImportKind::Default)
            {
                return None;
            }
            let imported_name = binding.imported_name.as_deref()?;
            let target = resolve_typescript_definition_import(
                cache,
                Path::new(importer_path),
                &import.specifier,
            )?;
            let (path, declaration) = resolve_typescript_export(cache, target, imported_name)?;
            if declaration.kind != DeclarationKind::Class {
                return None;
            }
            let target_index = cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .index(&path)?;
            let member = target_index.type_member(&declaration.name, member_name)?;
            Some(LspDefinitionLocation {
                uri: file_uri(&path),
                range: typescript_range(member.range),
            })
        }
        TypeMemberTarget::SuperMember {
            binding,
            member_name: property,
        } => {
            let (import, binding) = index.import_binding_by_range(*binding)?;
            let imported_name = binding.imported_name.as_deref()?;
            let superclass_path = resolve_typescript_definition_import(
                cache,
                Path::new(importer_path),
                &import.specifier,
            )?;
            let (superclass_path, superclass) =
                resolve_typescript_export(cache, superclass_path, imported_name)?;
            let superclass_index = cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .index(&superclass_path)?;
            let property = superclass_index.type_member(&superclass.name, property)?;
            let (namespace, type_name) = property.type_reference.as_ref()?;
            let type_import = superclass_index.imports.iter().find(|import| {
                import
                    .bindings
                    .iter()
                    .any(|binding| binding.local_name == *namespace)
            })?;
            let target = resolve_typescript_definition_import(
                cache,
                &superclass_path,
                &type_import.specifier,
            )?;
            let target_index = cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .index(&target)?;
            let member = target_index.type_member(type_name, member_name)?;
            Some(LspDefinitionLocation {
                uri: file_uri(&target),
                range: typescript_range(member.range),
            })
        }
        TypeMemberTarget::SuperMemberCallback {
            binding,
            member_name: property,
            callback_name,
            chain,
        } => {
            let (import, binding) = index.import_binding_by_range(*binding)?;
            let imported_name = binding.imported_name.as_deref()?;
            let superclass_path = resolve_typescript_definition_import(
                cache,
                Path::new(importer_path),
                &import.specifier,
            )?;
            let (superclass_path, superclass) =
                resolve_typescript_export(cache, superclass_path, imported_name)?;
            let superclass_index = cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .index(&superclass_path)?;
            let property = superclass_index.type_member(&superclass.name, property)?;
            let (namespace, type_name) = property.type_reference.as_ref()?;
            let type_import = superclass_index.imports.iter().find(|import| {
                import
                    .bindings
                    .iter()
                    .any(|binding| binding.type_only && binding.local_name == *namespace)
            })?;
            let target = resolve_typescript_definition_import(
                cache,
                &superclass_path,
                &type_import.specifier,
            )?;
            let target_index = cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .index(&target)?;
            let callback = target_index.type_member(type_name, callback_name)?;
            let callback_type = callback.callback_parameter_type.as_deref()?;
            let final_type =
                chain
                    .iter()
                    .try_fold(callback_type.to_owned(), |type_name, method| {
                        target_index
                            .type_member(&type_name, method)?
                            .return_type
                            .clone()
                    })?;
            let member = target_index.type_member(&final_type, member_name)?;
            Some(LspDefinitionLocation {
                uri: file_uri(&target),
                range: typescript_range(member.range),
            })
        }
        TypeMemberTarget::Union(arms) => {
            let mut locations = arms.iter().map(|arm| match arm {
                TypeMemberTarget::Inline(_) => None,
                _ => resolve_typescript_type_member_target(
                    cache,
                    importer_path,
                    index,
                    arm,
                    member_name,
                ),
            });
            let first = locations.next()??;
            locations
                .all(|location| location.as_ref() == Some(&first))
                .then_some(first)
        }
    }
}

fn unique_type_member_location(
    members: &[galaxy_core::typescript::TypeMember],
    member_name: &str,
    path: &Path,
) -> Option<LspDefinitionLocation> {
    let matches = members
        .iter()
        .filter(|member| member.name == member_name)
        .collect::<Vec<_>>();
    let [member] = matches.as_slice() else {
        return None;
    };
    Some(LspDefinitionLocation {
        uri: file_uri(path),
        range: typescript_range(member.range),
    })
}

fn module_root_location(path: &Path) -> LspDefinitionLocation {
    LspDefinitionLocation {
        uri: file_uri(path),
        range: LspRange {
            start_line: 0,
            start_character: 0,
            end_line: 0,
            end_character: 0,
        },
    }
}

const MAX_TYPESCRIPT_REEXPORT_DEPTH: usize = 32;

fn resolve_typescript_export(
    cache: &Mutex<TypeScriptNavigationCache>,
    path: PathBuf,
    name: &str,
) -> Option<(PathBuf, galaxy_core::typescript::Declaration)> {
    resolve_typescript_export_inner(cache, path, name, 0, &mut HashSet::new())
}

fn resolve_typescript_type_export(
    cache: &Mutex<TypeScriptNavigationCache>,
    path: PathBuf,
    name: &str,
) -> Option<(PathBuf, galaxy_core::typescript::Declaration)> {
    resolve_typescript_type_export_inner(cache, path, name, 0, &mut HashSet::new())
}

fn resolve_typescript_type_export_inner(
    cache: &Mutex<TypeScriptNavigationCache>,
    path: PathBuf,
    name: &str,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
) -> Option<(PathBuf, galaxy_core::typescript::Declaration)> {
    if depth == MAX_TYPESCRIPT_REEXPORT_DEPTH || !visited.insert(module_identity(&path)) {
        return None;
    }
    let index = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .index(&path)?;
    if let Some(declaration) = index.exported_type_declaration(name) {
        return Some((path, declaration.clone()));
    }
    if let Some(declaration) = index.export_assignment_namespace(name) {
        return Some((path, declaration.clone()));
    }
    let candidates = index
        .exports
        .iter()
        .filter_map(|export| {
            (export.type_only && export.exported_name.as_deref() == Some(name))
                .then_some((export.specifier.as_deref()?, export.name.as_deref()?))
        })
        .filter_map(|(specifier, imported_name)| {
            resolve_typescript_definition_import(cache, &path, specifier).and_then(|target| {
                resolve_typescript_type_export_inner(
                    cache,
                    target,
                    imported_name,
                    depth + 1,
                    visited,
                )
            })
        })
        .collect::<Vec<_>>();
    (candidates.len() == 1).then(|| candidates.into_iter().next().unwrap())
}

fn resolve_typescript_export_inner(
    cache: &Mutex<TypeScriptNavigationCache>,
    path: PathBuf,
    name: &str,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
) -> Option<(PathBuf, galaxy_core::typescript::Declaration)> {
    if depth == MAX_TYPESCRIPT_REEXPORT_DEPTH || !visited.insert(module_identity(&path)) {
        return None;
    }
    let index = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .index(&path)?;
    if let Some(declaration) = index.exported_declaration(name) {
        return Some((path, declaration.clone()));
    }
    let mut candidates: Vec<(PathBuf, galaxy_core::typescript::Declaration)> = Vec::new();
    for export in index.exports.iter().filter(|export| !export.type_only) {
        let Some(specifier) = export.specifier.as_deref() else {
            continue;
        };
        let imported_name = if export.exported_name.as_deref() == Some(name) {
            export.name.as_deref()
        } else if export.exported_name.is_none() {
            Some(name)
        } else {
            None
        };
        let Some(imported_name) = imported_name else {
            continue;
        };
        let Some(target) = resolve_typescript_definition_import(cache, &path, specifier) else {
            continue;
        };
        if let Some(candidate) =
            resolve_typescript_export_inner(cache, target, imported_name, depth + 1, visited)
            && !candidates.iter().any(|(candidate_path, declaration)| {
                candidate_path == &candidate.0 && declaration.range == candidate.1.range
            })
        {
            candidates.push(candidate);
        }
    }
    (candidates.len() == 1).then(|| candidates.remove(0))
}

fn module_identity(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

const MAX_TYPESCRIPT_COMPLETIONS: usize = 100;

fn typescript_completions(
    path: &str,
    text: &str,
    line: usize,
    character: usize,
) -> Vec<LspCompletionItem> {
    let Some(prefix) = typescript_identifier_prefix(text, line, character) else {
        return Vec::new();
    };
    let index = DocumentIndex::parse(path, text);
    let mut items = BTreeMap::new();
    if character > 0
        && let Some(symbol) = index.symbol_at(line, character - 1)
        && symbol.name.starts_with(&prefix)
    {
        items
            .entry(symbol.name.clone())
            .or_insert_with(|| typescript_completion_item(&symbol.name, symbol.kind));
    }
    for declaration in &index.declarations {
        if declaration.name.starts_with(&prefix) {
            items
                .entry(declaration.name.clone())
                .or_insert_with(|| typescript_completion_item(&declaration.name, declaration.kind));
        }
    }
    for import in &index.imports {
        for binding in &import.bindings {
            if !binding.type_only && binding.local_name.starts_with(&prefix) {
                items.entry(binding.local_name.clone()).or_insert_with(|| {
                    typescript_completion_item(&binding.local_name, DeclarationKind::Variable)
                });
            }
        }
    }
    items
        .into_values()
        .take(MAX_TYPESCRIPT_COMPLETIONS)
        .collect()
}

fn typescript_identifier_prefix(text: &str, line: usize, character: usize) -> Option<String> {
    let line = text.lines().nth(line)?;
    let mut column = 0;
    let mut byte = 0;
    for (offset, ch) in line.char_indices() {
        let width = ch.len_utf16();
        if column + width > character {
            return None;
        }
        column += width;
        byte = offset + ch.len_utf8();
        if column == character {
            break;
        }
    }
    if column != character {
        return None;
    }
    let prefix = &line[..byte];
    let start = prefix
        .char_indices()
        .rev()
        .find(|(_, ch)| !is_typescript_identifier_char(*ch))
        .map_or(0, |(offset, ch)| offset + ch.len_utf8());
    Some(prefix[start..].to_owned())
}

fn is_typescript_identifier_char(ch: char) -> bool {
    ch == '_' || ch == '$' || ch.is_ascii_alphanumeric()
}

fn typescript_completion_item(name: &str, kind: DeclarationKind) -> LspCompletionItem {
    LspCompletionItem {
        label: name.to_owned(),
        detail: Some(typescript_declaration_kind(kind).to_owned()),
        insert_text: name.to_owned(),
        is_snippet: false,
        sort_text: Some(name.to_owned()),
        additional_edits: Vec::new(),
        replace_from: None,
        kind: Some(typescript_completion_kind(kind)),
    }
}

fn typescript_hover_markdown(kind: DeclarationKind, name: &str, path: &Path) -> String {
    format!(
        "**{} `{}`**\n\n`{}`",
        typescript_declaration_kind(kind),
        name,
        path.display()
    )
}

fn typescript_declaration_kind(kind: DeclarationKind) -> &'static str {
    match kind {
        DeclarationKind::Function => "function",
        DeclarationKind::Class => "class",
        DeclarationKind::Interface => "interface",
        DeclarationKind::Type => "type",
        DeclarationKind::Enum => "enum",
        DeclarationKind::Namespace => "namespace",
        DeclarationKind::Variable => "variable",
        DeclarationKind::Parameter => "parameter",
    }
}

fn typescript_completion_kind(kind: DeclarationKind) -> u64 {
    match kind {
        DeclarationKind::Class => 7,
        DeclarationKind::Interface => 8,
        DeclarationKind::Enum => 13,
        DeclarationKind::Function => 3,
        DeclarationKind::Variable | DeclarationKind::Parameter => 6,
        DeclarationKind::Type | DeclarationKind::Namespace => 9,
    }
}

fn typescript_import_is_type_only(import: &galaxy_core::typescript::Import) -> bool {
    import.type_only
        || (!import.bindings.is_empty() && import.bindings.iter().all(|binding| binding.type_only))
}

fn resolve_typescript_import_uncached(importer: &Path, specifier: &str) -> Option<PathBuf> {
    if javascript::is_relative_typescript_specifier(specifier) {
        return javascript::resolve_relative_import(specifier, importer);
    }
    if specifier.starts_with("node:") {
        return javascript::resolve_node_types_specifier(specifier, importer);
    }
    let project = javascript::TypeScriptProject::detect(importer)?;
    javascript::resolve_typescript_specifier(&project, specifier, importer)
}

fn resolve_typescript_definition_import(
    cache: &Mutex<TypeScriptNavigationCache>,
    importer: &Path,
    specifier: &str,
) -> Option<PathBuf> {
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .resolve(importer, specifier)
}

fn direct_imported_name(binding: &ImportBinding) -> Option<&str> {
    (!binding.type_only && matches!(binding.kind, ImportKind::Named | ImportKind::Default))
        .then_some(binding.imported_name.as_deref())
        .flatten()
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

fn spawn_go_index(
    engine: &Arc<Mutex<GoEngine>>,
    root: &Path,
) -> Option<std::thread::JoinHandle<()>> {
    let engine = Arc::clone(engine);
    let root = root.to_path_buf();
    std::thread::Builder::new()
        .name("go-index".into())
        .spawn(move || {
            let built = GoEngine::build(&root);
            engine
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .install(built);
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
            let built = php::build(&root);
            engine
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .install(built);
        })
        .ok()
}

/// An outline entry in the shape the language servers use; `kind` is the
/// LSP `SymbolKind` number.
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

fn typescript_outline_symbol(
    declaration: &galaxy_core::typescript::Declaration,
    uri: &str,
) -> galaxy_core::analysis::DocumentSymbolInfo {
    let kind = match declaration.kind {
        DeclarationKind::Function => 12,
        DeclarationKind::Class => 5,
        DeclarationKind::Interface => 11,
        DeclarationKind::Type => 26,
        DeclarationKind::Enum => 10,
        DeclarationKind::Namespace => 3,
        DeclarationKind::Variable => 13,
        DeclarationKind::Parameter => 13,
    };
    galaxy_core::analysis::DocumentSymbolInfo {
        uri: Some(uri.to_owned()),
        name: declaration.name.clone(),
        kind,
        range: LspRange {
            start_line: declaration.range.start.line as u64,
            start_character: declaration.range.start.col as u64,
            end_line: declaration.range.end.line as u64,
            end_character: declaration.range.end.col as u64,
        },
        children: Vec::new(),
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

/// A UTF-16 LSP diagnostic range, supplied by the LSP manager to a document.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DiagnosticInfo {
    pub start_line: u64,
    pub start_character: u64,
    pub end_line: u64,
    pub end_character: u64,
    /// LSP severity: 1 error, 2 warning, 3 information, 4 hint.
    pub severity: u8,
    pub message: String,
    /// LSP `Unnecessary` tag: unused code, drawn faded.
    pub unnecessary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DiagnosticSpan {
    pub start: u64,
    pub end: u64,
    pub severity: u8,
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
    /// Soft wrap width in columns; `None` while wrap is off.
    wrap_columns: Option<usize>,
    /// Row table for `wrap_columns`, rebuilt when the text or width changes.
    wrap_map: Option<galaxy_core::wrap::WrapMap>,
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
        let service = SyntaxService::new(language.grammar());
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
            let map = galaxy_core::wrap::WrapMap::build_with(
                &self.editor.doc,
                columns,
                self.folds.hidden_ranges().to_vec(),
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
    /// by the word immediately before the caret and merges them with LSP
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

    /// Called for every document on each LSP poll; bumps
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
                let start = if line.line == diagnostic.start_line {
                    diagnostic.start_character
                } else {
                    0
                };
                let end = if line.line == diagnostic.end_line {
                    diagnostic.end_character
                } else {
                    line.text.encode_utf16().count() as u64
                };
                if start < end {
                    line.diagnostics.push(DiagnosticSpan {
                        start,
                        end,
                        severity: diagnostic.severity,
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
        let comment_marker = st
            .syntax
            .as_ref()
            .and_then(|syntax| syntax.language.comment_marker());
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
            Some(columns) => {
                movement::move_selections_wrapped(&ed.doc, &ed.sel, motion.into(), extend, columns)
            }
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
        galaxy_core::wrap::breaks_for_line(doc, line, columns)
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
                wrap_columns: None,
                wrap_map: None,
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
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    struct ChannelListener(Mutex<mpsc::Sender<CoreEvent>>);

    impl CoreListener for ChannelListener {
        fn on_event(&self, event: CoreEvent) {
            let _ = self.0.lock().unwrap().send(event);
        }
    }

    #[test]
    fn typescript_navigation_cache_reuses_indexes_and_revalidates_disk_changes() {
        let dir = tempfile::tempdir().unwrap();
        let dependency = dir.path().join("dependency.ts");
        std::fs::write(&dependency, "export const first = 1;\n").unwrap();
        let mut cache = TypeScriptNavigationCache::default();

        let first = cache.index(&dependency).unwrap();
        let second = cache.index(&dependency).unwrap();
        assert!(Arc::ptr_eq(&first, &second));

        std::fs::write(&dependency, "export const changed = 22;\n").unwrap();
        let changed = cache.index(&dependency).unwrap();
        assert!(!Arc::ptr_eq(&first, &changed));
        assert!(changed.exported_declaration("changed").is_some());
    }

    #[test]
    fn typescript_navigation_cache_revalidates_package_manifest_resolution() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
        let package = dir.path().join("node_modules/pkg");
        std::fs::create_dir_all(&package).unwrap();
        let manifest = package.join("package.json");
        std::fs::write(&manifest, r#"{"exports":"./first.ts"}"#).unwrap();
        std::fs::write(package.join("first.ts"), "export const value = 1;\n").unwrap();
        std::fs::write(package.join("second.ts"), "export const value = 2;\n").unwrap();
        let importer = dir.path().join("main.ts");
        std::fs::write(&importer, "import { value } from 'pkg';\nvalue;\n").unwrap();
        let mut cache = TypeScriptNavigationCache::default();

        assert!(
            cache
                .resolve(&importer, "pkg")
                .unwrap()
                .ends_with("first.ts")
        );
        std::fs::write(&manifest, r#"{"exports":"./second.ts"}"#).unwrap();
        assert!(
            cache
                .resolve(&importer, "pkg")
                .unwrap()
                .ends_with("second.ts")
        );
    }

    #[test]
    fn typescript_navigation_warmup_caches_import_targets_and_reexports() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
        let package = dir.path().join("node_modules/pkg");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("package.json"), r#"{"exports":"./entry.js"}"#).unwrap();
        std::fs::write(
            package.join("entry.js"),
            "export { value } from './types.js';\n",
        )
        .unwrap();
        std::fs::write(
            package.join("entry.d.ts"),
            "export { value } from './types.js';\n",
        )
        .unwrap();
        std::fs::write(
            package.join("types.d.ts"),
            "export declare const value: number;\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.ts"),
            "import { value } from 'pkg';\nvalue;\n",
        )
        .unwrap();

        let cache = Mutex::new(TypeScriptNavigationCache::default());
        warm_typescript_navigation(dir.path(), &cache);

        let cache = cache.lock().unwrap();
        assert!(
            cache
                .resolutions
                .values()
                .any(|resolution| resolution.target.ends_with("entry.d.ts"))
        );
        assert!(
            cache
                .indexes
                .keys()
                .any(|path| path.ends_with("entry.d.ts"))
        );
        assert!(
            cache
                .indexes
                .keys()
                .any(|path| path.ends_with("types.d.ts"))
        );
    }

    #[test]
    fn typescript_navigation_warmup_excludes_node_modules_sources() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
        let ignored = dir.path().join("node_modules/ignored");
        std::fs::create_dir_all(&ignored).unwrap();
        std::fs::write(
            ignored.join("package.json"),
            r#"{"exports":"./entry.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            ignored.join("entry.d.ts"),
            "export declare const ignored: number;\n",
        )
        .unwrap();
        std::fs::write(
            ignored.join("source.ts"),
            "import { ignored } from 'ignored';\nignored;\n",
        )
        .unwrap();

        let cache = Mutex::new(TypeScriptNavigationCache::default());
        warm_typescript_navigation(dir.path(), &cache);

        let cache = cache.lock().unwrap();
        assert!(cache.resolutions.is_empty());
        assert!(cache.indexes.is_empty());
    }

    #[test]
    fn syntax_is_detected_and_arrives_without_blocking_viewport() {
        let dir = tempfile::tempdir().unwrap();
        let rust = dir.path().join("main.rs");
        std::fs::write(&rust, "// note\nfn main() { let name = \"Galaxy\"; }\n").unwrap();
        let doc = Core::new()
            .open_document(rust.to_string_lossy().into_owned())
            .unwrap();
        assert_eq!(doc.syntax_language(), Some("Rust".into()));
        // First snapshot is allowed to have no tree: parsing stays off-thread.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let snap = doc.viewport(0, 2);
            if !snap.styles.is_empty() {
                assert!(snap.styles.iter().any(|run| run.scope == "comment"));
                assert!(snap.styles.iter().any(|run| run.scope == "keyword"));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "syntax worker did not return styles"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        // Edits keep the previous colors until the reparse lands, instead of
        // drawing one plain frame per keystroke.
        doc.edit(EditCommand::InsertText { text: "x".into() });
        assert!(!doc.viewport(0, 2).styles.is_empty());
        // Undo must resubmit too; colors come back for the undone text.
        assert!(doc.undo());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !doc.poll_syntax() {
            assert!(Instant::now() < deadline, "undo was not reparsed");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            doc.viewport(0, 2)
                .styles
                .iter()
                .any(|run| run.scope == "keyword")
        );
        let plain = dir.path().join("notes.txt");
        std::fs::write(&plain, "plain text").unwrap();
        let plain = Core::new()
            .open_document(plain.to_string_lossy().into_owned())
            .unwrap();
        assert_eq!(plain.syntax_language(), None);
        assert!(plain.viewport(0, 1).styles.is_empty());
    }

    #[test]
    fn outline_uses_the_completed_current_syntax_tree() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("outline.rs");
        std::fs::write(&path, "struct Item;\nfn make() -> Item { Item }\n").unwrap();
        let doc = Core::new()
            .open_document(path.to_string_lossy().into_owned())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let outline = doc.outline();
            if !outline.is_empty() {
                assert_eq!(
                    outline
                        .iter()
                        .map(|symbol| symbol.name.as_str())
                        .collect::<Vec<_>>(),
                    ["Item", "make"]
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "syntax worker did not return an outline"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn syntax_can_be_suspended_and_resumed_without_changing_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idle.rs");
        std::fs::write(&path, "fn idle() {}\n").unwrap();
        let doc = Core::new()
            .open_document(path.to_string_lossy().into_owned())
            .unwrap();
        assert!(doc.syntax_active());
        assert!(doc.suspend_syntax());
        assert!(!doc.syntax_active());
        assert_eq!(doc.lines(0, 1), ["fn idle() {}"]);
        assert!(doc.resume_syntax());
        assert!(doc.syntax_active());
    }

    #[test]
    fn diagnostics_are_clipped_to_visible_utf16_lines() {
        let doc = Core::new().new_document();
        doc.edit(EditCommand::InsertText {
            text: "a😀\nxyz".into(),
        });
        doc.set_diagnostics(vec![DiagnosticInfo {
            start_line: 0,
            start_character: 1,
            end_line: 1,
            end_character: 2,
            severity: 1,
            message: "bad".into(),
            unnecessary: false,
        }]);
        let snap = doc.viewport(0, 2);
        assert_eq!(snap.lines[0].diagnostics[0].start, 1);
        assert_eq!(snap.lines[0].diagnostics[0].end, 3);
        assert_eq!(snap.lines[1].diagnostics[0].start, 0);
        assert_eq!(snap.lines[1].diagnostics[0].end, 2);
    }

    fn folding_doc() -> Arc<DocumentHandle> {
        let doc = Core::new().new_document();
        doc.edit(EditCommand::InsertText {
            text: "fn a() {\n    one;\n    two;\n}\nfn b() {\n    three;\n}".into(),
        });
        doc
    }

    #[test]
    fn folding_hides_lines_and_shrinks_the_row_map() {
        let doc = folding_doc();
        assert_eq!(doc.total_rows(), 7);
        assert_eq!(doc.fold_state(0), FoldState::Expanded);
        assert_eq!(doc.fold_state(1), FoldState::Plain);
        assert!(doc.toggle_fold(0));
        assert_eq!(doc.fold_state(0), FoldState::Folded);
        assert_eq!(doc.total_rows(), 5);
        assert_eq!(
            doc.row_of_line(3),
            1,
            "the closing brace follows the header"
        );
        assert_eq!(doc.line_at_row(1).line, 3);

        let snap = doc.viewport(0, 7);
        let shown: Vec<u64> = snap.lines.iter().map(|l| l.line).collect();
        assert_eq!(shown, [0, 3, 4, 5, 6]);
        assert!(snap.lines[0].folded && snap.lines[0].foldable);
        assert!(!snap.lines[2].folded && snap.lines[2].foldable);

        doc.toggle_fold(4);
        doc.unfold_all();
        assert_eq!(doc.total_rows(), 7);
        assert!(!doc.has_folds());
        assert!(doc.toggle_fold(0));
        assert!(doc.toggle_fold(0));
        assert_eq!(doc.total_rows(), 7);
        assert!(!doc.toggle_fold(1), "nothing to fold on a leaf line");
    }

    #[test]
    fn sticky_scopes_name_the_headers_around_a_line() {
        let doc = folding_doc();
        assert_eq!(doc.sticky_scopes(1, 3), [0]);
        assert_eq!(doc.sticky_scopes(5, 3), [4]);
        assert!(doc.sticky_scopes(3, 3).is_empty());
        assert!(doc.sticky_scopes(99, 3).is_empty());
    }

    #[test]
    fn folding_moves_a_hidden_caret_to_the_header() {
        let doc = folding_doc();
        doc.set_caret(TextPos {
            line: 2,
            col_utf16: 2,
        });
        doc.toggle_fold(0);
        assert_eq!(doc.primary_cursor().line, 0);
    }

    #[test]
    fn arrows_step_over_folded_lines() {
        let doc = folding_doc();
        doc.toggle_fold(0);
        doc.set_caret(TextPos {
            line: 0,
            col_utf16: 0,
        });
        doc.move_cursors(CursorMotion::Down, false);
        assert_eq!(doc.primary_cursor().line, 3);
        doc.move_cursors(CursorMotion::Up, false);
        assert_eq!(doc.primary_cursor().line, 0);
    }

    #[test]
    fn typing_in_a_folded_range_opens_it() {
        let doc = folding_doc();
        doc.toggle_fold(4);
        assert!(doc.has_folds());
        doc.set_caret(TextPos {
            line: 4,
            col_utf16: 0,
        });
        doc.edit(EditCommand::InsertText { text: "// ".into() });
        assert!(doc.has_folds(), "typing on the header keeps its fold");
        doc.set_caret(TextPos {
            line: 0,
            col_utf16: 0,
        });
        doc.edit(EditCommand::InsertText { text: "x\n".into() });
        assert!(doc.has_folds());
        assert_eq!(
            doc.fold_state(5),
            FoldState::Folded,
            "the fold moved down a line"
        );
    }

    #[test]
    fn unnecessary_flag_reaches_the_viewport() {
        let doc = Core::new().new_document();
        doc.edit(EditCommand::InsertText {
            text: "use Foo;".into(),
        });
        doc.set_diagnostics(vec![DiagnosticInfo {
            start_line: 0,
            start_character: 0,
            end_line: 0,
            end_character: 8,
            severity: 4,
            message: "unused".into(),
            unnecessary: true,
        }]);
        let span = doc.viewport(0, 1).lines[0].diagnostics[0].clone();
        assert!(span.unnecessary);
        assert_eq!(span.severity, 4);
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
    fn opening_workspace_indexes_all_files_with_shared_progress() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
        std::fs::write(dir.path().join("tsconfig.json"), "{}\n").unwrap();
        std::fs::write(dir.path().join("main.ts"), "export const value = 1;\n").unwrap();
        std::fs::write(
            dir.path().join("go.mod"),
            "module example.com/test\n\ngo 1.22\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("main.go"), "package test\n").unwrap();
        std::fs::write(dir.path().join("composer.json"), "{}\n").unwrap();

        let (tx, rx) = mpsc::channel();
        let core = Core::new();
        core.set_listener(Arc::new(ChannelListener(Mutex::new(tx))));
        core.open_workspace(dir.path().to_string_lossy().into_owned())
            .unwrap();

        let root = dir.path().to_string_lossy().into_owned();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut started = None;
        let mut progress = 0;
        let mut finished = None;
        while Instant::now() < deadline && finished.is_none() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Ok(event) = rx.recv_timeout(remaining) else {
                break;
            };
            match event {
                CoreEvent::WorkspaceIndexStarted {
                    root: event_root,
                    total,
                } if event_root == root => {
                    started = Some(total);
                }
                CoreEvent::WorkspaceIndexProgress {
                    root: event_root,
                    completed,
                    ..
                } if event_root == root => {
                    progress = completed;
                }
                CoreEvent::WorkspaceIndexFinished {
                    root: event_root,
                    total,
                    ..
                } if event_root == root => {
                    finished = Some(total);
                }
                _ => {}
            }
        }
        assert_eq!(started, Some(0));
        assert_eq!(finished, Some(6));
        assert_eq!(progress, 6);

        let plain = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let core = Core::new();
        core.set_listener(Arc::new(ChannelListener(Mutex::new(tx))));
        core.open_workspace(plain.path().to_string_lossy().into_owned())
            .unwrap();
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            CoreEvent::WorkspaceIndexStarted { total: 0, .. }
        ));
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
            vec![
                ("src".into(), true),
                ("target".into(), true),
                ("README.md".into(), false)
            ]
        );
        assert!(ws.set_explorer_excludes(vec!["target".into()]));
        let root: Vec<_> = ws
            .list_dir(String::new())
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(root, vec!["src", "README.md"]);

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

    fn doc_at(path: &str, text: &str, caret_utf16: u64) -> Arc<DocumentHandle> {
        let doc = Arc::new(DocumentHandle::new(
            Document::from_text(text),
            PathBuf::from(path),
        ));
        doc.set_selections(
            vec![TextSelection {
                anchor: TextPos {
                    line: 0,
                    col_utf16: caret_utf16,
                },
                head: TextPos {
                    line: 0,
                    col_utf16: caret_utf16,
                },
            }],
            0,
        );
        doc
    }

    fn type_text(doc: &DocumentHandle, text: &str) {
        doc.edit(EditCommand::InsertText { text: text.into() });
    }

    #[test]
    fn typing_gt_and_slash_close_tags_in_markup_files() {
        let html = doc_at("page.html", "<div", 4);
        type_text(&html, ">");
        assert_eq!(html.text(), "<div></div>");
        assert_eq!(primary(&html), (5, 0), "caret between the tags");

        let slash = doc_at("page.html", "<ul><li>a<", 10);
        type_text(&slash, "/");
        assert_eq!(slash.text(), "<ul><li>a</li>");

        let tsx = doc_at("view.tsx", "const a = <Foo bar={() => 1}", 28);
        type_text(&tsx, ">");
        assert_eq!(tsx.text(), "const a = <Foo bar={() => 1}></Foo>");

        let generic = doc_at("view.tsx", "let x: Array<string", 19);
        type_text(&generic, ">");
        assert_eq!(
            generic.text(),
            "let x: Array<string>",
            "generic stays plain"
        );

        let ts = doc_at("lib.ts", "<div", 4);
        type_text(&ts, ">");
        assert_eq!(ts.text(), "<div>", "TypeScript has no tags");
    }

    #[test]
    fn closing_a_tag_is_one_undo_step_and_can_be_switched_off() {
        let doc = doc_at("page.html", "<div", 4);
        type_text(&doc, ">");
        assert!(doc.undo());
        assert_eq!(doc.text(), "<div");

        let off = doc_at("page.html", "<div", 4);
        off.set_tag_editing(false, true);
        type_text(&off, ">");
        assert_eq!(off.text(), "<div>");
    }

    #[test]
    fn editing_a_tag_name_renames_its_partner_in_one_undo_step() {
        let doc = doc_at("page.html", "<div>x</div>", 4);
        type_text(&doc, "s");
        assert_eq!(doc.text(), "<divs>x</divs>");
        assert_eq!(primary(&doc), (5, 0));
        doc.edit(EditCommand::Backspace);
        assert_eq!(doc.text(), "<div>x</div>");
        assert!(doc.undo());
        assert_eq!(
            doc.text(),
            "<divs>x</divs>",
            "undo reverts both names together"
        );
        assert!(doc.undo());
        assert_eq!(doc.text(), "<div>x</div>");

        let off = doc_at("page.html", "<div>x</div>", 4);
        off.set_tag_editing(true, false);
        type_text(&off, "s");
        assert_eq!(off.text(), "<divs>x</div>");

        let text = doc_at("notes.md", "<div>x</div>", 4);
        type_text(&text, "s");
        assert_eq!(text.text(), "<divs>x</div>", "only markup languages rename");
    }

    fn primary(doc: &DocumentHandle) -> (u64, u64) {
        let r = doc.selected_range();
        (r.location, r.length)
    }

    fn php_project() -> (tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(
            root.join("composer.json"),
            r#"{"autoload":{"psr-4":{"App\\":"app/"}}}"#,
        )
        .unwrap();
        let order = root.join("app/Order.php");
        let report = root.join("app/Report.php");
        std::fs::write(
            &order,
            "<?php\nnamespace App;\n\nclass Order\n{\n    public function total(): int\n    {\n        return 1;\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            &report,
            "<?php\nnamespace App;\n\nclass Report\n{\n    public function run(Order $order): int\n    {\n        return $order->total();\n    }\n}\n",
        )
        .unwrap();
        (
            dir,
            order.to_string_lossy().into_owned(),
            report.to_string_lossy().into_owned(),
        )
    }

    #[test]
    fn php_problems_reach_the_document_without_a_language_server() {
        let (_dir, _order, report_path) = php_project();
        let core = Core::new();
        let report = core.open_document(report_path.clone()).unwrap();
        core.php_documents
            .lock()
            .unwrap()
            .insert(file_uri(Path::new(&report_path)), Arc::downgrade(&report));
        let wait = |want: usize| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                core.poll_analysis();
                let found = report.lock().diagnostics.clone();
                if found.len() == want {
                    return found;
                }
                assert!(Instant::now() < deadline, "got {found:?}, wanted {want}");
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        // A clean file has none; the first poll only notes the text.
        wait(0);

        // Typing a class that does not exist is reported once text settles.
        assert!(report.apply_rename_edits(vec![RenameEdit {
            uri: String::new(),
            start_line: 5,
            start_character: 24,
            end_line: 5,
            end_character: 24,
            new_text: "Ghost $g, ".into(),
        }]));
        let found = wait(1);
        assert_eq!(found[0].severity, 2);
        assert!(found[0].message.contains("Ghost"), "{found:?}");

        // Fixing it clears the problem.
        assert!(report.undo());
        wait(0);
    }

    const GO_MAIN: &str = "package main\n\ntype User struct {\n\tName string\n}\n\nfunc (u *User) Greet(greeting string) string {\n\treturn greeting + u.Name\n}\n\nfunc main() {\n\tu := &User{Name: \"a\"}\n\tu.Greet(\"hi\")\n}\n";

    fn go_project() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("go.mod"),
            "module example.com/app\n\ngo 1.22\n",
        )
        .unwrap();
        let main = dir.path().join("main.go");
        std::fs::write(&main, GO_MAIN).unwrap();
        (dir, main.to_string_lossy().into_owned())
    }

    #[test]
    fn javascript_project_and_components_are_manifest_gated() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"web","dependencies":{"react":"19","vue":"3","svelte":"5"}}"#,
        )
        .unwrap();
        let react = src.join("Button.tsx");
        let vue = src.join("Panel.vue");
        let svelte = src.join("Card.svelte");
        std::fs::write(&react, "export const Button = () => null").unwrap();
        std::fs::write(&vue, "<template><div /></template>").unwrap();
        std::fs::write(&svelte, "<script>let value = 1;</script>").unwrap();
        let core = Core::new();
        let project = core
            .javascript_project(react.to_string_lossy().into_owned())
            .unwrap();
        assert_eq!(project.name.as_deref(), Some("web"));
        assert_eq!(project.frameworks, ["React", "Vue", "Svelte"]);
        let components = core.javascript_components(vue.to_string_lossy().into_owned());
        assert_eq!(
            components
                .iter()
                .map(|component| (
                    component.framework.as_str(),
                    component.name.as_str(),
                    component
                        .symbols
                        .iter()
                        .map(|symbol| (symbol.name.as_str(), symbol.props.as_slice()))
                        .collect::<Vec<_>>(),
                ))
                .collect::<Vec<_>>(),
            vec![
                ("React", "Button", vec![]),
                ("Svelte", "Card", vec![("Card", &[][..])]),
                ("Vue", "Panel", vec![("Panel", &[][..])]),
            ]
        );
        assert_eq!(
            core.resolve_javascript_relative_import(
                "./Button.js".into(),
                vue.to_string_lossy().into_owned(),
            )
            .map(|target| target.uri),
            Some(file_uri(&react))
        );
    }

    /// Asks again until the index, built off-thread, is ready.
    fn until(what: &str, mut ask: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !ask() {
            assert!(Instant::now() < deadline, "{what}: engine never answered");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn go_files_are_registered_and_other_files_are_not() {
        let (dir, main_path) = go_project();
        let core = Core::new();
        let main = core.open_document(main_path).unwrap();
        assert!(core.register_document(main, "go".into(), String::new()));
        let notes = dir.path().join("notes.txt");
        std::fs::write(&notes, "x").unwrap();
        let notes = core
            .open_document(notes.to_string_lossy().into_owned())
            .unwrap();
        assert!(!core.register_document(notes, "text".into(), String::new()));
    }

    #[test]
    fn typescript_documents_provide_outline_and_parse_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("outline.ts");
        std::fs::write(
            &path,
            "export class App {}\nexport interface Config {}\nconst broken = (\n",
        )
        .unwrap();
        let core = Core::new();
        let document = core
            .open_document(path.to_string_lossy().into_owned())
            .unwrap();

        assert!(core.register_document(document.clone(), "typescript".into(), String::new()));
        assert_eq!(core.poll_analysis(), 1);
        let diagnostics = document.lock().diagnostics.clone();
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].severity, 1);
        assert!(diagnostics[0].message.contains("Syntax"), "{diagnostics:?}");

        assert!(core.request_document_symbols(document.clone()));
        let symbols: Vec<(String, String)> = core
            .document_symbols(document)
            .into_iter()
            .map(|symbol| (symbol.name, symbol.kind))
            .collect();
        assert_eq!(
            symbols,
            vec![("App".into(), "5".into()), ("Config".into(), "11".into())]
        );
    }

    #[test]
    fn typescript_type_diagnostics_are_published_with_galaxy_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("types.ts");
        std::fs::write(
            &path,
            "const value: number = 'wrong';\nconst unknown = other?.field;\n",
        )
        .unwrap();
        let core = Core::new();
        let document = core
            .open_document(path.to_string_lossy().into_owned())
            .unwrap();

        assert!(core.register_document(document.clone(), "typescript".into(), String::new()));
        core.poll_analysis();
        let diagnostics = document.lock().diagnostics.clone();
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].severity, 1);
        assert!(
            diagnostics[0].message.contains("assignable"),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn typescript_completion_and_hover_are_local_and_range_exact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.ts");
        std::fs::write(
            &path,
            "import { remoteValue } from './dep';\nconst localValue = 1;\nfunction run(parameter: string) { return parameter; }\nloc\n",
        )
        .unwrap();
        let core = Core::new();
        let document = core
            .open_document(path.to_string_lossy().into_owned())
            .unwrap();

        assert!(core.request_completion(document.clone(), 3, 3));
        let labels: Vec<_> = core
            .completions(document.clone())
            .into_iter()
            .map(|item| item.label)
            .collect();
        assert_eq!(labels, ["localValue"]);

        assert!(core.request_hover(document.clone(), 2, 9));
        let hover = core.hover(document).unwrap();
        assert!(hover.contents.contains("function `run`"), "{hover:?}");
        assert!(hover.contents.contains("main.ts"), "{hover:?}");
        assert_eq!(
            (
                hover.start_line,
                hover.start_character,
                hover.end_line,
                hover.end_character
            ),
            (Some(2), Some(9), Some(2), Some(12))
        );
    }

    #[test]
    fn typescript_definition_follows_migration_table_callback_parameter() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("database/migrations/create_roles_table.ts");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "export default class {\n  async up() {\n    this.schema.createTable(this.tableName, (table) => {\n      table.uuid('id').primary()\n    })\n  }\n}\n",
        )
        .unwrap();
        let core = Core::new();
        let document = core
            .open_document(path.to_string_lossy().into_owned())
            .unwrap();

        assert!(core.request_definition(document.clone(), 3, 7));
        let definition = core.definition_locations(document);
        assert_eq!(definition.len(), 1);
        assert_eq!((definition[0].line, definition[0].column), (2, 45));
    }

    #[test]
    fn typescript_definition_follows_migration_schema_builder_method() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
        let lucid = dir.path().join("node_modules/@adonisjs/lucid");
        let knex = dir.path().join("node_modules/knex");
        std::fs::create_dir_all(lucid.join("build/src/schema")).unwrap();
        std::fs::create_dir_all(lucid.join("build/src/types")).unwrap();
        std::fs::create_dir_all(knex.join("types")).unwrap();
        std::fs::write(
            lucid.join("package.json"),
            r#"{"exports":{"./schema":"./build/src/schema/main.js"}}"#,
        )
        .unwrap();
        std::fs::write(
            knex.join("package.json"),
            r#"{"main":"knex.js","types":"types/index.d.ts"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(knex.join("lib")).unwrap();
        std::fs::write(
            knex.join("knex.js"),
            "module.exports = require('./lib/index');\n",
        )
        .unwrap();
        std::fs::write(knex.join("lib/index.js"), "module.exports = {};\n").unwrap();
        std::fs::write(
            lucid.join("build/src/schema/main.d.ts"),
            "import type { Knex } from 'knex';\nimport type { QueryClientContract } from '../types/database.js';\nimport type { RawQueryBindings } from '../types/querybuilder.js';\nexport declare class BaseSchema { db: QueryClientContract; get schema(): Knex.SchemaBuilder; raw(bindings: RawQueryBindings): Knex.Raw; }\n",
        )
        .unwrap();
        std::fs::write(
            lucid.join("build/src/types/database.d.ts"),
            "export interface QueryClientContract { connectionName: string; }\n",
        )
        .unwrap();
        std::fs::write(
            lucid.join("build/src/types/querybuilder.d.ts"),
            "export type RawQueryBindings = Record<string, string>;\n",
        )
        .unwrap();
        std::fs::write(lucid.join("build/src/schema/main.js"), "").unwrap();
        std::fs::write(
            knex.join("types/index.d.ts"),
            "declare function knex(): Knex.Knex;\ndeclare namespace Knex {\n  export class SchemaBuilder {}\n  interface SchemaBuilder { createTable(name: string, callback: (tableBuilder: CreateTableBuilder) => any): void; }\n  interface CreateTableBuilder extends TableBuilder {}\n  interface TableBuilder { uuid(name: string): ColumnBuilder; string(name: string): ColumnBuilder; bigInteger(name: string): ColumnBuilder; }\n  interface ColumnBuilder { primary(): ColumnBuilder; notNullable(): ColumnBuilder; unique(): ColumnBuilder; unsigned(): ColumnBuilder; }\n}\nexport = knex;\n",
        )
        .unwrap();
        let path = dir.path().join("database/migrations/create_roles_table.ts");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "import { BaseSchema } from '@adonisjs/lucid/schema'\n\nexport default class extends BaseSchema {\n  protected tableName = 'roles'\n\n  async up() {\n    this.schema.createTable(this.tableName, (table) => {\n      table.uuid('id').primary()\n      table.string('name').notNullable()\n      table.string('code').notNullable().unique()\n\n      table.bigInteger('created_at').unsigned()\n      table.bigInteger('updated_at').unsigned()\n    })\n  }\n}\n",
        )
        .unwrap();
        let core = Core::new();
        let declaration = DocumentIndex::parse(
            lucid.join("build/src/schema/main.d.ts"),
            "import type { Knex } from 'knex';\nimport type { QueryClientContract } from '../types/database.js';\nimport type { RawQueryBindings } from '../types/querybuilder.js';\nexport declare class BaseSchema { db: QueryClientContract; get schema(): Knex.SchemaBuilder; raw(bindings: RawQueryBindings): Knex.Raw; }\n",
        );
        assert!(declaration.exported_declaration("BaseSchema").is_some());
        assert_eq!(
            resolve_typescript_import_uncached(&path, "@adonisjs/lucid/schema"),
            Some(lucid.join("build/src/schema/main.js"))
        );
        assert_eq!(
            resolve_typescript_import_uncached(&lucid.join("build/src/schema/main.d.ts"), "knex"),
            Some(knex.join("types/index.d.ts"))
        );
        let schema_document = core
            .open_document(
                lucid
                    .join("build/src/schema/main.d.ts")
                    .to_string_lossy()
                    .into_owned(),
            )
            .unwrap();
        assert!(core.request_definition(schema_document.clone(), 0, 14));
        let namespace = core.definition_locations(schema_document.clone());
        assert_eq!(namespace.len(), 1);
        assert_eq!(
            namespace[0].uri,
            file_uri(&std::fs::canonicalize(knex.join("types/index.d.ts")).unwrap())
        );
        assert_eq!((namespace[0].line, namespace[0].column), (1, 18));
        assert!(core.request_definition(schema_document.clone(), 3, 73));
        let schema_builder_namespace = core.definition_locations(schema_document.clone());
        assert_eq!(schema_builder_namespace.len(), 1);
        assert_eq!(schema_builder_namespace[0].uri, namespace[0].uri);
        assert_eq!(
            (
                schema_builder_namespace[0].line,
                schema_builder_namespace[0].column
            ),
            (1, 18)
        );
        assert!(core.request_definition(schema_document.clone(), 3, 78));
        let schema_builder = core.definition_locations(schema_document.clone());
        assert_eq!(schema_builder.len(), 1);
        assert_eq!(
            schema_builder[0].uri,
            file_uri(&std::fs::canonicalize(knex.join("types/index.d.ts")).unwrap())
        );
        assert_eq!((schema_builder[0].line, schema_builder[0].column), (3, 12));
        assert!(core.request_definition(schema_document.clone(), 0, 28));
        let knex_module = core.definition_locations(schema_document.clone());
        assert_eq!(knex_module.len(), 1);
        assert_eq!(
            knex_module[0].uri,
            file_uri(&std::fs::canonicalize(knex.join("types/index.d.ts")).unwrap())
        );
        assert!(core.request_definition(schema_document.clone(), 1, 18));
        let query_client = core.definition_locations(schema_document.clone());
        assert_eq!(query_client.len(), 1);
        assert_eq!(
            query_client[0].uri,
            file_uri(&std::fs::canonicalize(lucid.join("build/src/types/database.d.ts")).unwrap())
        );
        assert_eq!((query_client[0].line, query_client[0].column), (0, 7));
        assert!(core.request_definition(schema_document.clone(), 3, 39));
        let query_client_annotation = core.definition_locations(schema_document.clone());
        assert_eq!(query_client_annotation.len(), 1);
        assert_eq!(query_client_annotation[0].uri, query_client[0].uri);
        assert_eq!(
            (
                query_client_annotation[0].line,
                query_client_annotation[0].column
            ),
            (0, 7)
        );
        assert!(core.request_definition(schema_document.clone(), 2, 18));
        let raw_bindings = core.definition_locations(schema_document.clone());
        assert_eq!(raw_bindings.len(), 1);
        assert_eq!(
            raw_bindings[0].uri,
            file_uri(
                &std::fs::canonicalize(lucid.join("build/src/types/querybuilder.d.ts")).unwrap()
            )
        );
        assert_eq!((raw_bindings[0].line, raw_bindings[0].column), (0, 7));
        assert!(core.request_definition(schema_document.clone(), 2, 42));
        let querybuilder_module = core.definition_locations(schema_document.clone());
        assert_eq!(querybuilder_module.len(), 1);
        assert_eq!(
            querybuilder_module[0].uri,
            file_uri(
                &std::fs::canonicalize(lucid.join("build/src/types/querybuilder.d.ts")).unwrap()
            )
        );

        let document = core
            .open_document(path.to_string_lossy().into_owned())
            .unwrap();
        assert!(core.request_definition(document.clone(), 0, 10));
        let base = core.definition_locations(document.clone());
        assert_eq!(base.len(), 1);
        assert_eq!(
            base[0].uri,
            file_uri(&std::fs::canonicalize(lucid.join("build/src/schema/main.d.ts")).unwrap())
        );
        assert!(core.request_definition(document.clone(), 6, 9));
        let schema = core.definition_locations(document.clone());
        assert_eq!(schema.len(), 1);
        assert_eq!(
            schema[0].uri,
            file_uri(&std::fs::canonicalize(lucid.join("build/src/schema/main.d.ts")).unwrap())
        );
        let migration_line = "      table.uuid('id').primary()";
        let method_column = |name: &str| {
            migration_line.find(&format!("table.{name}")).unwrap() as u64 + "table.".len() as u64
        };
        assert!(core.request_definition(document.clone(), 7, method_column("uuid")));
        let uuid = core.definition_locations(document.clone());
        assert_eq!(uuid.len(), 1);
        assert_eq!(
            uuid[0].uri,
            file_uri(&std::fs::canonicalize(knex.join("types/index.d.ts")).unwrap())
        );
        assert_eq!((uuid[0].line, uuid[0].column), (5, 27));
        let string_line = "      table.string('name').notNullable()";
        let string_column =
            string_line.find("table.string").unwrap() as u64 + "table.".len() as u64;
        assert!(core.request_definition(document.clone(), 8, string_column));
        let string = core.definition_locations(document.clone());
        assert_eq!(string.len(), 1);
        assert_eq!((string[0].line, string[0].column), (5, 62));
        let big_integer_line = "      table.bigInteger('created_at').unsigned()";
        let big_integer_column =
            big_integer_line.find("table.bigInteger").unwrap() as u64 + "table.".len() as u64;
        assert!(core.request_definition(document.clone(), 11, big_integer_column));
        let big_integer = core.definition_locations(document.clone());
        assert_eq!(big_integer.len(), 1);
        assert_eq!((big_integer[0].line, big_integer[0].column), (5, 99));
        let chained_line = "      table.string('name').notNullable()";
        let chained_column = |name: &str| chained_line.find(name).unwrap() as u64;
        assert!(core.request_definition(document.clone(), 8, chained_column("notNullable")));
        let not_nullable = core.definition_locations(document.clone());
        assert_eq!(not_nullable.len(), 1);
        assert_eq!((not_nullable[0].line, not_nullable[0].column), (6, 54));
        let unique_line = "      table.string('code').notNullable().unique()";
        assert!(core.request_definition(
            document.clone(),
            9,
            unique_line.find("unique").unwrap() as u64
        ));
        let unique = core.definition_locations(document.clone());
        assert_eq!(unique.len(), 1);
        assert_eq!((unique[0].line, unique[0].column), (6, 84));
        let unsigned_line = "      table.bigInteger('created_at').unsigned()";
        assert!(core.request_definition(
            document.clone(),
            11,
            unsigned_line.find("unsigned").unwrap() as u64
        ));
        let unsigned = core.definition_locations(document);
        assert_eq!(unsigned.len(), 1);
        assert_eq!((unsigned[0].line, unsigned[0].column), (6, 109));
    }

    #[test]
    fn typescript_import_diagnostics_use_project_resolution() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{"compilerOptions":{"baseUrl":".","paths":{"@lib/*":["src/*"]}}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/ok.ts"), "export const ok = 1;\n").unwrap();
        let path = dir.path().join("main.ts");
        std::fs::write(
            &path,
            "import { ok } from '@lib/ok';\nimport pkg from 'pkg';\nimport type { Type } from 'missing-types';\n",
        )
        .unwrap();
        let package = dir.path().join("node_modules/pkg");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("package.json"), r#"{"exports":"./index.ts"}"#).unwrap();
        std::fs::write(package.join("index.ts"), "export default 1;\n").unwrap();

        let core = Core::new();
        let document = core
            .open_document(path.to_string_lossy().into_owned())
            .unwrap();
        assert!(core.register_document(document.clone(), "typescript".into(), String::new()));
        core.poll_analysis();
        assert!(
            document.lock().diagnostics.is_empty(),
            "{:?}",
            document.lock().diagnostics
        );
        let uri = file_uri(&path);
        let first_revision = document.revision();
        core.poll_analysis();
        assert_eq!(
            core.typescript_checked.lock().unwrap().get(&uri),
            Some(&first_revision),
            "unchanged TypeScript buffers must not be reparsed by every analysis poll"
        );

        assert!(document.apply_rename_edits(vec![RenameEdit {
            uri: String::new(),
            start_line: 1,
            start_character: 17,
            end_line: 1,
            end_character: 20,
            new_text: "missing-pkg".into(),
        }]));
        core.poll_analysis();
        assert_eq!(
            core.typescript_checked.lock().unwrap().get(&uri),
            Some(&document.revision()),
            "an edit must invalidate the cached TypeScript analysis"
        );
        let diagnostics = document.lock().diagnostics.clone();
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].severity, 2);
        assert!(
            diagnostics[0].message.contains("missing-pkg"),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn typescript_local_navigation_is_shadow_aware_and_renames_stable_occurrences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shadow.ts");
        std::fs::write(
            &path,
            "const emoji = '😀';\nconst value = 1;\n{ const value = 2; value; }\nvalue;\n",
        )
        .unwrap();
        let core = Core::new();
        let document = core
            .open_document(path.to_string_lossy().into_owned())
            .unwrap();

        assert!(core.request_definition(document.clone(), 2, 19));
        assert_eq!(
            core.definition_locations(document.clone())
                .into_iter()
                .map(|location| (location.line, location.column))
                .collect::<Vec<_>>(),
            vec![(2, 8)]
        );

        assert!(core.request_references(document.clone(), 2, 19));
        assert_eq!(
            core.references(document.clone())
                .into_iter()
                .map(|location| (location.line, location.column))
                .collect::<Vec<_>>(),
            vec![(2, 8), (2, 19)]
        );

        assert!(core.request_rename(document.clone(), 2, 19, "innerValue".into()));
        let edits = core.rename_edits(document.clone());
        assert_eq!(edits.len(), 2);
        assert!(edits.iter().all(|edit| edit.new_text == "innerValue"));
        assert_eq!(
            edits
                .iter()
                .map(|edit| (edit.start_line, edit.start_character, edit.end_character))
                .collect::<Vec<_>>(),
            vec![(2, 8, 13), (2, 19, 24)]
        );
        assert!(!core.request_rename(document.clone(), 2, 19, "1bad".into()));
        assert!(core.rename_edits(document).is_empty());
    }

    #[test]
    fn typescript_navigation_resolves_direct_imports_through_projects_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{"compilerOptions":{"baseUrl":".","paths":{"@lib/*":["src/*"]}}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/dep.ts"),
            "const local = 1;\nexport { local as publicValue };\n",
        )
        .unwrap();
        let main_path = dir.path().join("main.ts");
        std::fs::write(
            &main_path,
            "import { publicValue as value } from '@lib/dep';\nvalue;\n",
        )
        .unwrap();
        let core = Core::new();
        let main = core
            .open_document(main_path.to_string_lossy().into_owned())
            .unwrap();

        assert!(core.request_definition(main.clone(), 1, 1));
        let definition = core.definition_locations(main.clone());
        assert_eq!(definition.len(), 1);
        assert!(definition[0].uri.ends_with("src/dep.ts"));
        assert_eq!((definition[0].line, definition[0].column), (0, 6));

        assert!(core.request_references(main.clone(), 1, 1));
        assert_eq!(core.references(main.clone()).len(), 3);

        std::fs::write(dir.path().join("src/unsafe.ts"), "export * from './dep';\n").unwrap();
        let unsafe_path = dir.path().join("unsafe-main.ts");
        std::fs::write(
            &unsafe_path,
            "import { publicValue } from '@lib/unsafe';\npublicValue;\n",
        )
        .unwrap();
        let unsafe_document = core
            .open_document(unsafe_path.to_string_lossy().into_owned())
            .unwrap();
        assert!(core.request_definition(unsafe_document.clone(), 1, 1));
        assert!(
            core.definition_locations(unsafe_document)[0]
                .uri
                .ends_with("src/dep.ts")
        );
    }

    #[test]
    fn typescript_navigation_resolves_package_exports_and_type_only_fallbacks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
        let package = dir.path().join("node_modules/pkg");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("package.json"), r#"{"exports":"./entry.ts"}"#).unwrap();
        std::fs::write(
            package.join("entry.ts"),
            "export default function run() {}\n",
        )
        .unwrap();
        let main_path = dir.path().join("main.ts");
        std::fs::write(&main_path, "import run from 'pkg';\nrun();\n").unwrap();
        let core = Core::new();
        let main = core
            .open_document(main_path.to_string_lossy().into_owned())
            .unwrap();
        assert!(core.request_definition(main.clone(), 1, 1));
        assert!(
            core.definition_locations(main.clone())[0]
                .uri
                .ends_with("entry.ts")
        );

        let unsupported_path = dir.path().join("unsupported.ts");
        std::fs::write(
            &unsupported_path,
            "import * as api from 'pkg';\nimport type Run from 'pkg';\napi.run();\n",
        )
        .unwrap();
        let unsupported = core
            .open_document(unsupported_path.to_string_lossy().into_owned())
            .unwrap();
        assert!(!core.request_definition(unsupported.clone(), 0, 13));
        assert!(core.request_definition(unsupported, 1, 13));
    }

    #[test]
    fn typescript_navigation_resolves_static_module_strings_and_namespace_members() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        std::fs::write(
            dir.path().join("dep.ts"),
            "export const value = 1;\nexport const Component = () => null;\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("reexport.ts"),
            "export { value } from './dep.js';\n",
        )
        .unwrap();
        let main_path = dir.path().join("main.tsx");
        std::fs::write(
            &main_path,
            "import * as api from './dep.js';\nimport './dep.js';\nexport { value } from './reexport.js';\nexport * from './dep.js';\napi.value;\nconst View = <api.Component />;\napi.value.deep;\nconst value = 1; ({ value });\n",
        )
        .unwrap();
        let core = Core::new();
        let main = core
            .open_document(main_path.to_string_lossy().into_owned())
            .unwrap();

        for (line, column) in [(0, 25), (1, 9), (2, 28), (3, 17)] {
            assert!(
                core.request_definition(main.clone(), line, column),
                "cursor {line}:{column}"
            );
            let locations = core.definition_locations(main.clone());
            assert_eq!(locations.len(), 1);
            assert!(
                locations[0]
                    .uri
                    .ends_with(if line == 2 { "reexport.ts" } else { "dep.ts" })
            );
            assert_eq!((locations[0].line, locations[0].column), (0, 0));
        }

        assert!(core.request_definition(main.clone(), 4, 5));
        let value = core.definition_locations(main.clone());
        assert!(value[0].uri.ends_with("dep.ts"));
        assert_eq!((value[0].line, value[0].column), (0, 13));

        assert!(core.request_definition(main.clone(), 5, 21));
        let component = core.definition_locations(main.clone());
        assert!(component[0].uri.ends_with("dep.ts"));
        assert_eq!((component[0].line, component[0].column), (1, 13));

        assert!(!core.request_definition(main.clone(), 6, 5));
        assert!(!core.request_definition(main, 7, 20));
    }

    #[test]
    fn typescript_navigation_resolves_package_imports_reexports_and_literal_dynamic_imports() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r##"{"imports":{"#alias/*":"./src/*.js"}}"##,
        )
        .unwrap();
        std::fs::write(dir.path().join("src/value.ts"), "export const value = 1;\n").unwrap();
        std::fs::write(
            dir.path().join("src/named.ts"),
            "export { value as named } from '#alias/value';\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/star.ts"),
            "export * from '#alias/named';\n",
        )
        .unwrap();
        let main_path = dir.path().join("main.ts");
        std::fs::write(
            &main_path,
            "import { named } from './src/star.js';\nnamed;\nimport('#alias/value');\nimport(alias);\n",
        )
        .unwrap();
        let core = Core::new();
        let main = core
            .open_document(main_path.to_string_lossy().into_owned())
            .unwrap();

        assert!(core.request_definition(main.clone(), 1, 1));
        let definition = core.definition_locations(main.clone());
        assert_eq!(definition.len(), 1);
        assert!(definition[0].uri.ends_with("src/value.ts"));
        assert_eq!((definition[0].line, definition[0].column), (0, 13));

        assert!(core.request_definition(main.clone(), 2, 10));
        assert!(
            core.definition_locations(main.clone())[0]
                .uri
                .ends_with("src/value.ts")
        );
        assert!(!core.request_definition(main, 3, 8));
    }

    #[test]
    fn typescript_navigation_reexport_cycles_and_ambiguous_stars_fall_back_to_module_roots() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        std::fs::write(dir.path().join("a.ts"), "export * from './b.js';\n").unwrap();
        std::fs::write(dir.path().join("b.ts"), "export * from './a.js';\n").unwrap();
        std::fs::write(dir.path().join("one.ts"), "export const value = 1;\n").unwrap();
        std::fs::write(dir.path().join("two.ts"), "export const value = 2;\n").unwrap();
        std::fs::write(
            dir.path().join("ambiguous.ts"),
            "export * from './one.js';\nexport * from './two.js';\n",
        )
        .unwrap();
        let main_path = dir.path().join("main.ts");
        std::fs::write(
            &main_path,
            "import { value as cycle } from './a.js';\nimport { value } from './ambiguous.js';\ncycle;\nvalue;\n",
        )
        .unwrap();
        let core = Core::new();
        let main = core
            .open_document(main_path.to_string_lossy().into_owned())
            .unwrap();
        for (line, target) in [(2, "a.ts"), (3, "ambiguous.ts")] {
            assert!(core.request_definition(main.clone(), line, 1));
            let definition = core.definition_locations(main.clone());
            assert!(definition[0].uri.ends_with(target), "{:?}", definition[0]);
            assert_eq!((definition[0].line, definition[0].column), (0, 0));
        }
    }

    #[test]
    fn typescript_navigation_resolves_type_imports_annotations_and_direct_members() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
        std::fs::write(
            dir.path().join("types.ts"),
            "export interface User { name: string; greet(): void }\nexport type Record = { title: string };\nexport const Value = 1;\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("alias.ts"),
            "export interface User { name: string; greet(): void }\nexport { User as Person };\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("reexport.ts"),
            "export type { Record } from './types';\n",
        )
        .unwrap();
        let main_path = dir.path().join("main.ts");
        std::fs::write(
            &main_path,
            "import type { Person } from './alias';\nimport { type Record } from './reexport';\nimport { Value } from './types';\nconst user: Person = {} as Person;\nfunction use(record: Record) { user.name; user.greet(); record.title; }\nclass Box { user: Person; show() { return this.user.name; } }\nValue;\nconst union: Person | undefined = undefined; union?.name;\nuser['name']; user.name.deep;\n",
        )
        .unwrap();
        let core = Core::new();
        let main = core
            .open_document(main_path.to_string_lossy().into_owned())
            .unwrap();

        for (line, column, target, target_line, target_column) in [
            (0, 14, "alias.ts", 0, 7),
            (1, 14, "types.ts", 1, 7),
            (3, 12, "alias.ts", 0, 7),
            (4, 36, "alias.ts", 0, 24),
            (4, 47, "alias.ts", 0, 38),
            (4, 63, "types.ts", 1, 23),
            (5, 53, "alias.ts", 0, 24),
        ] {
            assert!(
                core.request_definition(main.clone(), line, column),
                "cursor {line}:{column}"
            );
            let definition = core.definition_locations(main.clone());
            assert!(definition[0].uri.ends_with(target));
            assert_eq!(
                (definition[0].line, definition[0].column),
                (target_line, target_column),
                "cursor {line}:{column}"
            );
        }

        assert!(core.request_definition(main.clone(), 6, 1));
        assert!(
            core.definition_locations(main.clone())[0]
                .uri
                .ends_with("types.ts")
        );

        for (line, column) in [(7, 51), (8, 5), (8, 24)] {
            assert!(
                !core.request_definition(main.clone(), line, column),
                "cursor {line}:{column}"
            );
        }
    }

    #[test]
    fn typescript_navigation_resolves_direct_inline_object_annotation_members() {
        let dir = tempfile::tempdir().unwrap();
        let main_path = dir.path().join("main.ts");
        std::fs::write(
            &main_path,
            "const value: { name: string; run(): void } = {} as never;\n\
             value.name; value?.name; value[\"name\"]; value?.[\"name\"];\n\
             function use(parameter: { name: string }) { parameter.name; }\n\
             class Box { field: { name: string }; show() { return this.field.name; } }\n\
             value.name.deep; value[key]; value[`name`];\n",
        )
        .unwrap();
        let core = Core::new();
        let main = core
            .open_document(main_path.to_string_lossy().into_owned())
            .unwrap();

        for (line, column, target_line, target_column) in [
            (1, 6, 0, 15),
            (1, 19, 0, 15),
            (1, 32, 0, 15),
            (1, 48, 0, 15),
            (2, 56, 2, 26),
            (3, 65, 3, 21),
        ] {
            assert!(
                core.request_definition(main.clone(), line, column),
                "cursor {line}:{column}"
            );
            let definition = core.definition_locations(main.clone());
            assert_eq!(definition.len(), 1);
            assert_eq!(
                (definition[0].line, definition[0].column),
                (target_line, target_column),
                "cursor {line}:{column}"
            );
        }

        assert!(core.request_definition(main.clone(), 4, 6));
        let definition = core.definition_locations(main.clone());
        assert_eq!((definition[0].line, definition[0].column), (0, 15));
        for (line, column) in [(4, 11), (4, 24), (4, 35)] {
            assert!(
                !core.request_definition(main.clone(), line, column),
                "cursor {line}:{column}"
            );
        }
    }

    #[test]
    fn typescript_navigation_resolves_imported_superclass_members() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
        std::fs::write(
            dir.path().join("base.ts"),
            "export class NamedBase { protected schema = 1; create(): void {} }\nexport default class DefaultBase { public schema() {} }\nexport const NotClass = { schema: 1 };\n",
        )
        .unwrap();
        let main_path = dir.path().join("main.ts");
        std::fs::write(
            &main_path,
            "import { NamedBase } from './base';\nimport DefaultBase from './base';\nimport type { NamedBase as TypeBase } from './base';\nimport { NotClass } from './base';\nclass Named extends NamedBase { use() { this.schema; this?.schema; this[\"schema\"]; this.schema.create; this.unknown; } }\nclass Default extends DefaultBase { use() { this.schema; } }\nclass Typed extends TypeBase { use() { this.schema; } }\nclass Value extends NotClass { use() { this.schema; } }\nexport default class extends NamedBase { use() { this.schema; } }\n",
        )
        .unwrap();
        let core = Core::new();
        let main = core
            .open_document(main_path.to_string_lossy().into_owned())
            .unwrap();

        for (line, column, target_line, target_column) in [
            (4, 45, 0, 35),
            (4, 59, 0, 35),
            (4, 74, 0, 35),
            (5, 51, 1, 42),
            (8, 55, 0, 35),
        ] {
            assert!(
                core.request_definition(main.clone(), line, column),
                "cursor {line}:{column}"
            );
            let definition = core.definition_locations(main.clone());
            assert_eq!(definition.len(), 1);
            assert!(definition[0].uri.ends_with("base.ts"));
            assert_eq!(
                (definition[0].line, definition[0].column),
                (target_line, target_column)
            );
        }
        for (line, column) in [(4, 81), (4, 94), (6, 43), (7, 43)] {
            assert!(
                !core.request_definition(main.clone(), line, column),
                "cursor {line}:{column}"
            );
        }
    }

    #[test]
    fn typescript_definition_prefers_adjacent_package_declarations() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
        let package = dir.path().join("node_modules/@adonisjs/lucid");
        std::fs::create_dir_all(package.join("build/src/schema")).unwrap();
        std::fs::write(
            package.join("package.json"),
            r#"{"exports":{"./schema":"./build/src/schema/main.js","./without-types":"./build/src/schema/without-types.js"}}"#,
        )
        .unwrap();
        std::fs::write(
            package.join("build/src/schema/main.js"),
            "export class BaseSchema { get schema() { return null } }\n",
        )
        .unwrap();
        std::fs::write(
            package.join("build/src/schema/main.d.ts"),
            "export class BaseSchema { schema: string }\n",
        )
        .unwrap();
        std::fs::write(
            package.join("build/src/schema/without-types.js"),
            "export class WithoutTypes {}\n",
        )
        .unwrap();
        let main_path = dir.path().join("main.ts");
        let migration = "export default class extends BaseSchema { up() { this.schema; this.schema; } }\nWithoutTypes;";
        let source = format!(
            "import {{ BaseSchema }} from '@adonisjs/lucid/schema';\nimport {{ WithoutTypes }} from '@adonisjs/lucid/without-types';\n{migration}\n"
        );
        std::fs::write(&main_path, source).unwrap();
        let core = Core::new();
        let main = core
            .open_document(main_path.to_string_lossy().into_owned())
            .unwrap();

        assert!(core.request_definition(main.clone(), 0, 30));
        assert!(
            core.definition_locations(main.clone())[0]
                .uri
                .ends_with("build/src/schema/main.d.ts")
        );

        assert!(core.request_definition(main.clone(), 0, 10));
        assert!(
            core.definition_locations(main.clone())[0]
                .uri
                .ends_with("build/src/schema/main.d.ts")
        );

        let first = migration.find("this.schema").unwrap() + "this.".len();
        let second = migration.rfind("this.schema").unwrap() + "this.".len();
        for column in [first, second] {
            assert!(
                core.request_definition(main.clone(), 2, column as u64),
                "cursor 2:{column}"
            );
            let definition = core.definition_locations(main.clone());
            assert_eq!(definition.len(), 1);
            assert!(definition[0].uri.ends_with("build/src/schema/main.d.ts"));
        }
        assert!(core.request_definition(main.clone(), 3, 1));
        assert!(
            core.definition_locations(main.clone())[0]
                .uri
                .ends_with("build/src/schema/without-types.js")
        );
        assert!(!core.request_definition(main, 2, (first + "schema".len()) as u64));
    }

    #[test]
    fn typescript_definition_follows_package_declaration_reexports() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
        let adonis = dir
            .path()
            .join("node_modules/@adonisjs/core/build/src/helpers");
        let poppinss = dir.path().join("node_modules/@poppinss/utils/build/src");
        std::fs::create_dir_all(&adonis).unwrap();
        std::fs::create_dir_all(&poppinss).unwrap();
        std::fs::write(
            dir.path().join("node_modules/@adonisjs/core/package.json"),
            r#"{"exports":{"./helpers":"./build/src/helpers/main.js"}}"#,
        )
        .unwrap();
        std::fs::write(adonis.join("main.js"), "").unwrap();
        std::fs::write(
            adonis.join("main.d.ts"),
            "export { compose } from '@poppinss/utils';\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("node_modules/@poppinss/utils/package.json"),
            r#"{"exports":{".":"./build/index.js"}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path()
                .join("node_modules/@poppinss/utils/build/index.js"),
            "",
        )
        .unwrap();
        std::fs::write(
            dir.path()
                .join("node_modules/@poppinss/utils/build/index.d.ts"),
            "export { compose } from './src/compose.js';\n",
        )
        .unwrap();
        std::fs::write(
            poppinss.join("compose.d.ts"),
            "export declare function compose(): void;\n",
        )
        .unwrap();
        let main_path = dir.path().join("app/models/order.ts");
        std::fs::create_dir_all(main_path.parent().unwrap()).unwrap();
        std::fs::write(
            &main_path,
            "import { DateTime } from 'luxon';\n\
             import { BaseModel, beforeCreate, belongsTo, column } from '@adonisjs/lucid/orm';\n\
             import { compose } from '@adonisjs/core/helpers';\n\
             import { withAuthFinder } from '@adonisjs/auth/mixins/lucid';\n\
             import hash from '@adonisjs/core/services/hash';\n\
             import type { BelongsTo } from '@adonisjs/lucid/types/relations';\n\
             import User from '#models/user';\n\
             import Product from '#models/product';\n\
             \n\
             \n\
             \n\
             \n\
             export default class Order extends compose(BaseModel, withAuthFinder(() => hash.use('scrypt'), {\n\
               uids: ['email'],\n\
               passwordColumn: 'password',\n\
             })) {}\n",
        )
        .unwrap();
        let core = Core::new();
        let main = core
            .open_document(main_path.to_string_lossy().into_owned())
            .unwrap();

        let compose_in_import = 10;
        let compose_in_order_class = "export default class Order extends ".len() as u64;
        for (line, column) in [(2, compose_in_import), (12, compose_in_order_class)] {
            assert!(
                core.request_definition(main.clone(), line, column),
                "cursor {line}:{column}"
            );
            let definition = core.definition_locations(main.clone());
            assert_eq!(definition.len(), 1);
            assert!(
                definition[0]
                    .uri
                    .ends_with("@poppinss/utils/build/src/compose.d.ts"),
                "{definition:?}"
            );
            assert_eq!((definition[0].line, definition[0].column), (0, 24));
        }
    }

    #[test]
    fn typescript_definition_resolves_named_declaration_reexports() {
        let dir = tempfile::tempdir().unwrap();
        let strategies = dir.path().join("naming_strategies");
        std::fs::create_dir_all(&strategies).unwrap();
        std::fs::write(
            strategies.join("camel_case.d.ts"),
            "export declare class CamelCaseNamingStrategy {}\nexport declare class Internal {}\nexport interface Typed {}\n",
        )
        .unwrap();
        let barrel_path = dir.path().join("main.d.ts");
        std::fs::write(
            &barrel_path,
            "export { CamelCaseNamingStrategy } from './naming_strategies/camel_case.js';\nexport { Internal as Public } from './naming_strategies/camel_case.js';\nexport type { Typed } from './naming_strategies/camel_case.js';\nexport * from './naming_strategies/camel_case.js';\nexport { Broken as } from './naming_strategies/camel_case.js';\n",
        )
        .unwrap();
        let core = Core::new();
        let barrel = core
            .open_document(barrel_path.to_string_lossy().into_owned())
            .unwrap();

        for (line, column, target_line, target_column) in [
            (0, 10, 0, 21),
            (1, 10, 1, 21),
            (1, 22, 1, 21),
            (2, 15, 2, 7),
        ] {
            assert!(
                core.request_definition(barrel.clone(), line, column),
                "cursor {line}:{column}"
            );
            let definition = core.definition_locations(barrel.clone());
            assert_eq!(definition.len(), 1);
            assert!(
                definition[0]
                    .uri
                    .ends_with("naming_strategies/camel_case.d.ts")
            );
            assert_eq!(definition[0].line, target_line);
            assert_eq!(definition[0].column, target_column);
        }
        assert!(!core.request_definition(barrel.clone(), 3, 7));
        assert!(!core.request_definition(barrel, 4, 10));
    }

    #[test]
    fn typescript_navigation_resolves_node_modules_declaration_imports_safely() {
        let dir = tempfile::tempdir().unwrap();
        let lucid = dir
            .path()
            .join("node_modules/@adonisjs/lucid/build/src/connection");
        std::fs::create_dir_all(&lucid).unwrap();
        std::fs::write(
            dir.path().join("node_modules/@adonisjs/lucid/package.json"),
            r#"{"types":"./build/src/connection/index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            lucid.join("types.d.ts"),
            "export interface ConnectionConfig {}\nexport interface ConnectionContract {}\n",
        )
        .unwrap();
        let tarn = dir.path().join("node_modules/tarn/dist");
        std::fs::create_dir_all(&tarn).unwrap();
        std::fs::write(
            dir.path().join("node_modules/tarn/package.json"),
            r#"{"types":"./dist/tarn.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            tarn.join("tarn.d.ts"),
            "export * from './one.js';\nexport * from './two.js';\n",
        )
        .unwrap();
        std::fs::write(tarn.join("one.js"), "").unwrap();
        std::fs::write(tarn.join("two.js"), "").unwrap();
        std::fs::write(tarn.join("one.d.ts"), "export declare class Pool {}\n").unwrap();
        std::fs::write(tarn.join("two.d.ts"), "export declare class Other {}\n").unwrap();
        let knex = dir.path().join("node_modules/knex/types");
        std::fs::create_dir_all(&knex).unwrap();
        std::fs::write(
            dir.path().join("node_modules/knex/package.json"),
            r#"{"types":"./types/index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(knex.join("index.d.ts"), "declare namespace Knex {}\n").unwrap();
        let core = dir.path().join("node_modules/@adonisjs/core");
        std::fs::create_dir_all(core.join("build")).unwrap();
        std::fs::write(
            core.join("package.json"),
            r#"{"exports":{"./logger":"./build/logger.js"}}"#,
        )
        .unwrap();
        std::fs::write(
            core.join("build/logger.d.ts"),
            "export as namespace Logger;\n",
        )
        .unwrap();
        let node = dir.path().join("node_modules/@types/node");
        std::fs::create_dir_all(&node).unwrap();
        std::fs::write(
            node.join("package.json"),
            r#"{"exports":{"./events":"./events.d.ts"}}"#,
        )
        .unwrap();
        std::fs::write(
            node.join("events.d.ts"),
            "export declare class EventEmitter {}\n",
        )
        .unwrap();
        let connection = lucid.join("index.d.ts");
        std::fs::write(
            &connection,
            "const broken = ;\nimport type { ConnectionConfig, ConnectionContract } from './types.js';\nimport { Pool } from 'tarn';\nimport type Knex from 'knex';\nimport Logger from '@adonisjs/core/logger';\nimport { EventEmitter } from 'node:events';\nexport type UsesConfig = ConnectionConfig;\nexport type UsesContract = ConnectionContract;\nexport type UsesPool = Pool;\nexport type UsesKnex = Knex;\nexport type UsesLogger = Logger;\nexport type UsesEmitter = EventEmitter;\n",
        )
        .unwrap();
        let core = Core::new();
        let document = core
            .open_document(connection.to_string_lossy().into_owned())
            .unwrap();

        for (line, column, target_line) in [(1, 14, 0), (1, 32, 1)] {
            assert!(
                core.request_definition(document.clone(), line, column),
                "cursor {line}:{column}"
            );
            let definition = core.definition_locations(document.clone());
            assert!(definition[0].uri.ends_with("connection/types.d.ts"));
            assert_eq!((definition[0].line, definition[0].column), (target_line, 7));
        }
        for (line, column, target, target_position) in [
            (2, 9, "tarn/dist/one.d.ts", (0, 21)),
            (3, 12, "knex/types/index.d.ts", (0, 0)),
            (4, 7, "@adonisjs/core/build/logger.d.ts", (0, 0)),
            (5, 9, "@types/node/events.d.ts", (0, 21)),
        ] {
            assert!(
                core.request_definition(document.clone(), line, column),
                "cursor {line}:{column}"
            );
            let definition = core.definition_locations(document.clone());
            assert!(definition[0].uri.ends_with(target));
            assert_eq!((definition[0].line, definition[0].column), target_position);
        }
        let malformed = lucid.join("malformed.d.ts");
        std::fs::write(&malformed, "import { Malformed from 'tarn';\n").unwrap();
        let malformed = core
            .open_document(malformed.to_string_lossy().into_owned())
            .unwrap();
        assert!(!core.request_definition(malformed, 0, 9));
    }

    #[test]
    fn typescript_navigation_does_not_resolve_node_specifiers_without_local_types() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node_modules/pkg/index.d.ts");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path.parent().unwrap().join("package.json"), "{}").unwrap();
        std::fs::write(
            &path,
            "import { EventEmitter } from 'node:events';\nEventEmitter;\n",
        )
        .unwrap();
        let core = Core::new();
        let document = core
            .open_document(path.to_string_lossy().into_owned())
            .unwrap();
        assert!(!core.request_definition(document, 0, 9));
    }

    #[test]
    fn typescript_union_members_require_one_exact_definition() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
        std::fs::write(
            dir.path().join("types.ts"),
            "export interface User { name: string }\nexport interface Other { name: string }\n",
        )
        .unwrap();
        let main_path = dir.path().join("main.ts");
        std::fs::write(
            &main_path,
            "import type { User, Other } from './types';\n\
             import { User as ValueUser } from './types';\n\
             const same: User | User = {} as never; same.name;\n\
             const different: User | Other = {} as never; different.name;\n\
             const inline: { name: string } | { name: string } = {} as never; inline.name;\n\
             const value: ValueUser = {} as never; value.name;\n\
             { interface User { local: string } const local: User = {} as never; local.local; }\n",
        )
        .unwrap();
        let core = Core::new();
        let main = core
            .open_document(main_path.to_string_lossy().into_owned())
            .unwrap();

        assert!(core.request_definition(main.clone(), 2, 45));
        let same = core.definition_locations(main.clone());
        assert_eq!(same.len(), 1);
        assert!(same[0].uri.ends_with("types.ts"));
        assert_eq!((same[0].line, same[0].column), (0, 24));
        for (line, column) in [(3, 57), (4, 83), (5, 53)] {
            assert!(
                !core.request_definition(main.clone(), line, column),
                "cursor {line}:{column}"
            );
        }
        assert!(core.request_definition(main, 6, 78));
    }

    #[test]
    fn go_navigation_comes_from_the_engine() {
        let (_dir, main_path) = go_project();
        let core = Core::new();
        let main = core.open_document(main_path).unwrap();

        until("definition", || {
            core.request_definition(main.clone(), 12, 4)
        });
        let found = core.definition_locations(main.clone());
        assert_eq!(found.len(), 1);
        assert!(found[0].uri.ends_with("main.go"));
        assert_eq!((found[0].line, found[0].column), (6, 15));

        until("hover", || core.request_hover(main.clone(), 12, 4));
        let hover = core.hover(main.clone()).unwrap();
        assert!(hover.contents.contains("Greet"), "{}", hover.contents);

        until("references", || {
            core.request_references(main.clone(), 12, 4)
        });
        let mut lines: Vec<u64> = core
            .references(main.clone())
            .into_iter()
            .map(|r| r.line)
            .collect();
        lines.sort();
        assert_eq!(lines, [6, 12]);

        assert!(core.request_rename(main.clone(), 12, 4, "Hello".into()));
        let edits = core.rename_edits(main.clone());
        assert_eq!(edits.len(), 2);
        assert!(edits.iter().all(|e| e.new_text == "Hello"));
        assert!(!core.request_rename(main.clone(), 12, 4, "1bad".into()));

        until("symbols", || core.request_document_symbols(main.clone()));
        let names: Vec<String> = core
            .document_symbols(main.clone())
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(names.contains(&"User".to_owned()), "{names:?}");
        assert!(names.contains(&"Greet".to_owned()), "{names:?}");
    }

    #[test]
    fn go_completion_lists_members_after_a_dot() {
        let (_dir, main_path) = go_project();
        let core = Core::new();
        let main = core.open_document(main_path).unwrap();
        assert!(main.apply_rename_edits(vec![RenameEdit {
            uri: String::new(),
            start_line: 12,
            start_character: 3,
            end_line: 12,
            end_character: 14,
            new_text: String::new(),
        }]));
        until("completion", || {
            core.request_completion(main.clone(), 12, 3)
        });
        let labels: Vec<String> = core
            .completions(main.clone())
            .into_iter()
            .map(|c| c.label)
            .collect();
        assert!(labels.contains(&"Name".to_owned()), "{labels:?}");
        assert!(labels.contains(&"Greet".to_owned()), "{labels:?}");
    }

    #[test]
    fn go_problems_reach_the_document_without_a_language_server() {
        let (_dir, main_path) = go_project();
        let core = Core::new();
        let main = core.open_document(main_path).unwrap();
        assert!(core.register_document(main.clone(), "go".into(), String::new()));
        let wait = |want: usize| {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                core.poll_analysis();
                let found = main.lock().diagnostics.clone();
                if found.len() == want {
                    return found;
                }
                assert!(Instant::now() < deadline, "got {found:?}, wanted {want}");
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        wait(0);

        assert!(main.apply_rename_edits(vec![RenameEdit {
            uri: String::new(),
            start_line: 12,
            start_character: 1,
            end_line: 12,
            end_character: 1,
            new_text: "unused := 1\n\t".into(),
        }]));
        let found = wait(1);
        assert!(found[0].message.contains("unused"), "{found:?}");

        assert!(main.undo());
        wait(0);
    }

    #[test]
    fn blade_templates_get_laravel_names_from_the_engine() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |rel: &str, text: &str| {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write("artisan", "<?php");
        write(
            "composer.json",
            r#"{"autoload":{"psr-4":{"App\\":"app/"}}}"#,
        );
        write("resources/views/welcome.blade.php", "hi");
        let page = root.join("resources/views/page.blade.php");
        write(
            "resources/views/page.blade.php",
            "@include('wel')\n@include('gone')\n",
        );
        let page_path = page.to_string_lossy().into_owned();

        let core = Core::new();
        let doc = core.open_document(page_path.clone()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let items = loop {
            core.request_completion(doc.clone(), 0, 13);
            let items = core.completions(doc.clone());
            if !items.is_empty() {
                break items;
            }
            assert!(Instant::now() < deadline, "engine never became ready");
            std::thread::sleep(Duration::from_millis(20));
        };
        let item = items
            .iter()
            .find(|i| i.label == "welcome")
            .expect("welcome");
        assert_eq!(item.replace_from, Some(10));

        // A view that does not exist is reported, with a fix creating it.
        core.php_documents
            .lock()
            .unwrap()
            .insert(file_uri(&page), Arc::downgrade(&doc));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            core.poll_analysis();
            let found = doc.lock().diagnostics.clone();
            if found.len() == 2 {
                // `wel` is only a prefix of `welcome`, so it is missing too.
                assert!(found[1].message.contains("gone"), "{found:?}");
                break;
            }
            assert!(Instant::now() < deadline, "got {found:?}");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(core.request_code_actions(doc.clone(), 1, 14));
        let actions = core.code_actions(doc);
        assert_eq!(actions.len(), 1);
        assert!(
            actions[0].edits[0]
                .uri
                .ends_with("resources/views/gone.blade.php"),
            "{actions:?}"
        );
    }

    #[test]
    fn php_references_and_rename_come_from_the_engine() {
        let (_dir, order_path, report_path) = php_project();
        let core = Core::new();
        let report = core.open_document(report_path.clone()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        // The index is built off-thread; ask again until it is ready.
        while !core.request_references(report.clone(), 7, 24) {
            assert!(Instant::now() < deadline, "engine never became ready");
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut found: Vec<(String, u64, u64)> = core
            .references(report.clone())
            .into_iter()
            .map(|r| {
                (
                    r.uri.rsplit('/').next().unwrap().to_owned(),
                    r.line,
                    r.column,
                )
            })
            .collect();
        found.sort();
        assert_eq!(
            found,
            vec![("Order.php".into(), 5, 20), ("Report.php".into(), 7, 23)]
        );

        // Rename edits cover the declaration in the other file too.
        assert!(core.request_rename(report.clone(), 7, 24, "sum".into()));
        let mut edits: Vec<(String, u64, u64, String)> = core
            .rename_edits(report.clone())
            .into_iter()
            .map(|e| {
                (
                    e.uri.rsplit('/').next().unwrap().to_owned(),
                    e.start_line,
                    e.start_character,
                    e.new_text,
                )
            })
            .collect();
        edits.sort();
        assert_eq!(
            edits,
            vec![
                ("Order.php".into(), 5, 20, "sum".into()),
                ("Report.php".into(), 7, 23, "sum".into()),
            ]
        );
        // A name PHP would reject is not applied.
        assert!(!core.request_rename(report, 7, 24, "1 bad".into()));
        let _ = order_path;
    }

    #[test]
    fn php_signature_outline_and_actions_come_from_the_engine() {
        let (dir, _order_path, report_path) = php_project();
        let extra_path = dir.path().join("app/Extra.php");
        std::fs::write(
            &extra_path,
            "<?php\nnamespace App\\Services;\n\nuse App\\Order;\n\nclass Extra\n{\n}\n",
        )
        .unwrap();
        let core = Core::new();
        let report = core.open_document(report_path).unwrap();
        let extra = core
            .open_document(extra_path.to_string_lossy().into_owned())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);

        // Inside `total(` of `return $order->total();`.
        while !core.request_signature_help(report.clone(), 7, 29) {
            assert!(Instant::now() < deadline, "engine never became ready");
            std::thread::sleep(Duration::from_millis(20));
        }
        let signature = core.signature_help(report.clone()).expect("signature");
        assert_eq!(signature.label, "total(): int");
        assert!(signature.parameters.is_empty());

        assert!(core.request_document_symbols(report.clone()));
        let names: Vec<(String, String)> = core
            .document_symbols(report)
            .into_iter()
            .map(|s| (s.name, s.kind))
            .collect();
        // Kind 5 is a class, 6 a method.
        assert_eq!(
            names,
            vec![("Report".into(), "5".into()), ("run".into(), "6".into())]
        );

        // The unused `use App\Order;` on line 3 can be removed.
        assert!(core.request_code_actions(extra.clone(), 3, 5));
        let actions = core.code_actions(extra);
        assert_eq!(actions.len(), 1, "{actions:?}");
        assert_eq!(actions[0].title, "Remove unused import");
        let edit = &actions[0].edits[0];
        assert_eq!(
            (
                edit.start_line,
                edit.start_character,
                edit.end_line,
                edit.end_character
            ),
            (3, 0, 4, 0)
        );
        assert!(edit.new_text.is_empty());
    }

    #[test]
    fn typescript_code_actions_are_not_dispatched_to_the_php_engine() {
        let core = Core::new();
        let doc = doc_at(
            "component.ts",
            "import { Widget } from './widget';\nWidget;\n",
            0,
        );

        assert!(!core.request_code_actions(doc, 42, 99));
    }

    #[test]
    fn rename_edits_use_utf16_ranges_and_one_undo_step() {
        let doc = doc_with("😀 old old");
        assert!(doc.apply_rename_edits(vec![
            RenameEdit {
                uri: String::new(),
                start_line: 0,
                start_character: 3,
                end_line: 0,
                end_character: 6,
                new_text: "new".into()
            },
            RenameEdit {
                uri: String::new(),
                start_line: 0,
                start_character: 7,
                end_line: 0,
                end_character: 10,
                new_text: "new".into()
            },
        ]));
        assert_eq!(all_text(&doc), "😀 new new");
        assert!(doc.undo());
        assert_eq!(all_text(&doc), "😀 old old");
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
