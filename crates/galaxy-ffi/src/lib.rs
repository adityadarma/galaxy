//! UniFFI bridge exposed to Swift.
//!
//! Design rule: the UI calls in with coarse-grained commands and receives
//! batched events through `CoreListener`. No per-character FFI chatter.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use galaxy_core::change::{Change, ChangeSet};
use galaxy_core::clipboard;
use galaxy_core::commands::{self, CommandRegistry};
use galaxy_core::disk::{DiskCheck, DiskStamp};
use galaxy_core::document::{Document, Encoding, LineEnding};
use galaxy_core::edit::{self, IndentConfig};
use galaxy_core::file_finder::FileFinder as CoreFileFinder;
use galaxy_core::find::{FindError, FindOptions, Finder};
use galaxy_core::history::{EditKind, Editor};
use galaxy_core::keymap::{self, Keymap};
use galaxy_core::lsp::{
    LARAVEL_COMPANION, LspKey, LspManager, LspSettings, companion_server, lsp_language_id,
    lsp_server_language,
};
use galaxy_core::movement::{self, Motion};
use galaxy_core::multicursor;
use galaxy_core::navigation::{self, Location, NavHistory};
use galaxy_core::position::Utf16Position;
use galaxy_core::selection::{Selection, SelectionSet};
use galaxy_core::snippets;
use galaxy_core::syntax::{self, SyntaxLanguage, SyntaxService, SyntaxSnapshot};
use galaxy_core::terminal::TerminalSession;
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
    galaxy_core::version().to_string()
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
    lsp: Mutex<LspManager>,
    lsp_documents: Mutex<HashMap<String, Arc<DocumentHandle>>>,
}

impl Default for Core {
    fn default() -> Self {
        Self {
            listener: RwLock::new(None),
            lsp: Mutex::new(LspManager::default()),
            lsp_documents: Mutex::new(HashMap::new()),
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
                .name("galaxy-ping".into())
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

    /// Starts the built-in language server for a document when its executable
    /// is available on PATH. Unsupported languages and missing servers are
    /// intentionally non-fatal.
    pub fn start_lsp(
        &self,
        document: Arc<DocumentHandle>,
        language: String,
        root: String,
    ) -> Result<bool, CoreError> {
        let path = document.path();
        if path.is_empty() {
            return Ok(false);
        }
        let key = LspKey {
            language: lsp_server_language(&language),
            root: PathBuf::from(root),
        };
        let (settings, errors) = LspSettings::load(
            LspSettings::global_settings_path().as_deref(),
            Some(&key.root),
        );
        for error in errors {
            tracing::warn!(%error, "ignoring invalid LSP settings");
        }
        let Some(config) = settings.resolve_for_root(&language, Some(&key.root)) else {
            return Ok(false);
        };
        let language_id = lsp_language_id(&language, &path);
        let uri = file_uri(Path::new(&path));
        if self
            .lsp_documents
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&uri)
        {
            return Ok(true);
        }
        let companion = companion_server(&language, Some(&key.root), &config);
        let mut lsp = self.lsp.lock().unwrap_or_else(|e| e.into_inner());
        lsp.ensure_server(key.clone(), &config)?;
        let text = document.text();
        let doc = Document::from_text(&text);
        if let Some(companion) = companion {
            let companion_key = LspKey {
                language: LARAVEL_COMPANION.to_owned(),
                root: key.root.clone(),
            };
            // Optional extra: a failing laravel-lsp must not block the primary.
            match lsp.ensure_server(companion_key.clone(), &companion) {
                Ok(_) => {
                    if let Err(error) =
                        lsp.open_companion(companion_key, uri.clone(), language_id.clone(), &doc)
                    {
                        tracing::warn!(%error, "could not open document in laravel-lsp");
                    }
                }
                Err(error) => tracing::warn!(%error, "could not start laravel-lsp"),
            }
        }
        lsp.open_document(key, uri.clone(), language_id, &doc)?;
        self.lsp_documents
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(uri, document);
        Ok(true)
    }

    /// Polls all running default servers and applies their latest diagnostics
    /// to registered documents. Returns the number of consumed events.
    pub fn poll_lsp(&self) -> u64 {
        let mut lsp = self.lsp.lock().unwrap_or_else(|e| e.into_inner());
        let _ = lsp.flush_changes(Instant::now());
        let events = lsp.poll();
        let documents = self.lsp_documents.lock().unwrap_or_else(|e| e.into_inner());
        for uri in documents.keys() {
            let diagnostics = lsp
                .diagnostics(uri)
                .iter()
                .map(|diagnostic| DiagnosticInfo {
                    start_line: diagnostic.range.start_line,
                    start_character: diagnostic.range.start_character,
                    end_line: diagnostic.range.end_line,
                    end_character: diagnostic.range.end_character,
                    severity: match diagnostic.severity {
                        galaxy_core::lsp::DiagnosticSeverity::Error => 1,
                        galaxy_core::lsp::DiagnosticSeverity::Warning => 2,
                        galaxy_core::lsp::DiagnosticSeverity::Information => 3,
                        galaxy_core::lsp::DiagnosticSeverity::Hint => 4,
                    },
                    message: diagnostic.message.clone(),
                })
                .collect();
            if let Some(document) = documents.get(uri) {
                document.set_diagnostics(diagnostics);
            }
        }
        events.len() as u64
    }

    pub fn lsp_status(&self) -> LspStatusInfo {
        let mut lsp = self.lsp.lock().unwrap_or_else(|e| e.into_inner());
        LspStatusInfo {
            servers: lsp.server_count() as u64,
            progress: lsp
                .progress()
                .into_iter()
                .filter(|item| item.active)
                .map(|item| LspProgressInfo {
                    title: item.title,
                    message: item.message,
                    percentage: item.percentage,
                })
                .collect(),
            error: lsp.last_server_error(),
            processes: lsp
                .server_processes()
                .into_iter()
                .map(|process| LspProcessInfo {
                    language: process.language,
                    pid: process.pid as u64,
                    resident_memory_kib: process.resident_memory_kib,
                })
                .collect(),
        }
    }

    pub fn stop_lsp(&self, document: Arc<DocumentHandle>) -> bool {
        let path = document.path();
        if path.is_empty() {
            return false;
        }
        let uri = file_uri(Path::new(&path));
        let key = self
            .lsp_documents
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&uri)
            .map(|_| ());
        let Some(_) = key else {
            return false;
        };
        let mut documents = self.lsp_documents.lock().unwrap_or_else(|e| e.into_inner());
        let Some((uri, _)) = documents
            .iter()
            .find(|(_, value)| value.path() == path)
            .map(|(uri, value)| (uri.clone(), value.clone()))
        else {
            return false;
        };
        let mut lsp = self.lsp.lock().unwrap_or_else(|e| e.into_inner());
        let Some((key, _)) = lsp.document_key(&uri) else {
            return false;
        };
        for companion in lsp.companion_keys(&uri) {
            lsp.stop_server(&companion);
        }
        let stopped = lsp.stop_server(&key);
        documents.retain(|_, value| value.path() != path);
        stopped
    }

    pub fn restart_lsp(
        &self,
        document: Arc<DocumentHandle>,
        language: String,
        root: String,
    ) -> Result<bool, CoreError> {
        let path = document.path();
        if path.is_empty() {
            return Ok(false);
        }
        let key = LspKey {
            language: lsp_server_language(&language),
            root: PathBuf::from(root),
        };
        let restarted = {
            let mut lsp = self.lsp.lock().unwrap_or_else(|e| e.into_inner());
            let companion = LspKey {
                language: LARAVEL_COMPANION.to_owned(),
                root: key.root.clone(),
            };
            if let Err(error) = lsp.restart_server(&companion) {
                tracing::warn!(%error, "could not restart laravel-lsp");
            }
            lsp.restart_server(&key)?
        };
        if restarted {
            return Ok(true);
        }
        self.start_lsp(document, language, key.root.to_string_lossy().into_owned())
    }

    pub fn request_hover(&self, document: Arc<DocumentHandle>, line: u64, character: u64) -> bool {
        let path = document.path();
        if path.is_empty() {
            return false;
        }
        let uri = file_uri(Path::new(&path));
        self.lsp
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .request_hover(&uri, line, character, Instant::now())
            .ok()
            .flatten()
            .is_some()
    }

    pub fn hover(&self, document: Arc<DocumentHandle>) -> Option<HoverInfo> {
        let path = document.path();
        if path.is_empty() {
            return None;
        }
        let uri = file_uri(Path::new(&path));
        self.lsp
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
        self.lsp
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .request_completion(&uri, line, character, Instant::now())
            .ok()
            .flatten()
            .is_some()
    }

    pub fn completions(&self, document: Arc<DocumentHandle>) -> Vec<CompletionInfo> {
        let path = document.path();
        if path.is_empty() {
            return Vec::new();
        }
        let uri = file_uri(Path::new(&path));
        self.lsp
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .completions(&uri)
            .iter()
            .map(|item| CompletionInfo {
                label: item.label.clone(),
                detail: item.detail.clone(),
                insert_text: item.insert_text.clone(),
                is_snippet: item.is_snippet,
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
        self.lsp
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .request_signature_help(&uri, line, character, Instant::now())
            .ok()
            .flatten()
            .is_some()
    }

    pub fn signature_help(&self, document: Arc<DocumentHandle>) -> Option<SignatureInfo> {
        let path = document.path();
        if path.is_empty() {
            return None;
        }
        let uri = file_uri(Path::new(&path));
        self.lsp
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
        request_definition_kind(self, document, line, character, DefinitionKind::Definition)
    }

    pub fn request_type_definition(
        &self,
        document: Arc<DocumentHandle>,
        line: u64,
        character: u64,
    ) -> bool {
        request_definition_kind(self, document, line, character, DefinitionKind::Type)
    }

    pub fn request_implementation(
        &self,
        document: Arc<DocumentHandle>,
        line: u64,
        character: u64,
    ) -> bool {
        request_definition_kind(
            self,
            document,
            line,
            character,
            DefinitionKind::Implementation,
        )
    }

    pub fn definition_locations(&self, document: Arc<DocumentHandle>) -> Vec<DefinitionLocation> {
        let path = document.path();
        if path.is_empty() {
            return Vec::new();
        }
        let uri = file_uri(Path::new(&path));
        self.lsp
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
        self.lsp
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .request_references(&uri, line, character, Instant::now())
            .ok()
            .flatten()
            .is_some()
    }

    pub fn references(&self, document: Arc<DocumentHandle>) -> Vec<DefinitionLocation> {
        let path = document.path();
        if path.is_empty() {
            return Vec::new();
        }
        let uri = file_uri(Path::new(&path));
        self.lsp
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
        self.lsp
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .request_rename(&uri, line, character, &new_name, Instant::now())
            .ok()
            .flatten()
            .is_some()
    }

    pub fn rename_edits(&self, document: Arc<DocumentHandle>) -> Vec<RenameEdit> {
        let path = document.path();
        if path.is_empty() {
            return Vec::new();
        }
        let uri = file_uri(Path::new(&path));
        self.lsp
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
        let uri = file_uri(Path::new(&path));
        let mut lsp = self.lsp.lock().unwrap_or_else(|e| e.into_inner());
        let diagnostics = lsp.diagnostics(&uri).to_vec();
        let range = galaxy_core::lsp::Range {
            start_line: line,
            start_character: character,
            end_line: line,
            end_character: character,
        };
        lsp.request_code_actions(&uri, range, &diagnostics, Instant::now())
            .ok()
            .flatten()
            .is_some()
    }

    pub fn code_actions(&self, document: Arc<DocumentHandle>) -> Vec<CodeActionInfo> {
        let path = document.path();
        if path.is_empty() {
            return Vec::new();
        }
        let uri = file_uri(Path::new(&path));
        self.lsp
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
        self.lsp
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .request_document_symbols(&uri, Instant::now())
            .ok()
            .flatten()
            .is_some()
    }

    pub fn document_symbols(&self, document: Arc<DocumentHandle>) -> Vec<LspOutlineSymbol> {
        let path = document.path();
        if path.is_empty() {
            return Vec::new();
        }
        let uri = file_uri(Path::new(&path));
        fn flatten(
            source: &[galaxy_core::lsp::DocumentSymbolInfo],
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
        let lsp = self.lsp.lock().unwrap_or_else(|e| e.into_inner());
        flatten(lsp.document_symbols(&uri), &mut output);
        output
    }

    pub fn request_workspace_symbols(&self, root: String, query: String) -> bool {
        self.lsp
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .request_workspace_symbols(&root, &query, Instant::now())
            .ok()
            .flatten()
            .is_some()
    }

    pub fn workspace_symbols(&self, root: String) -> Vec<LspOutlineSymbol> {
        fn flatten(
            source: &[galaxy_core::lsp::DocumentSymbolInfo],
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
        let lsp = self.lsp.lock().unwrap_or_else(|e| e.into_inner());
        flatten(lsp.workspace_symbols(&root), &mut output);
        output
    }

    pub fn request_formatting(
        &self,
        document: Arc<DocumentHandle>,
        tab_size: u64,
        insert_spaces: bool,
    ) {
        let path = document.path();
        if path.is_empty() {
            return;
        }
        let uri = file_uri(Path::new(&path));
        let _ = self
            .lsp
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .request_formatting(&uri, tab_size, insert_spaces, Instant::now());
    }

    pub fn formatting_edits(&self, document: Arc<DocumentHandle>) -> Vec<RenameEdit> {
        let path = document.path();
        if path.is_empty() {
            return Vec::new();
        }
        let uri = file_uri(Path::new(&path));
        self.lsp
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .formatting_edits(&uri)
            .iter()
            .map(rename_edit)
            .collect()
    }

    pub fn request_range_formatting(
        &self,
        document: Arc<DocumentHandle>,
        range: Utf16Range,
        tab_size: u64,
        insert_spaces: bool,
    ) {
        let path = document.path();
        if path.is_empty() {
            return;
        }
        let uri = file_uri(Path::new(&path));
        let start = document.position_at(range.location);
        let end = document.position_at(range.location.saturating_add(range.length));
        let lsp_range = galaxy_core::lsp::Range {
            start_line: start.line,
            start_character: start.col_utf16,
            end_line: end.line,
            end_character: end.col_utf16,
        };
        let _ = self
            .lsp
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .request_range_formatting(&uri, lsp_range, tab_size, insert_spaces, Instant::now());
    }

    pub fn request_on_type_formatting(
        &self,
        document: Arc<DocumentHandle>,
        line: u64,
        character: u64,
        trigger: String,
        tab_size: u64,
        insert_spaces: bool,
    ) {
        let path = document.path();
        if path.is_empty() {
            return;
        }
        let uri = file_uri(Path::new(&path));
        let _ = self
            .lsp
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .request_on_type_formatting(
                &uri,
                line,
                character,
                &trigger,
                tab_size,
                insert_spaces,
                Instant::now(),
            );
    }

    pub fn request_semantic_tokens(&self, document: Arc<DocumentHandle>) {
        let path = document.path();
        if path.is_empty() {
            return;
        }
        let uri = file_uri(Path::new(&path));
        let _ = self
            .lsp
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .request_semantic_tokens(&uri, Instant::now());
    }

    pub fn request_inlay_hints(
        &self,
        document: Arc<DocumentHandle>,
        start_line: u64,
        end_line: u64,
    ) {
        let path = document.path();
        if path.is_empty() {
            return;
        }
        let uri = file_uri(Path::new(&path));
        let range = galaxy_core::lsp::Range {
            start_line,
            start_character: 0,
            end_line,
            end_character: u64::MAX,
        };
        let _ = self
            .lsp
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .request_inlay_hints(&uri, range, Instant::now());
    }

    pub fn semantic_tokens(&self, document: Arc<DocumentHandle>) -> Vec<SemanticTokenInfo> {
        let uri = file_uri(Path::new(&document.path()));
        self.lsp
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .semantic_tokens(&uri)
            .iter()
            .map(|token| SemanticTokenInfo {
                line: token.line,
                start: token.start,
                length: token.length,
                token_type: token.token_type,
            })
            .collect()
    }

    pub fn inlay_hints(&self, document: Arc<DocumentHandle>) -> Vec<InlayHintInfo> {
        let uri = file_uri(Path::new(&document.path()));
        self.lsp
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .inlay_hints(&uri)
            .iter()
            .map(|hint| InlayHintInfo {
                line: hint.line,
                character: hint.character,
                label: hint.label.clone(),
            })
            .collect()
    }

    pub fn problems(&self) -> Vec<ProblemInfo> {
        let lsp = self.lsp.lock().unwrap_or_else(|e| e.into_inner());
        lsp.problems()
            .into_iter()
            .map(|diagnostic| ProblemInfo {
                uri: diagnostic.uri.clone(),
                line: diagnostic.range.start_line,
                column: diagnostic.range.start_character,
                severity: match diagnostic.severity {
                    galaxy_core::lsp::DiagnosticSeverity::Error => 1,
                    galaxy_core::lsp::DiagnosticSeverity::Warning => 2,
                    galaxy_core::lsp::DiagnosticSeverity::Information => 3,
                    galaxy_core::lsp::DiagnosticSeverity::Hint => 4,
                },
                message: diagnostic.message.clone(),
                source: diagnostic.source.clone(),
                code: diagnostic.code.clone(),
            })
            .collect()
    }

    pub fn sync_lsp_document(&self, document: Arc<DocumentHandle>) {
        let path = document.path();
        if path.is_empty() {
            return;
        }
        let uri = file_uri(Path::new(&path));
        self.lsp
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .note_full_text(&uri, document.text(), Instant::now());
    }

    pub fn save_lsp_document(&self, document: Arc<DocumentHandle>) {
        let path = document.path();
        if path.is_empty() {
            return;
        }
        let uri = file_uri(Path::new(&path));
        let _ = self
            .lsp
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .save_document(&uri);
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

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct LspProgressInfo {
    pub title: String,
    pub message: Option<String>,
    pub percentage: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct LspStatusInfo {
    pub servers: u64,
    pub progress: Vec<LspProgressInfo>,
    pub error: Option<String>,
    pub processes: Vec<LspProcessInfo>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct LspProcessInfo {
    pub language: String,
    pub pid: u64,
    pub resident_memory_kib: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SemanticTokenInfo {
    pub line: u64,
    pub start: u64,
    pub length: u64,
    pub token_type: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct InlayHintInfo {
    pub line: u64,
    pub character: u64,
    pub label: String,
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

fn rename_edit(edit: &galaxy_core::lsp::RenameEdit) -> RenameEdit {
    RenameEdit {
        uri: edit.uri.clone(),
        start_line: edit.range.start_line,
        start_character: edit.range.start_character,
        end_line: edit.range.end_line,
        end_character: edit.range.end_character,
        new_text: edit.new_text.clone(),
    }
}

enum DefinitionKind {
    Definition,
    Type,
    Implementation,
}

fn request_definition_kind(
    core: &Core,
    document: Arc<DocumentHandle>,
    line: u64,
    character: u64,
    kind: DefinitionKind,
) -> bool {
    let path = document.path();
    if path.is_empty() {
        return false;
    }
    let uri = file_uri(Path::new(&path));
    let mut lsp = core.lsp.lock().unwrap_or_else(|e| e.into_inner());
    let result = match kind {
        DefinitionKind::Definition => lsp.request_definition(&uri, line, character, Instant::now()),
        DefinitionKind::Type => lsp.request_type_definition(&uri, line, character, Instant::now()),
        DefinitionKind::Implementation => {
            lsp.request_implementation(&uri, line, character, Instant::now())
        }
    };
    result.ok().flatten().is_some()
}

/// One row of the file explorer.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FileEntry {
    pub name: String,
    /// Path relative to the workspace root, `/` separated.
    pub rel_path: String,
    pub is_dir: bool,
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
        Ok(galaxy_core::git::branches(&self.inner.root())?
            .into_iter()
            .map(|branch| GitBranchInfo {
                name: branch.name,
                current: branch.current,
                remote: branch.remote,
            })
            .collect())
    }

    pub fn git_checkout(&self, name: String) -> Result<(), CoreError> {
        galaxy_core::git::checkout(&self.inner.root(), &name)?;
        Ok(())
    }

    pub fn git_create_branch(&self, name: String) -> Result<(), CoreError> {
        galaxy_core::git::create_branch(&self.inner.root(), &name)?;
        Ok(())
    }

    pub fn git_stage(&self, path: String) -> Result<(), CoreError> {
        galaxy_core::git::stage(&self.inner.root(), &path)?;
        Ok(())
    }

    pub fn git_stage_all(&self) -> Result<(), CoreError> {
        galaxy_core::git::stage_all(&self.inner.root())?;
        Ok(())
    }

    pub fn git_unstage(&self, path: String) -> Result<(), CoreError> {
        galaxy_core::git::unstage(&self.inner.root(), &path)?;
        Ok(())
    }

    pub fn git_unstage_all(&self) -> Result<(), CoreError> {
        galaxy_core::git::unstage_all(&self.inner.root())?;
        Ok(())
    }

    pub fn git_commit(&self, message: String) -> Result<(), CoreError> {
        galaxy_core::git::commit(&self.inner.root(), &message)?;
        Ok(())
    }

    pub fn terminal(&self, cols: u16, rows: u16) -> Result<Arc<TerminalHandle>, CoreError> {
        Ok(Arc::new(TerminalHandle {
            session: TerminalSession::spawn(self.inner.root(), cols, rows)?,
        }))
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
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DiagnosticSpan {
    pub start: u64,
    pub end: u64,
    pub severity: u8,
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
    /// Tab-expanded leading indentation columns for vertical guides.
    pub indent_guides: Vec<u64>,
    /// Matching bracket cells on this line, when the primary caret is beside one.
    pub brackets: Vec<LineSpan>,
    pub diagnostics: Vec<DiagnosticSpan>,
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
                        })
                        .collect(),
                })
                .collect(),
            primary: CursorInfo {
                line: s.primary.line as u64,
                col: s.primary.col as u64,
                col_utf16: s.primary.col_utf16 as u64,
            },
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

    /// Plain lines without selection info.
    pub fn lines(&self, first: u64, count: u64) -> Vec<String> {
        self.lock()
            .editor
            .doc
            .lines(clamp_usize(first), clamp_usize(count))
    }

    /// One snapshot per frame: visible lines, selections, carets, cursor.
    pub fn viewport(&self, first_line: u64, count: u64) -> ViewportSnapshot {
        let st = &mut *self.lock();
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
        let mut snap: ViewportSnapshot = if let Some((language, tree)) = syntax {
            st.editor
                .viewport_with_syntax(first_line, count, &tree, language)
                .into()
        } else {
            st.editor.viewport(first_line, count).into()
        };
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
                    });
                }
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
        {
            let ed = &mut st.editor;
            let (doc, sel) = (&ed.doc, &ed.sel);
            let (tx, kind) = match &command {
                EditCommand::InsertText { text } => {
                    let character =
                        (text.chars().count() == 1).then(|| text.chars().next().unwrap());
                    if let Some(open) = character.filter(|c| edit::pair_for(*c).is_some()) {
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
                        (edit::insert_text(doc, sel, text), insert_kind(text))
                    }
                }
                EditCommand::Paste { text } => (edit::paste(doc, sel, text), EditKind::Other),
                EditCommand::Backspace => (edit::backspace(doc, sel, &cfg), EditKind::Delete),
                EditCommand::DeleteForward => (edit::delete_forward(doc, sel), EditKind::Delete),
                EditCommand::Newline => (edit::newline(doc, sel, &cfg), EditKind::Other),
                EditCommand::Indent => (edit::indent(doc, sel, &cfg), EditKind::Other),
                EditCommand::Outdent => (edit::outdent(doc, sel, &cfg), EditKind::Other),
                EditCommand::ToggleComment => match comment_marker {
                    Some(marker) => (edit::toggle_comment(doc, sel, marker), EditKind::Other),
                    None => return,
                },
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
                finder: None,
                find_error: None,
                syntax,
                diagnostics: Vec::new(),
                diagnostics_revision: 0,
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
        }]);
        let snap = doc.viewport(0, 2);
        assert_eq!(snap.lines[0].diagnostics[0].start, 1);
        assert_eq!(snap.lines[0].diagnostics[0].end, 3);
        assert_eq!(snap.lines[1].diagnostics[0].start, 0);
        assert_eq!(snap.lines[1].diagnostics[0].end, 2);
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
