//! Installing plugins from git.
//!
//! Two steps so the UI can ask before anything runs:
//! [`fetch`] clones a pinned revision into a staging folder and reads the
//! manifest; [`install`] then runs the manifest's downloads and steps and
//! swaps the result into `<plugins>/<id>/`, rolling back on any failure.
//!
//! Nothing here goes through a shell. Git, curl and tar are launched with
//! argument arrays, with prompts disabled and a deadline on every process.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{
    Expand, MANIFEST_FILE, Manifest, platform, safe_relative, validate_id, validate_sha256,
};
use crate::lsp::{find_in_path, server_path_env};

const STATE_FILE: &str = "plugins.json";
const BUILTIN_SCHEME: &str = "builtin:";
const GIT_TIMEOUT: Duration = Duration::from_secs(300);
const STEP_TIMEOUT: Duration = Duration::from_secs(900);

/// Default plugin folder. `GALAXY_PLUGINS_DIR` overrides it (tests).
pub fn default_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("GALAXY_PLUGINS_DIR") {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support/Galaxy/plugins"))
}

/// Where a plugin comes from: a git repository, optionally a revision
/// (branch, tag or commit) and a folder inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub url: String,
    pub rev: Option<String>,
    pub subdir: Option<String>,
}

impl Source {
    /// `URL`, `URL#REV` or `URL#REV:SUBDIR` (`URL#:SUBDIR` for the default branch).
    pub fn parse(spec: &str) -> Result<Self, String> {
        let spec = spec.trim();
        let (url, fragment) = match spec.split_once('#') {
            Some((url, fragment)) => (url, Some(fragment)),
            None => (spec, None),
        };
        let (rev, subdir) = match fragment {
            None => (None, None),
            Some(fragment) => match fragment.split_once(':') {
                Some((rev, subdir)) => (Some(rev), Some(subdir)),
                None => (Some(fragment), None),
            },
        };
        let source = Self {
            url: url.to_owned(),
            rev: rev.filter(|r| !r.is_empty()).map(str::to_owned),
            subdir: subdir.filter(|s| !s.is_empty()).map(str::to_owned),
        };
        source.validate()?;
        Ok(source)
    }

    /// Marks a plugin bundled with Galaxy and installed locally on request.
    pub fn builtin(id: &str) -> Self {
        Self {
            url: format!("{BUILTIN_SCHEME}{id}"),
            rev: None,
            subdir: None,
        }
    }

    pub fn is_builtin(&self) -> bool {
        self.url.starts_with(BUILTIN_SCHEME)
    }

    pub fn to_spec(&self) -> String {
        match (&self.rev, &self.subdir) {
            (None, None) => self.url.clone(),
            (Some(rev), None) => format!("{}#{rev}", self.url),
            (rev, Some(subdir)) => {
                format!(
                    "{}#{}:{subdir}",
                    self.url,
                    rev.as_deref().unwrap_or_default()
                )
            }
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        let url = self.url.as_str();
        let allowed = ["https://", "ssh://", "file://", "git@", "/"]
            .iter()
            .any(|prefix| url.starts_with(prefix));
        if !allowed
            || url.contains("::")
            || url.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(format!(
                "unsupported repository url {url:?}: use https://, ssh://, git@host:path, file:// or an absolute path"
            ));
        }
        if let Some(rev) = &self.rev {
            let ok = !rev.starts_with('-')
                && !rev.contains("..")
                && rev
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
            if !ok {
                return Err(format!("invalid revision {rev:?}"));
            }
        }
        if let Some(subdir) = &self.subdir {
            safe_relative(subdir)?;
        }
        Ok(())
    }

    /// A full commit id never moves, so it never has updates.
    pub fn is_pinned(&self) -> bool {
        self.rev
            .as_deref()
            .is_some_and(|r| r.len() == 40 && r.chars().all(|c| c.is_ascii_hexdigit()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub source: Source,
    pub commit: String,
    pub version: String,
    /// SHA-256 over the fetched files, before install steps ran.
    pub tree_sha256: String,
    pub installed_at: u64,
}

/// `plugins.json`: what is installed, what is switched off, which registries
/// to read.
#[derive(Debug, Clone, Default)]
pub struct State {
    pub installed: BTreeMap<String, Installed>,
    pub disabled: BTreeSet<String>,
    pub registries: Vec<String>,
}

impl State {
    pub fn load(dir: &Path) -> Result<Self, String> {
        let path = dir.join(STATE_FILE);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let value: Value =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut state = Self::default();
        if let Some(map) = value.get("installed").and_then(Value::as_object) {
            for (id, entry) in map {
                validate_id(id)?;
                let text = |key: &str| {
                    entry
                        .get(key)
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| format!("{}: plugin {id} lacks {key}", path.display()))
                };
                state.installed.insert(
                    id.clone(),
                    Installed {
                        source: Source {
                            url: text("url")?,
                            rev: entry.get("ref").and_then(Value::as_str).map(str::to_owned),
                            subdir: entry
                                .get("subdir")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                        },
                        commit: text("commit")?,
                        version: text("version")?,
                        tree_sha256: text("treeSha256")?,
                        installed_at: entry
                            .get("installedAt")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    },
                );
            }
        }
        let strings = |key: &str| -> Vec<String> {
            value
                .get(key)
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        };
        state.disabled = strings("disabled").into_iter().collect();
        state.registries = strings("registries");
        Ok(state)
    }

    pub fn save(&self, dir: &Path) -> Result<(), String> {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let installed: serde_json::Map<String, Value> = self
            .installed
            .iter()
            .map(|(id, item)| {
                (
                    id.clone(),
                    json!({
                        "url": item.source.url,
                        "ref": item.source.rev,
                        "subdir": item.source.subdir,
                        "commit": item.commit,
                        "version": item.version,
                        "treeSha256": item.tree_sha256,
                        "installedAt": item.installed_at,
                    }),
                )
            })
            .collect();
        let value = json!({
            "installed": installed,
            "disabled": self.disabled,
            "registries": self.registries,
        });
        let text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
        let mut file = tempfile::NamedTempFile::new_in(dir).map_err(|e| e.to_string())?;
        io::Write::write_all(&mut file, text.as_bytes()).map_err(|e| e.to_string())?;
        file.persist(dir.join(STATE_FILE))
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// A fetched plugin waiting for the user's go-ahead.
#[derive(Debug, Clone)]
pub struct Pending {
    pub token: String,
    pub source: Source,
    pub commit: String,
    pub tree_sha256: String,
    /// Commit currently installed under the same id, if any.
    pub previous_commit: Option<String>,
    pub manifest: Manifest,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn new_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{}-{nanos}", std::process::id())
}

fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|c| c.is_ascii_digit() || c == '-')
}

fn staging_dir(dir: &Path, token: &str) -> Result<PathBuf, String> {
    if !valid_token(token) {
        return Err("invalid install token".into());
    }
    Ok(dir.join(".staging").join(token))
}

/// Runs a program with output captured in a temp file and a deadline.
fn run(
    program: &Path,
    args: &[OsString],
    cwd: Option<&Path>,
    env: &[(&str, &str)],
    timeout: Duration,
) -> Result<String, String> {
    let mut out = tempfile::tempfile().map_err(|e| e.to_string())?;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(out.try_clone().map_err(|e| e.to_string())?)
        .stderr(out.try_clone().map_err(|e| e.to_string())?)
        .env("PATH", server_path_env(&program.to_string_lossy()));
    for (key, value) in env {
        command.env(key, value);
    }
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", program.display()))?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{} timed out", program.display()));
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let mut bytes = Vec::new();
    out.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    out.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if status.success() {
        Ok(text)
    } else {
        let tail: String = text
            .chars()
            .rev()
            .take(2000)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Err(format!(
            "{} failed ({status}): {}",
            program.display(),
            tail.trim()
        ))
    }
}

pub(crate) fn git(args: &[&str], cwd: Option<&Path>) -> Result<String, String> {
    let program = find_in_path("git").ok_or("git is not installed")?;
    let mut full: Vec<OsString> = ["-c", "protocol.ext.allow=never"]
        .iter()
        .map(OsString::from)
        .collect();
    full.extend(args.iter().map(OsString::from));
    run(
        &program,
        &full,
        cwd,
        &[
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_SSH_COMMAND", "ssh -o BatchMode=yes"),
            ("GIT_HTTP_LOW_SPEED_LIMIT", "1000"),
            ("GIT_HTTP_LOW_SPEED_TIME", "60"),
        ],
        GIT_TIMEOUT,
    )
}

/// Shallow checkout of `source` into `dest`. Works for branches, tags and
/// commit ids alike. Returns the commit id; `.git` is removed afterwards.
pub(crate) fn checkout(source: &Source, dest: &Path) -> Result<String, String> {
    source.validate()?;
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    git(&["init", "-q"], Some(dest))?;
    git(
        &[
            "fetch",
            "-q",
            "--depth",
            "1",
            "--",
            &source.url,
            source.rev.as_deref().unwrap_or("HEAD"),
        ],
        Some(dest),
    )?;
    git(&["checkout", "-q", "--detach", "FETCH_HEAD"], Some(dest))?;
    let commit = git(&["rev-parse", "HEAD"], Some(dest))?.trim().to_owned();
    fs::remove_dir_all(dest.join(".git")).map_err(|e| e.to_string())?;
    Ok(commit)
}

/// The plugin folder inside a checkout; refuses subdirs that escape it.
pub(crate) fn plugin_root(checkout: &Path, subdir: Option<&str>) -> Result<PathBuf, String> {
    let root = match subdir {
        Some(subdir) => checkout.join(subdir),
        None => checkout.to_path_buf(),
    };
    let real = root
        .canonicalize()
        .map_err(|_| format!("folder {} not found in repository", subdir.unwrap_or(".")))?;
    let base = checkout.canonicalize().map_err(|e| e.to_string())?;
    if real.starts_with(&base) && real.is_dir() {
        Ok(real)
    } else {
        Err("plugin folder is outside the repository".into())
    }
}

/// Stages a built-in plugin for a local install: same confirmation and
/// install steps as a git plugin, but the manifest comes from Galaxy itself.
pub fn fetch_builtin(dir: &Path, id: &str) -> Result<Pending, String> {
    let text = super::builtin_manifest_text(id).ok_or_else(|| format!("{id} is not built in"))?;
    let manifest = Manifest::parse(text)?;
    if !manifest.has_install_work() {
        return Err(format!(
            "{id} has nothing to install: it uses the program on PATH"
        ));
    }
    let token = new_token();
    let staging = staging_dir(dir, &token)?;
    let result = (|| {
        let plugin = staging.join("plugin");
        fs::create_dir_all(&plugin).map_err(|e| e.to_string())?;
        fs::write(plugin.join(MANIFEST_FILE), text).map_err(|e| e.to_string())?;
        let pending = Pending {
            token: token.clone(),
            source: Source::builtin(id),
            commit: "builtin".to_owned(),
            tree_sha256: hash_tree(&plugin).map_err(|e| e.to_string())?,
            previous_commit: State::load(dir)?
                .installed
                .get(id)
                .map(|item| item.commit.clone()),
            manifest,
        };
        write_pending(&staging, &pending)?;
        Ok(pending)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

/// Clones `source` into staging and reads its manifest. Runs no plugin code.
/// `expect_sha256` (from a registry) must match the fetched files.
pub fn fetch(dir: &Path, source: &Source, expect_sha256: Option<&str>) -> Result<Pending, String> {
    let token = new_token();
    let staging = staging_dir(dir, &token)?;
    let result = fetch_into(dir, &staging, &token, source, expect_sha256);
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn fetch_into(
    dir: &Path,
    staging: &Path,
    token: &str,
    source: &Source,
    expect_sha256: Option<&str>,
) -> Result<Pending, String> {
    let src = staging.join("src");
    let commit = checkout(source, &src)?;
    let root = plugin_root(&src, source.subdir.as_deref())?;
    let text = fs::read_to_string(root.join(MANIFEST_FILE))
        .map_err(|e| format!("{MANIFEST_FILE} not found: {e}"))?;
    let manifest = Manifest::parse(&text).map_err(|e| format!("{MANIFEST_FILE}: {e}"))?;
    let tree_sha256 = hash_tree(&root).map_err(|e| e.to_string())?;
    if let Some(expected) = expect_sha256 {
        validate_sha256(expected)?;
        if !expected.eq_ignore_ascii_case(&tree_sha256) {
            return Err(format!(
                "checksum mismatch for {}: registry says {expected}, fetched {tree_sha256}",
                manifest.id
            ));
        }
    }
    let previous_commit = State::load(dir)?
        .installed
        .get(&manifest.id)
        .map(|item| item.commit.clone());
    // Only the plugin folder moves on to install; the rest of the repo goes.
    let plugin = staging.join("plugin");
    fs::rename(&root, &plugin).map_err(|e| e.to_string())?;
    let pending = Pending {
        token: token.to_owned(),
        source: source.clone(),
        commit,
        tree_sha256,
        previous_commit,
        manifest,
    };
    write_pending(staging, &pending)?;
    Ok(pending)
}

fn write_pending(staging: &Path, pending: &Pending) -> Result<(), String> {
    let value = json!({
        "url": pending.source.url,
        "ref": pending.source.rev,
        "subdir": pending.source.subdir,
        "commit": pending.commit,
        "treeSha256": pending.tree_sha256,
    });
    fs::write(staging.join("pending.json"), value.to_string()).map_err(|e| e.to_string())
}

fn read_pending(staging: &Path) -> Result<(Source, String, String, Manifest), String> {
    let value: Value = serde_json::from_str(
        &fs::read_to_string(staging.join("pending.json"))
            .map_err(|_| "nothing staged for this token".to_owned())?,
    )
    .map_err(|e| e.to_string())?;
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("pending.json lacks {key}"))
    };
    let manifest = Manifest::parse(
        &fs::read_to_string(staging.join("plugin").join(MANIFEST_FILE))
            .map_err(|e| e.to_string())?,
    )?;
    Ok((
        Source {
            url: text("url")?,
            rev: value.get("ref").and_then(Value::as_str).map(str::to_owned),
            subdir: value
                .get("subdir")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        text("commit")?,
        text("treeSha256")?,
        manifest,
    ))
}

/// Drops a staged plugin the user declined.
pub fn discard(dir: &Path, token: &str) -> Result<(), String> {
    let staging = staging_dir(dir, token)?;
    match fs::remove_dir_all(staging) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Runs downloads and steps for a staged plugin and puts it in place. The
/// previous version comes back if anything fails.
pub fn install(dir: &Path, token: &str) -> Result<Manifest, String> {
    let staging = staging_dir(dir, token)?;
    let (source, commit, tree_sha256, manifest) = read_pending(&staging)?;
    let target = dir.join(&manifest.id);
    let trash = staging.join("previous");
    let had_previous = target.exists();
    if had_previous {
        fs::rename(&target, &trash)
            .map_err(|e| format!("could not replace {}: {e}", manifest.id))?;
    }
    let outcome = fs::rename(staging.join("plugin"), &target)
        .map_err(|e| e.to_string())
        .and_then(|()| run_install(&staging, &target, &manifest))
        .and_then(|()| {
            let mut state = State::load(dir)?;
            state.installed.insert(
                manifest.id.clone(),
                Installed {
                    source,
                    commit,
                    version: manifest.version.clone(),
                    tree_sha256,
                    installed_at: unix_now(),
                },
            );
            state.save(dir)
        });
    if let Err(error) = outcome {
        let _ = fs::remove_dir_all(&target);
        if had_previous {
            let _ = fs::rename(&trash, &target);
        }
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    let _ = fs::remove_dir_all(&staging);
    Ok(manifest)
}

fn run_install(staging: &Path, target: &Path, manifest: &Manifest) -> Result<(), String> {
    for (index, download) in manifest.downloads_for(&platform()).into_iter().enumerate() {
        let file = staging.join(format!("download-{index}"));
        let curl = find_in_path("curl").ok_or("curl is not installed")?;
        run(
            &curl,
            &[
                "-fsSL",
                "--proto",
                "=https,file",
                "--max-time",
                "900",
                "-o",
                &file.to_string_lossy(),
                "--url",
                &download.url,
            ]
            .map(OsString::from),
            None,
            &[],
            STEP_TIMEOUT,
        )?;
        let actual = hash_file(&file).map_err(|e| e.to_string())?;
        if actual != download.sha256 {
            return Err(format!(
                "checksum mismatch for {}: expected {}, got {actual}",
                download.url, download.sha256
            ));
        }
        let dest = target.join(&download.dest);
        match download.archive.as_deref() {
            None => {
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                fs::copy(&file, &dest).map_err(|e| e.to_string())?;
                if download.executable {
                    fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))
                        .map_err(|e| e.to_string())?;
                }
            }
            Some(kind) => {
                fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
                let (tool, args): (&str, Vec<OsString>) = if kind == "zip" {
                    (
                        "unzip",
                        vec![
                            "-q".into(),
                            "-o".into(),
                            file.clone().into(),
                            "-d".into(),
                            dest.clone().into(),
                        ],
                    )
                } else {
                    (
                        "tar",
                        vec![
                            "-xzf".into(),
                            file.clone().into(),
                            "-C".into(),
                            dest.clone().into(),
                        ],
                    )
                };
                let program =
                    find_in_path(tool).ok_or_else(|| format!("{tool} is not installed"))?;
                run(&program, &args, None, &[], STEP_TIMEOUT)?;
            }
        }
    }
    let find = |name: &str| find_in_path(name);
    let expand = Expand {
        plugin_dir: Some(target),
        find: &find,
    };
    for step in &manifest.install_steps {
        let argv = step
            .iter()
            .map(|part| expand.string(part))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| format!("cannot resolve install step: {}", step.join(" ")))?;
        let program = find_in_path(&argv[0]).ok_or_else(|| {
            format!(
                "{} is not installed (needed by: {})",
                argv[0],
                step.join(" ")
            )
        })?;
        let args: Vec<OsString> = argv[1..].iter().map(OsString::from).collect();
        run(&program, &args, Some(target), &[], STEP_TIMEOUT)?;
    }
    for (path, expected) in &manifest.checksums {
        let actual =
            hash_file(&target.join(path)).map_err(|e| format!("checksum file {path}: {e}"))?;
        if actual != *expected {
            return Err(format!(
                "checksum mismatch for {path}: expected {expected}, got {actual}"
            ));
        }
    }
    Ok(())
}

/// Removes an installed plugin and forgets it. Only folders tracked in the
/// state file are touched.
pub fn uninstall(dir: &Path, id: &str) -> Result<(), String> {
    validate_id(id)?;
    let mut state = State::load(dir)?;
    if state.installed.remove(id).is_none() {
        return Err(format!("{id} is not an installed plugin"));
    }
    state.disabled.remove(id);
    match fs::remove_dir_all(dir.join(id)) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    state.save(dir)
}

pub fn set_enabled(dir: &Path, id: &str, enabled: bool) -> Result<(), String> {
    validate_id(id)?;
    let mut state = State::load(dir)?;
    if enabled {
        state.disabled.remove(id);
    } else {
        state.disabled.insert(id.to_owned());
    }
    state.save(dir)
}

pub fn add_registry(dir: &Path, url: &str) -> Result<(), String> {
    let mut state = State::load(dir)?;
    let url = url.trim();
    if url.is_empty() || url.starts_with('-') {
        return Err("invalid registry url".into());
    }
    if !state.registries.iter().any(|r| r == url) {
        state.registries.push(url.to_owned());
    }
    state.save(dir)
}

pub fn remove_registry(dir: &Path, url: &str) -> Result<(), String> {
    let mut state = State::load(dir)?;
    state.registries.retain(|r| r != url);
    state.save(dir)
}

/// Newest commit upstream when it differs from the installed one.
/// Plugins pinned to a commit never have updates.
pub fn check_update(dir: &Path, id: &str) -> Result<Option<String>, String> {
    let state = State::load(dir)?;
    let item = state
        .installed
        .get(id)
        .ok_or_else(|| format!("{id} is not an installed plugin"))?;
    if item.source.is_builtin() || item.source.is_pinned() {
        return Ok(None);
    }
    let output = git(
        &[
            "ls-remote",
            "--",
            &item.source.url,
            item.source.rev.as_deref().unwrap_or("HEAD"),
        ],
        None,
    )?;
    // Annotated tags list the tag object first and the commit as `^{}`.
    let lines: Vec<(&str, &str)> = output
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .collect();
    let latest = lines
        .iter()
        .find(|(_, name)| name.ends_with("^{}"))
        .or_else(|| lines.first())
        .map(|(sha, _)| (*sha).to_owned())
        .ok_or("revision not found upstream")?;
    if latest == item.commit {
        return Ok(None);
    }
    // In a shared repository the commit moves for unrelated changes; only
    // report an update when this plugin's own files differ.
    if item.source.subdir.is_some() {
        let pending = fetch(dir, &item.source, None)?;
        let changed = pending.tree_sha256 != item.tree_sha256;
        discard(dir, &pending.token)?;
        return Ok(changed.then_some(latest));
    }
    Ok(Some(latest))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn hash_file(path: &Path) -> io::Result<String> {
    let mut hasher = Sha256::new();
    io::copy(&mut fs::File::open(path)?, &mut hasher)?;
    Ok(hex(&hasher.finalize()))
}

/// Stable digest over names, kinds and contents. Symlinks hash their target
/// and are never followed.
pub fn hash_tree(root: &Path) -> io::Result<String> {
    fn walk(root: &Path, dir: &Path, hasher: &mut Sha256) -> io::Result<()> {
        let mut entries = fs::read_dir(dir)?.collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let kind = fs::symlink_metadata(&path)?;
            if kind.is_dir() {
                hasher.update(b"d\0");
                hasher.update(rel.as_os_str().as_bytes());
                hasher.update(b"\0");
                walk(root, &path, hasher)?;
            } else if kind.file_type().is_symlink() {
                hasher.update(b"l\0");
                hasher.update(rel.as_os_str().as_bytes());
                hasher.update(b"\0");
                hasher.update(fs::read_link(&path)?.as_os_str().as_bytes());
            } else {
                hasher.update(b"f\0");
                hasher.update(rel.as_os_str().as_bytes());
                hasher.update(b"\0");
                hasher.update(kind.len().to_le_bytes());
                io::copy(&mut fs::File::open(&path)?, hasher)?;
            }
        }
        Ok(())
    }
    let mut hasher = Sha256::new();
    walk(root, root, &mut hasher)?;
    Ok(hex(&hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_specs_round_trip_and_reject_unsafe_input() {
        let s = Source::parse("https://example.com/p.git#v1.2.0:plugins/laravel").unwrap();
        assert_eq!(s.rev.as_deref(), Some("v1.2.0"));
        assert_eq!(s.subdir.as_deref(), Some("plugins/laravel"));
        assert_eq!(Source::parse(&s.to_spec()).unwrap(), s);
        let default_branch = Source::parse("https://example.com/p.git#:sub").unwrap();
        assert_eq!(default_branch.rev, None);
        assert_eq!(default_branch.to_spec(), "https://example.com/p.git#:sub");

        for bad in [
            "http://example.com/p.git",
            "--upload-pack=touch /tmp/x",
            "ext::sh -c touch% /tmp/x",
            "https://example.com/p.git#--output=x",
            "https://example.com/p.git#v1:../escape",
            "https://example.com/p.git#v1:/etc",
            "https://exa mple.com/p.git",
            "relative/path",
        ] {
            assert!(Source::parse(bad).is_err(), "{bad} must be rejected");
        }
        assert!(Source::parse("git@github.com:user/repo.git").is_ok());
        assert!(Source::parse("file:///tmp/repo").is_ok());
        assert!(
            Source::parse(&format!("https://x/y#{}", "a".repeat(40)))
                .unwrap()
                .is_pinned()
        );
    }

    #[test]
    fn state_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        assert!(State::load(dir.path()).unwrap().installed.is_empty());
        let mut state = State::default();
        state.disabled.insert("gopls".into());
        state.registries.push("https://example.com/r.git".into());
        state.installed.insert(
            "demo".into(),
            Installed {
                source: Source::parse("https://example.com/p.git#v1:sub").unwrap(),
                commit: "abc".into(),
                version: "1.0.0".into(),
                tree_sha256: "def".into(),
                installed_at: 7,
            },
        );
        state.save(dir.path()).unwrap();
        let loaded = State::load(dir.path()).unwrap();
        assert_eq!(loaded.installed, state.installed);
        assert_eq!(loaded.disabled, state.disabled);
        assert_eq!(loaded.registries, state.registries);
    }

    #[test]
    fn tree_hash_tracks_content_and_names() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a"), "one").unwrap();
        let first = hash_tree(dir.path()).unwrap();
        assert_eq!(first, hash_tree(dir.path()).unwrap());
        fs::write(dir.path().join("a"), "two").unwrap();
        assert_ne!(first, hash_tree(dir.path()).unwrap());
    }

    #[test]
    fn tokens_cannot_escape_the_staging_folder() {
        let dir = Path::new("/plugins");
        assert!(staging_dir(dir, "12-345").is_ok());
        assert!(staging_dir(dir, "../x").is_err());
        assert!(staging_dir(dir, "").is_err());
    }
}
