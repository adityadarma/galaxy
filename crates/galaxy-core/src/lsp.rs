//! JSON-RPC 2.0 transport for language servers over stdio (M4-01).
//!
//! The transport owns a Tokio child process. A reader task decodes framed
//! messages and forwards them through a standard channel so the rest of the
//! editor can remain synchronous. No UI call waits for server output.

use std::collections::HashMap;
use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, mpsc};
use std::time::Duration;
use std::time::Instant;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::runtime::Runtime;

use crate::change::ChangeSet;
use crate::document::Document;

pub const DOCUMENT_SYNC_DEBOUNCE: Duration = Duration::from_millis(50);

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

/// Latest server diagnostics, replaced atomically per URI by
/// `textDocument/publishDiagnostics` as required by LSP.
#[derive(Debug, Default)]
pub struct DiagnosticStore {
    by_uri: HashMap<String, Vec<Diagnostic>>,
}

impl DiagnosticStore {
    pub fn publish(&mut self, message: &Value) -> Result<bool, String> {
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
        self.by_uri.insert(uri, diagnostics);
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

/// One pending `textDocument/didChange` batch. Contiguous edits retain a
/// UTF-16 range; a burst/multi-cursor batch falls back to full text safely.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingChange {
    pub version: u64,
    pub text: String,
    pub range: Option<Range>,
    pub range_length: Option<u64>,
    due_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start_line: u64,
    pub start_character: u64,
    pub end_line: u64,
    pub end_character: u64,
}

/// Tracks LSP document versions and debounces outgoing changes.
#[derive(Debug)]
pub struct DocumentSync {
    uri: String,
    language_id: String,
    version: u64,
    pending: Option<PendingChange>,
}

impl DocumentSync {
    pub fn new(uri: String, language_id: String) -> Self {
        Self {
            uri,
            language_id,
            version: 1,
            pending: None,
        }
    }

    pub fn did_open(&self, doc: &Document) -> Value {
        serde_json::json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":self.uri,"languageId":self.language_id,"version":self.version,"text":doc.text()}}})
    }

    pub fn note_full_text(&mut self, text: String, now: Instant) {
        self.version += 1;
        self.pending = Some(PendingChange {
            version: self.version,
            text,
            range: None,
            range_length: None,
            due_at: now + DOCUMENT_SYNC_DEBOUNCE,
        });
    }

    /// Records a change made to `before`; `after` must be the resulting doc.
    /// More than one change uses full text to avoid transformed offset bugs.
    pub fn note_change(
        &mut self,
        before: &Document,
        after: &Document,
        changes: &ChangeSet,
        now: Instant,
    ) {
        self.version += 1;
        let range = (changes.len() == 1)
            .then(|| changes.iter().next())
            .flatten()
            .map(|change| {
                let start = before.char_to_utf16(change.start());
                let end = before.char_to_utf16(change.end());
                Range {
                    start_line: start.line as u64,
                    start_character: start.col as u64,
                    end_line: end.line as u64,
                    end_character: end.col as u64,
                }
            });
        let range_length = (changes.len() == 1)
            .then(|| changes.iter().next())
            .flatten()
            .map(|change| {
                let start = before.rope().char_to_utf16_cu(change.start());
                let end = before.rope().char_to_utf16_cu(change.end());
                (end - start) as u64
            });
        let text = if let Some(change) = changes.iter().next().filter(|_| changes.len() == 1) {
            change.text().to_owned()
        } else {
            after.text()
        };
        let incremental = range.is_some();
        match &mut self.pending {
            Some(pending) => {
                // A second edit invalidates a former range against the server's
                // version, so collapse this debounce batch to full text.
                pending.version = self.version;
                pending.text = after.text();
                pending.range = None;
                pending.range_length = None;
                pending.due_at = now + DOCUMENT_SYNC_DEBOUNCE;
            }
            None => {
                self.pending = Some(PendingChange {
                    version: self.version,
                    text,
                    range: incremental.then_some(range.unwrap()),
                    range_length: incremental.then_some(range_length.unwrap()),
                    due_at: now + DOCUMENT_SYNC_DEBOUNCE,
                })
            }
        }
    }

    pub fn flush(&mut self, now: Instant) -> Option<Value> {
        let pending = self
            .pending
            .as_ref()
            .filter(|pending| now >= pending.due_at)?;
        let content = match pending.range {
            Some(range) => {
                serde_json::json!({"range":{"start":{"line":range.start_line,"character":range.start_character},"end":{"line":range.end_line,"character":range.end_character}},"rangeLength":pending.range_length,"text":pending.text})
            }
            None => serde_json::json!({"text":pending.text}),
        };
        let version = pending.version;
        self.pending = None;
        Some(
            serde_json::json!({"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":self.uri,"version":version},"contentChanges":[content]}}),
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
                },
            );
        }
        Ok(out)
    }

    /// Resolves a server override first, otherwise the built-in language
    /// default. Relative or bare commands are found in `PATH` at launch time.
    pub fn resolve(&self, language: &str) -> Option<ServerConfig> {
        self.servers
            .get(&language.to_ascii_lowercase())
            .cloned()
            .or_else(|| default_server(language))
    }
}

fn default_server(language: &str) -> Option<ServerConfig> {
    let (program, args) = match default_server_kind(language)? {
        "rust" => ("rust-analyzer", &[][..]),
        "typescript" => ("typescript-language-server", &["--stdio"] as &[&str]),
        "python" => ("pyright-langserver", &["--stdio"] as &[&str]),
        "go" => ("gopls", &[] as &[&str]),
        _ => return None,
    };
    find_in_path(program).map(|program| ServerConfig {
        program: program.to_string_lossy().into_owned(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        log_traffic: false,
        initialization_options: Value::Null,
    })
}

fn default_server_kind(language: &str) -> Option<&'static str> {
    let normalized = language.to_ascii_lowercase().replace([' ', '-'], "");
    match normalized.as_str() {
        "rust" => Some("rust"),
        "typescript" | "typescriptreact" | "tsx" | "javascript" | "javascriptreact" | "js" => {
            Some("typescript")
        }
        "python" => Some("python"),
        "go" => Some("go"),
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
    env::var_os("PATH")?
        .as_os_str()
        .to_str()?
        .split(':')
        .map(Path::new)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
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
    documents: HashMap<String, (LspKey, DocumentSync)>,
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
}

impl LspManager {
    /// Starts and initializes a server unless an equivalent key already runs.
    /// `true` means a child was spawned; `false` means the shared instance won.
    pub fn ensure_server(&mut self, key: LspKey, config: &ServerConfig) -> io::Result<bool> {
        if self.servers.contains_key(&key) {
            return Ok(false);
        }
        let transport = LspTransport::spawn(&config.program, &config.args, config.log_traffic)?;
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
            key,
            ManagedServer {
                transport,
                initialized: false,
            },
        );
        Ok(true)
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
        let Some(key) = self.documents.get(uri).map(|(key, _)| key.clone()) else {
            return Ok(None);
        };
        self.definition_locations.remove(uri);
        let id = self.request(
            &key,
            method.into(),
            serde_json::json!({
                "textDocument": { "uri": uri }, "position": { "line": line, "character": character }
            }),
            Duration::from_millis(1000),
            now,
        )?;
        self.definition_requests.insert((key, id), uri.to_owned());
        Ok(Some(id))
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
        let sync = DocumentSync::new(uri.clone(), language_id);
        self.send(&key, &sync.did_open(doc))?;
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
    }

    pub fn note_full_text(&mut self, uri: &str, text: String, now: Instant) {
        if let Some((_, sync)) = self.documents.get_mut(uri) {
            sync.note_full_text(text, now);
        }
    }

    pub fn flush_changes(&mut self, now: Instant) -> Vec<io::Error> {
        let ready: Vec<_> = self
            .documents
            .values_mut()
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
        self.send(key, &sync.did_save())
    }

    pub fn close_document(&mut self, uri: &str) -> io::Result<()> {
        let (key, sync) = self
            .documents
            .remove(uri)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LSP document is not open"))?;
        self.send(&key, &sync.did_close())
    }

    /// Drains server output without blocking. An initialize response with ID 0
    /// transitions the server and emits the mandatory `initialized` notification.
    pub fn poll(&mut self) -> Vec<LspEvent> {
        let mut events = Vec::new();
        for (key, server) in &mut self.servers {
            loop {
                match server.transport.try_recv() {
                    Ok(Incoming::Message(message)) if !server.initialized && message.get("id") == Some(&Value::from(0)) => {
                        match server.transport.send(&serde_json::json!({"jsonrpc": "2.0", "method": "initialized", "params": {}})) {
                            Ok(()) => { server.initialized = true; events.push(LspEvent::Initialized(key.clone())); }
                            Err(error) => events.push(LspEvent::Error(key.clone(), error.to_string())),
                        }
                    }
                    Ok(Incoming::Message(message)) if message.get("id").and_then(Value::as_u64).is_some() => {
                        let id = message["id"].as_u64().expect("checked above");
                        if let Some(uri) = self.hover_requests.remove(&(key.clone(), id))
                            && let Some(value) = message.get("result").and_then(parse_hover)
                        {
                            self.hover.insert(uri, value);
                        }
                        if let Some(uri) = self.completion_requests.remove(&(key.clone(), id))
                            && let Some(result) = message.get("result").and_then(parse_completion)
                        { self.completion.insert(uri, result); }
                        if let Some(uri) = self.signature_requests.remove(&(key.clone(), id))
                            && let Some(result) = message.get("result").and_then(parse_signature)
                        { self.signatures.insert(uri, result); }
                        if let Some(uri) = self.definition_requests.remove(&(key.clone(), id))
                            && let Some(result) = message.get("result").and_then(parse_definition_locations)
                        { self.definition_locations.insert(uri, result); }
                        if let Some(uri) = self.reference_requests.remove(&(key.clone(), id))
                            && let Some(result) = message.get("result").and_then(parse_definition_locations)
                        { self.references.insert(uri, result); }
                        if let Some(uri) = self.rename_requests.remove(&(key.clone(), id))
                            && let Some(result) = message.get("result").and_then(parse_workspace_edit)
                        { self.rename_edits.insert(uri, result); }
                        if let Some(uri) = self.code_action_requests.remove(&(key.clone(), id))
                            && let Some(result) = message.get("result").and_then(parse_code_actions)
                        { self.code_actions.insert(uri, result); }
                        if let Some(uri) = self.formatting_requests.remove(&(key.clone(), id))
                            && let Some(result) = message.get("result").and_then(|value| parse_text_edit_array(&uri, value))
                        { self.formatting_edits.insert(uri, result); }
                        if let Some(uri) = self.range_formatting_requests.remove(&(key.clone(), id))
                            && let Some(result) = message.get("result").and_then(|value| parse_text_edit_array(&uri, value))
                        { self.formatting_edits.insert(uri, result); }
                        if let Some(uri) = self.on_type_formatting_requests.remove(&(key.clone(), id))
                            && let Some(result) = message.get("result").and_then(|value| parse_text_edit_array(&uri, value))
                        { self.formatting_edits.insert(uri, result); }
                        if let Some(uri) = self.document_symbol_requests.remove(&(key.clone(), id))
                            && let Some(result) = message.get("result").and_then(parse_document_symbols)
                        { self.document_symbols.insert(uri, result); }
                        if let Some(root) = self.workspace_symbol_requests.remove(&(key.clone(), id))
                            && let Some(result) = message.get("result").and_then(parse_workspace_symbols)
                        { self.workspace_symbols.insert(root, result); }
                        if self.requests.remove(&(key.clone(), id)).is_some() {
                            events.push(LspEvent::Response {
                                key: key.clone(), id,
                                result: message.get("result").cloned(), error: message.get("error").cloned(),
                            });
                        }
                    }
                    Ok(Incoming::Message(message)) => events.push(LspEvent::Message(key.clone(), message)),
                    Ok(Incoming::Error(error)) => events.push(LspEvent::Error(key.clone(), error)),
                    Ok(Incoming::Closed) => { events.push(LspEvent::Closed(key.clone())); break; }
                    Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
                }
            }
        }
        for event in &events {
            if let LspEvent::Message(_, message) = event
                && let Err(error) = self.publish_diagnostics(message)
            {
                tracing::warn!(%error, "discarded invalid LSP diagnostics notification");
            }
        }
        events
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

/// Stdio JSON-RPC transport for one language-server process.
pub struct LspTransport {
    runtime: Runtime,
    child: Child,
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    incoming: mpsc::Receiver<Incoming>,
    log_traffic: bool,
}

impl LspTransport {
    /// Starts `program args...` with piped stdio. `log_traffic` writes JSON
    /// payloads to the tracing debug target `galaxy_lsp_traffic`.
    pub fn spawn(program: &str, args: &[String], log_traffic: bool) -> io::Result<Self> {
        let runtime = Runtime::new()?;
        let mut command = Command::new(program);
        command
            .args(args)
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
    use std::thread;
    use std::time::Instant;
    use tokio::io::AsyncWriteExt;

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
        let transport = LspTransport::spawn("/bin/sh", &args, false).unwrap();
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
    fn settings_parse_override_and_preserve_initialization_options() {
        let settings = LspSettings::from_json(&serde_json::json!({
            "servers": { "Rust": {
                "command": "/bin/sh", "args": ["-c", "server"], "logTraffic": true,
                "initializationOptions": { "checkOnSave": true }
            }}
        }))
        .unwrap();
        let config = settings.resolve("rust").unwrap();
        assert_eq!(config.program, "/bin/sh");
        assert_eq!(config.args, ["-c", "server"]);
        assert!(config.log_traffic);
        assert_eq!(config.initialization_options["checkOnSave"], true);
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
        let mut sync = DocumentSync::new("file:///test.rs".into(), "rust".into());
        assert_eq!(
            sync.did_open(&before)["params"]["textDocument"]["text"],
            "a😀中b"
        );
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
        let burst = sync.flush(now + DOCUMENT_SYNC_DEBOUNCE * 3).unwrap();
        assert!(burst["params"]["contentChanges"][0].get("range").is_none());
        assert_eq!(sync.did_save()["method"], "textDocument/didSave");
        assert_eq!(sync.did_close()["method"], "textDocument/didClose");
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
        let args = vec!["-c".to_owned(), "sleep 1".to_owned()];
        let key = LspKey {
            language: "rust".into(),
            root: PathBuf::from("/workspace"),
        };
        let config = ServerConfig {
            program: "/bin/sh".into(),
            args,
            log_traffic: false,
            initialization_options: Value::Null,
        };
        let mut manager = LspManager::default();
        manager.ensure_server(key.clone(), &config).unwrap();
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
            "read _; read _; read _; read _; body='{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"value\":1}}'; printf 'Content-Length: %s\\r\\n\\r\\n%s' \"${#body}\" \"$body\"; sleep 1".to_owned(),
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
        };
        let mut manager = LspManager::default();
        manager.ensure_server(key.clone(), &config).unwrap();
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
