//! The daemon's private Unix-socket JSON-RPC server.
//!
//! This is the security boundary: the daemon listens only on a socket inside a
//! 0700 directory (socket mode 0600) and drops any peer whose effective UID
//! differs from its own. Nothing listens on the network.
//!
//! Wire format: one JSON message per line (`\n`), at most
//! [`MAX_MESSAGE_BYTES`] bytes. The first request on a connection must be a
//! valid `hello`.

use std::collections::HashMap;
use std::fs::{self, Permissions};
use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dgc_protocol::methods::{
    AGENT_ARCHIVE, AGENT_CREATE, AGENT_EVENTS, AGENT_GET, AGENT_HANDOFF, AGENT_INTERRUPT,
    AGENT_LIST, AGENT_RESUME, AGENT_SEND, DAEMON_SHUTDOWN, DAEMON_STATUS, DaemonStatus, HELLO,
    HelloParams, PERMISSION_LIST, PERMISSION_RESPOND, PROFILE_LIST, PROJECT_ADD, PROJECT_LIST,
    PROJECT_REMOVE, ProviderStatus, SCHEDULE_CREATE, SCHEDULE_GET, SCHEDULE_LIST, SCHEDULE_PAUSE,
    SCHEDULE_REMOVE, SCHEDULE_RESUME, SCHEDULE_RUN, SCRIPT_LIST, SCRIPT_LOGS, SCRIPT_START,
    SCRIPT_STOP, SCRIPT_TRUST, SUBSCRIBE, UNSUBSCRIBE, WORKSPACE_ARCHIVE, WORKSPACE_CREATE,
    WORKSPACE_LIST,
};
use dgc_protocol::model::ProviderKind;
use dgc_protocol::rpc::{
    INTERNAL, INVALID_REQUEST, INVALID_STATE, JSONRPC_VERSION, METHOD_NOT_FOUND, Notification,
    PARSE_ERROR, Request, Response, RpcError, VERSION_MISMATCH,
};
use dgc_protocol::{MAX_MESSAGE_BYTES, PROTOCOL_VERSION};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout};

use crate::agent::AgentManager;
use crate::clip_error_detail;
use crate::config::{ConfigError, DaemonConfig};
use crate::handlers;
use crate::lock::{InstanceLock, LockError};
use crate::paths::{DaemonPaths, PathsError, ensure_private_dir};
use crate::provider::ProviderAdapter;
use crate::provider::claude::ClaudeAdapter;
use crate::provider::codex::CodexAdapter;
use crate::provider::resolve::resolve_providers;
use crate::schedule::{ScheduleManager, localtime::SystemClock};
use crate::script::{STOP_GRACE, ScriptManager};
use crate::store::{Store, StoreError};
use crate::subscription::Subscriptions;
use crate::time::now_rfc3339;
use crate::workspace::WorkspaceManager;

/// Capacity of each connection's outbound queue (responses and notifications).
pub const OUTBOUND_CAPACITY: usize = 1024;

/// How long shutdown waits for in-flight responses to flush.
pub const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

/// How long daemon shutdown gives live agent sessions to end.
pub const AGENT_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Paths(#[from] PathsError),
    #[error(transparent)]
    Lock(#[from] LockError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("{} exists and is not a socket; refusing to replace it", .0.display())]
    SocketPathOccupied(PathBuf),
}

/// One queued outbound line. `written` fires once the line was handed to the
/// socket (or dropped if the writer failed).
struct Outbound {
    line: String,
    written: Option<oneshot::Sender<()>>,
}

enum Queued {
    Line(Outbound),
    /// Flush everything queued before this and close the write half.
    Close,
}

/// Per-connection handle given to method handlers.
#[derive(Debug, Clone)]
pub struct ConnCtx {
    id: u64,
    out: mpsc::Sender<Queued>,
    subscriptions: Arc<Subscriptions>,
}

impl std::fmt::Debug for Queued {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Line(_) => f.write_str("Line"),
            Self::Close => f.write_str("Close"),
        }
    }
}

impl ConnCtx {
    fn new(id: u64, capacity: usize) -> (Self, mpsc::Receiver<Queued>) {
        let (out, rx) = mpsc::channel(capacity);
        let subscriptions = Arc::new(Subscriptions::default());
        (
            Self {
                id,
                out,
                subscriptions,
            },
            rx,
        )
    }

    /// This connection's event subscriptions.
    pub fn subscriptions(&self) -> &Subscriptions {
        &self.subscriptions
    }

    /// Resolves once the connection's writer is gone.
    pub async fn closed(&self) {
        self.out.closed().await;
    }

    /// Daemon-local connection number, for logs.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Queue a notification without waiting. Returns false when the queue is
    /// full (the subscriber is lagging) or the connection is gone; the caller
    /// then drops the subscription and later sends `lagged`.
    pub fn notify(&self, notification: &Notification) -> bool {
        let Some(line) = encode(notification) else {
            return false;
        };
        self.out
            .try_send(Queued::Line(Outbound {
                line,
                written: None,
            }))
            .is_ok()
    }

    /// Queue a notification, waiting for room (backfill and the `lagged`
    /// notice). Returns false if the connection is gone.
    pub async fn notify_wait(&self, notification: &Notification) -> bool {
        match encode(notification) {
            Some(line) => self.send_line(line, false).await,
            None => false,
        }
    }

    /// Queue a line, waiting for room: responses must never be dropped.
    /// Returns false if the connection's writer is gone. With `wait_written`
    /// also waits (bounded) until the line was written to the socket.
    async fn send_line(&self, line: String, wait_written: bool) -> bool {
        let (written, ack) = if wait_written {
            let (tx, rx) = oneshot::channel();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        if self
            .out
            .send(Queued::Line(Outbound { line, written }))
            .await
            .is_err()
        {
            return false;
        }
        if let Some(ack) = ack {
            let _ = timeout(FLUSH_TIMEOUT, ack).await;
        }
        true
    }

    async fn send_response(&self, response: &Response, wait_written: bool) -> bool {
        match encode(response) {
            Some(line) => self.send_line(line, wait_written).await,
            None => false,
        }
    }
}

fn encode<T: Serialize>(message: &T) -> Option<String> {
    match serde_json::to_string(message) {
        Ok(line) => Some(line),
        Err(err) => {
            tracing::error!(%err, "failed to serialize an outbound message");
            None
        }
    }
}

/// Routes method calls to their handlers.
pub struct DaemonDispatcher {
    config: DaemonConfig,
    started_at: String,
    shutdown: watch::Sender<bool>,
    workspaces: Arc<WorkspaceManager>,
    agents: Arc<AgentManager>,
    schedules: Arc<ScheduleManager>,
    scripts: Arc<ScriptManager>,
    providers: Vec<ProviderStatus>,
}

impl DaemonDispatcher {
    pub fn config(&self) -> &DaemonConfig {
        &self.config
    }

    /// Ask the daemon to stop. Used after the `daemon.shutdown` response has
    /// been written.
    pub fn request_shutdown(&self) {
        self.shutdown.send_replace(true);
    }

    /// Handle one request after the handshake. Handlers return JSON objects,
    /// never `null`.
    pub async fn call(
        &self,
        conn: &ConnCtx,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        let workspaces = self.workspaces.as_ref();
        let agents = &self.agents;
        let schedules = &self.schedules;
        let scripts = &self.scripts;
        match method {
            DAEMON_STATUS => self.status(),
            DAEMON_SHUTDOWN => Ok(json!({})),
            PROJECT_ADD => handlers::project_add(workspaces, params).await,
            PROJECT_LIST => handlers::project_list(workspaces).await,
            PROJECT_REMOVE => handlers::project_remove(workspaces, params).await,
            WORKSPACE_CREATE => handlers::workspace_create(workspaces, params).await,
            WORKSPACE_LIST => handlers::workspace_list(workspaces, params).await,
            WORKSPACE_ARCHIVE => {
                let _gate = schedules.exclude_runs().await;
                handlers::workspace_archive(workspaces, agents, scripts, params).await
            }
            PROFILE_LIST => handlers::profile_list(&self.config),
            AGENT_CREATE => handlers::agent_create(agents, params).await,
            AGENT_HANDOFF => handlers::agent_handoff(agents, params).await,
            AGENT_LIST => handlers::agent_list(agents, params),
            AGENT_GET => handlers::agent_get(agents, params),
            AGENT_SEND => handlers::agent_send(agents, params).await,
            AGENT_INTERRUPT => handlers::agent_interrupt(agents, params).await,
            AGENT_ARCHIVE => handlers::agent_archive(agents, params).await,
            AGENT_RESUME => handlers::agent_resume(agents, params).await,
            AGENT_EVENTS => handlers::agent_events(agents, params),
            SCHEDULE_CREATE => handlers::schedule_create(schedules, params).await,
            SCHEDULE_LIST => handlers::schedule_list(schedules, params),
            SCHEDULE_GET => handlers::schedule_get(schedules, params),
            SCHEDULE_PAUSE => handlers::schedule_pause(schedules, params).await,
            SCHEDULE_RESUME => handlers::schedule_resume(schedules, params).await,
            SCHEDULE_RUN => {
                handlers::schedule_run(schedules, params, self.shutdown.subscribe()).await
            }
            SCHEDULE_REMOVE => handlers::schedule_remove(schedules, params).await,
            SCRIPT_LIST => handlers::script_list(scripts, params).await,
            SCRIPT_TRUST => handlers::script_trust(scripts, params).await,
            SCRIPT_START => handlers::script_start(scripts, params).await,
            SCRIPT_STOP => handlers::script_stop(scripts, params).await,
            SCRIPT_LOGS => handlers::script_logs(scripts, params).await,
            PERMISSION_LIST => handlers::permission_list(agents, params),
            PERMISSION_RESPOND => handlers::permission_respond(agents, params).await,
            SUBSCRIBE => handlers::subscribe(agents, conn, params),
            UNSUBSCRIBE => handlers::unsubscribe(conn, params),
            other => Err(RpcError::new(
                METHOD_NOT_FOUND,
                format!("unknown method: {}", clip_error_detail(other)),
            )),
        }
    }

    fn status(&self) -> Result<Value, RpcError> {
        let status = DaemonStatus {
            pid: std::process::id(),
            protocol_version: PROTOCOL_VERSION,
            started_at: self.started_at.clone(),
            providers: self.providers.clone(),
            path_env: std::env::var_os("PATH")
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        serde_json::to_value(status)
            .map_err(|err| RpcError::new(INTERNAL, format!("cannot encode status: {err}")))
    }
}

/// Run the daemon until `shutdown` becomes true (a signal handler or the
/// `daemon.shutdown` method sends it). Removes the socket file on exit.
pub async fn run_daemon(
    paths: DaemonPaths,
    config: DaemonConfig,
    shutdown: watch::Sender<bool>,
) -> Result<(), DaemonError> {
    config.validate()?;
    ensure_private_dir(&paths.state_dir)?;
    let _lock = InstanceLock::acquire(&paths.state_dir)?;
    let store = Arc::new(Store::open(&paths.state_dir)?);
    let workspaces = Arc::new(WorkspaceManager::new(
        Arc::clone(&store),
        paths.worktrees_dir.clone(),
    ));
    let scripts = Arc::new(ScriptManager::new(
        &paths.state_dir,
        Arc::clone(&workspaces),
    )?);
    let socket_dir = paths
        .socket
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket path has no parent"))?;
    ensure_private_dir(socket_dir)?;
    remove_stale_socket(&paths.socket)?;

    // Bind before the slow part of startup (provider probes, bounded by
    // `VERSION_TIMEOUT` each and run concurrently) so an autostarting client
    // sees the socket at once. Its connection waits in the listen backlog
    // until the accept loop starts below; the client's hello timeout covers
    // that wait (see `dgc_client`'s `HELLO_TIMEOUT`).
    let listener = UnixListener::bind(&paths.socket)?;
    if let Err(err) = fs::set_permissions(&paths.socket, Permissions::from_mode(0o600)) {
        let _ = fs::remove_file(&paths.socket);
        return Err(err.into());
    }

    let providers = resolve_providers(&config, std::env::var_os("PATH").as_deref()).await;
    let agents = Arc::new(AgentManager::new(
        Arc::clone(&store),
        paths.clone(),
        config.clone(),
        adapters(&providers),
    ));
    // No provider process survives a restart: agents left live are stopped
    // before any connection is accepted.
    if let Err(err) = agents.recover_on_startup() {
        tracing::warn!(error = %err, "could not mark every interrupted agent stopped");
    }
    let schedules = match ScheduleManager::new(
        store,
        Arc::clone(&agents),
        Arc::clone(&workspaces),
        Arc::new(SystemClock),
    ) {
        Ok(schedules) => Arc::new(schedules),
        Err(err) => {
            let _ = fs::remove_file(&paths.socket);
            return Err(err.into());
        }
    };
    let schedule_loop = Arc::clone(&schedules).spawn_loop(shutdown.subscribe());
    tracing::info!(pid = std::process::id(), "daemon listening");

    let dispatcher = Arc::new(DaemonDispatcher {
        config,
        started_at: now_rfc3339(),
        shutdown: shutdown.clone(),
        workspaces,
        agents: Arc::clone(&agents),
        schedules: Arc::clone(&schedules),
        scripts: Arc::clone(&scripts),
        providers,
    });
    accept_loop(listener, dispatcher, shutdown.subscribe()).await;
    if schedule_loop.await.is_err() {
        tracing::warn!(error_kind = "task", "scheduler stopped unexpectedly");
    }
    drop(schedules.exclude_runs().await);
    scripts.shutdown_all(STOP_GRACE).await;
    agents.shutdown_all(AGENT_SHUTDOWN_GRACE).await;

    match fs::remove_file(&paths.socket) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!(%err, "could not remove the socket file"),
    }
    tracing::info!("daemon stopped");
    Ok(())
}

/// An adapter for every provider that was found.
fn adapters(providers: &[ProviderStatus]) -> HashMap<ProviderKind, Arc<dyn ProviderAdapter>> {
    providers
        .iter()
        .filter_map(|status| {
            let path = status.path.clone()?;
            let adapter: Arc<dyn ProviderAdapter> = match status.kind {
                ProviderKind::Claude => Arc::new(ClaudeAdapter::new(path)),
                ProviderKind::Codex => Arc::new(CodexAdapter::new(path)),
            };
            Some((status.kind, adapter))
        })
        .collect()
}

/// Remove a leftover socket file from a previous run. Anything that is not a
/// socket is left alone and reported. Safe only while holding the instance
/// lock, which proves no other daemon owns the file.
fn remove_stale_socket(path: &Path) -> Result<(), DaemonError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            fs::remove_file(path)?;
            Ok(())
        }
        Ok(_) => Err(DaemonError::SocketPathOccupied(path.to_owned())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

async fn wait_shutdown(rx: &mut watch::Receiver<bool>) {
    // An error means every sender is gone; treat that as shutdown too.
    let _ = rx.wait_for(|stop| *stop).await;
}

async fn accept_loop(
    listener: UnixListener,
    dispatcher: Arc<DaemonDispatcher>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut connections = JoinSet::new();
    let mut next_id = 0u64;
    loop {
        tokio::select! {
            () = wait_shutdown(&mut shutdown) => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _addr)) => {
                    next_id += 1;
                    connections.spawn(serve_connection(
                        stream,
                        next_id,
                        Arc::clone(&dispatcher),
                        shutdown.clone(),
                    ));
                }
                Err(err) => {
                    tracing::warn!(%err, "accept failed");
                    sleep(Duration::from_millis(50)).await;
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }

    // Stop accepting, then give in-flight connections a bounded time to flush.
    drop(listener);
    let drain = async { while connections.join_next().await.is_some() {} };
    if timeout(FLUSH_TIMEOUT, drain).await.is_err() {
        tracing::warn!("connections did not finish in time; aborting them");
        connections.abort_all();
    }
}

async fn serve_connection(
    stream: UnixStream,
    id: u64,
    dispatcher: Arc<DaemonDispatcher>,
    mut shutdown: watch::Receiver<bool>,
) {
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    match stream.peer_cred() {
        Ok(cred) if cred.uid() == euid => {}
        Ok(cred) => {
            tracing::warn!(conn = id, peer_uid = cred.uid(), "rejected a foreign UID");
            return;
        }
        Err(err) => {
            tracing::warn!(conn = id, %err, "cannot read peer credentials; dropping");
            return;
        }
    }

    let (read_half, write_half) = stream.into_split();
    let (ctx, queue) = ConnCtx::new(id, OUTBOUND_CAPACITY);
    let writer = tokio::spawn(write_loop(write_half, queue));
    tracing::debug!(conn = id, "connection opened");
    session(read_half, &ctx, &dispatcher, &mut shutdown).await;
    ctx.subscriptions().clear();
    // Queued responses are flushed before the writer sees `Close`.
    let _ = ctx.out.send(Queued::Close).await;
    let _ = writer.await;
    tracing::debug!(conn = id, "connection closed");
}

async fn write_loop(mut socket: OwnedWriteHalf, mut queue: mpsc::Receiver<Queued>) {
    while let Some(item) = queue.recv().await {
        let Queued::Line(Outbound { mut line, written }) = item else {
            break;
        };
        line.push('\n');
        let result = socket.write_all(line.as_bytes()).await;
        if let Some(written) = written {
            let _ = written.send(());
        }
        if result.is_err() {
            break;
        }
    }
    let _ = socket.shutdown().await;
}

enum LineRead {
    Line,
    Eof,
    Oversized,
}

/// Read one `\n`-terminated line into `buf` (terminator excluded), never
/// buffering more than `max` bytes of it. A final line without a terminator is
/// delivered at EOF.
async fn read_bounded_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max: usize,
) -> io::Result<LineRead> {
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return Ok(if buf.is_empty() {
                LineRead::Eof
            } else {
                LineRead::Line
            });
        }
        match chunk.iter().position(|&b| b == b'\n') {
            Some(end) => {
                if buf.len() + end > max {
                    return Ok(LineRead::Oversized);
                }
                buf.extend_from_slice(&chunk[..end]);
                reader.consume(end + 1);
                return Ok(LineRead::Line);
            }
            None => {
                let len = chunk.len();
                if buf.len() + len > max {
                    return Ok(LineRead::Oversized);
                }
                buf.extend_from_slice(chunk);
                reader.consume(len);
            }
        }
    }
}

/// Decode one request line. The error carries the best-known id (0 if none).
fn parse_request(line: &[u8]) -> Result<Request, Response> {
    let value: Value = serde_json::from_slice(line)
        .map_err(|_| Response::err(0, RpcError::new(PARSE_ERROR, "malformed JSON")))?;
    let id = value.get("id").and_then(Value::as_u64).unwrap_or(0);
    let request: Request = serde_json::from_value(value)
        .map_err(|_| Response::err(id, RpcError::new(INVALID_REQUEST, "not a valid request")))?;
    if request.jsonrpc != JSONRPC_VERSION {
        return Err(Response::err(
            id,
            RpcError::new(
                INVALID_REQUEST,
                format!("jsonrpc must be \"{JSONRPC_VERSION}\""),
            ),
        ));
    }
    Ok(request)
}

fn invalid_request(id: u64, message: &str) -> Response {
    Response::err(id, RpcError::new(INVALID_REQUEST, message))
}

/// Validate the handshake. `Ok` is the response to send; `Err` is the refusal
/// after which the connection closes.
fn handshake(request: &Request) -> Result<Response, Response> {
    if request.method != HELLO {
        return Err(invalid_request(request.id, "first request must be hello"));
    }
    let Ok(hello) = serde_json::from_value::<HelloParams>(request.params.clone()) else {
        return Err(invalid_request(request.id, "invalid hello params"));
    };
    if hello.protocol_version != PROTOCOL_VERSION {
        return Err(Response::err(
            request.id,
            RpcError::new(
                VERSION_MISMATCH,
                format!(
                    "protocol version {} is not supported; daemon speaks {PROTOCOL_VERSION}",
                    hello.protocol_version
                ),
            ),
        ));
    }
    Ok(Response::ok(
        request.id,
        json!({"protocol_version": PROTOCOL_VERSION, "daemon_pid": std::process::id()}),
    ))
}

async fn session(
    reader: OwnedReadHalf,
    ctx: &ConnCtx,
    dispatcher: &DaemonDispatcher,
    shutdown: &mut watch::Receiver<bool>,
) {
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    let mut greeted = false;
    loop {
        line.clear();
        let read = tokio::select! {
            read = read_bounded_line(&mut reader, &mut line, MAX_MESSAGE_BYTES) => read,
            () = wait_shutdown(shutdown) => return,
        };
        match read {
            Ok(LineRead::Line) => {}
            Ok(LineRead::Eof) => return,
            Ok(LineRead::Oversized) => {
                tracing::warn!(conn = ctx.id(), "oversized message; closing");
                return;
            }
            Err(err) => {
                tracing::debug!(conn = ctx.id(), %err, "read failed");
                return;
            }
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }

        let parsed = parse_request(&line);
        if !greeted {
            let outcome = match &parsed {
                Ok(request) => handshake(request),
                Err(response) => Err(invalid_request(
                    response.id,
                    "first request must be a valid hello",
                )),
            };
            match outcome {
                Ok(response) => {
                    if !ctx.send_response(&response, false).await {
                        return;
                    }
                    greeted = true;
                }
                Err(response) => {
                    ctx.send_response(&response, false).await;
                    return;
                }
            }
            continue;
        }

        let request = match parsed {
            Ok(request) => request,
            Err(response) => {
                if !ctx.send_response(&response, false).await {
                    return;
                }
                continue;
            }
        };
        let response = if request.method == HELLO {
            Response::err(
                request.id,
                RpcError::new(INVALID_STATE, "hello was already completed"),
            )
        } else {
            match dispatcher.call(ctx, &request.method, request.params).await {
                // `null` cannot round-trip through `Response.result`.
                Ok(Value::Null) => Response::ok(request.id, json!({})),
                Ok(value) => Response::ok(request.id, value),
                Err(error) => Response::err(request.id, error),
            }
        };
        let stop_after = request.method == DAEMON_SHUTDOWN && response.error.is_none();
        if !ctx.send_response(&response, stop_after).await {
            return;
        }
        // A `subscribe` just answered may start sending now.
        ctx.subscriptions().release();
        if stop_after {
            tracing::info!(conn = ctx.id(), "shutdown requested");
            dispatcher.request_shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_reports_false_when_the_queue_is_full() {
        let (ctx, mut rx) = ConnCtx::new(1, 2);
        let note = Notification::new("event", json!({}));
        assert!(ctx.notify(&note));
        assert!(ctx.notify(&note));
        assert!(!ctx.notify(&note), "third notification must not fit");
        // Draining makes room again.
        assert!(rx.try_recv().is_ok());
        assert!(ctx.notify(&note));
    }

    #[test]
    fn notify_reports_false_when_the_connection_is_gone() {
        let (ctx, rx) = ConnCtx::new(1, 2);
        drop(rx);
        assert!(!ctx.notify(&Notification::new("event", json!({}))));
    }

    #[tokio::test]
    async fn responses_wait_for_room_instead_of_dropping() {
        let (ctx, mut rx) = ConnCtx::new(1, 1);
        assert!(ctx.notify(&Notification::new("event", json!({}))));
        let sender = ctx.clone();
        let send = tokio::spawn(async move {
            sender
                .send_response(&Response::ok(7, json!({})), false)
                .await
        });
        sleep(Duration::from_millis(50)).await;
        assert!(
            !send.is_finished(),
            "response must wait while the queue is full"
        );
        assert!(rx.recv().await.is_some());
        assert!(send.await.expect("send task"));
        assert!(rx.recv().await.is_some(), "the response was queued");
    }

    #[test]
    fn clip_limits_chars_not_bytes() {
        let text = "あ".repeat(100);
        assert_eq!(clip_error_detail(&text).chars().count(), crate::ECHO_LIMIT);
    }

    #[tokio::test]
    async fn bounded_reader_rejects_long_lines_without_buffering_them() {
        let data = [b'a'; 100];
        let mut reader = BufReader::with_capacity(8, &data[..]);
        let mut buf = Vec::new();
        let res = read_bounded_line(&mut reader, &mut buf, 20)
            .await
            .expect("read");
        assert!(matches!(res, LineRead::Oversized));
        assert!(buf.len() <= 20 + 8);
    }

    #[tokio::test]
    async fn bounded_reader_splits_lines_and_delivers_unterminated_tail() {
        let mut reader = BufReader::with_capacity(4, &b"ab\ncde"[..]);
        let mut buf = Vec::new();
        assert!(matches!(
            read_bounded_line(&mut reader, &mut buf, 10)
                .await
                .expect("read"),
            LineRead::Line
        ));
        assert_eq!(buf, b"ab");
        buf.clear();
        assert!(matches!(
            read_bounded_line(&mut reader, &mut buf, 10)
                .await
                .expect("read"),
            LineRead::Line
        ));
        assert_eq!(buf, b"cde");
        buf.clear();
        assert!(matches!(
            read_bounded_line(&mut reader, &mut buf, 10)
                .await
                .expect("read"),
            LineRead::Eof
        ));
    }
}
