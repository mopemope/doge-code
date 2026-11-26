//! ClaudeAdapter: drives Claude Code (`claude -p`) over stream-json stdio.
//!
//! The wire format is the one recorded from Claude Code 2.1.332
//! (`tests/fixtures/README.md`, "Wire notes: Claude Code"). One process serves
//! one session for many turns; stdin closes it.
//!
//! - Launch: `-p --input-format stream-json --output-format stream-json
//!   --verbose --permission-prompt-tool stdio` plus `--model`,
//!   `--permission-mode` and `--resume` when set. `--permission-prompt-tool
//!   stdio` is hidden from `--help` but required: without it actions needing
//!   approval are denied without asking.
//! - `start` spawns the CLI and performs the `initialize` control request,
//!   bounded by [`CONTROL_TIMEOUT`]. If the CLI exits meanwhile, the error
//!   carries its stderr tail.
//! - Event mapping (CLI → [`ProviderEvent`]):
//!   - `system/init` (sent at the start of every turn) → `SessionStarted`.
//!   - `assistant`: each `text` block → `AssistantText { final_: true }` (split
//!     into 256 KiB chunks, all but the last `final_: false`), each
//!     `tool_use` block → [`EventKind::tool_call`]; `thinking` is ignored.
//!   - `user` `tool_result` blocks → [`EventKind::tool_result`]; plain text
//!     user blocks (e.g. the interrupt marker) are ignored.
//!   - `result` → `TurnCompleted { usage }`; with `is_error: true` (success is
//!     judged by `is_error`, not `subtype`) an `Error` event with the
//!     provider's bounded message comes first.
//!   - `control_request` `can_use_tool` → `PermissionRequested` (`Bash` →
//!     Command, `Edit|Write|MultiEdit|NotebookEdit` → FileChange, others →
//!     Tool); other control requests are answered with an error response.
//!   - `rate_limit_event` with `rejected` or an unknown status, or HTTP 429
//!     in an `assistant` or `result` row, → `RateLimited` once per turn.
//!   - An oversized stdout line → `Error` and the session continues.
//!   - Anything else (hook and thinking-token `system` lines, unknown types)
//!     is ignored with a debug log of its type.
//! - Decisions: Allow and AllowForSession → `allow` with `updatedInput` = the
//!   original tool input (no session-wide rule is sent); Deny → `deny` with a
//!   fixed message.
//! - Interrupt: with no turn in flight (its `result` already arrived) nothing
//!   is sent. Otherwise outstanding permission requests are answered `deny`,
//!   then the `interrupt` control request is sent and its response awaited.
//!   The interrupted turn's `result` arrives after that response and is
//!   swallowed, as are permission requests until then (answered `deny`). If
//!   every interrupted turn had started (sent `system/init`), the next
//!   `system/init` ends the swallow window even when no `result` came, since
//!   the CLI runs turns in order. Interrupting a turn that had not started
//!   logs a warning and relies on a `result` arriving (not recorded).
//! - The session owns the process group: dropping it (e.g. a manager timeout
//!   cancelling a call) kills the group. `shutdown` closes stdin, waits briefly
//!   and then terminates. Stdout EOF (read errors surface as EOF too) plus the
//!   exit status yields one `Exited`, unless `shutdown` caused it.
//! - Logs carry message types and ids only, never prompt or output text.
//!
//! The reader task applies stdout lines in order and blocks while the event
//! channel ([`EVENT_CHANNEL_CAPACITY`]) is full, so control responses queue
//! behind unread events; without partial messages the CLI emits one line per
//! content block, far fewer than the capacity per call.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard, PoisonError};
use std::time::Duration;

use dgc_protocol::event::EventKind;
use dgc_protocol::model::{Decision, PermissionKind, ProviderKind};
use dgc_protocol::{chunk_text, truncate_summary};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::process::ChildStdin;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};
use tracing::{debug, warn};

use super::{
    AgentSpec, BoxFut, ProviderAdapter, ProviderError, ProviderEvent, ProviderSession,
    rate_limited_event,
};
use crate::process::{ChildIo, ChildProcess, ChildSpec, LineEvent, TailBuffer};

/// `--permission-mode` values accepted by Claude Code 2.1.332.
const MODES: [&str; 6] = [
    "acceptEdits",
    "auto",
    "bypassPermissions",
    "manual",
    "dontAsk",
    "plan",
];
/// Modes that let Claude act without asking.
const AUTO_APPROVE_MODES: [&str; 3] = ["bypassPermissions", "auto", "acceptEdits"];

/// Bound on waiting for a control response (`initialize`, `interrupt`).
pub const CONTROL_TIMEOUT: Duration = Duration::from_secs(20);
/// Bound on writing one line to the CLI.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// After stdout EOF, how long to wait for the exit status before killing.
const EXIT_WAIT: Duration = Duration::from_secs(5);
/// Grace used when a process that closed stdout has to be terminated.
const EXIT_TERMINATE_GRACE: Duration = Duration::from_secs(1);
/// After exit, how long to wait for the stderr reader to drain.
const STDERR_SETTLE: Duration = Duration::from_millis(500);
/// How long a failed `start` waits for the exit report.
const STARTUP_EXIT_WAIT: Duration = Duration::from_secs(7);
/// `shutdown`: wait for a clean exit after closing stdin, then terminate.
const SHUTDOWN_EXIT_WAIT: Duration = Duration::from_secs(1);
const SHUTDOWN_TERMINATE_GRACE: Duration = Duration::from_secs(2);
/// Bound on stopping the reader task during `shutdown`.
const READER_STOP_WAIT: Duration = Duration::from_secs(1);
pub const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// Outbound message shapes, as recorded.
mod wire {
    use serde_json::{Value, json};

    pub(super) const BASE_ARGS: [&str; 8] = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--permission-prompt-tool",
        "stdio",
    ];
    pub(super) const DENY_MESSAGE: &str = "Denied by dgc user";

    pub(super) fn control_request(request_id: &str, request: Value) -> Value {
        json!({"type": "control_request", "request_id": request_id, "request": request})
    }

    pub(super) fn user_message(text: &str) -> Value {
        json!({
            "type": "user",
            "message": {"role": "user", "content": [{"type": "text", "text": text}]},
            "parent_tool_use_id": null,
            "session_id": "",
        })
    }

    fn control_success(request_id: &str, response: Value) -> Value {
        json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": request_id, "response": response},
        })
    }

    pub(super) fn permission_allow(request_id: &str, input: Value) -> Value {
        control_success(
            request_id,
            json!({"behavior": "allow", "updatedInput": input}),
        )
    }

    pub(super) fn permission_deny(request_id: &str) -> Value {
        control_success(
            request_id,
            json!({"behavior": "deny", "message": DENY_MESSAGE}),
        )
    }

    /// Error answer to a control request dgc does not handle. Not recorded;
    /// this is the Agent SDK's shape.
    pub(super) fn control_error(request_id: &str, error: &str) -> Value {
        json!({
            "type": "control_response",
            "response": {"subtype": "error", "request_id": request_id, "error": error},
        })
    }
}

pub struct ClaudeAdapter {
    program: PathBuf,
    prefix_args: Vec<String>,
}

impl ClaudeAdapter {
    /// Run `program` (normally `claude`, resolved on `PATH`).
    pub fn new(program: PathBuf) -> Self {
        Self::with_program(program, Vec::new())
    }

    /// Run `program` with `prefix_args` before the Claude arguments (tests
    /// pass the fake provider and its scenario).
    pub fn with_program(program: PathBuf, prefix_args: Vec<String>) -> Self {
        Self {
            program,
            prefix_args,
        }
    }

    fn args(&self, spec: &AgentSpec) -> Vec<String> {
        let mut args = self.prefix_args.clone();
        args.extend(wire::BASE_ARGS.iter().map(|arg| (*arg).to_owned()));
        let options = [
            ("--model", &spec.model),
            ("--permission-mode", &spec.mode),
            ("--resume", &spec.resume_session_id),
        ];
        for (flag, value) in options {
            if let Some(value) = value {
                args.push(flag.to_owned());
                args.push(value.clone());
            }
        }
        args
    }
}

impl ProviderAdapter for ClaudeAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Claude
    }

    fn validate_mode(&self, mode: Option<&str>) -> Result<bool, String> {
        match mode {
            None => Ok(false),
            Some(mode) if MODES.contains(&mode) => Ok(AUTO_APPROVE_MODES.contains(&mode)),
            Some(mode) => Err(format!(
                "unknown Claude permission mode {mode:?}; expected one of {}",
                MODES.join(", ")
            )),
        }
    }

    fn start(
        &self,
        spec: AgentSpec,
    ) -> BoxFut<'_, Result<Box<dyn ProviderSession>, ProviderError>> {
        Box::pin(async move {
            let child = ChildSpec {
                program: self.program.clone(),
                args: self.args(&spec),
                cwd: spec.cwd,
            };
            let (process, io) =
                ChildProcess::spawn(child).map_err(|e| ProviderError::Spawn(e.to_string()))?;
            debug!(agent_id = %spec.agent_id, "claude process started");
            // From here the session owns the process: an early return or a
            // cancelled future kills it.
            let mut session = ClaudeSession::new(process, io);
            if let Err(err) = session.initialize().await {
                return Err(session.startup_failure(err).await);
            }
            Ok(Box::new(session) as Box<dyn ProviderSession>)
        })
    }
}

fn lock<T>(mutex: &StdMutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, Clone, Copy)]
enum Control {
    Initialize,
    Interrupt,
}

impl Control {
    fn name(self) -> &'static str {
        match self {
            Self::Initialize => "initialize",
            Self::Interrupt => "interrupt",
        }
    }
}

#[derive(Default)]
struct State {
    /// dgc's control requests awaiting a response.
    controls: HashMap<String, (Control, oneshot::Sender<Value>)>,
    /// Unanswered `can_use_tool` requests: request id → tool input.
    permissions: HashMap<String, Value>,
    /// User messages sent whose `result` has not arrived.
    turns_in_flight: u32,
    /// How many of the in-flight turns have seen their `system/init`. The
    /// CLI runs turns in order, so these are the oldest ones.
    turns_started: u32,
    rate_limit_emitted: bool,
    /// `result`s still to arrive for interrupted turns; they are dropped.
    swallow_results: u32,
    /// Every interrupted turn had started when the interrupt was answered,
    /// so their results must precede the next `system/init`: that init
    /// closes the swallow window even if a result never came.
    swallow_ends_at_init: bool,
    /// An interrupt is in progress.
    interrupting: bool,
    /// Stdout reached EOF.
    exited: bool,
}

impl State {
    fn rate_limited(&mut self, reason: &str, resets_at: Option<i64>) -> Option<ProviderEvent> {
        if self.rate_limit_emitted || self.swallow_results > 0 {
            return None;
        }
        self.rate_limit_emitted = true;
        Some(rate_limited_event(ProviderKind::Claude, reason, resets_at))
    }
}

/// State shared by the session and its stdout reader task.
struct Shared {
    stdin: Mutex<Option<ChildStdin>>,
    state: StdMutex<State>,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    /// Write one JSON line, bounded by [`WRITE_TIMEOUT`].
    async fn write(&self, message: &Value) -> Result<(), ProviderError> {
        let mut line = message.to_string().into_bytes();
        line.push(b'\n');
        let write = async {
            let mut stdin = self.stdin.lock().await;
            let stdin = stdin.as_mut().ok_or(ProviderError::Exited)?;
            stdin.write_all(&line).await.map_err(io_error)?;
            stdin.flush().await.map_err(io_error)
        };
        timeout(WRITE_TIMEOUT, write)
            .await
            .unwrap_or(Err(ProviderError::TimedOut("write")))
    }
}

fn io_error(err: io::Error) -> ProviderError {
    if err.kind() == io::ErrorKind::BrokenPipe {
        ProviderError::Exited
    } else {
        ProviderError::Io(err.to_string())
    }
}

struct ClaudeSession {
    shared: Arc<Shared>,
    process: Arc<Mutex<ChildProcess>>,
    events: mpsc::Receiver<ProviderEvent>,
    reader: Option<JoinHandle<()>>,
    shutting_down: Arc<AtomicBool>,
    next_request: u64,
}

impl ClaudeSession {
    fn new(process: ChildProcess, io: ChildIo) -> Self {
        let ChildIo {
            stdin,
            lines,
            stderr_tail,
        } = io;
        let shared = Arc::new(Shared {
            stdin: Mutex::new(Some(stdin)),
            state: StdMutex::new(State::default()),
        });
        let process = Arc::new(Mutex::new(process));
        let shutting_down = Arc::new(AtomicBool::new(false));
        let (tx, events) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let reader = tokio::spawn(read_loop(Reader {
            shared: Arc::clone(&shared),
            process: Arc::clone(&process),
            lines,
            stderr_tail,
            events: tx,
            shutting_down: Arc::clone(&shutting_down),
        }));
        Self {
            shared,
            process,
            events,
            reader: Some(reader),
            shutting_down,
            next_request: 0,
        }
    }

    async fn initialize(&mut self) -> Result<(), ProviderError> {
        let response = self
            .control(Control::Initialize, json!({"subtype": "initialize"}))
            .await?;
        debug!(
            version = response["claude_code_version"]
                .as_str()
                .unwrap_or("unknown"),
            "claude initialized"
        );
        Ok(())
    }

    /// Turn a failed `initialize` into the start error: if the CLI exited,
    /// report its exit with the stderr tail.
    async fn startup_failure(&mut self, err: ProviderError) -> ProviderError {
        if !matches!(err, ProviderError::Exited | ProviderError::Io(_)) {
            return err;
        }
        let deadline = Instant::now() + STARTUP_EXIT_WAIT;
        loop {
            match tokio::time::timeout_at(deadline, self.events.recv()).await {
                Ok(Some(ProviderEvent::Exited {
                    success,
                    stderr_tail,
                })) => {
                    let tail = stderr_tail.trim();
                    return ProviderError::Spawn(if tail.is_empty() {
                        format!("provider exited during startup (success: {success})")
                    } else {
                        format!("provider exited during startup (success: {success}): {tail}")
                    });
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => return err,
            }
        }
    }

    /// Send a control request and wait for its response payload.
    async fn control(&mut self, kind: Control, request: Value) -> Result<Value, ProviderError> {
        self.next_request += 1;
        let request_id = format!("dgc-{}", self.next_request);
        let (tx, rx) = oneshot::channel();
        {
            let mut state = self.shared.state();
            if state.exited {
                return Err(ProviderError::Exited);
            }
            state.controls.insert(request_id.clone(), (kind, tx));
        }
        let forget = |shared: &Shared| {
            shared.state().controls.remove(&request_id);
        };
        if let Err(err) = self
            .shared
            .write(&wire::control_request(&request_id, request))
            .await
        {
            forget(&self.shared);
            return Err(err);
        }
        let response = match timeout(CONTROL_TIMEOUT, rx).await {
            Ok(Ok(response)) => response,
            // The reader dropped the waiter: stdout ended.
            Ok(Err(_)) => return Err(ProviderError::Exited),
            Err(_) => {
                forget(&self.shared);
                return Err(ProviderError::TimedOut(kind.name()));
            }
        };
        if response["subtype"] == "success" {
            Ok(response.get("response").cloned().unwrap_or(Value::Null))
        } else {
            let error = response["error"].as_str().unwrap_or("no detail");
            Err(ProviderError::Protocol(format!(
                "{} request failed: {}",
                kind.name(),
                truncate_summary(error)
            )))
        }
    }
}

impl ProviderSession for ClaudeSession {
    fn send_user<'a>(&'a mut self, text: &'a str) -> BoxFut<'a, Result<(), ProviderError>> {
        Box::pin(async move {
            {
                let mut state = self.shared.state();
                if state.exited {
                    return Err(ProviderError::Exited);
                }
                if state.turns_in_flight == 0 {
                    state.rate_limit_emitted = false;
                }
                state.turns_in_flight += 1;
            }
            let written = self.shared.write(&wire::user_message(text)).await;
            if written.is_err() {
                let mut state = self.shared.state();
                state.turns_in_flight = state.turns_in_flight.saturating_sub(1);
            }
            written
        })
    }

    fn steer<'a>(&'a mut self, _text: &'a str) -> BoxFut<'a, Result<bool, ProviderError>> {
        // Claude Code has no way to add input to a running turn.
        Box::pin(async { Ok(false) })
    }

    fn interrupt(&mut self) -> BoxFut<'_, Result<(), ProviderError>> {
        Box::pin(async move {
            let outstanding: Vec<String> = {
                let mut state = self.shared.state();
                if state.turns_in_flight == 0 {
                    // The turn already ended; its TurnCompleted is buffered.
                    // Interrupting an idle CLI was never recorded.
                    debug!("claude interrupt skipped: no turn in flight");
                    return Ok(());
                }
                state.interrupting = true;
                state.permissions.drain().map(|(id, _)| id).collect()
            };
            let result = async {
                // Outstanding requests are cancelled; answer them so the CLI
                // does not wait on them.
                for request_id in &outstanding {
                    self.shared
                        .write(&wire::permission_deny(request_id))
                        .await?;
                }
                self.control(Control::Interrupt, json!({"subtype": "interrupt"}))
                    .await
                    .map(drop)
            }
            .await;
            self.shared.state().interrupting = false;
            result
        })
    }

    fn respond_permission<'a>(
        &'a mut self,
        provider_request_id: &'a str,
        d: Decision,
    ) -> BoxFut<'a, Result<(), ProviderError>> {
        Box::pin(async move {
            let input = self
                .shared
                .state()
                .permissions
                .remove(provider_request_id)
                .ok_or_else(|| ProviderError::Protocol("unknown permission request".to_owned()))?;
            let message = match d {
                Decision::Allow | Decision::AllowForSession => {
                    wire::permission_allow(provider_request_id, input)
                }
                Decision::Deny => wire::permission_deny(provider_request_id),
            };
            self.shared.write(&message).await
        })
    }

    fn events(&mut self) -> &mut mpsc::Receiver<ProviderEvent> {
        &mut self.events
    }

    fn shutdown(&mut self) -> BoxFut<'_, Result<(), ProviderError>> {
        Box::pin(async move {
            self.shutting_down.store(true, Ordering::SeqCst);
            if let Some(reader) = self.reader.take() {
                reader.abort();
                let _ = timeout(READER_STOP_WAIT, reader).await;
            }
            // Closing stdin asks the CLI to exit.
            if let Ok(mut stdin) = timeout(WRITE_TIMEOUT, self.shared.stdin.lock()).await {
                stdin.take();
            }
            let mut process = self.process.lock().await;
            if let Ok(Ok(_)) = timeout(SHUTDOWN_EXIT_WAIT, process.wait()).await {
                return Ok(());
            }
            process
                .terminate(SHUTDOWN_TERMINATE_GRACE)
                .await
                .map(drop)
                .map_err(|e| ProviderError::Io(e.to_string()))
        })
    }
}

impl Drop for ClaudeSession {
    fn drop(&mut self) {
        // The reader holds the other reference to the process; once it is
        // gone the `ChildProcess` drop kills the group.
        if let Some(reader) = self.reader.take() {
            reader.abort();
        }
    }
}

struct Reader {
    shared: Arc<Shared>,
    process: Arc<Mutex<ChildProcess>>,
    lines: mpsc::Receiver<LineEvent>,
    stderr_tail: Arc<StdMutex<TailBuffer>>,
    events: mpsc::Sender<ProviderEvent>,
    shutting_down: Arc<AtomicBool>,
}

/// Apply stdout lines in order; at EOF report the exit once.
async fn read_loop(mut reader: Reader) {
    while let Some(line) = reader.lines.recv().await {
        let events = match line {
            LineEvent::Line(text) => handle_line(&reader.shared, &text).await,
            LineEvent::Oversized { bytes } => {
                debug!(bytes, "claude stdout line exceeded the limit");
                vec![ProviderEvent::Event(EventKind::Error {
                    message: "provider line exceeded 16 MiB".to_owned(),
                })]
            }
            LineEvent::Eof => break,
        };
        for event in events {
            if reader.events.send(event).await.is_err() {
                return; // The session is gone.
            }
        }
    }
    {
        let mut state = reader.shared.state();
        state.exited = true;
        // Dropping the waiters fails pending control requests.
        state.controls.clear();
        state.permissions.clear();
    }
    if reader.shutting_down.load(Ordering::SeqCst) {
        return;
    }
    let success = wait_exit(&reader.process).await;
    settle_stderr(&reader.stderr_tail).await;
    let stderr_tail = lock(&reader.stderr_tail).contents();
    if reader.shutting_down.load(Ordering::SeqCst) {
        return;
    }
    debug!(success, "claude process exited");
    let _ = reader
        .events
        .send(ProviderEvent::Exited {
            success,
            stderr_tail,
        })
        .await;
}

/// The exit status after stdout EOF; a process that keeps running without
/// stdout is terminated.
async fn wait_exit(process: &Mutex<ChildProcess>) -> bool {
    let mut process = process.lock().await;
    match timeout(EXIT_WAIT, process.wait()).await {
        Ok(Ok(status)) => status.success(),
        Ok(Err(error)) => {
            debug!(%error, "waiting for claude failed");
            false
        }
        Err(_) => {
            if let Err(error) = process.terminate(EXIT_TERMINATE_GRACE).await {
                debug!(%error, "terminating claude failed");
            }
            false
        }
    }
}

/// Wait (bounded) until the stderr reader has drained the pipe; it drops its
/// reference to the tail at stderr EOF.
async fn settle_stderr(tail: &Arc<StdMutex<TailBuffer>>) {
    let deadline = Instant::now() + STDERR_SETTLE;
    while Arc::strong_count(tail) > 1 && Instant::now() < deadline {
        sleep(Duration::from_millis(5)).await;
    }
}

async fn handle_line(shared: &Shared, text: &str) -> Vec<ProviderEvent> {
    let Ok(message) = serde_json::from_str::<Value>(text) else {
        debug!("ignoring non-JSON claude line");
        return Vec::new();
    };
    let message_type = message["type"].as_str().unwrap_or("");
    match message_type {
        "control_response" => {
            control_response(shared, &message);
            Vec::new()
        }
        "control_request" => control_request(shared, &message).await,
        "system" => match message["subtype"].as_str() {
            Some("init") => {
                turn_started(shared);
                match message["session_id"].as_str() {
                    Some(id) => vec![ProviderEvent::SessionStarted {
                        session_id: id.to_owned(),
                    }],
                    None => {
                        debug!("claude system/init without session_id");
                        Vec::new()
                    }
                }
            }
            subtype => {
                debug!(subtype, "ignoring claude system message");
                Vec::new()
            }
        },
        "assistant" => {
            let mut events = assistant_events(&message);
            if message["api_error_status"].as_u64() == Some(429) {
                events.extend(shared.state().rate_limited("http_429", None));
            }
            events
        }
        "user" => tool_results(&message),
        "result" => turn_result(shared, &message),
        "rate_limit_event" => {
            let info = &message["rate_limit_info"];
            match info["status"].as_str() {
                Some("allowed" | "allowed_warning") => {
                    debug!("dropped routine claude rate limit update");
                    Vec::new()
                }
                Some(status) => shared
                    .state()
                    .rate_limited(status, info["resetsAt"].as_i64())
                    .into_iter()
                    .collect(),
                None => Vec::new(),
            }
        }
        other => {
            debug!(message_type = other, "ignoring unknown claude message");
            Vec::new()
        }
    }
}

/// A `system/init`: the next queued turn has started. Results of interrupted
/// turns that had started before must have arrived by now; any still
/// expected were never sent, so stop waiting for them.
fn turn_started(shared: &Shared) {
    let mut state = shared.state();
    if state.swallow_results > 0 && state.swallow_ends_at_init {
        debug!(
            missing = state.swallow_results,
            "interrupted claude turn ended without a result"
        );
        let missing = state.swallow_results;
        state.turns_in_flight = state.turns_in_flight.saturating_sub(missing);
        state.turns_started = state.turns_started.saturating_sub(missing);
        state.swallow_results = 0;
        state.rate_limit_emitted = false;
    }
    if state.turns_started < state.turns_in_flight {
        state.turns_started += 1;
    }
}

fn control_response(shared: &Shared, message: &Value) {
    let response = &message["response"];
    let Some(request_id) = response["request_id"].as_str() else {
        debug!("claude control response without request_id");
        return;
    };
    let waiter = {
        let mut state = shared.state();
        let waiter = state.controls.remove(request_id);
        if let Some((Control::Interrupt, _)) = &waiter
            && response["subtype"] == "success"
        {
            // Lines are applied in order, so the turns still in flight are the
            // interrupted ones; their results come after this response.
            state.swallow_results = state.turns_in_flight;
            state.swallow_ends_at_init = state.turns_started >= state.turns_in_flight;
            if !state.swallow_ends_at_init {
                warn!(
                    request_id,
                    in_flight = state.turns_in_flight,
                    started = state.turns_started,
                    "claude interrupted a turn that had not started; its result may never come"
                );
            }
        }
        waiter
    };
    match waiter {
        Some((_, waiter)) => {
            let _ = waiter.send(response.clone());
        }
        None => debug!(request_id, "claude control response without a waiter"),
    }
}

async fn control_request(shared: &Shared, message: &Value) -> Vec<ProviderEvent> {
    let Some(request_id) = message["request_id"].as_str() else {
        debug!("claude control request without request_id");
        return Vec::new();
    };
    let request = &message["request"];
    let subtype = request["subtype"].as_str().unwrap_or("");
    if subtype != "can_use_tool" {
        debug!(subtype, "answering unsupported claude control request");
        if let Err(error) = shared
            .write(&wire::control_error(request_id, "unsupported by dgc"))
            .await
        {
            debug!(%error, "could not answer claude control request");
        }
        return Vec::new();
    }
    let tool = request["tool_name"].as_str().unwrap_or("unknown");
    let input = request.get("input").cloned().unwrap_or_else(|| json!({}));
    let summary = format!("{tool}: {}", input_summary(&input));
    let cancelled = {
        let mut state = shared.state();
        let cancelled = state.interrupting || state.swallow_results > 0;
        if !cancelled {
            state.permissions.insert(request_id.to_owned(), input);
        }
        cancelled
    };
    if cancelled {
        debug!(request_id, "denying permission request of interrupted turn");
        if let Err(error) = shared.write(&wire::permission_deny(request_id)).await {
            debug!(%error, "could not deny claude permission request");
        }
        return Vec::new();
    }
    vec![ProviderEvent::PermissionRequested {
        provider_request_id: request_id.to_owned(),
        kind: permission_kind(tool),
        summary,
        raw: request.clone(),
    }]
}

fn permission_kind(tool: &str) -> PermissionKind {
    match tool {
        "Bash" => PermissionKind::Command,
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => PermissionKind::FileChange,
        _ => PermissionKind::Tool,
    }
}

/// The most telling input field of a tool call, else its compact JSON.
fn input_summary(input: &Value) -> String {
    ["command", "file_path", "notebook_path", "path"]
        .iter()
        .find_map(|key| input[*key].as_str())
        .map_or_else(|| input.to_string(), str::to_owned)
}

fn content_blocks(message: &Value) -> &[Value] {
    message["message"]["content"]
        .as_array()
        .map_or(&[], Vec::as_slice)
}

/// A long text block becomes several `AssistantText` events of at most
/// [`MAX_EVENT_TEXT_BYTES`](dgc_protocol::MAX_EVENT_TEXT_BYTES); all but the
/// last are `final_: false`.
fn assistant_events(message: &Value) -> Vec<ProviderEvent> {
    content_blocks(message)
        .iter()
        .flat_map(|block| match block["type"].as_str() {
            Some("text") => {
                let chunks = chunk_text(block["text"].as_str().unwrap_or(""));
                let last = chunks.len().saturating_sub(1);
                chunks
                    .into_iter()
                    .enumerate()
                    .map(|(n, text)| EventKind::AssistantText {
                        text,
                        final_: n == last,
                    })
                    .collect()
            }
            Some("tool_use") => vec![EventKind::tool_call(
                block["name"].as_str().unwrap_or("unknown"),
                &input_summary(&block["input"]),
            )],
            _ => Vec::new(),
        })
        .map(ProviderEvent::Event)
        .collect()
}

fn tool_results(message: &Value) -> Vec<ProviderEvent> {
    content_blocks(message)
        .iter()
        .filter(|block| block["type"] == "tool_result")
        .map(|block| {
            let summary = match &block["content"] {
                Value::String(text) => text.clone(),
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            };
            let is_error = block["is_error"].as_bool().unwrap_or(false);
            ProviderEvent::Event(EventKind::tool_result(&summary, is_error))
        })
        .collect()
}

fn turn_result(shared: &Shared, message: &Value) -> Vec<ProviderEvent> {
    let rate_limited = {
        let mut state = shared.state();
        state.turns_in_flight = state.turns_in_flight.saturating_sub(1);
        state.turns_started = state.turns_started.saturating_sub(1);
        // Requests of the finished turn can no longer be answered.
        state.permissions.clear();
        if state.swallow_results > 0 {
            state.swallow_results -= 1;
            state.rate_limit_emitted = false;
            debug!("dropped result of interrupted claude turn");
            return Vec::new();
        }
        let rate_limited = if message["api_error_status"].as_u64() == Some(429) {
            state.rate_limited("http_429", None)
        } else {
            None
        };
        state.rate_limit_emitted = false;
        rate_limited
    };
    let mut events = Vec::with_capacity(3);
    events.extend(rate_limited);
    if message["is_error"].as_bool().unwrap_or(false) {
        let text = message["result"].as_str().unwrap_or("").trim();
        let error = if text.is_empty() {
            format!(
                "provider turn failed ({}, {})",
                message["subtype"].as_str().unwrap_or("unknown"),
                message["terminal_reason"].as_str().unwrap_or("unknown")
            )
        } else {
            truncate_summary(text)
        };
        events.push(ProviderEvent::Event(EventKind::Error { message: error }));
    }
    events.push(ProviderEvent::TurnCompleted {
        usage: message.get("usage").cloned(),
    });
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use dgc_protocol::MAX_MESSAGE_BYTES;

    #[test]
    fn a_huge_assistant_text_is_split_into_bounded_events() {
        let text = "z".repeat(2 * 1024 * 1024);
        let message = serde_json::json!({
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": text}]}
        });
        let events = assistant_events(&message);
        assert!(events.len() >= 8, "{} events", events.len());
        let mut joined = String::new();
        for (n, event) in events.iter().enumerate() {
            let ProviderEvent::Event(kind @ EventKind::AssistantText { text, final_ }) = event
            else {
                panic!("unexpected event {event:?}");
            };
            let size = serde_json::to_vec(kind).expect("serialize").len();
            assert!(size < MAX_MESSAGE_BYTES / 2, "{size} bytes");
            assert_eq!(
                *final_,
                n + 1 == events.len(),
                "only the last chunk is final"
            );
            joined.push_str(text);
        }
        assert_eq!(joined, text);
    }
}
