//! Bounded native Rust navigation and analysis.
//!
//! This module is the Rust semantic engine. It does not start or depend on an
//! external language server. Type inference, macro expansion, and Cargo
//! feature selection are implemented here as the native engine grows.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, mpsc};
use std::time::{Duration, Instant};

use ignore::WalkBuilder;
use tree_sitter::{Node, Parser, Tree};

use crate::analysis::{DiagnosticSeverity, Range, RenameEdit};
use crate::lang::{
    Action, Completion, Edit, HoverResult, Insertion, ItemKind, Occurrence, OutlineSymbol, Problem,
    Severity, Signature, Target,
};
use crate::parameter_hints::{self, Argument, Parameter, ParameterHint};

const MAX_FILES: usize = 4_000;
const MAX_FILE_BYTES: u64 = 2 << 20;
const MAX_ITEMS: usize = 200;
const MAX_PROBLEMS: usize = 200;
const MAX_OCCURRENCES: usize = 2_000;
const CARGO_OUTPUT_LIMIT: usize = 64 * 1024;
const CARGO_RETAINED_JSON_LIMIT: usize = 8 * 1024 * 1024;
const CARGO_REFRESH_TIMEOUT: Duration = Duration::from_secs(120);
const CARGO_OUTPUT_COLLECTION_TIMEOUT: Duration = Duration::from_secs(2);
const NO_DEFAULT_FEATURES: &str = "__galaxy_no_default_features";
const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];
const STD_TYPES: &[&str] = &[
    "String", "Vec", "Option", "Result", "Box", "Rc", "Arc", "HashMap", "HashSet",
];

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Declaration {
    name: String,
    crate_name: String,
    module: Vec<String>,
    visibility: Visibility,
    kind: ItemKind,
    path: PathBuf,
    line: u32,
    column: u32,
    scope_depth: usize,
    signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Visibility {
    Private,
    Public,
    Restricted(Vec<String>),
}

#[derive(Debug, Clone)]
struct CargoDependency {
    name: String,
    package: String,
    path: Option<PathBuf>,
    git: bool,
    optional: bool,
    default_features: bool,
    features: HashSet<String>,
}

#[derive(Debug, Clone)]
struct CargoTarget {
    name: String,
    source_root: PathBuf,
    root: PathBuf,
}

#[derive(Debug, Clone)]
struct CargoPackage {
    name: String,
    root: PathBuf,
    targets: Vec<CargoTarget>,
    dependencies: Vec<CargoDependency>,
    selected_features: HashSet<String>,
    indexed: bool,
}

#[derive(Debug, Clone, Default)]
struct CfgContext {
    features: HashSet<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct UseBinding {
    local: String,
    target: Vec<String>,
    glob: bool,
    public: bool,
    module: Vec<String>,
}

/// Project declarations built on a worker thread.
#[derive(Debug, Default)]
pub struct Built {
    declarations: Vec<Declaration>,
    files: Vec<PathBuf>,
    uses: HashMap<PathBuf, Vec<UseBinding>>,
    module_identities: HashMap<PathBuf, Vec<String>>,
    source_roots: Vec<PathBuf>,
    packages: Vec<CargoPackage>,
}

/// Static index for one Cargo project or repository.
#[derive(Debug)]
pub struct RustEngine {
    declarations: Vec<Declaration>,
    files: Vec<PathBuf>,
    uses: HashMap<PathBuf, Vec<UseBinding>>,
    module_identities: HashMap<PathBuf, Vec<String>>,
    source_roots: Vec<PathBuf>,
    packages: Vec<CargoPackage>,
    open: HashMap<PathBuf, (Vec<Declaration>, Vec<UseBinding>, String)>,
    ready: bool,
    /// `canonicalize` results. Name resolution asks for the crate and module
    /// of every indexed file per identifier; a `realpath` syscall each time
    /// made workspace diagnostics take minutes.
    canonical_paths: RwLock<HashMap<PathBuf, PathBuf>>,
    /// `crate_for` results, for the same reason: it scans every Cargo target.
    crate_names: RwLock<HashMap<PathBuf, String>>,
    /// `member_declarations` results per file, type and member name. Hover and
    /// definition on a method chain ask once per link, and each answer parsed
    /// every candidate file again. Entries are checked against the file's
    /// current stamp, so no explicit invalidation is needed.
    member_cache: Mutex<HashMap<MemberKey, (SourceStamp, Vec<Declaration>)>>,
}

/// File, type and member name of a `member_cache` entry.
type MemberKey = (PathBuf, String, String);

/// Identifies one version of a source file: the hash of an open buffer, or
/// the modification time and size of a file on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SourceStamp {
    Open(u64),
    Disk(std::time::SystemTime, u64),
}

const MEMBER_CACHE_LIMIT: usize = 4096;

fn text_stamp(text: &str) -> SourceStamp {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    SourceStamp::Open(hasher.finish())
}

fn disk_stamp(path: &Path) -> Option<SourceStamp> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(SourceStamp::Disk(metadata.modified().ok()?, metadata.len()))
}

/// Result of a Cargo refresh requested manually or after saving Rust source.
/// Indexing and file watching never invoke Cargo because it can execute code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedSourceRefresh {
    pub succeeded: bool,
    pub timed_out: bool,
    pub canceled: bool,
    pub compiler_artifacts: u32,
    pub message: String,
    pub diagnostics: Vec<CompilerDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilerDiagnostic {
    pub uri: String,
    pub range: Range,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub code: Option<String>,
    pub actions: Vec<crate::analysis::CodeActionInfo>,
}

struct ActiveGeneratedSourceRefresh {
    child: Mutex<Child>,
    canceled: AtomicBool,
}

fn active_generated_source_refreshes()
-> &'static Mutex<HashMap<PathBuf, Arc<ActiveGeneratedSourceRefresh>>> {
    static ACTIVE: OnceLock<Mutex<HashMap<PathBuf, Arc<ActiveGeneratedSourceRefresh>>>> =
        OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_generated_source_refresh(
    root: &Path,
    refresh: Arc<ActiveGeneratedSourceRefresh>,
) -> bool {
    let mut active = active_generated_source_refreshes()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if active.contains_key(root) {
        return false;
    }
    active.insert(root.to_path_buf(), refresh);
    true
}

fn unregister_generated_source_refresh(root: &Path, refresh: &Arc<ActiveGeneratedSourceRefresh>) {
    let mut active = active_generated_source_refreshes()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if active
        .get(root)
        .is_some_and(|registered| Arc::ptr_eq(registered, refresh))
    {
        active.remove(root);
    }
}

/// Cancels the active Cargo refresh for `root`. Returns false when none is running.
pub fn cancel_generated_source_refresh(root: &Path) -> bool {
    let refresh = active_generated_source_refreshes()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(root)
        .cloned();
    let Some(refresh) = refresh else {
        return false;
    };
    let killed = refresh
        .child
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .kill()
        .is_ok();
    if killed {
        refresh.canceled.store(true, Ordering::Release);
    }
    killed
}

/// Runs Cargo to update build-script and proc-macro output for `root`. An
/// automatic refresh checks only the package containing `source`; a manual
/// refresh checks the workspace.
/// Uses no shell and bounds the command duration and captured output.
pub fn refresh_generated_sources(root: &Path, source: Option<&Path>) -> GeneratedSourceRefresh {
    let child = match spawn_cargo(root, source) {
        Ok(child) => child,
        Err(message) => return refresh_failure(message),
    };
    let refresh = Arc::new(ActiveGeneratedSourceRefresh {
        child: Mutex::new(child),
        canceled: AtomicBool::new(false),
    });
    if !register_generated_source_refresh(root, Arc::clone(&refresh)) {
        let mut child = refresh
            .child
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _ = child.kill();
        let _ = child.wait();
        return refresh_failure("A Cargo refresh is already running for this workspace.".into());
    }
    let result = (|| {
        let (stdout, stderr) = {
            let mut child = refresh
                .child
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            (
                child.stdout.take().map(read_cargo_stdout),
                child.stderr.take().map(read_bounded),
            )
        };
        let deadline = Instant::now() + CARGO_REFRESH_TIMEOUT;
        let (status, timed_out) = loop {
            let mut child = refresh
                .child
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match child.try_wait() {
                Ok(Some(status)) => break (Some(status), false),
                Ok(None) if Instant::now() < deadline => {
                    drop(child);
                    std::thread::sleep(Duration::from_millis(50));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break (None, true);
                }
                Err(error) => return refresh_failure(format!("Could not wait for cargo: {error}")),
            }
        };
        let stdout = stdout
            .map(|reader| collect_bounded(reader, CARGO_OUTPUT_COLLECTION_TIMEOUT))
            .unwrap_or_default();
        let stderr = stderr
            .map(|reader| collect_bounded(reader, CARGO_OUTPUT_COLLECTION_TIMEOUT))
            .unwrap_or_default();
        let compiler_artifacts = cargo_compiler_artifact_count(&stdout);
        if refresh.canceled.load(Ordering::Acquire) {
            return refresh_canceled(compiler_artifacts);
        }
        if timed_out {
            return GeneratedSourceRefresh {
                succeeded: false,
                timed_out: true,
                canceled: false,
                compiler_artifacts,
                message: "Cargo refresh timed out after 120 seconds.".into(),
                diagnostics: Vec::new(),
            };
        }
        let diagnostics = parse_compiler_diagnostics(root, &stdout);
        if status.is_some_and(|status| status.success()) {
            return GeneratedSourceRefresh {
                succeeded: true,
                timed_out: false,
                canceled: false,
                compiler_artifacts,
                message: format!(
                    "Rust checked: {} errors, {} warnings.",
                    diagnostics
                        .iter()
                        .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
                        .count(),
                    diagnostics
                        .iter()
                        .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Warning)
                        .count(),
                ),
                diagnostics,
            };
        }
        let errors = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
            .count();
        let warnings = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Warning)
            .count();
        let message = if errors + warnings > 0 {
            format!("Rust check found {errors} errors, {warnings} warnings.")
        } else {
            cargo_failure_message(&stderr)
        };
        let mut failure = refresh_failure(message);
        failure.diagnostics = diagnostics;
        failure
    })();
    unregister_generated_source_refresh(root, &refresh);
    result
}

fn refresh_canceled(compiler_artifacts: u32) -> GeneratedSourceRefresh {
    GeneratedSourceRefresh {
        succeeded: false,
        timed_out: false,
        canceled: true,
        compiler_artifacts,
        message: "Cargo refresh canceled.".into(),
        diagnostics: Vec::new(),
    }
}

fn parse_compiler_diagnostics(root: &Path, output: &[u8]) -> Vec<CompilerDiagnostic> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut out = Vec::new();
    for line in String::from_utf8_lossy(output).lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(message) = value.get("message").filter(|_| {
            value.get("reason").and_then(serde_json::Value::as_str) == Some("compiler-message")
        }) else {
            continue;
        };
        let Some(span) = message
            .get("spans")
            .and_then(serde_json::Value::as_array)
            .and_then(|spans| {
                spans.iter().find(|span| {
                    span.get("is_primary").and_then(serde_json::Value::as_bool) == Some(true)
                })
            })
        else {
            continue;
        };
        let Some((path, range)) = compiler_span(root.as_path(), span) else {
            continue;
        };
        let level = message
            .get("level")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("error");
        let severity = match level {
            "warning" => DiagnosticSeverity::Warning,
            "note" => DiagnosticSeverity::Information,
            "help" => DiagnosticSeverity::Hint,
            _ => DiagnosticSeverity::Error,
        };
        let mut text = message
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Rust compiler diagnostic")
            .to_owned();
        let labels: Vec<_> = message
            .get("spans")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|span| span.get("label").and_then(serde_json::Value::as_str))
            .collect();
        if !labels.is_empty() {
            text.push_str(&format!("\n{}", labels.join("\n")));
        }
        let code = message
            .get("code")
            .and_then(|code| code.get("code").or(Some(code)))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let edits: Vec<RenameEdit> = message
            .get("children")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|child| {
                child
                    .get("spans")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .filter(|span| {
                span.get("suggestion_applicability")
                    .and_then(serde_json::Value::as_str)
                    == Some("MachineApplicable")
            })
            .filter_map(|span| {
                Some((
                    compiler_span(root.as_path(), span)?,
                    span.get("suggested_replacement")?.as_str()?.to_owned(),
                ))
            })
            .filter(|((suggestion_path, _), _)| suggestion_path == &path)
            .map(|((_, range), new_text)| RenameEdit {
                uri: file_uri_for_compiler(&path),
                range,
                new_text,
            })
            .collect();
        let actions = if edits.is_empty() {
            Vec::new()
        } else {
            vec![crate::analysis::CodeActionInfo {
                title: message
                    .get("children")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .find_map(|child| child.get("message").and_then(serde_json::Value::as_str))
                    .unwrap_or("Apply compiler suggestion")
                    .to_owned(),
                kind: Some("quickfix".into()),
                edits,
            }]
        };
        let uri = file_uri_for_compiler(&path);
        // `--all-targets` can report the same source error for lib and test
        // targets. Keep one only when the diagnostic and its available fixes match.
        if out.iter().any(|existing: &CompilerDiagnostic| {
            existing.uri == uri
                && existing.range == range
                && existing.severity == severity
                && existing.message == text
                && existing.code == code
                && existing.actions == actions
        }) {
            continue;
        }
        out.push(CompilerDiagnostic {
            uri,
            range,
            severity,
            message: text,
            code,
            actions,
        });
    }
    out
}

fn compiler_span(root: &Path, span: &serde_json::Value) -> Option<(PathBuf, Range)> {
    let path = PathBuf::from(span.get("file_name")?.as_str()?);
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    let path = path.canonicalize().ok()?;
    if !path.starts_with(root) {
        return None;
    }
    let source = std::fs::read_to_string(&path).ok()?;
    let line = |number: u64| source.lines().nth(number.saturating_sub(1) as usize);
    let column = |number: u64, column: u64| {
        line(number).map(|line| {
            line.chars()
                .take(column.saturating_sub(1) as usize)
                .map(char::len_utf16)
                .sum::<usize>() as u64
        })
    };
    Some((
        path,
        Range {
            start_line: span.get("line_start")?.as_u64()?.saturating_sub(1),
            start_character: column(
                span.get("line_start")?.as_u64()?,
                span.get("column_start")?.as_u64()?,
            )?,
            end_line: span.get("line_end")?.as_u64()?.saturating_sub(1),
            end_character: column(
                span.get("line_end")?.as_u64()?,
                span.get("column_end")?.as_u64()?,
            )?,
        },
    ))
}

fn file_uri_for_compiler(path: &Path) -> String {
    format!("file://{}", path.to_string_lossy())
}

fn cargo_refresh_args(root: &Path, source: Option<&Path>) -> Vec<String> {
    let mut args = vec!["check".to_owned()];
    if let Some(package) = source.and_then(|source| cargo_package_name_for_file(root, source)) {
        args.extend(["-p".to_owned(), package]);
    } else {
        args.push("--workspace".to_owned());
    }
    args.extend([
        "--all-targets".to_owned(),
        "--offline".to_owned(),
        "--message-format=json".to_owned(),
    ]);
    args
}

fn cargo_package_name_for_file(root: &Path, source: &Path) -> Option<String> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let source = source
        .canonicalize()
        .unwrap_or_else(|_| source.to_path_buf());
    source
        .ancestors()
        .take_while(|directory| directory.starts_with(&root))
        .find_map(|directory| {
            let contents = std::fs::read_to_string(directory.join("Cargo.toml")).ok()?;
            toml_section_value(&contents, "package", "name")
        })
}

fn cargo_executables(cargo_home: Option<&Path>, home: Option<&Path>) -> Vec<PathBuf> {
    let cargo_name = format!("cargo{}", std::env::consts::EXE_SUFFIX);
    let home_cargo = home.map(|home| home.join(".cargo"));
    let mut executables = Vec::new();
    for root in [cargo_home, home_cargo.as_deref()] {
        let Some(root) = root else {
            continue;
        };
        let executable = root.join("bin").join(&cargo_name);
        if is_executable_file(&executable) && !executables.contains(&executable) {
            executables.push(executable);
        }
    }
    executables.push(PathBuf::from(cargo_name));
    executables
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn spawn_cargo(root: &Path, source: Option<&Path>) -> Result<Child, String> {
    let cargo_home = std::env::var_os("CARGO_HOME").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    for executable in cargo_executables(cargo_home.as_deref(), home.as_deref()) {
        let mut command = Command::new(executable);
        command
            .args(cargo_refresh_args(root, source))
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("Could not start cargo: {error}")),
        }
    }
    Err("Could not find cargo. Install Rust with rustup or set CARGO_HOME/PATH.".into())
}

fn read_bounded<R: Read + Send + 'static>(mut reader: R) -> mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0; 8192];
        while let Ok(read) = reader.read(&mut buffer) {
            if read == 0 {
                break;
            }
            let remaining = CARGO_OUTPUT_LIMIT.saturating_sub(output.len());
            output.extend_from_slice(&buffer[..read.min(remaining)]);
        }
        let _ = sender.send(output);
    });
    receiver
}

fn read_cargo_stdout<R: Read + Send + 'static>(mut reader: R) -> mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let mut retained = Vec::new();
        let mut line = Vec::new();
        let mut line_overflowed = false;
        let mut buffer = [0; 8192];
        while let Ok(read) = reader.read(&mut buffer) {
            if read == 0 {
                break;
            }
            for byte in &buffer[..read] {
                if !line_overflowed {
                    if line.len() < CARGO_RETAINED_JSON_LIMIT {
                        line.push(*byte);
                    } else {
                        line_overflowed = true;
                    }
                }
                if *byte != b'\n' {
                    continue;
                }
                if !line_overflowed && output.len() + line.len() <= CARGO_OUTPUT_LIMIT {
                    output.extend_from_slice(&line);
                } else if !line_overflowed
                    && is_retained_cargo_record(&line)
                    && retained.len() + line.len() <= CARGO_RETAINED_JSON_LIMIT
                {
                    retained.extend_from_slice(&line);
                }
                line.clear();
                line_overflowed = false;
            }
        }
        if !retained.is_empty() && !output.is_empty() {
            output.push(b'\n');
        }
        output.extend_from_slice(&retained);
        let _ = sender.send(output);
    });
    receiver
}

fn is_retained_cargo_record(line: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(line).is_ok_and(|message| {
        matches!(
            message.get("reason").and_then(serde_json::Value::as_str),
            Some("compiler-message" | "build-finished")
        )
    })
}

fn collect_bounded(receiver: mpsc::Receiver<Vec<u8>>, timeout: Duration) -> Vec<u8> {
    receiver.recv_timeout(timeout).unwrap_or_default()
}

fn cargo_compiler_artifact_count(stdout: &[u8]) -> u32 {
    std::str::from_utf8(stdout)
        .unwrap_or_default()
        .lines()
        .filter(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|message| {
                    message
                        .get("reason")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                .is_some_and(|reason| reason == "compiler-artifact")
        })
        .count() as u32
}

fn cargo_failure_message(stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let detail = stderr
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("Cargo check failed.")
        .trim();
    format!(
        "Cargo refresh failed: {}",
        detail.chars().take(240).collect::<String>()
    )
}

fn refresh_failure(message: String) -> GeneratedSourceRefresh {
    GeneratedSourceRefresh {
        succeeded: false,
        timed_out: false,
        canceled: false,
        compiler_artifacts: 0,
        message,
        diagnostics: Vec::new(),
    }
}

impl RustEngine {
    pub fn index_stats(&self) -> crate::LanguageIndexStats {
        let imports = self.uses.values().map(Vec::len).sum();
        let source_bytes = self.open.values().map(|(_, _, text)| text.len()).sum();
        crate::LanguageIndexStats {
            files: self.files.len(),
            symbols: self.declarations.len(),
            imports,
            source_bytes,
            heap_lower_bound_bytes: self.files.len() * std::mem::size_of::<PathBuf>()
                + self.declarations.len() * std::mem::size_of::<Declaration>()
                + imports * std::mem::size_of::<UseBinding>()
                + source_bytes,
            ..Default::default()
        }
    }

    pub fn pending(_root: &Path) -> Self {
        Self {
            declarations: Vec::new(),
            files: Vec::new(),
            uses: HashMap::new(),
            module_identities: HashMap::new(),
            source_roots: Vec::new(),
            packages: Vec::new(),
            open: HashMap::new(),
            ready: false,
            canonical_paths: RwLock::new(HashMap::new()),
            crate_names: RwLock::new(HashMap::new()),
            member_cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn build(root: &Path) -> Built {
        Self::build_with_cache(root, &mut RustIndexCache::default())
    }

    /// Builds the index, reparsing only files whose size or modification time
    /// differ from `cache`. `cache` is replaced with entries for this build.
    pub fn build_with_cache(root: &Path, cache: &mut RustIndexCache) -> Built {
        Self::build_with_scheduler_and_cache(
            root,
            crate::index_scheduler::shared_index_scheduler(),
            cache,
        )
    }

    pub fn build_with_scheduler(
        root: &Path,
        scheduler: &crate::index_scheduler::IndexScheduler,
    ) -> Built {
        Self::build_with_scheduler_and_cache(root, scheduler, &mut RustIndexCache::default())
    }

    fn build_with_scheduler_and_cache(
        root: &Path,
        scheduler: &crate::index_scheduler::IndexScheduler,
        cache: &mut RustIndexCache,
    ) -> Built {
        let mut declarations = Vec::new();
        let mut files = Vec::new();
        let mut uses = HashMap::new();
        let mut file_cache = FileCache {
            previous: std::mem::take(&mut cache.files),
            next: HashMap::new(),
        };
        let packages = cargo_packages(root);
        let roots = cargo_source_roots(root);
        let module_graph = static_module_graph(&packages, &mut file_cache);
        let mut candidates = Vec::new();
        for package in &packages {
            if !package.indexed {
                continue;
            }
            for target in &package.targets {
                let source_root = &target.source_root;
                let mut walker = WalkBuilder::new(source_root);
                walker
                    .hidden(false)
                    .git_ignore(true)
                    .git_global(true)
                    .parents(true);
                for entry in walker.build().flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|ext| ext.to_str()) != Some("rs")
                        || entry
                            .metadata()
                            .is_ok_and(|metadata| metadata.len() > MAX_FILE_BYTES)
                    {
                        continue;
                    }
                    let features = package.selected_features.clone();
                    candidates.push((
                        path.to_path_buf(),
                        source_root.clone(),
                        target.name.clone(),
                        CfgContext { features },
                        module_graph.identities.get(path).cloned(),
                        module_graph.uses.get(path).cloned(),
                    ));
                }
            }
        }
        for path in module_graph.identities.keys() {
            if candidates.iter().any(|candidate| candidate.0 == *path) {
                continue;
            }
            if let Some(package) = packages
                .iter()
                .find(|package| path.starts_with(&package.root))
                && let Some(target) = package.targets.first()
            {
                candidates.push((
                    path.clone(),
                    target.source_root.clone(),
                    target.name.clone(),
                    CfgContext {
                        features: package.selected_features.clone(),
                    },
                    module_graph.identities.get(path).cloned(),
                    module_graph.uses.get(path).cloned(),
                ));
            }
        }
        candidates.sort_by(|left, right| left.0.cmp(&right.0));
        candidates.dedup_by(|left, right| left.0 == right.0);
        candidates.truncate(MAX_FILES);
        let reusable = &file_cache;
        let indexed = scheduler.map_ordered(
            candidates,
            |(path, source_root, crate_name, cfg, module, graph_uses)| {
                let stamp = std::fs::metadata(&path)
                    .ok()
                    .and_then(|metadata| FileStamp::of(&metadata));
                let context = DeclarationContext {
                    source_root: source_root.clone(),
                    crate_name: crate_name.clone(),
                    features: sorted_features(&cfg),
                    module: module.clone(),
                };
                let cached = stamp.and_then(|stamp| reusable.fresh(&path, stamp));
                let cached_declarations = cached
                    .and_then(|file| file.declarations.as_ref())
                    .filter(|cached| cached.context == context)
                    .map(|cached| cached.items.clone());
                let parse_uses = graph_uses.is_none();
                let cached_uses = cached.and_then(|file| file.uses.clone());
                let text = if cached_declarations.is_none() || (parse_uses && cached_uses.is_none())
                {
                    std::fs::read_to_string(&path).ok()?
                } else {
                    String::new()
                };
                let parsed_declarations = cached_declarations.unwrap_or_else(|| {
                    parse_declarations(
                        &path,
                        &text,
                        &source_root,
                        &crate_name,
                        &cfg,
                        module.as_deref(),
                    )
                });
                let file_uses = graph_uses
                    .unwrap_or_else(|| cached_uses.unwrap_or_else(|| parse_use_bindings(&text)));
                Some((
                    path,
                    parsed_declarations,
                    file_uses,
                    stamp.map(|stamp| (stamp, context, parse_uses)),
                ))
            },
        );
        for (path, parsed_declarations, parsed_uses, record) in indexed.into_iter().flatten() {
            if let Some((stamp, context, parse_uses)) = record {
                let file = file_cache.entry(&path, stamp);
                file.declarations = Some(CachedDeclarations {
                    context,
                    items: parsed_declarations.clone(),
                });
                if parse_uses {
                    file.uses = Some(parsed_uses.clone());
                }
            }
            files.push(path.clone());
            declarations.extend(parsed_declarations);
            uses.insert(path, parsed_uses);
        }
        cache.files = file_cache.next;
        declarations.sort_by(|left, right| {
            (&left.name, &left.path, left.line).cmp(&(&right.name, &right.path, right.line))
        });
        Built {
            declarations,
            files,
            uses,
            module_identities: module_graph.identities,
            source_roots: roots,
            packages,
        }
    }

    pub fn install(&mut self, built: Built) {
        self.declarations = built.declarations;
        self.files = built.files;
        self.uses = built.uses;
        self.module_identities = built.module_identities;
        self.source_roots = built.source_roots;
        self.packages = built.packages;
        self.ready = true;
        self.clear_canonical_paths();
    }

    pub fn from_built(built: Built) -> Self {
        let mut engine = Self::pending(Path::new(""));
        engine.install(built);
        engine
    }

    pub fn into_built(self) -> Built {
        Built {
            declarations: self.declarations,
            files: self.files,
            uses: self.uses,
            module_identities: self.module_identities,
            source_roots: self.source_roots,
            packages: self.packages,
        }
    }

    /// Hash of everything besides a file's own text that decides its
    /// diagnostics: declarations (without positions), imports, module layout
    /// and Cargo metadata. Callers reuse cached diagnostics of unchanged files
    /// while this stays the same.
    pub fn diagnostics_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let state = &mut hasher;
        self.declarations.len().hash(state);
        for item in &self.declarations {
            item.name.hash(state);
            item.crate_name.hash(state);
            item.module.hash(state);
            match &item.visibility {
                Visibility::Private => 0_u8.hash(state),
                Visibility::Public => 1_u8.hash(state),
                Visibility::Restricted(module) => {
                    2_u8.hash(state);
                    module.hash(state);
                }
            }
            (item.kind as u8).hash(state);
            item.path.hash(state);
            item.scope_depth.hash(state);
        }
        self.files.hash(state);
        let mut uses = self.uses.iter().collect::<Vec<_>>();
        uses.sort_by(|left, right| left.0.cmp(right.0));
        for (path, bindings) in uses {
            path.hash(state);
            bindings.len().hash(state);
            for binding in bindings {
                binding.local.hash(state);
                binding.target.hash(state);
                binding.glob.hash(state);
                binding.public.hash(state);
                binding.module.hash(state);
            }
        }
        let mut identities = self.module_identities.iter().collect::<Vec<_>>();
        identities.sort();
        identities.hash(state);
        self.source_roots.hash(state);
        fn sorted(set: &HashSet<String>) -> Vec<&String> {
            let mut values = set.iter().collect::<Vec<_>>();
            values.sort();
            values
        }
        self.packages.len().hash(state);
        for package in &self.packages {
            package.name.hash(state);
            package.root.hash(state);
            package.indexed.hash(state);
            sorted(&package.selected_features).hash(state);
            package.targets.len().hash(state);
            for target in &package.targets {
                target.name.hash(state);
                target.source_root.hash(state);
                target.root.hash(state);
            }
            package.dependencies.len().hash(state);
            for dependency in &package.dependencies {
                dependency.name.hash(state);
                dependency.package.hash(state);
                dependency.path.hash(state);
                dependency.git.hash(state);
                dependency.optional.hash(state);
                dependency.default_features.hash(state);
                sorted(&dependency.features).hash(state);
            }
        }
        hasher.finish()
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Applies source-file watcher changes without rediscovering Cargo roots.
    /// Cargo metadata changes can alter every module and require `build`.
    pub fn apply_disk_changes(&mut self, paths: &[PathBuf]) -> bool {
        if !self.ready
            || paths.iter().any(|path| {
                matches!(
                    path.file_name().and_then(|name| name.to_str()),
                    Some("Cargo.toml" | "Cargo.lock")
                )
            })
        {
            return false;
        }
        self.clear_canonical_paths();
        for path in paths {
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                continue;
            }
            self.open.remove(path);
            self.declarations.retain(|item| item.path != *path);
            self.uses.remove(path);
            self.files.retain(|file| file != path);
            if let Ok(text) = std::fs::read_to_string(path)
                && text.len() as u64 <= MAX_FILE_BYTES
            {
                let source_root = self.source_root_for(path);
                let cfg = self.cfg_for(path);
                self.declarations.extend(parse_declarations(
                    path,
                    &text,
                    &source_root,
                    &self.crate_for(path),
                    &cfg,
                    self.module_identities.get(path).map(Vec::as_slice),
                ));
                self.uses.insert(path.clone(), parse_use_bindings(&text));
                self.files.push(path.clone());
            }
        }
        self.declarations.sort_by(|left, right| {
            (&left.name, &left.path, left.line).cmp(&(&right.name, &right.path, right.line))
        });
        true
    }

    pub fn sync_document(&mut self, path: &Path, text: &str) {
        self.open.insert(
            path.to_path_buf(),
            (
                parse_declarations(
                    path,
                    text,
                    &self.source_root_for(path),
                    &self.crate_for(path),
                    &self.cfg_for(path),
                    self.module_identities.get(path).map(Vec::as_slice),
                ),
                parse_use_bindings(text),
                text.to_owned(),
            ),
        );
    }

    pub fn close_document(&mut self, path: &Path) {
        self.open.remove(path);
    }

    pub fn complete(&mut self, path: &Path, text: &str, line: u32, column: u32) -> Vec<Completion> {
        self.sync_document(path, text);
        let prefix = word_before(text, line, column);
        let calls = adds_call_parentheses(text, line, column);
        if member_access_before(text, line, column) {
            return self
                .member_completions(path, text, line, column)
                .into_iter()
                .filter(|item| item.label.starts_with(prefix))
                .take(MAX_ITEMS)
                .map(|item| if calls { with_call(item) } else { item })
                .collect();
        }
        let mut candidates = self.visible_declarations(path, prefix);
        candidates.extend(local_declarations(
            path,
            text,
            cursor_byte(text, line, column),
        ));
        candidates.retain(|item| item.name.starts_with(prefix));
        candidates
            .sort_by(|left, right| left.name.cmp(&right.name).then(left.path.cmp(&right.path)));
        candidates.dedup_by(|left, right| left.name == right.name && left.kind == right.kind);
        let mut completions: Vec<_> = candidates
            .into_iter()
            .map(|item| self.declaration_completion(path, text, item))
            .collect();
        completions.extend(
            STD_TYPES
                .iter()
                .filter(|name| name.starts_with(prefix))
                .map(|name| std_type_completion(name, text)),
        );
        completions.sort_by(|left, right| left.label.cmp(&right.label));
        completions.dedup_by(|left, right| left.label == right.label && left.kind == right.kind);
        completions
            .into_iter()
            .take(MAX_ITEMS)
            .map(|item| if calls { with_call(item) } else { item })
            .collect()
    }

    pub fn signature(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<Signature> {
        self.sync_document(path, text);
        let byte = cursor_byte(text, line, column);
        let (name, active) = call_at(text, byte)?;
        let declaration = self
            .resolve(path, &name, byte)
            .into_iter()
            .find(|item| item.kind == ItemKind::Function)?;
        let parameters = signature_parameters(&declaration.signature);
        Some(Signature {
            label: declaration.signature,
            documentation: None,
            parameters,
            active,
        })
    }

    /// Parameter-name hints for the calls on lines `first..=last`. Free and
    /// associated functions only: method calls would need the receiver's
    /// type, which the engine looks up by reading the project.
    pub fn parameter_hints(
        &mut self,
        path: &Path,
        text: &str,
        first: u32,
        last: u32,
    ) -> Vec<ParameterHint> {
        self.sync_document(path, text);
        let Some(tree) = parse_tree(text) else {
            return Vec::new();
        };
        let mut sources: HashMap<PathBuf, Option<String>> = HashMap::new();
        let mut out = Vec::new();
        for (name, associated, arguments) in rust_call_sites(text, tree.root_node(), first, last) {
            let declarations: Vec<_> = self
                .resolve_nonlocal(path, &name)
                .into_iter()
                .filter(|item| item.kind == ItemKind::Function)
                .collect();
            let [declaration] = declarations.as_slice() else {
                continue;
            };
            let Some(source) = self.declaration_source(&mut sources, path, text, declaration)
            else {
                continue;
            };
            if let Some(parameters) =
                rust_parameters_at(source, declaration.line, declaration.column, associated)
            {
                out.extend(parameter_hints::hints(&parameters, &arguments));
            }
        }
        self.type_hints(
            path,
            text,
            tree.root_node(),
            (first, last),
            &mut sources,
            &mut out,
        );
        out.sort_by_key(|hint| (hint.line, hint.column));
        out
    }

    /// Source text of the file that holds `declaration`: the open buffer, the
    /// text being analyzed, or the file on disk. Cached in `sources`.
    fn declaration_source<'a>(
        &self,
        sources: &'a mut HashMap<PathBuf, Option<String>>,
        path: &Path,
        text: &str,
        declaration: &Declaration,
    ) -> Option<&'a str> {
        sources
            .entry(declaration.path.clone())
            .or_insert_with(|| {
                if declaration.path == path {
                    Some(text.to_owned())
                } else {
                    self.document(&declaration.path)
                        .map(|(_, text)| text.to_owned())
                        .or_else(|| std::fs::read_to_string(&declaration.path).ok())
                }
            })
            .as_deref()
    }

    /// `: type` labels after the name in `let` statements without a type, on
    /// lines `rows.0..=rows.1`. Only types that can be named with certainty
    /// get a label: literals, struct literals, and calls of one resolved
    /// function with a written return type.
    fn type_hints(
        &self,
        path: &Path,
        text: &str,
        root: Node<'_>,
        rows: (u32, u32),
        sources: &mut HashMap<PathBuf, Option<String>>,
        out: &mut Vec<ParameterHint>,
    ) {
        let (first, last) = (rows.0 as usize, rows.1 as usize);
        let mut found = 0;
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.end_position().row < first || node.start_position().row > last {
                continue;
            }
            if node.kind() == "let_declaration"
                && found < parameter_hints::MAX_CALLS
                && let Some(hint) = self.let_type_hint(path, text, node, sources)
            {
                found += 1;
                out.push(hint);
            }
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
        }
    }

    fn let_type_hint(
        &self,
        path: &Path,
        text: &str,
        declaration: Node<'_>,
        sources: &mut HashMap<PathBuf, Option<String>>,
    ) -> Option<ParameterHint> {
        if declaration.child_by_field_name("type").is_some() {
            return None;
        }
        let mut pattern = declaration.child_by_field_name("pattern")?;
        if pattern.kind() == "mut_pattern" {
            let mut cursor = pattern.walk();
            pattern = pattern.named_children(&mut cursor).last()?;
        }
        if pattern.kind() != "identifier" {
            return None;
        }
        let name = pattern.utf8_text(text.as_bytes()).ok()?;
        let value = declaration.child_by_field_name("value")?;
        let label = match value.kind() {
            "string_literal" | "raw_string_literal" => "&str".to_owned(),
            "boolean_literal" => "bool".to_owned(),
            "char_literal" => "char".to_owned(),
            "integer_literal" | "float_literal" | "unary_expression" | "negative_literal" => {
                numeric_literal_type(text, declaration, value, name)?
            }
            "struct_expression" => {
                let type_name = value.child_by_field_name("name")?;
                if type_name.kind() != "type_identifier" {
                    return None;
                }
                let type_name = type_name.utf8_text(text.as_bytes()).ok()?;
                if type_name == "Self" {
                    return None;
                }
                type_name.to_owned()
            }
            "call_expression" => self.call_result_type(path, text, value, sources)?,
            "macro_invocation" => vec_literal_type(text, declaration, value, name)?,
            _ => return None,
        };
        Some(ParameterHint {
            line: pattern.end_position().row as u32,
            column: utf16_column(text, pattern.end_byte()),
            label: format!(": {label}"),
            return_type: false,
        })
    }

    /// The written return type of the single function that `call` names.
    fn call_result_type(
        &self,
        path: &Path,
        text: &str,
        call: Node<'_>,
        sources: &mut HashMap<PathBuf, Option<String>>,
    ) -> Option<String> {
        let function = call.child_by_field_name("function")?;
        let (name, owner) = match function.kind() {
            "identifier" => (function, None),
            "scoped_identifier" => {
                let owner = function
                    .child_by_field_name("path")?
                    .utf8_text(text.as_bytes())
                    .ok()?
                    .rsplit("::")
                    .next()?;
                (function.child_by_field_name("name")?, Some(owner))
            }
            _ => return None,
        };
        let name = name.utf8_text(text.as_bytes()).ok()?;
        if name.starts_with(|c: char| c.is_uppercase()) {
            return None;
        }
        if let Some(owner) = owner.filter(|owner| STD_TYPES.contains(owner)) {
            let declarations = self.resolve_std_member(owner, name);
            let [declaration] = declarations.as_slice() else {
                return None;
            };
            let source = self.declaration_source(sources, path, text, declaration)?;
            return rust_return_type_at(source, declaration.line, declaration.column, Some(owner));
        }
        let declarations: Vec<_> = self
            .resolve_nonlocal(path, name)
            .into_iter()
            .filter(|item| item.kind == ItemKind::Function)
            .collect();
        let [declaration] = declarations.as_slice() else {
            return None;
        };
        let source = self.declaration_source(sources, path, text, declaration)?;
        rust_return_type_at(source, declaration.line, declaration.column, owner)
    }

    pub fn definition(&mut self, path: &Path, text: &str, line: u32, column: u32) -> Vec<Target> {
        self.sync_document(path, text);
        let Some(name) = identifier_at(text, line, column) else {
            return Vec::new();
        };
        let byte = cursor_byte(text, line, column);
        if STD_TYPES.contains(&name.as_str())
            && let Some(path) = std_type_source(&name)
            && let Some(declaration) = std_type_declaration(&path, &name)
        {
            return vec![Target {
                path: declaration.path,
                line: declaration.line,
                column: declaration.column,
            }];
        }
        if let Some((ty, _)) = std_member_access(text, byte, &name) {
            let declarations = self.resolve_std_member(&ty, &name);
            if !declarations.is_empty() {
                return declarations
                    .into_iter()
                    .map(|item| Target {
                        path: item.path,
                        line: item.line,
                        column: item.column,
                    })
                    .take(MAX_ITEMS)
                    .collect();
            }
        }
        self.resolve(path, &name, cursor_byte(text, line, column))
            .into_iter()
            .map(|item| Target {
                path: item.path,
                line: item.line,
                column: item.column,
            })
            .take(MAX_ITEMS)
            .collect()
    }

    pub fn hover(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<HoverResult> {
        self.sync_document(path, text);
        let (name, start, end) = identifier_range(text, line, column)?;
        let byte = cursor_byte(text, line, column);
        let declaration = if STD_TYPES.contains(&name.as_str()) {
            std_type_source(&name).and_then(|path| std_type_declaration(&path, &name))
        } else if let Some((ty, _)) = std_member_access(text, byte, &name) {
            self.resolve_std_member(&ty, &name).into_iter().next()
        } else {
            self.resolve(path, &name, byte).into_iter().next()
        }?;
        Some(HoverResult {
            markdown: format!("```rust\n{}\n```", declaration.signature),
            line,
            start_column: start,
            end_column: end,
        })
    }

    pub fn diagnostics(&mut self, path: &Path, text: &str) -> Vec<Problem> {
        self.sync_document(path, text);
        self.diagnostics_indexed(path, text)
    }

    /// Diagnoses source already represented by the static workspace index.
    /// Unlike open-document diagnostics, this does not duplicate declarations,
    /// imports, and source text in the mutable document cache.
    pub fn diagnostics_indexed(&self, path: &Path, text: &str) -> Vec<Problem> {
        let Some(tree) = parse_tree(text) else {
            return Vec::new();
        };
        let mut errors = Vec::new();
        collect_errors(tree.root_node(), text, &mut errors);
        errors.extend(module_problems(path, text, &self.cfg_for(path)));
        let local_bindings = all_local_binding_names(tree.root_node(), text);
        // The static index skips `#[cfg(test)]` modules, so imports declared
        // there are only visible by reading this file's own text.
        let file_uses = parse_use_bindings(text);
        collect_unresolved(
            self,
            path,
            text,
            tree.root_node(),
            &local_bindings,
            &file_uses,
            &mut errors,
        );
        errors.sort_by_key(|problem| (problem.start_line, problem.start_column));
        errors.truncate(MAX_PROBLEMS);
        errors
    }

    pub fn actions(&mut self, path: &Path, text: &str, line: u32, _column: u32) -> Vec<Action> {
        self.sync_document(path, text);
        let Some(source) = text.lines().nth(line as usize) else {
            return Vec::new();
        };
        let Some(binding) = parse_use_bindings(source)
            .into_iter()
            .find(|binding| !binding.glob)
        else {
            return Vec::new();
        };
        if identifier_count(text, &binding.local) != 1 {
            return Vec::new();
        }
        vec![Action {
            title: format!("Remove unused import `{}`", binding.local),
            edits: vec![Edit {
                start_line: line,
                start_column: 0,
                end_line: line + 1,
                end_column: 0,
                text: String::new(),
            }],
            files: Vec::new(),
        }]
    }

    /// Conservative native formatting for valid Rust syntax. It changes only
    /// indentation and trailing whitespace, preserving token spacing.
    pub fn format(&self, text: &str) -> Option<String> {
        let tree = parse_tree(text)?;
        if tree.root_node().has_error() {
            return None;
        }
        let mut depth = 0usize;
        let mut out = String::new();
        for line in text.lines() {
            let content = line.trim();
            if content.starts_with('}') {
                depth = depth.saturating_sub(1);
            }
            if !content.is_empty() {
                out.push_str(&"    ".repeat(depth));
                out.push_str(content);
            }
            out.push('\n');
            let (opens, closes) = structural_braces(content);
            depth += opens;
            depth = depth.saturating_sub(closes);
        }
        Some(out)
    }

    /// Finds identifier occurrences when the name has exactly one top-level
    /// declaration in this project. Ambiguous names require richer resolution.
    pub fn references(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Vec<Occurrence> {
        self.sync_document(path, text);
        let Some(name) = identifier_at(text, line, column) else {
            return Vec::new();
        };
        let declarations = self.resolve(path, &name, cursor_byte(text, line, column));
        if declarations.len() != 1 {
            return Vec::new();
        }
        if declarations[0].kind == ItemKind::Variable {
            return local_occurrences(path, text, &name, &declarations[0]);
        }
        let target = &declarations[0];
        self.sources()
            .into_iter()
            .flat_map(|(candidate_path, source)| {
                item_occurrences(self, &candidate_path, &source, &name, target)
            })
            .take(MAX_OCCURRENCES)
            .collect()
    }

    /// Validates that a local rename is safe enough for the current static
    /// resolver, then returns all edits as UTF-16 occurrences.
    pub fn rename(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
        new_name: &str,
    ) -> Result<Vec<Occurrence>, String> {
        if !valid_name(new_name) {
            return Err("invalid Rust identifier".to_owned());
        }
        let found = self.references(path, text, line, column);
        if found.is_empty() {
            return Err("symbol is ambiguous or unknown".to_owned());
        }
        Ok(found)
    }

    pub fn outline(&self, path: &Path, text: &str) -> Vec<OutlineSymbol> {
        parse_declarations(
            path,
            text,
            &self.source_root_for(path),
            &self.crate_for(path),
            &self.cfg_for(path),
            self.module_identities.get(path).map(Vec::as_slice),
        )
        .into_iter()
        .map(|item| OutlineSymbol {
            name: item.name,
            kind: item_kind_name(item.kind),
            line: item.line,
            column: item.column,
            children: Vec::new(),
        })
        .collect()
    }

    fn all_declarations(&self) -> impl Iterator<Item = &Declaration> {
        self.declarations
            .iter()
            .filter(|item| !self.open.contains_key(&item.path))
            .chain(
                self.open
                    .values()
                    .flat_map(|(declarations, _, _)| declarations),
            )
    }

    fn declarations_named(&self, name: &str) -> Vec<Declaration> {
        let start = self
            .declarations
            .partition_point(|declaration| declaration.name.as_str() < name);
        let end = self
            .declarations
            .partition_point(|declaration| declaration.name.as_str() <= name);
        let mut matches: Vec<_> = self.declarations[start..end]
            .iter()
            .filter(|declaration| !self.open.contains_key(&declaration.path))
            .cloned()
            .collect();
        matches.extend(
            self.open
                .values()
                .flat_map(|(declarations, _, _)| declarations)
                .filter(|declaration| declaration.name == name)
                .cloned(),
        );
        matches
    }

    fn visible_declarations(&self, path: &Path, prefix: &str) -> Vec<Declaration> {
        let accessible = self.accessibility_filter(path);
        let mut visible: Vec<_> = self
            .all_declarations()
            .filter(|item| item.name.starts_with(prefix) && accessible(item))
            .cloned()
            .collect();
        let aliases = self.use_bindings(path);
        for binding in aliases {
            if !binding.glob && !binding.local.starts_with(prefix) {
                continue;
            }
            let targets = if binding.glob {
                self.declarations_in_module(path, &binding.target)
            } else {
                self.resolve_path(path, &binding.target)
            };
            for target in targets {
                if binding.glob && !target.name.starts_with(prefix) {
                    continue;
                }
                let mut alias = target.clone();
                if !binding.glob {
                    alias.name = binding.local.clone();
                }
                visible.push(alias);
            }
        }
        visible
    }

    fn member_completions(
        &self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Vec<Completion> {
        let byte = cursor_byte(text, line, column);
        let before = &text[..byte];
        let Some(receiver) = before
            .rsplit_once("::")
            .or_else(|| before.rsplit_once('.'))
            .map(|(left, _)| left.trim_end())
            .and_then(|left| left.rsplit(|character: char| !is_ident(character)).next())
        else {
            return Vec::new();
        };
        let Some(ty) = self
            .receiver_type(path, text, before, receiver)
            .or_else(|| STD_TYPES.contains(&receiver).then(|| receiver.to_owned()))
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if STD_TYPES.contains(&ty.as_str()) {
            out.extend(std_member_completions(&ty));
        }
        for (_, source) in self.sources() {
            out.extend(struct_fields(&source, &ty));
            out.extend(impl_methods(&source, &ty));
        }
        // Several `From` impls share the label `from`. Keep the one taking
        // `&str`, so `String::from` completes to `from("")`.
        out.sort_by_key(|item| (item.label.clone(), !takes_str(item)));
        out.dedup_by(|left, right| left.label == right.label);
        out
    }

    fn resolve(&self, path: &Path, name: &str, at: usize) -> Vec<Declaration> {
        if let Some((_, text)) = self.document(path)
            && let Some(declarations) = self.resolve_scoped_path(path, text, at)
            && !declarations.is_empty()
        {
            return declarations;
        }
        if let Some((_, text)) = self.document(path)
            && let Some(variants) = self.resolve_enum_variant(path, text, name, at)
            && !variants.is_empty()
        {
            return variants;
        }
        if let Some((_, text)) = self.document(path)
            && let Some(members) = self.resolve_member(path, text, name, at)
        {
            return members;
        }
        if let Some((_, text)) = self.document(path)
            && parse_tree(text).is_some_and(|tree| member_access_at(tree.root_node(), at).is_some())
            && let Some(declaration) = std_prelude_trait_method(name)
        {
            return vec![declaration];
        }
        if let Some((_, text)) = self.document(path)
            && let Some(local) = local_declaration(path, text, name, at)
        {
            return vec![local];
        }
        self.resolve_nonlocal(path, name)
    }

    fn resolve_scoped_path(&self, path: &Path, text: &str, at: usize) -> Option<Vec<Declaration>> {
        let tree = parse_tree(text)?;
        let Some(scoped) = scoped_path_at(tree.root_node(), at) else {
            return self.resolve_macro_path(path, text, at);
        };
        let scoped_text = scoped.utf8_text(text.as_bytes()).ok()?;
        let relative = at.saturating_sub(scoped.start_byte());
        let mut start = 0;
        let mut target = Vec::new();
        for segment in scoped_text.split("::") {
            let end = start + segment.len();
            target.push(segment.to_owned());
            if start <= relative && relative <= end {
                if target.len() == 1 {
                    if let Some(declaration) = self.dependency_root(path, segment) {
                        return Some(vec![declaration]);
                    }
                    return Some(self.resolve_nonlocal(path, segment));
                }
                let declarations = self.resolve_path(path, &target);
                if !declarations.is_empty() {
                    return Some(declarations);
                }
                return Some(self.resolve_associated_item(path, &target));
            }
            start = end + 2;
        }
        None
    }

    /// tree-sitter leaves the body of `vec![..]`, `json!{..}` and other macro
    /// calls as a flat token tree, so `Table::create` there has no
    /// `scoped_identifier` node. Read the path from the text instead.
    fn resolve_macro_path(&self, path: &Path, text: &str, at: usize) -> Option<Vec<Declaration>> {
        let bytes = text.as_bytes();
        let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
        let mut end = at.min(bytes.len());
        while end < bytes.len() && is_word(bytes[end]) {
            end += 1;
        }
        let mut start = at.min(end);
        while start > 0 && is_word(bytes[start - 1]) {
            start -= 1;
        }
        if start == end {
            return None;
        }
        let mut target = vec![text[start..end].to_owned()];
        while start >= 2 && &bytes[start - 2..start] == b"::" {
            let segment_end = start - 2;
            let mut segment_start = segment_end;
            while segment_start > 0 && is_word(bytes[segment_start - 1]) {
                segment_start -= 1;
            }
            if segment_start == segment_end {
                break;
            }
            target.insert(0, text[segment_start..segment_end].to_owned());
            start = segment_start;
        }
        if target.len() < 2 {
            return None;
        }
        let declarations = self.resolve_path(path, &target);
        if !declarations.is_empty() {
            return Some(declarations);
        }
        let associated = self.resolve_associated_item(path, &target);
        (!associated.is_empty()).then_some(associated)
    }

    /// `Type::item` and `module::Type::item`: resolve the owner type, then
    /// look for the item in that type's `impl` blocks.
    fn resolve_associated_item(&self, path: &Path, target: &[String]) -> Vec<Declaration> {
        let Some((name, owner)) = target.split_last() else {
            return Vec::new();
        };
        let owners = match owner {
            [] => return Vec::new(),
            [single] => self.resolve_nonlocal(path, single),
            _ => self.resolve_path(path, owner),
        };
        let mut matches = Vec::new();
        for declaration in owners {
            let source = self
                .open
                .get(&declaration.path)
                .map(|(_, _, text)| text.clone())
                .or_else(|| std::fs::read_to_string(&declaration.path).ok());
            if let Some(source) = source {
                matches.extend(member_declarations(
                    &declaration.path,
                    &source,
                    &declaration.name,
                    name,
                ));
            }
        }
        matches.sort_by(|left, right| {
            (&left.path, left.line, left.column).cmp(&(&right.path, right.line, right.column))
        });
        matches.dedup_by(|left, right| {
            left.path == right.path && left.line == right.line && left.column == right.column
        });
        matches
    }

    /// `pub use framework_database as database;` makes `framework::database`
    /// a path into another crate. Switch to that crate when a module segment
    /// is such a re-exported dependency.
    fn follow_crate_reexports(
        &self,
        mut crate_name: String,
        mut module: Vec<String>,
    ) -> (String, Vec<String>) {
        'outer: for _ in 0..8 {
            for depth in 0..module.len() {
                if let Some((package, inner)) =
                    self.reexported_crate(&crate_name, &module[..depth], &module[depth])
                {
                    crate_name = package;
                    module.splice(..=depth, inner);
                    continue 'outer;
                }
            }
            break;
        }
        (crate_name, module)
    }

    fn reexported_crate(
        &self,
        crate_name: &str,
        parent: &[String],
        segment: &str,
    ) -> Option<(String, Vec<String>)> {
        for (binding_path, bindings) in &self.uses {
            if self.crate_for(binding_path) != crate_name {
                continue;
            }
            for binding in bindings.iter().filter(|binding| {
                binding.public
                    && !binding.glob
                    && binding.local == segment
                    && if binding.module.is_empty() {
                        self.module_for(binding_path) == parent
                    } else {
                        binding.module == parent
                    }
            }) {
                // `pub use framework_http as http;` names a crate, and
                // `pub use axum::routing;` a module inside one.
                let Some(dependency) = self.dependency_for(binding_path, &binding.target[0]) else {
                    continue;
                };
                let inner = binding.target[1..].to_vec();
                let is_module = inner.is_empty()
                    || self.all_declarations().any(|item| {
                        item.crate_name == dependency.package && item.module.starts_with(&inner)
                    });
                if is_module {
                    return Some((dependency.package, inner));
                }
            }
        }
        None
    }

    fn dependency_root(&self, path: &Path, name: &str) -> Option<Declaration> {
        let dependency = self.dependency_for(path, name)?;
        let package = self
            .packages
            .iter()
            .find(|package| package.name == dependency.package)?;
        let target = package
            .targets
            .iter()
            .find(|target| target.root.file_name().is_some_and(|file| file == "lib.rs"))?;
        Some(Declaration {
            name: name.to_owned(),
            crate_name: package.name.clone(),
            module: Vec::new(),
            visibility: Visibility::Public,
            kind: ItemKind::Class,
            path: target.root.clone(),
            line: 0,
            column: 0,
            scope_depth: 0,
            signature: format!("crate {name}"),
        })
    }

    fn resolve_enum_variant(
        &self,
        path: &Path,
        text: &str,
        name: &str,
        at: usize,
    ) -> Option<Vec<Declaration>> {
        let tree = parse_tree(text)?;
        let scoped = scoped_identifier_at(tree.root_node(), at)?;
        let owner = scoped.child_by_field_name("path")?;
        let owner_name = owner.utf8_text(text.as_bytes()).ok()?;
        if !is_ident_text(owner_name) {
            return None;
        }
        let enums: Vec<_> = self
            .resolve(path, owner_name, owner.start_byte())
            .into_iter()
            .filter(|declaration| declaration.kind == ItemKind::Enum)
            .collect();
        let mut variants = Vec::new();
        for declaration in enums {
            let source = self
                .open
                .get(&declaration.path)
                .map(|(_, _, text)| text.clone())
                .or_else(|| std::fs::read_to_string(&declaration.path).ok());
            if let Some(source) = source {
                variants.extend(enum_variant_declarations(&declaration, &source, name));
            }
        }
        variants.sort_by(|left, right| {
            (&left.path, left.line, left.column).cmp(&(&right.path, right.line, right.column))
        });
        variants.dedup_by(|left, right| {
            left.path == right.path && left.line == right.line && left.column == right.column
        });
        Some(variants)
    }

    fn resolve_std_member(&self, ty: &str, name: &str) -> Vec<Declaration> {
        let Some(path) = std_member_source(ty, name) else {
            return Vec::new();
        };
        let Ok(source) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        member_declarations(&path, &source, ty, name)
    }

    /// Resolve `value.member` after the lightweight local type inference used
    /// by member completion. Do not fall back to a name-only match: a method
    /// name is frequently shared by unrelated types.
    fn resolve_member(
        &self,
        path: &Path,
        text: &str,
        name: &str,
        at: usize,
    ) -> Option<Vec<Declaration>> {
        let tree = parse_tree(text)?;
        if let Some(value) = member_access_at(tree.root_node(), at)
            .and_then(|member| member.child_by_field_name("value"))
            && value.kind() == "call_expression"
        {
            let mut sources = HashMap::new();
            let ty = self.expression_type(path, text, value, &mut sources, 0)?;
            let matches = self.member_matches(&ty, name);
            return (matches.len() == 1).then_some(matches);
        }
        let ast_receiver = member_access_at(tree.root_node(), at).and_then(|member| {
            member.child_by_field_name("value").and_then(|receiver| {
                receiver
                    .utf8_text(text.as_bytes())
                    .ok()
                    .map(|name| (name, member.start_byte()))
            })
        });
        let (receiver, before) = if let Some((receiver, start)) = ast_receiver {
            (receiver, &text[..start])
        } else {
            let before = &text[..at];
            let (left, _) = before.rsplit_once('.')?;
            let receiver = left
                .trim_end()
                .rsplit(|character: char| !is_ident(character))
                .next()?;
            (receiver, left)
        };
        if !is_ident_text(receiver) {
            return None;
        }
        let ty = self.receiver_type(path, text, before, receiver)?;
        let matches = self.member_matches(&ty, name);
        (matches.len() == 1).then_some(matches)
    }

    fn member_matches(&self, ty: &str, name: &str) -> Vec<Declaration> {
        let mut paths: Vec<_> = self
            .declarations_named(name)
            .into_iter()
            .chain(self.declarations_named(ty))
            .map(|declaration| declaration.path)
            .collect();
        paths.sort();
        paths.dedup();
        let mut matches = Vec::new();
        for candidate in paths {
            let stamp = match self.open.get(&candidate) {
                Some((_, _, text)) => Some(text_stamp(text)),
                None => disk_stamp(&candidate),
            };
            let key = (candidate.clone(), ty.to_owned(), name.to_owned());
            if let Some(stamp) = &stamp
                && let Some((cached, found)) = self
                    .member_cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&key)
                && cached == stamp
            {
                matches.extend(found.iter().cloned());
                continue;
            }
            let source = self
                .open
                .get(&candidate)
                .map(|(_, _, text)| text.clone())
                .or_else(|| std::fs::read_to_string(&candidate).ok());
            if let Some(source) = source {
                let found = if source.contains(ty) {
                    member_declarations(&candidate, &source, ty, name)
                } else {
                    Vec::new()
                };
                if let Some(stamp) = stamp {
                    let mut cache = self.member_cache.lock().unwrap_or_else(|e| e.into_inner());
                    if cache.len() >= MEMBER_CACHE_LIMIT {
                        cache.clear();
                    }
                    cache.insert(key, (stamp, found.clone()));
                }
                matches.extend(found);
            }
        }
        matches.sort_by(|left, right| {
            (&left.path, left.line, left.column).cmp(&(&right.path, right.line, right.column))
        });
        matches.dedup_by(|left, right| {
            left.path == right.path && left.line == right.line && left.column == right.column
        });
        matches
    }

    /// Type of a call chain such as `Type::new().name("x")`, from the written
    /// return type of each single matching associated function or method.
    fn expression_type(
        &self,
        path: &Path,
        text: &str,
        expression: Node<'_>,
        sources: &mut HashMap<PathBuf, Option<String>>,
        depth: usize,
    ) -> Option<String> {
        if depth > 16 {
            return None;
        }
        if expression.kind() == "identifier" {
            let name = expression.utf8_text(text.as_bytes()).ok()?;
            let before = text.get(..expression.start_byte())?;
            return self.receiver_type(path, text, before, name);
        }
        if expression.kind() != "call_expression" {
            return None;
        }
        let function = expression.child_by_field_name("function")?;
        if function.kind() == "identifier" {
            let name = function.utf8_text(text.as_bytes()).ok()?;
            let declarations: Vec<_> = self
                .resolve_nonlocal(path, name)
                .into_iter()
                .filter(|item| item.kind == ItemKind::Function)
                .collect();
            let [declaration] = declarations.as_slice() else {
                return None;
            };
            let source = self.declaration_source(sources, path, text, declaration)?;
            return named_return_type(source, declaration, None);
        }
        let (owner, name) = match function.kind() {
            "scoped_identifier" => {
                let owner = function
                    .child_by_field_name("path")?
                    .utf8_text(text.as_bytes())
                    .ok()?
                    .rsplit("::")
                    .next()?
                    .to_owned();
                let name = function
                    .child_by_field_name("name")?
                    .utf8_text(text.as_bytes())
                    .ok()?;
                (owner, name.to_owned())
            }
            "field_expression" => {
                let receiver = function.child_by_field_name("value")?;
                let owner = self.expression_type(path, text, receiver, sources, depth + 1)?;
                let name = function
                    .child_by_field_name("field")?
                    .utf8_text(text.as_bytes())
                    .ok()?;
                (owner, name.to_owned())
            }
            _ => return None,
        };
        let mut owner = owner;
        let mut declarations = self.member_matches(&owner, &name);
        if declarations.is_empty()
            && let Some(target) = self.alias_target(&owner)
        {
            declarations = self.member_matches(&target, &name);
            owner = target;
        }
        let [declaration] = declarations.as_slice() else {
            return None;
        };
        let source = self.declaration_source(sources, path, text, declaration)?;
        named_return_type(source, declaration, Some(&owner))
    }

    /// `Router` for `pub type AppRouter = Router<Container>;`, when exactly
    /// one type alias has that name.
    fn alias_target(&self, ty: &str) -> Option<String> {
        let aliases: Vec<_> = self
            .declarations_named(ty)
            .into_iter()
            .filter_map(|item| {
                let signature = item.signature.trim_start();
                let signature = signature
                    .strip_prefix("pub ")
                    .or_else(|| {
                        signature
                            .strip_prefix("pub(")
                            .and_then(|rest| rest.split_once(')'))
                            .map(|(_, rest)| rest)
                    })
                    .unwrap_or(signature)
                    .trim_start();
                let (_, target) = signature.strip_prefix("type ")?.split_once('=')?;
                named_type(target.trim().trim_end_matches(';'))
            })
            .collect();
        match aliases.as_slice() {
            [target] if target != ty => Some(target.clone()),
            _ => None,
        }
    }

    fn receiver_type(
        &self,
        path: &Path,
        text: &str,
        before: &str,
        receiver: &str,
    ) -> Option<String> {
        if let Some(tree) = parse_tree(text) {
            let root = tree.root_node();
            if let Some(call) = local_call_binding(root, text, receiver, before.len()) {
                let mut sources = HashMap::new();
                if let Some(ty) = self
                    .call_result_type(path, text, call, &mut sources)
                    .and_then(|ty| named_type(&ty))
                {
                    return Some(ty);
                }
                if let Some(ty) = self.expression_type(path, text, call, &mut sources, 0) {
                    return Some(ty);
                }
            }
            if let Some(ty) = for_binding_type(root, text, receiver, before.len()) {
                return Some(ty);
            }
        }
        receiver_type(before, receiver)
    }

    fn declaration_completion(&self, path: &Path, text: &str, item: Declaration) -> Completion {
        let mut result = completion(&item.name, &item.signature, item.kind);
        let current_crate = self.crate_for(path);
        let same_module = item.crate_name == current_crate && item.module == self.module_for(path);
        let imported = self
            .use_bindings(path)
            .iter()
            .any(|binding| binding.local == item.name || binding.glob);
        if item.path != path && !same_module && !imported && item.visibility == Visibility::Public {
            let root = if item.crate_name == current_crate {
                "crate".to_owned()
            } else {
                item.crate_name.replace('-', "_")
            };
            if !root.is_empty() {
                let mut segments = vec![root];
                segments.extend(item.module.clone());
                segments.push(item.name.clone());
                result.extra.push(Insertion {
                    line: import_insertion_line(text),
                    column: 0,
                    text: format!("use {};\n", segments.join("::")),
                });
            }
        }
        result
    }

    /// Resolves project and import declarations without reparsing local scopes.
    /// Workspace diagnostics call this after collecting every local binding from
    /// their already-parsed syntax tree.
    fn resolve_nonlocal(&self, path: &Path, name: &str) -> Vec<Declaration> {
        let matching_name = self.declarations_named(name);
        let local: Vec<_> = matching_name
            .iter()
            .filter(|item| item.path == path && item.name == name)
            .cloned()
            .collect();
        if !local.is_empty() {
            return local;
        }
        if let Some(binding) = self
            .use_bindings(path)
            .iter()
            .find(|binding| binding.local == name)
            && !binding.glob
            && let Some(target) = self.resolve_path(path, &binding.target).into_iter().next()
        {
            return vec![target];
        }
        let globbed: Vec<_> = self
            .use_bindings(path)
            .into_iter()
            .filter(|binding| binding.glob)
            .flat_map(|binding| {
                self.resolve_name_in_module(
                    path,
                    path,
                    &binding.target,
                    name,
                    &mut HashSet::new(),
                    0,
                )
            })
            .collect();
        if globbed.len() == 1 {
            return globbed;
        }
        let matching: Vec<_> = matching_name
            .into_iter()
            .filter(|item| self.accessible_from(path, item))
            .collect();
        if matching.len() == 1 {
            matching
        } else {
            Vec::new()
        }
    }

    fn resolve_name_in_module(
        &self,
        requester: &Path,
        context: &Path,
        target: &[String],
        name: &str,
        seen: &mut HashSet<(String, Vec<String>, String)>,
        depth: usize,
    ) -> Vec<Declaration> {
        if depth >= 32 {
            return Vec::new();
        }
        let current_crate = self.crate_for(context);
        let (crate_name, module) = match target.first().map(String::as_str) {
            Some("crate" | "self" | "super") => {
                (current_crate, self.module_target(context, target))
            }
            Some(first) => match self.dependency_for(context, first) {
                Some(dependency) => (dependency.package, target[1..].to_vec()),
                // Edition 2018 paths may start at a child module:
                // `use helpers::*;` inside `engine.rs`.
                None => {
                    let mut module = self.module_for(context);
                    module.extend_from_slice(target);
                    (current_crate, module)
                }
            },
            None => return Vec::new(),
        };
        let key = (crate_name.clone(), module.clone(), name.to_owned());
        if !seen.insert(key) {
            return Vec::new();
        }

        let mut resolved: Vec<_> = self
            .declarations_named(name)
            .into_iter()
            .filter(|item| {
                item.crate_name == crate_name
                    && item.module == module
                    && self.accessible_from(requester, item)
            })
            .collect();
        let requester_module = self.module_for(requester);
        for (binding_path, bindings) in &self.uses {
            if self.crate_for(binding_path) != crate_name {
                continue;
            }
            let binding_module = self.module_for(binding_path);
            for binding in bindings.iter().filter(|binding| {
                let declared_module = if binding.module.is_empty() {
                    &binding_module
                } else {
                    &binding.module
                };
                declared_module == &module
                    && (binding.public || module_is_within(&requester_module, &module))
                    && (binding.glob || binding.local == name)
            }) {
                let targets = if binding.glob {
                    self.resolve_name_in_module(
                        requester,
                        binding_path,
                        &binding.target,
                        name,
                        seen,
                        depth + 1,
                    )
                } else {
                    self.resolve_path_bounded(
                        binding_path,
                        &binding.target,
                        &mut HashSet::new(),
                        depth + 1,
                    )
                };
                resolved.extend(targets);
            }
        }
        resolved.sort_by(|left, right| {
            (&left.path, left.line, left.column).cmp(&(&right.path, right.line, right.column))
        });
        resolved.dedup_by(|left, right| {
            left.path == right.path && left.line == right.line && left.column == right.column
        });
        resolved
    }

    fn resolve_path(&self, path: &Path, target: &[String]) -> Vec<Declaration> {
        self.resolve_path_bounded(path, target, &mut HashSet::new(), 0)
    }

    /// Follow only explicit public re-export edges. This makes aliases usable
    /// without treating macro output or private imports as declarations.
    fn resolve_path_bounded(
        &self,
        path: &Path,
        target: &[String],
        seen: &mut HashSet<(String, Vec<String>, String)>,
        depth: usize,
    ) -> Vec<Declaration> {
        if depth >= 32 {
            return Vec::new();
        }
        let Some(name) = target.last() else {
            return Vec::new();
        };
        let current_crate = self.crate_for(path);
        let current = self.module_for(path);
        let (crate_name, module) = match target.first().map(String::as_str) {
            Some("crate") => (
                current_crate,
                target
                    .get(1..target.len().saturating_sub(1))
                    .unwrap_or_default()
                    .to_vec(),
            ),
            Some("self") => {
                let mut module = current;
                module.extend_from_slice(
                    target
                        .get(1..target.len().saturating_sub(1))
                        .unwrap_or_default(),
                );
                (current_crate, module)
            }
            Some("super") => {
                let mut module = current;
                let parents = target
                    .iter()
                    .take_while(|segment| segment.as_str() == "super")
                    .count();
                for _ in 0..parents {
                    module.pop();
                }
                module.extend_from_slice(
                    target
                        .get(parents..target.len().saturating_sub(1))
                        .unwrap_or_default(),
                );
                (current_crate, module)
            }
            _ if target.len() > 1 => {
                if let Some(dependency) = self.dependency_for(path, &target[0]) {
                    (dependency.package, target[1..target.len() - 1].to_vec())
                } else {
                    let mut module = current;
                    module.extend_from_slice(&target[..target.len() - 1]);
                    (current_crate, module)
                }
            }
            _ => return self.dependency_root(path, &target[0]).into_iter().collect(),
        };
        let (crate_name, module) = self.follow_crate_reexports(crate_name, module);
        let matches: Vec<_> = self
            .declarations_named(name)
            .into_iter()
            .filter(|item| {
                item.crate_name == crate_name
                    && item.module == module
                    && self.module_accessible_from(path, &crate_name, &module)
                    && self.accessible_from(path, item)
            })
            .collect();
        if matches.len() == 1 {
            return matches;
        }
        let key = (crate_name.clone(), module.clone(), (*name).clone());
        if !seen.insert(key) {
            return Vec::new();
        }
        let mut resolved = Vec::new();
        for (binding_path, bindings) in &self.uses {
            if self.crate_for(binding_path) != crate_name {
                continue;
            }
            for binding in bindings.iter().filter(|binding| {
                binding.public
                    && if binding.module.is_empty() {
                        self.module_for(binding_path) == module
                    } else {
                        binding.module == module
                    }
            }) {
                if binding.glob || binding.local == *name {
                    let targets = if binding.glob {
                        self.declarations_in_module(binding_path, &binding.target)
                    } else {
                        self.resolve_path_bounded(binding_path, &binding.target, seen, depth + 1)
                    };
                    resolved.extend(
                        targets
                            .into_iter()
                            .filter(|item| !binding.glob || item.name == *name),
                    );
                }
            }
        }
        resolved.sort_by(|left, right| {
            (&left.path, left.line, left.column).cmp(&(&right.path, right.line, right.column))
        });
        resolved.dedup_by(|left, right| {
            left.path == right.path && left.line == right.line && left.column == right.column
        });
        if resolved.len() == 1 {
            resolved
        } else {
            Vec::new()
        }
    }

    fn declarations_in_module(&self, path: &Path, target: &[String]) -> Vec<Declaration> {
        let current_crate = self.crate_for(path);
        let (crate_name, module) = match target.first().map(String::as_str) {
            Some("crate" | "self" | "super") => (current_crate, self.module_target(path, target)),
            Some(dependency) => {
                let Some(dependency) = self.dependency_for(path, dependency) else {
                    return Vec::new();
                };
                (dependency.package, target[1..].to_vec())
            }
            None => return Vec::new(),
        };
        if !self.module_accessible_from(path, &crate_name, &module) {
            return Vec::new();
        }
        let accessible = self.accessibility_filter(path);
        self.all_declarations()
            .filter(|item| item.crate_name == crate_name && item.module == module)
            .filter(|item| accessible(item))
            .cloned()
            .collect()
    }

    fn module_target(&self, path: &Path, target: &[String]) -> Vec<String> {
        let current = self.module_for(path);
        match target.first().map(String::as_str) {
            Some("crate") => target[1..].to_vec(),
            Some("self") => {
                let mut module = current;
                module.extend_from_slice(&target[1..]);
                module
            }
            Some("super") => {
                let parents = target
                    .iter()
                    .take_while(|segment| segment.as_str() == "super")
                    .count();
                let mut module = current;
                for _ in 0..parents {
                    module.pop();
                }
                module.extend_from_slice(&target[parents..]);
                module
            }
            _ => target.to_vec(),
        }
    }

    fn source_root_for(&self, path: &Path) -> PathBuf {
        self.source_roots
            .iter()
            .find(|root| path.starts_with(root))
            .cloned()
            .or_else(|| path.parent().map(Path::to_path_buf))
            .unwrap_or_default()
    }

    fn canonical(&self, path: &Path) -> PathBuf {
        if let Some(canonical) = self
            .canonical_paths
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(path)
        {
            return canonical.clone();
        }
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.canonical_paths
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path.to_path_buf(), canonical.clone());
        canonical
    }

    fn clear_canonical_paths(&self) {
        self.canonical_paths
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.crate_names
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    fn module_for(&self, path: &Path) -> Vec<String> {
        let canonical = self.canonical(path);
        self.module_identities
            .get(&canonical)
            .or_else(|| self.module_identities.get(path))
            .cloned()
            .unwrap_or_else(|| module_path(path, &self.source_root_for(path)))
    }

    fn crate_for(&self, path: &Path) -> String {
        if let Some(name) = self
            .crate_names
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(path)
        {
            return name.clone();
        }
        let name = self.crate_for_uncached(path);
        self.crate_names
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path.to_path_buf(), name.clone());
        name
    }

    fn crate_for_uncached(&self, path: &Path) -> String {
        let path = self.canonical(path);
        self.packages
            .iter()
            .flat_map(|package| package.targets.iter())
            .find(|target| path.starts_with(&target.source_root))
            .map(|target| target.name.clone())
            .or_else(|| {
                self.packages
                    .iter()
                    .find(|package| path.starts_with(&package.root))
                    .map(|package| package.name.clone())
            })
            .unwrap_or_default()
    }

    fn dependency_for(&self, path: &Path, name: &str) -> Option<CargoDependency> {
        let path = self.canonical(path);
        let package = self
            .packages
            .iter()
            .find(|package| path.starts_with(&package.root))?;
        package
            .dependencies
            .iter()
            .find(|dependency| dependency.name == name)
            .cloned()
    }

    /// Private items are kept within their defining module. This intentionally
    /// does not approximate Rust's wider parent/child private-item rules.
    fn accessible_from(&self, path: &Path, item: &Declaration) -> bool {
        self.accessibility_filter(path)(item)
    }

    /// Resolves the caller's crate and module once, so filtering a whole
    /// index does not repeat that lookup for every declaration.
    fn accessibility_filter(&self, path: &Path) -> impl Fn(&Declaration) -> bool {
        let current_crate = self.crate_for(path);
        let current_module = self.module_for(path);
        move |item| {
            if item.crate_name != current_crate {
                return item.visibility == Visibility::Public;
            }
            match &item.visibility {
                Visibility::Public => true,
                Visibility::Private => module_is_within(&current_module, &item.module),
                Visibility::Restricted(scope) => module_is_within(&current_module, scope),
            }
        }
    }

    /// Every named module on a path must itself be visible from the caller.
    fn module_accessible_from(&self, path: &Path, crate_name: &str, module: &[String]) -> bool {
        (1..=module.len()).all(|depth| {
            let parent = &module[..depth - 1];
            let name = &module[depth - 1];
            self.declarations_named(name)
                .iter()
                .filter(|item| {
                    item.kind == ItemKind::Class
                        && item.name == *name
                        && item.crate_name == crate_name
                        && item.module == parent
                })
                .all(|item| self.accessible_from(path, item))
        })
    }

    fn cfg_for(&self, path: &Path) -> CfgContext {
        let path = self.canonical(path);
        self.packages
            .iter()
            .find(|package| path.starts_with(&package.root))
            .map(|package| CfgContext {
                features: package.selected_features.clone(),
            })
            .unwrap_or_default()
    }

    fn use_bindings(&self, path: &Path) -> Vec<UseBinding> {
        self.open
            .get(path)
            .map(|(_, uses, _)| uses.clone())
            .or_else(|| self.uses.get(path).cloned())
            .unwrap_or_default()
    }

    fn sources(&self) -> Vec<(PathBuf, String)> {
        let mut paths = self.files.clone();
        paths.extend(self.open.keys().cloned());
        paths.sort();
        paths.dedup();
        paths
            .into_iter()
            .filter_map(|path| {
                self.open
                    .get(&path)
                    .map(|(_, _, text)| (path.clone(), text.clone()))
                    .or_else(|| std::fs::read_to_string(&path).ok().map(|text| (path, text)))
            })
            .collect()
    }

    fn document(&self, path: &Path) -> Option<(PathBuf, &str)> {
        self.open
            .get(path)
            .map(|(_, _, text)| (path.to_path_buf(), text.as_str()))
    }
}

fn member_access_at(node: Node<'_>, at: usize) -> Option<Node<'_>> {
    if node.start_byte() > at || node.end_byte() < at {
        return None;
    }
    if node.kind() == "field_expression"
        && node
            .child_by_field_name("field")
            .is_some_and(|field| field.start_byte() <= at && at <= field.end_byte())
    {
        return Some(node);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find_map(|child| member_access_at(child, at))
}

fn scoped_identifier_at(node: Node<'_>, at: usize) -> Option<Node<'_>> {
    if node.start_byte() > at || node.end_byte() < at {
        return None;
    }
    if node.kind() == "scoped_identifier"
        && node
            .child_by_field_name("name")
            .is_some_and(|name| name.start_byte() <= at && at <= name.end_byte())
    {
        return Some(node);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find_map(|child| scoped_identifier_at(child, at))
}

fn scoped_path_at(node: Node<'_>, at: usize) -> Option<Node<'_>> {
    if node.start_byte() > at || node.end_byte() < at {
        return None;
    }
    if node.kind() == "scoped_identifier" {
        return Some(node);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find_map(|child| scoped_path_at(child, at))
}

fn structural_braces(line: &str) -> (usize, usize) {
    let mut quoted = false;
    let mut escaped = false;
    let mut opens = 0;
    let mut closes = 0;
    let mut characters = line.chars().peekable();
    while let Some(character) = characters.next() {
        if !quoted && character == '/' && characters.peek() == Some(&'/') {
            break;
        }
        if character == '"' && !escaped {
            quoted = !quoted;
        }
        if !quoted {
            match character {
                '{' => opens += 1,
                '}' => closes += 1,
                _ => {}
            }
        }
        escaped = character == '\\' && !escaped;
        if character != '\\' {
            escaped = false;
        }
    }
    (opens, closes)
}

/// The nearest Cargo workspace root, then `.git`, then the file directory.
pub fn project_root(file: &Path) -> PathBuf {
    let start = file.parent().unwrap_or(file);
    let Some(package) = start
        .ancestors()
        .find(|dir| dir.join("Cargo.toml").is_file())
    else {
        return start
            .ancestors()
            .find(|dir| dir.join(".git").exists())
            .unwrap_or(start)
            .to_path_buf();
    };
    package
        .ancestors()
        .skip(1)
        .find(|dir| workspace_covers(dir, package))
        .unwrap_or(package)
        .to_path_buf()
}

/// Whether `dir` holds a Cargo workspace listing `package` in `members` or
/// `exclude`. Excluded packages count so they share the workspace engine and
/// its references, matching how `cargo_packages` indexes them.
fn workspace_covers(dir: &Path, package: &Path) -> bool {
    let Ok(contents) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
        return false;
    };
    if !contents.lines().any(|line| line.trim() == "[workspace]") {
        return false;
    }
    toml_array_values(&contents, "members")
        .into_iter()
        .chain(toml_array_values(&contents, "exclude"))
        .any(|member| match member.strip_suffix("/*") {
            Some(parent) => package.parent() == Some(dir.join(parent).as_path()),
            None => package == dir.join(member),
        })
}

/// Source directories in the local Cargo package or workspace. A normal
/// package uses `src`; custom `path` targets add their containing directory.
/// Workspace members are expanded before indexing so crates do not disappear
/// merely because the opened folder is a virtual Cargo workspace. This reads
/// manifests only; dependency resolution and feature selection come later in
/// the native Cargo graph.
fn cargo_source_roots(root: &Path) -> Vec<PathBuf> {
    let manifest = root.join("Cargo.toml");
    let mut packages = vec![root.to_path_buf()];
    let Ok(contents) = std::fs::read_to_string(&manifest) else {
        return vec![root.join("src")];
    };
    for member in toml_array_values(&contents, "members")
        .into_iter()
        .chain(toml_array_values(&contents, "exclude"))
    {
        let pattern = root.join(member);
        if member.ends_with("/*") {
            let Some(parent) = pattern.parent() else {
                continue;
            };
            let Ok(entries) = std::fs::read_dir(parent) else {
                continue;
            };
            packages.extend(
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.is_dir() && path.join("Cargo.toml").is_file()),
            );
        } else if pattern.join("Cargo.toml").is_file() {
            packages.push(pattern);
        }
    }
    packages.sort();
    packages.dedup();
    let mut roots = Vec::new();
    for package in packages {
        roots.extend(cargo_package_source_roots(&package));
    }
    roots.retain(|path| path.is_dir());
    roots.sort();
    roots.dedup();
    roots
}

fn cargo_packages(root: &Path) -> Vec<CargoPackage> {
    let manifest = root.join("Cargo.toml");
    let Ok(contents) = std::fs::read_to_string(&manifest) else {
        return vec![cargo_package(root).unwrap_or_else(|| CargoPackage {
            name: String::new(),
            root: root.to_path_buf(),
            targets: vec![CargoTarget {
                name: String::new(),
                source_root: root.join("src"),
                root: root.join("src/lib.rs"),
            }],
            dependencies: Vec::new(),
            selected_features: HashSet::new(),
            indexed: true,
        })];
    };
    let mut roots = vec![root.to_path_buf()];
    for member in toml_array_values(&contents, "members")
        .into_iter()
        .chain(toml_array_values(&contents, "exclude"))
    {
        let pattern = root.join(member);
        if member.ends_with("/*") {
            if let Some(parent) = pattern.parent()
                && let Ok(entries) = std::fs::read_dir(parent)
            {
                roots.extend(
                    entries
                        .flatten()
                        .map(|entry| entry.path())
                        .filter(|path| path.is_dir() && path.join("Cargo.toml").is_file()),
                );
            }
        } else if pattern.join("Cargo.toml").is_file() {
            roots.push(pattern);
        }
    }
    // A nested workspace (often an app) is intentionally absent from the
    // parent's `members`, but its files and dependencies still belong to the
    // opened folder. Only inspect immediate children to avoid indexing target/.
    if let Ok(children) = std::fs::read_dir(root) {
        for child in children.flatten() {
            let package = child.path();
            if let Ok(manifest) = std::fs::read_to_string(package.join("Cargo.toml"))
                && manifest.lines().any(|line| line.trim() == "[workspace]")
                && toml_section_value(&manifest, "package", "name").is_some()
            {
                roots.push(package);
            }
        }
    }
    let mut packages = Vec::new();
    let mut processed = HashMap::new();
    let mut requested: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    // Process workspace members before nested apps. Feature requests from an
    // app then reach its dependencies before their first visit, even if that
    // app's name sorts ahead of the library names.
    roots.sort_by_key(|path| {
        std::fs::read_to_string(path.join("Cargo.toml"))
            .ok()
            .is_some_and(|manifest| {
                manifest.lines().any(|line| line.trim() == "[workspace]")
                    && toml_section_value(&manifest, "package", "name").is_some()
            })
    });
    while let Some(root) = roots.pop() {
        let root = root.canonicalize().unwrap_or(root);
        let requested_features = requested.get(&root).cloned().unwrap_or_default();
        if processed.get(&root) == Some(&requested_features) {
            continue;
        }
        let Some(package) = cargo_package_with_requested(&root, requested_features.clone()) else {
            continue;
        };
        processed.insert(root.clone(), requested_features);
        for dependency in &package.dependencies {
            let Some(path) = dependency
                .path
                .as_ref()
                .filter(|path| path.join("Cargo.toml").is_file())
            else {
                continue;
            };
            let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            let requested = requested.entry(path.clone()).or_default();
            requested.extend(dependency.features.iter().cloned());
            if dependency.default_features {
                requested.remove(NO_DEFAULT_FEATURES);
            } else if requested.is_empty() {
                requested.insert(NO_DEFAULT_FEATURES.to_owned());
            }
            roots.push(path);
        }
        if let Some(previous) = packages
            .iter()
            .position(|known: &CargoPackage| known.root == root)
        {
            packages[previous] = package;
        } else {
            packages.push(package);
        }
    }
    let locked = cargo_lock_packages(root);
    let cached: Vec<_> = packages
        .iter()
        .flat_map(|package| package.dependencies.iter())
        .filter(|dependency| dependency.path.is_none())
        .filter_map(|dependency| cached_package(dependency, &locked))
        .collect();
    let mut cached = cached;
    // `pub use sea_query;` makes another crate part of this crate's public
    // API, so its types are reachable from user code and must be indexed too.
    let mut next = 0;
    while next < cached.len() && cached.len() < MAX_REEXPORTED_PACKAGES {
        let package = cached[next].clone();
        next += 1;
        for dependency in reexported_dependencies(&package) {
            if let Some(found) = cached_package(&dependency, &locked)
                && !packages.iter().any(|known| known.root == found.root)
                && !cached.iter().any(|known| known.root == found.root)
            {
                cached.push(found);
            }
        }
    }
    for package in cached {
        if !packages.iter().any(|known| known.root == package.root) {
            packages.push(package);
        }
    }
    packages.sort_by(|left, right| left.root.cmp(&right.root));
    packages
}

const MAX_REEXPORTED_PACKAGES: usize = 64;

/// Registry dependencies that `package` re-exports with a top-level
/// `pub use dependency...;` in one of its target roots.
fn reexported_dependencies(package: &CargoPackage) -> Vec<CargoDependency> {
    let mut names = HashSet::new();
    for target in &package.targets {
        let Ok(source) = std::fs::read_to_string(&target.root) else {
            continue;
        };
        for line in source.lines() {
            let Some(rest) = line.strip_prefix("pub use ") else {
                continue;
            };
            let name: String = rest.chars().take_while(|c| is_ident(*c)).collect();
            if !name.is_empty() {
                names.insert(name);
            }
        }
    }
    package
        .dependencies
        .iter()
        .filter(|dependency| dependency.path.is_none() && !dependency.git)
        .filter(|dependency| {
            names.contains(&dependency.package)
                || names.contains(&dependency.name.replace('-', "_"))
        })
        .cloned()
        .collect()
}

fn cargo_package(root: &Path) -> Option<CargoPackage> {
    cargo_package_with_requested(root, HashSet::new())
}

fn cargo_package_with_requested(
    root: &Path,
    requested_features: HashSet<String>,
) -> Option<CargoPackage> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let contents = std::fs::read_to_string(root.join("Cargo.toml")).ok()?;
    let name = toml_section_value(&contents, "package", "name")?.replace('-', "_");
    let targets = cargo_target_roots(&root)
        .into_iter()
        .map(|root| CargoTarget {
            name: name.clone(),
            source_root: root.parent().unwrap_or(&root).to_path_buf(),
            root,
        })
        .collect();
    let default_features = if requested_features.contains(NO_DEFAULT_FEATURES) {
        HashSet::new()
    } else {
        cargo_default_features(&contents)
    };
    let features = cargo_features(&contents);
    let selected_features = selected_features(&default_features, &features, &requested_features);
    Some(CargoPackage {
        name,
        root: root.clone(),
        targets,
        dependencies: cargo_dependencies(&root, &contents, &selected_features, &features),
        selected_features,
        indexed: true,
    })
}

fn cargo_dependencies(
    root: &Path,
    contents: &str,
    selected_features: &HashSet<String>,
    feature_definitions: &HashMap<String, Vec<String>>,
) -> Vec<CargoDependency> {
    let mut section = "";
    let mut dependencies = Vec::new();
    let normalized = inline_dependency_tables(contents);
    for line in normalized.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = line.trim_matches(&['[', ']'][..]);
            continue;
        }
        if !dependency_section_active(section) {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let mut name = name.trim();
        let mut value = value.trim();
        if value == "true"
            && let Some(inherited_name) = name.strip_suffix(".workspace")
        {
            name = inherited_name;
            value = "{ workspace = true }";
        }
        let inherited = (toml_inline_value(value, "workspace").as_deref() == Some("true"))
            .then(|| workspace_dependency(root, name))
            .flatten();
        let (dependency_root, value) = inherited
            .as_ref()
            .map(|(root, value)| (root.as_path(), value.as_str()))
            .unwrap_or((root, value));
        let simple_version = toml_string(value);
        let path = toml_inline_value(value, "path");
        let version = toml_inline_value(value, "version").or(simple_version);
        let git = toml_inline_value(value, "git");
        let registry = toml_inline_value(value, "registry");
        if path.is_none() && version.is_none() && git.is_none() && registry.is_none() {
            continue;
        }
        let package = toml_inline_value(value, "package")
            .unwrap_or_else(|| name.to_owned())
            .replace('-', "_");
        let optional = toml_inline_value(value, "optional").as_deref() == Some("true");
        let default_features =
            toml_inline_value(value, "default-features").as_deref() != Some("false");
        let mut features: HashSet<_> = toml_array_values_in_inline(value, "features")
            .into_iter()
            .map(str::to_owned)
            .collect();
        features.extend(forwarded_dependency_features(
            name,
            selected_features,
            feature_definitions,
        ));
        let dependency = CargoDependency {
            name: name.replace('-', "_"),
            package,
            path: path.map(|path| dependency_root.join(path)),
            git: git.is_some(),
            optional,
            default_features,
            features,
        };
        if !dependency.optional
            || selected_features.contains(&dependency.name)
            || selected_features.contains(name)
        {
            dependencies.push(dependency);
        }
    }
    dependencies
}

fn workspace_dependency(root: &Path, name: &str) -> Option<(PathBuf, String)> {
    for directory in root.ancestors() {
        let Ok(contents) = std::fs::read_to_string(directory.join("Cargo.toml")) else {
            continue;
        };
        let mut section = "";
        for line in contents.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.starts_with('[') && line.ends_with(']') {
                section = line.trim_matches(&['[', ']'][..]);
                continue;
            }
            if section != "workspace.dependencies" {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key.trim() == name {
                return Some((directory.to_path_buf(), value.trim().to_owned()));
            }
        }
    }
    None
}

/// Cargo feature entries such as `dep/feature` enable a feature in a
/// dependency. The package's selected features have already been expanded,
/// so only direct forwarding remains here.
fn forwarded_dependency_features(
    dependency: &str,
    selected_features: &HashSet<String>,
    definitions: &HashMap<String, Vec<String>>,
) -> HashSet<String> {
    let mut forwarded = HashSet::new();
    for feature in selected_features {
        for entry in definitions.get(feature).into_iter().flatten() {
            let Some((name, feature)) = entry.split_once('/') else {
                continue;
            };
            if name.trim_end_matches('?') == dependency
                && (!name.ends_with('?') || selected_features.contains(dependency))
            {
                forwarded.insert(feature.to_owned());
            }
        }
    }
    forwarded
}

/// Rewrites `[dependencies.name]` tables, as written by `cargo publish`, into
/// the one-line `name = { ... }` form the dependency parser reads.
fn inline_dependency_tables(contents: &str) -> String {
    const SECTIONS: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];
    fn flush(out: &mut String, table: Option<(&'static str, String, Vec<String>)>) {
        if let Some((section, name, entries)) = table {
            out.push_str(&format!(
                "[{section}]\n{name} = {{ {} }}\n",
                entries.join(", ")
            ));
        }
    }
    let mut out = String::new();
    let mut table: Option<(&'static str, String, Vec<String>)> = None;
    let mut current = String::new();
    let mut depth = 0i32;
    for line in contents.lines() {
        let code = line.split('#').next().unwrap_or("").trim();
        if depth == 0 && code.starts_with('[') && code.ends_with(']') {
            flush(&mut out, table.take());
            let header = code.trim_matches(&['[', ']'][..]);
            let named = SECTIONS.into_iter().find_map(|section| {
                header.strip_prefix(section).and_then(|rest| {
                    rest.strip_prefix('.')
                        .map(|name| (section, name.trim_matches('"').to_owned()))
                })
            });
            if let Some((section, name)) = named {
                table = Some((section, name, Vec::new()));
            } else {
                out.push_str(line);
                out.push('\n');
            }
            continue;
        }
        let Some((_, _, entries)) = table.as_mut() else {
            out.push_str(line);
            out.push('\n');
            continue;
        };
        if code.is_empty() {
            continue;
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(code);
        depth += code.matches('[').count() as i32 - code.matches(']').count() as i32;
        if depth <= 0 {
            depth = 0;
            entries.push(std::mem::take(&mut current));
        }
    }
    flush(&mut out, table.take());
    out
}

fn dependency_section_active(section: &str) -> bool {
    matches!(
        section,
        "dependencies" | "dev-dependencies" | "build-dependencies"
    ) || section.strip_prefix("target.").is_some_and(|target| {
        target.ends_with(".dependencies")
            || target.ends_with(".dev-dependencies")
            || target.ends_with(".build-dependencies")
    }) && target_section_matches(section)
}

fn target_section_matches(section: &str) -> bool {
    let Some(target) = section.strip_prefix("target.") else {
        return false;
    };
    let target = target
        .strip_suffix(".dependencies")
        .or_else(|| target.strip_suffix(".dev-dependencies"))
        .or_else(|| target.strip_suffix(".build-dependencies"))
        .unwrap_or("")
        .trim_matches('"')
        .trim_matches('\'');
    target == "cfg(unix)"
        || target == "cfg(macos)"
        || target == "cfg(target_os = \"macos\")"
        || target == "cfg(target_arch = \"aarch64\")"
        || target.contains("target_os = \"macos\"") && target.contains("target_arch = \"aarch64\"")
}

fn cargo_features(contents: &str) -> HashMap<String, Vec<String>> {
    let mut features = HashMap::new();
    let mut section = "";
    for line in contents.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = line.trim_matches(&['[', ']'][..]);
        } else if section == "features"
            && let Some((name, value)) = line.split_once('=')
        {
            features.insert(
                name.trim().to_owned(),
                toml_strings(value).into_iter().map(str::to_owned).collect(),
            );
        }
    }
    features
}

fn selected_features(
    defaults: &HashSet<String>,
    features: &HashMap<String, Vec<String>>,
    requested: &HashSet<String>,
) -> HashSet<String> {
    let mut selected = HashSet::new();
    let mut pending: Vec<_> = defaults.iter().chain(requested).cloned().collect();
    while let Some(entry) = pending.pop() {
        if entry.contains("?/") {
            continue;
        }
        let feature = entry
            .strip_prefix("dep:")
            .unwrap_or(&entry)
            .trim_end_matches('?')
            .split('/')
            .next()
            .unwrap_or(&entry);
        if selected.insert(feature.to_owned()) {
            for entry in features.get(feature).into_iter().flatten() {
                pending.push(entry.to_owned());
            }
        }
    }
    selected
}

fn toml_string(value: &str) -> Option<String> {
    let value = value.trim();
    (value.starts_with('"') && value.ends_with('"') && value.len() >= 2)
        .then(|| value[1..value.len() - 1].to_owned())
}

/// Package-local default features only. Dependency feature unification belongs
/// to Cargo and is deliberately not guessed by this static index.
fn cargo_default_features(contents: &str) -> HashSet<String> {
    let mut defaults = HashSet::new();
    for feature in toml_array_values_in_section(contents, "features", "default") {
        defaults.insert(feature.to_owned());
    }
    defaults
}

fn toml_array_values_in_section<'a>(contents: &'a str, section: &str, key: &str) -> Vec<&'a str> {
    let mut in_section = false;
    let mut collecting = false;
    let mut values = Vec::new();
    for line in contents.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_section = line.trim_matches(&['[', ']'][..]) == section;
            collecting = false;
            continue;
        }
        if !in_section {
            continue;
        }
        if !collecting {
            let Some((found, value)) = line.split_once('=') else {
                continue;
            };
            if found.trim() != key {
                continue;
            }
            collecting = true;
            collect_toml_strings(value, &mut values);
            if value.contains(']') {
                break;
            }
        } else {
            collect_toml_strings(line, &mut values);
            if line.contains(']') {
                break;
            }
        }
    }
    values
}

fn toml_section_value(contents: &str, section: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for line in contents.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_section = line.trim_matches(&['[', ']'][..]) == section;
            continue;
        }
        if in_section
            && let Some((found, value)) = line.split_once('=')
            && found.trim() == key
        {
            return Some(value.trim().trim_matches('"').to_owned());
        }
    }
    None
}

fn toml_inline_value(value: &str, key: &str) -> Option<String> {
    let value = value.trim().trim_matches(&['{', '}'][..]);
    // Commas inside a feature array belong to the array, not the table.
    // A substring search also accepts compact `features=[...]` syntax.
    let mut depth = 0usize;
    let mut start = 0;
    let mut fields = Vec::new();
    for (at, character) in value.char_indices() {
        match character {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                fields.push(&value[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    fields.push(&value[start..]);
    fields.into_iter().find_map(|field| {
        let (found, value) = field.split_once('=')?;
        (found.trim() == key).then(|| value.trim().trim_matches('"').to_owned())
    })
}

fn toml_array_values_in_inline<'a>(value: &'a str, key: &str) -> Vec<&'a str> {
    let Some(at) = value.find(key) else {
        return Vec::new();
    };
    let rest = &value[at + key.len()..];
    let Some(array) = rest.trim_start().strip_prefix('=') else {
        return Vec::new();
    };
    let Some(array) = array.trim_start().strip_prefix('[') else {
        return Vec::new();
    };
    let Some((array, _)) = array.split_once(']') else {
        return Vec::new();
    };
    toml_strings(array)
}

fn toml_strings(source: &str) -> Vec<&str> {
    let mut values = Vec::new();
    collect_toml_strings(source, &mut values);
    values
}

#[derive(Debug, Clone)]
struct LockedPackage {
    name: String,
    version: String,
    source: Option<String>,
}

fn cargo_lock_packages(root: &Path) -> Vec<LockedPackage> {
    let Some(lockfile) = root
        .ancestors()
        .map(|directory| directory.join("Cargo.lock"))
        .find(|path| path.is_file())
    else {
        return Vec::new();
    };
    let Ok(contents) = std::fs::read_to_string(lockfile) else {
        return Vec::new();
    };
    let mut packages = Vec::new();
    let mut current: Option<LockedPackage> = None;
    for line in contents.lines().map(str::trim) {
        if line == "[[package]]" {
            if let Some(package) = current.take() {
                packages.push(package);
            }
            current = Some(LockedPackage {
                name: String::new(),
                version: String::new(),
                source: None,
            });
        } else if let Some(package) = &mut current {
            if let Some(value) = line.strip_prefix("name = ").and_then(toml_string) {
                package.name = value;
            } else if let Some(value) = line.strip_prefix("version = ").and_then(toml_string) {
                package.version = value;
            } else if let Some(value) = line.strip_prefix("source = ").and_then(toml_string) {
                package.source = Some(value);
            }
        }
    }
    if let Some(package) = current {
        packages.push(package);
    }
    packages
}

fn cached_package(dependency: &CargoDependency, locked: &[LockedPackage]) -> Option<CargoPackage> {
    let package = locked.iter().find(|package| {
        package.name.replace('-', "_") == dependency.package
            && if dependency.git {
                package
                    .source
                    .as_deref()
                    .is_some_and(|source| source.starts_with("git+"))
            } else {
                package
                    .source
                    .as_deref()
                    .is_some_and(|source| source.starts_with("registry+"))
            }
    })?;
    let root = if dependency.git {
        git_checkout_package(&package.name, &package.version)?
    } else {
        registry_package_source(&package.name, &package.version)?
    };
    let contents = std::fs::read_to_string(root.join("Cargo.toml")).ok()?;
    if toml_section_value(&contents, "package", "name")? != package.name
        || toml_section_value(&contents, "package", "version")? != package.version
    {
        return None;
    }
    let mut cached = cargo_package(&root)?;
    cached.indexed = true;
    Some(cached)
}

fn cargo_home() -> Option<PathBuf> {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
}

fn registry_package_source(name: &str, version: &str) -> Option<PathBuf> {
    registry_package_source_in(&cargo_home()?, name, version)
}

fn registry_package_source_in(cargo_home: &Path, name: &str, version: &str) -> Option<PathBuf> {
    let src = cargo_home.join("registry/src");
    std::fs::read_dir(src)
        .ok()?
        .flatten()
        .map(|entry| entry.path().join(format!("{name}-{version}")))
        .find(|path| path.is_dir())
}

fn git_checkout_package(name: &str, version: &str) -> Option<PathBuf> {
    let checkouts = cargo_home()?.join("git/checkouts");
    for checkout in std::fs::read_dir(checkouts).ok()?.flatten() {
        for revision in std::fs::read_dir(checkout.path()).ok()?.flatten() {
            let path = revision.path();
            let manifest = path.join("Cargo.toml");
            if let Ok(contents) = std::fs::read_to_string(manifest)
                && toml_section_value(&contents, "package", "name").as_deref() == Some(name)
                && toml_section_value(&contents, "package", "version").as_deref() == Some(version)
            {
                return Some(path);
            }
        }
    }
    None
}

fn cargo_package_source_roots(root: &Path) -> Vec<PathBuf> {
    let manifest = root.join("Cargo.toml");
    let mut roots = vec![root.join("src")];
    let Ok(contents) = std::fs::read_to_string(manifest) else {
        return roots;
    };
    for line in contents.lines().map(str::trim) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "path" {
            continue;
        }
        let value = value.trim().trim_matches('"');
        let path = root.join(value);
        if path.extension().is_some_and(|extension| extension == "rs")
            && let Some(parent) = path.parent()
        {
            roots.push(parent.to_path_buf());
        }
    }
    roots
}

/// Manifest-declared and conventional Cargo entry points. This is deliberately
/// structural: it does not ask Cargo which target configuration it would build.
fn cargo_target_roots(root: &Path) -> Vec<PathBuf> {
    let mut targets = vec![root.join("src/lib.rs"), root.join("src/main.rs")];
    for directory in ["src/bin", "examples", "tests", "benches"] {
        if let Ok(entries) = std::fs::read_dir(root.join(directory)) {
            targets.extend(
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.extension().is_some_and(|extension| extension == "rs")),
            );
        }
    }
    if let Ok(contents) = std::fs::read_to_string(root.join("Cargo.toml")) {
        targets.extend(contents.lines().map(str::trim).filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "path")
                .then(|| root.join(value.trim().trim_matches('"')))
                .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        }));
    }
    targets.retain(|path| path.is_file());
    targets.sort();
    targets.dedup();
    targets
}

/// Quoted values from a TOML array assignment. Cargo workspace membership is
/// intentionally parsed narrowly here: strings and comments, not arbitrary
/// TOML expressions. The parser can be replaced by the full manifest model
/// once package metadata, features, and dependencies are added.
fn toml_array_values<'a>(contents: &'a str, key: &str) -> Vec<&'a str> {
    let mut collecting = false;
    let mut values = Vec::new();
    for line in contents.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if !collecting {
            let Some((found, value)) = line.split_once('=') else {
                continue;
            };
            if found.trim() != key {
                continue;
            }
            collecting = true;
            collect_toml_strings(value, &mut values);
            if value.contains(']') {
                break;
            }
        } else {
            collect_toml_strings(line, &mut values);
            if line.contains(']') {
                break;
            }
        }
    }
    values
}

fn collect_toml_strings<'a>(source: &'a str, values: &mut Vec<&'a str>) {
    let mut rest = source;
    while let Some(start) = rest.find('"') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('"') else { break };
        values.push(&rest[..end]);
        rest = &rest[end + 1..];
    }
}

#[derive(Default)]
struct StaticModuleGraph {
    identities: HashMap<PathBuf, Vec<String>>,
    uses: HashMap<PathBuf, Vec<UseBinding>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
enum StaticItem {
    Use {
        module: Vec<String>,
        bindings: Vec<UseBinding>,
    },
    Module {
        module: Vec<String>,
        path_override: Option<String>,
    },
    Include {
        module: Vec<String>,
        source: String,
    },
}

/// Parse results of Rust files from a previous build, reused while a file's
/// size and modification time stay the same.
#[derive(Debug, Default)]
pub struct RustIndexCache {
    files: HashMap<PathBuf, CachedRustFile>,
}

const RUST_INDEX_CACHE_SCHEMA: u32 = 2;

#[derive(serde::Serialize)]
struct StoredRustIndexRef<'a> {
    schema: u32,
    engine: u128,
    root: &'a Path,
    files: &'a HashMap<PathBuf, CachedRustFile>,
}

#[derive(serde::Deserialize)]
struct StoredRustIndex {
    schema: u32,
    engine: u128,
    root: PathBuf,
    files: HashMap<PathBuf, CachedRustFile>,
}

impl RustIndexCache {
    /// Loads the cache saved for `root`. An unreadable file, another schema,
    /// another root or a rebuilt executable gives an empty cache.
    pub fn load(path: &Path, root: &Path) -> Self {
        let files = std::fs::read(path)
            .ok()
            .and_then(|bytes| bincode::deserialize::<StoredRustIndex>(&bytes).ok())
            .filter(|stored| {
                stored.schema == RUST_INDEX_CACHE_SCHEMA
                    && stored.engine == engine_stamp()
                    && stored.root == root
            })
            .map(|stored| stored.files)
            .unwrap_or_default();
        Self { files }
    }

    pub fn save(&self, path: &Path, root: &Path) -> io::Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("cache path has no parent"))?;
        std::fs::create_dir_all(parent)?;
        let bytes = bincode::serialize(&StoredRustIndexRef {
            schema: RUST_INDEX_CACHE_SCHEMA,
            engine: engine_stamp(),
            root,
            files: &self.files,
        })
        .map_err(io::Error::other)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        io::Write::write_all(&mut temporary, &bytes)?;
        temporary.persist(path).map_err(|error| error.error)?;
        Ok(())
    }
}

/// Modification time of the running executable. Parse results are only valid
/// for the parser code that produced them, so a rebuilt app drops the cache.
fn engine_stamp() -> u128 {
    std::env::current_exe()
        .and_then(std::fs::metadata)
        .ok()
        .and_then(|metadata| FileStamp::of(&metadata))
        .map_or(0, |stamp| stamp.modified_nanos)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct FileStamp {
    size: u64,
    modified_nanos: u128,
}

impl FileStamp {
    fn of(metadata: &std::fs::Metadata) -> Option<Self> {
        let modified = metadata
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        Some(Self {
            size: metadata.len(),
            modified_nanos: modified.as_nanos(),
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CachedRustFile {
    stamp: FileStamp,
    scan: Option<CachedScan>,
    declarations: Option<CachedDeclarations>,
    uses: Option<Vec<UseBinding>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CachedScan {
    features: Vec<String>,
    items: Vec<StaticItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct DeclarationContext {
    source_root: PathBuf,
    crate_name: String,
    features: Vec<String>,
    module: Option<Vec<String>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CachedDeclarations {
    context: DeclarationContext,
    items: Vec<Declaration>,
}

/// Entries of the previous build and the entries confirmed by this one.
struct FileCache {
    previous: HashMap<PathBuf, CachedRustFile>,
    next: HashMap<PathBuf, CachedRustFile>,
}

impl FileCache {
    fn fresh(&self, path: &Path, stamp: FileStamp) -> Option<&CachedRustFile> {
        self.next
            .get(path)
            .or_else(|| self.previous.get(path))
            .filter(|file| file.stamp == stamp)
    }

    fn entry(&mut self, path: &Path, stamp: FileStamp) -> &mut CachedRustFile {
        let previous = &mut self.previous;
        let file = self.next.entry(path.to_path_buf()).or_insert_with(|| {
            previous
                .remove(path)
                .filter(|file| file.stamp == stamp)
                .unwrap_or_else(|| CachedRustFile::new(stamp))
        });
        if file.stamp != stamp {
            *file = CachedRustFile::new(stamp);
        }
        file
    }
}

impl CachedRustFile {
    fn new(stamp: FileStamp) -> Self {
        Self {
            stamp,
            scan: None,
            declarations: None,
            uses: None,
        }
    }
}

fn sorted_features(cfg: &CfgContext) -> Vec<String> {
    let mut features = cfg.features.iter().cloned().collect::<Vec<_>>();
    features.sort();
    features
}

/// Discover the subset of Rust's module graph that can be followed without
/// macro expansion. Files not reached here retain their filesystem identity.
fn static_module_graph(packages: &[CargoPackage], cache: &mut FileCache) -> StaticModuleGraph {
    let mut graph = StaticModuleGraph::default();
    let mut visited = HashSet::new();
    for package in packages.iter().filter(|package| package.indexed) {
        for target in &package.targets {
            for path in static_target_roots(package, target) {
                discover_static_module(
                    &path,
                    Vec::new(),
                    &package.root,
                    &CfgContext {
                        features: package.selected_features.clone(),
                    },
                    &mut graph,
                    &mut visited,
                    cache,
                );
            }
        }
    }
    graph
}

fn static_target_roots(_package: &CargoPackage, target: &CargoTarget) -> Vec<PathBuf> {
    let mut roots = vec![target.root.clone()];
    roots.sort();
    roots.dedup();
    roots
}

fn discover_static_module(
    path: &Path,
    module: Vec<String>,
    manifest_dir: &Path,
    cfg: &CfgContext,
    graph: &mut StaticModuleGraph,
    visited: &mut HashSet<PathBuf>,
    cache: &mut FileCache,
) {
    if graph.identities.len() >= MAX_FILES {
        return;
    }
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !visited.insert(path.clone()) {
        return;
    }
    let Ok(metadata) = std::fs::metadata(&path) else {
        return;
    };
    if metadata.len() > MAX_FILE_BYTES {
        return;
    }
    let stamp = FileStamp::of(&metadata);
    let features = sorted_features(cfg);
    let cached = stamp
        .and_then(|stamp| cache.fresh(&path, stamp))
        .and_then(|file| file.scan.as_ref())
        .filter(|scan| scan.features == features)
        .map(|scan| scan.items.clone());
    let items = match cached {
        Some(items) => items,
        None => {
            let Ok(text) = std::fs::read_to_string(&path) else {
                return;
            };
            let mut items = Vec::new();
            if let Some(tree) = parse_tree(&text) {
                scan_static_items(&text, tree.root_node(), &[], cfg, &mut items);
            }
            items
        }
    };
    if let Some(stamp) = stamp {
        cache.entry(&path, stamp).scan = Some(CachedScan {
            features,
            items: items.clone(),
        });
    }
    graph.identities.insert(path.clone(), module.clone());
    for item in items {
        match item {
            StaticItem::Use {
                module: suffix,
                bindings,
            } => {
                let binding_module = [module.as_slice(), &suffix].concat();
                graph
                    .uses
                    .entry(path.clone())
                    .or_default()
                    .extend(bindings.into_iter().map(|mut binding| {
                        binding.module = binding_module.clone();
                        binding
                    }));
            }
            StaticItem::Module {
                module: suffix,
                path_override,
            } => {
                let Some(name) = suffix.last() else {
                    continue;
                };
                if let Some(child_path) = static_module_file(&path, path_override.as_deref(), name)
                {
                    discover_static_module(
                        &child_path,
                        [module.as_slice(), &suffix].concat(),
                        manifest_dir,
                        cfg,
                        graph,
                        visited,
                        cache,
                    );
                }
            }
            StaticItem::Include {
                module: suffix,
                source,
            } => {
                let Some(include) = static_include_file(&source, &path, manifest_dir) else {
                    continue;
                };
                discover_static_module(
                    &include,
                    [module.as_slice(), &suffix].concat(),
                    manifest_dir,
                    cfg,
                    graph,
                    visited,
                    cache,
                );
                let include = include.canonicalize().unwrap_or(include);
                if let Some(bindings) = graph.uses.get(&include).cloned() {
                    graph.uses.entry(path.clone()).or_default().extend(bindings);
                }
            }
        }
    }
}

/// Module-graph facts of one file that do not depend on where the file sits
/// in the graph. `module` is relative to the file's own module.
fn scan_static_items(
    text: &str,
    node: Node<'_>,
    module: &[String],
    cfg: &CfgContext,
    out: &mut Vec<StaticItem>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if cfg_disabled(child, text, cfg) {
            continue;
        }
        if child.kind() == "use_declaration" {
            out.push(StaticItem::Use {
                module: module.to_vec(),
                bindings: use_bindings(child.utf8_text(text.as_bytes()).unwrap_or("")),
            });
        }
        if child.kind() == "mod_item" {
            let Some(name) = child
                .child_by_field_name("name")
                .and_then(|name| name.utf8_text(text.as_bytes()).ok())
            else {
                continue;
            };
            let mut child_module = module.to_vec();
            child_module.push(name.to_owned());
            if child
                .utf8_text(text.as_bytes())
                .is_ok_and(|source| source.trim_end().ends_with('}'))
            {
                scan_static_items(text, child, &child_module, cfg, out);
            } else {
                out.push(StaticItem::Module {
                    module: child_module,
                    path_override: module_path_override(child, text).map(str::to_owned),
                });
            }
            continue;
        }
        if child.kind() == "macro_invocation"
            && let Ok(source) = child.utf8_text(text.as_bytes())
            && source.trim().starts_with("include!")
        {
            out.push(StaticItem::Include {
                module: module.to_vec(),
                source: source.trim().to_owned(),
            });
            continue;
        }
        scan_static_items(text, child, module, cfg, out);
    }
}

fn static_module_file(path: &Path, path_override: Option<&str>, name: &str) -> Option<PathBuf> {
    if let Some(override_path) = path_override {
        let path = module_directory(path).join(override_path);
        return path.is_file().then_some(path);
    }
    let directory = module_directory(path);
    let files = [
        directory.join(format!("{name}.rs")),
        directory.join(name).join("mod.rs"),
    ];
    let mut found = files.into_iter().filter(|path| path.is_file());
    let path = found.next()?;
    found.next().is_none().then_some(path)
}

fn static_include_file(source: &str, path: &Path, manifest_dir: &Path) -> Option<PathBuf> {
    let argument = source
        .trim()
        .strip_prefix("include!")?
        .trim()
        .strip_prefix('(')?
        .strip_suffix(')')?
        .trim();
    if let Some(relative) = rust_string_literal(argument) {
        return static_include_path(path.parent()?, &relative);
    }
    let body = argument
        .strip_prefix("concat!")?
        .trim()
        .strip_prefix('(')?
        .strip_suffix(')')?;
    let manifest_prefix = "env!(\"CARGO_MANIFEST_DIR\")";
    if let Some(suffix) = body
        .strip_prefix(manifest_prefix)
        .map(str::trim_start)
        .and_then(|suffix| suffix.strip_prefix(','))
        .map(str::trim)
        .and_then(rust_string_literal)
    {
        return static_include_path(manifest_dir, suffix.trim_start_matches(['/', '\\']));
    }
    let suffix = body
        .strip_prefix("env!(\"OUT_DIR\")")?
        .trim_start()
        .strip_prefix(',')?
        .trim();
    out_dir_include_file_in(&manifest_dir.join("target"), &rust_string_literal(suffix)?)
}

fn static_include_path(base: &Path, relative: &str) -> Option<PathBuf> {
    let include = PathBuf::from(relative);
    let include = if include.is_absolute() {
        include
    } else {
        base.join(include)
    };
    include.is_file().then_some(include)
}

/// Find a single existing Cargo build-script output without evaluating `OUT_DIR`.
fn out_dir_include_file_in(target_dir: &Path, suffix: &str) -> Option<PathBuf> {
    let suffix = Path::new(suffix.trim_start_matches(['/', '\\']));
    if suffix.as_os_str().is_empty()
        || !suffix
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
    {
        return None;
    }
    let target_dir = target_dir.canonicalize().ok()?;
    let mut matches = Vec::new();
    for entry in WalkBuilder::new(&target_dir)
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .parents(false)
        .build()
        .flatten()
        .take(MAX_FILES)
    {
        let path = entry.path();
        if !path.is_file()
            || entry
                .metadata()
                .map_or(true, |metadata| metadata.len() > MAX_FILE_BYTES)
        {
            continue;
        }
        let Ok(path) = path.canonicalize() else {
            continue;
        };
        let Ok(relative) = path.strip_prefix(&target_dir) else {
            continue;
        };
        let components: Vec<_> = relative.components().collect();
        let Some(build) = components
            .iter()
            .position(|component| component.as_os_str() == "build")
        else {
            continue;
        };
        if components
            .get(build + 2)
            .is_none_or(|component| component.as_os_str() != "out")
            || components[build + 3..]
                .iter()
                .map(|component| component.as_os_str())
                .ne(suffix.components().map(|component| component.as_os_str()))
        {
            continue;
        }
        matches.push(path);
        if matches.len() > 1 {
            return None;
        }
    }
    matches.pop()
}

fn rust_string_literal(source: &str) -> Option<String> {
    let source = source.trim();
    (source.starts_with('"') && source.ends_with('"') && source.len() >= 2)
        .then(|| source[1..source.len() - 1].to_owned())
}

fn module_path(path: &Path, source_root: &Path) -> Vec<String> {
    let Ok(relative) = path.strip_prefix(source_root) else {
        return Vec::new();
    };
    let mut parts: Vec<_> = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .map(str::to_owned)
        .collect();
    let Some(file) = parts.pop() else {
        return Vec::new();
    };
    let stem = Path::new(&file)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("");
    if !matches!(stem, "lib" | "main" | "mod") && !stem.is_empty() {
        parts.push(stem.to_owned());
    }
    parts
}

fn parse_tree(text: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .ok()?;
    parser.parse(text, None)
}

fn parse_declarations(
    path: &Path,
    text: &str,
    source_root: &Path,
    crate_name: &str,
    cfg: &CfgContext,
    module: Option<&[String]>,
) -> Vec<Declaration> {
    let Some(tree) = parse_tree(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let module = module.map_or_else(|| module_path(path, source_root), <[String]>::to_vec);
    collect_declarations(
        path,
        text,
        tree.root_node(),
        crate_name,
        &module,
        cfg,
        &mut out,
    );
    collect_macro_functions(
        path,
        text,
        tree.root_node(),
        crate_name,
        &module,
        cfg,
        &mut out,
    );
    out
}

/// Functions written by an item-level macro call, such as axum's
/// `top_level_handler_fn!(get, GET);`. Only a `macro_rules!` in the same
/// file whose body declares `fn $name` counts. The call's first identifier
/// is the function name, and the declaration points at it.
fn collect_macro_functions(
    path: &Path,
    text: &str,
    root: Node<'_>,
    crate_name: &str,
    module: &[String],
    cfg: &CfgContext,
    out: &mut Vec<Declaration>,
) {
    let mut templates: HashMap<&str, Option<(String, String)>> = HashMap::new();
    let mut cursor = root.walk();
    for item in root.named_children(&mut cursor) {
        // Tree-sitter wraps an item-level `mac!(...);` in an expression statement.
        let call = match item.kind() {
            "expression_statement" => match item.named_child(0) {
                Some(call) => call,
                None => continue,
            },
            _ => item,
        };
        if call.kind() != "macro_invocation" || cfg_disabled(item, text, cfg) {
            continue;
        }
        let Some(mac) = call
            .child_by_field_name("macro")
            .filter(|mac| mac.kind() == "identifier")
            .and_then(|mac| mac.utf8_text(text.as_bytes()).ok())
        else {
            continue;
        };
        let Some((variable, template)) = templates
            .entry(mac)
            .or_insert_with(|| macro_function_template(text, mac))
        else {
            continue;
        };
        let mut tokens_cursor = call.walk();
        let Some(tokens) = call
            .named_children(&mut tokens_cursor)
            .find(|child| child.kind() == "token_tree")
        else {
            continue;
        };
        let mut name_cursor = tokens.walk();
        let Some(name) = tokens
            .named_children(&mut name_cursor)
            .next()
            .filter(|name| name.kind() == "identifier")
        else {
            continue;
        };
        let Ok(name_text) = name.utf8_text(text.as_bytes()) else {
            continue;
        };
        let signature = template.replace(variable.as_str(), name_text);
        if signature.contains('$') {
            continue;
        }
        let prefix = signature.find("fn ").map_or("", |end| &signature[..end]);
        let point = name.start_position();
        out.push(Declaration {
            name: name_text.to_owned(),
            crate_name: crate_name.to_owned(),
            module: module.to_vec(),
            visibility: item_visibility(prefix, module),
            kind: ItemKind::Function,
            path: path.to_path_buf(),
            line: point.row as u32,
            column: utf16_column(text, name.start_byte()),
            scope_depth: 0,
            signature,
        });
    }
}

/// `("$name", "pub fn $name<H>(handler: H) -> MethodRouter")` for the
/// `macro_rules! mac` in `text` that declares a function named by a
/// macro variable.
fn macro_function_template(text: &str, mac: &str) -> Option<(String, String)> {
    let header = format!("macro_rules! {mac}");
    let start = text
        .match_indices(&header)
        .map(|(start, _)| start + header.len())
        .find(|end| !text[*end..].starts_with(is_ident))?;
    let rest = &text[start..];
    let open = rest.find('{')?;
    let mut depth = 0usize;
    let mut close = None;
    for (offset, character) in rest[open..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let body = &rest[open..close?];
    body.match_indices("fn $").find_map(|(at, _)| {
        let variable = &body[at + 3..];
        let length = 1 + variable[1..].find(|c: char| !is_ident(c))?;
        let after = variable[length..].trim_start();
        if length == 1 || !(after.starts_with('<') || after.starts_with('(')) {
            return None;
        }
        let line_start = body[..at].rfind('\n').map_or(0, |newline| newline + 1);
        let line = &body[line_start..];
        let line_end = line.find(['\n', '{']).unwrap_or(line.len());
        let signature = line[..line_end].trim().trim_end_matches("where").trim();
        Some((variable[..length].to_owned(), signature.to_owned()))
    })
}

fn collect_declarations(
    path: &Path,
    text: &str,
    node: Node<'_>,
    crate_name: &str,
    module: &[String],
    cfg: &CfgContext,
    out: &mut Vec<Declaration>,
) {
    collect_declaration(path, text, node, crate_name, module, cfg, out);
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let mut child_module = module.to_vec();
        if node.kind() == "mod_item"
            && child.kind() == "declaration_list"
            && let Some(name) = node.child_by_field_name("name")
            && let Ok(name) = name.utf8_text(text.as_bytes())
        {
            child_module.push(name.to_owned());
        }
        collect_declarations(path, text, child, crate_name, &child_module, cfg, out);
    }
}

fn parse_use_bindings(text: &str) -> Vec<UseBinding> {
    let Some(tree) = parse_tree(text) else {
        return Vec::new();
    };
    let mut bindings = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "use_declaration" {
            let source = node.utf8_text(text.as_bytes()).unwrap_or("");
            bindings.extend(use_bindings(source));
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    bindings
}

fn use_bindings(source: &str) -> Vec<UseBinding> {
    let source = source.trim();
    let public = source.starts_with("pub use ");
    let source = source
        .trim_start_matches("pub ")
        .trim_start_matches("use ")
        .trim_end_matches(';')
        .trim();
    let mut out = Vec::new();
    collect_use_bindings(source, "", public, &mut out);
    out
}

fn collect_use_bindings(source: &str, prefix: &str, public: bool, out: &mut Vec<UseBinding>) {
    for item in split_use_items(source) {
        let item = item.trim();
        if let Some(open) = item.find('{') {
            let before = item[..open].trim_end_matches("::").trim();
            let close = item.rfind('}').unwrap_or(item.len());
            let next_prefix = if before.is_empty() {
                prefix.to_owned()
            } else {
                format!("{prefix}{before}::")
            };
            collect_use_bindings(&item[open + 1..close], &next_prefix, public, out);
            continue;
        }
        let (path, alias) = item.rsplit_once(" as ").unwrap_or((item, ""));
        if path.trim_end().ends_with("::*") {
            let target: Vec<String> =
                format!("{prefix}{}", path.trim_end().trim_end_matches("::*"))
                    .split("::")
                    .map(str::trim)
                    .filter(|segment| !segment.is_empty())
                    .map(str::to_owned)
                    .collect();
            if !target.is_empty() && target.iter().all(|segment| is_ident_text(segment)) {
                out.push(UseBinding {
                    local: "*".to_owned(),
                    target,
                    glob: true,
                    public,
                    module: Vec::new(),
                });
            }
            continue;
        }
        let target: Vec<String> = format!("{prefix}{path}")
            .split("::")
            .map(str::trim)
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect();
        let local = if alias.is_empty() {
            target.last().map(String::as_str).unwrap_or("")
        } else {
            alias.trim()
        };
        if !target.is_empty()
            && target.iter().all(|segment| is_ident_text(segment))
            && is_ident_text(local)
        {
            out.push(UseBinding {
                local: local.to_owned(),
                target,
                glob: false,
                public,
                module: Vec::new(),
            });
        }
    }
}

fn split_use_items(source: &str) -> Vec<&str> {
    let mut depth = 0;
    let mut start = 0;
    let mut out = Vec::new();
    for (index, character) in source.char_indices() {
        match character {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&source[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    out.push(&source[start..]);
    out
}

fn collect_declaration(
    path: &Path,
    text: &str,
    node: Node<'_>,
    crate_name: &str,
    module: &[String],
    cfg: &CfgContext,
    out: &mut Vec<Declaration>,
) {
    if cfg_disabled(node, text, cfg) {
        return;
    }
    let kind = match node.kind() {
        "function_item" => ItemKind::Function,
        // `extern "C" { fn name(...); }` declares callable module items.
        // Trait method signatures share the node kind but are not.
        "function_signature_item"
            if node
                .parent()
                .and_then(|list| list.parent())
                .is_some_and(|block| block.kind() == "foreign_mod_item") =>
        {
            ItemKind::Function
        }
        "struct_item" => ItemKind::Class,
        "enum_item" => ItemKind::Enum,
        "trait_item" => ItemKind::Interface,
        "type_item" => ItemKind::Class,
        "const_item" | "static_item" => ItemKind::Constant,
        "mod_item" => ItemKind::Class,
        _ => return,
    };
    let Some(name) = node.child_by_field_name("name") else {
        return;
    };
    let name_text = name.utf8_text(text.as_bytes()).unwrap_or("");
    if name_text.is_empty() {
        return;
    }
    let point = name.start_position();
    let signature = text[node.start_byte()..node.end_byte()]
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    out.push(Declaration {
        name: name_text.to_owned(),
        crate_name: crate_name.to_owned(),
        module: module.to_vec(),
        visibility: item_visibility(&text[node.start_byte()..name.start_byte()], module),
        kind,
        path: path.to_path_buf(),
        line: point.row as u32,
        column: utf16_column(text, name.start_byte()),
        scope_depth: 0,
        signature,
    });
}

fn item_visibility(prefix: &str, module: &[String]) -> Visibility {
    let prefix = prefix.trim_start();
    if prefix.starts_with("pub ") || prefix == "pub" {
        return Visibility::Public;
    }
    let Some(scope) = prefix
        .strip_prefix("pub(")
        .and_then(|value| value.split_once(')'))
        .map(|(scope, _)| scope.trim())
    else {
        return Visibility::Private;
    };
    let target = match scope {
        "crate" => Some(Vec::new()),
        "self" => Some(module.to_vec()),
        "super" => Some(module[..module.len().saturating_sub(1)].to_vec()),
        _ => scope
            .strip_prefix("in ")
            .and_then(|path| visibility_scope(path.trim(), module)),
    };
    target
        .map(Visibility::Restricted)
        .unwrap_or(Visibility::Private)
}

fn visibility_scope(path: &str, module: &[String]) -> Option<Vec<String>> {
    let segments: Vec<_> = path
        .split("::")
        .filter(|segment| !segment.is_empty())
        .collect();
    let first = *segments.first()?;
    let mut scope = match first {
        "crate" => Vec::new(),
        "self" => module.to_vec(),
        "super" => {
            let parents = segments
                .iter()
                .take_while(|segment| **segment == "super")
                .count();
            module[..module.len().saturating_sub(parents)].to_vec()
        }
        _ => return None,
    };
    let skip = if first == "super" {
        segments
            .iter()
            .take_while(|segment| **segment == "super")
            .count()
    } else {
        1
    };
    scope.extend(segments[skip..].iter().map(|segment| (*segment).to_owned()));
    Some(scope)
}

fn module_is_within(module: &[String], scope: &[String]) -> bool {
    module.starts_with(scope)
}

fn cfg_disabled(node: Node<'_>, text: &str, cfg: &CfgContext) -> bool {
    let node_attributes = text[node.start_byte()..node.end_byte()]
        .lines()
        .take_while(|line| {
            let line = line.trim();
            line.is_empty() || (line.starts_with("#[") && line.ends_with(']'))
        });
    if node_attributes
        .filter_map(cfg_attribute_expression)
        .any(|expression| !cfg_matches(expression, cfg))
    {
        return true;
    }

    // Older Tree-sitter Rust grammars exclude outer attributes from `mod_item`.
    text[..node.start_byte()]
        .lines()
        .rev()
        .take_while(|line| {
            let line = line.trim();
            line.is_empty() || (line.starts_with("#[") && line.ends_with(']'))
        })
        .filter_map(cfg_attribute_expression)
        .any(|expression| !cfg_matches(expression, cfg))
}

fn cfg_attribute_expression(line: &str) -> Option<&str> {
    line.trim()
        .strip_prefix("#[cfg(")
        .and_then(|value| value.strip_suffix(")]"))
}

fn cfg_matches(expression: &str, cfg: &CfgContext) -> bool {
    let expression = expression.trim();
    if let Some(arguments) = cfg_call_arguments(expression, "any") {
        return split_cfg_arguments(arguments)
            .iter()
            .any(|arg| cfg_matches(arg, cfg));
    }
    if let Some(arguments) = cfg_call_arguments(expression, "all") {
        return split_cfg_arguments(arguments)
            .iter()
            .all(|arg| cfg_matches(arg, cfg));
    }
    if let Some(arguments) = cfg_call_arguments(expression, "not") {
        let arguments = split_cfg_arguments(arguments);
        return arguments.len() == 1 && !cfg_matches(arguments[0], cfg);
    }
    match expression {
        "unix" | "macos" => true,
        "test" | "false" => false,
        _ => expression.split_once('=').is_some_and(|(key, value)| {
            match (key.trim(), value.trim().trim_matches('"')) {
                ("target_os", "macos") => true,
                ("target_arch", "aarch64") => true,
                ("feature", feature) => cfg.features.contains(feature),
                _ => false,
            }
        }),
    }
}

fn cfg_call_arguments<'a>(expression: &'a str, name: &str) -> Option<&'a str> {
    expression
        .strip_prefix(name)
        .and_then(|arguments| arguments.strip_prefix('('))
        .and_then(|arguments| arguments.strip_suffix(')'))
}

fn split_cfg_arguments(source: &str) -> Vec<&str> {
    let mut depth = 0;
    let mut quoted = false;
    let mut start = 0;
    let mut out = Vec::new();
    for (index, character) in source.char_indices() {
        match character {
            '"' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth -= 1,
            ',' if !quoted && depth == 0 => {
                out.push(source[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    if !source[start..].trim().is_empty() {
        out.push(source[start..].trim());
    }
    out
}

fn local_declaration(path: &Path, text: &str, name: &str, at: usize) -> Option<Declaration> {
    local_declarations(path, text, at)
        .into_iter()
        .filter(|binding| binding.name == name)
        .max_by_key(|binding| (binding.scope_depth, binding.line, binding.column))
}

fn local_declarations(path: &Path, text: &str, at: usize) -> Vec<Declaration> {
    let Some(tree) = parse_tree(text) else {
        return Vec::new();
    };
    let mut bindings = Vec::new();
    collect_local_bindings(path, text, tree.root_node(), at, 0, &mut bindings);
    bindings
}

fn collect_local_bindings(
    path: &Path,
    text: &str,
    node: Node<'_>,
    at: usize,
    depth: usize,
    out: &mut Vec<Declaration>,
) {
    if node.start_byte() > at || node.end_byte() < at {
        return;
    }
    if node.kind() == "function_item"
        && let Some(parameters) = node.child_by_field_name("parameters")
    {
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            collect_pattern_bindings(path, text, parameter, depth, out);
        }
    }
    if node.kind() == "parameters" {
        let mut cursor = node.walk();
        for parameter in node.named_children(&mut cursor) {
            collect_pattern_bindings(path, text, parameter, depth, out);
        }
    }
    if node.kind() == "closure_parameters" {
        let mut cursor = node.walk();
        for parameter in node.named_children(&mut cursor) {
            collect_pattern_bindings(path, text, parameter, depth, out);
        }
    }
    if node.kind() == "let_declaration"
        && node.start_byte() < at
        && let Some(pattern) = node
            .child_by_field_name("pattern")
            .or_else(|| node.named_child(0))
    {
        collect_pattern_bindings(path, text, pattern, depth, out);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if node.kind() == "block"
            && child.kind() == "let_declaration"
            && child.start_byte() < at
            && let Some(pattern) = child
                .child_by_field_name("pattern")
                .or_else(|| child.named_child(0))
        {
            collect_pattern_bindings(path, text, pattern, depth + 1, out);
        }
        if child.start_byte() <= at && child.end_byte() >= at {
            collect_local_bindings(path, text, child, at, depth + 1, out);
        }
    }
}

fn collect_pattern_bindings(
    path: &Path,
    text: &str,
    node: Node<'_>,
    depth: usize,
    out: &mut Vec<Declaration>,
) {
    if node.kind() == "identifier" {
        let name = node.utf8_text(text.as_bytes()).unwrap_or("");
        if !name.is_empty() {
            let point = node.start_position();
            out.push(Declaration {
                name: name.to_owned(),
                crate_name: String::new(),
                module: Vec::new(),
                visibility: Visibility::Private,
                kind: ItemKind::Variable,
                path: path.to_path_buf(),
                line: point.row as u32,
                column: utf16_column(text, node.start_byte()),
                scope_depth: depth,
                signature: format!("let {name}"),
            });
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_pattern_bindings(path, text, child, depth, out);
    }
}

fn collect_errors(node: Node<'_>, text: &str, out: &mut Vec<Problem>) {
    if out.len() >= MAX_PROBLEMS || !node.has_error() {
        return;
    }
    if node.is_error() || node.is_missing() {
        let start = node.start_position();
        let end = node.end_position();
        out.push(Problem {
            start_line: start.row as u32,
            start_column: utf16_column(text, node.start_byte()),
            end_line: end.row as u32,
            end_column: utf16_column(text, node.end_byte()),
            severity: Severity::Error,
            message: "Syntax error".to_owned(),
            unnecessary: false,
        });
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_errors(child, text, out);
    }
}

/// Check conventional paths and literal `#[path = "..."]` overrides for
/// out-of-line modules without invoking Cargo or the compiler.
fn module_problems(path: &Path, text: &str, cfg: &CfgContext) -> Vec<Problem> {
    let Some(tree) = parse_tree(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "mod_item"
            && !text[node.start_byte()..node.end_byte()]
                .trim_end()
                .ends_with('}')
            && !cfg_disabled(node, text, cfg)
            && let Some(name) = node.child_by_field_name("name")
            && let Ok(name_text) = name.utf8_text(text.as_bytes())
        {
            let found = module_path_override(node, text)
                .map(|override_path| module_directory(path).join(override_path))
                .filter(|candidate| candidate.is_file())
                .map(|candidate| vec![candidate])
                .unwrap_or_else(|| {
                    let parent = module_directory(path);
                    [
                        parent.join(format!("{name_text}.rs")),
                        parent.join(name_text).join("mod.rs"),
                    ]
                    .into_iter()
                    .filter(|candidate| candidate.is_file())
                    .collect()
                });
            let message = if found.is_empty() {
                Some(format!("module `{name_text}` not found"))
            } else if found.len() > 1 {
                Some(format!(
                    "module `{name_text}` found at both conventional paths"
                ))
            } else if found[0] == path {
                Some(format!("module `{name_text}` creates a cyclic module path"))
            } else {
                None
            };
            if let Some(message) = message {
                let point = name.start_position();
                out.push(Problem {
                    start_line: point.row as u32,
                    start_column: utf16_column(text, name.start_byte()),
                    end_line: point.row as u32,
                    end_column: utf16_column(text, name.end_byte()),
                    severity: Severity::Error,
                    message,
                    unnecessary: false,
                });
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    out
}

fn module_path_override<'a>(node: Node<'_>, text: &'a str) -> Option<&'a str> {
    let prefix = &text[..node.start_byte()];
    if let Some(value) = prefix
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .and_then(|line| line.trim().strip_prefix("#[path = "))
    {
        let value = value.trim_end_matches(']').trim().trim_matches('"');
        return (!value.is_empty()).then_some(value);
    }
    let mut previous = node.prev_named_sibling();
    while let Some(attribute) = previous {
        if attribute.kind() == "attribute_item" {
            let source = attribute.utf8_text(text.as_bytes()).ok()?.trim();
            if let Some(value) = source
                .strip_prefix("#[path")
                .and_then(|value| value.strip_prefix('='))
            {
                let value = value.trim().trim_end_matches(']').trim().trim_matches('"');
                return (!value.is_empty()).then_some(value);
            }
        }
        previous = attribute.prev_named_sibling();
    }
    None
}

fn module_directory(path: &Path) -> PathBuf {
    if matches!(
        path.file_stem().and_then(|stem| stem.to_str()),
        Some("lib" | "main" | "mod")
    ) {
        path.parent().unwrap_or(path).to_path_buf()
    } else {
        path.parent()
            .unwrap_or(path)
            .join(path.file_stem().unwrap_or_default())
    }
}

#[allow(dead_code)]
fn collect_unresolved(
    engine: &RustEngine,
    path: &Path,
    text: &str,
    node: Node<'_>,
    local_bindings: &HashSet<String>,
    file_uses: &[UseBinding],
    out: &mut Vec<Problem>,
) {
    if out.len() >= MAX_PROBLEMS || node.has_error() {
        return;
    }
    if node.kind() == "identifier"
        && identifier_is_reference(node)
        && is_call_target(node)
        && !is_attribute_argument(node)
        && let Ok(name) = node.utf8_text(text.as_bytes())
        && !is_prelude_name(name)
        // `Self(...)` constructs the impl's own tuple struct.
        && name != "Self"
        && !is_field_shorthand(node)
        && !is_lifetime_name(node, text)
        && !is_member_name(node, text)
        && !is_closure_binding(node, text)
        && !file_uses.iter().any(|binding| binding.local == name)
        && !engine
            .use_bindings(path)
            .iter()
            .any(|binding| binding.local == name)
        && !is_match_binding(node, text)
        && !local_bindings.contains(name)
        && engine.resolve_nonlocal(path, name).is_empty()
    {
        let point = node.start_position();
        out.push(Problem {
            start_line: point.row as u32,
            start_column: utf16_column(text, node.start_byte()),
            end_line: point.row as u32,
            end_column: utf16_column(text, node.end_byte()),
            severity: Severity::Error,
            message: format!("cannot resolve `{name}`"),
            unnecessary: false,
        });
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_unresolved(engine, path, text, child, local_bindings, file_uses, out);
    }
}

#[allow(dead_code)]
fn all_local_binding_names(node: Node<'_>, text: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    collect_all_local_binding_names(node, text, &mut names);
    names
}

#[allow(dead_code)]
fn collect_all_local_binding_names(node: Node<'_>, text: &str, names: &mut HashSet<String>) {
    match node.kind() {
        "let_declaration" => {
            if let Some(pattern) = node
                .child_by_field_name("pattern")
                .or_else(|| node.named_child(0))
            {
                collect_pattern_binding_names(pattern, text, names);
            }
        }
        "match_arm" => {
            if let Some(pattern) = node
                .child_by_field_name("pattern")
                .or_else(|| node.named_child(0))
            {
                collect_pattern_binding_names(pattern, text, names);
            }
        }
        "parameters" | "closure_parameters" => {
            let mut cursor = node.walk();
            for parameter in node.named_children(&mut cursor) {
                collect_pattern_binding_names(parameter, text, names);
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_all_local_binding_names(child, text, names);
    }
}

#[allow(dead_code)]
fn collect_pattern_binding_names(node: Node<'_>, text: &str, names: &mut HashSet<String>) {
    if node.kind() == "identifier" {
        if let Ok(name) = node.utf8_text(text.as_bytes()) {
            names.insert(name.to_owned());
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_pattern_binding_names(child, text, names);
    }
}

#[allow(dead_code)]
fn is_match_binding(node: Node<'_>, text: &str) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "match_arm" {
            let arm = &text[parent.start_byte()..parent.end_byte()];
            return current.start_byte() - parent.start_byte() < arm.find("=>").unwrap_or(0);
        }
        current = parent;
    }
    false
}

/// The native resolver does not model every Rust expression and pattern form.
/// Restrict unresolved diagnostics to direct calls, where a missing name is
/// actionable, instead of flooding Problems with pattern and metadata names.
#[allow(dead_code)]
fn is_call_target(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    parent.kind() == "call_expression"
        && parent
            .child_by_field_name("function")
            .map(|function| function.id())
            == Some(node.id())
}

/// Names supplied by Rust's standard prelude have no local declaration for the
/// lightweight resolver to find. Keeping this list to prelude items preserves
/// unresolved diagnostics for ordinary project symbols.
#[allow(dead_code)]
fn is_prelude_name(name: &str) -> bool {
    matches!(
        name,
        "Some"
            | "None"
            | "Ok"
            | "Err"
            | "Option"
            | "Result"
            | "String"
            | "Vec"
            | "Box"
            | "Default"
            | "Clone"
            | "Copy"
            | "Drop"
            | "Eq"
            | "Ord"
            | "PartialEq"
            | "PartialOrd"
            | "Send"
            | "Sized"
            | "Sync"
            | "assert"
            | "assert_eq"
            | "assert_ne"
            | "debug_assert"
            | "debug_assert_eq"
            | "debug_assert_ne"
            | "panic"
            | "todo"
            | "unimplemented"
            | "unreachable"
            | "format"
            | "println"
            | "eprintln"
            | "print"
            | "eprint"
            | "vec"
            | "drop"
    )
}

/// `Entry { uri }` reuses the surrounding `uri` binding; Tree-sitter exposes
/// its only identifier as a field initializer, not as a local declaration.
#[allow(dead_code)]
fn is_field_shorthand(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    parent.kind() == "field_initializer" && parent.named_child_count() == 1
}

#[allow(dead_code)]
fn is_lifetime_name(node: Node<'_>, text: &str) -> bool {
    node.start_byte() > 0 && text.as_bytes().get(node.start_byte() - 1) == Some(&b'\'')
}

/// Member names are resolved from a receiver type, which is intentionally
/// outside this name-only resolver. Their receiver remains checked normally.
#[allow(dead_code)]
fn is_member_name(node: Node<'_>, text: &str) -> bool {
    text.as_bytes().get(node.start_byte().saturating_sub(1)) == Some(&b'.')
}

/// Closure parameters are lexical bindings. The body identifier has a normal
/// expression parent, so find the enclosing closure and compare its parameters.
#[allow(dead_code)]
fn is_closure_binding(node: Node<'_>, text: &str) -> bool {
    let Ok(name) = node.utf8_text(text.as_bytes()) else {
        return false;
    };
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "closure_expression" {
            let mut cursor = parent.walk();
            for child in parent.named_children(&mut cursor) {
                if child.kind() == "closure_parameters" && contains_identifier(child, text, name) {
                    return true;
                }
            }
            return false;
        }
        current = parent.parent();
    }
    false
}

#[allow(dead_code)]
fn contains_identifier(node: Node<'_>, text: &str, name: &str) -> bool {
    if node.kind() == "identifier" && node.utf8_text(text.as_bytes()).ok() == Some(name) {
        return true;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .any(|child| contains_identifier(child, text, name))
}

/// Attribute names and arguments may be procedural macros or compiler-builtins.
/// The local resolver cannot expand them, so reporting them as unresolved floods
/// the editor with false positives for common `#[derive(...)]` declarations.
#[allow(dead_code)]
fn is_attribute_argument(node: Node<'_>) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "attribute_item" {
            return true;
        }
        current = parent.parent();
    }
    false
}

#[allow(dead_code)]
fn identifier_is_reference(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    !matches!(
        parent.kind(),
        "function_item"
            | "struct_item"
            | "enum_item"
            | "trait_item"
            | "type_item"
            | "mod_item"
            | "field_declaration"
            | "parameter"
            | "closure_parameters"
            | "let_declaration"
            | "use_as_clause"
            | "scoped_identifier"
    ) && parent.child_by_field_name("name").map(|name| name.id()) != Some(node.id())
}

fn identifier_count(text: &str, name: &str) -> usize {
    let Some(tree) = parse_tree(text) else {
        return 0;
    };
    let mut count = 0;
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier" && node.utf8_text(text.as_bytes()).ok() == Some(name) {
            count += 1;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    count
}

fn identifier_at(text: &str, line: u32, column: u32) -> Option<String> {
    identifier_range(text, line, column).map(|range| range.0)
}

fn identifier_range(text: &str, line: u32, column: u32) -> Option<(String, u32, u32)> {
    let start = line_start(text, line)?;
    let end = text[start..]
        .find('\n')
        .map_or(text.len(), |offset| start + offset);
    let byte = byte_at_column(&text[start..end], column) + start;
    let left = text[..byte]
        .char_indices()
        .rev()
        .find(|(_, c)| !is_ident(*c))
        .map_or(0, |(at, c)| at + c.len_utf8());
    let right = text[byte..]
        .char_indices()
        .find(|(_, c)| !is_ident(*c))
        .map_or(text.len(), |(at, _)| byte + at);
    (left < right).then(|| {
        (
            text[left..right].to_owned(),
            utf16_column(text, left),
            utf16_column(text, right),
        )
    })
}

fn word_before(text: &str, line: u32, column: u32) -> &str {
    let Some(start) = line_start(text, line) else {
        return "";
    };
    let end = text[start..]
        .find('\n')
        .map_or(text.len(), |offset| start + offset);
    let byte = start + byte_at_column(&text[start..end], column);
    let word_start = text[..byte]
        .char_indices()
        .rev()
        .find(|(_, c)| !is_ident(*c))
        .map_or(0, |(at, c)| at + c.len_utf8());
    &text[word_start..byte]
}

fn member_access_before(text: &str, line: u32, column: u32) -> bool {
    let byte = cursor_byte(text, line, column);
    text[..byte].trim_end().ends_with('.')
        || text[..byte].trim_end().ends_with("::")
        || text[..byte]
            .rsplit_once("::")
            .or_else(|| text[..byte].rsplit_once('.'))
            .is_some_and(|(_, right)| right.chars().all(is_ident))
}

fn receiver_type(before: &str, receiver: &str) -> Option<String> {
    let binding = format!("let {receiver}:");
    if let Some((_, after)) = before.rsplit_once(&binding) {
        return after
            .trim_start()
            .split(|character: char| !is_ident(character))
            .next()
            .filter(|ty| !ty.is_empty())
            .map(str::to_owned);
    }
    let construction = format!("let {receiver} = ");
    before
        .rsplit_once(&construction)
        .and_then(|(_, after)| {
            after
                .split(|character: char| !is_ident(character))
                .next()
                .filter(|ty| !ty.is_empty())
                .map(str::to_owned)
        })
        .or_else(|| parameter_type(before, receiver))
}

fn local_call_binding<'tree>(
    root: Node<'tree>,
    text: &str,
    receiver: &str,
    before: usize,
) -> Option<Node<'tree>> {
    let mut found = None;
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.start_byte() >= before {
            continue;
        }
        // A `let` that is still open at `before` is the one being read, as in
        // `let a = a.next();`, so its value cannot type its own receiver.
        if node.kind() == "let_declaration" && node.end_byte() > before {
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
            continue;
        }
        if node.kind() == "let_declaration"
            && let Some(pattern) = node.child_by_field_name("pattern")
            && pattern
                .utf8_text(text.as_bytes())
                .ok()
                .map(|written| written.strip_prefix("mut ").unwrap_or(written).trim())
                == Some(receiver)
            && let Some(value) = node.child_by_field_name("value")
            && value.kind() == "call_expression"
        {
            found = Some(value);
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    found
}

/// Element type of the loop variable in `for receiver in <collection>` when
/// the cursor is inside that loop and the collection's type is written out
/// (a parameter, a typed `let`, or a `self.field`).
fn for_binding_type(root: Node<'_>, text: &str, receiver: &str, byte: usize) -> Option<String> {
    let mut ancestors = Vec::new();
    let mut node = root.descendant_for_byte_range(byte, byte);
    while let Some(current) = node {
        ancestors.push(current);
        node = current.parent();
    }
    let mut collection = ancestors.iter().find_map(|node| match node.kind() {
        "for_expression"
            if node.child_by_field_name("pattern").is_some_and(|pattern| {
                let written = pattern.utf8_text(text.as_bytes()).unwrap_or_default();
                written.strip_prefix("mut ").unwrap_or(written).trim() == receiver
            }) && node
                .child_by_field_name("body")
                .is_some_and(|body| body.start_byte() <= byte) =>
        {
            node.child_by_field_name("value")
        }
        "closure_expression" => closure_source(*node, text, receiver, byte),
        _ => None,
    })?;
    loop {
        match collection.kind() {
            "reference_expression" | "parenthesized_expression" => {
                collection = collection
                    .child_by_field_name("value")
                    .or_else(|| collection.named_child(0))?;
            }
            "call_expression" => {
                let function = collection.child_by_field_name("function")?;
                let method = function
                    .child_by_field_name("field")?
                    .utf8_text(text.as_bytes())
                    .ok()?;
                if !matches!(
                    method,
                    "iter"
                        | "iter_mut"
                        | "into_iter"
                        | "rev"
                        | "skip"
                        | "take"
                        | "by_ref"
                        | "filter"
                ) {
                    return None;
                }
                collection = function.child_by_field_name("value")?;
            }
            _ => break,
        }
    }
    let declared = match collection.kind() {
        "identifier" => {
            let name = collection.utf8_text(text.as_bytes()).ok()?;
            declared_local_type(&ancestors, text, name, byte)?
        }
        "field_expression" => {
            let owner = collection.child_by_field_name("value")?;
            if owner.utf8_text(text.as_bytes()).ok()? != "self" {
                return None;
            }
            let field = collection
                .child_by_field_name("field")?
                .utf8_text(text.as_bytes())
                .ok()?;
            declared_self_field_type(root, &ancestors, text, field)?
        }
        _ => return None,
    };
    element_type(&declared)
}

/// For `<source>.map(|receiver| ...)` and similar iterator methods, the
/// expression the closure parameter's elements come from.
fn closure_source<'tree>(
    closure: Node<'tree>,
    text: &str,
    receiver: &str,
    byte: usize,
) -> Option<Node<'tree>> {
    if closure.child_by_field_name("body")?.start_byte() > byte {
        return None;
    }
    let parameters = closure.child_by_field_name("parameters")?;
    let mut cursor = parameters.walk();
    let mut named = parameters.named_children(&mut cursor);
    let only = named.next().filter(|_| named.next().is_none())?;
    if only.kind() != "identifier" || only.utf8_text(text.as_bytes()).ok()? != receiver {
        return None;
    }
    let arguments = closure.parent().filter(|node| node.kind() == "arguments")?;
    let call = arguments
        .parent()
        .filter(|node| node.kind() == "call_expression")?;
    let function = call.child_by_field_name("function")?;
    let method = function
        .child_by_field_name("field")?
        .utf8_text(text.as_bytes())
        .ok()?;
    matches!(
        method,
        "map"
            | "filter"
            | "any"
            | "all"
            | "find"
            | "position"
            | "for_each"
            | "filter_map"
            | "flat_map"
            | "take_while"
            | "skip_while"
            | "inspect"
            | "max_by_key"
            | "min_by_key"
    )
    .then(|| function.child_by_field_name("value"))
    .flatten()
}

fn declared_local_type(
    ancestors: &[Node<'_>],
    text: &str,
    name: &str,
    byte: usize,
) -> Option<String> {
    let matches_name = |pattern: Node<'_>| {
        let written = pattern.utf8_text(text.as_bytes()).unwrap_or_default();
        written.strip_prefix("mut ").unwrap_or(written).trim() == name
    };
    for ancestor in ancestors {
        match ancestor.kind() {
            "block" => {
                let mut cursor = ancestor.walk();
                let found = ancestor
                    .named_children(&mut cursor)
                    .filter(|child| child.kind() == "let_declaration" && child.end_byte() <= byte)
                    .filter(|child| {
                        child
                            .child_by_field_name("pattern")
                            .is_some_and(matches_name)
                    })
                    .filter_map(|child| child.child_by_field_name("type"))
                    .last();
                if let Some(ty) = found {
                    return ty.utf8_text(text.as_bytes()).ok().map(str::to_owned);
                }
            }
            "function_item" => {
                let parameters = ancestor.child_by_field_name("parameters")?;
                let mut cursor = parameters.walk();
                let found = parameters
                    .named_children(&mut cursor)
                    .filter(|parameter| parameter.kind() == "parameter")
                    .find(|parameter| {
                        parameter
                            .child_by_field_name("pattern")
                            .is_some_and(matches_name)
                    })
                    .and_then(|parameter| parameter.child_by_field_name("type"));
                return found.and_then(|ty| ty.utf8_text(text.as_bytes()).ok().map(str::to_owned));
            }
            _ => {}
        }
    }
    None
}

fn declared_self_field_type(
    root: Node<'_>,
    ancestors: &[Node<'_>],
    text: &str,
    field: &str,
) -> Option<String> {
    let owner = ancestors
        .iter()
        .find(|node| node.kind() == "impl_item")?
        .child_by_field_name("type")?
        .utf8_text(text.as_bytes())
        .ok()
        .and_then(named_type)?;
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "struct_item"
            && node
                .child_by_field_name("name")
                .and_then(|name| name.utf8_text(text.as_bytes()).ok())
                == Some(owner.as_str())
            && let Some(body) = node.child_by_field_name("body")
        {
            let mut cursor = body.walk();
            return body
                .named_children(&mut cursor)
                .filter(|child| child.kind() == "field_declaration")
                .find(|child| {
                    child
                        .child_by_field_name("name")
                        .and_then(|name| name.utf8_text(text.as_bytes()).ok())
                        == Some(field)
                })
                .and_then(|child| child.child_by_field_name("type"))
                .and_then(|ty| ty.utf8_text(text.as_bytes()).ok().map(str::to_owned));
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    None
}

/// `Vec<Box<dyn Migration>>` and `&[Migration]` both give `Migration`.
fn element_type(ty: &str) -> Option<String> {
    let ty = ty
        .trim()
        .trim_start_matches('&')
        .trim_start_matches("mut ")
        .trim();
    let mut inner = if let Some(slice) = ty.strip_prefix('[') {
        slice.split([';', ']']).next()?.trim().to_owned()
    } else {
        generic_argument(ty)?
    };
    loop {
        let trimmed = inner
            .trim()
            .trim_start_matches('&')
            .trim_start_matches("mut ")
            .trim_start_matches("dyn ")
            .trim_start_matches("impl ")
            .trim();
        let wrapped = ["Box", "Arc", "Rc"].iter().find_map(|wrapper| {
            trimmed
                .strip_prefix(wrapper)
                .filter(|rest| rest.starts_with('<'))
                .and_then(|_| generic_argument(trimmed))
        });
        match wrapped {
            Some(next) => inner = next,
            None => return named_type(trimmed),
        }
    }
}

/// First top-level generic argument of `Name<A, B>`.
fn generic_argument(ty: &str) -> Option<String> {
    let open = ty.find('<')?;
    let mut depth = 0usize;
    let mut start = open + 1;
    for (offset, character) in ty.char_indices().skip_while(|(offset, _)| *offset < open) {
        match character {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(ty[start..offset].trim().to_owned());
                }
            }
            ',' if depth == 1 => return Some(ty[start..offset].trim().to_owned()),
            _ => {}
        }
        if depth == 1 && character == '<' {
            start = offset + 1;
        }
    }
    None
}

fn named_type(ty: &str) -> Option<String> {
    ty.trim()
        .trim_start_matches('&')
        .trim_start_matches("mut ")
        .rsplit("::")
        .next()?
        .split(|character: char| !is_ident(character))
        .find(|segment| !segment.is_empty())
        .map(str::to_owned)
}

fn std_member_access(text: &str, byte: usize, name: &str) -> Option<(String, usize)> {
    let before = text.get(..byte)?;
    let word_start = before
        .char_indices()
        .rev()
        .find(|(_, character)| !is_ident(*character))
        .map_or(0, |(offset, character)| offset + character.len_utf8());
    let prefix = text.get(..word_start)?;
    let marker = prefix.rfind("::")?;
    let receiver = prefix[..marker]
        .rsplit(|character: char| !is_ident(character))
        .next()?;
    (STD_TYPES.contains(&receiver) && text[word_start..].starts_with(name))
        .then(|| (receiver.to_owned(), marker))
}

fn parameter_type(before: &str, receiver: &str) -> Option<String> {
    let function = before.rsplit_once("fn ")?.1;
    let parameters = function.split_once('(')?.1.split_once(')')?.0;
    parameters.split(',').find_map(|parameter| {
        let (name, ty) = parameter.trim().split_once(':')?;
        (name.trim().trim_start_matches("mut ") == receiver)
            .then(|| {
                ty.trim()
                    .trim_start_matches('&')
                    .trim_start_matches("mut ")
                    .split(|character: char| !is_ident(character))
                    .next()
                    .unwrap_or("")
                    .to_owned()
            })
            .filter(|ty| !ty.is_empty())
    })
}

fn struct_fields(text: &str, ty: &str) -> Vec<Completion> {
    let Some((_, rest)) = text.split_once(&format!("struct {ty}")) else {
        return Vec::new();
    };
    let Some(body) = rest
        .split_once('{')
        .and_then(|(_, after)| after.split_once('}').map(|(body, _)| body))
    else {
        return Vec::new();
    };
    body.lines()
        .filter_map(|line| {
            line.trim()
                .split_once(':')
                .map(|(name, value)| completion(name.trim(), value.trim(), ItemKind::Variable))
        })
        .collect()
}

fn impl_methods(text: &str, ty: &str) -> Vec<Completion> {
    let Some((_, rest)) = text.split_once(&format!("impl {ty}")) else {
        return Vec::new();
    };
    let Some(body) = rest
        .split_once('{')
        .and_then(|(_, after)| after.split_once('}').map(|(body, _)| body))
    else {
        return Vec::new();
    };
    body.lines()
        .filter_map(|line| {
            let line = line.trim();
            let signature = line.strip_prefix("pub ").unwrap_or(line);
            let signature = signature
                .strip_prefix("const ")
                .or_else(|| signature.strip_prefix("unsafe "))
                .or_else(|| signature.strip_prefix("async "))
                .unwrap_or(signature);
            let name = signature.strip_prefix("fn ")?.split('(').next()?;
            Some(completion(name, line, ItemKind::Function))
        })
        .collect()
}

fn std_member_completions(ty: &str) -> Vec<Completion> {
    let Some(path) = std_type_source(ty) else {
        return Vec::new();
    };
    let Ok(source) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut completions = impl_methods(&source, ty);
    completions.extend(trait_associated_functions(&source, ty));
    completions
}

fn trait_associated_functions(text: &str, ty: &str) -> Vec<Completion> {
    let Some(tree) = parse_tree(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "impl_item"
            && node.child_by_field_name("trait").is_some()
            && node
                .child_by_field_name("type")
                .and_then(|node| node.utf8_text(text.as_bytes()).ok())
                .is_some_and(|implemented| implemented.trim() == ty)
        {
            let mut descendants = vec![node];
            while let Some(descendant) = descendants.pop() {
                if descendant.kind() != "function_item" {
                    let mut cursor = descendant.walk();
                    descendants.extend(descendant.named_children(&mut cursor));
                    continue;
                }
                let Some(name) = descendant
                    .child_by_field_name("name")
                    .and_then(|name| name.utf8_text(text.as_bytes()).ok())
                else {
                    continue;
                };
                let signature = text[descendant.start_byte()..descendant.end_byte()]
                    .lines()
                    .next()
                    .unwrap_or(name);
                out.push(completion(name, signature, ItemKind::Function));
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    out
}

fn std_member_source(ty: &str, name: &str) -> Option<PathBuf> {
    let path = std_type_source(ty)?;
    let source = std::fs::read_to_string(&path).ok()?;
    member_declarations(&path, &source, ty, name)
        .into_iter()
        .next()
        .map(|_| path)
}

fn std_prelude_trait_method(name: &str) -> Option<Declaration> {
    let (trait_name, relative) = match name {
        "into" => ("Into", "core/src/convert/mod.rs"),
        _ => return None,
    };
    let path = rust_source_file(relative)?;
    let source = std::fs::read_to_string(&path).ok()?;
    let tree = parse_tree(&source)?;
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "trait_item"
            && node
                .child_by_field_name("name")
                .and_then(|node| node.utf8_text(source.as_bytes()).ok())
                == Some(trait_name)
        {
            let mut descendants = vec![node];
            while let Some(descendant) = descendants.pop() {
                if descendant.kind() == "function_signature_item"
                    && let Some(method) = descendant.child_by_field_name("name")
                    && method.utf8_text(source.as_bytes()).ok() == Some(name)
                {
                    let point = method.start_position();
                    return Some(Declaration {
                        name: name.to_owned(),
                        crate_name: "core".to_owned(),
                        module: vec!["convert".to_owned()],
                        visibility: Visibility::Public,
                        kind: ItemKind::Function,
                        path,
                        line: point.row as u32,
                        column: utf16_column(&source, method.start_byte()),
                        scope_depth: 0,
                        signature: descendant
                            .utf8_text(source.as_bytes())
                            .unwrap_or(name)
                            .to_owned(),
                    });
                }
                let mut cursor = descendant.walk();
                descendants.extend(descendant.named_children(&mut cursor));
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    None
}

fn rust_source_file(relative: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let toolchains = PathBuf::from(home).join(".rustup/toolchains");
    std::fs::read_dir(toolchains)
        .ok()?
        .flatten()
        .map(|entry| {
            entry
                .path()
                .join("lib/rustlib/src/rust/library")
                .join(relative)
        })
        .find(|path| path.is_file())
}

fn std_type_declaration(path: &Path, ty: &str) -> Option<Declaration> {
    let source = std::fs::read_to_string(path).ok()?;
    let tree = parse_tree(&source)?;
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "struct_item" | "enum_item")
            && node
                .child_by_field_name("name")
                .and_then(|name| name.utf8_text(source.as_bytes()).ok())
                == Some(ty)
        {
            return Some(std_type_target(
                path,
                ty,
                node.start_position().row,
                source[node.start_byte()..node.end_byte()]
                    .lines()
                    .next()
                    .unwrap_or(ty),
            ));
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    source.lines().enumerate().find_map(|(line, signature)| {
        let declaration = signature.trim_start();
        ["pub struct ", "pub enum "].into_iter().find_map(|prefix| {
            declaration
                .strip_prefix(prefix)
                .filter(|rest| {
                    rest.strip_prefix(ty)
                        .is_some_and(|tail| tail.starts_with(['<', ' ', '{']))
                })
                .map(|_| std_type_target(path, ty, line, signature))
        })
    })
}

fn std_type_target(path: &Path, ty: &str, line: usize, signature: &str) -> Declaration {
    Declaration {
        name: ty.to_owned(),
        crate_name: String::new(),
        module: Vec::new(),
        visibility: Visibility::Public,
        kind: ItemKind::Class,
        path: path.to_path_buf(),
        line: line as u32,
        column: signature.find(ty).unwrap_or(0) as u32,
        scope_depth: 0,
        signature: signature.to_owned(),
    }
}

fn std_type_source(ty: &str) -> Option<PathBuf> {
    let relative = match ty {
        "String" => "alloc/src/string.rs",
        "Vec" => "alloc/src/vec/mod.rs",
        "Option" => "core/src/option.rs",
        "Result" => "core/src/result.rs",
        "Box" => "alloc/src/boxed.rs",
        "Rc" => "alloc/src/rc.rs",
        "Arc" => "alloc/src/sync.rs",
        "HashMap" => "std/src/collections/hash/map.rs",
        "HashSet" => "std/src/collections/hash/set.rs",
        _ => return None,
    };
    rust_source_file(relative)
}

fn member_declarations(path: &Path, text: &str, ty: &str, name: &str) -> Vec<Declaration> {
    let Some(tree) = parse_tree(text) else {
        return Vec::new();
    };
    let mut members = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "field_declaration" => {
                if struct_for_member(node, text).as_deref() != Some(ty) {
                    continue;
                }
                push_member_declaration(path, text, node, name, ItemKind::Variable, &mut members);
            }
            "function_item" | "function_signature_item"
                if impl_for_member(node, text).as_deref() == Some(ty) =>
            {
                push_member_declaration(path, text, node, name, ItemKind::Function, &mut members);
            }
            _ => {}
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    members.extend(struct_field_declarations(path, text, ty, name));
    members
}

fn enum_variant_declarations(
    declaration: &Declaration,
    text: &str,
    name: &str,
) -> Vec<Declaration> {
    let Some(tree) = parse_tree(text) else {
        return Vec::new();
    };
    let mut stack = vec![tree.root_node()];
    let mut variants = Vec::new();
    while let Some(node) = stack.pop() {
        if node.kind() == "enum_item"
            && node
                .child_by_field_name("name")
                .and_then(|name| name.utf8_text(text.as_bytes()).ok())
                == Some(declaration.name.as_str())
        {
            let mut descendants = vec![node];
            while let Some(descendant) = descendants.pop() {
                if descendant.kind() == "enum_variant" {
                    let Some(variant) = descendant.child_by_field_name("name") else {
                        continue;
                    };
                    if variant.utf8_text(text.as_bytes()).ok() != Some(name) {
                        continue;
                    }
                    let point = variant.start_position();
                    variants.push(Declaration {
                        name: name.to_owned(),
                        crate_name: declaration.crate_name.clone(),
                        module: declaration.module.clone(),
                        visibility: declaration.visibility.clone(),
                        kind: ItemKind::Enum,
                        path: declaration.path.clone(),
                        line: point.row as u32,
                        column: utf16_column(text, variant.start_byte()),
                        scope_depth: 0,
                        signature: descendant
                            .utf8_text(text.as_bytes())
                            .unwrap_or(name)
                            .to_owned(),
                    });
                    continue;
                }
                let mut cursor = descendant.walk();
                descendants.extend(descendant.named_children(&mut cursor));
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    variants
}

fn struct_field_declarations(path: &Path, text: &str, ty: &str, name: &str) -> Vec<Declaration> {
    let header = format!("struct {ty}");
    let Some(rest) = text.match_indices(&header).find_map(|(start, _)| {
        let rest = &text[start + header.len()..];
        (!rest.starts_with(is_ident)).then_some(rest)
    }) else {
        return Vec::new();
    };
    let Some((_, body)) = rest.split_once('{') else {
        return Vec::new();
    };
    let Some((body, _)) = body.split_once('}') else {
        return Vec::new();
    };
    let Some(offset) = body
        .find(name)
        .filter(|offset| body[*offset + name.len()..].trim_start().starts_with(':'))
    else {
        return Vec::new();
    };
    let start = text.find(body).unwrap_or(0) + offset;
    let point = text[..start].lines().count().saturating_sub(1) as u32;
    vec![Declaration {
        name: name.to_owned(),
        crate_name: String::new(),
        module: Vec::new(),
        visibility: Visibility::Private,
        kind: ItemKind::Variable,
        path: path.to_path_buf(),
        line: point,
        column: utf16_column(text, start),
        scope_depth: 0,
        signature: body.trim().to_owned(),
    }]
}

fn push_member_declaration(
    path: &Path,
    text: &str,
    node: Node<'_>,
    name: &str,
    kind: ItemKind,
    out: &mut Vec<Declaration>,
) {
    let Some(member_name) = node.child_by_field_name("name").or_else(|| {
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .find(|child| matches!(child.kind(), "identifier" | "field_identifier"))
    }) else {
        return;
    };
    if member_name.utf8_text(text.as_bytes()).ok() != Some(name) {
        return;
    }
    let point = member_name.start_position();
    out.push(Declaration {
        name: name.to_owned(),
        crate_name: String::new(),
        module: Vec::new(),
        visibility: Visibility::Private,
        kind,
        path: path.to_path_buf(),
        line: point.row as u32,
        column: utf16_column(text, member_name.start_byte()),
        scope_depth: 0,
        signature: text[node.start_byte()..node.end_byte()]
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_owned(),
    });
}

fn struct_for_member(node: Node<'_>, text: &str) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "struct_item" {
            return parent
                .child_by_field_name("name")?
                .utf8_text(text.as_bytes())
                .ok()
                .map(str::to_owned);
        }
        current = parent.parent();
    }
    None
}

fn impl_for_member(node: Node<'_>, text: &str) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "impl_item" {
            let ty = parent.child_by_field_name("type")?;
            return named_type(ty.utf8_text(text.as_bytes()).ok()?);
        }
        if parent.kind() == "trait_item" {
            return parent
                .child_by_field_name("name")?
                .utf8_text(text.as_bytes())
                .ok()
                .map(str::to_owned);
        }
        current = parent.parent();
    }
    None
}

/// False where a completed function name must stay bare: inside a `use`
/// declaration, or when `(` already follows the cursor.
fn adds_call_parentheses(text: &str, line: u32, column: u32) -> bool {
    let byte = cursor_byte(text, line, column);
    let line_start = text[..byte].rfind('\n').map_or(0, |newline| newline + 1);
    let written = text[line_start..byte].trim_start();
    let in_use = ["use ", "pub use ", "pub(crate) use "]
        .iter()
        .any(|keyword| written.starts_with(keyword));
    let after = text[byte..].trim_start_matches(is_ident);
    !in_use && !after.starts_with('(')
}

/// The parameters of `fn name(...)` in `detail`, without `self`. `None`
/// when the signature is cut off before its closing parenthesis.
fn call_parameters(detail: &str, name: &str) -> Option<Vec<String>> {
    let header = format!("fn {name}");
    let start = detail
        .match_indices(&header)
        .map(|(at, _)| at + header.len())
        .find(|end| !detail[*end..].starts_with(is_ident))?;
    let rest = detail[start..].trim_start();
    let mut depth = 0usize;
    let mut open = None;
    for (at, character) in rest.char_indices() {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            '(' if depth == 0 => {
                open = Some(at);
                break;
            }
            _ => {}
        }
    }
    let rest = &rest[open? + 1..];
    let mut parameters = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    for character in rest.chars() {
        match character {
            '(' | '<' | '[' | '{' => depth += 1,
            ')' if depth == 0 => {
                parameters.push(std::mem::take(&mut current));
                return Some(
                    parameters
                        .into_iter()
                        .map(|parameter| parameter.trim().to_owned())
                        .filter(|parameter| {
                            let receiver = parameter
                                .trim_start_matches('&')
                                .trim_start_matches(|c: char| c == '\'' || is_ident(c))
                                .trim_start()
                                .trim_start_matches("mut ")
                                .trim_start();
                            !parameter.is_empty()
                                && receiver != "self"
                                && !receiver.starts_with("self:")
                                && parameter != "self"
                                && parameter != "&self"
                                && parameter != "&mut self"
                                && parameter != "mut self"
                        })
                        .collect(),
                );
            }
            ')' | '>' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parameters.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(character);
    }
    None
}

/// The written type of a parameter, with lifetimes dropped: `&'a str` is
/// `&str`.
fn written_parameter_type(parameter: &str) -> String {
    let ty = parameter
        .split_once(':')
        .map_or(parameter, |(_, ty)| ty)
        .trim();
    let mut out = String::new();
    let mut rest = ty;
    while let Some(at) = rest.find('\'') {
        out.push_str(&rest[..at]);
        rest = rest[at + 1..].trim_start_matches(is_ident).trim_start();
    }
    out.push_str(rest);
    out
}

fn takes_str(item: &Completion) -> bool {
    item.detail
        .as_deref()
        .and_then(|detail| call_parameters(detail, &item.label))
        .is_some_and(|parameters| {
            matches!(parameters.as_slice(), [only] if written_parameter_type(only) == "&str")
        })
}

/// A function completes as a call: `len()`, `push($0)`, `from("$0")` for a
/// single string parameter, or `from('$0')` for a single `char`.
fn with_call(mut item: Completion) -> Completion {
    if item.kind != ItemKind::Function || item.is_snippet || item.insert_text != item.label {
        return item;
    }
    let parameters = item
        .detail
        .as_deref()
        .and_then(|detail| call_parameters(detail, &item.label));
    let name = &item.label;
    item.insert_text = match parameters.as_deref() {
        Some([]) => {
            item.insert_text = format!("{name}()");
            return item;
        }
        Some([only]) => match written_parameter_type(only).as_str() {
            "&str"
            | "impl Into<String>"
            | "impl AsRef<str>"
            | "impl AsRef<Path>"
            | "impl AsRef<std::path::Path>"
            | "impl Into<PathBuf>" => format!("{name}(\"$0\")"),
            "char" => format!("{name}('$0')"),
            _ => format!("{name}($0)"),
        },
        _ => format!("{name}($0)"),
    };
    item.is_snippet = true;
    item
}

fn completion(name: &str, detail: &str, kind: ItemKind) -> Completion {
    Completion {
        label: name.to_owned(),
        detail: Some(detail.to_owned()),
        insert_text: name.to_owned(),
        is_snippet: false,
        kind,
        sort: name.to_owned(),
        extra: Vec::new(),
        replace_from: None,
    }
}

fn std_type_completion(name: &str, text: &str) -> Completion {
    let mut item = completion(name, "Rust standard library type", ItemKind::Class);
    let module = match name {
        "HashMap" | "HashSet" => "std::collections",
        "Rc" => "std::rc",
        "Arc" => "std::sync",
        _ => return item,
    };
    let already_imported = text.lines().any(|line| {
        let line = line.trim();
        line.starts_with("use ") && line.contains(module) && line.contains(name)
    });
    if !already_imported {
        item.extra.push(Insertion {
            line: import_insertion_line(text),
            column: 0,
            text: format!("use {module}::{name};\n"),
        });
    }
    item
}

fn import_insertion_line(text: &str) -> u32 {
    text.lines()
        .take_while(|line| {
            let line = line.trim();
            line.is_empty() || line.starts_with("#![") || line.starts_with("//!")
        })
        .count() as u32
}

fn call_at(text: &str, byte: usize) -> Option<(String, u32)> {
    let tree = parse_tree(text)?;
    let call = deepest_call(tree.root_node(), byte)?;
    let function = call.child_by_field_name("function")?;
    let name = function
        .utf8_text(text.as_bytes())
        .ok()?
        .rsplit("::")
        .next()?
        .trim();
    if name.is_empty() {
        return None;
    }
    let arguments = call.child_by_field_name("arguments")?;
    let mut active = 0;
    let mut cursor = arguments.walk();
    for argument in arguments.named_children(&mut cursor) {
        if argument.end_byte() < byte {
            active += 1;
        }
    }
    Some((name.to_owned(), active))
}

/// Calls on lines `first..=last`: the called name, whether it is called
/// by path (`Type::f(x)`, where `self` is an argument), and the arguments.
fn rust_call_sites(
    text: &str,
    root: Node<'_>,
    first: u32,
    last: u32,
) -> Vec<(String, bool, Vec<Argument>)> {
    let (first, last) = (first as usize, last as usize);
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.end_position().row < first || node.start_position().row > last {
            continue;
        }
        if node.kind() == "call_expression"
            && out.len() < parameter_hints::MAX_CALLS
            && let Some(call) = rust_call_site(text, node)
        {
            out.push(call);
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    out
}

fn rust_call_site(text: &str, call: Node<'_>) -> Option<(String, bool, Vec<Argument>)> {
    let mut function = call.child_by_field_name("function")?;
    if function.kind() == "generic_function" {
        function = function.child_by_field_name("function")?;
    }
    let (name, associated) = match function.kind() {
        "identifier" => (function, false),
        "scoped_identifier" => (function.child_by_field_name("name")?, true),
        _ => return None,
    };
    let name = name.utf8_text(text.as_bytes()).ok()?;
    // Tuple structs and enum variants (`Some(x)`) take no named parameters.
    if name.starts_with(|c: char| c.is_uppercase()) {
        return None;
    }
    let list = call.child_by_field_name("arguments")?;
    let mut cursor = list.walk();
    let arguments: Vec<_> = list
        .named_children(&mut cursor)
        .filter(|argument| {
            !matches!(
                argument.kind(),
                "attribute_item" | "line_comment" | "block_comment"
            )
        })
        .map(|argument| {
            let value = match argument.kind() {
                "reference_expression" => argument.child_by_field_name("value").unwrap_or(argument),
                _ => argument,
            };
            let named = match value.kind() {
                "identifier" => Some(value),
                "field_expression" => value.child_by_field_name("field"),
                _ => None,
            };
            Argument {
                line: argument.start_position().row as u32,
                column: utf16_column(text, argument.start_byte()),
                name: named
                    .and_then(|node| node.utf8_text(text.as_bytes()).ok())
                    .map(str::to_owned),
                spread: false,
                named: false,
            }
        })
        .collect();
    (!arguments.is_empty()).then(|| (name.to_owned(), associated, arguments))
}

/// Parameters of the function whose name is at `line`/`column` of `text`.
/// `self` counts only when the function is called by path.
fn rust_parameters_at(
    text: &str,
    line: u32,
    column: u32,
    associated: bool,
) -> Option<Vec<Parameter>> {
    let tree = parse_tree(text)?;
    let byte = cursor_byte(text, line, column);
    let mut node = tree.root_node().descendant_for_byte_range(byte, byte)?;
    while !matches!(node.kind(), "function_item" | "function_signature_item") {
        node = node.parent()?;
    }
    let list = node.child_by_field_name("parameters")?;
    let mut cursor = list.walk();
    let mut out = Vec::new();
    for parameter in list.named_children(&mut cursor) {
        match parameter.kind() {
            "self_parameter" if associated => out.push(Parameter {
                name: "self".to_owned(),
                rest: false,
            }),
            "parameter" => {
                let pattern = parameter.child_by_field_name("pattern")?;
                let pattern = if pattern.kind() == "mut_pattern" {
                    let mut inner = pattern.walk();
                    pattern.named_children(&mut inner).last().unwrap_or(pattern)
                } else {
                    pattern
                };
                let name = if pattern.kind() == "identifier" {
                    pattern
                        .utf8_text(text.as_bytes())
                        .ok()?
                        .trim_start_matches('_')
                } else {
                    ""
                };
                out.push(Parameter {
                    name: name.to_owned(),
                    rest: false,
                });
            }
            _ => {}
        }
    }
    Some(out)
}

/// Type of a number written in a `let`. A suffix (`89u64`) names the type.
/// Without one the type is only the default (`i32`, `f64`) when nothing
/// later in the block could pick another: any use of the variable outside a
/// formatting macro may.
fn numeric_literal_type(
    text: &str,
    declaration: Node<'_>,
    value: Node<'_>,
    name: &str,
) -> Option<String> {
    numeric_literal_type_with(text, value, || only_printed_after(text, declaration, name))
}

/// The written suffix of a number, or `i32`/`f64` when `defaulted` says no
/// later use can pick another type.
fn numeric_literal_type_with(
    text: &str,
    value: Node<'_>,
    defaulted: impl FnOnce() -> bool,
) -> Option<String> {
    let mut literal = value;
    if matches!(value.kind(), "unary_expression" | "negative_literal") {
        if !text[value.start_byte()..value.end_byte()].starts_with('-') {
            return None;
        }
        let mut cursor = value.walk();
        literal = value
            .named_children(&mut cursor)
            .find(|child| matches!(child.kind(), "integer_literal" | "float_literal"))?;
    }
    let source = literal.utf8_text(text.as_bytes()).ok()?;
    const INTEGERS: [&str; 12] = [
        "u128", "i128", "usize", "isize", "u16", "u32", "u64", "i16", "i32", "i64", "u8", "i8",
    ];
    if let Some(suffix) = INTEGERS.iter().find(|suffix| source.ends_with(**suffix)) {
        return Some((*suffix).to_owned());
    }
    let hex = source.starts_with("0x") || source.starts_with("0X");
    if !hex && (source.ends_with("f32") || source.ends_with("f64")) {
        return Some(source[source.len() - 3..].to_owned());
    }
    if !defaulted() {
        return None;
    }
    let float = literal.kind() == "float_literal";
    Some(if float { "f64" } else { "i32" }.to_owned())
}

/// `Vec<T>` for `vec![a, b, ...]` or `vec![a; n]` when every element is a
/// literal of one type. Numbers follow the same rule as a plain `let`.
fn vec_literal_type(
    text: &str,
    declaration: Node<'_>,
    value: Node<'_>,
    name: &str,
) -> Option<String> {
    let mac = value
        .child_by_field_name("macro")?
        .utf8_text(text.as_bytes())
        .ok()?;
    if mac.rsplit("::").next() != Some("vec") {
        return None;
    }
    let mut cursor = value.walk();
    let tokens = value
        .named_children(&mut cursor)
        .find(|child| child.kind() == "token_tree")?;
    let mut cursor = tokens.walk();
    let repeated = tokens
        .children(&mut cursor)
        .any(|child| child.kind() == ";");
    let mut cursor = tokens.walk();
    // `vec![value; n]` has one element; `n` is the length.
    let elements = tokens
        .named_children(&mut cursor)
        .take(if repeated { 1 } else { usize::MAX });
    let mut element_type = None;
    let mut defaulted = None;
    for element in elements {
        let label = match element.kind() {
            "string_literal" | "raw_string_literal" => "&str".to_owned(),
            "boolean_literal" => "bool".to_owned(),
            "char_literal" => "char".to_owned(),
            "integer_literal" | "float_literal" => {
                numeric_literal_type_with(text, element, || {
                    *defaulted
                        .get_or_insert_with(|| vec_elements_defaulted(text, declaration, name))
                })?
            }
            _ => return None,
        };
        if element_type.get_or_insert_with(|| label.clone()) != &label {
            return None;
        }
    }
    element_type.map(|element| format!("Vec<{element}>"))
}

/// True when no later use of the vector `name` can give its numbers a type
/// other than the default. Allowed uses: formatting macros, a method
/// statement whose arguments are plain numbers (`x.push(5);`, `x.sort();`),
/// and `let y = x[i];` or `let y = &x[i];` where `y` is only printed.
fn vec_elements_defaulted(text: &str, declaration: Node<'_>, name: &str) -> bool {
    let Some(block) = declaration.parent() else {
        return false;
    };
    let plain_number = |node: Node<'_>| {
        matches!(node.kind(), "integer_literal" | "float_literal")
            && node.utf8_text(text.as_bytes()).is_ok_and(|source| {
                source
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '_' || c == '.')
            })
    };
    let allowed = |node: Node<'_>| {
        if is_formatted(text, node) {
            return true;
        }
        let Some(parent) = node.parent() else {
            return false;
        };
        match parent.kind() {
            "field_expression" if parent.child_by_field_name("value") == Some(node) => {
                let Some(call) = parent.parent() else {
                    return false;
                };
                if call.kind() != "call_expression"
                    || call.child_by_field_name("function") != Some(parent)
                    || call.parent().map(|statement| statement.kind())
                        != Some("expression_statement")
                {
                    return false;
                }
                let Some(arguments) = call.child_by_field_name("arguments") else {
                    return false;
                };
                let mut cursor = arguments.walk();
                arguments
                    .named_children(&mut cursor)
                    .all(|argument| argument.is_extra() || plain_number(argument))
            }
            "index_expression" if parent.named_child(0) == Some(node) => {
                let mut value = parent;
                if value
                    .parent()
                    .is_some_and(|up| up.kind() == "reference_expression")
                {
                    value = value.parent().unwrap_or(value);
                }
                let Some(binding) = value.parent() else {
                    return false;
                };
                if binding.kind() != "let_declaration"
                    || binding.child_by_field_name("value") != Some(value)
                    || binding.child_by_field_name("type").is_some()
                {
                    return false;
                }
                binding
                    .child_by_field_name("pattern")
                    .filter(|pattern| pattern.kind() == "identifier")
                    .and_then(|pattern| pattern.utf8_text(text.as_bytes()).ok())
                    .is_some_and(|element| only_printed_after(text, binding, element))
            }
            _ => false,
        }
    };
    let mut stack = vec![block];
    while let Some(node) = stack.pop() {
        if node.end_byte() <= declaration.end_byte() {
            continue;
        }
        if node.kind() == "identifier"
            && node.utf8_text(text.as_bytes()).ok() == Some(name)
            && !allowed(node)
        {
            return false;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    true
}

/// True when `node` sits in the arguments of a macro that only formats them.
fn is_formatted(text: &str, node: Node<'_>) -> bool {
    const FORMATTING: [&str; 8] = [
        "println", "print", "eprintln", "eprint", "format", "dbg", "write", "writeln",
    ];
    let mut ancestor = node.parent();
    while ancestor.is_some_and(|parent| parent.kind() == "token_tree") {
        ancestor = ancestor.and_then(|parent| parent.parent());
    }
    ancestor
        .filter(|parent| parent.kind() == "macro_invocation")
        .and_then(|parent| parent.child_by_field_name("macro"))
        .and_then(|mac| mac.utf8_text(text.as_bytes()).ok())
        .and_then(|mac| mac.rsplit("::").next())
        .is_some_and(|mac| FORMATTING.contains(&mac))
}

/// True when every later use of `name` in the enclosing block is an argument
/// of a macro that only formats it.
fn only_printed_after(text: &str, declaration: Node<'_>, name: &str) -> bool {
    let Some(block) = declaration.parent() else {
        return false;
    };
    let mut stack = vec![block];
    while let Some(node) = stack.pop() {
        if node.end_byte() <= declaration.end_byte() {
            continue;
        }
        if node.kind() == "identifier"
            && node.utf8_text(text.as_bytes()).ok() == Some(name)
            && !is_formatted(text, node)
        {
            return false;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    true
}

/// The return type written on the function whose name is at `line`/`column`.
/// `owner` replaces `Self` for associated calls (`Coffee::new()`). Generic,
/// `async` and `impl Trait` signatures give no answer.
fn rust_return_type_at(text: &str, line: u32, column: u32, owner: Option<&str>) -> Option<String> {
    let tree = parse_tree(text)?;
    let byte = cursor_byte(text, line, column);
    let mut node = tree.root_node().descendant_for_byte_range(byte, byte)?;
    while node.kind() != "function_item" {
        node = node.parent()?;
    }
    if node.child_by_field_name("type_parameters").is_some() {
        return None;
    }
    let mut cursor = node.walk();
    let asynchronous = node.named_children(&mut cursor).any(|child| {
        child.kind() == "function_modifiers"
            && child
                .utf8_text(text.as_bytes())
                .is_ok_and(|m| m.contains("async"))
    });
    if asynchronous {
        return None;
    }
    let written = node
        .child_by_field_name("return_type")?
        .utf8_text(text.as_bytes())
        .ok()?
        .trim();
    if written.contains("impl ") || written.contains("dyn ") || written.contains('\'') {
        return None;
    }
    if !written
        .split(|c: char| !is_ident(c))
        .any(|word| word == "Self")
    {
        return Some(written.to_owned());
    }
    let owner =
        owner.filter(|owner| owner.starts_with(|c: char| c.is_uppercase()) && *owner != "Self")?;
    let mut out = String::new();
    let mut word = String::new();
    for character in written.chars().chain(std::iter::once(' ')) {
        if is_ident(character) {
            word.push(character);
            continue;
        }
        out.push_str(if word == "Self" { owner } else { &word });
        word.clear();
        out.push(character);
    }
    out.pop();
    Some(out)
}

/// The type name a call of `declaration` returns, for following a call
/// chain. Unlike [`rust_return_type_at`], a generic function still answers
/// when its return type is not one of its own type parameters
/// (`fn route_layer<L>(self, layer: L) -> MethodRouter<S, E>`), and a
/// function written by a macro answers from its signature.
fn named_return_type(text: &str, declaration: &Declaration, owner: Option<&str>) -> Option<String> {
    if let Some(ty) = rust_return_type_at(text, declaration.line, declaration.column, owner) {
        return named_type(&ty);
    }
    let tree = parse_tree(text);
    let function = tree.as_ref().and_then(|tree| {
        let byte = cursor_byte(text, declaration.line, declaration.column);
        let mut node = tree.root_node().descendant_for_byte_range(byte, byte)?;
        while node.kind() != "function_item" {
            node = node.parent()?;
        }
        Some(node)
    });
    let (parameters, written) = match function {
        Some(node) => {
            let mut cursor = node.walk();
            let asynchronous = node.named_children(&mut cursor).any(|child| {
                child.kind() == "function_modifiers"
                    && child
                        .utf8_text(text.as_bytes())
                        .is_ok_and(|m| m.contains("async"))
            });
            if asynchronous {
                return None;
            }
            let parameters = node
                .child_by_field_name("type_parameters")
                .and_then(|node| node.utf8_text(text.as_bytes()).ok())
                .unwrap_or("")
                .to_owned();
            let written = node
                .child_by_field_name("return_type")?
                .utf8_text(text.as_bytes())
                .ok()?
                .to_owned();
            (parameters, written)
        }
        None => {
            let signature = declaration.signature.as_str();
            if signature.contains("async ") {
                return None;
            }
            let after_name = signature.split_once(&format!("fn {}", declaration.name))?.1;
            let (parameters, rest) = match after_name.strip_prefix('<') {
                Some(generic) => generic.split_once('>')?,
                None => ("", after_name),
            };
            let written = rest.split_once("->")?.1;
            let written = written.split(" where").next()?.trim_end_matches('{').trim();
            (parameters.to_owned(), written.to_owned())
        }
    };
    if written.contains("impl ") || written.contains("dyn ") {
        return None;
    }
    let ty = named_type(&written)?;
    if ty == "Self" {
        return owner
            .filter(|owner| owner.starts_with(|c: char| c.is_uppercase()) && *owner != "Self")
            .map(str::to_owned);
    }
    let generic = parameters
        .trim_matches(['<', '>'])
        .split(',')
        .filter_map(|parameter| parameter.trim().split([':', ' ', '=']).next())
        .any(|parameter| parameter == ty);
    (!generic).then_some(ty)
}

fn deepest_call(node: Node<'_>, byte: usize) -> Option<Node<'_>> {
    if node.start_byte() > byte || node.end_byte() < byte {
        return None;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(call) = deepest_call(child, byte) {
            return Some(call);
        }
    }
    (node.kind() == "call_expression").then_some(node)
}

fn signature_parameters(signature: &str) -> Vec<String> {
    signature
        .split_once('(')
        .and_then(|(_, after)| after.split_once(')').map(|(parameters, _)| parameters))
        .map(|parameters| {
            parameters
                .split(',')
                .map(str::trim)
                .filter(|parameter| !parameter.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn line_start(text: &str, line: u32) -> Option<usize> {
    let mut start = 0;
    for _ in 0..line {
        start += text[start..].find('\n')? + 1;
    }
    Some(start)
}

fn byte_at_column(line: &str, column: u32) -> usize {
    let mut units = 0;
    for (offset, character) in line.char_indices() {
        if units >= column {
            return offset;
        }
        units += character.len_utf16() as u32;
    }
    line.len()
}

fn cursor_byte(text: &str, line: u32, column: u32) -> usize {
    line_start(text, line).map_or(text.len(), |start| {
        let end = text[start..]
            .find('\n')
            .map_or(text.len(), |offset| start + offset);
        start + byte_at_column(&text[start..end], column)
    })
}

fn utf16_column(text: &str, byte: usize) -> u32 {
    let line_start = text[..byte].rfind('\n').map_or(0, |offset| offset + 1);
    text[line_start..byte].encode_utf16().count() as u32
}

fn is_ident(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

fn is_ident_text(text: &str) -> bool {
    let mut characters = text.chars();
    matches!(characters.next(), Some(character) if is_ident(character)) && characters.all(is_ident)
}

fn item_kind_name(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Class => "Class",
        ItemKind::Interface => "Interface",
        ItemKind::Enum => "Enum",
        ItemKind::Function => "Function",
        ItemKind::Constant => "Constant",
        _ => "Variable",
    }
}

fn valid_name(name: &str) -> bool {
    let mut characters = name.chars();
    matches!(characters.next(), Some(character) if character == '_' || character.is_alphabetic())
        && characters.all(|character| character == '_' || character.is_alphanumeric())
        && !KEYWORDS.contains(&name)
}

fn item_occurrences(
    engine: &RustEngine,
    path: &Path,
    text: &str,
    name: &str,
    target: &Declaration,
) -> Vec<Occurrence> {
    let Some(tree) = parse_tree(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier"
            && node.utf8_text(text.as_bytes()).ok() == Some(name)
            && engine
                .resolve(path, name, node.start_byte())
                .first()
                .is_some_and(|found| {
                    found.path == target.path
                        && found.line == target.line
                        && found.column == target.column
                })
        {
            let point = node.start_position();
            out.push(Occurrence {
                path: path.to_path_buf(),
                line: point.row as u32,
                start_column: utf16_column(text, node.start_byte()),
                end_column: utf16_column(text, node.end_byte()),
                dollar: false,
            });
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    out.sort();
    out
}

fn local_occurrences(
    path: &Path,
    text: &str,
    name: &str,
    declaration: &Declaration,
) -> Vec<Occurrence> {
    let Some(tree) = parse_tree(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier"
            && node.utf8_text(text.as_bytes()).ok() == Some(name)
            && (utf16_column(text, node.start_byte()) == declaration.column
                && node.start_position().row as u32 == declaration.line
                || local_declaration(path, text, name, node.start_byte()).is_some_and(|found| {
                    found.line == declaration.line && found.column == declaration.column
                }))
        {
            let point = node.start_position();
            out.push(Occurrence {
                path: path.to_path_buf(),
                line: point.row as u32,
                start_column: utf16_column(text, node.start_byte()),
                end_column: utf16_column(text, node.end_byte()),
                dollar: false,
            });
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn cargo_refresh_uses_the_requested_shell_free_arguments() {
        assert_eq!(
            cargo_refresh_args(Path::new("/tmp/missing-workspace"), None),
            [
                "check",
                "--workspace",
                "--all-targets",
                "--offline",
                "--message-format=json"
            ]
        );
    }

    #[test]
    fn cargo_refresh_checks_only_the_package_containing_the_saved_file() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("crates/editor-core");
        let source = package.join("src/lib.rs");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(
            package.join("Cargo.toml"),
            "[package]\nname = \"editor-core\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(&source, "").unwrap();

        assert_eq!(
            cargo_refresh_args(root.path(), Some(&source)),
            [
                "check",
                "-p",
                "editor-core",
                "--all-targets",
                "--offline",
                "--message-format=json"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn cargo_executables_prioritizes_valid_rustup_locations_before_path() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let cargo_home = dir.path().join("cargo-home");
        let home = dir.path().join("home");
        for root in [&cargo_home, &home.join(".cargo")] {
            let binary = root.join("bin").join("cargo");
            std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
            std::fs::write(&binary, "").unwrap();
            let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&binary, permissions).unwrap();
        }

        assert_eq!(
            cargo_executables(Some(&cargo_home), Some(&home)),
            vec![
                cargo_home.join("bin/cargo"),
                home.join(".cargo/bin/cargo"),
                PathBuf::from("cargo"),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn cargo_executables_ignores_missing_or_non_executable_locations() {
        let dir = tempfile::tempdir().unwrap();
        let cargo_home = dir.path().join("cargo-home");
        let home = dir.path().join("home");
        let binary = home.join(".cargo/bin/cargo");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(binary, "").unwrap();

        assert_eq!(
            cargo_executables(Some(&cargo_home), Some(&home)),
            vec![PathBuf::from("cargo")]
        );
    }

    #[test]
    fn cargo_refresh_counts_compiler_artifacts_only() {
        let output = b"{\"reason\":\"compiler-artifact\"}\n{\"reason\":\"build-finished\"}\n{\"reason\":\"compiler-artifact\"}\n";
        assert_eq!(cargo_compiler_artifact_count(output), 2);
    }

    #[test]
    fn canceled_refresh_is_distinct_from_timeout_and_failure() {
        let canceled = refresh_canceled(3);
        assert!(!canceled.succeeded);
        assert!(!canceled.timed_out);
        assert!(canceled.canceled);
        assert_eq!(canceled.compiler_artifacts, 3);

        let failed = refresh_failure("failed".into());
        assert!(!failed.timed_out);
        assert!(!failed.canceled);
    }

    #[test]
    fn cancel_returns_false_without_an_active_refresh() {
        assert!(!cancel_generated_source_refresh(Path::new(
            "/tmp/galaxy-no-active-cargo-refresh"
        )));
    }

    #[test]
    fn cargo_refresh_output_collection_returns_after_timeout() {
        let (sender, receiver) = mpsc::channel();
        let output = collect_bounded(receiver, Duration::from_millis(1));
        assert!(output.is_empty());
        drop(sender);
    }

    #[test]
    fn cargo_stdout_retains_late_compiler_messages_after_generic_limit() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src/lib.rs");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "fn main() {}\n").unwrap();
        let diagnostic = format!(
            r#"{{"reason":"compiler-message","message":{{"message":"late diagnostic","level":"error","spans":[{{"file_name":"{}","line_start":1,"column_start":1,"line_end":1,"column_end":3,"is_primary":true}}],"children":[]}}}}"#,
            source.display()
        );
        let artifact =
            b"{\"reason\":\"compiler-artifact\",\"package_id\":\"large-workspace-artifact\"}\n";
        let mut stdout = Vec::new();
        while stdout.len() <= CARGO_OUTPUT_LIMIT {
            stdout.extend_from_slice(artifact);
        }
        stdout.extend_from_slice(diagnostic.as_bytes());
        stdout.push(b'\n');
        stdout.extend_from_slice(b"{\"reason\":\"build-finished\",\"success\":false}\n");

        let output = collect_bounded(
            super::read_cargo_stdout(std::io::Cursor::new(stdout)),
            Duration::from_secs(1),
        );
        assert!(std::str::from_utf8(&output).unwrap().lines().any(|line| {
            serde_json::from_str::<serde_json::Value>(line).is_ok_and(|value| {
                value.get("reason").and_then(serde_json::Value::as_str) == Some("build-finished")
            })
        }));
        let diagnostics = parse_compiler_diagnostics(dir.path(), &output);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].message, "late diagnostic");
    }

    #[test]
    fn compiler_diagnostics_parse_machine_applicable_unicode_suggestions() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src/lib.rs");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "fn main() { let cafe = \"cafe\"; }\n").unwrap();
        let json = format!(
            r#"{{"reason":"compiler-message","message":{{"message":"borrow of moved value","level":"error","code":{{"code":"E0382"}},"spans":[{{"file_name":"{}","line_start":1,"column_start":17,"line_end":1,"column_end":21,"is_primary":true,"label":"value moved here"}}],"children":[{{"message":"consider cloning","spans":[{{"file_name":"{}","line_start":1,"column_start":17,"line_end":1,"column_end":21,"suggested_replacement":"café.clone()","suggestion_applicability":"MachineApplicable"}}]}}]}}}}"#,
            source.display(),
            source.display()
        );
        let diagnostics = parse_compiler_diagnostics(dir.path(), json.as_bytes());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_deref(), Some("E0382"));
        assert_eq!(
            diagnostics[0].message,
            "borrow of moved value\nvalue moved here"
        );
        assert_eq!(diagnostics[0].actions[0].edits[0].new_text, "café.clone()");
        assert_eq!(diagnostics[0].range.start_character, 16);
    }

    #[test]
    fn compiler_diagnostics_deduplicate_repeated_target_output() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src/lib.rs");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "fn main() {}\n").unwrap();
        let json = format!(
            r#"{{"reason":"compiler-message","message":{{"message":"borrow of moved value","level":"error","code":{{"code":"E0382"}},"spans":[{{"file_name":"{}","line_start":1,"column_start":1,"line_end":1,"column_end":3,"is_primary":true,"label":"value moved here"}}],"children":[]}}}}"#,
            source.display()
        );
        let output = format!("{json}\n{json}\n");
        let diagnostics = parse_compiler_diagnostics(dir.path(), output.as_bytes());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_deref(), Some("E0382"));
    }

    use super::*;

    #[test]
    fn function_completions_insert_call_parentheses() {
        let path = Path::new("/tmp/call-snippets.rs");
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        let insert = |engine: &mut RustEngine, source: &str, label: &str| {
            let line = source.lines().count() as u32 - 1;
            let column = source.lines().last().unwrap().len() as u32;
            engine
                .complete(path, source, line, column)
                .into_iter()
                .find(|item| item.label == label)
                .map(|item| (item.insert_text, item.is_snippet))
        };
        assert_eq!(
            insert(&mut engine, "fn main() {\n    String::from", "from"),
            Some(("from(\"$0\")".to_owned(), true))
        );
        assert_eq!(
            insert(&mut engine, "fn main() {\n    String::ne", "new"),
            Some(("new()".to_owned(), false))
        );
        let source = "fn greet(name: u32, loud: bool) {}\nfn main() {\n    gre";
        assert_eq!(
            insert(&mut engine, source, "greet"),
            Some(("greet($0)".to_owned(), true))
        );
        assert!(!adds_call_parentheses("use std::mem::sw", 0, 16));
        assert!(!adds_call_parentheses("pub use crate::gre", 0, 18));
        assert!(!adds_call_parentheses("    gre(1)", 0, 7));
        assert!(adds_call_parentheses("    gre", 0, 7));
    }

    #[test]
    fn call_parameters_skip_receivers_and_generics() {
        assert_eq!(
            call_parameters("pub fn push(&mut self, value: T)", "push"),
            Some(vec!["value: T".to_owned()])
        );
        assert_eq!(
            call_parameters(
                "fn from_iter<I: IntoIterator<Item = char>>(iter: I) -> Self",
                "from_iter"
            ),
            Some(vec!["iter: I".to_owned()])
        );
        assert_eq!(
            call_parameters("pub fn len(&self) -> usize", "len"),
            Some(vec![])
        );
        assert_eq!(
            call_parameters("fn from_iter<I: IntoIterator<Item = C", "from_iter"),
            None
        );
        assert_eq!(written_parameter_type("s: &'a str"), "&str");
    }

    #[test]
    fn macro_written_functions_are_declared() {
        let source = "macro_rules! handler_fn {\n    (\n        $(#[$m:meta])+\n        $name:ident, $method:ident\n    ) => {\n        $(#[$m])+\n        pub fn $name<H, T, S>(handler: H) -> MethodRouter<S, Infallible>\n        where\n            H: Handler<T, S>,\n        {\n            on(MethodFilter::$method, handler)\n        }\n    };\n}\n\nhandler_fn!(get, GET);\nhandler_fn!(post, POST);\n";
        let found = parse_declarations(
            Path::new("/tmp/routing/method_routing.rs"),
            source,
            Path::new("/tmp"),
            "axum",
            &CfgContext::default(),
            None,
        );
        let get = found
            .iter()
            .find(|item| item.name == "get")
            .expect("get declared");
        assert_eq!((get.line, get.column), (15, 12));
        assert_eq!(get.kind, ItemKind::Function);
        assert_eq!(get.visibility, Visibility::Public);
        assert_eq!(
            get.signature,
            "pub fn get<H, T, S>(handler: H) -> MethodRouter<S, Infallible>"
        );
        assert!(found.iter().any(|item| item.name == "post"));
    }

    fn definition_at(
        engine: &mut RustEngine,
        path: &Path,
        source: &str,
        name: &str,
    ) -> Vec<Target> {
        let offset = source.rfind(name).unwrap();
        let before = &source[..offset];
        engine.definition(
            path,
            source,
            before.bytes().filter(|byte| *byte == b'\n').count() as u32,
            before.rsplit('\n').next().unwrap_or("").len() as u32,
        )
    }

    #[test]
    fn open_buffer_provides_completion_definition_hover_and_outline() {
        let path = Path::new("/tmp/lib.rs");
        let text = "pub struct Coffee;\npub fn brew() {}\nfn main() { brew }";
        let column = text.lines().nth(2).unwrap().find("brew").unwrap() as u32 + 2;
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        assert_eq!(engine.complete(path, text, 2, column + 2)[0].label, "brew");
        assert_eq!(engine.definition(path, text, 2, column)[0].line, 1);
        assert!(
            engine
                .hover(path, text, 2, column)
                .unwrap()
                .markdown
                .contains("fn brew")
        );
        assert_eq!(engine.outline(path, text).len(), 3);
    }

    #[test]
    fn reports_rust_syntax_errors() {
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        assert!(
            !engine
                .diagnostics(Path::new("/tmp/lib.rs"), "fn broken(")
                .is_empty()
        );
    }

    #[test]
    fn disk_delta_replaces_and_removes_file_declarations() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("src");
        std::fs::create_dir_all(&source).unwrap();
        let path = source.join("lib.rs");
        std::fs::write(&path, "pub fn old_name() {}\n").unwrap();
        let mut engine = RustEngine::pending(root.path());
        engine.install(RustEngine::build(root.path()));
        std::fs::write(&path, "pub fn new_name() {}\n").unwrap();
        assert!(engine.apply_disk_changes(std::slice::from_ref(&path)));
        assert!(
            engine
                .declarations
                .iter()
                .any(|item| item.name == "new_name")
        );
        assert!(
            !engine
                .declarations
                .iter()
                .any(|item| item.name == "old_name")
        );
        std::fs::remove_file(&path).unwrap();
        assert!(engine.apply_disk_changes(std::slice::from_ref(&path)));
        assert!(!engine.declarations.iter().any(|item| item.path == path));
    }

    #[test]
    fn references_and_rename_require_a_unique_top_level_declaration() {
        let path = Path::new("/tmp/lib.rs");
        let text = "pub fn brew() {}\nfn main() { brew(); }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        let uses = engine.references(path, text, 1, 13);
        assert_eq!(uses.len(), 2);
        assert_eq!(
            engine
                .rename(path, text, 1, 13, "make_coffee")
                .unwrap()
                .len(),
            2
        );
        assert!(engine.rename(path, text, 1, 13, "fn").is_err());
    }

    #[test]
    fn imported_alias_resolves_to_its_unique_declaration() {
        let definition = PathBuf::from("/tmp/models.rs");
        let importer = Path::new("/tmp/lib.rs");
        let source = "use crate::models::Coffee as Drink;\nfn main() { Drink; }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built {
            declarations: vec![Declaration {
                name: "Coffee".to_owned(),
                crate_name: String::new(),
                module: vec!["models".to_owned()],
                visibility: Visibility::Public,
                kind: ItemKind::Class,
                path: definition.clone(),
                line: 0,
                column: 11,
                scope_depth: 0,
                signature: "pub struct Coffee;".to_owned(),
            }],
            files: Vec::new(),
            uses: HashMap::new(),
            module_identities: HashMap::new(),
            source_roots: Vec::new(),
            packages: Vec::new(),
        });
        assert_eq!(
            engine.definition(importer, source, 1, 14)[0].path,
            definition
        );
        assert!(
            engine
                .hover(importer, source, 1, 14)
                .unwrap()
                .markdown
                .contains("Coffee")
        );
    }

    #[test]
    fn module_paths_distinguish_same_named_items() {
        let root = Path::new("/project/src");
        let mut engine = RustEngine::pending(Path::new("/project"));
        engine.install(Built {
            declarations: vec![
                Declaration {
                    name: "Config".to_owned(),
                    crate_name: String::new(),
                    module: vec!["client".to_owned()],
                    visibility: Visibility::Public,
                    kind: ItemKind::Class,
                    path: PathBuf::from("/project/src/client.rs"),
                    line: 0,
                    column: 11,
                    scope_depth: 0,
                    signature: "pub struct Config;".to_owned(),
                },
                Declaration {
                    name: "Config".to_owned(),
                    crate_name: String::new(),
                    module: vec!["server".to_owned()],
                    visibility: Visibility::Public,
                    kind: ItemKind::Class,
                    path: PathBuf::from("/project/src/server.rs"),
                    line: 0,
                    column: 11,
                    scope_depth: 0,
                    signature: "pub struct Config;".to_owned(),
                },
            ],
            files: Vec::new(),
            uses: HashMap::new(),
            module_identities: HashMap::new(),
            source_roots: vec![root.to_path_buf()],
            packages: Vec::new(),
        });
        let source = "use crate::server::Config;\nfn main() { Config; }";
        assert_eq!(
            engine.definition(Path::new("/project/src/lib.rs"), source, 1, 14)[0].path,
            PathBuf::from("/project/src/server.rs")
        );
    }

    #[test]
    fn lexical_scope_prefers_the_nearest_binding() {
        let path = Path::new("/tmp/lib.rs");
        let source =
            "fn brew(coffee: u8) {\n let coffee = 2;\n { let coffee = 3; coffee; }\n coffee;\n}";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        assert_eq!(engine.definition(path, source, 2, 22)[0].line, 2);
        assert_eq!(engine.definition(path, source, 3, 2)[0].line, 1);
    }

    #[test]
    fn grouped_imports_and_repeated_super_resolve_the_full_path() {
        let root = Path::new("/project/src");
        let mut engine = RustEngine::pending(Path::new("/project"));
        engine.install(Built {
            declarations: vec![Declaration {
                name: "Coffee".to_owned(),
                crate_name: String::new(),
                module: vec!["models".to_owned(), "drink".to_owned()],
                visibility: Visibility::Public,
                kind: ItemKind::Class,
                path: PathBuf::from("/project/src/models/drink.rs"),
                line: 0,
                column: 11,
                scope_depth: 0,
                signature: "pub struct Coffee;".to_owned(),
            }],
            files: Vec::new(),
            uses: HashMap::new(),
            module_identities: HashMap::new(),
            source_roots: vec![root.to_path_buf()],
            packages: Vec::new(),
        });
        let source = "use crate::models::{drink::{Coffee as Bean}};\nfn main() { Bean; }";
        assert_eq!(
            engine.definition(Path::new("/project/src/lib.rs"), source, 1, 14)[0].path,
            PathBuf::from("/project/src/models/drink.rs")
        );
        let nested = Path::new("/project/src/models/drink/nested.rs");
        let source = "use super::super::drink::Coffee;\nfn main() { Coffee; }";
        assert_eq!(
            engine.definition(nested, source, 1, 14)[0].path,
            PathBuf::from("/project/src/models/drink.rs")
        );
    }

    #[test]
    fn rename_local_binding_excludes_shadowed_bindings() {
        let path = Path::new("/tmp/lib.rs");
        let source = "fn main() {\n let coffee = 1;\n { let coffee = 2; coffee; }\n coffee;\n}";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        let references = engine.references(path, source, 2, 21);
        assert_eq!(references.len(), 2);
        assert_eq!(references[0].line, 2);
        assert_eq!(references[1].line, 2);
        assert_eq!(
            engine
                .rename(path, source, 2, 21, "espresso")
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn glob_import_and_item_references_follow_the_resolved_declaration() {
        let root = Path::new("/project/src");
        let target = PathBuf::from("/project/src/models.rs");
        let source_path = Path::new("/project/src/lib.rs");
        let mut engine = RustEngine::pending(Path::new("/project"));
        engine.install(Built {
            declarations: vec![Declaration {
                name: "Coffee".to_owned(),
                crate_name: String::new(),
                module: vec!["models".to_owned()],
                visibility: Visibility::Public,
                kind: ItemKind::Class,
                path: target.clone(),
                line: 0,
                column: 11,
                scope_depth: 0,
                signature: "pub struct Coffee;".to_owned(),
            }],
            files: vec![target],
            uses: HashMap::new(),
            module_identities: HashMap::new(),
            source_roots: vec![root.to_path_buf()],
            packages: Vec::new(),
        });
        let source = "use crate::models::*;\nfn main() { Coffee; }";
        assert_eq!(
            engine.definition(source_path, source, 1, 14)[0].path,
            PathBuf::from("/project/src/models.rs")
        );
    }

    #[test]
    fn member_completion_and_signature_use_local_type_annotations() {
        let path = Path::new("/tmp/lib.rs");
        let source = "struct Coffee { beans: u8 }\nimpl Coffee { fn brew(&self, minutes: u8) {} }\nfn main() { let coffee: Coffee = Coffee { beans: 1 }; coffee.br; brew(1, 2); }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        assert!(
            engine
                .complete(path, source, 2, 65)
                .iter()
                .any(|item| item.label == "brew")
        );
        let argument = source.lines().nth(2).unwrap().rfind('2').unwrap() as u32;
        let signature = engine.signature(path, source, 2, argument).unwrap();
        assert!(signature.label.contains("fn brew"));
        assert_eq!(signature.active, 1);
    }

    #[test]
    fn member_completion_and_definition_infer_function_return_type() {
        let path = Path::new("/tmp/lib.rs");
        let source = "struct Coffee;\nimpl Coffee { fn brew(&self) {} }\nfn make_coffee() -> Coffee { Coffee }\nfn main() { let coffee = make_coffee(); coffee.br; coffee.brew(); }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        let line = source.lines().nth(3).unwrap();
        let completion_column = line.find(".br;").unwrap() as u32 + 3;

        assert!(
            engine
                .complete(path, source, 3, completion_column)
                .iter()
                .any(|item| item.label == "brew")
        );
        let column = line.rfind("brew").unwrap() as u32 + 1;
        let definitions = engine.definition(path, source, 3, column);
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].line, 1);
    }

    #[test]
    fn definition_infers_for_loop_variable_from_declared_collection() {
        let path = Path::new("/tmp/lib.rs");
        let source = "pub trait Migration { fn name(&self) -> &'static str; }\n\
pub struct Migrator { migrations: Vec<Box<dyn Migration>> }\n\
impl Migrator {\n\
    pub fn new(migrations: Vec<Box<dyn Migration>>) {\n\
        for migration in &migrations { migration.name(); }\n\
    }\n\
    fn run(&self) {\n\
        for migration in self.migrations.iter().rev() { migration.name(); }\n\
    }\n\
    fn check(&self) {\n\
        self.migrations.iter().any(|migration| migration.name() == \"\");\n\
    }\n\
}\n";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        for line in [4_u32, 7, 10] {
            let text = source.lines().nth(line as usize).unwrap();
            let column = text.rfind("name").unwrap() as u32 + 1;
            let found = engine.definition(path, source, line, column);
            assert_eq!(found.len(), 1, "line {line}");
            assert_eq!(found[0].line, 0);
        }
    }

    #[test]
    fn element_type_unwraps_collections_and_smart_pointers() {
        assert_eq!(
            element_type("Vec<Box<dyn Migration>>").as_deref(),
            Some("Migration")
        );
        assert_eq!(element_type("&[Coffee]").as_deref(), Some("Coffee"));
        assert_eq!(
            element_type("HashMap<String, Coffee>").as_deref(),
            Some("String")
        );
    }

    #[test]
    fn project_completion_adds_import_for_public_item() {
        let root = Path::new("/project/src");
        let path = Path::new("/project/src/lib.rs");
        let target = PathBuf::from("/project/src/models.rs");
        let mut engine = RustEngine::pending(Path::new("/project"));
        engine.install(Built {
            declarations: vec![Declaration {
                name: "Coffee".to_owned(),
                crate_name: String::new(),
                module: vec!["models".to_owned()],
                visibility: Visibility::Public,
                kind: ItemKind::Class,
                path: target.clone(),
                line: 0,
                column: 11,
                scope_depth: 0,
                signature: "pub struct Coffee;".to_owned(),
            }],
            files: vec![target],
            source_roots: vec![root.to_path_buf()],
            ..Built::default()
        });

        let item = engine
            .complete(path, "//! docs\nfn main() { Coff }", 1, 20)
            .into_iter()
            .find(|item| item.label == "Coffee")
            .unwrap();
        assert_eq!(item.extra.len(), 1);
        assert_eq!(item.extra[0].line, 1);
        assert_eq!(item.extra[0].text, "use crate::models::Coffee;\n");
    }

    #[test]
    fn std_string_members_complete_and_goto_definition() {
        let path = Path::new("/tmp/std.rs");
        let source = "fn main() { let text = String::; String::new(); }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());

        let completions = engine.complete(path, source, 0, 31);
        assert!(completions.iter().any(|item| item.label == "new"));

        let name = source.rfind("new").unwrap() as u32 + 1;
        let definitions = engine.definition(path, source, 0, name);
        assert_eq!(definitions.len(), 1);
        assert!(
            definitions[0]
                .path
                .ends_with("lib/rustlib/src/rust/library/alloc/src/string.rs")
        );
    }

    #[test]
    fn standard_library_types_goto_definition() {
        let path = Path::new("/tmp/std-return.rs");
        let cases = [
            ("String", "library/alloc/src/string.rs"),
            ("Vec", "library/alloc/src/vec/mod.rs"),
            ("Option", "library/core/src/option.rs"),
            ("Result", "library/core/src/result.rs"),
            ("Box", "library/alloc/src/boxed.rs"),
            ("Rc", "library/alloc/src/rc.rs"),
            ("Arc", "library/alloc/src/sync.rs"),
            ("HashMap", "library/std/src/collections/hash/map.rs"),
            ("HashSet", "library/std/src/collections/hash/set.rs"),
        ];
        let source = cases
            .iter()
            .enumerate()
            .map(|(index, (ty, _))| format!("fn value_{index}() -> {ty} {{ todo!() }}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());

        for (line, (ty, expected)) in cases.iter().enumerate() {
            let column = source.lines().nth(line).unwrap().find(ty).unwrap() as u32 + 1;
            let targets = engine.definition(path, &source, line as u32, column);
            assert_eq!(targets.len(), 1, "{ty} targets: {targets:?}");
            assert!(
                targets[0].path.ends_with(expected),
                "unexpected {ty} target: {:?}",
                targets[0].path
            );
        }

        let source = "fn problems() -> Vec<Problem> { Vec::new() }";
        let vec_type = engine.definition(path, source, 0, 18);
        assert_eq!(vec_type.len(), 1, "return Vec targets: {vec_type:?}");
        assert!(vec_type[0].path.ends_with("library/alloc/src/vec/mod.rs"));

        let new = engine.definition(path, source, 0, 39);
        assert_eq!(new.len(), 1, "Vec::new targets: {new:?}");
        assert!(new[0].path.ends_with("library/alloc/src/vec/mod.rs"));
        assert_eq!(new[0].line, 438);

        let option_hover = engine
            .hover(path, "fn hover() -> Option<HoverResult> { todo!() }", 0, 15)
            .unwrap();
        assert!(
            option_hover.markdown.contains("enum Option"),
            "{}",
            option_hover.markdown
        );

        let vec_hover = engine.hover(path, source, 0, 18).unwrap();
        assert!(
            vec_hover.markdown.contains("struct Vec"),
            "{}",
            vec_hover.markdown
        );

        let new_hover = engine.hover(path, source, 0, 39).unwrap();
        assert!(
            new_hover.markdown.contains("fn new()"),
            "{}",
            new_hover.markdown
        );
        assert!(
            !new_hover.markdown.contains("root: &Path"),
            "{}",
            new_hover.markdown
        );
    }

    #[test]
    fn std_string_completion_includes_trait_associated_functions() {
        let path = Path::new("/tmp/std-from.rs");
        let source = "fn main() { let text = String::fro; }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());

        let completions = engine.complete(path, source, 0, 34);
        assert!(completions.iter().any(|item| item.label == "from"));
    }

    #[test]
    fn definition_resolves_associated_function_on_scoped_type() {
        let path = Path::new("/tmp/assoc-fn.rs");
        let source = "struct Auth;\nimpl Auth { fn from_env() -> Self { Auth } }\nstruct Server;\nimpl Server { fn from_env() -> Self { Server } }\nfn main() { let a = Auth::from_env(); let s = Server::from_env(); }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());

        let line = source.lines().nth(4).unwrap();
        let auth = line.find("Auth::from_env").unwrap() as u32 + "Auth::".len() as u32 + 2;
        let server = line.find("Server::from_env").unwrap() as u32 + "Server::".len() as u32 + 2;
        assert_eq!(engine.definition(path, source, 4, auth)[0].line, 1);
        assert_eq!(engine.definition(path, source, 4, server)[0].line, 3);
    }

    #[test]
    fn definition_follows_reexported_crate_alias_and_builder_chain() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("database");
        let framework = root.path().join("framework");
        let app = root.path().join("app");
        for directory in [&database, &framework, &app] {
            std::fs::create_dir_all(directory.join("src")).unwrap();
        }
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"database\", \"framework\", \"app\"]\n",
        )
        .unwrap();
        std::fs::write(
            database.join("Cargo.toml"),
            "[package]\nname = \"framework-database\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(
            database.join("src/lib.rs"),
            "pub struct DatabaseConfig;\nimpl DatabaseConfig { pub fn new() -> Self { DatabaseConfig } }\npub fn connect() {}\n",
        )
        .unwrap();
        std::fs::write(
            framework.join("Cargo.toml"),
            "[package]\nname = \"framework\"\nversion = \"0.1.0\"\n[dependencies]\nframework-database = { path = \"../database\" }\n",
        )
        .unwrap();
        std::fs::write(
            framework.join("src/lib.rs"),
            "pub use framework_database as database;\n",
        )
        .unwrap();
        std::fs::write(
            app.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nframework = { path = \"../framework\" }\n",
        )
        .unwrap();
        let source = "struct App;\nstruct AppBuilder;\nimpl App { fn new() -> AppBuilder { AppBuilder } }\nimpl AppBuilder { fn name(self, n: &str) -> Self { self } async fn build(self) -> App { App } }\nfn main() {\n    framework::database::connect();\n    framework::database::DatabaseConfig::new();\n    let a = App::new().name(\"x\").build();\n}\n";
        let path = app.join("src/main.rs");
        std::fs::write(&path, source).unwrap();
        let mut engine = RustEngine::pending(root.path());
        engine.install(RustEngine::build(root.path()));
        let lib = database.join("src/lib.rs").canonicalize().unwrap();
        let column = |line: usize, needle: &str| {
            source.lines().nth(line).unwrap().find(needle).unwrap() as u32 + 1
        };

        let alias = engine.definition(&path, source, 5, column(5, "database"));
        assert_eq!(alias.len(), 1);
        assert_eq!(alias[0].path, lib);
        let connect = engine.definition(&path, source, 5, column(5, "connect"));
        assert_eq!(
            (connect.len(), connect[0].path.clone(), connect[0].line),
            (1, lib.clone(), 2)
        );
        let new = engine.definition(&path, source, 6, column(6, "new"));
        assert_eq!((new.len(), new[0].path.clone(), new[0].line), (1, lib, 1));
        let name = engine.definition(&path, source, 7, column(7, "name"));
        assert_eq!((name.len(), name[0].line), (1, 3));
        let build = engine.definition(&path, source, 7, column(7, "build"));
        assert_eq!((build.len(), build[0].line), (1, 3));
    }

    #[test]
    fn indexed_diagnostics_accept_imports_inside_cfg_test_module() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let path = root.path().join("src/lib.rs");
        let text = "pub fn connect() {}\n#[cfg(test)]\nmod tests {\n    use super::connect as open;\n    use std::mem::swap;\n    fn run() { open(); let (mut a, mut b) = (1, 2); swap(&mut a, &mut b); }\n}\n";
        std::fs::write(&path, text).unwrap();
        let snapshot = RustEngine::from_built(RustEngine::build(root.path()));
        assert!(snapshot.diagnostics_indexed(&path, text).is_empty());
    }

    #[test]
    fn definition_follows_method_chain_on_mut_let_binding() {
        let path = Path::new("/tmp/chain.rs");
        let source = "struct Column;\nimpl Column {\n    fn not_null(&mut self) -> &mut Self { self }\n    fn unique_key(&mut self) -> &mut Self { self }\n}\nstruct Schema;\nimpl Schema {\n    fn string(&self) -> Column { Column }\n}\nfn main() {\n    let schema = Schema;\n    let mut email = schema.string();\n    email.not_null().unique_key();\n}\nstruct Other;\nimpl Other {\n    fn not_null(&self) {}\n    fn unique_key(&self) {}\n}\n";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());

        let line = source.lines().nth(12).unwrap();
        let not_null = line.find("not_null").unwrap() as u32 + 1;
        let unique_key = line.find("unique_key").unwrap() as u32 + 1;
        assert_eq!(engine.definition(path, source, 12, not_null)[0].line, 2);
        assert_eq!(engine.definition(path, source, 12, unique_key)[0].line, 3);
    }

    #[test]
    fn dependency_tables_in_published_manifests_are_read() {
        let manifest = "[package]\nname = \"x\"\n[dependencies.sea-query]\nversion = \"0.32.5\"\nfeatures = [\n    \"a\",\n    \"b\",\n]\n[dependencies]\nserde = \"1\"\n";
        let dependencies = cargo_dependencies(
            Path::new("/tmp"),
            manifest,
            &HashSet::new(),
            &HashMap::new(),
        );
        let mut names: Vec<_> = dependencies.iter().map(|d| d.package.as_str()).collect();
        names.sort();
        assert_eq!(names, ["sea_query", "serde"]);
        let sea_query = dependencies
            .iter()
            .find(|d| d.package == "sea_query")
            .unwrap();
        assert!(sea_query.features.contains("a") && sea_query.features.contains("b"));
    }

    #[test]
    fn reexported_dependencies_are_limited_to_pub_use_crates() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(
            root.path().join("src/lib.rs"),
            "pub use sea_query;\nuse serde as _;\n",
        )
        .unwrap();
        let dependency = |name: &str| CargoDependency {
            name: name.into(),
            package: name.into(),
            path: None,
            git: false,
            optional: false,
            default_features: true,
            features: HashSet::new(),
        };
        let package = CargoPackage {
            name: "host".into(),
            root: root.path().to_path_buf(),
            targets: vec![CargoTarget {
                name: "host".into(),
                source_root: root.path().join("src"),
                root: root.path().join("src/lib.rs"),
            }],
            dependencies: vec![dependency("sea_query"), dependency("serde")],
            selected_features: HashSet::new(),
            indexed: true,
        };
        let found = reexported_dependencies(&package);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].package, "sea_query");
    }

    #[test]
    fn definition_resolves_associated_function_inside_macro_arguments() {
        let path = Path::new("/tmp/macro-path.rs");
        let source = "struct Table;\nimpl Table { fn create() -> Table { Table } }\nfn main() {\n    let rows = vec![Table::create()];\n}\n";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());

        let line = source.lines().nth(3).unwrap();
        let create = line.find("create").unwrap() as u32 + 1;
        let table = line.find("Table").unwrap() as u32 + 1;
        assert_eq!(engine.definition(path, source, 3, create)[0].line, 1);
        assert_eq!(engine.definition(path, source, 3, table)[0].line, 0);
    }

    #[test]
    fn member_matches_reuses_parsed_members_until_the_buffer_changes() {
        let path = Path::new("/tmp/member-cache.rs");
        let source = "struct Table;\nimpl Table { fn create() -> Table { Table } }\n";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        engine.sync_document(path, source);

        let first = engine.member_matches("Table", "create");
        assert_eq!(first.len(), 1);
        assert_eq!(engine.member_cache.lock().unwrap().len(), 1);
        let again = engine.member_matches("Table", "create");
        assert_eq!(
            (again[0].line, again[0].column),
            (first[0].line, first[0].column)
        );

        engine.sync_document(path, &format!("\n{source}"));
        let moved = engine.member_matches("Table", "create");
        assert_eq!(moved[0].line, first[0].line + 1);
    }

    #[test]
    fn resolve_path_ignores_bare_path_keywords() {
        let path = Path::new("/tmp/bare-keyword.rs");
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());

        for target in [
            vec!["crate"],
            vec!["self"],
            vec!["super"],
            vec!["super", "super"],
        ] {
            let target: Vec<String> = target.into_iter().map(String::from).collect();
            assert!(engine.resolve_path(path, &target).is_empty());
        }
    }

    #[test]
    fn standard_library_types_complete_before_associated_members() {
        let path = Path::new("/tmp/std-type.rs");
        let source = "fn main() { let value = Str }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());

        let completions = engine.complete(path, source, 0, 27);
        assert!(completions.iter().any(|item| item.label == "String"));
    }

    #[test]
    fn standard_library_collection_completion_adds_missing_import() {
        let path = Path::new("/tmp/std-collection.rs");
        let source = "#![allow(dead_code)]\nfn main() { let value = HashM }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());

        let completions = engine.complete(path, source, 1, 29);
        let item = completions
            .iter()
            .find(|item| item.label == "HashMap")
            .unwrap();
        assert_eq!(item.extra.len(), 1);
        assert_eq!(item.extra[0].line, 1);
        assert_eq!(item.extra[0].text, "use std::collections::HashMap;\n");

        let imported = "use std::collections::{HashMap, HashSet};\nfn main() { let value = HashM }";
        let completions = engine.complete(path, imported, 1, 29);
        let item = completions
            .iter()
            .find(|item| item.label == "HashMap")
            .unwrap();
        assert!(item.extra.is_empty());
    }

    #[test]
    fn parameter_hints_name_function_arguments() {
        let path = Path::new("/tmp/hints.rs");
        let source = "fn brew(\n    minutes: u8,\n    mut cups: HashMap<u8, u8>,\n) {}\nstruct Pot;\nimpl Pot { fn fill(&self, litres: u8) {} }\nfn main() { brew(5, map); brew(minutes, &cups); Pot::fill(&pot, 2); Some(1); }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        let found = engine.parameter_hints(path, source, 6, 6);
        let line = source.lines().nth(6).unwrap();
        let labels: Vec<(u32, &str)> = found
            .iter()
            .map(|hint| (hint.column, hint.label.as_str()))
            .collect();
        assert_eq!(
            labels,
            vec![
                (line.find("5,").unwrap() as u32, "minutes:"),
                (line.find("map").unwrap() as u32, "cups:"),
                (line.find("&pot").unwrap() as u32, "self:"),
                (line.find("2)").unwrap() as u32, "litres:"),
            ]
        );
    }

    #[test]
    fn let_bindings_without_a_type_get_one() {
        let path = Path::new("/tmp/lets.rs");
        let source = concat!(
            "struct Coffee;\n",
            "impl Coffee { fn new() -> Self { Coffee } fn cups() -> Vec<Self> { vec![] } }\n",
            "fn count() -> usize { 3 }\n",
            "fn pick<T>(item: T) -> T { item }\n",
            "fn main() {\n",
            "    let a = 89;\n",
            "    let b = 89u64;\n",
            "    let c = -1.5;\n",
            "    let d = \"hi\";\n",
            "    let mut e = true;\n",
            "    let f = Coffee;\n",
            "    let g = count();\n",
            "    let h = Coffee::new();\n",
            "    let i = Coffee::cups();\n",
            "    let j = pick(1);\n",
            "    let k: u8 = 4;\n",
            "    let (l, m) = (1, 2);\n",
            "    let n = 5;\n",
            "    takes(n);\n",
            "    let text = String::new();\n",
            "    println!(\"{a} {}\", a);\n",
            "}\n",
        );
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        let found = engine.parameter_hints(path, source, 0, 40);
        let labels: Vec<(u32, &str)> = found
            .iter()
            .filter(|hint| hint.label.starts_with(':'))
            .map(|hint| (hint.line, hint.label.as_str()))
            .collect();
        assert_eq!(
            labels,
            vec![
                (5, ": i32"),
                (6, ": u64"),
                (7, ": f64"),
                (8, ": &str"),
                (9, ": bool"),
                (11, ": usize"),
                (12, ": Coffee"),
                (13, ": Vec<Coffee>"),
                (19, ": String"),
            ]
        );
        let line = source.lines().nth(5).unwrap();
        assert_eq!(
            found.iter().find(|hint| hint.line == 5).unwrap().column as usize,
            line.find(" =").unwrap()
        );
    }

    #[test]
    fn literal_type_hints_work_while_indexing() {
        let path = Path::new("/tmp/pending-hints.rs");
        let source = "fn main() {\n    let answer = 42;\n}\n";
        let mut engine = RustEngine::pending(Path::new("/tmp"));

        assert!(!engine.is_ready());
        let found = engine.parameter_hints(path, source, 0, 2);
        assert!(
            found
                .iter()
                .any(|hint| hint.line == 1 && hint.label == ": i32")
        );
    }

    #[test]
    fn type_hint_names_vec_of_literals() {
        let path = Path::new("/tmp/vec-hints.rs");
        let source = "fn main() {\n    let mut x = vec![1, 2, 3];\n    let names = vec![\"a\", \"b\"];\n    let zeros = vec![0u8; 4];\n    let mixed = vec![1, \"a\"];\n    let items = vec![];\n    let used = vec![1, 2];\n    let total: u64 = used.iter().sum();\n}\n";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        let found = engine.parameter_hints(path, source, 0, 9);
        let label = |line: u32| {
            found
                .iter()
                .find(|hint| hint.line == line)
                .map(|hint| hint.label.as_str())
        };
        assert_eq!(label(1), Some(": Vec<i32>"));
        assert_eq!(label(2), Some(": Vec<&str>"));
        assert_eq!(label(3), Some(": Vec<u8>"));
        assert_eq!(label(4), None);
        assert_eq!(label(5), None);
        assert_eq!(label(6), None);
    }

    #[test]
    fn vec_type_hint_survives_push_and_index() {
        let path = Path::new("/tmp/vec-push-hints.rs");
        let source = "fn main() {\n    let  mut x = vec![1,2,3,4,5];\n    let first = &x[50];\n    x.push(5);\n    let mut y = vec![1, 2];\n    y.push(5u8);\n    let mut z = vec![1, 2];\n    let n: u8 = z[0];\n    let mut w = vec![1.5];\n    w.push(2.0);\n    println!(\"{:?}\", w);\n}\n";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        let found = engine.parameter_hints(path, source, 0, 12);
        let label = |line: u32| {
            found
                .iter()
                .find(|hint| hint.line == line)
                .map(|hint| hint.label.as_str())
        };
        assert_eq!(label(1), Some(": Vec<i32>"));
        assert_eq!(label(4), None);
        assert_eq!(label(6), None);
        assert_eq!(label(8), Some(": Vec<f64>"));
    }

    #[test]
    fn member_completion_uses_workspace_definitions() {
        let definition = PathBuf::from("/tmp/models.rs");
        let path = Path::new("/tmp/lib.rs");
        let source = "fn main() { let coffee: Coffee = todo!(); coffee.br }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built {
            declarations: Vec::new(),
            files: vec![definition.clone()],
            uses: HashMap::new(),
            module_identities: HashMap::new(),
            source_roots: vec![PathBuf::from("/tmp")],
            packages: Vec::new(),
        });
        engine.sync_document(
            &definition,
            "pub struct Coffee { pub beans: u8 }\nimpl Coffee { pub fn brew(&self) {} }",
        );
        assert!(
            engine
                .complete(path, source, 0, 55)
                .iter()
                .any(|item| item.label == "brew")
        );
    }

    #[test]
    fn cargo_workspace_source_roots_include_members_custom_targets_and_globs() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/core\", \"tools/*\"]\n",
        )
        .unwrap();
        for package in ["crates/core", "tools/cli", "tools/codegen"] {
            let package = root.path().join(package);
            std::fs::create_dir_all(package.join("src")).unwrap();
            std::fs::write(package.join("Cargo.toml"), "[package]\nname = \"test\"\n").unwrap();
        }
        let custom = root.path().join("crates/core/generated");
        std::fs::create_dir_all(&custom).unwrap();
        std::fs::write(
            root.path().join("crates/core/Cargo.toml"),
            "[package]\nname = \"core\"\n[[bin]]\npath = \"generated/main.rs\"\n",
        )
        .unwrap();

        let roots = cargo_source_roots(root.path());
        assert_eq!(
            roots,
            vec![
                root.path().join("crates/core/generated"),
                root.path().join("crates/core/src"),
                root.path().join("tools/cli/src"),
                root.path().join("tools/codegen/src"),
            ]
        );
    }

    #[test]
    fn cargo_workspace_source_roots_include_excluded_packages() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/core\"]\nexclude = [\"fuzz\"]\n",
        )
        .unwrap();
        for package in ["crates/core", "fuzz"] {
            let package = root.path().join(package);
            std::fs::create_dir_all(package.join("src")).unwrap();
            std::fs::write(package.join("Cargo.toml"), "[package]\nname = \"test\"\n").unwrap();
        }

        assert_eq!(
            cargo_source_roots(root.path()),
            vec![
                root.path().join("crates/core/src"),
                root.path().join("fuzz/src"),
            ]
        );
    }

    #[test]
    fn project_root_is_the_workspace_that_lists_the_package() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"fuzz\"]\n",
        )
        .unwrap();
        for package in ["crates/core", "fuzz", "vendor/other"] {
            let package = root.path().join(package);
            std::fs::create_dir_all(package.join("src")).unwrap();
            std::fs::write(package.join("Cargo.toml"), "[package]\nname = \"test\"\n").unwrap();
        }

        assert_eq!(
            project_root(&root.path().join("crates/core/src/lib.rs")),
            root.path()
        );
        assert_eq!(
            project_root(&root.path().join("fuzz/src/main.rs")),
            root.path()
        );
        assert_eq!(
            project_root(&root.path().join("vendor/other/src/lib.rs")),
            root.path().join("vendor/other")
        );
    }

    #[test]
    fn references_include_uses_from_excluded_workspace_packages() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        std::fs::write(
            base.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/core\"]\nexclude = [\"fuzz\"]\n",
        )
        .unwrap();
        let core = base.join("crates/core");
        std::fs::create_dir_all(core.join("src")).unwrap();
        std::fs::write(core.join("Cargo.toml"), "[package]\nname = \"rpccore\"\n").unwrap();
        let lib = core.join("src/lib.rs");
        let rpc = core.join("src/rpc.rs");
        std::fs::write(&lib, "pub mod rpc;\n").unwrap();
        let declaration = "pub fn parse_payload(bytes: &[u8]) -> bool { bytes.is_empty() }\n";
        std::fs::write(&rpc, declaration).unwrap();
        let fuzz = base.join("fuzz");
        std::fs::create_dir_all(fuzz.join("fuzz_targets")).unwrap();
        std::fs::write(
            fuzz.join("Cargo.toml"),
            "[package]\nname = \"fuzz\"\n\n[dependencies]\nrpccore = { path = \"../crates/core\" }\n\n\
             [[bin]]\nname = \"payload\"\npath = \"fuzz_targets/payload.rs\"\n",
        )
        .unwrap();
        let target = fuzz.join("fuzz_targets/payload.rs");
        std::fs::write(
            &target,
            "use rpccore::rpc::parse_payload;\nfn main() { let _ = parse_payload(b\"\"); }\n",
        )
        .unwrap();

        let project = project_root(&rpc);
        assert_eq!(project, base);
        let mut engine = RustEngine::pending(&project);
        engine.install(RustEngine::build(&project));
        let found = engine.references(&rpc, declaration, 0, 8);
        let mut lines: Vec<_> = found
            .iter()
            .map(|usage| (usage.path.clone(), usage.line))
            .collect();
        lines.sort();
        assert_eq!(
            lines,
            vec![(rpc.clone(), 0), (target.clone(), 0), (target, 1)],
            "{found:?}"
        );
    }

    #[test]
    fn index_cache_reuses_unchanged_files_and_reparses_changed_ones() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.path().join("src/net")).unwrap();
        std::fs::write(
            root.path().join("src/lib.rs"),
            "pub mod net;\nmod inline { pub mod deep; }\npub use net::Client;\n",
        )
        .unwrap();
        std::fs::write(root.path().join("src/net/mod.rs"), "pub struct Client;\n").unwrap();
        std::fs::create_dir_all(root.path().join("src/inline")).unwrap();
        std::fs::write(root.path().join("src/inline/deep.rs"), "pub fn deep() {}\n").unwrap();
        let names = |built: &Built| {
            let mut names = built
                .declarations
                .iter()
                .map(|item| (item.name.clone(), item.module.clone(), item.line))
                .collect::<Vec<_>>();
            names.sort();
            names
        };

        let cold = RustEngine::build(root.path());
        let mut cache = RustIndexCache::default();
        let first = RustEngine::build_with_cache(root.path(), &mut cache);
        let file = root.path().join("cache.bin");
        cache.save(&file, root.path()).unwrap();
        let mut cache = RustIndexCache::load(&file, root.path());
        assert_eq!(cache.files.len(), first.files.len());
        let warm = RustEngine::build_with_cache(root.path(), &mut cache);
        assert_eq!(names(&warm), names(&cold));
        assert_eq!(warm.module_identities, cold.module_identities);
        assert_eq!(warm.uses.len(), cold.uses.len());

        std::fs::write(
            root.path().join("src/net/mod.rs"),
            "pub struct Client;\npub struct Server;\n",
        )
        .unwrap();
        let changed = RustEngine::build_with_cache(root.path(), &mut cache);
        assert!(
            changed
                .declarations
                .iter()
                .any(|item| item.name == "Server" && item.module == ["net"])
        );
        assert_eq!(names(&changed), names(&RustEngine::build(root.path())));
    }

    #[test]
    fn index_cache_ignores_cache_of_another_root() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/lib.rs"), "pub fn run() {}\n").unwrap();
        let mut cache = RustIndexCache::default();
        RustEngine::build_with_cache(root.path(), &mut cache);
        let file = root.path().join("cache.bin");
        cache.save(&file, root.path()).unwrap();
        assert!(RustIndexCache::load(&file, other.path()).files.is_empty());
    }

    #[test]
    fn diagnostics_fingerprint_ignores_bodies_but_tracks_declarations() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        let lib = root.path().join("src/lib.rs");
        let fingerprint =
            || RustEngine::from_built(RustEngine::build(root.path())).diagnostics_fingerprint();

        std::fs::write(&lib, "pub fn run() {}\n").unwrap();
        let original = fingerprint();
        std::fs::write(&lib, "\n\npub fn run() {\n    let x = 1;\n}\n").unwrap();
        assert_eq!(fingerprint(), original);

        std::fs::write(&lib, "pub fn run() {}\npub fn stop() {}\n").unwrap();
        assert_ne!(fingerprint(), original);
    }

    #[test]
    fn use_path_module_segment_prefers_reexported_crate_over_local_function() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"oikos\", \"oikos-hash\"]\n",
        )
        .unwrap();
        for package in ["app", "oikos", "oikos-hash"] {
            std::fs::create_dir_all(root.path().join(package).join("src")).unwrap();
        }
        std::fs::write(
            root.path().join("app/Cargo.toml"),
            "[workspace]\n[package]\nname = \"app\"\n[dependencies]\noikos={path=\"../oikos\", features=[\"hash\", \"logging\"]}\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("oikos/Cargo.toml"),
            "[package]\nname = \"oikos\"\n[features]\nhash = [\"dep:oikos-hash\"]\n[dependencies]\noikos-hash = { path = \"../oikos-hash\", optional = true }\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("oikos/src/lib.rs"),
            "#[cfg(feature = \"hash\")]\npub use oikos_hash as hash;\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("oikos-hash/Cargo.toml"),
            "[package]\nname = \"oikos-hash\"\n",
        )
        .unwrap();
        let hash_lib = root.path().join("oikos-hash/src/lib.rs");
        std::fs::write(
            &hash_lib,
            "pub struct HashConfig;\npub struct HashSettings;\n",
        )
        .unwrap();
        let app = root.path().join("app/src/lib.rs");
        let source = "use oikos::hash::{HashConfig, HashSettings};\npub fn hash() {}\n";
        std::fs::write(&app, source).unwrap();

        let mut engine = RustEngine::pending(root.path());
        engine.install(RustEngine::build(root.path()));
        assert_eq!(engine.crate_for(&app), "app");
        let column = source.lines().next().unwrap().find("hash").unwrap() as u32 + 1;
        let definitions = engine.definition(&app, source, 0, column);
        assert_eq!(definitions.len(), 1, "{definitions:?}");
        assert_eq!(definitions[0].path, hash_lib.canonicalize().unwrap());
        let hover = engine.hover(&app, source, 0, column).unwrap();
        assert!(hover.markdown.contains("crate oikos_hash"), "{hover:?}");
    }

    #[test]
    fn path_dependency_alias_resolves_only_the_declared_package() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\", \"library\", \"other\"]\n",
        )
        .unwrap();
        for package in ["app", "library", "other"] {
            std::fs::create_dir_all(root.path().join(package).join("src")).unwrap();
        }
        std::fs::write(
            root.path().join("app/Cargo.toml"),
            "[package]\nname = \"app\"\n[dependencies]\nrenamed = { package = \"library\", path = \"../library\" }\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("library/Cargo.toml"),
            "[package]\nname = \"library\"\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("other/Cargo.toml"),
            "[package]\nname = \"other\"\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("library/src/lib.rs"),
            "pub struct Widget;\n",
        )
        .unwrap();
        std::fs::write(root.path().join("other/src/lib.rs"), "pub struct Widget;\n").unwrap();
        let app = root.path().join("app/src/lib.rs");
        let source = "use renamed::Widget;\nfn main() { Widget; }\n";
        std::fs::write(&app, source).unwrap();

        let mut engine = RustEngine::pending(root.path());
        engine.install(RustEngine::build(root.path()));
        let target = engine.definition(&app, source, 1, 14);
        assert_eq!(target.len(), 1);
        assert_eq!(
            target[0].path,
            root.path()
                .join("library/src/lib.rs")
                .canonicalize()
                .unwrap()
        );
    }

    #[test]
    fn out_of_line_modules_and_disabled_cfg_items_are_indexed_correctly() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src/api")).unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\n",
        )
        .unwrap();
        let lib = root.path().join("src/lib.rs");
        std::fs::write(&lib, "mod api;\n#[cfg(false)]\npub struct Hidden;\n").unwrap();
        let endpoint = root.path().join("src/api/mod.rs");
        std::fs::write(&endpoint, "pub struct Endpoint;\n").unwrap();

        let mut engine = RustEngine::pending(root.path());
        engine.install(RustEngine::build(root.path()));
        let source = "use crate::api::Endpoint;\nfn main() { Endpoint; }\n";
        let target = engine.definition(&lib, source, 1, 14);
        assert_eq!(target.len(), 1);
        assert_eq!(target[0].path, endpoint.canonicalize().unwrap());
        assert!(!engine.declarations.iter().any(|item| item.name == "Hidden"));
    }

    #[test]
    fn path_modules_use_declared_identity_for_nested_imports() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src/generated/api")).unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\n",
        )
        .unwrap();
        let lib = root.path().join("src/lib.rs");
        std::fs::write(&lib, "#[path = \"generated/api.rs\"]\npub mod api;\n").unwrap();
        std::fs::write(
            root.path().join("src/generated/api.rs"),
            "pub mod nested;\n",
        )
        .unwrap();
        let nested = root.path().join("src/generated/api/nested.rs");
        std::fs::write(&nested, "pub struct Endpoint;\n").unwrap();

        let mut engine = RustEngine::pending(root.path());
        engine.install(RustEngine::build(root.path()));
        let source = "use crate::api::nested::Endpoint;\nfn main() { Endpoint; }\n";
        let target = definition_at(&mut engine, &lib, source, "Endpoint");
        assert_eq!(target.len(), 1);
        assert_eq!(target[0].path, nested.canonicalize().unwrap());
    }

    #[test]
    fn custom_target_roots_seed_the_static_module_graph() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("generated")).unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\n[[bin]]\npath = \"generated/main.rs\"\n",
        )
        .unwrap();
        let main = root.path().join("generated/main.rs");
        std::fs::write(&main, "#[path = \"api.rs\"] mod api;\n").unwrap();
        let api = root.path().join("generated/api.rs");
        std::fs::write(&api, "pub struct Endpoint;\n").unwrap();

        let mut engine = RustEngine::pending(root.path());
        engine.install(RustEngine::build(root.path()));
        let source = "use crate::api::Endpoint;\nfn main() { Endpoint; }\n";
        assert_eq!(
            definition_at(&mut engine, &main, source, "Endpoint")[0].path,
            api.canonicalize().unwrap()
        );
    }

    #[test]
    fn literal_includes_share_the_including_module_identity_and_imports() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src/parts")).unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\n",
        )
        .unwrap();
        let lib = root.path().join("src/lib.rs");
        std::fs::write(
            &lib,
            "pub struct Root;\ninclude!(\"parts/root.rs\");\nmod api { include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/src/parts/api.rs\")); }\n",
        )
        .unwrap();
        let root_include = root.path().join("src/parts/root.rs");
        std::fs::write(
            &root_include,
            "use crate::Root;\npub fn root() -> Root { Root }\n",
        )
        .unwrap();
        let api_include = root.path().join("src/parts/api.rs");
        std::fs::write(&api_include, "pub struct Endpoint;\n").unwrap();

        let mut engine = RustEngine::pending(root.path());
        engine.install(RustEngine::build(root.path()));
        let root_source = std::fs::read_to_string(&root_include).unwrap();
        assert!(!definition_at(&mut engine, &root_include, &root_source, "Root").is_empty());
        let source = "use crate::api::Endpoint;\nfn main() { Endpoint; }\n";
        assert_eq!(
            definition_at(&mut engine, &lib, source, "Endpoint")[0].path,
            api_include.canonicalize().unwrap()
        );
    }

    #[test]
    fn dynamic_includes_are_not_read() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\n",
        )
        .unwrap();
        let lib = root.path().join("src/lib.rs");
        std::fs::write(
            &lib,
            "include!(concat!(env!(\"OUT_DIR\"), \"/generated.rs\"));\npub struct Local;\n",
        )
        .unwrap();

        let mut engine = RustEngine::pending(root.path());
        engine.install(RustEngine::build(root.path()));
        assert!(engine.declarations.iter().any(|item| item.name == "Local"));
        assert!(
            engine
                .diagnostics(&lib, &std::fs::read_to_string(&lib).unwrap())
                .is_empty()
        );
    }

    #[test]
    fn out_dir_include_uses_one_local_build_output() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\n",
        )
        .unwrap();
        let lib = root.path().join("src/lib.rs");
        std::fs::write(
            &lib,
            "include!(concat!(env!(\"OUT_DIR\"), \"/generated.rs\"));\n",
        )
        .unwrap();
        let generated = root
            .path()
            .join("target/debug/build/app-hash/out/generated.rs");
        std::fs::create_dir_all(generated.parent().unwrap()).unwrap();
        std::fs::write(&generated, "pub struct Generated;\n").unwrap();

        let mut engine = RustEngine::pending(root.path());
        engine.install(RustEngine::build(root.path()));
        let source = "fn main() { Generated; }\n";
        let target = definition_at(&mut engine, &lib, source, "Generated");
        assert_eq!(target.len(), 1);
        assert_eq!(target[0].path, generated.canonicalize().unwrap());
    }

    #[test]
    fn out_dir_include_ignores_ambiguous_or_unsafe_outputs() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        for build in ["one", "two"] {
            let output = target.join(format!("debug/build/{build}/out/generated.rs"));
            std::fs::create_dir_all(output.parent().unwrap()).unwrap();
            std::fs::write(output, "pub struct Generated;\n").unwrap();
        }
        assert_eq!(out_dir_include_file_in(&target, "/generated.rs"), None);
        assert_eq!(out_dir_include_file_in(&target, "../generated.rs"), None);
        assert_eq!(out_dir_include_file_in(&target, "/tmp/generated.rs"), None);
    }

    #[test]
    fn manifest_dependencies_resolve_local_and_external_packages() {
        let dependencies = cargo_dependencies(
            Path::new("/project"),
            "[dependencies]\nlocal = { package = \"local-package\", path = \"../local\", version = \"1\" }\nregistry = \"2.0\"\ngit = { git = \"https://example.test/repo\", registry = \"private\" }\n",
            &HashSet::new(),
            &HashMap::new(),
        );
        assert_eq!(dependencies.len(), 3);
        assert_eq!(dependencies[0].package, "local_package");
        assert_eq!(
            dependencies[0].path,
            Some(PathBuf::from("/project/../local"))
        );
        assert_eq!(dependencies[1].path, None);
        assert_eq!(dependencies[1].package, "registry");
        assert_eq!(dependencies[2].package, "git");
    }

    #[test]
    fn cfg_evaluator_uses_mac_arm_and_package_default_features() {
        let cfg = CfgContext {
            features: ["editor".to_owned()].into_iter().collect(),
        };
        assert!(cfg_matches(
            "all(unix, target_os = \"macos\", target_arch = \"aarch64\", feature = \"editor\")",
            &cfg
        ));
        assert!(cfg_matches("any(not(feature = \"missing\"), macos)", &cfg));
        assert!(!cfg_matches("all(feature = \"missing\", unix)", &cfg));
    }

    #[test]
    fn registry_cache_locator_uses_the_locked_name_and_version() {
        let root = tempfile::tempdir().unwrap();
        let cargo = root.path().join("cargo");
        let source = cargo.join("registry/src/example-registry/widget-1.2.3");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("Cargo.toml"),
            "[package]\nname = \"widget\"\nversion = \"1.2.3\"\n",
        )
        .unwrap();
        // The locator reads no registry index and creates no files.
        assert_eq!(
            registry_package_source_in(&cargo, "widget", "1.2.3"),
            Some(source)
        );
    }

    #[test]
    fn target_dependencies_and_local_optional_features_are_selected() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\n",
        )
        .unwrap();
        for package in ["app", "optional", "mac"] {
            std::fs::create_dir_all(root.path().join(package).join("src")).unwrap();
        }
        std::fs::write(
            root.path().join("app/Cargo.toml"),
            "[package]\nname = \"app\"\n[dependencies]\noptional = { path = \"../optional\", optional = true }\n[target.'cfg(target_os = \"macos\")'.dependencies]\nmac = { path = \"../mac\", features = [\"enabled\"] }\n[features]\ndefault = [\"dep:optional\"]\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("optional/Cargo.toml"),
            "[package]\nname = \"optional\"\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("mac/Cargo.toml"),
            "[package]\nname = \"mac\"\n[features]\nenabled = []\n",
        )
        .unwrap();
        let package = cargo_package(root.path().join("app").as_path()).unwrap();
        assert_eq!(package.dependencies.len(), 2);
        assert!(
            package
                .dependencies
                .iter()
                .any(|dependency| dependency.name == "optional")
        );
        assert!(
            package
                .dependencies
                .iter()
                .any(|dependency| dependency.name == "mac")
        );
        let packages = cargo_packages(root.path());
        assert!(
            packages
                .iter()
                .find(|package| package.name == "mac")
                .is_some_and(|package| package.selected_features.contains("enabled"))
        );
    }

    #[test]
    fn local_dependency_feature_forwarding_is_selected() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("app/src")).unwrap();
        std::fs::create_dir_all(root.path().join("library/src")).unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("app/Cargo.toml"),
            "[package]\nname = \"app\"\n[dependencies]\nlibrary = { path = \"../library\" }\n[features]\ndefault = [\"with-library\"]\nwith-library = [\"library/extra\"]\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("library/Cargo.toml"),
            "[package]\nname = \"library\"\n[features]\nextra = []\n",
        )
        .unwrap();

        let packages = cargo_packages(root.path());
        assert!(
            packages
                .iter()
                .find(|package| package.name == "library")
                .is_some_and(|package| package.selected_features.contains("extra"))
        );
    }

    #[test]
    fn public_reexports_follow_aliases_groups_globs_and_stop_at_cycles() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src/api")).unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\n",
        )
        .unwrap();
        let lib = root.path().join("src/lib.rs");
        std::fs::write(
            &lib,
            "pub mod api;\npub use api::{Alias, *};\npub use loop_a::Missing;\nmod loop_a { pub use super::loop_b::Missing; }\nmod loop_b { pub use super::loop_a::Missing; }\n",
        )
        .unwrap();
        let api = root.path().join("src/api/mod.rs");
        std::fs::write(&api, "pub mod inner;\npub use inner::Widget as Alias;\n").unwrap();
        let inner = root.path().join("src/api/inner.rs");
        std::fs::write(&inner, "pub struct Widget;\npub struct Other;\n").unwrap();

        let mut engine = RustEngine::pending(root.path());
        engine.install(RustEngine::build(root.path()));
        let source = "use crate::{Alias, Other};\nfn main() { Alias; Other; Missing; }\n";
        assert_eq!(
            definition_at(&mut engine, &lib, source, "Alias")[0].path,
            inner.canonicalize().unwrap()
        );
        assert_eq!(
            definition_at(&mut engine, &lib, source, "Other")[0].path,
            inner.canonicalize().unwrap()
        );
        assert!(definition_at(&mut engine, &lib, source, "Missing").is_empty());
    }

    #[test]
    fn private_reexports_do_not_escape_their_module() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\n",
        )
        .unwrap();
        let lib = root.path().join("src/lib.rs");
        std::fs::write(
            &lib,
            "pub mod api { pub struct Widget; use Widget as Alias; }\n",
        )
        .unwrap();
        let mut engine = RustEngine::pending(root.path());
        engine.install(RustEngine::build(root.path()));
        let source = "use crate::api::Alias;\nfn main() { Alias; }\n";
        assert!(definition_at(&mut engine, &lib, source, "Alias").is_empty());
    }

    #[test]
    fn cargo_target_roots_cover_all_target_kinds_and_custom_paths() {
        let root = tempfile::tempdir().unwrap();
        for path in [
            "src/lib.rs",
            "src/main.rs",
            "src/bin/tool.rs",
            "examples/demo.rs",
            "tests/integration.rs",
            "benches/speed.rs",
            "generated/custom.rs",
        ] {
            let path = root.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\n[[example]]\npath = \"generated/custom.rs\"\n",
        )
        .unwrap();
        assert_eq!(cargo_target_roots(root.path()).len(), 7);
    }

    #[test]
    fn weak_dependency_features_do_not_enable_optional_dependencies() {
        let definitions =
            HashMap::from([("with-extra".to_owned(), vec!["optional?/extra".to_owned()])]);
        let selected = ["with-extra".to_owned()].into_iter().collect();
        assert!(forwarded_dependency_features("optional", &selected, &definitions).is_empty());
        let selected = ["with-extra".to_owned(), "optional".to_owned()]
            .into_iter()
            .collect();
        assert!(
            forwarded_dependency_features("optional", &selected, &definitions).contains("extra")
        );
    }

    #[test]
    fn disabled_dependency_defaults_do_not_select_local_default_features() {
        let root = tempfile::tempdir().unwrap();
        for package in ["app", "library"] {
            std::fs::create_dir_all(root.path().join(package).join("src")).unwrap();
        }
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("app/Cargo.toml"),
            "[package]\nname = \"app\"\n[dependencies]\nlibrary = { path = \"../library\", default-features = false }\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("library/Cargo.toml"),
            "[package]\nname = \"library\"\n[features]\ndefault = [\"enabled\"]\nenabled = []\n",
        )
        .unwrap();
        let packages = cargo_packages(root.path());
        assert!(
            !packages
                .iter()
                .find(|package| package.name == "library")
                .is_some_and(|package| package.selected_features.contains("enabled"))
        );
    }

    #[test]
    fn private_imports_do_not_cross_module_boundaries() {
        let root = Path::new("/project/src");
        let mut engine = RustEngine::pending(Path::new("/project"));
        engine.install(Built {
            declarations: vec![Declaration {
                name: "Hidden".to_owned(),
                crate_name: String::new(),
                module: vec!["models".to_owned()],
                visibility: Visibility::Private,
                kind: ItemKind::Class,
                path: PathBuf::from("/project/src/models.rs"),
                line: 0,
                column: 7,
                scope_depth: 0,
                signature: "struct Hidden;".to_owned(),
            }],
            files: Vec::new(),
            uses: HashMap::new(),
            module_identities: HashMap::new(),
            source_roots: vec![root.to_path_buf()],
            packages: Vec::new(),
        });
        let source = "use crate::models::Hidden;\nfn main() { Hidden; }";
        assert!(
            engine
                .definition(Path::new("/project/src/lib.rs"), source, 1, 14)
                .is_empty()
        );
    }

    #[test]
    fn bounded_visibility_follows_module_and_crate_scopes() {
        let root = tempfile::tempdir().unwrap();
        let library = root.path().join("library");
        let app = root.path().join("app");
        for package in [&library, &app] {
            std::fs::create_dir_all(package.join("src")).unwrap();
        }
        std::fs::create_dir_all(library.join("src/child")).unwrap();
        std::fs::create_dir_all(library.join("src/api")).unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"library\", \"app\"]\n",
        )
        .unwrap();
        std::fs::write(
            library.join("Cargo.toml"),
            "[package]\nname = \"library\"\n",
        )
        .unwrap();
        std::fs::write(
            app.join("Cargo.toml"),
            "[package]\nname = \"app\"\n[dependencies]\nlibrary = { path = \"../library\" }\n",
        )
        .unwrap();
        std::fs::write(
            library.join("src/lib.rs"),
            "pub mod child;\npub mod sibling;\npub mod api;\npub(crate) fn crate_only() {}\npub(in crate::api) fn api_only() {}\npub fn external() {}\n",
        )
        .unwrap();
        std::fs::write(
            library.join("src/child.rs"),
            "pub mod nested;\nfn private_child() {}\npub(super) fn parent_visible() {}\n",
        )
        .unwrap();
        std::fs::write(
            library.join("src/child/nested.rs"),
            "use super::private_child;\nfn use_private() { private_child(); }\n",
        )
        .unwrap();
        std::fs::write(
            library.join("src/sibling.rs"),
            "use crate::{crate_only, child::{private_child, parent_visible}};\nfn use_parent() { crate_only(); parent_visible(); }\n",
        )
        .unwrap();
        std::fs::write(library.join("src/api.rs"), "pub mod nested;\n").unwrap();
        std::fs::write(
            library.join("src/api/nested.rs"),
            "use crate::api_only;\nfn use_api() { api_only(); }\n",
        )
        .unwrap();
        std::fs::write(
            app.join("src/lib.rs"),
            "use library::{crate_only, external};\nfn use_external() { external(); }\n",
        )
        .unwrap();

        let mut engine = RustEngine::pending(root.path());
        engine.install(RustEngine::build(root.path()));
        let child_nested = library.join("src/child/nested.rs");
        let child_source = std::fs::read_to_string(&child_nested).unwrap();
        assert!(
            !definition_at(&mut engine, &child_nested, &child_source, "private_child").is_empty()
        );

        let sibling = library.join("src/sibling.rs");
        let sibling_source = std::fs::read_to_string(&sibling).unwrap();
        assert!(definition_at(&mut engine, &sibling, &sibling_source, "private_child").is_empty());
        assert!(!definition_at(&mut engine, &sibling, &sibling_source, "crate_only").is_empty());
        assert!(
            !definition_at(&mut engine, &sibling, &sibling_source, "parent_visible").is_empty()
        );

        let api_nested = library.join("src/api/nested.rs");
        let api_source = std::fs::read_to_string(&api_nested).unwrap();
        assert!(!definition_at(&mut engine, &api_nested, &api_source, "api_only").is_empty());
        let library_root = library.join("src/lib.rs");
        let outside_api = "use crate::api_only;\nfn outside() { api_only(); }\n";
        assert!(definition_at(&mut engine, &library_root, outside_api, "api_only").is_empty());

        let app_root = app.join("src/lib.rs");
        let app_source = std::fs::read_to_string(&app_root).unwrap();
        assert!(definition_at(&mut engine, &app_root, &app_source, "crate_only").is_empty());
        assert!(!definition_at(&mut engine, &app_root, &app_source, "external").is_empty());
    }

    #[test]
    fn child_glob_import_resolves_parent_private_reexport() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src/parent")).unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\n",
        )
        .unwrap();
        std::fs::write(root.path().join("src/lib.rs"), "mod parent;\n").unwrap();
        std::fs::write(
            root.path().join("src/parent.rs"),
            "mod ambient;\nmod syntax;\nuse self::syntax::*;\n",
        )
        .unwrap();
        let syntax = root.path().join("src/parent/syntax.rs");
        std::fs::write(&syntax, "pub(super) fn language_for_path() {}\n").unwrap();
        let ambient = root.path().join("src/parent/ambient.rs");
        let source = "use super::*;\nfn ambient() { language_for_path(); }\n";
        std::fs::write(&ambient, source).unwrap();

        let mut engine = RustEngine::pending(root.path());
        engine.install(RustEngine::build(root.path()));
        let definitions = definition_at(&mut engine, &ambient, source, "language_for_path");
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].path, syntax.canonicalize().unwrap());
        assert!(
            engine
                .diagnostics(&ambient, source)
                .iter()
                .all(|problem| !problem.message.contains("language_for_path"))
        );
    }

    #[test]
    fn parses_all_bounded_visibility_forms() {
        let declarations = parse_declarations(
            Path::new("/project/src/lib.rs"),
            "fn private() {}\npub fn public() {}\npub(crate) fn crate_only() {}\npub(self) fn self_only() {}\npub(super) fn parent() {}\npub(in crate::api) fn api() {}\nmod child { pub(in super::super::api) fn ancestor_api() {} }\n",
            Path::new("/project/src"),
            "project",
            &CfgContext::default(),
            None,
        );
        let visibility = |name| {
            declarations
                .iter()
                .find(|item| item.name == name)
                .unwrap()
                .visibility
                .clone()
        };
        assert_eq!(visibility("private"), Visibility::Private);
        assert_eq!(visibility("public"), Visibility::Public);
        assert_eq!(visibility("crate_only"), Visibility::Restricted(Vec::new()));
        assert_eq!(visibility("self_only"), Visibility::Restricted(Vec::new()));
        assert_eq!(visibility("parent"), Visibility::Restricted(Vec::new()));
        assert_eq!(
            visibility("api"),
            Visibility::Restricted(vec!["api".to_owned()])
        );
        assert_eq!(
            visibility("ancestor_api"),
            Visibility::Restricted(vec!["api".to_owned()])
        );
    }

    #[test]
    fn diagnostics_report_missing_and_duplicate_out_of_line_modules() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("lib.rs");
        std::fs::write(&source, "mod absent;\nmod both;\n").unwrap();
        std::fs::write(root.path().join("both.rs"), "").unwrap();
        std::fs::create_dir(root.path().join("both")).unwrap();
        std::fs::write(root.path().join("both/mod.rs"), "").unwrap();
        let mut engine = RustEngine::pending(root.path());
        let problems = engine.diagnostics(&source, "mod absent;\nmod both;\n");
        assert!(
            problems
                .iter()
                .any(|problem| problem.message == "module `absent` not found")
        );
        assert!(
            problems
                .iter()
                .any(|problem| problem.message == "module `both` found at both conventional paths")
        );
    }

    #[test]
    fn diagnostics_ignore_cfg_test_modules_in_normal_static_context() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("lib.rs");
        let text = "#[cfg(test)]\nmod missing_tests;\nmod missing;\n";
        let mut engine = RustEngine::pending(root.path());
        let problems = engine.diagnostics(&source, text);
        assert!(
            !problems
                .iter()
                .any(|problem| problem.message == "module `missing_tests` not found")
        );
        assert!(
            problems
                .iter()
                .any(|problem| problem.message == "module `missing` not found")
        );
    }

    #[test]
    fn diagnostics_ignore_cfg_test_modules_with_path_attribute() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("lib.rs");
        let text = "#[cfg(test)]\n#[path = \"missing_tests.rs\"]\nmod missing_tests;\n";
        let mut engine = RustEngine::pending(root.path());
        let problems = engine.diagnostics(&source, text);
        assert!(
            !problems
                .iter()
                .any(|problem| problem.message == "module `missing_tests` not found")
        );
    }

    #[test]
    fn diagnostics_respect_literal_path_module_overrides() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("lib.rs");
        std::fs::create_dir_all(root.path().join("generated")).unwrap();
        std::fs::write(root.path().join("generated/api.rs"), "").unwrap();
        let text = "#[path = \"generated/api.rs\"]\nmod api;\n";
        let mut engine = RustEngine::pending(root.path());
        assert!(engine.diagnostics(&source, text).is_empty());
    }

    #[test]
    fn member_definition_resolves_methods_and_fields_in_another_buffer() {
        let definition = PathBuf::from("/tmp/models.rs");
        let path = Path::new("/tmp/lib.rs");
        let source = "fn main() { let coffee: Coffee = todo!(); coffee.brew(); coffee.beans; }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built {
            declarations: Vec::new(),
            files: vec![definition.clone()],
            uses: HashMap::new(),
            module_identities: HashMap::new(),
            source_roots: vec![PathBuf::from("/tmp")],
            packages: Vec::new(),
        });
        engine.sync_document(
            &definition,
            "pub struct Coffee { pub beans: u8 }\nimpl Coffee {\n    pub fn brew(&self) {}\n}",
        );

        let method_column = source.find("brew").unwrap() as u32 + 1;
        let method = engine.definition(path, source, 0, method_column);
        assert_eq!(method.len(), 1);
        assert_eq!(method[0].path, definition);
        assert_eq!((method[0].line, method[0].column), (2, 11));

        let field_column = source.rfind("beans").unwrap() as u32 + 1;
        let field = engine.definition(path, source, 0, field_column);
        assert_eq!(field.len(), 1);
        assert_eq!(field[0].path, definition);
        assert_eq!((field[0].line, field[0].column), (0, 24));
    }

    #[test]
    fn enum_variant_definition_resolves_through_parent_glob_import() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src/parent")).unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\n",
        )
        .unwrap();
        std::fs::write(root.path().join("src/lib.rs"), "mod parent;\n").unwrap();
        std::fs::write(
            root.path().join("src/parent.rs"),
            "mod syntax;\npub enum DocumentLanguage { TypeScript, Tsx }\n",
        )
        .unwrap();
        let syntax = root.path().join("src/parent/syntax.rs");
        let source = "use super::*;\nfn language(value: DocumentLanguage) { match value { DocumentLanguage::TypeScript => {} } }\n";
        std::fs::write(&syntax, source).unwrap();
        let mut engine = RustEngine::pending(root.path());
        engine.install(RustEngine::build(root.path()));

        let column = source.lines().nth(1).unwrap().find("TypeScript").unwrap() as u32 + 1;
        let target = engine.definition(&syntax, source, 1, column);
        assert_eq!(target.len(), 1);
        assert_eq!(
            target[0].path,
            root.path().join("src/parent.rs").canonicalize().unwrap()
        );
        assert_eq!((target[0].line, target[0].column), (1, 28));
    }

    #[test]
    fn definition_resolves_dependency_constant_and_into_trait_method() {
        let root = tempfile::tempdir().unwrap();
        let dependency = root.path().join("tree-sitter-typescript");
        let app = root.path().join("app");
        std::fs::create_dir_all(dependency.join("src")).unwrap();
        std::fs::create_dir_all(app.join("src")).unwrap();
        std::fs::write(
            dependency.join("Cargo.toml"),
            "[package]\nname = \"tree-sitter-typescript\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(
            dependency.join("src/lib.rs"),
            "pub const LANGUAGE_TYPESCRIPT: u8 = 1;\n",
        )
        .unwrap();
        std::fs::write(
            app.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\ntree-sitter-typescript.workspace = true\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\", \"tree-sitter-typescript\"]\n[workspace.dependencies]\ntree-sitter-typescript = { path = \"tree-sitter-typescript\" }\n",
        )
        .unwrap();
        let source =
            "fn language() { let _ = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(); }\n";
        let path = app.join("src/lib.rs");
        std::fs::write(&path, source).unwrap();
        let mut engine = RustEngine::pending(root.path());
        engine.install(RustEngine::build(root.path()));

        let crate_column = source.find("tree_sitter_typescript").unwrap() as u32 + 1;
        let crate_target = engine.definition(&path, source, 0, crate_column);
        assert_eq!(crate_target.len(), 1);
        assert_eq!(
            crate_target[0].path,
            dependency.join("src/lib.rs").canonicalize().unwrap()
        );
        assert_eq!((crate_target[0].line, crate_target[0].column), (0, 0));

        let constant_column = source.find("LANGUAGE_TYPESCRIPT").unwrap() as u32 + 1;
        let constant = engine.definition(&path, source, 0, constant_column);
        assert_eq!(constant.len(), 1);
        assert_eq!(
            constant[0].path,
            dependency.join("src/lib.rs").canonicalize().unwrap()
        );
        assert_eq!((constant[0].line, constant[0].column), (0, 10));

        if rust_source_file("core/src/convert/mod.rs").is_some() {
            let into_column = source.find("into").unwrap() as u32 + 1;
            let into = engine.definition(&path, source, 0, into_column);
            assert_eq!(into.len(), 1);
            assert!(into[0].path.ends_with("core/src/convert/mod.rs"));
            assert_eq!(into[0].line, 457);
        }
    }

    #[test]
    fn real_workspace_definition_resolves_tree_sitter_registry_dependency() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let path = root.join("src/typescript/syntax.rs");
        let source = std::fs::read_to_string(&path).unwrap();
        let mut engine = RustEngine::pending(&root);
        engine.install(RustEngine::build(&root));
        assert!(
            engine
                .packages
                .iter()
                .any(|package| package.name == "tree_sitter_javascript"),
            "indexed packages: {:?}",
            engine
                .packages
                .iter()
                .map(|package| package.name.as_str())
                .collect::<Vec<_>>()
        );
        let line = source
            .lines()
            .position(|line| line.contains("tree_sitter_javascript::LANGUAGE.into()"))
            .unwrap() as u32;
        let text = source.lines().nth(line as usize).unwrap();

        let crate_column = text.find("tree_sitter_javascript").unwrap() as u32 + 1;
        let crate_target = engine.definition(&path, &source, line, crate_column);
        assert_eq!(crate_target.len(), 1);
        assert!(
            crate_target[0]
                .path
                .ends_with("tree-sitter-javascript-0.25.0/bindings/rust/lib.rs")
        );

        let constant_column = text.find("LANGUAGE").unwrap() as u32 + 1;
        let constant = engine.definition(&path, &source, line, constant_column);
        assert_eq!(constant.len(), 1);
        assert!(
            constant[0]
                .path
                .ends_with("tree-sitter-javascript-0.25.0/bindings/rust/lib.rs")
        );
        assert_eq!(constant[0].line, 30);
    }

    #[test]
    fn member_completion_infers_receiver_type_from_function_parameter() {
        let path = Path::new("/tmp/lib.rs");
        let source = "struct Coffee { beans: u8 }\nimpl Coffee { fn brew(&self) {} }\nfn serve(coffee: &Coffee) { coffee.br }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        assert!(
            engine
                .complete(path, source, 2, 40)
                .iter()
                .any(|item| item.label == "brew")
        );
    }

    #[test]
    fn signature_help_tracks_the_innermost_nested_call() {
        let path = Path::new("/tmp/lib.rs");
        let source = "fn brew(minutes: u8, temperature: u8) {}\nfn grind(beans: u8, setting: u8) {}\nfn main() { brew(grind(1, 2), 3); }";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        let inner = engine.signature(path, source, 2, 26).unwrap();
        assert!(inner.label.contains("fn grind"));
        assert_eq!(inner.active, 1);
        let outer = engine.signature(path, source, 2, 31).unwrap();
        assert!(outer.label.contains("fn brew"));
        assert_eq!(outer.active, 1);
    }

    #[test]
    fn offers_unused_import_removal_with_unresolved_call_diagnostic() {
        let path = Path::new("/tmp/lib.rs");
        let source = "use crate::models::Coffee;\nfn main() { missing(); }\n";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        assert_eq!(
            engine.diagnostics(path, source)[0].message,
            "cannot resolve `missing`"
        );
        let actions = engine.actions(path, source, 0, 0);
        assert_eq!(actions[0].title, "Remove unused import `Coffee`");
    }

    #[test]
    fn does_not_report_builtin_derive_attributes_as_unresolved() {
        let path = Path::new("/tmp/lib.rs");
        let source = "#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub struct Range;\n";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        assert!(engine.diagnostics(path, source).is_empty());
    }

    #[test]
    fn does_not_report_prelude_closure_or_field_shorthand_as_unresolved() {
        let path = Path::new("/tmp/lib.rs");
        let source = "struct Entry<'a> { uri: &'a str }\nfn main() {\n    let uri = \"file:///tmp\";\n    let _entry = Entry { uri };\n    let value = Some(uri);\n    assert!(value.is_some());\n    [1].iter().map(|d| d + 1).for_each(drop);\n}\n";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        let diagnostics = engine.diagnostics(path, source);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn does_not_report_imported_or_match_bound_names_as_unresolved() {
        let path = Path::new("/tmp/lib.rs");
        let source = "use tree_sitter::Node;\nfn inspect(value: &str) {\n    Node;\n    match value {\n        comment => drop(comment),\n    }\n}\n";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        let diagnostics = engine.diagnostics(path, source);
        assert!(
            diagnostics
                .iter()
                .all(|problem| !problem.message.contains("Node")
                    && !problem.message.contains("comment")),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn diagnostics_report_only_the_direct_unresolved_call() {
        let path = Path::new("/tmp/lib.rs");
        let source = "fn inspect(project: Option<&str>) {\n    if let Some(comment) = project {\n        for child in comment.chars() {\n            let _ = child;\n        }\n    } else {\n        missing();\n    }\n}\n";
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        let diagnostics = engine.diagnostics(path, source);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].message, "cannot resolve `missing`");
    }

    #[test]
    fn self_constructors_and_extern_functions_are_not_unresolved() {
        let path = Path::new("/tmp/lib.rs");
        let source = concat!(
            "pub struct Rng(u64);\n",
            "impl Rng { pub fn new(seed: u64) -> Self { Self(seed) } }\n",
            "unsafe extern \"C\" { fn malloc_size(ptr: *const u8) -> usize; }\n",
            "fn main() { let _ = unsafe { malloc_size(std::ptr::null()) }; }\n",
        );
        let mut engine = RustEngine::pending(Path::new("/tmp"));
        engine.install(Built::default());
        let diagnostics = engine.diagnostics(path, source);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn names_from_a_relative_glob_import_in_the_parent_module_resolve() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("src");
        std::fs::create_dir_all(src.join("engine")).unwrap();
        // Same-named public items elsewhere (as in dependencies) stop the
        // "only one match" fallback, so the glob path itself has to resolve.
        std::fs::write(src.join("lib.rs"), "mod engine;\npub mod other;\n").unwrap();
        std::fs::write(
            src.join("other.rs"),
            "pub fn empty_tree() {}\npub fn offset() {}\n",
        )
        .unwrap();
        std::fs::write(
            src.join("engine.rs"),
            "mod helpers;\nmod navigate;\nuse helpers::*;\npub(crate) use helpers::offset;\n",
        )
        .unwrap();
        std::fs::write(
            src.join("engine/helpers.rs"),
            "pub(super) fn empty_tree() {}\npub(in crate::engine) fn offset() {}\n",
        )
        .unwrap();
        let navigate = src.join("engine/navigate.rs");
        let source = "use super::*;\nfn run() { empty_tree(); offset(); missing(); }\n";
        std::fs::write(&navigate, source).unwrap();
        let mut engine = RustEngine::pending(root.path());
        engine.install(RustEngine::build(root.path()));

        let messages: Vec<_> = engine
            .diagnostics(&navigate, source)
            .into_iter()
            .map(|problem| problem.message)
            .collect();
        assert_eq!(messages, ["cannot resolve `missing`"]);
    }

    #[test]
    fn formatter_only_normalizes_indentation_for_valid_syntax() {
        let engine = RustEngine::pending(Path::new("/tmp"));
        assert_eq!(
            engine.format("fn main() {\nlet coffee = 1;  \n}\n"),
            Some("fn main() {\n    let coffee = 1;\n}\n".to_owned())
        );
        assert_eq!(
            structural_braces("let label = \"{not a block}\"; // }"),
            (0, 0)
        );
        assert_eq!(engine.format("fn main("), None);
    }
}
