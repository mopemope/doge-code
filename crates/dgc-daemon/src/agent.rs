//! AgentManager: the agent state machine over provider sessions.
//!
//! - Each live agent (from `create`/`resume` until it fails, stops or is
//!   archived) has one tokio task, its actor (`actor.rs`), that owns the
//!   `ProviderSession` and the message queue. The manager sends it
//!   `Command`s over an mpsc channel and gets the answer on a oneshot, so a
//!   command sent while the agent is starting is handled once the start ends.
//! - Agent records live in the [`Store`] and are rewritten on every change;
//!   `get`/`list` read them from there.
//! - Every event is appended to the agent's [`EventLog`] first, then sent on
//!   one global broadcast channel (capacity [`EVENTS_CHANNEL_CAPACITY`]), so a
//!   subscriber can always backfill from the log by `seq`. Broadcast sends
//!   never block: a subscriber that stops reading only lags itself.
//! - The message queue is persisted to `<state>/agents/<id>/queue.json` on
//!   every change and loaded again by `resume`.
//! - Open log handles are bounded (see [`LogCache`]): live agents keep theirs,
//!   at most [`MAX_IDLE_LOG_HANDLES`] other agents' handles are cached.
//! - Lock order: `live` → `logs` → one `EventLog`. `permissions` is never held
//!   while taking another lock, and no lock is held across an `.await`.
//!   Agents that are not live are changed only while holding `live`, so such a
//!   change cannot race with a resume or with an actor's final transition.
//! - Provider calls are bounded by [`ProviderTimeouts`]; a call that times
//!   out fails the agent, so a wedged session never blocks archive.
//! - Tracing carries ids and states only, never prompt or output text.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use dgc_protocol::chunk_text;
use dgc_protocol::event::{AgentEvent, EventKind};
use dgc_protocol::ids::{AgentId, PermissionId, WorkspaceId};
use dgc_protocol::methods::{
    AgentCreateParams, AgentEventsParams, AgentSendParams, MAX_EVENTS_LIMIT,
    PermissionRespondParams,
};
use dgc_protocol::model::{
    Agent, AgentState, PendingPermission, ProviderKind, SendBehavior, SendOutcome, Workspace,
    WorkspaceState,
};
use dgc_protocol::rpc::{INTERNAL, INVALID_PARAMS, INVALID_STATE, NOT_FOUND};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::{Instant, timeout_at};
use tracing::{info, warn};

mod actor;

use self::actor::{Command, Launch, Start, register, run_actor};
use crate::config::DaemonConfig;
use crate::events_log::EventLog;
use crate::paths::{DaemonPaths, ensure_private_dir};
use crate::provider::{ProviderAdapter, ProviderError};
use crate::store::{Store, write_atomic};
use crate::time::now_rfc3339;

/// Capacity of the global event broadcast channel.
pub const EVENTS_CHANNEL_CAPACITY: usize = 1024;
const QUEUE_VERSION: u32 = 1;
const QUEUE_FILE: &str = "queue.json";
/// Most event log handles kept open for agents that are not live (read
/// through `events_page`), beyond the handles of live agents.
const MAX_IDLE_LOG_HANDLES: usize = 64;
/// Directory under the state dir holding agent records (same as `Store`).
const AGENTS_DIR: &str = "agents";
/// How long archive (and a failing agent) waits for its session to end.
const SESSION_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// Extra time `shutdown_all` gives actors beyond the session grace.
const SHUTDOWN_MARGIN: Duration = Duration::from_millis(500);
/// Default bound on `ProviderAdapter::start`.
pub const PROVIDER_START_TIMEOUT: Duration = Duration::from_secs(60);
/// Default bound on each `ProviderSession` call other than `shutdown`.
pub const PROVIDER_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Bounds on provider calls. A call that exceeds its bound fails the agent,
/// so one wedged session cannot block the agent's later commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderTimeouts {
    pub start: Duration,
    pub call: Duration,
}

impl Default for ProviderTimeouts {
    fn default() -> Self {
        Self {
            start: PROVIDER_START_TIMEOUT,
            call: PROVIDER_CALL_TIMEOUT,
        }
    }
}

/// Agent operation failures. Messages never contain prompt or agent text.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// The named entity (`agent`, `workspace`, `permission`) does not exist.
    #[error("{0} not found")]
    NotFound(&'static str),
    /// Not allowed now; `state` is the agent state in snake_case, or
    /// `workspace_archived`.
    #[error("operation not allowed in state {state}")]
    InvalidState { state: String },
    #[error("{0}")]
    InvalidParams(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("storage error: {0}")]
    Storage(String),
}

impl AgentError {
    /// JSON-RPC error code for this failure.
    pub fn code(&self) -> i32 {
        match self {
            Self::NotFound(_) => NOT_FOUND,
            Self::InvalidState { .. } => INVALID_STATE,
            Self::InvalidParams(_) => INVALID_PARAMS,
            Self::Provider(_) | Self::Storage(_) => INTERNAL,
        }
    }

    fn state(state: AgentState) -> Self {
        Self::InvalidState {
            state: state_label(state).to_owned(),
        }
    }
}

fn state_label(state: AgentState) -> &'static str {
    match state {
        AgentState::Starting => "starting",
        AgentState::Idle => "idle",
        AgentState::Running => "running",
        AgentState::WaitingPermission => "waiting_permission",
        AgentState::Stopped => "stopped",
        AgentState::Failed => "failed",
        AgentState::Archived => "archived",
    }
}

fn storage(err: impl std::fmt::Display) -> AgentError {
    AgentError::Storage(err.to_string())
}

fn provider(err: ProviderError) -> AgentError {
    AgentError::Provider(err.to_string())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QueueItem {
    text: String,
    behavior: SendBehavior,
}

#[derive(Serialize)]
struct QueueFileRef<'a> {
    version: u32,
    items: &'a VecDeque<QueueItem>,
}

#[derive(Deserialize)]
struct QueueFile {
    version: u32,
    items: VecDeque<QueueItem>,
}

/// Read a persisted queue; a missing file is an empty queue.
fn load_queue(path: &Path) -> Result<VecDeque<QueueItem>, AgentError> {
    let read_error = |e: io::Error| storage(format_args!("cannot read {}: {e}", path.display()));
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(VecDeque::new()),
        Err(err) => return Err(read_error(err)),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(read_error)?;
    let parsed: QueueFile = serde_json::from_slice(&bytes)
        .map_err(|e| storage(format_args!("invalid {}: {e}", path.display())))?;
    if parsed.version != QUEUE_VERSION {
        return Err(storage(format_args!(
            "{} has unsupported version {}",
            path.display(),
            parsed.version
        )));
    }
    Ok(parsed.items)
}

fn save_queue(path: &Path, items: &VecDeque<QueueItem>) -> Result<(), AgentError> {
    if let Some(dir) = path.parent() {
        ensure_private_dir(dir).map_err(storage)?;
    }
    let bytes = serde_json::to_vec(&QueueFileRef {
        version: QUEUE_VERSION,
        items,
    })
    .map_err(storage)?;
    write_atomic(path, &bytes).map_err(storage)
}

struct PendingEntry {
    permission: PendingPermission,
    provider_request_id: String,
}

struct CachedLog {
    log: Arc<Mutex<EventLog>>,
    /// Value of [`LogCache::tick`] at the last use.
    last_used: u64,
}

/// Open event logs, one handle per agent, opened on first use.
///
/// Handles of live agents are never evicted: a live agent appends through
/// its handle, and a second handle for the same file would hand out `seq`
/// numbers twice. Handles of agents that are not live are evicted least
/// recently used first beyond [`MAX_IDLE_LOG_HANDLES`], and dropped when the
/// agent is archived.
///
/// Log-lock invariant (the subscription seam relies on it): `emit` appends
/// and broadcasts under the agent's log lock, and `events_page` reads under
/// the same lock. Eviction keeps that intact because it only removes a handle
/// that nothing else holds (`Arc::strong_count == 1`, checked under the
/// `logs` lock, which also guards every clone), so no append or read can be
/// in flight on an evicted handle. Re-opening the log of an agent that is not
/// live is safe: the only appends to such an agent are state changes made
/// while holding `live`, each through whichever single handle is cached.
#[derive(Default)]
struct LogCache {
    logs: HashMap<AgentId, CachedLog>,
    /// Agents with an actor; maintained under `live` by `register_live` and
    /// `Shared::finish`.
    live: HashSet<AgentId>,
    tick: u64,
}

impl LogCache {
    fn get(&mut self, id: &AgentId) -> Option<Arc<Mutex<EventLog>>> {
        self.tick += 1;
        let tick = self.tick;
        self.logs.get_mut(id).map(|cached| {
            cached.last_used = tick;
            Arc::clone(&cached.log)
        })
    }

    fn insert(&mut self, id: &AgentId, log: Arc<Mutex<EventLog>>) {
        self.tick += 1;
        self.logs.insert(
            id.clone(),
            CachedLog {
                log,
                last_used: self.tick,
            },
        );
        self.evict();
    }

    /// Drop least recently used handles of non-live agents beyond the bound.
    /// Handles in use are skipped; the cache may exceed the bound until they
    /// are released.
    fn evict(&mut self) {
        let mut idle = self
            .logs
            .iter()
            .filter(|(id, _)| !self.live.contains(*id))
            .map(|(id, cached)| (cached.last_used, id.clone(), Arc::strong_count(&cached.log)))
            .collect::<Vec<_>>();
        if idle.len() <= MAX_IDLE_LOG_HANDLES {
            return;
        }
        idle.sort_by_key(|(last_used, ..)| *last_used);
        let excess = idle.len() - MAX_IDLE_LOG_HANDLES;
        let mut removed = 0;
        for (_, id, holders) in idle {
            if removed == excess {
                break;
            }
            if holders == 1 {
                self.logs.remove(&id);
                removed += 1;
            }
        }
    }

    fn mark_live(&mut self, id: &AgentId) {
        self.live.insert(id.clone());
    }

    fn mark_not_live(&mut self, id: &AgentId) {
        self.live.remove(id);
        self.evict();
    }

    /// Forget the agent's handle (it was archived; nothing appends again).
    fn forget(&mut self, id: &AgentId) {
        self.live.remove(id);
        self.logs.remove(id);
    }
}

/// State shared by the manager and every actor.
struct Shared {
    store: Arc<Store>,
    agents_dir: PathBuf,
    events_max_bytes: u64,
    events: broadcast::Sender<AgentEvent>,
    /// Command channels of live agents.
    live: Mutex<HashMap<AgentId, mpsc::Sender<Command>>>,
    logs: Mutex<LogCache>,
    permissions: Mutex<HashMap<PermissionId, PendingEntry>>,
}

impl Shared {
    fn agent_dir(&self, id: &AgentId) -> PathBuf {
        self.agents_dir.join(id.as_str())
    }

    fn queue_path(&self, id: &AgentId) -> PathBuf {
        self.agent_dir(id).join(QUEUE_FILE)
    }

    fn log(&self, id: &AgentId) -> Result<Arc<Mutex<EventLog>>, AgentError> {
        let mut logs = lock(&self.logs);
        if let Some(log) = logs.get(id) {
            return Ok(log);
        }
        let log = EventLog::open(&self.agent_dir(id), id.clone(), self.events_max_bytes)
            .map_err(storage)?;
        let log = Arc::new(Mutex::new(log));
        logs.insert(id, Arc::clone(&log));
        Ok(log)
    }

    /// Add a live agent to `live` (held by the caller) and pin its log.
    fn register_live(
        &self,
        live: &mut HashMap<AgentId, mpsc::Sender<Command>>,
        id: &AgentId,
    ) -> mpsc::Receiver<Command> {
        lock(&self.logs).mark_live(id);
        register(live, id)
    }

    /// Append `kind` to the agent's log, then broadcast it. Broadcasting under
    /// the log lock keeps the live stream in `seq` order.
    fn emit(&self, id: &AgentId, kind: EventKind) -> Result<AgentEvent, AgentError> {
        let log = self.log(id)?;
        let mut log = lock(&log);
        let event = log.append(kind).map_err(storage)?;
        // An error only means nobody is subscribed.
        let _ = self.events.send(event.clone());
        Ok(event)
    }

    /// Record a user message as one `UserMessage` event per chunk of at most
    /// [`MAX_EVENT_TEXT_BYTES`](dgc_protocol::MAX_EVENT_TEXT_BYTES), all with
    /// the same `behavior`.
    fn emit_user(
        &self,
        id: &AgentId,
        text: &str,
        behavior: SendBehavior,
    ) -> Result<(), AgentError> {
        for chunk in chunk_text(text) {
            self.emit(
                id,
                EventKind::UserMessage {
                    text: chunk,
                    behavior,
                },
            )?;
        }
        Ok(())
    }

    /// Record the message that starts a turn: its `UserMessage` chunks, then
    /// one `TurnStarted`.
    fn emit_turn_start(
        &self,
        id: &AgentId,
        text: &str,
        behavior: SendBehavior,
    ) -> Result<(), AgentError> {
        self.emit_user(id, text, behavior)?;
        self.emit(id, EventKind::TurnStarted).map(drop)
    }

    fn emit_or_warn(&self, id: &AgentId, kind: EventKind) {
        if let Err(err) = self.emit(id, kind) {
            warn!(agent_id = %id, error = %err, "could not record agent event");
        }
    }

    fn put_agent(&self, agent: &Agent) -> Result<(), AgentError> {
        self.store.put_agent(agent).map_err(storage)
    }

    /// Set, store and announce `state`.
    fn store_state(&self, agent: &mut Agent, state: AgentState) -> Result<(), AgentError> {
        agent.state = state;
        let stored = self.put_agent(agent);
        let emitted = self.emit(&agent.id, EventKind::StateChanged { state });
        stored.and(emitted.map(drop))
    }

    /// Final transition of a live agent: leave the live set and store `state`
    /// in one step under `live`, so a concurrent resume or archive sees either
    /// the live actor or the final state.
    fn finish(&self, agent: &mut Agent, state: AgentState) {
        let mut live = lock(&self.live);
        live.remove(&agent.id);
        lock(&self.logs).mark_not_live(&agent.id);
        if let Err(err) = self.store_state(agent, state) {
            warn!(agent_id = %agent.id, ?state, error = %err, "could not store final agent state");
        }
        if state == AgentState::Archived {
            lock(&self.logs).forget(&agent.id);
        }
        info!(agent_id = %agent.id, ?state, "agent is no longer live");
    }
}

/// Supervises agents. Cheap to share behind an `Arc`.
pub struct AgentManager {
    shared: Arc<Shared>,
    adapters: HashMap<ProviderKind, Arc<dyn ProviderAdapter>>,
    timeouts: ProviderTimeouts,
}

impl AgentManager {
    pub fn new(
        store: Arc<Store>,
        paths: DaemonPaths,
        config: DaemonConfig,
        adapters: HashMap<ProviderKind, Arc<dyn ProviderAdapter>>,
    ) -> Self {
        let (events, _) = broadcast::channel(EVENTS_CHANNEL_CAPACITY);
        Self {
            shared: Arc::new(Shared {
                store,
                agents_dir: paths.state_dir.join(AGENTS_DIR),
                events_max_bytes: config.events_max_bytes,
                events,
                live: Mutex::new(HashMap::new()),
                logs: Mutex::new(LogCache::default()),
                permissions: Mutex::new(HashMap::new()),
            }),
            adapters,
            timeouts: ProviderTimeouts::default(),
        }
    }

    /// Replace the provider call bounds (tests use short ones).
    pub fn with_timeouts(mut self, timeouts: ProviderTimeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    fn load(&self, id: &AgentId) -> Result<Agent, AgentError> {
        self.shared
            .store
            .get_agent(id)
            .map_err(storage)?
            .ok_or(AgentError::NotFound("agent"))
    }

    fn active_workspace(&self, id: &WorkspaceId) -> Result<Workspace, AgentError> {
        let workspace = self
            .shared
            .store
            .get_workspace(id)
            .map_err(storage)?
            .ok_or(AgentError::NotFound("workspace"))?;
        if workspace.state != WorkspaceState::Active {
            return Err(AgentError::InvalidState {
                state: "workspace_archived".to_owned(),
            });
        }
        Ok(workspace)
    }

    fn adapter(&self, kind: ProviderKind) -> Result<Arc<dyn ProviderAdapter>, AgentError> {
        self.adapters
            .get(&kind)
            .cloned()
            .ok_or_else(|| AgentError::InvalidParams(format!("provider {kind:?} is not available")))
    }

    /// Store a new agent, record its initial prompt and start it. Returns once
    /// the start has been initiated; the agent becomes `running` (or `failed`)
    /// asynchronously.
    pub async fn create(&self, params: AgentCreateParams) -> Result<Agent, AgentError> {
        let workspace = self.active_workspace(&params.workspace_id)?;
        let adapter = self.adapter(params.provider)?;
        let auto_approve = adapter
            .validate_mode(params.mode.as_deref())
            .map_err(AgentError::InvalidParams)?;
        let mut agent = Agent {
            id: AgentId::generate(),
            workspace_id: params.workspace_id,
            title: params.title,
            provider: params.provider,
            model: params.model,
            mode: params.mode,
            auto_approve,
            provider_session_id: None,
            state: AgentState::Starting,
            parent_id: params.parent_id,
            labels: params.labels,
            created_at: now_rfc3339(),
            last_error: None,
        };
        self.shared.put_agent(&agent)?;
        let recorded = self
            .shared
            .emit(
                &agent.id,
                EventKind::StateChanged {
                    state: AgentState::Starting,
                },
            )
            .and_then(|_| {
                self.shared
                    .emit_turn_start(&agent.id, &params.initial_prompt, SendBehavior::Queue)
            });
        if let Err(err) = recorded {
            // Do not leave a record that claims to be starting.
            agent.state = AgentState::Failed;
            agent.last_error = Some(err.to_string());
            let _ = self.shared.put_agent(&agent);
            return Err(err);
        }
        let commands = self
            .shared
            .register_live(&mut lock(&self.shared.live), &agent.id);
        info!(agent_id = %agent.id, provider = ?agent.provider, auto_approve, "agent created");
        let launch = Launch {
            adapter,
            agent: agent.clone(),
            cwd: workspace.path,
            start: Start::Create {
                prompt: params.initial_prompt,
            },
            queue: VecDeque::new(),
            timeouts: self.timeouts,
        };
        tokio::spawn(run_actor(Arc::clone(&self.shared), launch, commands));
        Ok(agent)
    }

    /// Restart a stopped or failed agent from its provider session. The first
    /// queued message, if any, is sent; otherwise the agent becomes idle.
    pub async fn resume(&self, id: &AgentId) -> Result<Agent, AgentError> {
        let (agent, cwd, adapter, queue, commands) = {
            let mut live = lock(&self.shared.live);
            let mut agent = self.load(id)?;
            if live.contains_key(id)
                || !matches!(agent.state, AgentState::Stopped | AgentState::Failed)
            {
                return Err(AgentError::state(agent.state));
            }
            let workspace = self.active_workspace(&agent.workspace_id)?;
            let adapter = self.adapter(agent.provider)?;
            let queue = load_queue(&self.shared.queue_path(id))?;
            agent.last_error = None;
            self.shared.store_state(&mut agent, AgentState::Starting)?;
            let commands = self.shared.register_live(&mut live, id);
            (agent, workspace.path, adapter, queue, commands)
        };
        info!(agent_id = %agent.id, queued = queue.len(), "agent resuming");
        let launch = Launch {
            adapter,
            agent: agent.clone(),
            cwd,
            start: Start::Resume,
            queue,
            timeouts: self.timeouts,
        };
        tokio::spawn(run_actor(Arc::clone(&self.shared), launch, commands));
        Ok(agent)
    }

    /// Hand a command to the agent's actor and wait for its answer.
    async fn request<T>(
        &self,
        id: &AgentId,
        make: impl FnOnce(oneshot::Sender<Result<T, AgentError>>) -> Command,
    ) -> Result<T, AgentError> {
        let sender = lock(&self.shared.live).get(id).cloned();
        let Some(sender) = sender else {
            return Err(self.not_live(id));
        };
        let (reply, answer) = oneshot::channel();
        if sender.send(make(reply)).await.is_err() {
            return Err(self.not_live(id));
        }
        answer.await.unwrap_or_else(|_| Err(self.not_live(id)))
    }

    /// Error for an agent without an actor: its stored state, or not found.
    fn not_live(&self, id: &AgentId) -> AgentError {
        match self.load(id) {
            Ok(agent) => AgentError::state(agent.state),
            Err(err) => err,
        }
    }

    pub async fn send(&self, params: AgentSendParams) -> Result<SendOutcome, AgentError> {
        let AgentSendParams {
            agent_id,
            text,
            behavior,
        } = params;
        self.request(&agent_id, |reply| Command::Send {
            text,
            behavior,
            reply,
        })
        .await
    }

    /// Stop the running turn; pending permissions are cancelled and the agent
    /// becomes idle.
    pub async fn interrupt(&self, id: &AgentId) -> Result<(), AgentError> {
        self.request(id, |reply| Command::Interrupt { reply }).await
    }

    pub async fn respond_permission(
        &self,
        params: PermissionRespondParams,
    ) -> Result<(), AgentError> {
        let agent_id = lock(&self.shared.permissions)
            .get(&params.id)
            .map(|entry| entry.permission.agent_id.clone())
            .ok_or(AgentError::NotFound("permission"))?;
        let result = self
            .request(&agent_id, |reply| Command::RespondPermission {
                id: params.id,
                decision: params.decision,
                reply,
            })
            .await;
        match result {
            // The agent ended meanwhile, cancelling the request.
            Err(AgentError::InvalidState { .. }) => Err(AgentError::NotFound("permission")),
            other => other,
        }
    }

    /// Shut the agent's session down (if live) and mark it archived.
    pub async fn archive(&self, id: &AgentId) -> Result<Agent, AgentError> {
        let sender = lock(&self.shared.live).get(id).cloned();
        if let Some(sender) = sender {
            let (reply, answer) = oneshot::channel();
            if sender.send(Command::Archive { reply }).await.is_ok()
                && let Ok(result) = answer.await
            {
                return result;
            }
            // The actor ended before handling the request: archive the
            // stored record instead.
        }
        let live = lock(&self.shared.live);
        let mut agent = self.load(id)?;
        if live.contains_key(id) || agent.state == AgentState::Archived {
            return Err(AgentError::state(agent.state));
        }
        self.shared.store_state(&mut agent, AgentState::Archived)?;
        lock(&self.shared.logs).forget(id);
        info!(agent_id = %agent.id, "agent archived");
        Ok(agent)
    }

    pub fn get(&self, id: &AgentId) -> Result<Agent, AgentError> {
        self.load(id)
    }

    pub fn list(&self) -> Result<Vec<Agent>, AgentError> {
        self.shared.store.list_agents().map_err(storage)
    }

    /// Unresolved permission requests, oldest first, optionally of one agent.
    pub fn pending_permissions(&self, agent: Option<AgentId>) -> Vec<PendingPermission> {
        let mut pending: Vec<PendingPermission> = lock(&self.shared.permissions)
            .values()
            .filter(|entry| {
                agent
                    .as_ref()
                    .is_none_or(|id| *id == entry.permission.agent_id)
            })
            .map(|entry| entry.permission.clone())
            .collect();
        pending.sort_by(|a, b| a.id.cmp(&b.id));
        pending
    }

    /// Receiver of every agent's events. `agent` is advisory: the receiver
    /// carries all agents' events and the consumer filters by `agent_id`. A
    /// receiver that falls more than [`EVENTS_CHANNEL_CAPACITY`] events
    /// behind gets `Lagged` and should backfill with [`Self::events_page`].
    pub fn subscribe(&self, agent: Option<AgentId>) -> broadcast::Receiver<AgentEvent> {
        let _ = agent;
        self.shared.events.subscribe()
    }

    /// Number of cached event log handles. For tests.
    #[doc(hidden)]
    pub fn open_log_handles(&self) -> usize {
        lock(&self.shared.logs).logs.len()
    }

    /// One page of stored events, from a sequence cursor or the end of the log.
    pub fn events_page(&self, params: &AgentEventsParams) -> Result<Vec<AgentEvent>, AgentError> {
        self.load(&params.agent_id)?;
        let log = self.shared.log(&params.agent_id)?;
        let log = lock(&log);
        match params.tail {
            Some(n) => log.read_tail(n.min(MAX_EVENTS_LIMIT)),
            None => log.read_page(params.since_seq, params.clamped_limit()),
        }
        .map_err(storage)
    }

    /// At daemon start no provider process exists: mark agents stored as
    /// starting/running/waiting/idle as stopped. Every agent is processed;
    /// the first failure is returned.
    pub fn recover_on_startup(&self) -> Result<(), AgentError> {
        let live = lock(&self.shared.live);
        let mut first_error = None;
        for mut agent in self.shared.store.list_agents().map_err(storage)? {
            let previous = agent.state;
            if live.contains_key(&agent.id)
                || !matches!(
                    previous,
                    AgentState::Starting
                        | AgentState::Running
                        | AgentState::WaitingPermission
                        | AgentState::Idle
                )
            {
                continue;
            }
            match self.shared.store_state(&mut agent, AgentState::Stopped) {
                Ok(()) => info!(agent_id = %agent.id, from = ?previous, "agent stopped by restart"),
                Err(err) => {
                    warn!(agent_id = %agent.id, error = %err, "could not mark agent stopped");
                    first_error.get_or_insert(err);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Shut down every live session (daemon exit). Agents become `stopped`.
    /// Waits at most `grace` plus a short margin.
    pub async fn shutdown_all(&self, grace: Duration) {
        let senders: Vec<(AgentId, mpsc::Sender<Command>)> = lock(&self.shared.live)
            .iter()
            .map(|(id, sender)| (id.clone(), sender.clone()))
            .collect();
        let deadline = Instant::now() + grace + SHUTDOWN_MARGIN;
        let mut answers = Vec::with_capacity(senders.len());
        for (id, sender) in senders {
            let (reply, answer) = oneshot::channel();
            match timeout_at(deadline, sender.send(Command::Shutdown { grace, reply })).await {
                Ok(Ok(())) => answers.push((id, answer)),
                Ok(Err(_)) => {} // Already ended.
                Err(_) => warn!(agent_id = %id, "agent did not accept shutdown in time"),
            }
        }
        for (id, answer) in answers {
            if timeout_at(deadline, answer).await.is_err() {
                warn!(agent_id = %id, "agent did not shut down in time");
            }
        }
    }
}
