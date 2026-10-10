//! JSON-RPC 2.0 transport for language servers over stdio (M4-01).
//!
//! The transport owns a Tokio child process. A reader task decodes framed
//! messages and forwards them through a standard channel so the rest of the
//! editor can remain synchronous. No UI call waits for server output.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::runtime::Runtime;

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
}

/// Lifecycle events observed by the synchronous editor core.
#[derive(Debug, Clone, PartialEq)]
pub enum LspEvent {
    Initialized(LspKey),
    Message(LspKey, Value),
    Error(LspKey, String),
    Closed(LspKey),
}

struct ManagedServer {
    transport: LspTransport,
    initialized: bool,
}

/// Owns one server per `(language, workspace root)` and drives the minimal
/// JSON-RPC lifecycle. General request IDs and cancellation are M4-05.
#[derive(Default)]
pub struct LspManager {
    servers: HashMap<LspKey, ManagedServer>,
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
                "capabilities": {}
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
                    Ok(Incoming::Message(message)) => events.push(LspEvent::Message(key.clone(), message)),
                    Ok(Incoming::Error(error)) => events.push(LspEvent::Error(key.clone(), error)),
                    Ok(Incoming::Closed) => { events.push(LspEvent::Closed(key.clone())); break; }
                    Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
                }
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
    }
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
}
