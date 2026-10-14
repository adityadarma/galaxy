//! JSON-RPC 2.0 transport for language servers over stdio (M4-01).
//!
//! The transport owns a Tokio child process. A reader task decodes framed
//! messages and forwards them through a standard channel so the rest of the
//! editor can remain synchronous. No UI call waits for server output.

use std::collections::{HashMap, HashSet};
use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::Duration;
use std::time::Instant;

use ropey::Rope;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::runtime::{Builder, Runtime};

use crate::change::ChangeSet;
use crate::document::Document;
use crate::plugins::{Catalog, companion_language};

pub const DOCUMENT_SYNC_DEBOUNCE: Duration = Duration::from_millis(50);

/// Fuzz-safe JSON-RPC payload validation shared by the transport tests.
pub fn parse_json_rpc_payload(bytes: &[u8]) -> Result<Value, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if !value.is_object() {
        return Err("JSON-RPC payload must be an object".into());
    }
    Ok(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub uri: String,
    pub range: Range,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverInfo {
    pub contents: String,
    pub range: Option<Range>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    pub detail: Option<String>,
    pub insert_text: String,
    pub is_snippet: bool,
    pub sort_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureInfo {
    pub label: String,
    pub documentation: Option<String>,
    pub parameters: Vec<String>,
    pub active_parameter: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionLocation {
    pub uri: String,
    pub range: Range,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameEdit {
    pub uri: String,
    pub range: Range,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeActionInfo {
    pub title: String,
    pub kind: Option<String>,
    pub edits: Vec<RenameEdit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentSymbolInfo {
    pub uri: Option<String>,
    pub name: String,
    pub kind: u64,
    pub range: Range,
    pub children: Vec<DocumentSymbolInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticTokenInfo {
    pub line: u64,
    pub start: u64,
    pub length: u64,
    pub token_type: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlayHintInfo {
    pub line: u64,
    pub character: u64,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProgressInfo {
    pub token: String,
    pub title: String,
    pub message: Option<String>,
    pub percentage: Option<f64>,
    pub active: bool,
}

/// Latest server diagnostics, replaced atomically per URI by
/// `textDocument/publishDiagnostics` as required by LSP.
#[derive(Debug, Default)]
pub struct DiagnosticStore {
    by_uri: HashMap<String, Vec<Diagnostic>>,
    /// Per-server lists when several servers serve one document (PHP:
    /// intelephense plus laravel-lsp). `by_uri` is their concatenation, so
    /// one server publishing never wipes the other's diagnostics.
    by_server: HashMap<String, HashMap<String, Vec<Diagnostic>>>,
}

impl DiagnosticStore {
    pub fn publish(&mut self, message: &Value) -> Result<bool, String> {
        self.publish_from("", message)
    }

    pub fn publish_from(&mut self, server: &str, message: &Value) -> Result<bool, String> {
        if message.get("method").and_then(Value::as_str) != Some("textDocument/publishDiagnostics")
        {
            return Ok(false);
        }
        let params = message
            .get("params")
            .and_then(Value::as_object)
            .ok_or("publishDiagnostics missing params")?;
        let uri = params
            .get("uri")
            .and_then(Value::as_str)
            .ok_or("publishDiagnostics missing uri")?
            .to_owned();
        let diagnostics = params
            .get("diagnostics")
            .and_then(Value::as_array)
            .ok_or("publishDiagnostics missing diagnostics")?
            .iter()
            .map(|value| parse_diagnostic(&uri, value))
            .collect::<Result<Vec<_>, _>>()?;
        let servers = self.by_server.entry(uri.clone()).or_default();
        servers.insert(server.to_owned(), diagnostics);
        let mut names: Vec<_> = servers.keys().cloned().collect();
        names.sort();
        let merged = names
            .iter()
            .flat_map(|name| servers[name].iter().cloned())
            .collect();
        self.by_uri.insert(uri, merged);
        Ok(true)
    }

    pub fn for_uri(&self, uri: &str) -> &[Diagnostic] {
        self.by_uri.get(uri).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn problems(&self) -> Vec<&Diagnostic> {
        let mut out: Vec<_> = self.by_uri.values().flatten().collect();
        out.sort_by_key(|diagnostic| {
            (
                &diagnostic.uri,
                diagnostic.range.start_line,
                diagnostic.range.start_character,
                diagnostic.severity,
            )
        });
        out
    }
}

fn parse_diagnostic(uri: &str, value: &Value) -> Result<Diagnostic, String> {
    let range = value
        .get("range")
        .and_then(Value::as_object)
        .ok_or("diagnostic missing range")?;
    let position = |name: &str| -> Result<(u64, u64), String> {
        let position = range
            .get(name)
            .and_then(Value::as_object)
            .ok_or_else(|| format!("diagnostic missing range.{name}"))?;
        Ok((
            position
                .get("line")
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("diagnostic missing {name}.line"))?,
            position
                .get("character")
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("diagnostic missing {name}.character"))?,
        ))
    };
    let (start_line, start_character) = position("start")?;
    let (end_line, end_character) = position("end")?;
    let severity = match value.get("severity").and_then(Value::as_u64).unwrap_or(1) {
        1 => DiagnosticSeverity::Error,
        2 => DiagnosticSeverity::Warning,
        3 => DiagnosticSeverity::Information,
        _ => DiagnosticSeverity::Hint,
    };
    let code = value.get("code").and_then(|code| {
        code.as_str()
            .map(str::to_owned)
            .or_else(|| code.as_u64().map(|code| code.to_string()))
    });
    Ok(Diagnostic {
        uri: uri.to_owned(),
        range: Range {
            start_line,
            start_character,
            end_line,
            end_character,
        },
        severity,
        message: value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        source: value
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_owned),
        code,
    })
}

/// One incremental `contentChanges` entry: replace `range` (UTF-16, in the
/// old text) with `text`. Covers everything between the first and last
/// differing byte, so a burst or multi-cursor batch still sends one span.
#[derive(Debug, Clone, PartialEq)]
pub struct IncrementalChange {
    pub range: Range,
    pub range_length: u64,
    pub text: String,
}

/// Diffs two ropes (see [`crate::rope_diff`]) and converts the span to LSP
/// UTF-16 coordinates in the old text. `None` when the texts are equal.
pub fn incremental_change(old: &Rope, new: &Rope) -> Option<IncrementalChange> {
    let change = crate::rope_diff::changed_bytes(old, new)?;
    let old_start = old.byte_to_char(change.start);
    let old_end = old.byte_to_char(change.old_end);
    Some(IncrementalChange {
        range: Range {
            start_line: old.char_to_line(old_start) as u64,
            start_character: utf16_column(old, old_start) as u64,
            end_line: old.char_to_line(old_end) as u64,
            end_character: utf16_column(old, old_end) as u64,
        },
        range_length: (old.char_to_utf16_cu(old_end) - old.char_to_utf16_cu(old_start)) as u64,
        text: new.byte_slice(change.start..change.new_end).to_string(),
    })
}

fn utf16_column(rope: &Rope, char_idx: usize) -> usize {
    let line_start = rope.line_to_char(rope.char_to_line(char_idx));
    rope.char_to_utf16_cu(char_idx) - rope.char_to_utf16_cu(line_start)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start_line: u64,
    pub start_character: u64,
    pub end_line: u64,
    pub end_character: u64,
}

/// Tracks LSP document versions and debounces outgoing changes.
///
/// `shadow` is the text the server currently has. It is a `Rope` clone, so
/// it shares nodes with the editor's document instead of holding a second
/// full copy; only nodes edited since the last sync are duplicated. Each
/// flush diffs `shadow` against the newest text and sends just the changed
/// span, so no full-document `String` lives past `didOpen` serialization.
#[derive(Debug)]
pub struct DocumentSync {
    uri: String,
    language_id: String,
    version: u64,
    shadow: Rope,
    pending: Option<PendingSync>,
}

#[derive(Debug)]
struct PendingSync {
    target: Rope,
    due_at: Instant,
}

impl DocumentSync {
    pub fn new(uri: String, language_id: String, text: Rope) -> Self {
        Self {
            uri,
            language_id,
            version: 1,
            shadow: text,
            pending: None,
        }
    }

    /// Serializes the full text only for this one message. Also used to
    /// replay `didOpen` after a server (re)initializes.
    pub fn did_open(&self) -> Value {
        let text = self.pending.as_ref().map_or(&self.shadow, |p| &p.target);
        serde_json::json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":self.uri,"languageId":self.language_id,"version":self.version,"text":text.to_string()}}})
    }

    /// Starts tracking `doc` as opened at its current text.
    pub fn for_document(uri: String, language_id: String, doc: &Document) -> Self {
        Self::new(uri, language_id, doc.rope().clone())
    }

    /// Queues `doc`'s current text (a shared rope handle, not a copy).
    pub fn note_document(&mut self, doc: &Document, now: Instant) {
        self.note_text(doc.rope(), now);
    }

    /// `didOpen` carries the newest text, so nothing remains to send.
    fn mark_opened(&mut self) {
        if let Some(pending) = self.pending.take() {
            self.shadow = pending.target;
        }
    }

    /// Queues the document's current text. Cheap: clones a rope handle.
    pub fn note_text(&mut self, text: &Rope, now: Instant) {
        self.pending = Some(PendingSync {
            target: text.clone(),
            due_at: now + DOCUMENT_SYNC_DEBOUNCE,
        });
    }

    pub fn note_full_text(&mut self, text: String, now: Instant) {
        self.note_text(&Rope::from_str(&text), now);
    }

    /// Records a change; only `after` matters because each flush diffs the
    /// server's text against the newest one.
    pub fn note_change(
        &mut self,
        _before: &Document,
        after: &Document,
        _changes: &ChangeSet,
        now: Instant,
    ) {
        self.note_text(after.rope(), now);
    }

    pub fn flush(&mut self, now: Instant) -> Option<Value> {
        if self.pending.as_ref().is_none_or(|p| now < p.due_at) {
            return None;
        }
        let target = self.pending.take()?.target;
        let change = incremental_change(&self.shadow, &target);
        self.shadow = target;
        let change = change?;
        self.version += 1;
        let range = change.range;
        let content = serde_json::json!({"range":{"start":{"line":range.start_line,"character":range.start_character},"end":{"line":range.end_line,"character":range.end_character}},"rangeLength":change.range_length,"text":change.text});
        Some(
            serde_json::json!({"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":self.uri,"version":self.version},"contentChanges":[content]}}),
        )
    }

    pub fn did_save(&self) -> Value {
        serde_json::json!({"jsonrpc":"2.0","method":"textDocument/didSave","params":{"textDocument":{"uri":self.uri}}})
    }
    pub fn did_close(&self) -> Value {
        serde_json::json!({"jsonrpc":"2.0","method":"textDocument/didClose","params":{"textDocument":{"uri":self.uri}}})
    }
}

/// A language server is shared by all documents with this language and root.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LspKey {
    pub language: String,
    pub root: PathBuf,
}

/// Process launch details. Detection and user overrides arrive in M4-03.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub program: String,
    pub args: Vec<String>,
    pub log_traffic: bool,
    pub initialization_options: Value,
    pub restart_backoff: [Duration; 3],
}

/// Server configuration supplied by global or workspace settings. The JSON
/// shape is intentionally small and forwards unknown option contents intact:
/// `{ "rust": { "command": "rust-analyzer", "args": [],
/// "initializationOptions": { ... } } }`.
#[derive(Debug, Default, Clone)]
pub struct LspSettings {
    pub servers: HashMap<String, ServerConfig>,
}

impl LspSettings {
    pub fn from_json(value: &Value) -> Result<Self, String> {
        let Some(servers) = value.get("servers").and_then(Value::as_object) else {
            return Ok(Self::default());
        };
        let mut out = Self::default();
        for (language, entry) in servers {
            let object = entry
                .as_object()
                .ok_or_else(|| format!("server config for {language} must be an object"))?;
            let program = object
                .get("command")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("server config for {language} needs string command"))?;
            let args = object
                .get("args")
                .map(|args| {
                    args.as_array()
                        .ok_or_else(|| format!("server args for {language} must be an array"))?
                        .iter()
                        .map(|arg| {
                            arg.as_str().map(str::to_owned).ok_or_else(|| {
                                format!("server args for {language} must be strings")
                            })
                        })
                        .collect()
                })
                .transpose()?
                .unwrap_or_default();
            let restart_backoff = object
                .get("restartBackoffMs")
                .map(|values| {
                    let values = values.as_array().ok_or_else(|| {
                        format!("restartBackoffMs for {language} must be an array")
                    })?;
                    if values.len() != 3 {
                        return Err(format!(
                            "restartBackoffMs for {language} must contain exactly three values"
                        ));
                    }
                    let mut backoff = [Duration::ZERO; 3];
                    for (index, value) in values.iter().enumerate() {
                        let milliseconds = value.as_u64().ok_or_else(|| {
                            format!("restartBackoffMs for {language} must contain milliseconds")
                        })?;
                        backoff[index] = Duration::from_millis(milliseconds);
                    }
                    Ok(backoff)
                })
                .transpose()?
                .unwrap_or([
                    Duration::ZERO,
                    Duration::from_secs(1),
                    Duration::from_secs(2),
                ]);
            out.servers.insert(
                language.to_ascii_lowercase(),
                ServerConfig {
                    program: program.to_owned(),
                    args,
                    log_traffic: object
                        .get("logTraffic")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    initialization_options: object
                        .get("initializationOptions")
                        .cloned()
                        .unwrap_or(Value::Null),
                    restart_backoff,
                },
            );
        }
        Ok(out)
    }

    /// Resolves a server override first, otherwise the built-in language
    /// default. Relative or bare commands are found in `PATH` at launch time.
    pub fn resolve(&self, language: &str) -> Option<ServerConfig> {
        self.resolve_for_root(language, None)
    }

    /// Like [`Self::resolve`], but lets root-sensitive plugins inspect the
    /// workspace. Uses the built-in plugins only; see [`Self::resolve_in`].
    pub fn resolve_for_root(&self, language: &str, root: Option<&Path>) -> Option<ServerConfig> {
        self.resolve_in(language, root, &Catalog::builtin())
    }

    /// A settings override first, otherwise the best enabled plugin in
    /// `catalog` whose command resolves.
    pub fn resolve_in(
        &self,
        language: &str,
        root: Option<&Path>,
        catalog: &Catalog,
    ) -> Option<ServerConfig> {
        self.override_for(language).or_else(|| {
            catalog
                .primary(language, root, &find_in_path)
                .map(|(_, config)| config)
        })
    }

    /// Overrides match the exact language first (`blade`), then the shared
    /// server kind (`php`), so one PHP entry also covers Blade views.
    fn override_for(&self, language: &str) -> Option<ServerConfig> {
        let lower = language.to_ascii_lowercase();
        self.servers.get(&lower).cloned().or_else(|| {
            default_server_kind(language).and_then(|kind| self.servers.get(kind).cloned())
        })
    }

    /// Loads global then workspace settings; workspace entries win per
    /// language. Missing files are fine; unreadable or invalid ones are
    /// returned as errors next to whatever did load.
    pub fn load(global: Option<&Path>, workspace_root: Option<&Path>) -> (Self, Vec<String>) {
        let mut out = Self::default();
        let mut errors = Vec::new();
        let workspace = workspace_root.map(|root| root.join(".galaxy/settings.json"));
        for path in [global.map(Path::to_path_buf), workspace]
            .into_iter()
            .flatten()
        {
            match Self::load_file(&path) {
                Ok(Some(settings)) => out.servers.extend(settings.servers),
                Ok(None) => {}
                Err(error) => errors.push(format!("{}: {error}", path.display())),
            }
        }
        (out, errors)
    }

    /// Default global settings file used by the macOS app.
    pub fn global_settings_path() -> Option<PathBuf> {
        env::var_os("HOME").map(|home| {
            PathBuf::from(home).join("Library/Application Support/Galaxy/settings.json")
        })
    }

    fn load_file(path: &Path) -> Result<Option<Self>, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let value: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        // Documented shape is `{ "lsp": { "servers": … } }`; a top-level
        // `servers` key is still accepted for older files.
        let section = value.get("lsp").unwrap_or(&value);
        Self::from_json(section).map(Some)
    }
}

/// Maps editor display names (`"PHP"`, `"TypeScript React"`) to the
/// `languageId` values LSP servers expect. Blade views are detected from the
/// `.blade.php` suffix because they share the PHP grammar.
pub fn lsp_language_id(language: &str, path: &str) -> String {
    if path.to_ascii_lowercase().ends_with(".blade.php") {
        return "blade".to_owned();
    }
    let normalized = language.to_ascii_lowercase().replace([' ', '-'], "");
    match normalized.as_str() {
        "typescriptreact" | "tsx" => "typescriptreact",
        "javascriptreact" | "jsx" => "javascriptreact",
        "js" => "javascript",
        "shellscript" | "bash" => "shellscript",
        _ => return normalized,
    }
    .to_owned()
}

/// Server identity used for [`LspKey::language`]. Languages served by the
/// same server (PHP and Blade, TS and JS) share one process per root.
pub fn lsp_server_language(language: &str) -> String {
    default_server_kind(language)
        .map(str::to_owned)
        .unwrap_or_else(|| language.to_ascii_lowercase())
}

#[cfg(test)]
fn default_server(
    language: &str,
    root: Option<&Path>,
    find: &dyn Fn(&str) -> Option<PathBuf>,
) -> Option<ServerConfig> {
    Catalog::builtin()
        .primary(language, root, find)
        .map(|(_, config)| config)
}

/// Servers to attach next to `primary` for this language and root, each with
/// the `LspKey::language` it runs under. Laravel projects get `laravel-lsp`
/// for config/route/view navigation unless it already is the primary server.
pub fn companion_servers(
    catalog: &Catalog,
    language: &str,
    root: Option<&Path>,
    primary: &ServerConfig,
) -> Vec<(String, ServerConfig)> {
    let kind = lsp_server_language(language);
    catalog
        .companions(language, root, &primary.program, &find_in_path)
        .into_iter()
        .map(|(plugin, config)| (companion_language(&kind, &plugin.manifest.id), config))
        .collect()
}

#[cfg(test)]
fn companion_server_with(
    language: &str,
    root: Option<&Path>,
    primary: &ServerConfig,
    find: &dyn Fn(&str) -> Option<PathBuf>,
) -> Option<ServerConfig> {
    Catalog::builtin()
        .companions(language, root, &primary.program, find)
        .into_iter()
        .next()
        .map(|(_, config)| config)
}

#[cfg(test)]
fn companion_server(
    language: &str,
    root: Option<&Path>,
    primary: &ServerConfig,
) -> Option<ServerConfig> {
    companion_server_with(language, root, primary, &find_in_path)
}

pub(crate) fn default_server_kind(language: &str) -> Option<&'static str> {
    let normalized = language.to_ascii_lowercase().replace([' ', '-'], "");
    match normalized.as_str() {
        "rust" => Some("rust"),
        "typescript" | "typescriptreact" | "tsx" | "javascript" | "javascriptreact" | "js" => {
            Some("typescript")
        }
        "python" => Some("python"),
        "go" => Some("go"),
        "php" | "blade" => Some("php"),
        _ => None,
    }
}

/// Finds an executable with no shell expansion. Absolute or relative paths
/// are accepted only when executable; bare names are searched through PATH.
pub fn find_in_path(program: &str) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return is_executable(path).then(|| path.to_path_buf());
    }
    search_directories()
        .into_iter()
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

/// `PATH` plus common install locations. GUI apps on macOS start with a
/// minimal `PATH`, so script servers (`#!/usr/bin/env php`, `node`) also need
/// these directories in the child environment, see [`server_path_env`].
fn search_directories() -> Vec<PathBuf> {
    let mut directories = env::var_os("PATH")
        .into_iter()
        .flat_map(|value| {
            value
                .to_string_lossy()
                .split(':')
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        directories.extend([
            home.join(".composer/vendor/bin"),
            home.join(".config/composer/vendor/bin"),
            home.join(".local/bin"),
            // `cargo install` (rust-analyzer) and `go install` (gopls).
            home.join(".cargo/bin"),
            home.join("go/bin"),
            // Laravel Herd ships its own php binary.
            home.join("Library/Application Support/Herd/bin"),
        ]);
        let nvm_bin = home.join(".nvm/versions/node");
        if let Ok(entries) = std::fs::read_dir(nvm_bin) {
            for entry in entries.flatten() {
                directories.push(entry.path().join("bin"));
            }
        }
    }
    directories.extend([
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ]);
    let mut seen = HashSet::new();
    directories.retain(|dir| !dir.as_os_str().is_empty() && seen.insert(dir.clone()));
    directories
}

/// `PATH` for a server child: the server's own directory first, then the
/// search directories, so `#!/usr/bin/env php` resolves when Galaxy is
/// launched from Finder with launchd's minimal `PATH`.
pub(crate) fn server_path_env(program: &str) -> std::ffi::OsString {
    let mut directories = Vec::new();
    if let Some(parent) = Path::new(program)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        directories.push(parent.to_path_buf());
    }
    directories.extend(search_directories());
    env::join_paths(directories).unwrap_or_else(|_| env::var_os("PATH").unwrap_or_default())
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_file()
        && path
            .metadata()
            .is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
}

/// Lifecycle events observed by the synchronous editor core.
#[derive(Debug, Clone, PartialEq)]
pub enum LspEvent {
    Initialized(LspKey),
    Response {
        key: LspKey,
        id: u64,
        result: Option<Value>,
        error: Option<Value>,
    },
    TimedOut {
        key: LspKey,
        id: u64,
        method: String,
    },
    Message(LspKey, Value),
    Error(LspKey, String),
    Closed(LspKey),
}

struct ManagedServer {
    transport: LspTransport,
    initialized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerProcessInfo {
    pub language: String,
    pub pid: u32,
    pub resident_memory_kib: Option<u64>,
}

#[derive(Debug, Clone)]
struct PendingRequest {
    method: String,
    deadline: Instant,
}

/// Owns one server per `(language, workspace root)` and drives the minimal
/// JSON-RPC lifecycle. General request IDs and cancellation are M4-05.
#[derive(Default)]
pub struct LspManager {
    servers: HashMap<LspKey, ManagedServer>,
    configs: HashMap<LspKey, ServerConfig>,
    restart_history: HashMap<LspKey, Vec<Instant>>,
    pending_restarts: HashMap<LspKey, Instant>,
    stopped: HashSet<LspKey>,
    documents: HashMap<String, (LspKey, DocumentSync)>,
    /// Extra servers attached to a document next to its primary one, e.g.
    /// laravel-lsp beside intelephense. They get the full document
    /// lifecycle; feature requests fan out only where results merge cleanly
    /// (definitions, diagnostics).
    companions: HashMap<String, Vec<(LspKey, DocumentSync)>>,
    requests: HashMap<(LspKey, u64), PendingRequest>,
    next_request_id: u64,
    diagnostics: DiagnosticStore,
    hover: HashMap<String, HoverInfo>,
    hover_requests: HashMap<(LspKey, u64), String>,
    completion: HashMap<String, Vec<CompletionItem>>,
    completion_requests: HashMap<(LspKey, u64), String>,
    signatures: HashMap<String, SignatureInfo>,
    signature_requests: HashMap<(LspKey, u64), String>,
    definition_locations: HashMap<String, Vec<DefinitionLocation>>,
    definition_requests: HashMap<(LspKey, u64), String>,
    references: HashMap<String, Vec<DefinitionLocation>>,
    reference_requests: HashMap<(LspKey, u64), String>,
    rename_edits: HashMap<String, Vec<RenameEdit>>,
    rename_requests: HashMap<(LspKey, u64), String>,
    code_actions: HashMap<String, Vec<CodeActionInfo>>,
    code_action_requests: HashMap<(LspKey, u64), String>,
    formatting_edits: HashMap<String, Vec<RenameEdit>>,
    formatting_requests: HashMap<(LspKey, u64), String>,
    range_formatting_requests: HashMap<(LspKey, u64), String>,
    on_type_formatting_requests: HashMap<(LspKey, u64), String>,
    document_symbols: HashMap<String, Vec<DocumentSymbolInfo>>,
    document_symbol_requests: HashMap<(LspKey, u64), String>,
    workspace_symbols: HashMap<String, Vec<DocumentSymbolInfo>>,
    workspace_symbol_requests: HashMap<(LspKey, u64), String>,
    semantic_tokens: HashMap<String, Vec<SemanticTokenInfo>>,
    semantic_token_requests: HashMap<(LspKey, u64), String>,
    inlay_hints: HashMap<String, Vec<InlayHintInfo>>,
    inlay_hint_requests: HashMap<(LspKey, u64), String>,
    progress: HashMap<String, ProgressInfo>,
    server_errors: Vec<String>,
}

impl LspManager {
    /// Starts and initializes a server unless an equivalent key already runs.
    /// `true` means a child was spawned; `false` means the shared instance won.
    pub fn ensure_server(&mut self, key: LspKey, config: &ServerConfig) -> io::Result<bool> {
        self.stopped.remove(&key);
        if self.servers.contains_key(&key) {
            return Ok(false);
        }
        let transport =
            LspTransport::spawn(&config.program, &config.args, config.log_traffic, &key.root)?;
        transport.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": format!("file://{}", key.root.to_string_lossy()),
                "capabilities": {},
                "initializationOptions": config.initialization_options
            }
        }))?;
        self.servers.insert(
            key.clone(),
            ManagedServer {
                transport,
                initialized: false,
            },
        );
        self.configs.insert(key, config.clone());
        Ok(true)
    }

    pub fn stop_server(&mut self, key: &LspKey) -> bool {
        self.stopped.insert(key.clone());
        self.pending_restarts.remove(key);
        self.requests
            .retain(|(request_key, _), _| request_key != key);
        self.servers.remove(key).is_some()
    }

    /// Replaces the launch details used by the next [`Self::restart_server`],
    /// e.g. after a plugin was installed. No-op for unknown keys.
    pub fn set_config(&mut self, key: &LspKey, config: ServerConfig) {
        if let Some(existing) = self.configs.get_mut(key) {
            *existing = config;
        }
    }

    pub fn restart_server(&mut self, key: &LspKey) -> io::Result<bool> {
        self.stopped.remove(key);
        self.pending_restarts.remove(key);
        let Some(config) = self.configs.get(key).cloned() else {
            return Ok(false);
        };
        self.stop_running_server(key);
        self.ensure_server(key.clone(), &config)
    }

    fn stop_running_server(&mut self, key: &LspKey) {
        self.requests
            .retain(|(request_key, _), _| request_key != key);
        self.servers.remove(key);
    }

    fn auto_restart(&mut self, key: &LspKey, now: Instant) {
        if self.stopped.contains(key) {
            return;
        }
        let restart_count = self.restart_history.entry(key.clone()).or_default();
        restart_count.retain(|started| now.duration_since(*started) < Duration::from_secs(300));
        if restart_count.len() >= 3 {
            self.server_errors.push(format!(
                "LSP server restart limit reached: {}",
                key.language
            ));
            return;
        }
        let delay = self
            .configs
            .get(key)
            .map(|config| config.restart_backoff[restart_count.len()])
            .unwrap_or(Duration::ZERO);
        restart_count.push(now);
        self.pending_restarts.insert(key.clone(), now + delay);
    }

    fn restart_due_servers(&mut self, now: Instant) {
        let due: Vec<_> = self
            .pending_restarts
            .iter()
            .filter(|(_, deadline)| **deadline <= now)
            .map(|(key, _)| key.clone())
            .collect();
        for key in due {
            self.pending_restarts.remove(&key);
            if self.stopped.contains(&key) {
                continue;
            }
            let Some(config) = self.configs.get(&key).cloned() else {
                continue;
            };
            self.stop_running_server(&key);
            if let Err(error) = self.ensure_server(key.clone(), &config) {
                self.server_errors
                    .push(format!("LSP restart failed: {error}"));
            }
        }
    }

    pub fn len(&self) -> usize {
        self.servers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    /// Consumes `textDocument/publishDiagnostics`; other notifications return
    /// false so callers can route them to their feature-specific handlers.
    pub fn publish_diagnostics(&mut self, message: &Value) -> Result<bool, String> {
        self.diagnostics.publish(message)
    }

    pub fn diagnostics(&self, uri: &str) -> &[Diagnostic] {
        self.diagnostics.for_uri(uri)
    }

    pub fn problems(&self) -> Vec<&Diagnostic> {
        self.diagnostics.problems()
    }

    pub fn request_hover(
        &mut self,
        uri: &str,
        line: u64,
        character: u64,
        now: Instant,
    ) -> io::Result<Option<u64>> {
        let Some(key) = self.documents.get(uri).map(|(key, _)| key.clone()) else {
            return Ok(None);
        };
        self.hover.remove(uri);
        let id = self.request(
            &key,
            "textDocument/hover".into(),
            serde_json::json!({
                "textDocument": { "uri": uri }, "position": { "line": line, "character": character }
            }),
            Duration::from_millis(750),
            now,
        )?;
        self.hover_requests.insert((key, id), uri.to_owned());
        Ok(Some(id))
    }

    pub fn hover(&self, uri: &str) -> Option<&HoverInfo> {
        self.hover.get(uri)
    }

    pub fn request_completion(
        &mut self,
        uri: &str,
        line: u64,
        character: u64,
        now: Instant,
    ) -> io::Result<Option<u64>> {
        let Some(key) = self.documents.get(uri).map(|(key, _)| key.clone()) else {
            return Ok(None);
        };
        let id = self.request(&key, "textDocument/completion".into(), serde_json::json!({
            "textDocument": { "uri": uri }, "position": { "line": line, "character": character }, "context": { "triggerKind": 1 }
        }), Duration::from_millis(1000), now)?;
        self.completion_requests.insert((key, id), uri.to_owned());
        Ok(Some(id))
    }

    pub fn completions(&self, uri: &str) -> &[CompletionItem] {
        self.completion.get(uri).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn request_signature_help(
        &mut self,
        uri: &str,
        line: u64,
        character: u64,
        now: Instant,
    ) -> io::Result<Option<u64>> {
        let Some(key) = self.documents.get(uri).map(|(key, _)| key.clone()) else {
            return Ok(None);
        };
        let id = self.request(&key, "textDocument/signatureHelp".into(), serde_json::json!({
            "textDocument": { "uri": uri }, "position": { "line": line, "character": character }, "context": { "triggerKind": 1 }
        }), Duration::from_millis(750), now)?;
        self.signature_requests.insert((key, id), uri.to_owned());
        Ok(Some(id))
    }

    pub fn signature_help(&self, uri: &str) -> Option<&SignatureInfo> {
        self.signatures.get(uri)
    }

    pub fn request_definition(
        &mut self,
        uri: &str,
        line: u64,
        character: u64,
        now: Instant,
    ) -> io::Result<Option<u64>> {
        self.request_definition_kind(uri, line, character, "textDocument/definition", now)
    }

    pub fn request_type_definition(
        &mut self,
        uri: &str,
        line: u64,
        character: u64,
        now: Instant,
    ) -> io::Result<Option<u64>> {
        self.request_definition_kind(uri, line, character, "textDocument/typeDefinition", now)
    }

    pub fn request_implementation(
        &mut self,
        uri: &str,
        line: u64,
        character: u64,
        now: Instant,
    ) -> io::Result<Option<u64>> {
        self.request_definition_kind(uri, line, character, "textDocument/implementation", now)
    }

    fn request_definition_kind(
        &mut self,
        uri: &str,
        line: u64,
        character: u64,
        method: &str,
        now: Instant,
    ) -> io::Result<Option<u64>> {
        let keys: Vec<LspKey> = self
            .documents
            .get(uri)
            .map(|(key, _)| key.clone())
            .into_iter()
            .chain(self.companion_keys(uri))
            .collect();
        if keys.is_empty() {
            return Ok(None);
        }
        self.definition_locations.remove(uri);
        // Late answers to an older lookup must not leak into this one.
        self.definition_requests.retain(|_, pending| pending != uri);
        let params = serde_json::json!({
            "textDocument": { "uri": uri }, "position": { "line": line, "character": character }
        });
        let mut first = None;
        let mut last_error = None;
        for key in keys {
            match self.request(
                &key,
                method.into(),
                params.clone(),
                // laravel-lsp builds its project index (by running PHP) on
                // the first lookup, which takes a few seconds.
                Duration::from_secs(5),
                now,
            ) {
                Ok(id) => {
                    self.definition_requests.insert((key, id), uri.to_owned());
                    first.get_or_insert(id);
                }
                Err(error) => last_error = Some(error),
            }
        }
        match (first, last_error) {
            (Some(id), _) => Ok(Some(id)),
            (None, Some(error)) => Err(error),
            (None, None) => Ok(None),
        }
    }

    /// Keys of the companion servers attached to `uri`.
    pub fn companion_keys(&self, uri: &str) -> Vec<LspKey> {
        self.companions
            .get(uri)
            .map(|entries| entries.iter().map(|(key, _)| key.clone()).collect())
            .unwrap_or_default()
    }

    /// Attaches an extra server to an already open document and sends
    /// `didOpen` to it (now, or once it finishes initializing).
    pub fn open_companion(
        &mut self,
        key: LspKey,
        uri: String,
        language_id: String,
        doc: &Document,
    ) -> io::Result<()> {
        let sync = DocumentSync::new(uri.clone(), language_id, doc.rope().clone());
        if self
            .servers
            .get(&key)
            .is_some_and(|server| server.initialized)
        {
            self.send(&key, &sync.did_open())?;
        }
        let entries = self.companions.entry(uri).or_default();
        entries.retain(|(existing, _)| existing != &key);
        entries.push((key, sync));
        Ok(())
    }

    pub fn definition_locations(&self, uri: &str) -> &[DefinitionLocation] {
        self.definition_locations
            .get(uri)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn request_references(
        &mut self,
        uri: &str,
        line: u64,
        character: u64,
        now: Instant,
    ) -> io::Result<Option<u64>> {
        let Some(key) = self.documents.get(uri).map(|(key, _)| key.clone()) else {
            return Ok(None);
        };
        self.references.remove(uri);
        let id = self.request(&key, "textDocument/references".into(), serde_json::json!({
            "textDocument": { "uri": uri }, "position": { "line": line, "character": character }, "context": { "includeDeclaration": true }
        }), Duration::from_millis(1000), now)?;
        self.reference_requests.insert((key, id), uri.to_owned());
        Ok(Some(id))
    }

    pub fn references(&self, uri: &str) -> &[DefinitionLocation] {
        self.references.get(uri).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn request_rename(
        &mut self,
        uri: &str,
        line: u64,
        character: u64,
        new_name: &str,
        now: Instant,
    ) -> io::Result<Option<u64>> {
        let Some(key) = self.documents.get(uri).map(|(key, _)| key.clone()) else {
            return Ok(None);
        };
        self.rename_edits.remove(uri);
        let id = self.request(&key, "textDocument/rename".into(), serde_json::json!({
            "textDocument": { "uri": uri }, "position": { "line": line, "character": character }, "newName": new_name
        }), Duration::from_millis(1500), now)?;
        self.rename_requests.insert((key, id), uri.to_owned());
        Ok(Some(id))
    }

    pub fn rename_edits(&self, uri: &str) -> &[RenameEdit] {
        self.rename_edits.get(uri).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn request_code_actions(
        &mut self,
        uri: &str,
        range: Range,
        diagnostics: &[Diagnostic],
        now: Instant,
    ) -> io::Result<Option<u64>> {
        let Some(key) = self.documents.get(uri).map(|(key, _)| key.clone()) else {
            return Ok(None);
        };
        self.code_actions.remove(uri);
        let diagnostics = diagnostics.iter().map(|item| serde_json::json!({
            "range": {"start":{"line":item.range.start_line,"character":item.range.start_character},"end":{"line":item.range.end_line,"character":item.range.end_character}},
            "severity": match item.severity { DiagnosticSeverity::Error => 1, DiagnosticSeverity::Warning => 2, DiagnosticSeverity::Information => 3, DiagnosticSeverity::Hint => 4 }, "message": item.message
        })).collect::<Vec<_>>();
        let id = self.request(&key, "textDocument/codeAction".into(), serde_json::json!({
            "textDocument": { "uri": uri }, "range": {"start":{"line":range.start_line,"character":range.start_character},"end":{"line":range.end_line,"character":range.end_character}}, "context": { "diagnostics": diagnostics }
        }), Duration::from_millis(1000), now)?;
        self.code_action_requests.insert((key, id), uri.to_owned());
        Ok(Some(id))
    }

    pub fn code_actions(&self, uri: &str) -> &[CodeActionInfo] {
        self.code_actions.get(uri).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn request_formatting(
        &mut self,
        uri: &str,
        tab_size: u64,
        insert_spaces: bool,
        now: Instant,
    ) -> io::Result<Option<u64>> {
        let Some(key) = self.documents.get(uri).map(|(key, _)| key.clone()) else {
            return Ok(None);
        };
        self.formatting_edits.remove(uri);
        let id = self.request(
            &key,
            "textDocument/formatting".into(),
            serde_json::json!({
                "textDocument": { "uri": uri },
                "options": { "tabSize": tab_size, "insertSpaces": insert_spaces }
            }),
            Duration::from_millis(1500),
            now,
        )?;
        self.formatting_requests.insert((key, id), uri.to_owned());
        Ok(Some(id))
    }

    pub fn formatting_edits(&self, uri: &str) -> &[RenameEdit] {
        self.formatting_edits
            .get(uri)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn request_document_symbols(&mut self, uri: &str, now: Instant) -> io::Result<Option<u64>> {
        let Some(key) = self.documents.get(uri).map(|(key, _)| key.clone()) else {
            return Ok(None);
        };
        self.document_symbols.remove(uri);
        let id = self.request(
            &key,
            "textDocument/documentSymbol".into(),
            serde_json::json!({
                "textDocument": { "uri": uri }
            }),
            Duration::from_millis(1000),
            now,
        )?;
        self.document_symbol_requests
            .insert((key, id), uri.to_owned());
        Ok(Some(id))
    }

    pub fn document_symbols(&self, uri: &str) -> &[DocumentSymbolInfo] {
        self.document_symbols
            .get(uri)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn request_workspace_symbols(
        &mut self,
        root: &str,
        query: &str,
        now: Instant,
    ) -> io::Result<Option<u64>> {
        let key = self
            .documents
            .values()
            .find(|(key, _)| key.root.to_string_lossy() == root)
            .map(|(key, _)| key.clone());
        let Some(key) = key else {
            return Ok(None);
        };
        self.workspace_symbols.remove(root);
        let id = self.request(
            &key,
            "workspace/symbol".into(),
            serde_json::json!({ "query": query }),
            Duration::from_millis(1000),
            now,
        )?;
        self.workspace_symbol_requests
            .insert((key, id), root.to_owned());
        Ok(Some(id))
    }

    pub fn workspace_symbols(&self, root: &str) -> &[DocumentSymbolInfo] {
        self.workspace_symbols
            .get(root)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn request_range_formatting(
        &mut self,
        uri: &str,
        range: Range,
        tab_size: u64,
        insert_spaces: bool,
        now: Instant,
    ) -> io::Result<Option<u64>> {
        self.request_formatting_variant(
            uri,
            "textDocument/rangeFormatting",
            serde_json::json!({
                "textDocument": { "uri": uri }, "range": range_value(&range),
                "options": { "tabSize": tab_size, "insertSpaces": insert_spaces }
            }),
            now,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn request_on_type_formatting(
        &mut self,
        uri: &str,
        line: u64,
        character: u64,
        trigger: &str,
        tab_size: u64,
        insert_spaces: bool,
        now: Instant,
    ) -> io::Result<Option<u64>> {
        self.request_formatting_variant(uri, "textDocument/onTypeFormatting", serde_json::json!({
            "textDocument": { "uri": uri }, "position": { "line": line, "character": character },
            "ch": trigger, "options": { "tabSize": tab_size, "insertSpaces": insert_spaces }
        }), now, false)
    }

    pub fn request_semantic_tokens(&mut self, uri: &str, now: Instant) -> io::Result<Option<u64>> {
        let Some(key) = self.documents.get(uri).map(|(key, _)| key.clone()) else {
            return Ok(None);
        };
        let id = self.request(
            &key,
            "textDocument/semanticTokens/full".into(),
            serde_json::json!({
                "textDocument": { "uri": uri }
            }),
            Duration::from_millis(1500),
            now,
        )?;
        self.semantic_token_requests
            .insert((key, id), uri.to_owned());
        Ok(Some(id))
    }

    pub fn request_inlay_hints(
        &mut self,
        uri: &str,
        range: Range,
        now: Instant,
    ) -> io::Result<Option<u64>> {
        let Some(key) = self.documents.get(uri).map(|(key, _)| key.clone()) else {
            return Ok(None);
        };
        let id = self.request(
            &key,
            "textDocument/inlayHint".into(),
            serde_json::json!({
                "textDocument": { "uri": uri }, "range": range_to_value(&range)
            }),
            Duration::from_millis(1500),
            now,
        )?;
        self.inlay_hint_requests.insert((key, id), uri.to_owned());
        Ok(Some(id))
    }

    pub fn semantic_tokens(&self, uri: &str) -> &[SemanticTokenInfo] {
        self.semantic_tokens
            .get(uri)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
    pub fn inlay_hints(&self, uri: &str) -> &[InlayHintInfo] {
        self.inlay_hints.get(uri).map(Vec::as_slice).unwrap_or(&[])
    }
    pub fn progress(&self) -> Vec<ProgressInfo> {
        self.progress.values().cloned().collect()
    }
    pub fn server_count(&self) -> usize {
        self.servers.len()
    }
    pub fn last_server_error(&self) -> Option<String> {
        self.server_errors.last().cloned()
    }
    pub fn server_processes(&mut self) -> Vec<ServerProcessInfo> {
        self.servers
            .iter_mut()
            .map(|(key, server)| ServerProcessInfo {
                language: key.language.clone(),
                pid: server.transport.pid(),
                resident_memory_kib: server.transport.resident_memory_kib(),
            })
            .collect()
    }
    pub fn document_key(&self, uri: &str) -> Option<(LspKey, bool)> {
        self.documents.get(uri).map(|(key, _)| {
            (
                key.clone(),
                self.servers
                    .get(key)
                    .is_some_and(|server| server.initialized),
            )
        })
    }

    fn request_formatting_variant(
        &mut self,
        uri: &str,
        method: &str,
        params: Value,
        now: Instant,
        range: bool,
    ) -> io::Result<Option<u64>> {
        let Some(key) = self.documents.get(uri).map(|(key, _)| key.clone()) else {
            return Ok(None);
        };
        self.formatting_edits.remove(uri);
        let id = self.request(
            &key,
            method.into(),
            params,
            Duration::from_millis(1500),
            now,
        )?;
        if range {
            self.range_formatting_requests
                .insert((key, id), uri.to_owned());
        } else {
            self.on_type_formatting_requests
                .insert((key, id), uri.to_owned());
        }
        Ok(Some(id))
    }

    /// Sends a notification to one initialized or initializing server.
    pub fn send(&self, key: &LspKey, message: &Value) -> io::Result<()> {
        self.servers
            .get(key)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "language server is not running")
            })?
            .transport
            .send(message)
    }

    /// Sends a tracked JSON-RPC request. IDs 0 and 1 are reserved for the
    /// initialize and shutdown lifecycles; normal requests start at 2.
    pub fn request(
        &mut self,
        key: &LspKey,
        method: String,
        params: Value,
        timeout: Duration,
        now: Instant,
    ) -> io::Result<u64> {
        if !self
            .servers
            .get(key)
            .is_some_and(|server| server.initialized)
        {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "language server is not initialized",
            ));
        }
        let id = self.next_request_id.max(2);
        self.next_request_id = id.checked_add(1).unwrap_or(2);
        self.send(
            key,
            &serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
        )?;
        self.requests.insert(
            (key.clone(), id),
            PendingRequest {
                method,
                deadline: now + timeout,
            },
        );
        Ok(id)
    }

    /// Cancels a request that is no longer useful. Late responses are ignored.
    pub fn cancel_request(&mut self, key: &LspKey, id: u64) -> io::Result<bool> {
        if self.requests.remove(&(key.clone(), id)).is_none() {
            return Ok(false);
        }
        self.send(
            key,
            &serde_json::json!({"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":id}}),
        )?;
        Ok(true)
    }

    /// Cancels and reports requests whose deadlines passed. Call from the
    /// app's regular nonblocking LSP poll loop.
    pub fn expire_requests(&mut self, now: Instant) -> Vec<LspEvent> {
        let expired: Vec<_> = self
            .requests
            .iter()
            .filter(|(_, pending)| now >= pending.deadline)
            .map(|((key, id), pending)| (key.clone(), *id, pending.method.clone()))
            .collect();
        expired
            .into_iter()
            .map(|(key, id, method)| {
                let _ = self.cancel_request(&key, id);
                LspEvent::TimedOut { key, id, method }
            })
            .collect()
    }

    /// Registers a document and immediately sends `didOpen` to its shared server.
    pub fn open_document(
        &mut self,
        key: LspKey,
        uri: String,
        language_id: String,
        doc: &Document,
    ) -> io::Result<()> {
        let sync = DocumentSync::new(uri.clone(), language_id, doc.rope().clone());
        if self
            .servers
            .get(&key)
            .is_some_and(|server| server.initialized)
        {
            self.send(&key, &sync.did_open())?;
        }
        self.documents.insert(uri, (key, sync));
        Ok(())
    }

    /// Queues a document change. Use [`Self::flush_changes`] from the app's
    /// event loop; it sends only batches whose debounce deadline has passed.
    pub fn note_document_change(
        &mut self,
        uri: &str,
        before: &Document,
        after: &Document,
        changes: &ChangeSet,
        now: Instant,
    ) {
        if let Some((_, sync)) = self.documents.get_mut(uri) {
            sync.note_change(before, after, changes, now);
        }
        for (_, sync) in self.companions.get_mut(uri).into_iter().flatten() {
            sync.note_change(before, after, changes, now);
        }
    }

    pub fn note_full_text(&mut self, uri: &str, text: String, now: Instant) {
        self.note_text(uri, &Rope::from_str(&text), now);
    }

    /// Queues `doc`'s current text; see [`Self::note_text`].
    pub fn note_document(&mut self, uri: &str, doc: &Document, now: Instant) {
        self.note_text(uri, doc.rope(), now);
    }

    /// Queues the document's current text for the primary server and every
    /// companion. Each gets a rope handle (shared nodes), not a `String`
    /// copy; the flush sends only the span that differs.
    pub fn note_text(&mut self, uri: &str, text: &Rope, now: Instant) {
        for (_, sync) in self.companions.get_mut(uri).into_iter().flatten() {
            sync.note_text(text, now);
        }
        if let Some((_, sync)) = self.documents.get_mut(uri) {
            sync.note_text(text, now);
        }
    }

    pub fn flush_changes(&mut self, now: Instant) -> Vec<io::Error> {
        let servers = &self.servers;
        let ready: Vec<_> = self
            .documents
            .values_mut()
            .chain(self.companions.values_mut().flatten())
            // Until `initialized`, keep changes queued: the replayed didOpen
            // carries them, and `shadow` must match what the server has.
            .filter(|(key, _)| servers.get(key).is_some_and(|server| server.initialized))
            .filter_map(|(key, sync)| sync.flush(now).map(|message| (key.clone(), message)))
            .collect();
        ready
            .into_iter()
            .filter_map(|(key, message)| self.send(&key, &message).err())
            .collect()
    }

    pub fn save_document(&self, uri: &str) -> io::Result<()> {
        let (key, sync) = self
            .documents
            .get(uri)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LSP document is not open"))?;
        for (companion, companion_sync) in self.companions.get(uri).into_iter().flatten() {
            let _ = self.send(companion, &companion_sync.did_save());
        }
        self.send(key, &sync.did_save())
    }

    pub fn close_document(&mut self, uri: &str) -> io::Result<()> {
        for (companion, sync) in self.companions.remove(uri).into_iter().flatten() {
            let _ = self.send(&companion, &sync.did_close());
        }
        let (key, sync) = self
            .documents
            .remove(uri)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LSP document is not open"))?;
        self.send(&key, &sync.did_close())
    }

    /// Drains server output without blocking. An initialize response with ID 0
    /// transitions the server and emits the mandatory `initialized` notification.
    pub fn poll(&mut self) -> Vec<LspEvent> {
        self.restart_due_servers(Instant::now());
        let mut events = Vec::new();
        for (key, server) in &mut self.servers {
            loop {
                match server.transport.try_recv() {
                    Ok(Incoming::Message(message))
                        if !server.initialized
                            && message.get("id") == Some(&Value::from(0))
                            && message.get("method").is_none() =>
                    {
                        match server.transport.send(&serde_json::json!({"jsonrpc": "2.0", "method": "initialized", "params": {}})) {
                            Ok(()) => {
                                server.initialized = true;
                                // didOpen carries the newest text, so it also settles
                                // any change queued while the server was starting.
                                let opens: Vec<_> = self
                                    .documents
                                    .values_mut()
                                    .chain(self.companions.values_mut().flatten())
                                    .filter(|(document_key, _)| document_key == key)
                                    .map(|(_, sync)| {
                                        let open = sync.did_open();
                                        sync.mark_opened();
                                        open
                                    })
                                    .collect();
                                for open in opens {
                                    if let Err(error) = server.transport.send(&open) {
                                        events.push(LspEvent::Error(key.clone(), error.to_string()));
                                    }
                                }
                                events.push(LspEvent::Initialized(key.clone()));
                            }
                            Err(error) => events.push(LspEvent::Error(key.clone(), error.to_string())),
                        }
                    }
                    // Server-to-client requests carry an id too; their ids
                    // can collide with ours, so answer them instead of
                    // treating them as responses.
                    Ok(Incoming::Message(message))
                        if message.get("id").is_some() && message.get("method").is_some() =>
                    {
                        let result = if message["method"] == "workspace/configuration" {
                            let items = message["params"]["items"].as_array().map_or(0, Vec::len);
                            Value::Array(vec![Value::Null; items])
                        } else {
                            Value::Null
                        };
                        let reply = serde_json::json!({"jsonrpc": "2.0", "id": message["id"], "result": result});
                        if let Err(error) = server.transport.send(&reply) {
                            events.push(LspEvent::Error(key.clone(), error.to_string()));
                        }
                    }
                    Ok(Incoming::Message(message))
                        if message.get("id").and_then(Value::as_u64).is_some() =>
                    {
                        let id = message["id"].as_u64().expect("checked above");
                        if let Some(uri) = self.hover_requests.remove(&(key.clone(), id))
                            && let Some(value) = message.get("result").and_then(parse_hover)
                        {
                            self.hover.insert(uri, value);
                        }
                        if let Some(uri) = self.completion_requests.remove(&(key.clone(), id))
                            && let Some(result) = message.get("result").and_then(parse_completion)
                        {
                            self.completion.insert(uri, result);
                        }
                        if let Some(uri) = self.signature_requests.remove(&(key.clone(), id))
                            && let Some(result) = message.get("result").and_then(parse_signature)
                        {
                            self.signatures.insert(uri, result);
                        }
                        // Several servers may answer one lookup; keep every location.
                        if let Some(uri) = self.definition_requests.remove(&(key.clone(), id))
                            && let Some(result) =
                                message.get("result").and_then(parse_definition_locations)
                        {
                            self.definition_locations
                                .entry(uri)
                                .or_default()
                                .extend(result);
                        }
                        if let Some(uri) = self.reference_requests.remove(&(key.clone(), id))
                            && let Some(result) =
                                message.get("result").and_then(parse_definition_locations)
                        {
                            self.references.insert(uri, result);
                        }
                        if let Some(uri) = self.rename_requests.remove(&(key.clone(), id))
                            && let Some(result) =
                                message.get("result").and_then(parse_workspace_edit)
                        {
                            self.rename_edits.insert(uri, result);
                        }
                        if let Some(uri) = self.code_action_requests.remove(&(key.clone(), id))
                            && let Some(result) = message.get("result").and_then(parse_code_actions)
                        {
                            self.code_actions.insert(uri, result);
                        }
                        if let Some(uri) = self.formatting_requests.remove(&(key.clone(), id))
                            && let Some(result) = message
                                .get("result")
                                .and_then(|value| parse_text_edit_array(&uri, value))
                        {
                            self.formatting_edits.insert(uri, result);
                        }
                        if let Some(uri) = self.range_formatting_requests.remove(&(key.clone(), id))
                            && let Some(result) = message
                                .get("result")
                                .and_then(|value| parse_text_edit_array(&uri, value))
                        {
                            self.formatting_edits.insert(uri, result);
                        }
                        if let Some(uri) =
                            self.on_type_formatting_requests.remove(&(key.clone(), id))
                            && let Some(result) = message
                                .get("result")
                                .and_then(|value| parse_text_edit_array(&uri, value))
                        {
                            self.formatting_edits.insert(uri, result);
                        }
                        if let Some(uri) = self.document_symbol_requests.remove(&(key.clone(), id))
                            && let Some(result) =
                                message.get("result").and_then(parse_document_symbols)
                        {
                            self.document_symbols.insert(uri, result);
                        }
                        if let Some(root) =
                            self.workspace_symbol_requests.remove(&(key.clone(), id))
                            && let Some(result) =
                                message.get("result").and_then(parse_workspace_symbols)
                        {
                            self.workspace_symbols.insert(root, result);
                        }
                        if let Some(uri) = self.semantic_token_requests.remove(&(key.clone(), id))
                            && let Some(result) =
                                message.get("result").and_then(parse_semantic_tokens)
                        {
                            self.semantic_tokens.insert(uri, result);
                        }
                        if let Some(uri) = self.inlay_hint_requests.remove(&(key.clone(), id))
                            && let Some(result) = message.get("result").and_then(parse_inlay_hints)
                        {
                            self.inlay_hints.insert(uri, result);
                        }
                        if self.requests.remove(&(key.clone(), id)).is_some() {
                            events.push(LspEvent::Response {
                                key: key.clone(),
                                id,
                                result: message.get("result").cloned(),
                                error: message.get("error").cloned(),
                            });
                        }
                    }
                    Ok(Incoming::Message(message)) => {
                        events.push(LspEvent::Message(key.clone(), message))
                    }
                    Ok(Incoming::Error(error)) => {
                        self.server_errors.push(error.clone());
                        events.push(LspEvent::Error(key.clone(), error));
                    }
                    Ok(Incoming::Closed) => {
                        self.server_errors
                            .push(format!("LSP server closed: {}", key.language));
                        events.push(LspEvent::Closed(key.clone()));
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
                }
            }
        }
        for event in &events {
            // Tagged per server so a companion never clears the primary's list.
            if let LspEvent::Message(key, message) = event
                && let Err(error) = self.diagnostics.publish_from(&key.language, message)
            {
                tracing::warn!(%error, "discarded invalid LSP diagnostics notification");
            }
            if let LspEvent::Message(_, message) = event {
                let _ = self.publish_progress(message);
            }
        }
        let closed: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                LspEvent::Closed(key) => Some(key.clone()),
                _ => None,
            })
            .collect();
        for key in closed {
            self.auto_restart(&key, Instant::now());
        }
        events
    }

    fn publish_progress(&mut self, message: &Value) -> bool {
        if message.get("method").and_then(Value::as_str) != Some("$/progress") {
            return false;
        }
        let Some(params) = message.get("params").and_then(Value::as_object) else {
            return false;
        };
        let Some(token) = params.get("token").map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string())
        }) else {
            return false;
        };
        let Some(value) = params.get("value").and_then(Value::as_object) else {
            return false;
        };
        match value.get("kind").and_then(Value::as_str) {
            Some("begin") => {
                self.progress.insert(
                    token.clone(),
                    ProgressInfo {
                        token,
                        title: value
                            .get("title")
                            .and_then(Value::as_str)
                            .unwrap_or("Language server")
                            .into(),
                        message: value
                            .get("message")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        percentage: value.get("percentage").and_then(Value::as_f64),
                        active: true,
                    },
                );
            }
            Some("report") => {
                if let Some(progress) = self.progress.get_mut(&token) {
                    progress.message = value
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    progress.percentage = value.get("percentage").and_then(Value::as_f64);
                }
            }
            Some("end") => {
                if let Some(progress) = self.progress.get_mut(&token) {
                    progress.message = value
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    progress.active = false;
                }
            }
            _ => return false,
        }
        true
    }

    /// Sends the graceful shutdown sequence to all children. Dropping their
    /// transports then guarantees process cleanup even if a server ignores it.
    pub fn shutdown_all(&mut self) {
        for server in self.servers.values() {
            let _ = server.transport.send(&serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "shutdown", "params": null}));
            let _ = server
                .transport
                .send(&serde_json::json!({"jsonrpc": "2.0", "method": "exit", "params": {}}));
        }
        self.servers.clear();
        self.documents.clear();
        self.companions.clear();
    }
}

fn parse_hover(value: &Value) -> Option<HoverInfo> {
    if value.is_null() {
        return None;
    }
    let contents = value.get("contents")?;
    let text = match contents {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(text) => Some(text.clone()),
                Value::Object(object) => object
                    .get("value")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(object) => object.get("value").and_then(Value::as_str)?.to_owned(),
        _ => return None,
    };
    let range = value.get("range").and_then(|range| {
        let position = |name: &str| {
            let position = range.get(name)?.as_object()?;
            Some((
                position.get("line")?.as_u64()?,
                position.get("character")?.as_u64()?,
            ))
        };
        let (start_line, start_character) = position("start")?;
        let (end_line, end_character) = position("end")?;
        Some(Range {
            start_line,
            start_character,
            end_line,
            end_character,
        })
    });
    Some(HoverInfo {
        contents: text,
        range,
    })
}

fn parse_completion(value: &Value) -> Option<Vec<CompletionItem>> {
    let items = value
        .get("items")
        .or_else(|| value.as_array().map(|_| value))?
        .as_array()?;
    Some(
        items
            .iter()
            .filter_map(|item| {
                let object = item.as_object()?;
                let label = object.get("label")?.as_str()?.to_owned();
                let edit_text = object
                    .get("textEdit")
                    .and_then(|edit| edit.get("newText"))
                    .and_then(Value::as_str);
                let insert_text = edit_text
                    .or_else(|| object.get("insertText").and_then(Value::as_str))
                    .unwrap_or(&label)
                    .to_owned();
                Some(CompletionItem {
                    label,
                    detail: object
                        .get("detail")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    insert_text,
                    is_snippet: object.get("insertTextFormat").and_then(Value::as_u64) == Some(2),
                    sort_text: object
                        .get("sortText")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
            })
            .collect(),
    )
}

fn parse_signature(value: &Value) -> Option<SignatureInfo> {
    let signature = value.get("signatures")?.as_array()?.first()?.as_object()?;
    let label = signature.get("label")?.as_str()?.to_owned();
    let parameters = signature
        .get("parameters")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match item {
                    Value::Object(object) => object
                        .get("label")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    Value::String(text) => Some(text.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let documentation = signature.get("documentation").and_then(|value| {
        value.as_str().map(str::to_owned).or_else(|| {
            value
                .get("value")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
    });
    Some(SignatureInfo {
        label,
        documentation,
        parameters,
        active_parameter: value
            .get("activeParameter")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

fn parse_definition_locations(value: &Value) -> Option<Vec<DefinitionLocation>> {
    let values = match value {
        Value::Array(items) => items,
        Value::Object(object) if object.get("uri").is_some() => std::slice::from_ref(value),
        _ => return None,
    };
    Some(
        values
            .iter()
            .filter_map(|item| {
                let object = item.as_object()?;
                let uri = object
                    .get("uri")
                    .or_else(|| object.get("targetUri"))?
                    .as_str()?
                    .to_owned();
                let range = object
                    .get("range")
                    .or_else(|| object.get("targetSelectionRange"))
                    .and_then(parse_range)?;
                Some(DefinitionLocation { uri, range })
            })
            .collect(),
    )
}

fn parse_workspace_edit(value: &Value) -> Option<Vec<RenameEdit>> {
    let mut edits = Vec::new();
    if let Some(changes) = value.get("changes").and_then(Value::as_object) {
        for (uri, items) in changes {
            parse_text_edits(uri, items, &mut edits);
        }
    }
    if let Some(document_changes) = value.get("documentChanges").and_then(Value::as_array) {
        for change in document_changes {
            if let (Some(uri), Some(items)) = (
                change.pointer("/textDocument/uri").and_then(Value::as_str),
                change.get("edits"),
            ) {
                parse_text_edits(uri, items, &mut edits);
            }
        }
    }
    Some(edits)
}

fn parse_code_actions(value: &Value) -> Option<Vec<CodeActionInfo>> {
    let items = value.as_array()?;
    Some(
        items
            .iter()
            .filter_map(|item| {
                let object = item.as_object()?;
                let title = object.get("title")?.as_str()?.to_owned();
                let edits = object
                    .get("edit")
                    .and_then(parse_workspace_edit)
                    .unwrap_or_default();
                Some(CodeActionInfo {
                    title,
                    kind: object
                        .get("kind")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    edits,
                })
            })
            .filter(|action| !action.edits.is_empty())
            .collect(),
    )
}

fn parse_text_edit_array(uri: &str, value: &Value) -> Option<Vec<RenameEdit>> {
    let edits = value.as_array()?;
    Some(
        edits
            .iter()
            .filter_map(|item| {
                let range = parse_range(item.get("range")?)?;
                Some(RenameEdit {
                    uri: uri.to_owned(),
                    range,
                    new_text: item.get("newText")?.as_str()?.to_owned(),
                })
            })
            .collect(),
    )
}

fn parse_document_symbols(value: &Value) -> Option<Vec<DocumentSymbolInfo>> {
    let mut symbols = Vec::new();
    for value in value.as_array()? {
        symbols.push(parse_document_symbol(value)?);
    }
    Some(symbols)
}

fn parse_document_symbol(value: &Value) -> Option<DocumentSymbolInfo> {
    let object = value.as_object()?;
    let range = object
        .get("range")
        .or_else(|| object.get("selectionRange"))
        .and_then(parse_range)
        .or_else(|| object.get("location")?.get("range").and_then(parse_range))?;
    Some(DocumentSymbolInfo {
        uri: object
            .get("location")
            .and_then(|location| location.get("uri"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        name: object.get("name")?.as_str()?.to_owned(),
        kind: object.get("kind")?.as_u64()?,
        range,
        children: object
            .get("children")
            .and_then(parse_document_symbols)
            .unwrap_or_default(),
    })
}

fn parse_workspace_symbols(value: &Value) -> Option<Vec<DocumentSymbolInfo>> {
    parse_document_symbols(value)
}

fn range_value(range: &Range) -> Value {
    serde_json::json!({
        "start": { "line": range.start_line, "character": range.start_character },
        "end": { "line": range.end_line, "character": range.end_character }
    })
}

fn parse_text_edits(uri: &str, value: &Value, output: &mut Vec<RenameEdit>) {
    let Some(items) = value.as_array() else {
        return;
    };
    output.extend(items.iter().filter_map(|item| {
        let range = parse_range(item.get("range")?)?;
        Some(RenameEdit {
            uri: uri.to_owned(),
            range,
            new_text: item.get("newText")?.as_str()?.to_owned(),
        })
    }));
}

fn parse_range(value: &Value) -> Option<Range> {
    let position = |name: &str| {
        let position = value.get(name)?.as_object()?;
        Some((
            position.get("line")?.as_u64()?,
            position.get("character")?.as_u64()?,
        ))
    };
    let (start_line, start_character) = position("start")?;
    let (end_line, end_character) = position("end")?;
    Some(Range {
        start_line,
        start_character,
        end_line,
        end_character,
    })
}

fn range_to_value(range: &Range) -> Value {
    serde_json::json!({
        "start": { "line": range.start_line, "character": range.start_character },
        "end": { "line": range.end_line, "character": range.end_character }
    })
}

fn parse_semantic_tokens(value: &Value) -> Option<Vec<SemanticTokenInfo>> {
    let data = value.get("data")?.as_array()?;
    let mut line = 0;
    let mut start = 0;
    let mut output = Vec::new();
    for chunk in data.chunks_exact(5) {
        let delta_line = chunk[0].as_u64()?;
        let delta_start = chunk[1].as_u64()?;
        line += delta_line;
        start = if delta_line == 0 {
            start + delta_start
        } else {
            delta_start
        };
        output.push(SemanticTokenInfo {
            line,
            start,
            length: chunk[2].as_u64()?,
            token_type: chunk[3].as_u64()?,
        });
    }
    Some(output)
}

fn parse_inlay_hints(value: &Value) -> Option<Vec<InlayHintInfo>> {
    let items = value.as_array()?;
    Some(
        items
            .iter()
            .filter_map(|item| {
                let position = item.get("position")?;
                let label = match item.get("label")? {
                    Value::String(text) => text.clone(),
                    Value::Array(parts) => parts
                        .iter()
                        .filter_map(|part| part.get("value")?.as_str())
                        .collect::<String>(),
                    _ => return None,
                };
                Some(InlayHintInfo {
                    line: position.get("line")?.as_u64()?,
                    character: position.get("character")?.as_u64()?,
                    label,
                })
            })
            .collect(),
    )
}

impl Drop for LspManager {
    fn drop(&mut self) {
        self.shutdown_all();
    }
}

/// A received JSON-RPC payload. Invalid JSON and broken streams are surfaced
/// as errors rather than silently discarded, so a manager can restart a server.
#[derive(Debug)]
pub enum Incoming {
    Message(Value),
    Error(String),
    Closed,
}

/// Worker threads for the runtime every language server shares. Work per
/// server is one reader task plus pipe writes, so two threads are plenty.
/// Before this, each server built a default multi-thread runtime with one
/// worker per CPU core.
pub const LSP_RUNTIME_THREADS: usize = 2;

/// One small Tokio runtime shared by every language-server transport. It
/// lives for the process; it is never dropped, so dropping a transport from
/// any context is safe.
fn shared_runtime() -> io::Result<&'static Runtime> {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    if let Some(runtime) = RUNTIME.get() {
        return Ok(runtime);
    }
    let runtime = Builder::new_multi_thread()
        .worker_threads(LSP_RUNTIME_THREADS)
        .thread_name("galaxy-lsp")
        .enable_all()
        .build()?;
    // A racing caller may win; the loser's unused runtime is dropped here,
    // outside any async context.
    Ok(RUNTIME.get_or_init(|| runtime))
}

/// Stdio JSON-RPC transport for one language-server process.
pub struct LspTransport {
    runtime: &'static Runtime,
    child: Child,
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    incoming: mpsc::Receiver<Incoming>,
    log_traffic: bool,
    rss_cache: Option<(Instant, Option<u64>)>,
}

impl LspTransport {
    /// Starts `program args...` with piped stdio. `log_traffic` writes JSON
    /// payloads to the tracing debug target `galaxy_lsp_traffic`.
    pub fn spawn(
        program: &str,
        args: &[String],
        log_traffic: bool,
        root: &Path,
    ) -> io::Result<Self> {
        let runtime = shared_runtime()?;
        let mut command = Command::new(program);
        command.args(args).env("PATH", server_path_env(program));
        if root.is_dir() {
            command.current_dir(root);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = {
            let _guard = runtime.enter();
            command.spawn()?
        };
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("language server stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("language server stdout unavailable"))?;
        let (tx, incoming) = mpsc::channel();
        runtime.spawn(read_messages(BufReader::new(stdout), tx, log_traffic));
        Ok(Self {
            runtime,
            child,
            stdin: Arc::new(tokio::sync::Mutex::new(stdin)),
            incoming,
            log_traffic,
            rss_cache: None,
        })
    }

    /// Writes a framed JSON-RPC payload. This is the only synchronous wait:
    /// an OS pipe write, not a request/response round trip.
    pub fn send(&self, message: &Value) -> io::Result<()> {
        let payload = serde_json::to_vec(message).map_err(io::Error::other)?;
        if self.log_traffic {
            tracing::debug!(target: "galaxy_lsp_traffic", direction = "out", message = %message);
        }
        self.runtime.block_on(async {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(&frame(&payload)).await?;
            stdin.flush().await
        })
    }

    /// Returns an already received event without waiting.
    pub fn try_recv(&self) -> Result<Incoming, mpsc::TryRecvError> {
        self.incoming.try_recv()
    }

    /// Test and manager convenience; waits no longer than `timeout`.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Incoming, mpsc::RecvTimeoutError> {
        self.incoming.recv_timeout(timeout)
    }

    pub fn pid(&self) -> u32 {
        self.child.id().unwrap_or_default()
    }

    /// Samples RSS through macOS `ps` at most once per five seconds. This is
    /// diagnostic data, so an unavailable process metric remains optional.
    pub fn resident_memory_kib(&mut self) -> Option<u64> {
        let now = Instant::now();
        if let Some((sampled_at, value)) = self.rss_cache
            && now.duration_since(sampled_at) < Duration::from_secs(5)
        {
            return value;
        }
        let pid = self.pid();
        let value = if pid == 0 {
            None
        } else {
            StdCommand::new("/bin/ps")
                .args(["-o", "rss=", "-p", &pid.to_string()])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .and_then(|output| output.trim().parse().ok())
        };
        self.rss_cache = Some((now, value));
        value
    }
}

impl Drop for LspTransport {
    fn drop(&mut self) {
        let _ = self.runtime.block_on(self.child.kill());
    }
}

fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", payload.len()).into_bytes();
    out.extend_from_slice(payload);
    out
}

async fn read_messages<R: AsyncRead + Unpin>(
    mut reader: BufReader<R>,
    tx: mpsc::Sender<Incoming>,
    log_traffic: bool,
) {
    loop {
        match read_frame(&mut reader).await {
            Ok(Some(payload)) => match serde_json::from_slice::<Value>(&payload) {
                Ok(message) => {
                    if log_traffic {
                        tracing::debug!(target: "galaxy_lsp_traffic", direction = "in", message = %message);
                    }
                    if tx.send(Incoming::Message(message)).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = tx.send(Incoming::Error(format!(
                        "invalid JSON-RPC payload: {error}"
                    )));
                }
            },
            Ok(None) => {
                let _ = tx.send(Incoming::Closed);
                return;
            }
            Err(error) => {
                let _ = tx.send(Incoming::Error(error.to_string()));
                return;
            }
        }
    }
}

async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> io::Result<Option<Vec<u8>>> {
    let mut headers = Vec::new();
    loop {
        let before = headers.len();
        let count = reader.read_until(b'\n', &mut headers).await?;
        if count == 0 {
            return if headers.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated LSP headers",
                ))
            };
        }
        if headers.len() - before > 8 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LSP header line exceeds 8 KiB",
            ));
        }
        if headers.ends_with(b"\r\n\r\n") {
            break;
        }
        if headers.len() > 16 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LSP headers exceed 16 KiB",
            ));
        }
    }
    let header_text = std::str::from_utf8(&headers).map_err(io::Error::other)?;
    let length = header_text
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length:").map(str::trim))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?
        .parse::<usize>()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length"))?;
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload).await?;
    Ok(Some(payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// End-to-end check that incremental `didChange` keeps a real server's
    /// copy identical to ours. Diverged text shows up as wrong symbols or
    /// stray syntax diagnostics. Needs `gopls` on PATH:
    /// `cargo test -p galaxy-core --lib gopls -- --ignored`.
    #[test]
    #[ignore = "requires gopls on PATH"]
    fn gopls_stays_in_sync_through_incremental_edits() {
        let Some(gopls) = find_in_path("gopls") else {
            panic!("gopls not found on PATH");
        };
        let root = std::env::temp_dir().join(format!("galaxy-gopls-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("go.mod"), "module example.com/sync\n\ngo 1.21\n").unwrap();
        let path = root.join("main.go");
        let original = "package main\n\n// 😀 中文 comment\nfunc alpha() int {\n\treturn 1\n}\n\nfunc main() {\n\t_ = alpha()\n}\n";
        fs::write(&path, original).unwrap();
        let uri = format!("file://{}", path.display());

        let key = LspKey {
            language: "go".into(),
            root: root.clone(),
        };
        let config = ServerConfig {
            program: gopls.to_string_lossy().into_owned(),
            args: Vec::new(),
            log_traffic: false,
            initialization_options: Value::Null,
            restart_backoff: [Duration::ZERO; 3],
        };
        let mut manager = LspManager::default();
        manager.ensure_server(key.clone(), &config).unwrap();
        let mut doc = Document::from_text(original);
        manager
            .open_document(key.clone(), uri.clone(), "go".into(), &doc)
            .unwrap();
        // Edit only after `initialized`; otherwise the changes ride along in
        // the replayed didOpen and the incremental path is never exercised.
        let deadline = Instant::now() + Duration::from_secs(30);
        while !manager.poll().contains(&LspEvent::Initialized(key.clone())) {
            assert!(Instant::now() < deadline, "gopls did not initialize");
            thread::sleep(Duration::from_millis(20));
        }
        // Diffing the shadow against `doc` must produce a range change, not
        // full text, for this to test anything.
        {
            let (_, sync) = manager.documents.get(&uri).unwrap();
            assert_eq!(sync.shadow.to_string(), original);
        }

        let wait = |manager: &mut LspManager, what: &str, done: &dyn Fn(&LspManager) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                manager.flush_changes(Instant::now() + DOCUMENT_SYNC_DEBOUNCE);
                manager.poll();
                if done(manager) {
                    return;
                }
                assert!(Instant::now() < deadline, "timed out waiting for {what}");
                thread::sleep(Duration::from_millis(20));
            }
        };
        let edit = |manager: &mut LspManager, doc: &mut Document, find: &str, replace: &str| {
            let text = doc.text();
            let byte = text
                .find(find)
                .unwrap_or_else(|| panic!("{find:?} not in text"));
            let start = text[..byte].chars().count();
            let end = start + find.chars().count();
            let change = ChangeSet::new(
                vec![crate::change::Change::new(start, end, replace)],
                doc.len_chars(),
            );
            let before = doc.clone();
            doc.apply(&change);
            manager.note_document_change(&uri, &before, doc, &change, Instant::now());
        };

        // Several edits inside one debounce window, after a non-ASCII line,
        // collapse into one incremental span. The result references an
        // undefined name, so gopls must report it.
        edit(&mut manager, &mut doc, "return 1", "return missing + 1");
        edit(
            &mut manager,
            &mut doc,
            "func main",
            "func beta() string {\n\treturn \"β\"\n}\n\nfunc main",
        );
        edit(
            &mut manager,
            &mut doc,
            "_ = alpha()",
            "_ = alpha()\n\t_ = beta()",
        );
        wait(&mut manager, "undefined-name diagnostic", &|m| {
            m.diagnostics(&uri)
                .iter()
                .any(|d| d.message.contains("missing"))
        });

        // Fix it with a deletion; a clean file proves the server's text
        // matches ours byte for byte.
        edit(&mut manager, &mut doc, "missing + ", "");
        wait(&mut manager, "diagnostics to clear", &|m| {
            m.diagnostics(&uri).is_empty()
        });

        manager
            .request_document_symbols(&uri, Instant::now())
            .unwrap();
        wait(&mut manager, "document symbols", &|m| {
            !m.document_symbols(&uri).is_empty()
        });
        let mut names: Vec<_> = manager
            .document_symbols(&uri)
            .iter()
            .map(|s| s.name.clone())
            .collect();
        names.sort();
        assert_eq!(names, ["alpha", "beta", "main"]);
        // `beta` starts on the line our text has it on.
        let beta_line = doc
            .text()
            .lines()
            .position(|l| l.starts_with("func beta"))
            .unwrap();
        let beta = manager
            .document_symbols(&uri)
            .iter()
            .find(|s| s.name == "beta")
            .unwrap();
        assert_eq!(beta.range.start_line as usize, beta_line);

        manager.shutdown_all();
        let _ = fs::remove_dir_all(&root);
    }
    use std::process::Command as StdCommand;
    use std::thread;
    use std::time::Instant;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn parses_delta_encoded_semantic_tokens() {
        let value = serde_json::json!({ "data": [1, 2, 3, 4, 0, 0, 5, 2, 1, 0] });
        let tokens = parse_semantic_tokens(&value).unwrap();
        assert_eq!(
            tokens[0],
            SemanticTokenInfo {
                line: 1,
                start: 2,
                length: 3,
                token_type: 4
            }
        );
        assert_eq!(
            tokens[1],
            SemanticTokenInfo {
                line: 1,
                start: 7,
                length: 2,
                token_type: 1
            }
        );
    }

    #[test]
    fn parses_string_and_marked_inlay_labels() {
        let value = serde_json::json!([
            { "position": { "line": 2, "character": 4 }, "label": ": number" },
            { "position": { "line": 3, "character": 1 }, "label": [{ "value": "->" }, { "value": " bool" }] }
        ]);
        assert_eq!(parse_inlay_hints(&value).unwrap()[1].label, "-> bool");
    }

    #[test]
    fn decodes_multiple_content_length_frames() {
        let runtime = Runtime::new().unwrap();
        runtime.block_on(async {
            let (mut writer, reader) = tokio::io::duplex(1024);
            writer
                .write_all(&frame(br#"{"jsonrpc":"2.0","method":"one"}"#))
                .await
                .unwrap();
            writer
                .write_all(&frame(br#"{"jsonrpc":"2.0","method":"two"}"#))
                .await
                .unwrap();
            drop(writer);
            let mut reader = BufReader::new(reader);
            assert_eq!(
                read_frame(&mut reader).await.unwrap().unwrap(),
                br#"{"jsonrpc":"2.0","method":"one"}"#
            );
            assert_eq!(
                read_frame(&mut reader).await.unwrap().unwrap(),
                br#"{"jsonrpc":"2.0","method":"two"}"#
            );
            assert!(read_frame(&mut reader).await.unwrap().is_none());
        });
    }

    #[test]
    fn rejects_missing_content_length() {
        let runtime = Runtime::new().unwrap();
        runtime.block_on(async {
            let (mut writer, reader) = tokio::io::duplex(128);
            writer
                .write_all(b"Content-Type: application/vscode-jsonrpc\r\n\r\n")
                .await
                .unwrap();
            drop(writer);
            let mut reader = BufReader::new(reader);
            assert!(read_frame(&mut reader).await.is_err());
        });
    }

    #[test]
    fn process_transport_sends_and_receives_json_rpc() {
        // `read` consumes the two LSP header lines, then this tiny server
        // replies with one fixed JSON-RPC notification on stdout.
        let args = vec![
            "-c".to_owned(),
            "read _; read _; body='{\"jsonrpc\":\"2.0\",\"method\":\"ready\"}'; printf 'Content-Length: %s\\r\\n\\r\\n%s' \"${#body}\" \"$body\"".to_owned(),
        ];
        let transport = LspTransport::spawn("/bin/sh", &args, false, Path::new("/tmp")).unwrap();
        transport
            .send(&serde_json::json!({"jsonrpc": "2.0", "method": "initialize"}))
            .unwrap();
        match transport.recv_timeout(Duration::from_secs(2)).unwrap() {
            Incoming::Message(message) => assert_eq!(message["method"], "ready"),
            other => panic!("expected JSON-RPC message, got {other:?}"),
        }
    }

    #[test]
    fn manager_shares_a_server_and_completes_initialize_lifecycle() {
        let args = vec![
            "-c".to_owned(),
            "read _; read _; body='{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{\"capabilities\":{}}}'; printf 'Content-Length: %s\\r\\n\\r\\n%s' \"${#body}\" \"$body\"; sleep 1".to_owned(),
        ];
        let key = LspKey {
            language: "rust".into(),
            root: PathBuf::from("/workspace"),
        };
        let config = ServerConfig {
            program: "/bin/sh".into(),
            args,
            log_traffic: false,
            initialization_options: Value::Null,
            restart_backoff: [
                Duration::ZERO,
                Duration::from_secs(1),
                Duration::from_secs(2),
            ],
        };
        let mut manager = LspManager::default();
        assert!(manager.ensure_server(key.clone(), &config).unwrap());
        assert!(!manager.ensure_server(key.clone(), &config).unwrap());
        assert_eq!(manager.len(), 1);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if manager.poll().contains(&LspEvent::Initialized(key.clone())) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "initialize response was not observed"
            );
            thread::sleep(Duration::from_millis(1));
        }
        manager.shutdown_all();
        assert_eq!(manager.len(), 0);
    }

    #[test]
    fn stop_server_prevents_restarting_until_explicit_start() {
        let args = vec!["-c".to_owned(), "sleep 5".to_owned()];
        let key = LspKey {
            language: "rust".into(),
            root: PathBuf::from("/workspace"),
        };
        let config = ServerConfig {
            program: "/bin/sh".into(),
            args,
            log_traffic: false,
            initialization_options: Value::Null,
            restart_backoff: [
                Duration::ZERO,
                Duration::from_secs(1),
                Duration::from_secs(2),
            ],
        };
        let mut manager = LspManager::default();
        assert!(manager.ensure_server(key.clone(), &config).unwrap());
        let processes = manager.server_processes();
        assert_eq!(processes.len(), 1);
        assert_eq!(processes[0].language, "rust");
        assert!(processes[0].pid > 0);
        assert!(manager.stop_server(&key));
        assert_eq!(manager.len(), 0);
        assert!(manager.restart_server(&key).unwrap());
        assert_eq!(manager.len(), 1);
        manager.shutdown_all();
    }

    #[test]
    fn tracks_lsp_progress_begin_report_and_end() {
        let mut manager = LspManager::default();
        let begin = serde_json::json!({
            "jsonrpc": "2.0", "method": "$/progress",
            "params": { "token": "index", "value": { "kind": "begin", "title": "Indexing", "percentage": 10 } }
        });
        assert!(manager.publish_progress(&begin));
        assert_eq!(manager.progress()[0].title, "Indexing");
        assert!(manager.progress()[0].active);

        let report = serde_json::json!({
            "jsonrpc": "2.0", "method": "$/progress",
            "params": { "token": "index", "value": { "kind": "report", "message": "src", "percentage": 50 } }
        });
        assert!(manager.publish_progress(&report));
        assert_eq!(manager.progress()[0].percentage, Some(50.0));

        let end = serde_json::json!({
            "jsonrpc": "2.0", "method": "$/progress",
            "params": { "token": "index", "value": { "kind": "end", "message": "Done" } }
        });
        assert!(manager.publish_progress(&end));
        assert!(!manager.progress()[0].active);
    }

    #[test]
    fn settings_parse_override_and_preserve_initialization_options() {
        let settings = LspSettings::from_json(&serde_json::json!({
            "servers": { "Rust": {
                "command": "/bin/sh", "args": ["-c", "server"], "logTraffic": true,
                "initializationOptions": { "checkOnSave": true },
                "restartBackoffMs": [25, 50, 100]
            }}
        }))
        .unwrap();
        let config = settings.resolve("rust").unwrap();
        assert_eq!(config.program, "/bin/sh");
        assert_eq!(config.args, ["-c", "server"]);
        assert!(config.log_traffic);
        assert_eq!(config.initialization_options["checkOnSave"], true);
        assert_eq!(
            config.restart_backoff,
            [
                Duration::from_millis(25),
                Duration::from_millis(50),
                Duration::from_millis(100)
            ]
        );
        assert!(
            LspSettings::from_json(&serde_json::json!({
                "servers": { "Rust": { "command": "/bin/sh", "restartBackoffMs": [1, 2] } }
            }))
            .is_err()
        );
    }

    fn fake_find(available: &'static [&'static str]) -> impl Fn(&str) -> Option<PathBuf> {
        move |program| {
            available
                .contains(&program)
                .then(|| PathBuf::from(format!("/fake/bin/{program}")))
        }
    }

    #[test]
    fn server_path_env_includes_program_dir_and_fallbacks() {
        let path = server_path_env("/custom/tools/bin/laravel-lsp");
        let dirs: Vec<PathBuf> = env::split_paths(&path).collect();
        assert_eq!(dirs[0], PathBuf::from("/custom/tools/bin"));
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(dirs.contains(&PathBuf::from("/usr/bin")));
        let unique: HashSet<_> = dirs[1..].iter().collect();
        assert_eq!(unique.len(), dirs.len() - 1);
    }

    #[test]
    fn script_server_runs_with_minimal_parent_path() {
        // `#!/usr/bin/env` interpreters must resolve through the child PATH.
        let dir = tempfile::tempdir().unwrap();
        let interpreter = dir.path().join("fake-interp");
        std::fs::write(&interpreter, "#!/bin/sh\nexec /bin/cat\n").unwrap();
        let server = dir.path().join("fake-server");
        std::fs::write(&server, "#!/usr/bin/env fake-interp\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        for file in [&interpreter, &server] {
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let transport =
            LspTransport::spawn(server.to_str().unwrap(), &[], false, dir.path()).unwrap();
        let ping = serde_json::json!({"jsonrpc": "2.0", "method": "ping"});
        transport.send(&ping).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(Incoming::Message(message)) = transport.try_recv() {
                assert_eq!(message["method"], "ping");
                break;
            }
            assert!(Instant::now() < deadline, "script server did not echo");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn php_prefers_laravel_lsp_only_in_laravel_projects() {
        let laravel = tempfile::tempdir().unwrap();
        std::fs::write(laravel.path().join("artisan"), "<?php").unwrap();
        let plain = tempfile::tempdir().unwrap();
        let all = fake_find(&["laravel-lsp", "intelephense", "phpactor"]);

        // Laravel: a general PHP server is primary, laravel-lsp the companion.
        let primary = default_server("PHP", Some(laravel.path()), &all).unwrap();
        assert_eq!(primary.program, "/fake/bin/intelephense");
        let companion =
            companion_server_with("Blade", Some(laravel.path()), &primary, &all).unwrap();
        assert_eq!(companion.program, "/fake/bin/laravel-lsp");

        // Plain PHP: no laravel-lsp at all.
        let config = default_server("PHP", Some(plain.path()), &all).unwrap();
        assert_eq!(config.program, "/fake/bin/intelephense");
        assert_eq!(config.args, ["--stdio"]);
        assert!(companion_server_with("PHP", Some(plain.path()), &config, &all).is_none());
        assert!(default_server("PHP", Some(plain.path()), &fake_find(&["laravel-lsp"])).is_none());
    }

    #[test]
    fn laravel_lsp_gets_the_local_php_binary() {
        let laravel = tempfile::tempdir().unwrap();
        std::fs::write(laravel.path().join("artisan"), "<?php").unwrap();
        let all = fake_find(&["intelephense", "laravel-lsp", "php"]);
        let primary = default_server("php", Some(laravel.path()), &all).unwrap();
        let companion = companion_server_with("php", Some(laravel.path()), &primary, &all).unwrap();
        assert_eq!(
            companion.initialization_options["phpCommand"],
            serde_json::json!(["/fake/bin/php"])
        );
        // Without a local php, leave laravel-lsp's own detection alone.
        let no_php = fake_find(&["intelephense", "laravel-lsp"]);
        let companion =
            companion_server_with("php", Some(laravel.path()), &primary, &no_php).unwrap();
        assert!(companion.initialization_options.is_null());
    }

    #[test]
    fn laravel_lsp_alone_is_primary_without_a_duplicate_companion() {
        let laravel = tempfile::tempdir().unwrap();
        std::fs::write(laravel.path().join("artisan"), "<?php").unwrap();
        let only = fake_find(&["laravel-lsp"]);
        let primary = default_server("php", Some(laravel.path()), &only).unwrap();
        assert_eq!(primary.program, "/fake/bin/laravel-lsp");
        assert!(companion_server_with("php", Some(laravel.path()), &primary, &only).is_none());
        assert!(companion_server_with("rust", Some(laravel.path()), &primary, &only).is_none());
    }

    #[test]
    fn diagnostics_from_two_servers_are_kept_side_by_side() {
        let mut store = DiagnosticStore::default();
        let publish = |message: &str| {
            serde_json::json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{
                "uri":"file:///a.php","diagnostics":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"severity":1,"message":message}]
            }})
        };
        store
            .publish_from("php", &publish("from intelephense"))
            .unwrap();
        store
            .publish_from("php-laravel", &publish("from laravel"))
            .unwrap();
        assert_eq!(store.for_uri("file:///a.php").len(), 2);
        let cleared = serde_json::json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///a.php","diagnostics":[]}});
        store.publish_from("php-laravel", &cleared).unwrap();
        let left = store.for_uri("file:///a.php");
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].message, "from intelephense");
    }

    #[test]
    fn php_falls_back_to_intelephense_then_phpactor() {
        let laravel = tempfile::tempdir().unwrap();
        std::fs::write(laravel.path().join("artisan"), "<?php").unwrap();
        let config =
            default_server("php", Some(laravel.path()), &fake_find(&["intelephense"])).unwrap();
        assert_eq!(config.program, "/fake/bin/intelephense");
        let config = default_server("php", None, &fake_find(&["phpactor"])).unwrap();
        assert_eq!(config.program, "/fake/bin/phpactor");
        assert_eq!(config.args, ["language-server"]);
        assert!(default_server("php", None, &fake_find(&[])).is_none());
    }

    #[test]
    fn php_override_covers_blade_and_exact_entry_wins() {
        let settings = LspSettings::from_json(&serde_json::json!({
            "servers": {
                "php": { "command": "/bin/php-server" },
                "go": { "command": "/bin/go-server" }
            }
        }))
        .unwrap();
        assert_eq!(settings.resolve("PHP").unwrap().program, "/bin/php-server");
        assert_eq!(
            settings.resolve("Blade").unwrap().program,
            "/bin/php-server"
        );
        let settings = LspSettings::from_json(&serde_json::json!({
            "servers": {
                "php": { "command": "/bin/php-server" },
                "blade": { "command": "/bin/blade-server" }
            }
        }))
        .unwrap();
        assert_eq!(
            settings.resolve("blade").unwrap().program,
            "/bin/blade-server"
        );
    }

    #[test]
    fn settings_load_merges_global_and_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.json");
        std::fs::write(
            &global,
            r#"{ "lsp": { "servers": {
                "php": { "command": "/global/php" },
                "go": { "command": "/global/go" }
            } } }"#,
        )
        .unwrap();
        let root = dir.path().join("ws");
        std::fs::create_dir_all(root.join(".galaxy")).unwrap();
        std::fs::write(
            root.join(".galaxy/settings.json"),
            r#"{ "editor": { "tabSize": 2 },
                "lsp": { "servers": { "php": {
                    "command": "/ws/php",
                    "initializationOptions": { "storagePath": "/tmp/x" }
                } } } }"#,
        )
        .unwrap();
        let (settings, errors) = LspSettings::load(Some(&global), Some(&root));
        assert!(errors.is_empty(), "{errors:?}");
        let php = settings.resolve("php").unwrap();
        assert_eq!(php.program, "/ws/php");
        assert_eq!(php.initialization_options["storagePath"], "/tmp/x");
        assert_eq!(settings.resolve("go").unwrap().program, "/global/go");

        let missing = dir.path().join("missing.json");
        let (settings, errors) = LspSettings::load(Some(&missing), None);
        assert!(errors.is_empty());
        assert!(settings.servers.is_empty());

        std::fs::write(&global, "{ not json").unwrap();
        let (_, errors) = LspSettings::load(Some(&global), None);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn language_ids_follow_lsp_spec() {
        assert_eq!(lsp_language_id("PHP", "/app/Models/User.php"), "php");
        assert_eq!(
            lsp_language_id("PHP", "/resources/views/welcome.blade.php"),
            "blade"
        );
        assert_eq!(lsp_language_id("Rust", "/src/main.rs"), "rust");
        assert_eq!(lsp_language_id("TSX", "/a.tsx"), "typescriptreact");
        assert_eq!(
            lsp_language_id("TypeScript React", "/a.tsx"),
            "typescriptreact"
        );
        assert_eq!(lsp_language_id("JavaScript", "/a.js"), "javascript");
        assert_eq!(lsp_server_language("Blade"), "php");
        assert_eq!(lsp_server_language("PHP"), "php");
        assert_eq!(lsp_server_language("TSX"), "typescript");
    }

    #[test]
    fn display_language_names_resolve_to_typescript_server_kind() {
        for language in [
            "TypeScript",
            "TypeScript React",
            "JavaScript",
            "JavaScript React",
        ] {
            assert_eq!(
                default_server_kind(language),
                Some("typescript"),
                "{language}"
            );
        }
        assert_eq!(default_server_kind("TypeScript Server"), None);
    }

    #[test]
    fn real_gopls_completes_initialize_when_available() {
        let Some(program) = find_in_path("gopls") else {
            return;
        };
        let workspace = tempfile::tempdir().unwrap();
        let key = LspKey {
            language: "go".into(),
            root: workspace.path().to_path_buf(),
        };
        let config = ServerConfig {
            program: program.to_string_lossy().into_owned(),
            args: Vec::new(),
            log_traffic: false,
            initialization_options: Value::Null,
            restart_backoff: [
                Duration::ZERO,
                Duration::from_secs(1),
                Duration::from_secs(2),
            ],
        };
        let mut manager = LspManager::default();
        manager.ensure_server(key.clone(), &config).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if manager.poll().contains(&LspEvent::Initialized(key.clone())) {
                break;
            }
            assert!(Instant::now() < deadline, "gopls did not initialize");
            thread::sleep(Duration::from_millis(10));
        }
        manager.shutdown_all();
    }

    #[test]
    fn real_rust_analyzer_completes_when_available() {
        let Some(program) = find_in_path("rust-analyzer") else {
            return;
        };
        if !StdCommand::new(&program)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join("Cargo.toml"),
            "[package]\nname = \"galaxy_lsp_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(workspace.path().join("src")).unwrap();
        let source = "fn main() {\n    let value = String::new();\n    value.\n}\n";
        let source_path = workspace.path().join("src/main.rs");
        std::fs::write(&source_path, source).unwrap();
        let key = LspKey {
            language: "rust".into(),
            root: workspace.path().to_path_buf(),
        };
        let config = ServerConfig {
            program: program.to_string_lossy().into_owned(),
            args: Vec::new(),
            log_traffic: false,
            initialization_options: Value::Null,
            restart_backoff: [
                Duration::ZERO,
                Duration::from_secs(1),
                Duration::from_secs(2),
            ],
        };
        let mut manager = LspManager::default();
        manager.ensure_server(key.clone(), &config).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !manager.poll().contains(&LspEvent::Initialized(key.clone())) {
            assert!(
                Instant::now() < deadline,
                "rust-analyzer did not initialize"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let uri = format!("file://{}", source_path.to_string_lossy());
        let document = Document::from_text(source);
        manager
            .open_document(key.clone(), uri.clone(), "rust".into(), &document)
            .unwrap();
        manager
            .request_completion(&uri, 2, 10, Instant::now())
            .unwrap();
        while manager.completions(&uri).is_empty() {
            assert!(
                Instant::now() < deadline,
                "rust-analyzer returned no completions"
            );
            manager.poll();
            thread::sleep(Duration::from_millis(10));
        }
        manager.shutdown_all();
    }

    #[test]
    fn rust_analyzer_smoke_test_on_ripgrep_checkout_when_configured() {
        let Ok(root) = std::env::var("GALAXY_RIPGREP_PATH") else {
            return;
        };
        let root = PathBuf::from(root);
        let source_path = root.join("crates/core/main.rs");
        if !root.join("Cargo.toml").is_file() || !source_path.is_file() {
            return;
        }
        let Some(program) = find_in_path("rust-analyzer") else {
            return;
        };
        if !StdCommand::new(&program)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            return;
        }
        let source = std::fs::read_to_string(&source_path).unwrap();
        let key = LspKey {
            language: "rust".into(),
            root,
        };
        let config = ServerConfig {
            program: program.to_string_lossy().into_owned(),
            args: Vec::new(),
            log_traffic: false,
            initialization_options: Value::Null,
            restart_backoff: [
                Duration::ZERO,
                Duration::from_secs(1),
                Duration::from_secs(2),
            ],
        };
        let mut manager = LspManager::default();
        manager.ensure_server(key.clone(), &config).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !manager.poll().contains(&LspEvent::Initialized(key.clone())) {
            assert!(
                Instant::now() < deadline,
                "rust-analyzer did not initialize ripgrep"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let uri = format!("file://{}", source_path.to_string_lossy());
        let document = Document::from_text(&source);
        manager
            .open_document(key, uri, "rust".into(), &document)
            .unwrap();
        manager.shutdown_all();
    }

    /// Opt-in: `GALAXY_LARAVEL_PATH=/path/to/laravel-app` with intelephense
    /// and laravel-lsp installed. Checks class definitions (intelephense)
    /// and config strings (laravel-lsp) through one manager.
    #[test]
    fn laravel_definitions_merge_both_servers_when_configured() {
        let Ok(root) = std::env::var("GALAXY_LARAVEL_PATH") else {
            return;
        };
        let root = PathBuf::from(root);
        let source_path = root.join("bootstrap/app.php");
        let (Some(primary), true) = (
            default_server("php", Some(&root), &find_in_path),
            source_path.is_file(),
        ) else {
            return;
        };
        let Some(companion) = companion_server("php", Some(&root), &primary) else {
            return;
        };
        let source = std::fs::read_to_string(&source_path).unwrap();
        let key = LspKey {
            language: "php".into(),
            root: root.clone(),
        };
        let companion_key = LspKey {
            language: companion_language("php", "laravel"),
            root,
        };
        let mut manager = LspManager::default();
        manager.ensure_server(key.clone(), &primary).unwrap();
        manager
            .ensure_server(companion_key.clone(), &companion)
            .unwrap();
        let uri = format!("file://{}", source_path.to_string_lossy());
        let document = Document::from_text(&source);
        manager
            .open_companion(companion_key.clone(), uri.clone(), "php".into(), &document)
            .unwrap();
        manager
            .open_document(key.clone(), uri.clone(), "php".into(), &document)
            .unwrap();
        let mut ready = HashSet::new();
        let deadline = Instant::now() + Duration::from_secs(30);
        while ready.len() < 2 {
            for event in manager.poll() {
                if let LspEvent::Initialized(key) = event {
                    ready.insert(key);
                }
            }
            assert!(Instant::now() < deadline, "PHP servers did not initialize");
            thread::sleep(Duration::from_millis(20));
        }
        // Indexing takes a while on a real project; retry until it resolves.
        let (line, column) = source
            .lines()
            .enumerate()
            .find_map(|(i, text)| text.find("Application::").map(|c| (i, c + 1)))
            .expect("bootstrap/app.php mentions Application::");
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            manager
                .request_definition(&uri, line as u64, column as u64, Instant::now())
                .unwrap();
            let asked = Instant::now();
            while Instant::now() < asked + Duration::from_secs(2)
                && manager.definition_locations(&uri).is_empty()
            {
                manager.poll();
                thread::sleep(Duration::from_millis(20));
            }
            if manager
                .definition_locations(&uri)
                .iter()
                .any(|location| location.uri.ends_with("Foundation/Application.php"))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no class definition for Application"
            );
        }
        manager.shutdown_all();
    }

    /// Opt-in: `config('services.sipay.username')` in possipay must resolve
    /// through the laravel-lsp companion to `config/services.php`.
    #[test]
    fn laravel_config_string_definition_when_configured() {
        let Ok(root) = std::env::var("GALAXY_LARAVEL_PATH") else {
            return;
        };
        let root = PathBuf::from(root);
        let source_path = root.join("app/Libraries/SiPay.php");
        let (Some(primary), true) = (
            default_server("php", Some(&root), &find_in_path),
            source_path.is_file(),
        ) else {
            return;
        };
        let Some(companion) = companion_server("php", Some(&root), &primary) else {
            return;
        };
        let source = std::fs::read_to_string(&source_path).unwrap();
        let key = LspKey {
            language: "php".into(),
            root: root.clone(),
        };
        let companion_key = LspKey {
            language: companion_language("php", "laravel"),
            root,
        };
        let mut manager = LspManager::default();
        manager.ensure_server(key.clone(), &primary).unwrap();
        manager
            .ensure_server(companion_key.clone(), &companion)
            .unwrap();
        let uri = format!("file://{}", source_path.to_string_lossy());
        let document = Document::from_text(&source);
        manager
            .open_companion(companion_key, uri.clone(), "php".into(), &document)
            .unwrap();
        manager
            .open_document(key, uri.clone(), "php".into(), &document)
            .unwrap();
        let mut ready = 0;
        let deadline = Instant::now() + Duration::from_secs(30);
        while ready < 2 {
            ready += manager
                .poll()
                .iter()
                .filter(|event| matches!(event, LspEvent::Initialized(_)))
                .count();
            assert!(Instant::now() < deadline, "PHP servers did not initialize");
            thread::sleep(Duration::from_millis(20));
        }
        let (line, column) = source
            .lines()
            .enumerate()
            .find_map(|(i, text)| text.find("services.sipay.username").map(|c| (i, c + 3)))
            .expect("SiPay.php uses services.sipay.username");
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut log = Vec::new();
        loop {
            let asked = Instant::now();
            manager
                .request_definition(&uri, line as u64, column as u64, asked)
                .unwrap();
            while Instant::now() < asked + Duration::from_secs(5)
                && manager.definition_locations(&uri).is_empty()
            {
                for event in manager.poll() {
                    if let LspEvent::Response {
                        key, id, result, ..
                    } = event
                    {
                        log.push(format!(
                            "{}ms {} id={id} {}",
                            asked.elapsed().as_millis(),
                            key.language,
                            result
                                .map(|value| value.to_string())
                                .unwrap_or_default()
                                .chars()
                                .take(200)
                                .collect::<String>()
                        ));
                    }
                }
                for event in manager.expire_requests(Instant::now()) {
                    if let LspEvent::TimedOut { key, method, .. } = event {
                        log.push(format!(
                            "{}ms TIMEOUT {} {method}",
                            asked.elapsed().as_millis(),
                            key.language
                        ));
                    }
                }
                thread::sleep(Duration::from_millis(20));
            }
            if manager
                .definition_locations(&uri)
                .iter()
                .any(|location| location.uri.ends_with("config/services.php"))
            {
                eprintln!(
                    "CONFIG ok after {}ms: {log:#?}",
                    asked.elapsed().as_millis()
                );
                break;
            }
            assert!(Instant::now() < deadline, "no config definition: {log:#?}");
        }
        manager.shutdown_all();
    }

    /// Opt-in like the definition test: hover on a class name in a real
    /// Laravel project must produce hover contents.
    #[test]
    fn laravel_hover_returns_contents_when_configured() {
        let Ok(root) = std::env::var("GALAXY_LARAVEL_PATH") else {
            return;
        };
        let root = PathBuf::from(root);
        let source_path = root.join("bootstrap/providers.php");
        let (Some(primary), true) = (
            default_server("php", Some(&root), &find_in_path),
            source_path.is_file(),
        ) else {
            return;
        };
        let source = std::fs::read_to_string(&source_path).unwrap();
        let key = LspKey {
            language: "php".into(),
            root,
        };
        let mut manager = LspManager::default();
        manager.ensure_server(key.clone(), &primary).unwrap();
        let uri = format!("file://{}", source_path.to_string_lossy());
        manager
            .open_document(
                key.clone(),
                uri.clone(),
                "php".into(),
                &Document::from_text(&source),
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !manager.poll().contains(&LspEvent::Initialized(key.clone())) {
            assert!(Instant::now() < deadline, "intelephense did not initialize");
            thread::sleep(Duration::from_millis(20));
        }
        let (line, column) = source
            .lines()
            .enumerate()
            .find_map(|(i, text)| text.find("AppServiceProvider").map(|c| (i, c + 3)))
            .expect("providers.php mentions AppServiceProvider");
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut raw = Vec::new();
        loop {
            manager
                .request_hover(&uri, line as u64, column as u64, Instant::now())
                .unwrap();
            let asked = Instant::now();
            while Instant::now() < asked + Duration::from_secs(3) && manager.hover(&uri).is_none() {
                for event in manager.poll() {
                    if let LspEvent::Message(_, message) = event
                        && message.get("id").is_some()
                    {
                        raw.push(message.to_string().chars().take(300).collect::<String>());
                    }
                }
                thread::sleep(Duration::from_millis(20));
            }
            if manager.hover(&uri).is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no hover; responses seen: {raw:#?}"
            );
        }
        manager.shutdown_all();
    }

    #[test]
    fn finds_executable_in_an_explicit_path() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("test-lsp");
        fs::write(&executable, "#!/bin/sh\n").unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();
        assert_eq!(
            find_in_path(&executable.to_string_lossy()),
            Some(executable)
        );
    }

    #[test]
    fn document_sync_uses_utf16_ranges_and_debounces_bursts() {
        let before = Document::from_text("a😀中b");
        let mut after = before.clone();
        let change = ChangeSet::new(
            vec![crate::change::Change::new(1, 3, "X")],
            before.len_chars(),
        );
        after.apply(&change);
        let now = Instant::now();
        let mut sync = DocumentSync::new(
            "file:///test.rs".into(),
            "rust".into(),
            before.rope().clone(),
        );
        assert_eq!(sync.did_open()["params"]["textDocument"]["text"], "a😀中b");
        sync.note_change(&before, &after, &change, now);
        assert!(sync.flush(now).is_none());
        let message = sync.flush(now + DOCUMENT_SYNC_DEBOUNCE).unwrap();
        let content = &message["params"]["contentChanges"][0];
        assert_eq!(content["range"]["start"]["character"], 1);
        assert_eq!(content["range"]["end"]["character"], 4);
        assert_eq!(content["rangeLength"], 3);
        assert_eq!(content["text"], "X");

        let next = ChangeSet::new(
            vec![crate::change::Change::insert(1, "!")],
            after.len_chars(),
        );
        let mut newest = after.clone();
        newest.apply(&next);
        sync.note_change(&after, &newest, &next, now + DOCUMENT_SYNC_DEBOUNCE);
        sync.note_change(
            &newest,
            &newest,
            &ChangeSet::new(
                vec![crate::change::Change::insert(0, "?")],
                newest.len_chars(),
            ),
            now + DOCUMENT_SYNC_DEBOUNCE + Duration::from_millis(1),
        );
        // A burst stays incremental: one span diffed against the server's text.
        let burst = sync.flush(now + DOCUMENT_SYNC_DEBOUNCE * 3).unwrap();
        let content = &burst["params"]["contentChanges"][0];
        assert_eq!(content["range"]["start"]["character"], 1);
        assert_eq!(content["range"]["end"]["character"], 1);
        assert_eq!(content["rangeLength"], 0);
        assert_eq!(content["text"], "!");
        assert_eq!(burst["params"]["textDocument"]["version"], 3);
        assert_eq!(sync.did_save()["method"], "textDocument/didSave");
        assert_eq!(sync.did_close()["method"], "textDocument/didClose");
    }

    /// Applies an LSP incremental change to `old` the way a server would.
    fn apply_incremental(old: &str, change: &IncrementalChange) -> String {
        let utf16: Vec<u16> = old.encode_utf16().collect();
        let line_offset = |line: u64| -> usize {
            let mut seen = 0;
            let mut offset = 0;
            for (i, unit) in utf16.iter().enumerate() {
                if seen == line {
                    break;
                }
                if *unit == u16::from(b'\n') {
                    seen += 1;
                }
                offset = i + 1;
            }
            if line == 0 { 0 } else { offset }
        };
        let start = line_offset(change.range.start_line) + change.range.start_character as usize;
        let end = line_offset(change.range.end_line) + change.range.end_character as usize;
        assert_eq!((end - start) as u64, change.range_length);
        let mut out = String::from_utf16(&utf16[..start]).unwrap();
        out.push_str(&change.text);
        out.push_str(&String::from_utf16(&utf16[end..]).unwrap());
        out
    }

    #[test]
    fn incremental_change_round_trips_edits() {
        let cases = [
            ("", "abc"),
            ("abc", ""),
            ("abc", "abc"),
            ("hello world", "hello brave world"),
            ("aaaa", "aaaaa"),
            ("line1\nline2\nline3\n", "line1\nLINE2\nline3\n"),
            ("a😀b", "a😁b"),
            ("中文\n😀x", "中文\n😀yx"),
            ("first\nsecond", "first\nsecond\nthird"),
            ("x\ny\nz", "z"),
        ];
        for (old, new) in cases {
            let change = incremental_change(&Rope::from_str(old), &Rope::from_str(new));
            match change {
                None => assert_eq!(old, new),
                Some(change) => {
                    assert_eq!(apply_incremental(old, &change), new, "{old:?} -> {new:?}")
                }
            }
        }
    }

    #[test]
    fn incremental_change_spans_large_ropes_without_full_text() {
        let base = "fn value() -> u32 { 42 }\n".repeat(20_000);
        let old = Rope::from_str(&base);
        let mut new = old.clone();
        let at = new.len_chars() / 2;
        new.insert(at, "// edited\n");
        let change = incremental_change(&old, &new).unwrap();
        assert_eq!(change.text, "// edited\n");
        assert_eq!(change.range_length, 0);
        assert_eq!(apply_incremental(&base, &change), new.to_string());
    }

    #[test]
    fn changes_wait_for_initialized_and_did_open_settles_them() {
        let doc = Document::from_text("abc");
        let mut sync = DocumentSync::new("file:///t.rs".into(), "rust".into(), doc.rope().clone());
        let mut after = doc.clone();
        after.apply(&ChangeSet::new(
            vec![crate::change::Change::insert(3, "d")],
            3,
        ));
        let now = Instant::now();
        sync.note_document(&after, now);
        // Replay at initialize sends the newest text and clears the queue.
        assert_eq!(sync.did_open()["params"]["textDocument"]["text"], "abcd");
        sync.mark_opened();
        assert!(sync.flush(now + DOCUMENT_SYNC_DEBOUNCE).is_none());
    }

    #[test]
    fn shared_runtime_is_reused() {
        let a = shared_runtime().unwrap() as *const Runtime;
        let b = shared_runtime().unwrap() as *const Runtime;
        assert_eq!(a, b);
    }

    #[test]
    fn manager_sends_document_lifecycle_notifications() {
        let args = vec![
            "-c".to_owned(),
            "read _; read _; body='{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{}}'; printf 'Content-Length: %s\\r\\n\\r\\n%s' \"${#body}\" \"$body\"; sleep 1".to_owned(),
        ];
        let key = LspKey {
            language: "rust".into(),
            root: PathBuf::from("/workspace"),
        };
        let config = ServerConfig {
            program: "/bin/sh".into(),
            args,
            log_traffic: false,
            initialization_options: Value::Null,
            restart_backoff: [
                Duration::ZERO,
                Duration::from_secs(1),
                Duration::from_secs(2),
            ],
        };
        let mut manager = LspManager::default();
        manager.ensure_server(key.clone(), &config).unwrap();
        let doc = Document::from_text("a😀");
        manager
            .open_document(key, "file:///doc.rs".into(), "rust".into(), &doc)
            .unwrap();
        let change = ChangeSet::new(
            vec![crate::change::Change::insert(doc.len_chars(), "!")],
            doc.len_chars(),
        );
        let mut after = doc.clone();
        after.apply(&change);
        let now = Instant::now();
        manager.note_document_change("file:///doc.rs", &doc, &after, &change, now);
        assert!(manager.flush_changes(now).is_empty());
        assert!(
            manager
                .flush_changes(now + DOCUMENT_SYNC_DEBOUNCE)
                .is_empty()
        );
        manager.save_document("file:///doc.rs").unwrap();
        manager.close_document("file:///doc.rs").unwrap();
        assert!(manager.save_document("file:///doc.rs").is_err());
    }

    #[test]
    fn requests_get_ids_and_stale_work_is_cancelled_or_timed_out() {
        let args = vec![
            "-c".to_owned(),
            "read _; read _; body='{".to_owned()
                + "\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{\"capabilities\":{}}}'; printf 'Content-Length: %s\\r\\n\\r\\n%s' \"${#body}\" \"$body\"; sleep 1",
        ];
        let key = LspKey {
            language: "rust".into(),
            root: PathBuf::from("/workspace"),
        };
        let config = ServerConfig {
            program: "/bin/sh".into(),
            args,
            log_traffic: false,
            initialization_options: Value::Null,
            restart_backoff: [
                Duration::ZERO,
                Duration::from_secs(1),
                Duration::from_secs(2),
            ],
        };
        let mut manager = LspManager::default();
        manager.ensure_server(key.clone(), &config).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !manager
            .poll()
            .iter()
            .any(|event| matches!(event, LspEvent::Initialized(_)))
        {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        let now = Instant::now();
        let cancelled = manager
            .request(
                &key,
                "textDocument/hover".into(),
                Value::Null,
                Duration::from_secs(1),
                now,
            )
            .unwrap();
        assert_eq!(cancelled, 2);
        assert!(manager.cancel_request(&key, cancelled).unwrap());
        assert!(!manager.cancel_request(&key, cancelled).unwrap());
        let timed = manager
            .request(
                &key,
                "textDocument/completion".into(),
                Value::Null,
                Duration::from_millis(1),
                now,
            )
            .unwrap();
        assert_eq!(timed, 3);
        assert_eq!(
            manager.expire_requests(now + Duration::from_millis(1)),
            [LspEvent::TimedOut {
                key: key.clone(),
                id: timed,
                method: "textDocument/completion".into()
            }]
        );
        assert!(
            manager
                .expire_requests(now + Duration::from_secs(2))
                .is_empty()
        );
    }

    #[test]
    fn matching_response_completes_a_tracked_request() {
        let args = vec![
            "-c".to_owned(),
            "read _; read _; body='{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{\"capabilities\":{}}}'; printf 'Content-Length: %s\\r\\n\\r\\n%s' \"${#body}\" \"$body\"; read _; read _; body='{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"value\":1}}'; printf 'Content-Length: %s\\r\\n\\r\\n%s' \"${#body}\" \"$body\"; sleep 1".to_owned(),
        ];
        let key = LspKey {
            language: "rust".into(),
            root: PathBuf::from("/workspace"),
        };
        let config = ServerConfig {
            program: "/bin/sh".into(),
            args,
            log_traffic: false,
            initialization_options: Value::Null,
            restart_backoff: [
                Duration::ZERO,
                Duration::from_secs(1),
                Duration::from_secs(2),
            ],
        };
        let mut manager = LspManager::default();
        manager.ensure_server(key.clone(), &config).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !manager
            .poll()
            .iter()
            .any(|event| matches!(event, LspEvent::Initialized(_)))
        {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        manager
            .request(
                &key,
                "workspace/symbol".into(),
                Value::Null,
                Duration::from_secs(1),
                Instant::now(),
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let events = manager.poll();
            if let Some(LspEvent::Response {
                id, result, error, ..
            }) = events
                .into_iter()
                .find(|event| matches!(event, LspEvent::Response { .. }))
            {
                assert_eq!(id, 2);
                assert_eq!(result.unwrap()["value"], 1);
                assert_eq!(error, None);
                break;
            }
            assert!(Instant::now() < deadline, "response was not observed");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn definition_request_round_trips_after_initialize() {
        let args = vec![
            "-c".to_owned(),
            "read _; read _; body='{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{\"capabilities\":{}}}'; printf 'Content-Length: %s\\r\\n\\r\\n%s' \"${#body}\" \"$body\"; read _; read _; read _; read _; body='{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":[{\"uri\":\"file:///workspace/src/lib.rs\",\"range\":{\"start\":{\"line\":4,\"character\":2},\"end\":{\"line\":4,\"character\":8}}}]}'; printf 'Content-Length: %s\\r\\n\\r\\n%s' \"${#body}\" \"$body\"; sleep 1".to_owned(),
        ];
        let key = LspKey {
            language: "rust".into(),
            root: PathBuf::from("/workspace"),
        };
        let config = ServerConfig {
            program: "/bin/sh".into(),
            args,
            log_traffic: false,
            initialization_options: Value::Null,
            restart_backoff: [
                Duration::ZERO,
                Duration::from_secs(1),
                Duration::from_secs(2),
            ],
        };
        let mut manager = LspManager::default();
        manager.ensure_server(key.clone(), &config).unwrap();
        let document = Document::from_text("fn call() { target(); }\nfn target() {}\n");
        manager
            .open_document(
                key.clone(),
                "file:///workspace/src/main.rs".into(),
                "rust".into(),
                &document,
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !manager
            .poll()
            .iter()
            .any(|event| matches!(event, LspEvent::Initialized(_)))
        {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        manager
            .request_definition("file:///workspace/src/main.rs", 0, 13, Instant::now())
            .unwrap();
        while manager
            .definition_locations("file:///workspace/src/main.rs")
            .is_empty()
        {
            assert!(Instant::now() < deadline);
            manager.poll();
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            manager.definition_locations("file:///workspace/src/main.rs")[0],
            DefinitionLocation {
                uri: "file:///workspace/src/lib.rs".into(),
                range: Range {
                    start_line: 4,
                    start_character: 2,
                    end_line: 4,
                    end_character: 8,
                },
            }
        );
        manager.shutdown_all();
    }

    #[test]
    fn publish_diagnostics_replaces_a_uri_and_orders_problems() {
        let mut manager = LspManager::default();
        let first = serde_json::json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///b.rs","diagnostics":[
            {"range":{"start":{"line":2,"character":1},"end":{"line":2,"character":4}},"severity":2,"message":"warn","source":"rustc","code":"W1"},
            {"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"severity":1,"message":"error"}
        ]}});
        assert!(manager.publish_diagnostics(&first).unwrap());
        assert_eq!(manager.diagnostics("file:///b.rs").len(), 2);
        assert_eq!(
            manager
                .problems()
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>(),
            ["error", "warn"]
        );
        let replacement = serde_json::json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///b.rs","diagnostics":[]}});
        assert!(manager.publish_diagnostics(&replacement).unwrap());
        assert!(manager.diagnostics("file:///b.rs").is_empty());
        assert!(
            !manager
                .publish_diagnostics(&serde_json::json!({"method":"window/logMessage"}))
                .unwrap()
        );
    }

    #[test]
    fn parses_marked_hover_contents_and_range() {
        let hover = parse_hover(&serde_json::json!({
            "contents": [{"language":"rust","value":"fn main()"}, "A description"],
            "range": {"start":{"line":2,"character":1},"end":{"line":2,"character":5}}
        }))
        .unwrap();
        assert_eq!(hover.contents, "fn main()\nA description");
        assert_eq!(
            hover.range,
            Some(Range {
                start_line: 2,
                start_character: 1,
                end_line: 2,
                end_character: 5
            })
        );
    }

    #[test]
    fn parses_completion_list_and_snippet_format() {
        let items = parse_completion(&serde_json::json!({"items":[
            {"label":"println!","detail":"macro","insertText":"println!($0)","insertTextFormat":2},
            {"label":"print","textEdit":{"newText":"print"}}
        ]}))
        .unwrap();
        assert_eq!(items[0].label, "println!");
        assert!(items[0].is_snippet);
        assert_eq!(items[1].insert_text, "print");
    }

    #[test]
    fn parses_signature_parameters_and_active_parameter() {
        let signature = parse_signature(&serde_json::json!({
            "signatures": [{"label":"read(path, options)","documentation":{"kind":"markdown","value":"Reads a file."},"parameters":[{"label":"path"},{"label":"options"}]}],
            "activeSignature": 0, "activeParameter": 1
        })).unwrap();
        assert_eq!(signature.label, "read(path, options)");
        assert_eq!(signature.parameters, ["path", "options"]);
        assert_eq!(signature.active_parameter, 1);
        assert_eq!(signature.documentation.as_deref(), Some("Reads a file."));
    }

    #[test]
    fn parses_definition_locations_and_links() {
        let locations = parse_definition_locations(&serde_json::json!([
            {"uri":"file:///tmp/lib.rs","range":{"start":{"line":4,"character":2},"end":{"line":4,"character":8}}},
            {"targetUri":"file:///tmp/main.rs","targetSelectionRange":{"start":{"line":9,"character":1},"end":{"line":9,"character":5}}}
        ])).unwrap();
        assert_eq!(locations.len(), 2);
        assert_eq!(locations[0].uri, "file:///tmp/lib.rs");
        assert_eq!(locations[0].range.start_line, 4);
        assert_eq!(locations[1].uri, "file:///tmp/main.rs");
        assert_eq!(locations[1].range.start_character, 1);
    }

    #[test]
    fn parses_reference_locations() {
        let locations = parse_definition_locations(&serde_json::json!([
            {"uri":"file:///tmp/a.rs","range":{"start":{"line":1,"character":3},"end":{"line":1,"character":7}}}
        ])).unwrap();
        assert_eq!(
            locations,
            vec![DefinitionLocation {
                uri: "file:///tmp/a.rs".into(),
                range: Range {
                    start_line: 1,
                    start_character: 3,
                    end_line: 1,
                    end_character: 7
                }
            }]
        );
    }

    #[test]
    fn parses_workspace_edit_changes_and_document_changes() {
        let edits = parse_workspace_edit(&serde_json::json!({
            "changes": {"file:///a.rs": [{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"newText":"x"}]},
            "documentChanges": [{"textDocument":{"uri":"file:///b.rs","version":null},"edits":[{"range":{"start":{"line":2,"character":1},"end":{"line":2,"character":2}},"newText":"y"}]}]
        })).unwrap();
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].uri, "file:///a.rs");
        assert_eq!(edits[1].new_text, "y");
    }

    #[test]
    fn parses_document_formatting_text_edits() {
        let edits = parse_text_edit_array(
            "file:///main.rs",
            &serde_json::json!([
                {"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":3}},"newText":"let"}
            ]),
        )
        .unwrap();
        assert_eq!(
            edits,
            vec![RenameEdit {
                uri: "file:///main.rs".into(),
                range: Range {
                    start_line: 0,
                    start_character: 0,
                    end_line: 0,
                    end_character: 3
                },
                new_text: "let".into(),
            }]
        );
    }
}
