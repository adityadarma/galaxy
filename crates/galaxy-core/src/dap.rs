//! Debug Adapter Protocol client.
//!
//! Two layers:
//! - [`DapState`]: pure protocol state machine. Takes incoming JSON messages,
//!   returns outgoing JSON messages and [`DebugEvent`]s. No I/O.
//! - [`DebugSession`]: owns the adapter child process and a reader task, and
//!   exposes a synchronous, poll-friendly API for the UI thread.
//!
//! Framing is the same `Content-Length` framing LSP uses, so it reuses
//! `lsp::frame` / `lsp::read_frame`. DAP messages carry no `jsonrpc` field:
//! `{seq, type: "request"|"response"|"event", ...}`.

use std::collections::HashMap;
use std::io;
use std::process::Stdio;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::runtime::Runtime;

use crate::lsp::{Incoming, frame, read_frame, server_path_env, shared_runtime};

/// Stack depth fetched after each stop.
const STACK_LEVELS: u32 = 20;
/// How long `stop` waits for the adapter to exit after `disconnect`.
const DISCONNECT_GRACE: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Breakpoint {
    /// 1-based line.
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackFrame {
    pub id: i64,
    pub name: String,
    pub path: Option<String>,
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub name: String,
    pub variables_reference: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub type_name: Option<String>,
    pub variables_reference: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchConfig {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub stop_on_entry: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugEvent {
    /// Adapter sent `initialized`; breakpoints and `configurationDone` are
    /// already sent internally.
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
    /// Auto-fetched after a stop.
    Frames {
        thread_id: i64,
        frames: Vec<StackFrame>,
    },
    /// Result of `request_variables`.
    Variables {
        reference: i64,
        variables: Vec<Variable>,
    },
    Scopes {
        frame_id: i64,
        scopes: Vec<Scope>,
    },
    Evaluated {
        expression: String,
        result: String,
        ok: bool,
    },
    Error(String),
    AdapterClosed,
}

/// What a pending request needs when its response arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingContext {
    None,
    Thread(i64),
    Frame(i64),
    Reference(i64),
    Expression(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    command: String,
    context: PendingContext,
}

/// Pure DAP state machine. Feed it adapter messages with
/// [`DapState::handle_message`]; send whatever it returns.
#[derive(Debug)]
pub struct DapState {
    seq: i64,
    pending: HashMap<i64, Pending>,
    config: LaunchConfig,
    breakpoints: HashMap<String, Vec<u32>>,
    initialized: bool,
    stopped: bool,
    terminated: bool,
    thread_id: Option<i64>,
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn i64_field(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

fn u32_field(value: &Value, key: &str) -> u32 {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}

fn array_field<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
}

impl DapState {
    pub fn new(config: LaunchConfig, breakpoints: HashMap<String, Vec<u32>>) -> Self {
        Self {
            seq: 0,
            pending: HashMap::new(),
            config,
            breakpoints,
            initialized: false,
            stopped: false,
            terminated: false,
            thread_id: None,
        }
    }

    pub fn is_terminated(&self) -> bool {
        self.terminated
    }

    /// Thread used by resume/step requests: last stopped thread, else 1.
    fn current_thread(&self) -> i64 {
        self.thread_id.unwrap_or(1)
    }

    fn request(&mut self, command: &str, arguments: Value, context: PendingContext) -> Value {
        self.seq += 1;
        let seq = self.seq;
        self.pending.insert(
            seq,
            Pending {
                command: command.to_owned(),
                context,
            },
        );
        json!({
            "seq": seq,
            "type": "request",
            "command": command,
            "arguments": arguments,
        })
    }

    /// `initialize` followed immediately by `launch`. Adapters such as
    /// lldb-dap answer `launch` only after `configurationDone`, so the
    /// response is not awaited.
    pub fn start_messages(&mut self) -> Vec<Value> {
        let initialize = self.request(
            "initialize",
            json!({
                "clientID": "galaxy",
                "clientName": "Galaxy",
                "adapterID": "galaxy",
                "linesStartAt1": true,
                "columnsStartAt1": true,
                "pathFormat": "path",
                "supportsRunInTerminalRequest": false,
                "supportsVariableType": true,
            }),
            PendingContext::None,
        );
        let mut launch_args = json!({
            "program": self.config.program,
            "args": self.config.args,
            "stopOnEntry": self.config.stop_on_entry,
        });
        if let Some(cwd) = &self.config.cwd {
            launch_args["cwd"] = json!(cwd);
        }
        let launch = self.request("launch", launch_args, PendingContext::None);
        vec![initialize, launch]
    }

    fn set_breakpoints_message(&mut self, path: &str) -> Value {
        let lines = self.breakpoints.get(path).cloned().unwrap_or_default();
        let breakpoints: Vec<Value> = lines.iter().map(|line| json!({ "line": line })).collect();
        self.request(
            "setBreakpoints",
            json!({
                "source": { "path": path },
                "breakpoints": breakpoints,
                "sourceModified": false,
            }),
            PendingContext::None,
        )
    }

    /// Records `lines` for `path`. Sent right away once the adapter has sent
    /// `initialized`; before that they go out with the handshake.
    pub fn set_breakpoints(&mut self, path: &str, lines: &[u32]) -> Vec<Value> {
        let mut deduped: Vec<u32> = Vec::with_capacity(lines.len());
        for line in lines {
            if !deduped.contains(line) {
                deduped.push(*line);
            }
        }
        self.breakpoints.insert(path.to_owned(), deduped);
        if self.initialized {
            vec![self.set_breakpoints_message(path)]
        } else {
            Vec::new()
        }
    }

    pub fn continue_(&mut self) -> Vec<Value> {
        let thread = self.current_thread();
        vec![self.request(
            "continue",
            json!({ "threadId": thread }),
            PendingContext::None,
        )]
    }

    pub fn pause(&mut self) -> Vec<Value> {
        let thread = self.current_thread();
        vec![self.request("pause", json!({ "threadId": thread }), PendingContext::None)]
    }

    pub fn step_over(&mut self) -> Vec<Value> {
        self.step("next")
    }

    pub fn step_in(&mut self) -> Vec<Value> {
        self.step("stepIn")
    }

    pub fn step_out(&mut self) -> Vec<Value> {
        self.step("stepOut")
    }

    fn step(&mut self, command: &str) -> Vec<Value> {
        let thread = self.current_thread();
        vec![self.request(command, json!({ "threadId": thread }), PendingContext::None)]
    }

    pub fn request_scopes(&mut self, frame_id: i64) -> Vec<Value> {
        vec![self.request(
            "scopes",
            json!({ "frameId": frame_id }),
            PendingContext::Frame(frame_id),
        )]
    }

    pub fn request_variables(&mut self, reference: i64) -> Vec<Value> {
        vec![self.request(
            "variables",
            json!({ "variablesReference": reference }),
            PendingContext::Reference(reference),
        )]
    }

    pub fn evaluate(&mut self, expression: &str, frame_id: Option<i64>) -> Vec<Value> {
        let mut args = json!({ "expression": expression, "context": "repl" });
        if let Some(frame_id) = frame_id {
            args["frameId"] = json!(frame_id);
        }
        vec![self.request(
            "evaluate",
            args,
            PendingContext::Expression(expression.to_owned()),
        )]
    }

    pub fn disconnect(&mut self) -> Value {
        self.request(
            "disconnect",
            json!({ "terminateDebuggee": true }),
            PendingContext::None,
        )
    }

    /// Handles one adapter message. Returns `(outgoing, events)`.
    pub fn handle_message(&mut self, message: &Value) -> (Vec<Value>, Vec<DebugEvent>) {
        match message.get("type").and_then(Value::as_str) {
            Some("response") => self.handle_response(message),
            Some("event") => self.handle_event(message),
            Some("request") => (vec![self.handle_reverse_request(message)], Vec::new()),
            _ => (
                Vec::new(),
                vec![DebugEvent::Error(format!(
                    "unrecognized DAP message: {message}"
                ))],
            ),
        }
    }

    fn handle_reverse_request(&mut self, message: &Value) -> Value {
        self.seq += 1;
        let command = str_field(message, "command").unwrap_or_default();
        let reason = match command.as_str() {
            "runInTerminal" => "runInTerminal is not supported".to_owned(),
            "startDebugging" => "startDebugging is not supported".to_owned(),
            other => format!("unsupported request: {other}"),
        };
        json!({
            "seq": self.seq,
            "type": "response",
            "request_seq": i64_field(message, "seq").unwrap_or(0),
            "success": false,
            "command": command,
            "message": reason,
        })
    }

    fn handle_event(&mut self, message: &Value) -> (Vec<Value>, Vec<DebugEvent>) {
        let body = message.get("body").cloned().unwrap_or(Value::Null);
        let mut out = Vec::new();
        let mut events = Vec::new();
        match message.get("event").and_then(Value::as_str) {
            Some("initialized") => {
                if !self.initialized {
                    self.initialized = true;
                    events.push(DebugEvent::Initialized);
                    let mut paths: Vec<String> = self.breakpoints.keys().cloned().collect();
                    paths.sort();
                    for path in paths {
                        out.push(self.set_breakpoints_message(&path));
                    }
                    out.push(self.request("configurationDone", json!({}), PendingContext::None));
                }
            }
            Some("output") => {
                events.push(DebugEvent::Output {
                    category: str_field(&body, "category").unwrap_or_else(|| "console".to_owned()),
                    text: str_field(&body, "output").unwrap_or_default(),
                });
            }
            Some("stopped") => {
                let thread_id =
                    i64_field(&body, "threadId").unwrap_or_else(|| self.current_thread());
                self.thread_id = Some(thread_id);
                self.stopped = true;
                events.push(DebugEvent::Stopped {
                    thread_id,
                    reason: str_field(&body, "reason").unwrap_or_else(|| "unknown".to_owned()),
                    description: str_field(&body, "description")
                        .or_else(|| str_field(&body, "text")),
                });
                out.push(self.request(
                    "stackTrace",
                    json!({ "threadId": thread_id, "levels": STACK_LEVELS }),
                    PendingContext::Thread(thread_id),
                ));
            }
            Some("continued") => {
                // Adapters may send this alongside the request response;
                // report a resume once per stop.
                if self.stopped {
                    self.stopped = false;
                    events.push(DebugEvent::Continued);
                }
            }
            Some("exited") => {
                events.push(DebugEvent::Exited {
                    code: i64_field(&body, "exitCode").unwrap_or(0),
                });
            }
            Some("terminated") => {
                self.terminated = true;
                self.stopped = false;
                events.push(DebugEvent::Terminated);
            }
            _ => {}
        }
        (out, events)
    }

    fn handle_response(&mut self, message: &Value) -> (Vec<Value>, Vec<DebugEvent>) {
        let Some(pending) =
            i64_field(message, "request_seq").and_then(|seq| self.pending.remove(&seq))
        else {
            return (Vec::new(), Vec::new());
        };
        let success = message
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let body = message.get("body").cloned().unwrap_or(Value::Null);
        let events = if success {
            Self::response_events(&pending, &body)
        } else {
            Self::failure_events(&pending, message, &body)
        };
        (Vec::new(), events)
    }

    fn failure_events(pending: &Pending, message: &Value, body: &Value) -> Vec<DebugEvent> {
        let reason = str_field(message, "message")
            .or_else(|| {
                body.get("error")
                    .and_then(|error| str_field(error, "format"))
            })
            .unwrap_or_else(|| "request failed".to_owned());
        match (&pending.command[..], &pending.context) {
            ("disconnect", _) => Vec::new(),
            ("evaluate", PendingContext::Expression(expression)) => vec![DebugEvent::Evaluated {
                expression: expression.clone(),
                result: reason,
                ok: false,
            }],
            (command, _) => vec![DebugEvent::Error(format!("{command} failed: {reason}"))],
        }
    }

    fn response_events(pending: &Pending, body: &Value) -> Vec<DebugEvent> {
        match (&pending.command[..], &pending.context) {
            ("stackTrace", PendingContext::Thread(thread_id)) => {
                let frames = array_field(body, "stackFrames")
                    .iter()
                    .map(parse_frame)
                    .collect();
                vec![DebugEvent::Frames {
                    thread_id: *thread_id,
                    frames,
                }]
            }
            ("scopes", PendingContext::Frame(frame_id)) => {
                let scopes = array_field(body, "scopes")
                    .iter()
                    .map(|scope| Scope {
                        name: str_field(scope, "name").unwrap_or_default(),
                        variables_reference: i64_field(scope, "variablesReference").unwrap_or(0),
                    })
                    .collect();
                vec![DebugEvent::Scopes {
                    frame_id: *frame_id,
                    scopes,
                }]
            }
            ("variables", PendingContext::Reference(reference)) => {
                let variables = array_field(body, "variables")
                    .iter()
                    .map(|variable| Variable {
                        name: str_field(variable, "name").unwrap_or_default(),
                        value: str_field(variable, "value").unwrap_or_default(),
                        type_name: str_field(variable, "type"),
                        variables_reference: i64_field(variable, "variablesReference").unwrap_or(0),
                    })
                    .collect();
                vec![DebugEvent::Variables {
                    reference: *reference,
                    variables,
                }]
            }
            ("evaluate", PendingContext::Expression(expression)) => vec![DebugEvent::Evaluated {
                expression: expression.clone(),
                result: str_field(body, "result").unwrap_or_default(),
                ok: true,
            }],
            _ => Vec::new(),
        }
    }
}

fn parse_frame(frame: &Value) -> StackFrame {
    StackFrame {
        id: i64_field(frame, "id").unwrap_or(0),
        name: str_field(frame, "name").unwrap_or_default(),
        path: frame
            .get("source")
            .and_then(|source| str_field(source, "path")),
        line: u32_field(frame, "line"),
        column: u32_field(frame, "column"),
    }
}

/// Stdio transport for one debug adapter process.
struct DapTransport {
    runtime: &'static Runtime,
    child: Child,
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    incoming: mpsc::Receiver<Incoming>,
}

impl DapTransport {
    fn spawn(command: &[String], cwd: Option<&str>) -> io::Result<Self> {
        let (program, args) = command
            .split_first()
            .ok_or_else(|| io::Error::other("empty debug adapter command"))?;
        let runtime = shared_runtime()?;
        let mut process = Command::new(program);
        process.args(args).env("PATH", server_path_env(program));
        if let Some(dir) = cwd.map(std::path::Path::new).filter(|dir| dir.is_dir()) {
            process.current_dir(dir);
        }
        process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = {
            let _guard = runtime.enter();
            process.spawn()?
        };
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("debug adapter stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("debug adapter stdout unavailable"))?;
        let (tx, incoming) = mpsc::channel();
        runtime.spawn(read_messages(BufReader::new(stdout), tx));
        Ok(Self {
            runtime,
            child,
            stdin: Arc::new(tokio::sync::Mutex::new(stdin)),
            incoming,
        })
    }

    /// Writes one framed message: an OS pipe write, not a round trip.
    fn send(&self, message: &Value) -> io::Result<()> {
        let payload = serde_json::to_vec(message).map_err(io::Error::other)?;
        tracing::debug!(target: "galaxy_dap_traffic", direction = "out", message = %message);
        self.runtime.block_on(async {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(&frame(&payload)).await?;
            stdin.flush().await
        })
    }

    fn try_recv(&self) -> Result<Incoming, mpsc::TryRecvError> {
        self.incoming.try_recv()
    }
}

impl Drop for DapTransport {
    fn drop(&mut self) {
        let _ = self.runtime.block_on(self.child.kill());
    }
}

async fn read_messages<R: AsyncRead + Unpin>(mut reader: BufReader<R>, tx: mpsc::Sender<Incoming>) {
    loop {
        match read_frame(&mut reader).await {
            Ok(Some(payload)) => match serde_json::from_slice::<Value>(&payload) {
                Ok(message) => {
                    tracing::debug!(target: "galaxy_dap_traffic", direction = "in", message = %message);
                    if tx.send(Incoming::Message(message)).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = tx.send(Incoming::Error(format!("invalid DAP payload: {error}")));
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

/// One debug session: adapter process plus protocol state.
pub struct DebugSession {
    transport: DapTransport,
    state: DapState,
    /// Events produced outside `poll` (send failures); drained by `poll`.
    queued: Vec<DebugEvent>,
    closed: bool,
    stopped_by_user: bool,
}

impl DebugSession {
    /// Spawns the adapter and sends `initialize` + `launch`. Everything else
    /// is driven by [`DebugSession::poll`].
    pub fn start(
        adapter_command: &[String],
        config: LaunchConfig,
        breakpoints: HashMap<String, Vec<u32>>,
    ) -> Result<Self, String> {
        let transport = DapTransport::spawn(adapter_command, config.cwd.as_deref())
            .map_err(|error| format!("failed to start debug adapter: {error}"))?;
        let mut state = DapState::new(config, breakpoints);
        for message in state.start_messages() {
            transport
                .send(&message)
                .map_err(|error| format!("failed to talk to debug adapter: {error}"))?;
        }
        Ok(Self {
            transport,
            state,
            queued: Vec::new(),
            closed: false,
            stopped_by_user: false,
        })
    }

    /// Non-blocking. Drains adapter messages, advances the handshake, and
    /// returns the resulting events.
    pub fn poll(&mut self) -> Vec<DebugEvent> {
        let mut events = std::mem::take(&mut self.queued);
        if self.stopped_by_user {
            return events;
        }
        while !self.closed {
            match self.transport.try_recv() {
                Ok(Incoming::Message(message)) => {
                    let (out, mut new_events) = self.state.handle_message(&message);
                    events.append(&mut new_events);
                    self.send_all(&out, &mut events);
                }
                Ok(Incoming::Error(error)) => events.push(DebugEvent::Error(error)),
                Ok(Incoming::Closed) | Err(mpsc::TryRecvError::Disconnected) => {
                    self.closed = true;
                    events.push(DebugEvent::AdapterClosed);
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        events
    }

    fn send_all(&self, messages: &[Value], events: &mut Vec<DebugEvent>) {
        for message in messages {
            if let Err(error) = self.transport.send(message) {
                events.push(DebugEvent::Error(format!("send to debug adapter: {error}")));
                return;
            }
        }
    }

    fn dispatch(&mut self, messages: Vec<Value>) {
        if self.closed || self.stopped_by_user {
            return;
        }
        let mut errors = Vec::new();
        self.send_all(&messages, &mut errors);
        self.queued.append(&mut errors);
    }

    pub fn set_breakpoints(&mut self, path: &str, lines: &[u32]) {
        let messages = self.state.set_breakpoints(path, lines);
        self.dispatch(messages);
    }

    pub fn continue_(&mut self) {
        let messages = self.state.continue_();
        self.dispatch(messages);
    }

    pub fn pause(&mut self) {
        let messages = self.state.pause();
        self.dispatch(messages);
    }

    pub fn step_over(&mut self) {
        let messages = self.state.step_over();
        self.dispatch(messages);
    }

    pub fn step_in(&mut self) {
        let messages = self.state.step_in();
        self.dispatch(messages);
    }

    pub fn step_out(&mut self) {
        let messages = self.state.step_out();
        self.dispatch(messages);
    }

    pub fn request_scopes(&mut self, frame_id: i64) {
        let messages = self.state.request_scopes(frame_id);
        self.dispatch(messages);
    }

    pub fn request_variables(&mut self, reference: i64) {
        let messages = self.state.request_variables(reference);
        self.dispatch(messages);
    }

    /// Evaluates in the `repl` context. Result arrives as
    /// [`DebugEvent::Evaluated`].
    pub fn evaluate(&mut self, expression: &str, frame_id: Option<i64>) {
        let messages = self.state.evaluate(expression, frame_id);
        self.dispatch(messages);
    }

    /// Sends `disconnect` with `terminateDebuggee`, then kills the adapter
    /// if it does not exit shortly. Idempotent.
    pub fn stop(&mut self) {
        if self.stopped_by_user {
            return;
        }
        if !self.closed {
            let message = self.state.disconnect();
            let _ = self.transport.send(&message);
            let deadline = Instant::now() + DISCONNECT_GRACE;
            while Instant::now() < deadline {
                if matches!(self.transport.child.try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        self.stopped_by_user = true;
        let _ = self.transport.runtime.block_on(self.transport.child.kill());
    }

    pub fn is_running(&self) -> bool {
        !self.stopped_by_user && !self.closed && !self.state.is_terminated()
    }
}

impl Drop for DebugSession {
    fn drop(&mut self) {
        if !self.stopped_by_user && !self.closed {
            self.stop();
        }
    }
}

/// Path of Xcode's `lldb-dap`, the debug adapter for C, C++, Rust and
/// Swift programs on macOS. `xcrun` first, then the usual Xcode location,
/// then `PATH`.
pub fn find_lldb_dap() -> Option<String> {
    use std::path::Path;
    if let Ok(output) = std::process::Command::new("xcrun")
        .args(["-f", "lldb-dap"])
        .output()
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !path.is_empty() && Path::new(&path).is_file() {
            return Some(path);
        }
    }
    let xcode = "/Applications/Xcode.app/Contents/Developer/usr/bin/lldb-dap";
    if Path::new(xcode).is_file() {
        return Some(xcode.to_owned());
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("lldb-dap"))
            .find(|candidate| candidate.is_file())
            .map(|candidate| candidate.to_string_lossy().into_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> LaunchConfig {
        LaunchConfig {
            program: "/tmp/prog".into(),
            args: vec!["--flag".into(), "x".into()],
            cwd: Some("/tmp".into()),
            stop_on_entry: false,
        }
    }

    fn state_with(breakpoints: &[(&str, &[u32])]) -> DapState {
        let map = breakpoints
            .iter()
            .map(|(path, lines)| ((*path).to_owned(), lines.to_vec()))
            .collect();
        DapState::new(config(), map)
    }

    /// State that has sent its handshake and received `initialized`.
    fn ready_state() -> DapState {
        let mut state = state_with(&[]);
        state.start_messages();
        state.handle_message(&json!({"seq": 1, "type": "event", "event": "initialized"}));
        state
    }

    fn response(request_seq: i64, command: &str, body: Value) -> Value {
        json!({
            "seq": 100, "type": "response", "request_seq": request_seq,
            "success": true, "command": command, "body": body,
        })
    }

    fn failure(request_seq: i64, command: &str, message: &str) -> Value {
        json!({
            "seq": 100, "type": "response", "request_seq": request_seq,
            "success": false, "command": command, "message": message,
        })
    }

    fn event(name: &str, body: Value) -> Value {
        json!({"seq": 50, "type": "event", "event": name, "body": body})
    }

    fn last_seq(messages: &[Value]) -> i64 {
        messages.last().unwrap()["seq"].as_i64().unwrap()
    }

    #[test]
    fn start_sends_initialize_then_launch() {
        let mut state = state_with(&[]);
        let messages = state.start_messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["seq"], 1);
        assert_eq!(messages[0]["type"], "request");
        assert_eq!(messages[0]["command"], "initialize");
        let args = &messages[0]["arguments"];
        assert_eq!(args["clientID"], "galaxy");
        assert_eq!(args["adapterID"], "galaxy");
        assert_eq!(args["linesStartAt1"], true);
        assert_eq!(args["columnsStartAt1"], true);
        assert_eq!(args["supportsRunInTerminalRequest"], false);
        assert_eq!(messages[1]["seq"], 2);
        assert_eq!(messages[1]["command"], "launch");
        let launch = &messages[1]["arguments"];
        assert_eq!(launch["program"], "/tmp/prog");
        assert_eq!(launch["args"], json!(["--flag", "x"]));
        assert_eq!(launch["cwd"], "/tmp");
        assert_eq!(launch["stopOnEntry"], false);
    }

    #[test]
    fn launch_omits_cwd_when_unset() {
        let mut cfg = config();
        cfg.cwd = None;
        cfg.stop_on_entry = true;
        let mut state = DapState::new(cfg, HashMap::new());
        let messages = state.start_messages();
        assert!(messages[1]["arguments"].get("cwd").is_none());
        assert_eq!(messages[1]["arguments"]["stopOnEntry"], true);
    }

    #[test]
    fn initialized_sends_breakpoints_then_configuration_done() {
        let mut state = state_with(&[("/b.c", &[7]), ("/a.c", &[3, 5])]);
        state.start_messages();
        let (out, events) =
            state.handle_message(&json!({"seq": 1, "type": "event", "event": "initialized"}));
        assert_eq!(events, vec![DebugEvent::Initialized]);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0]["command"], "setBreakpoints");
        assert_eq!(out[0]["arguments"]["source"]["path"], "/a.c");
        assert_eq!(
            out[0]["arguments"]["breakpoints"],
            json!([{"line": 3}, {"line": 5}])
        );
        assert_eq!(out[1]["arguments"]["source"]["path"], "/b.c");
        assert_eq!(out[2]["command"], "configurationDone");
    }

    #[test]
    fn duplicate_initialized_is_ignored() {
        let mut state = ready_state();
        let (out, events) =
            state.handle_message(&json!({"seq": 2, "type": "event", "event": "initialized"}));
        assert!(out.is_empty());
        assert!(events.is_empty());
    }

    #[test]
    fn initialized_without_breakpoints_only_sends_configuration_done() {
        let mut state = state_with(&[]);
        state.start_messages();
        let (out, _) =
            state.handle_message(&json!({"seq": 1, "type": "event", "event": "initialized"}));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["command"], "configurationDone");
    }

    #[test]
    fn set_breakpoints_before_initialized_is_deferred() {
        let mut state = state_with(&[]);
        state.start_messages();
        assert!(state.set_breakpoints("/a.c", &[4]).is_empty());
        let (out, _) =
            state.handle_message(&json!({"seq": 1, "type": "event", "event": "initialized"}));
        assert_eq!(out[0]["arguments"]["source"]["path"], "/a.c");
        assert_eq!(out[0]["arguments"]["breakpoints"], json!([{"line": 4}]));
    }

    #[test]
    fn set_breakpoints_after_initialized_sends_full_list() {
        let mut state = ready_state();
        let out = state.set_breakpoints("/a.c", &[2, 2, 9]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["command"], "setBreakpoints");
        assert_eq!(
            out[0]["arguments"]["breakpoints"],
            json!([{"line": 2}, {"line": 9}])
        );
        let cleared = state.set_breakpoints("/a.c", &[]);
        assert_eq!(cleared[0]["arguments"]["breakpoints"], json!([]));
    }

    #[test]
    fn stopped_emits_event_and_requests_stack_trace() {
        let mut state = ready_state();
        let (out, events) = state.handle_message(&event(
            "stopped",
            json!({"reason": "breakpoint", "threadId": 7, "description": "breakpoint 1.1"}),
        ));
        assert_eq!(
            events,
            vec![DebugEvent::Stopped {
                thread_id: 7,
                reason: "breakpoint".into(),
                description: Some("breakpoint 1.1".into()),
            }]
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["command"], "stackTrace");
        assert_eq!(out[0]["arguments"]["threadId"], 7);
        assert_eq!(out[0]["arguments"]["levels"], 20);
    }

    #[test]
    fn stopped_falls_back_to_text_and_previous_thread() {
        let mut state = ready_state();
        state.handle_message(&event("stopped", json!({"reason": "step", "threadId": 5})));
        let (out, events) =
            state.handle_message(&event("stopped", json!({"reason": "pause", "text": "hi"})));
        assert_eq!(
            events,
            vec![DebugEvent::Stopped {
                thread_id: 5,
                reason: "pause".into(),
                description: Some("hi".into()),
            }]
        );
        assert_eq!(out[0]["arguments"]["threadId"], 5);
    }

    #[test]
    fn stopped_without_any_thread_defaults_to_one() {
        let mut state = ready_state();
        let (out, events) = state.handle_message(&event("stopped", json!({})));
        assert_eq!(
            events,
            vec![DebugEvent::Stopped {
                thread_id: 1,
                reason: "unknown".into(),
                description: None,
            }]
        );
        assert_eq!(out[0]["arguments"]["threadId"], 1);
    }

    #[test]
    fn stack_trace_response_becomes_frames() {
        let mut state = ready_state();
        let (out, _) = state.handle_message(&event(
            "stopped",
            json!({"reason": "breakpoint", "threadId": 3}),
        ));
        let seq = last_seq(&out);
        let (out2, events) = state.handle_message(&response(
            seq,
            "stackTrace",
            json!({"stackFrames": [
                {"id": 1000, "name": "main", "line": 3, "column": 5,
                 "source": {"name": "a.c", "path": "/src/a.c"}},
                {"id": 1001, "name": "start", "line": 0, "column": 0}
            ], "totalFrames": 2}),
        ));
        assert!(out2.is_empty());
        assert_eq!(
            events,
            vec![DebugEvent::Frames {
                thread_id: 3,
                frames: vec![
                    StackFrame {
                        id: 1000,
                        name: "main".into(),
                        path: Some("/src/a.c".into()),
                        line: 3,
                        column: 5,
                    },
                    StackFrame {
                        id: 1001,
                        name: "start".into(),
                        path: None,
                        line: 0,
                        column: 0,
                    },
                ],
            }]
        );
    }

    #[test]
    fn continued_event_emits_once_per_stop() {
        let mut state = ready_state();
        let (_, events) = state.handle_message(&event("continued", json!({"threadId": 1})));
        assert!(events.is_empty());
        state.handle_message(&event("stopped", json!({"reason": "step", "threadId": 1})));
        let (_, events) = state.handle_message(&event("continued", json!({"threadId": 1})));
        assert_eq!(events, vec![DebugEvent::Continued]);
        let (_, events) = state.handle_message(&event("continued", json!({"threadId": 1})));
        assert!(events.is_empty());
    }

    #[test]
    fn output_event_defaults_category() {
        let mut state = ready_state();
        let (_, events) = state.handle_message(&event("output", json!({"output": "hello\n"})));
        assert_eq!(
            events,
            vec![DebugEvent::Output {
                category: "console".into(),
                text: "hello\n".into(),
            }]
        );
        let (_, events) = state.handle_message(&event(
            "output",
            json!({"category": "stdout", "output": "x"}),
        ));
        assert_eq!(
            events,
            vec![DebugEvent::Output {
                category: "stdout".into(),
                text: "x".into(),
            }]
        );
    }

    #[test]
    fn exited_and_terminated_events() {
        let mut state = ready_state();
        let (_, events) = state.handle_message(&event("exited", json!({"exitCode": 3})));
        assert_eq!(events, vec![DebugEvent::Exited { code: 3 }]);
        assert!(!state.is_terminated());
        let (_, events) = state.handle_message(&event("terminated", json!({})));
        assert_eq!(events, vec![DebugEvent::Terminated]);
        assert!(state.is_terminated());
    }

    #[test]
    fn unrelated_events_are_ignored() {
        let mut state = ready_state();
        for name in ["thread", "module", "breakpoint", "capabilities", "process"] {
            let (out, events) = state.handle_message(&event(name, json!({})));
            assert!(out.is_empty() && events.is_empty(), "{name}");
        }
    }

    #[test]
    fn failed_response_becomes_error() {
        let mut state = state_with(&[]);
        let messages = state.start_messages();
        let seq = last_seq(&messages);
        let (_, events) = state.handle_message(&failure(seq, "launch", "no such program"));
        assert_eq!(
            events,
            vec![DebugEvent::Error("launch failed: no such program".into())]
        );
    }

    #[test]
    fn failed_response_uses_body_error_format() {
        let mut state = ready_state();
        let out = state.request_scopes(4);
        let seq = last_seq(&out);
        let (_, events) = state.handle_message(&json!({
            "type": "response", "request_seq": seq, "success": false,
            "command": "scopes", "body": {"error": {"id": 1, "format": "bad frame"}},
        }));
        assert_eq!(
            events,
            vec![DebugEvent::Error("scopes failed: bad frame".into())]
        );
    }

    #[test]
    fn failed_disconnect_is_silent() {
        let mut state = ready_state();
        let message = state.disconnect();
        assert_eq!(message["arguments"]["terminateDebuggee"], true);
        let seq = message["seq"].as_i64().unwrap();
        let (_, events) = state.handle_message(&failure(seq, "disconnect", "already gone"));
        assert!(events.is_empty());
    }

    #[test]
    fn response_for_unknown_request_is_ignored() {
        let mut state = ready_state();
        let (out, events) = state.handle_message(&response(9999, "stackTrace", json!({})));
        assert!(out.is_empty() && events.is_empty());
    }

    #[test]
    fn response_is_consumed_once() {
        let mut state = ready_state();
        let out = state.evaluate("1+1", None);
        let seq = last_seq(&out);
        let reply = response(seq, "evaluate", json!({"result": "2"}));
        assert_eq!(state.handle_message(&reply).1.len(), 1);
        assert!(state.handle_message(&reply).1.is_empty());
    }

    #[test]
    fn run_in_terminal_reverse_request_is_rejected() {
        let mut state = ready_state();
        let (out, events) = state.handle_message(&json!({
            "seq": 12, "type": "request", "command": "runInTerminal",
            "arguments": {"args": ["a"]},
        }));
        assert!(events.is_empty());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["type"], "response");
        assert_eq!(out[0]["request_seq"], 12);
        assert_eq!(out[0]["success"], false);
        assert_eq!(out[0]["command"], "runInTerminal");
        assert!(
            out[0]["message"]
                .as_str()
                .unwrap()
                .contains("runInTerminal")
        );
        assert!(out[0]["seq"].as_i64().unwrap() > 0);
    }

    #[test]
    fn unknown_reverse_request_is_rejected() {
        let mut state = ready_state();
        let (out, _) = state.handle_message(&json!({
            "seq": 3, "type": "request", "command": "frobnicate",
        }));
        assert_eq!(out[0]["success"], false);
        assert_eq!(out[0]["request_seq"], 3);
        assert!(out[0]["message"].as_str().unwrap().contains("frobnicate"));
    }

    #[test]
    fn reverse_response_seq_does_not_collide_with_requests() {
        let mut state = ready_state();
        let (out, _) = state.handle_message(&json!({"seq": 1, "type": "request", "command": "x"}));
        let reply_seq = out[0]["seq"].as_i64().unwrap();
        let next = state.continue_();
        assert!(next[0]["seq"].as_i64().unwrap() > reply_seq);
    }

    #[test]
    fn scopes_and_variables_round_trip() {
        let mut state = ready_state();
        let out = state.request_scopes(1000);
        assert_eq!(out[0]["command"], "scopes");
        assert_eq!(out[0]["arguments"]["frameId"], 1000);
        let (_, events) = state.handle_message(&response(
            last_seq(&out),
            "scopes",
            json!({"scopes": [
                {"name": "Locals", "variablesReference": 11, "expensive": false},
                {"name": "Registers", "variablesReference": 12},
            ]}),
        ));
        assert_eq!(
            events,
            vec![DebugEvent::Scopes {
                frame_id: 1000,
                scopes: vec![
                    Scope {
                        name: "Locals".into(),
                        variables_reference: 11,
                    },
                    Scope {
                        name: "Registers".into(),
                        variables_reference: 12,
                    },
                ],
            }]
        );

        let out = state.request_variables(11);
        assert_eq!(out[0]["arguments"]["variablesReference"], 11);
        let (_, events) = state.handle_message(&response(
            last_seq(&out),
            "variables",
            json!({"variables": [
                {"name": "x", "value": "1", "type": "int", "variablesReference": 0},
                {"name": "p", "value": "0x10", "variablesReference": 40},
            ]}),
        ));
        assert_eq!(
            events,
            vec![DebugEvent::Variables {
                reference: 11,
                variables: vec![
                    Variable {
                        name: "x".into(),
                        value: "1".into(),
                        type_name: Some("int".into()),
                        variables_reference: 0,
                    },
                    Variable {
                        name: "p".into(),
                        value: "0x10".into(),
                        type_name: None,
                        variables_reference: 40,
                    },
                ],
            }]
        );
    }

    #[test]
    fn evaluate_success_and_failure() {
        let mut state = ready_state();
        let out = state.evaluate("x + 1", Some(1000));
        assert_eq!(out[0]["command"], "evaluate");
        assert_eq!(out[0]["arguments"]["expression"], "x + 1");
        assert_eq!(out[0]["arguments"]["frameId"], 1000);
        assert_eq!(out[0]["arguments"]["context"], "repl");
        let (_, events) = state.handle_message(&response(
            last_seq(&out),
            "evaluate",
            json!({"result": "2", "variablesReference": 0}),
        ));
        assert_eq!(
            events,
            vec![DebugEvent::Evaluated {
                expression: "x + 1".into(),
                result: "2".into(),
                ok: true,
            }]
        );

        let out = state.evaluate("nope", None);
        assert!(out[0]["arguments"].get("frameId").is_none());
        let (_, events) =
            state.handle_message(&failure(last_seq(&out), "evaluate", "use of undeclared"));
        assert_eq!(
            events,
            vec![DebugEvent::Evaluated {
                expression: "nope".into(),
                result: "use of undeclared".into(),
                ok: false,
            }]
        );
    }

    #[test]
    fn execution_requests_use_stopped_thread() {
        let mut state = ready_state();
        assert_eq!(state.continue_()[0]["arguments"]["threadId"], 1);
        state.handle_message(&event("stopped", json!({"reason": "step", "threadId": 9})));
        let cases = [
            (state.continue_(), "continue"),
            (state.pause(), "pause"),
            (state.step_over(), "next"),
            (state.step_in(), "stepIn"),
            (state.step_out(), "stepOut"),
        ];
        for (messages, command) in cases {
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0]["command"], command);
            assert_eq!(messages[0]["arguments"]["threadId"], 9, "{command}");
        }
    }

    #[test]
    fn seq_increases_monotonically() {
        let mut state = ready_state();
        let a = last_seq(&state.step_over());
        let b = last_seq(&state.step_in());
        let c = last_seq(&state.request_variables(1));
        assert!(a < b && b < c);
    }

    #[test]
    fn execution_responses_emit_nothing() {
        let mut state = ready_state();
        for messages in [state.continue_(), state.step_over(), state.pause()] {
            let seq = last_seq(&messages);
            let command = messages[0]["command"].as_str().unwrap().to_owned();
            let (out, events) = state.handle_message(&response(seq, &command, json!({})));
            assert!(out.is_empty() && events.is_empty(), "{command}");
        }
    }

    #[test]
    fn malformed_message_reports_error() {
        let mut state = ready_state();
        let (out, events) = state.handle_message(&json!({"hello": "world"}));
        assert!(out.is_empty());
        assert!(matches!(events.as_slice(), [DebugEvent::Error(_)]));
    }

    #[test]
    fn stack_frame_with_missing_fields_defaults() {
        let parsed = parse_frame(&json!({"id": 2}));
        assert_eq!(
            parsed,
            StackFrame {
                id: 2,
                name: String::new(),
                path: None,
                line: 0,
                column: 0,
            }
        );
    }

    #[test]
    fn framing_round_trips() {
        let payload = br#"{"seq":1,"type":"event","event":"initialized"}"#;
        let bytes = frame(payload);
        assert!(bytes.starts_with(b"Content-Length: 46\r\n\r\n"));
        let read = shared_runtime()
            .unwrap()
            .block_on(async {
                let mut reader = BufReader::new(&bytes[..]);
                read_frame(&mut reader).await
            })
            .unwrap();
        assert_eq!(read.as_deref(), Some(&payload[..]));
    }

    #[test]
    fn start_rejects_empty_command() {
        let result = DebugSession::start(&[], config(), HashMap::new());
        assert!(result.is_err());
    }

    // ---- integration: real lldb-dap --------------------------------------

    use std::path::PathBuf;
    use std::process::Command as StdCommand;

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Polls until `pred` matches a buffered event. Panics with the full
    /// event log on timeout.
    fn wait_for(
        session: &mut DebugSession,
        buffer: &mut Vec<DebugEvent>,
        log: &mut Vec<DebugEvent>,
        what: &str,
        pred: impl Fn(&DebugEvent) -> bool,
    ) -> DebugEvent {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let new = session.poll();
            log.extend(new.iter().cloned());
            buffer.extend(new);
            if let Some(position) = buffer.iter().position(&pred) {
                return buffer.remove(position);
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; events so far: {log:#?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn lldb_dap_breakpoint_step_and_variables() {
        let Some(adapter) = find_lldb_dap() else {
            eprintln!("skipping: lldb-dap not found");
            return;
        };
        if StdCommand::new("clang").arg("--version").output().is_err() {
            eprintln!("skipping: clang not found");
            return;
        }
        let dir =
            TempDir(std::env::temp_dir().join(format!("galaxy_dap_test_{}", std::process::id())));
        std::fs::create_dir_all(&dir.0).unwrap();
        let source = dir.0.join("dap_test.c");
        let binary = dir.0.join("dap_test");
        std::fs::write(
            &source,
            "int main(void) {\n    int x = 1;\n    x = x + 2;\n    return x;\n}\n",
        )
        .unwrap();
        let compile = StdCommand::new("clang")
            .args(["-g", "-O0", "-o"])
            .arg(&binary)
            .arg(&source)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "clang failed: {}",
            String::from_utf8_lossy(&compile.stderr)
        );

        let source_path = source.to_string_lossy().into_owned();
        let mut breakpoints = HashMap::new();
        breakpoints.insert(source_path.clone(), vec![3u32]);
        let mut session = DebugSession::start(
            &[adapter],
            LaunchConfig {
                program: binary.to_string_lossy().into_owned(),
                args: Vec::new(),
                cwd: Some(dir.0.to_string_lossy().into_owned()),
                stop_on_entry: false,
            },
            breakpoints,
        )
        .unwrap();
        assert!(session.is_running());

        let mut buffer = Vec::new();
        let mut log = Vec::new();

        let stopped = wait_for(
            &mut session,
            &mut buffer,
            &mut log,
            "breakpoint stop",
            |e| matches!(e, DebugEvent::Stopped { reason, .. } if reason.contains("breakpoint")),
        );
        let DebugEvent::Stopped { thread_id, .. } = stopped else {
            unreachable!()
        };

        let frames = wait_for(&mut session, &mut buffer, &mut log, "frames", |e| {
            matches!(e, DebugEvent::Frames { .. })
        });
        let DebugEvent::Frames {
            thread_id: frames_thread,
            frames,
        } = frames
        else {
            unreachable!()
        };
        assert_eq!(frames_thread, thread_id);
        let top = frames
            .iter()
            .find(|frame| {
                frame
                    .path
                    .as_deref()
                    .is_some_and(|path| path.ends_with("dap_test.c"))
            })
            .unwrap_or_else(|| panic!("no dap_test.c frame in {frames:#?}"));
        assert_eq!(top.line, 3);

        session.request_scopes(top.id);
        let scopes = wait_for(&mut session, &mut buffer, &mut log, "scopes", |e| {
            matches!(e, DebugEvent::Scopes { .. })
        });
        let DebugEvent::Scopes { scopes, .. } = scopes else {
            unreachable!()
        };
        let locals = scopes
            .iter()
            .find(|scope| scope.name.to_lowercase().contains("local"))
            .or_else(|| scopes.first())
            .expect("at least one scope");
        session.request_variables(locals.variables_reference);
        let variables = wait_for(&mut session, &mut buffer, &mut log, "variables", |e| {
            matches!(e, DebugEvent::Variables { .. })
        });
        let DebugEvent::Variables { variables, .. } = variables else {
            unreachable!()
        };
        assert!(
            variables.iter().any(|variable| variable.name == "x"),
            "no variable x in {variables:#?}"
        );

        session.evaluate("x", Some(top.id));
        let evaluated = wait_for(&mut session, &mut buffer, &mut log, "evaluate", |e| {
            matches!(e, DebugEvent::Evaluated { .. })
        });
        assert!(
            matches!(&evaluated, DebugEvent::Evaluated { ok: true, result, .. } if result.contains('1')),
            "unexpected evaluate result: {evaluated:?}"
        );

        session.step_over();
        wait_for(&mut session, &mut buffer, &mut log, "step stop", |e| {
            matches!(e, DebugEvent::Stopped { .. })
        });
        let frames = wait_for(
            &mut session,
            &mut buffer,
            &mut log,
            "frames after step",
            |e| matches!(e, DebugEvent::Frames { .. }),
        );
        if let DebugEvent::Frames { frames, .. } = frames {
            assert!(
                frames.iter().any(|frame| frame.line == 4
                    && frame
                        .path
                        .as_deref()
                        .is_some_and(|path| path.ends_with("dap_test.c"))),
                "expected line 4 after step, got {frames:#?}"
            );
        }

        session.continue_();
        wait_for(&mut session, &mut buffer, &mut log, "termination", |e| {
            matches!(e, DebugEvent::Terminated | DebugEvent::Exited { .. })
        });

        session.stop();
        assert!(!session.is_running());
        session.stop();
    }
}
