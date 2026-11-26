//! CodexAdapter: drives `codex app-server` over stdio JSON-RPC.
//!
//! The wire format is the one recorded from codex-cli 0.172.0
//! (`tests/fixtures/README.md`, "Wire notes: Codex app-server"). One process
//! serves one thread for many turns; stdin closes it. Messages carry no
//! `jsonrpc` field.
//!
//! - Modes (`--mode`): `read-only` → sandbox `read-only`, approval policy
//!   `on-request`; `workspace-write` (the default) → `workspace-write` /
//!   `on-request`; `untrusted` → `workspace-write` / `untrusted`;
//!   `full-access` → `danger-full-access` / `never` (auto-approve). Under
//!   `workspace-write` + `on-request`, commands inside the workspace run
//!   sandboxed without asking (Codex's own default); `untrusted` is the mode
//!   that asks before commands.
//! - `start` spawns `codex app-server`, sends `initialize` and `initialized`,
//!   then `thread/start {cwd, approvalPolicy, sandbox, model?}` or, to resume,
//!   `thread/resume {threadId, model?, excludeTurns: true, approvalPolicy,
//!   sandbox}` in a new process (`excludeTurns` avoids the full-history
//!   `deprecationNotice` and the replay of old turns; dgc does not read them).
//!   The policy and sandbox of the agent's mode (the default
//!   `workspace-write` / `on-request` when none is given) are always sent on
//!   resume, so the thread never keeps a stored policy the agent does not
//!   show (in the 0.172.0 schema, not recorded). The thread id is reported as
//!   `SessionStarted`. If the server
//!   exits during startup the error carries its stderr tail.
//! - Requests: dgc's ids increase from 1. Every request waits for its
//!   response for at most [`RPC_TIMEOUT`]. Incoming lines are classified by
//!   shape, since server requests use their own ids from 0: `method` + `id` →
//!   server request, `method` alone → notification, `result`/`error` without
//!   `method` → response to dgc.
//! - Turns are tracked by id: the current turn comes from `turn/start`'s
//!   result (or `turn/started`) and ends with its `turn/completed`.
//!   - `send_user` → `turn/start {threadId, input}`.
//!   - `steer` → `turn/steer {threadId, expectedTurnId, input}` when a turn is
//!     running, else `Ok(false)` without sending. A steer rejected because
//!     its turn ended also returns `Ok(false)`: either the turn's
//!     `turn/completed` already arrived, or the error is -32600 ("no active
//!     turn to steer", which can precede `turn/completed`). The manager then
//!     interrupts, which retires the turn (see below).
//!   - `interrupt` → nothing without a running turn. Otherwise approvals of
//!     that turn still pending are answered with the Deny mapping (`decline`
//!     where offered, else `cancel`; a permission-profile request grants
//!     nothing; the manager counts them cancelled),
//!     then `turn/interrupt {threadId, turnId}`. Once it is answered (`{}`, or
//!     error -32600 "no active turn" when the turn ended first), that turn's
//!     `turn/completed`, notifications and approvals are dropped (approvals
//!     answered as above) — by turn id, so other turns are unaffected.
//! - Event mapping (server → [`ProviderEvent`]):
//!   - `item/agentMessage/delta` text is aggregated per `itemId` and emitted
//!     on `item/completed` as `AssistantText { final_: true }` (the item's
//!     own `text` unless the buffer was split). A message that never
//!     completes (a steered-away or interrupted one) is emitted with
//!     `final_: false` when the next message starts, its turn is interrupted
//!     or its turn ends. A buffer reaching [`MAX_MESSAGE_BUFFER_BYTES`] is
//!     emitted as a non-final chunk, and any text longer than
//!     [`MAX_EVENT_TEXT_BYTES`] is split into several events (all but the
//!     last non-final).
//!   - `commandExecution` / `fileChange` items → `tool_call` on
//!     `item/started` (command line / `<kind> <path>` list) and `tool_result`
//!     on `item/completed` (output or status; an error unless `completed`
//!     with exit 0).
//!   - `turn/completed` → `TurnCompleted { usage }` with the last
//!     `thread/tokenUsage/updated` of that turn; `status: failed` is preceded
//!     by an `Error` with the turn's `error` notification message (bounded).
//!     The `items` of `turn/completed` are not used (often `notLoaded`).
//!   - `item/commandExecution/requestApproval` → `PermissionRequested`
//!     (Command; summary = the command), `item/fileChange/requestApproval` →
//!     FileChange (summary = the changes of the item's `item/started`; the
//!     request has no diff), `item/permissions/requestApproval` → Other
//!     (summary = `permissions: <network access / read|write paths>
//!     (reason: ...)`; schema-derived, not recorded). `provider_request_id` is
//!     the server's request id as JSON text (e.g. `"0"`). Other server
//!     requests (e.g. `mcpServer/elicitation/request`) get the JSON-RPC error
//!     -32601 and an `Error` event naming the method.
//!   - An `error` with `usageLimitExceeded` or `rateLimitExceeded`, or
//!     `account/rateLimits/updated` with a reached type, → `RateLimited`
//!     once per turn. Routine account updates are dropped.
//!   - An oversized stdout line → `Error` and the session continues.
//!   - Anything else is ignored with a debug log of its method.
//! - Decisions use the request's `availableDecisions` when present: Allow →
//!   `accept`; AllowForSession → `acceptForSession` if offered, else
//!   `accept`; Deny → `decline` if offered, else `cancel`. `cancel` also
//!   interrupts the turn on Codex's side (its `turn/completed` then arrives
//!   with status `interrupted` and is delivered). File-change requests carry
//!   no list, so Deny is `decline` there. Permission-profile requests answer
//!   `{permissions, scope}`: Allow grants the requested profile with scope
//!   `turn`, AllowForSession with scope `session`, Deny grants `{}`.
//! - The session owns the process group: dropping it kills the group.
//!   `shutdown` closes stdin, waits briefly and then terminates. Stdout EOF
//!   plus the exit status yields one `Exited`, unless `shutdown` caused it.
//! - Logs carry methods, ids and states only, never prompt or output text.
//!
//! The reader task applies stdout lines in order and blocks while the event
//! channel ([`EVENT_CHANNEL_CAPACITY`]) is full; aggregating deltas keeps the
//! number of events per turn small.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard, PoisonError};
use std::time::Duration;

use dgc_protocol::event::EventKind;
use dgc_protocol::model::{Decision, PermissionKind, ProviderKind};
use dgc_protocol::{MAX_EVENT_TEXT_BYTES, chunk_text, truncate_summary};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::process::ChildStdin;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};
use tracing::debug;

use super::{AgentSpec, BoxFut, ProviderAdapter, ProviderError, ProviderEvent, ProviderSession};
use crate::process::{ChildIo, ChildProcess, ChildSpec, LineEvent, TailBuffer};

/// `--mode` values for Codex.
const MODES: [&str; 4] = ["read-only", "workspace-write", "untrusted", "full-access"];

/// Bound on waiting for the response to one of dgc's requests.
pub const RPC_TIMEOUT: Duration = Duration::from_secs(20);
/// Bound on writing one line to the server.
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
/// Aggregated message text past this size is emitted as a non-final chunk
/// (the same bound as one event's text).
pub const MAX_MESSAGE_BUFFER_BYTES: usize = MAX_EVENT_TEXT_BYTES;

/// Outbound message shapes, as recorded.
mod wire {
    use serde_json::{Value, json};

    pub(super) const APP_SERVER: &str = "app-server";
    /// JSON-RPC "invalid request"; the server's answer to steering or
    /// interrupting when no turn is active.
    pub(super) const INVALID_REQUEST: i64 = -32600;
    pub(super) const METHOD_NOT_FOUND: i64 = -32601;

    pub(super) fn request(id: u64, method: &str, params: Value) -> Value {
        json!({"method": method, "id": id, "params": params})
    }

    pub(super) fn notification(method: &str) -> Value {
        json!({"method": method})
    }

    pub(super) fn initialize_params() -> Value {
        json!({"clientInfo": {"name": "dgc", "title": "dgc", "version": env!("CARGO_PKG_VERSION")}})
    }

    pub(super) fn text_input(text: &str) -> Value {
        json!([{"type": "text", "text": text}])
    }

    /// Answer to an approval request.
    pub(super) fn decision(id: &Value, decision: &str) -> Value {
        json!({"id": id, "result": {"decision": decision}})
    }

    /// Answer to `item/permissions/requestApproval`: the granted profile and
    /// its scope (`turn` or `session`).
    pub(super) fn permissions_grant(id: &Value, permissions: &Value, scope: &str) -> Value {
        json!({"id": id, "result": {"permissions": permissions, "scope": scope}})
    }

    /// Answer to a server request dgc does not handle.
    pub(super) fn method_not_found(id: &Value) -> Value {
        json!({"id": id, "error": {"code": METHOD_NOT_FOUND, "message": "not supported by dgc"}})
    }
}

/// Approval policy and sandbox sent for a mode.
#[derive(Debug, Clone, Copy)]
struct Policy {
    approval: &'static str,
    sandbox: &'static str,
    auto_approve: bool,
}

fn policy(mode: Option<&str>) -> Result<Policy, String> {
    let (approval, sandbox, auto_approve) = match mode.unwrap_or("workspace-write") {
        "read-only" => ("on-request", "read-only", false),
        "workspace-write" => ("on-request", "workspace-write", false),
        "untrusted" => ("untrusted", "workspace-write", false),
        "full-access" => ("never", "danger-full-access", true),
        other => {
            return Err(format!(
                "unknown Codex mode {other:?}; expected one of {}",
                MODES.join(", ")
            ));
        }
    };
    Ok(Policy {
        approval,
        sandbox,
        auto_approve,
    })
}

pub struct CodexAdapter {
    program: PathBuf,
    prefix_args: Vec<String>,
}

impl CodexAdapter {
    /// Run `program` (normally `codex`, resolved on `PATH`).
    pub fn new(program: PathBuf) -> Self {
        Self::with_program(program, Vec::new())
    }

    /// Run `program` with `prefix_args` before `app-server` (tests pass the
    /// fake provider and its scenario).
    pub fn with_program(program: PathBuf, prefix_args: Vec<String>) -> Self {
        Self {
            program,
            prefix_args,
        }
    }
}

impl ProviderAdapter for CodexAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Codex
    }

    fn validate_mode(&self, mode: Option<&str>) -> Result<bool, String> {
        policy(mode).map(|policy| policy.auto_approve)
    }

    fn start(
        &self,
        spec: AgentSpec,
    ) -> BoxFut<'_, Result<Box<dyn ProviderSession>, ProviderError>> {
        Box::pin(async move {
            let policy = policy(spec.mode.as_deref()).map_err(ProviderError::Spawn)?;
            let mut args = self.prefix_args.clone();
            args.push(wire::APP_SERVER.to_owned());
            let child = ChildSpec {
                program: self.program.clone(),
                args,
                cwd: spec.cwd.clone(),
            };
            let (process, io) =
                ChildProcess::spawn(child).map_err(|e| ProviderError::Spawn(e.to_string()))?;
            debug!(agent_id = %spec.agent_id, "codex app-server started");
            // From here the session owns the process: an early return or a
            // cancelled future kills it.
            let mut session = CodexSession::new(process, io);
            if let Err(err) = session.open(&spec, policy).await {
                return Err(session.startup_failure(err).await);
            }
            Ok(Box::new(session) as Box<dyn ProviderSession>)
        })
    }
}

fn lock<T>(mutex: &StdMutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// dgc's requests, with what the reader must apply on their response.
#[derive(Debug, Clone)]
enum Rpc {
    Initialize,
    ThreadStart,
    ThreadResume,
    TurnStart,
    TurnSteer,
    TurnInterrupt { turn_id: String },
}

impl Rpc {
    fn method(&self) -> &'static str {
        match self {
            Self::Initialize => "initialize",
            Self::ThreadStart => "thread/start",
            Self::ThreadResume => "thread/resume",
            Self::TurnStart => "turn/start",
            Self::TurnSteer => "turn/steer",
            Self::TurnInterrupt { .. } => "turn/interrupt",
        }
    }
}

/// A JSON-RPC error response.
#[derive(Debug)]
struct RpcError {
    code: i64,
    message: String,
}

type RpcReply = Result<Value, RpcError>;

fn rpc_failed(method: &str, err: RpcError) -> ProviderError {
    ProviderError::Protocol(format!(
        "{method} request failed ({}): {}",
        err.code,
        truncate_summary(&err.message)
    ))
}

/// An approval request awaiting the user's decision.
struct Approval {
    /// The server's request id, echoed in the answer.
    server_id: Value,
    turn_id: String,
    request: ApprovalRequest,
}

/// What kind of answer the server expects.
enum ApprovalRequest {
    /// Command / file-change approval, answered with `{decision}`.
    Decision {
        /// String entries of `availableDecisions`, when the request has the list.
        offered: Option<Vec<String>>,
    },
    /// `item/permissions/requestApproval`, answered with the granted
    /// permission profile and its scope.
    Permissions {
        /// The request's `permissions` profile.
        requested: Value,
    },
}

impl Approval {
    fn offers(&self, decision: &str) -> bool {
        match &self.request {
            ApprovalRequest::Decision { offered } => offered
                .as_ref()
                .is_none_or(|offered| offered.iter().any(|d| d == decision)),
            ApprovalRequest::Permissions { .. } => false,
        }
    }

    fn decision(&self, d: Decision) -> &'static str {
        match d {
            Decision::Allow => "accept",
            Decision::AllowForSession if self.offers("acceptForSession") => "acceptForSession",
            Decision::AllowForSession => "accept",
            Decision::Deny if self.offers("decline") => "decline",
            Decision::Deny => "cancel",
        }
    }

    /// The JSON-RPC response that answers this request with `d`.
    /// Permissions: Allow grants the requested profile for the turn,
    /// AllowForSession for the session, Deny grants nothing.
    fn answer(&self, d: Decision) -> Value {
        match &self.request {
            ApprovalRequest::Decision { .. } => wire::decision(&self.server_id, self.decision(d)),
            ApprovalRequest::Permissions { requested } => match d {
                Decision::Allow => wire::permissions_grant(&self.server_id, requested, "turn"),
                Decision::AllowForSession => {
                    wire::permissions_grant(&self.server_id, requested, "session")
                }
                Decision::Deny => wire::permissions_grant(&self.server_id, &json!({}), "turn"),
            },
        }
    }
}

/// A started command or file-change item.
struct ToolItem {
    turn_id: String,
    summary: String,
}

/// Agent message text aggregated from deltas.
struct Message {
    item_id: String,
    turn_id: String,
    text: String,
    /// Part of the text was already emitted as a chunk.
    chunked: bool,
}

#[derive(Default)]
struct State {
    /// dgc's requests awaiting a response.
    rpcs: HashMap<u64, (Rpc, oneshot::Sender<RpcReply>)>,
    /// The running turn.
    current_turn: Option<String>,
    rate_limit_emitted: bool,
    /// The turn an `interrupt` call is stopping.
    interrupting: Option<String>,
    /// Interrupted turns whose `turn/completed` has not arrived; their
    /// notifications are dropped.
    suppressed: HashSet<String>,
    /// Unanswered approvals by `provider_request_id`.
    approvals: HashMap<String, Approval>,
    /// Started tool items by item id.
    items: HashMap<String, ToolItem>,
    /// Uncompleted agent messages, in start order.
    messages: Vec<Message>,
    /// Latest token usage: (turn id, tokenUsage).
    usage: Option<(String, Value)>,
    /// Latest final `error` notification: (turn id, bounded message).
    turn_error: Option<(String, String)>,
    /// Stdout reached EOF.
    exited: bool,
}

impl State {
    fn start_turn(&mut self, turn_id: &str) {
        // The RPC response and turn/started can both announce the same turn.
        if self.current_turn.as_deref() != Some(turn_id) {
            self.current_turn = Some(turn_id.to_owned());
            self.rate_limit_emitted = false;
        }
    }

    fn rate_limited(&mut self, reason: &str, resets_at: Option<i64>) -> Vec<ProviderEvent> {
        if self.rate_limit_emitted {
            return Vec::new();
        }
        self.rate_limit_emitted = true;
        let mut detail = json!({"provider": "codex", "reason": reason});
        if let Some(resets_at) = resets_at {
            detail["resets_at"] = json!(resets_at);
        }
        vec![ProviderEvent::Event(EventKind::RateLimited { detail })]
    }

    fn ignores_turn(&self, turn_id: &str) -> bool {
        self.suppressed.contains(turn_id) || self.interrupting.as_deref() == Some(turn_id)
    }

    /// Emit the uncompleted messages selected by `flush` as non-final text.
    fn flush_messages(&mut self, flush: impl Fn(&Message) -> bool) -> Vec<ProviderEvent> {
        let (flushed, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.messages)
            .into_iter()
            .partition(flush);
        self.messages = kept;
        flushed
            .into_iter()
            .flat_map(|message| text_events(&message.text, false))
            .collect()
    }

    /// Forget a turn's items and approvals; its messages are flushed.
    fn end_turn(&mut self, turn_id: &str) -> Vec<ProviderEvent> {
        self.items.retain(|_, item| item.turn_id != turn_id);
        self.approvals
            .retain(|_, approval| approval.turn_id != turn_id);
        if self.current_turn.as_deref() == Some(turn_id) {
            self.current_turn = None;
            self.rate_limit_emitted = false;
        }
        self.flush_messages(|message| message.turn_id == turn_id)
    }
}

/// `text` as `AssistantText` events of at most [`MAX_EVENT_TEXT_BYTES`]
/// each; all but the last are non-final, the last is `final_`. Empty text
/// gives no event.
fn text_events(text: &str, final_: bool) -> Vec<ProviderEvent> {
    if text.is_empty() {
        return Vec::new();
    }
    let chunks = chunk_text(text);
    let last = chunks.len().saturating_sub(1);
    chunks
        .into_iter()
        .enumerate()
        .map(|(n, text)| {
            ProviderEvent::Event(EventKind::AssistantText {
                text,
                final_: final_ && n == last,
            })
        })
        .collect()
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

struct CodexSession {
    shared: Arc<Shared>,
    process: Arc<Mutex<ChildProcess>>,
    events: mpsc::Receiver<ProviderEvent>,
    reader: Option<JoinHandle<()>>,
    shutting_down: Arc<AtomicBool>,
    next_id: u64,
    thread_id: String,
}

impl CodexSession {
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
            next_id: 0,
            thread_id: String::new(),
        }
    }

    /// Handshake, then start or resume the thread.
    async fn open(&mut self, spec: &AgentSpec, policy: Policy) -> Result<(), ProviderError> {
        self.call(Rpc::Initialize, wire::initialize_params())
            .await?;
        self.shared
            .write(&wire::notification("initialized"))
            .await?;
        let (rpc, mut params) = match &spec.resume_session_id {
            Some(thread_id) => {
                // The policy is always sent (the default mode's when none is
                // given), so a resumed thread never keeps a stored policy the
                // agent record does not show.
                let params = json!({
                    "threadId": thread_id,
                    "excludeTurns": true,
                    "approvalPolicy": policy.approval,
                    "sandbox": policy.sandbox,
                });
                (Rpc::ThreadResume, params)
            }
            None => {
                let cwd = spec.cwd.to_str().ok_or_else(|| {
                    ProviderError::Spawn("workspace path is not valid UTF-8".to_owned())
                })?;
                let params = json!({
                    "cwd": cwd,
                    "approvalPolicy": policy.approval,
                    "sandbox": policy.sandbox,
                });
                (Rpc::ThreadStart, params)
            }
        };
        if let Some(model) = &spec.model {
            params["model"] = json!(model);
        }
        let result = self.call(rpc, params).await?;
        self.thread_id = thread_id(&result)
            .ok_or_else(|| ProviderError::Protocol("thread response without thread id".to_owned()))?
            .to_owned();
        debug!(agent_id = %spec.agent_id, thread_id = %self.thread_id, "codex thread ready");
        Ok(())
    }

    /// Turn a failed startup into the start error: if the server exited,
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

    /// Send a request and wait (bounded) for its response.
    async fn request(&mut self, rpc: Rpc, params: Value) -> Result<RpcReply, ProviderError> {
        self.next_id += 1;
        let id = self.next_id;
        let method = rpc.method();
        let (tx, rx) = oneshot::channel();
        {
            let mut state = self.shared.state();
            if state.exited {
                return Err(ProviderError::Exited);
            }
            state.rpcs.insert(id, (rpc, tx));
        }
        if let Err(err) = self.shared.write(&wire::request(id, method, params)).await {
            self.shared.state().rpcs.remove(&id);
            return Err(err);
        }
        match timeout(RPC_TIMEOUT, rx).await {
            Ok(Ok(reply)) => Ok(reply),
            // The reader dropped the waiter: stdout ended.
            Ok(Err(_)) => Err(ProviderError::Exited),
            Err(_) => {
                self.shared.state().rpcs.remove(&id);
                Err(ProviderError::TimedOut(method))
            }
        }
    }

    /// A request whose error response is a failure.
    async fn call(&mut self, rpc: Rpc, params: Value) -> Result<Value, ProviderError> {
        let method = rpc.method();
        self.request(rpc, params)
            .await?
            .map_err(|err| rpc_failed(method, err))
    }

    fn current_turn(&self) -> Option<String> {
        self.shared.state().current_turn.clone()
    }
}

fn thread_id(result: &Value) -> Option<&str> {
    result["thread"]["id"].as_str()
}

impl ProviderSession for CodexSession {
    fn send_user<'a>(&'a mut self, text: &'a str) -> BoxFut<'a, Result<(), ProviderError>> {
        Box::pin(async move {
            let params = json!({"threadId": self.thread_id, "input": wire::text_input(text)});
            self.call(Rpc::TurnStart, params).await.map(drop)
        })
    }

    fn steer<'a>(&'a mut self, text: &'a str) -> BoxFut<'a, Result<bool, ProviderError>> {
        Box::pin(async move {
            let Some(turn_id) = self.current_turn() else {
                return Ok(false);
            };
            let params = json!({
                "threadId": self.thread_id,
                "expectedTurnId": turn_id,
                "input": wire::text_input(text),
            });
            match self.request(Rpc::TurnSteer, params).await? {
                Ok(_) => Ok(true),
                // The turn ended before the steer arrived: nothing was added.
                // Codex may answer -32600 "no active turn to steer" before
                // that turn's turn/completed reaches us, so the code counts
                // as "ended" even while the turn still looks current; the
                // manager's fallback interrupt then retires it.
                Err(err)
                    if err.code == wire::INVALID_REQUEST
                        || self.current_turn().as_deref() != Some(turn_id.as_str()) =>
                {
                    debug!(code = err.code, "codex steer raced the end of its turn");
                    Ok(false)
                }
                Err(err) => Err(rpc_failed("turn/steer", err)),
            }
        })
    }

    fn interrupt(&mut self) -> BoxFut<'_, Result<(), ProviderError>> {
        Box::pin(async move {
            let (turn_id, outstanding) = {
                let mut state = self.shared.state();
                let Some(turn_id) = state.current_turn.clone() else {
                    // The turn already ended; its TurnCompleted is buffered.
                    debug!("codex interrupt skipped: no turn running");
                    return Ok(());
                };
                state.interrupting = Some(turn_id.clone());
                let ids: Vec<String> = state
                    .approvals
                    .iter()
                    .filter(|(_, approval)| approval.turn_id == turn_id)
                    .map(|(id, _)| id.clone())
                    .collect();
                let outstanding: Vec<Approval> = ids
                    .iter()
                    .filter_map(|id| state.approvals.remove(id))
                    .collect();
                (turn_id, outstanding)
            };
            let result = async {
                // Outstanding requests are cancelled; answer them so the
                // server does not wait on them.
                for approval in &outstanding {
                    self.shared.write(&approval.answer(Decision::Deny)).await?;
                }
                let params = json!({"threadId": self.thread_id, "turnId": turn_id});
                let rpc = Rpc::TurnInterrupt {
                    turn_id: turn_id.clone(),
                };
                match self.request(rpc, params).await? {
                    Ok(_) => Ok(()),
                    // "no active turn": it ended first; the reader dropped
                    // whatever of it remains.
                    Err(err) if err.code == wire::INVALID_REQUEST => {
                        debug!(code = err.code, "codex turn ended before the interrupt");
                        Ok(())
                    }
                    Err(err) => Err(rpc_failed("turn/interrupt", err)),
                }
            }
            .await;
            self.shared.state().interrupting = None;
            result
        })
    }

    fn respond_permission<'a>(
        &'a mut self,
        provider_request_id: &'a str,
        d: Decision,
    ) -> BoxFut<'a, Result<(), ProviderError>> {
        Box::pin(async move {
            let approval = self
                .shared
                .state()
                .approvals
                .remove(provider_request_id)
                .ok_or_else(|| ProviderError::Protocol("unknown permission request".to_owned()))?;
            self.shared.write(&approval.answer(d)).await
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
            // Closing stdin asks the server to exit.
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

impl Drop for CodexSession {
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

/// What one stdout line produced: events, and possibly a response to hand
/// to its waiting request (after the events are queued).
#[derive(Default)]
struct Handled {
    events: Vec<ProviderEvent>,
    reply: Option<(oneshot::Sender<RpcReply>, RpcReply)>,
}

impl From<Vec<ProviderEvent>> for Handled {
    fn from(events: Vec<ProviderEvent>) -> Self {
        Self {
            events,
            reply: None,
        }
    }
}

/// Apply stdout lines in order; at EOF report the exit once.
async fn read_loop(mut reader: Reader) {
    while let Some(line) = reader.lines.recv().await {
        let handled = match line {
            LineEvent::Line(text) => handle_line(&reader.shared, &text).await,
            LineEvent::Oversized { bytes } => {
                debug!(bytes, "codex stdout line exceeded the limit");
                vec![ProviderEvent::Event(EventKind::Error {
                    message: "provider line exceeded 16 MiB".to_owned(),
                })]
                .into()
            }
            LineEvent::Eof => break,
        };
        for event in handled.events {
            if reader.events.send(event).await.is_err() {
                return; // The session is gone.
            }
        }
        if let Some((waiter, reply)) = handled.reply {
            let _ = waiter.send(reply);
        }
    }
    {
        let mut state = reader.shared.state();
        state.exited = true;
        // Dropping the waiters fails pending requests.
        state.rpcs.clear();
        state.approvals.clear();
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
    debug!(success, "codex process exited");
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
            debug!(%error, "waiting for codex failed");
            false
        }
        Err(_) => {
            if let Err(error) = process.terminate(EXIT_TERMINATE_GRACE).await {
                debug!(%error, "terminating codex failed");
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

async fn handle_line(shared: &Shared, text: &str) -> Handled {
    let Ok(message) = serde_json::from_str::<Value>(text) else {
        debug!("ignoring non-JSON codex line");
        return Handled::default();
    };
    let method = message["method"].as_str();
    let id = message.get("id").filter(|id| !id.is_null());
    match (method, id) {
        (Some(method), Some(id)) => server_request(shared, method, id, &message["params"])
            .await
            .into(),
        (Some(method), None) => {
            notification(&mut shared.state(), method, &message["params"]).into()
        }
        (None, Some(_)) if message.get("result").is_some() || message.get("error").is_some() => {
            response(&mut shared.state(), &message)
        }
        _ => {
            debug!("ignoring unclassified codex message");
            Handled::default()
        }
    }
}

/// A response to one of dgc's requests; applies what the request implies
/// before its waiter sees it.
fn response(state: &mut State, message: &Value) -> Handled {
    let Some(id) = message["id"].as_u64() else {
        debug!("codex response with a non-numeric id");
        return Handled::default();
    };
    let Some((rpc, waiter)) = state.rpcs.remove(&id) else {
        debug!(id, "codex response without a waiter");
        return Handled::default();
    };
    let reply = match message.get("error") {
        Some(error) => Err(RpcError {
            code: error["code"].as_i64().unwrap_or(0),
            message: error["message"].as_str().unwrap_or("no detail").to_owned(),
        }),
        None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
    };
    let mut events = Vec::new();
    match (&rpc, &reply) {
        (Rpc::ThreadStart | Rpc::ThreadResume, Ok(result)) => {
            if let Some(thread_id) = thread_id(result) {
                events.push(ProviderEvent::SessionStarted {
                    session_id: thread_id.to_owned(),
                });
            }
        }
        (Rpc::TurnStart, Ok(result)) => {
            if let Some(turn_id) = result["turn"]["id"].as_str() {
                state.start_turn(turn_id);
            }
        }
        (Rpc::TurnInterrupt { turn_id }, reply)
            if reply
                .as_ref()
                .err()
                .is_none_or(|err| err.code == wire::INVALID_REQUEST) =>
        {
            // From here on the turn is over for the manager. If its
            // turn/completed has not come yet, it will be dropped.
            if state.current_turn.as_deref() == Some(turn_id.as_str()) {
                state.suppressed.insert(turn_id.clone());
            }
            events = state.end_turn(turn_id);
            debug!(turn_id = %turn_id, "codex turn interrupted");
        }
        _ => {}
    }
    Handled {
        events,
        reply: Some((waiter, reply)),
    }
}

fn notification(state: &mut State, method: &str, params: &Value) -> Vec<ProviderEvent> {
    if let Some(turn_id) = params["turnId"].as_str()
        && state.suppressed.contains(turn_id)
    {
        debug!(method, "dropped notification of interrupted codex turn");
        return Vec::new();
    }
    match method {
        "turn/started" => {
            if let Some(turn_id) = params["turn"]["id"].as_str()
                && !state.ignores_turn(turn_id)
            {
                state.start_turn(turn_id);
            }
            Vec::new()
        }
        "item/started" => item_started(state, params),
        "item/agentMessage/delta" => message_delta(state, params),
        "item/completed" => item_completed(state, params),
        "thread/tokenUsage/updated" => {
            if let Some(turn_id) = params["turnId"].as_str() {
                state.usage = Some((turn_id.to_owned(), params["tokenUsage"].clone()));
            }
            Vec::new()
        }
        "error" => {
            if params["willRetry"].as_bool() == Some(true) {
                debug!("codex reported a retried error");
            } else if let Some(turn_id) = params["turnId"].as_str() {
                let message = params["error"]["message"].as_str().unwrap_or("");
                state.turn_error = Some((turn_id.to_owned(), truncate_summary(message)));
            }
            match params["error"]["codexErrorInfo"].as_str() {
                Some(reason @ ("usageLimitExceeded" | "rateLimitExceeded")) => {
                    state.rate_limited(reason, None)
                }
                _ => Vec::new(),
            }
        }
        "turn/completed" => turn_completed(state, params),
        "account/rateLimits/updated" => {
            let snapshot = &params["rateLimits"];
            let Some(reason) = snapshot["rateLimitReachedType"].as_str() else {
                debug!("dropped routine codex rate limit update");
                return Vec::new();
            };
            let resets_at = ["primary", "secondary"]
                .into_iter()
                .filter_map(|window| snapshot[window]["resetsAt"].as_i64())
                .min();
            state.rate_limited(reason, resets_at)
        }
        other => {
            debug!(method = other, "ignoring codex notification");
            Vec::new()
        }
    }
}

fn item_started(state: &mut State, params: &Value) -> Vec<ProviderEvent> {
    let item = &params["item"];
    let (Some(item_id), Some(turn_id)) = (item["id"].as_str(), params["turnId"].as_str()) else {
        return Vec::new();
    };
    match item["type"].as_str() {
        Some("agentMessage") => {
            // An earlier message that never completed (steered away) ends
            // here, so its text does not run into this one.
            let events = state.flush_messages(|message| message.item_id != item_id);
            state.messages.push(Message {
                item_id: item_id.to_owned(),
                turn_id: turn_id.to_owned(),
                text: String::new(),
                chunked: false,
            });
            events
        }
        Some(kind @ ("commandExecution" | "fileChange")) => {
            let summary = if kind == "commandExecution" {
                item["command"].as_str().unwrap_or("").to_owned()
            } else {
                changes_summary(&item["changes"])
            };
            let event = EventKind::tool_call(kind, &summary);
            state.items.insert(
                item_id.to_owned(),
                ToolItem {
                    turn_id: turn_id.to_owned(),
                    summary,
                },
            );
            vec![ProviderEvent::Event(event)]
        }
        _ => Vec::new(),
    }
}

/// `<kind> <path>` for each change of a fileChange item.
fn changes_summary(changes: &Value) -> String {
    changes
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .map(|change| {
            format!(
                "{} {}",
                change["kind"]["type"].as_str().unwrap_or("change"),
                change["path"].as_str().unwrap_or("?")
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn message_delta(state: &mut State, params: &Value) -> Vec<ProviderEvent> {
    let (Some(item_id), Some(delta)) = (params["itemId"].as_str(), params["delta"].as_str()) else {
        return Vec::new();
    };
    let index = match state.messages.iter().position(|m| m.item_id == item_id) {
        Some(index) => index,
        None => {
            state.messages.push(Message {
                item_id: item_id.to_owned(),
                turn_id: params["turnId"].as_str().unwrap_or("").to_owned(),
                text: String::new(),
                chunked: false,
            });
            state.messages.len() - 1
        }
    };
    let message = &mut state.messages[index];
    message.text.push_str(delta);
    if message.text.len() < MAX_MESSAGE_BUFFER_BYTES {
        return Vec::new();
    }
    message.chunked = true;
    text_events(&std::mem::take(&mut message.text), false)
}

fn item_completed(state: &mut State, params: &Value) -> Vec<ProviderEvent> {
    let item = &params["item"];
    let Some(item_id) = item["id"].as_str() else {
        return Vec::new();
    };
    match item["type"].as_str() {
        Some("agentMessage") => {
            let buffered = state
                .messages
                .iter()
                .position(|m| m.item_id == item_id)
                .map(|index| state.messages.remove(index));
            let text = match (buffered, item["text"].as_str()) {
                (Some(message), _) if message.chunked => message.text,
                (_, Some(text)) => text.to_owned(),
                (Some(message), None) => message.text,
                (None, None) => String::new(),
            };
            text_events(&text, true)
        }
        Some("commandExecution") => {
            state.items.remove(item_id);
            let status = item["status"].as_str().unwrap_or("unknown");
            let exit_code = item["exitCode"].as_i64();
            let summary = match (item["aggregatedOutput"].as_str(), exit_code) {
                (Some(output), _) if !output.is_empty() => output.to_owned(),
                (_, Some(code)) => format!("{status} (exit {code})"),
                _ => status.to_owned(),
            };
            let is_error = status != "completed" || exit_code.is_some_and(|code| code != 0);
            vec![ProviderEvent::Event(EventKind::tool_result(
                &summary, is_error,
            ))]
        }
        Some("fileChange") => {
            state.items.remove(item_id);
            let status = item["status"].as_str().unwrap_or("unknown");
            vec![ProviderEvent::Event(EventKind::tool_result(
                status,
                status != "completed",
            ))]
        }
        _ => Vec::new(),
    }
}

fn turn_completed(state: &mut State, params: &Value) -> Vec<ProviderEvent> {
    let turn = &params["turn"];
    let Some(turn_id) = turn["id"].as_str() else {
        debug!("codex turn/completed without a turn id");
        return Vec::new();
    };
    let ignored = state.ignores_turn(turn_id);
    state.suppressed.remove(turn_id);
    let mut events = state.end_turn(turn_id);
    let usage = match state.usage.take() {
        Some((id, usage)) if id == turn_id => Some(usage),
        other => {
            state.usage = other;
            None
        }
    };
    let error = match state.turn_error.take() {
        Some((id, message)) if id == turn_id => Some(message),
        other => {
            state.turn_error = other;
            None
        }
    };
    if ignored {
        debug!(turn_id, "dropped turn/completed of interrupted codex turn");
        return events;
    }
    if turn["status"] == "failed" {
        let message = error
            .or_else(|| turn["error"]["message"].as_str().map(truncate_summary))
            .unwrap_or_else(|| "provider turn failed".to_owned());
        events.push(ProviderEvent::Event(EventKind::Error { message }));
    }
    events.push(ProviderEvent::TurnCompleted { usage });
    events
}

/// A request from the server: approvals become `PermissionRequested`, the
/// rest are refused.
async fn server_request(
    shared: &Shared,
    method: &str,
    id: &Value,
    params: &Value,
) -> Vec<ProviderEvent> {
    let kind = match method {
        "item/commandExecution/requestApproval" => PermissionKind::Command,
        "item/fileChange/requestApproval" => PermissionKind::FileChange,
        "item/permissions/requestApproval" => PermissionKind::Other,
        _ => {
            debug!(method, "refusing unsupported codex server request");
            if let Err(error) = shared.write(&wire::method_not_found(id)).await {
                debug!(%error, "could not answer codex server request");
            }
            return vec![ProviderEvent::Event(EventKind::Error {
                message: truncate_summary(&format!("unsupported provider request {method}")),
            })];
        }
    };
    let provider_request_id = id.to_string();
    let request = match kind {
        PermissionKind::Other => ApprovalRequest::Permissions {
            requested: params["permissions"].clone(),
        },
        _ => ApprovalRequest::Decision {
            offered: params["availableDecisions"].as_array().map(|decisions| {
                decisions
                    .iter()
                    .filter_map(|d| d.as_str().map(str::to_owned))
                    .collect()
            }),
        },
    };
    let approval = Approval {
        server_id: id.clone(),
        turn_id: params["turnId"].as_str().unwrap_or("").to_owned(),
        request,
    };
    {
        let mut state = shared.state();
        if !state.ignores_turn(&approval.turn_id) {
            let item = params["itemId"]
                .as_str()
                .and_then(|item_id| state.items.get(item_id));
            let summary = match kind {
                PermissionKind::Command => params["command"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| item.map(|item| item.summary.clone()))
                    .unwrap_or_else(|| "command".to_owned()),
                PermissionKind::Other => permissions_summary(params),
                _ => item
                    .map(|item| item.summary.clone())
                    .filter(|summary| !summary.is_empty())
                    .unwrap_or_else(|| "file change".to_owned()),
            };
            state
                .approvals
                .insert(provider_request_id.clone(), approval);
            return vec![ProviderEvent::PermissionRequested {
                provider_request_id,
                kind,
                summary: truncate_summary(&summary),
                raw: params.clone(),
            }];
        }
    }
    // The turn is being interrupted: the request is cancelled.
    debug!(request_id = %provider_request_id, "cancelling approval request of interrupted codex turn");
    if let Err(error) = shared.write(&approval.answer(Decision::Deny)).await {
        debug!(%error, "could not cancel codex approval request");
    }
    Vec::new()
}

/// `permissions: <what>[, <what>...] (reason: <reason>)` for an
/// `item/permissions/requestApproval`: `network access`, `read|write <path>`
/// for the legacy lists and `<access> <path>` for `entries` (a glob pattern,
/// or a special path's kind and subpath). The caller bounds the length.
fn permissions_summary(params: &Value) -> String {
    let profile = &params["permissions"];
    let mut parts: Vec<String> = Vec::new();
    if profile["network"]["enabled"].as_bool() == Some(true) {
        parts.push("network access".to_owned());
    }
    let file_system = &profile["fileSystem"];
    for access in ["read", "write"] {
        for path in file_system[access].as_array().into_iter().flatten() {
            if let Some(path) = path.as_str() {
                parts.push(format!("{access} {path}"));
            }
        }
    }
    for entry in file_system["entries"].as_array().into_iter().flatten() {
        let access = entry["access"].as_str().unwrap_or("?");
        let path = &entry["path"];
        let target = match path["type"].as_str() {
            Some("path") => path["path"].as_str().unwrap_or("?").to_owned(),
            Some("glob_pattern") => path["pattern"].as_str().unwrap_or("?").to_owned(),
            Some("special") => {
                let value = &path["value"];
                let kind = value["kind"].as_str().unwrap_or("?");
                let named = value["path"].as_str().unwrap_or(kind);
                match value["subpath"].as_str() {
                    Some(sub) => format!("{named}/{sub}"),
                    None => named.to_owned(),
                }
            }
            _ => path.to_string(),
        };
        parts.push(format!("{access} {target}"));
    }
    if parts.is_empty() {
        parts.push("(none listed)".to_owned());
    }
    let mut summary = format!("permissions: {}", parts.join(", "));
    if let Some(reason) = params["reason"].as_str().filter(|r| !r.is_empty()) {
        summary.push_str(&format!(" (reason: {reason})"));
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use dgc_protocol::MAX_MESSAGE_BYTES;

    /// Assistant texts of `events`, checking every event's size bound.
    fn texts(events: &[ProviderEvent]) -> Vec<(String, bool)> {
        events
            .iter()
            .map(|event| match event {
                ProviderEvent::Event(kind @ EventKind::AssistantText { text, final_ }) => {
                    let size = serde_json::to_vec(kind).expect("serialize").len();
                    assert!(size < MAX_MESSAGE_BYTES / 2, "{size} bytes");
                    (text.clone(), *final_)
                }
                other => panic!("unexpected event {other:?}"),
            })
            .collect()
    }

    #[test]
    fn permission_grant_answers_follow_the_schema() {
        let requested = serde_json::json!({"network": {"enabled": true}});
        let approval = Approval {
            server_id: serde_json::json!(7),
            turn_id: "t".to_owned(),
            request: ApprovalRequest::Permissions {
                requested: requested.clone(),
            },
        };
        assert_eq!(
            approval.answer(Decision::Allow),
            serde_json::json!({"id": 7, "result": {"permissions": requested, "scope": "turn"}})
        );
        assert_eq!(
            approval.answer(Decision::AllowForSession),
            serde_json::json!({"id": 7, "result": {"permissions": requested, "scope": "session"}})
        );
        assert_eq!(
            approval.answer(Decision::Deny),
            serde_json::json!({"id": 7, "result": {"permissions": {}, "scope": "turn"}})
        );
    }

    #[test]
    fn command_decisions_use_the_offered_list() {
        let approval = Approval {
            server_id: serde_json::json!(0),
            turn_id: "t".to_owned(),
            request: ApprovalRequest::Decision {
                offered: Some(vec!["accept".to_owned(), "cancel".to_owned()]),
            },
        };
        let decision = |d| approval.answer(d)["result"]["decision"].clone();
        assert_eq!(decision(Decision::Allow), "accept");
        assert_eq!(decision(Decision::AllowForSession), "accept");
        assert_eq!(decision(Decision::Deny), "cancel");
    }

    #[test]
    fn a_huge_completed_message_is_split_into_bounded_events() {
        let text = "c".repeat(2 * 1024 * 1024);
        let mut state = State::default();
        let params = serde_json::json!({
            "item": {"id": "msg_1", "type": "agentMessage", "text": text}
        });
        let chunks = texts(&item_completed(&mut state, &params));
        assert!(chunks.len() >= 8, "{} chunks", chunks.len());
        let last = chunks.len() - 1;
        for (n, (_, final_)) in chunks.iter().enumerate() {
            assert_eq!(*final_, n == last, "only the last chunk is final");
        }
        let joined: String = chunks.into_iter().map(|(t, _)| t).collect();
        assert_eq!(joined, text);
    }

    #[test]
    fn streamed_deltas_are_emitted_in_bounded_chunks() {
        let mut state = State::default();
        let delta = "d".repeat(64 * 1024);
        let mut events = Vec::new();
        for _ in 0..32 {
            let params = serde_json::json!({
                "itemId": "msg_1", "turnId": "t1", "delta": delta
            });
            events.extend(message_delta(&mut state, &params));
        }
        let params = serde_json::json!({"item": {"id": "msg_1", "type": "agentMessage"}});
        events.extend(item_completed(&mut state, &params));
        let chunks = texts(&events);
        assert!(chunks.len() >= 8, "{} chunks", chunks.len());
        let joined: String = chunks.into_iter().map(|(t, _)| t).collect();
        assert_eq!(joined.len(), 32 * delta.len());
    }
}
