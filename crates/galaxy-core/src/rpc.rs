//! Plumbing shared by the debugger: `Content-Length` framing, one small
//! Tokio runtime for every child-process transport, and the `PATH` a child
//! process gets when Galaxy is launched from Finder.

use std::collections::HashSet;
use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::runtime::{Builder, Runtime};

/// Fuzz-safe JSON-RPC payload validation shared by transport tests.
pub fn parse_json_rpc_payload(bytes: &[u8]) -> Result<Value, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if !value.is_object() {
        return Err("JSON-RPC payload must be an object".into());
    }
    Ok(value)
}

/// A received framed payload. Invalid JSON and broken streams are surfaced
/// as errors rather than silently discarded, so the session can report them.
#[derive(Debug)]
pub enum Incoming {
    Message(Value),
    Error(String),
    Closed,
}

/// Worker threads for the shared child-process runtime. Work per child is
/// one reader task plus pipe writes, so two threads are plenty, instead of a
/// default multi-thread runtime with one worker per CPU core.
pub const RUNTIME_THREADS: usize = 2;

/// One small Tokio runtime shared by every child-process transport. It
/// lives for the process; it is never dropped, so dropping a transport from
/// any context is safe.
pub(crate) fn shared_runtime() -> io::Result<&'static Runtime> {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    if let Some(runtime) = RUNTIME.get() {
        return Ok(runtime);
    }
    let runtime = Builder::new_multi_thread()
        .worker_threads(RUNTIME_THREADS)
        .thread_name("galaxy-rpc")
        .enable_all()
        .build()?;
    // A racing caller may win; the loser's unused runtime is dropped here,
    // outside any async context.
    Ok(RUNTIME.get_or_init(|| runtime))
}

/// `PATH` plus common install locations. GUI apps on macOS start with a
/// minimal `PATH`, so a child launched through a script (`#!/usr/bin/env ...`)
/// also needs these directories in its environment, see [`server_path_env`].
pub(crate) fn search_directories() -> Vec<PathBuf> {
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
            // `cargo install` and `go install` tools.
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

/// `PATH` for a child process: the program's own directory first, then the
/// search directories, so `#!/usr/bin/env` shebangs resolve when Galaxy is
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

pub(crate) fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", payload.len()).into_bytes();
    out.extend_from_slice(payload);
    out
}

pub(crate) async fn read_frame<R: AsyncRead + Unpin>(
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
                    "truncated message headers",
                ))
            };
        }
        if headers.len() - before > 8 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "message header line exceeds 8 KiB",
            ));
        }
        if headers.ends_with(b"\r\n\r\n") {
            break;
        }
        if headers.len() > 16 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "message headers exceed 16 KiB",
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
