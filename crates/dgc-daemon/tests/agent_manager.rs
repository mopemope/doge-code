//! AgentManager state machine driven through an in-test scripted provider.
//!
//! The test plays the provider: every started session is handed to the test,
//! which decides when the "provider" emits events and observes which session
//! methods the manager called.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dgc_daemon::agent::{AgentError, AgentManager, CreateOptions, ProviderTimeouts};
use dgc_daemon::config::DaemonConfig;
use dgc_daemon::paths::DaemonPaths;
use dgc_daemon::provider::{
    AgentSpec, BoxFut, ProviderAdapter, ProviderError, ProviderEvent, ProviderSession,
};
use dgc_daemon::store::Store;
use dgc_protocol::event::{AgentEvent, EventKind};
use dgc_protocol::ids::{AgentId, ProjectId, WorkspaceId};
use dgc_protocol::methods::{
    AgentCreateParams, AgentEventsParams, AgentSendParams, PermissionRespondParams,
};
use dgc_protocol::model::{
    Agent, AgentState, Decision, Isolation, PermissionKind, ProviderKind, Resolution, SendBehavior,
    SendOutcome, Workspace, WorkspaceState,
};
use tempfile::{TempDir, tempdir};
use tokio::sync::mpsc;
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(5);
/// Provider call bounds used by every test manager.
const TEST_PROVIDER_TIMEOUT: Duration = Duration::from_millis(300);

/// What the manager asked a scripted session to do.
#[derive(Debug, Clone, PartialEq)]
enum Call {
    SendUser(String),
    Steer(String),
    Interrupt,
    Respond(String, Decision),
    Shutdown,
}

/// The test's end of one started session.
struct SessionHandle {
    spec: AgentSpec,
    events: mpsc::Sender<ProviderEvent>,
    calls: mpsc::UnboundedReceiver<Call>,
    /// Events the session emits from inside `interrupt`, before it returns
    /// (a provider finishing the turn just as the interrupt arrives).
    interrupt_emits: Arc<Mutex<Vec<ProviderEvent>>>,
}

impl SessionHandle {
    async fn emit(&self, event: ProviderEvent) {
        self.events.send(event).await.expect("session alive");
    }

    async fn next_call(&mut self) -> Call {
        timeout(WAIT, self.calls.recv())
            .await
            .expect("timed out waiting for a session call")
            .expect("session dropped")
    }

    fn assert_no_call(&mut self) {
        match self.calls.try_recv() {
            Err(mpsc::error::TryRecvError::Empty) => {}
            other => panic!("unexpected call {other:?}"),
        }
    }
}

struct ScriptedAdapter {
    steer_supported: bool,
    fail_start: bool,
    start_hangs: bool,
    interrupt_hangs: bool,
    sessions: mpsc::UnboundedSender<SessionHandle>,
}

impl ProviderAdapter for ScriptedAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Codex
    }

    fn validate_mode(&self, mode: Option<&str>) -> Result<bool, String> {
        match mode {
            None | Some("workspace-write") => Ok(false),
            Some("full-access") => Ok(true),
            Some(other) => Err(format!("unknown mode {other}")),
        }
    }

    fn start(
        &self,
        spec: AgentSpec,
    ) -> BoxFut<'_, Result<Box<dyn ProviderSession>, ProviderError>> {
        Box::pin(async move {
            if self.fail_start {
                return Err(ProviderError::Spawn("scripted start failure".to_owned()));
            }
            if self.start_hangs {
                std::future::pending::<()>().await;
            }
            let (events_tx, events_rx) = mpsc::channel(4096);
            let (calls_tx, calls_rx) = mpsc::unbounded_channel();
            let interrupt_emits = Arc::new(Mutex::new(Vec::new()));
            self.sessions
                .send(SessionHandle {
                    spec,
                    events: events_tx.clone(),
                    calls: calls_rx,
                    interrupt_emits: Arc::clone(&interrupt_emits),
                })
                .map_err(|_| ProviderError::Spawn("test gone".to_owned()))?;
            Ok(Box::new(ScriptedSession {
                steer_supported: self.steer_supported,
                interrupt_hangs: self.interrupt_hangs,
                events: events_rx,
                events_tx,
                interrupt_emits,
                calls: calls_tx,
            }) as Box<dyn ProviderSession>)
        })
    }
}

struct ScriptedSession {
    steer_supported: bool,
    interrupt_hangs: bool,
    events: mpsc::Receiver<ProviderEvent>,
    events_tx: mpsc::Sender<ProviderEvent>,
    interrupt_emits: Arc<Mutex<Vec<ProviderEvent>>>,
    calls: mpsc::UnboundedSender<Call>,
}

impl ScriptedSession {
    fn record(&self, call: Call) -> Result<(), ProviderError> {
        self.calls
            .send(call)
            .map_err(|_| ProviderError::Io("test gone".to_owned()))
    }
}

impl ProviderSession for ScriptedSession {
    fn send_user<'a>(&'a mut self, text: &'a str) -> BoxFut<'a, Result<(), ProviderError>> {
        Box::pin(async move { self.record(Call::SendUser(text.to_owned())) })
    }

    fn steer<'a>(&'a mut self, text: &'a str) -> BoxFut<'a, Result<bool, ProviderError>> {
        Box::pin(async move {
            self.record(Call::Steer(text.to_owned()))?;
            Ok(self.steer_supported)
        })
    }

    fn interrupt(&mut self) -> BoxFut<'_, Result<(), ProviderError>> {
        Box::pin(async move {
            self.record(Call::Interrupt)?;
            if self.interrupt_hangs {
                std::future::pending::<()>().await;
            }
            let emits = std::mem::take(&mut *self.interrupt_emits.lock().expect("emits lock"));
            for event in emits {
                self.events_tx
                    .send(event)
                    .await
                    .map_err(|_| ProviderError::Io("events closed".to_owned()))?;
            }
            Ok(())
        })
    }

    fn respond_permission<'a>(
        &'a mut self,
        provider_request_id: &'a str,
        d: Decision,
    ) -> BoxFut<'a, Result<(), ProviderError>> {
        Box::pin(async move { self.record(Call::Respond(provider_request_id.to_owned(), d)) })
    }

    fn events(&mut self) -> &mut mpsc::Receiver<ProviderEvent> {
        &mut self.events
    }

    fn shutdown(&mut self) -> BoxFut<'_, Result<(), ProviderError>> {
        Box::pin(async move { self.record(Call::Shutdown) })
    }
}

struct Harness {
    _dir: TempDir,
    paths: DaemonPaths,
    store: Arc<Store>,
    workspace: Workspace,
    manager: AgentManager,
    sessions: mpsc::UnboundedReceiver<SessionHandle>,
}

struct Options {
    steer_supported: bool,
    fail_start: bool,
    start_hangs: bool,
    interrupt_hangs: bool,
}

const DEFAULT: Options = Options {
    steer_supported: true,
    fail_start: false,
    start_hangs: false,
    interrupt_hangs: false,
};

impl Harness {
    fn new(options: Options) -> Self {
        let dir = tempdir().expect("tempdir");
        let paths = DaemonPaths::under(dir.path());
        let store = Arc::new(Store::open(&paths.state_dir).expect("store"));
        let ws_path = dir.path().join("ws");
        std::fs::create_dir_all(&ws_path).expect("ws dir");
        let workspace = Workspace {
            id: WorkspaceId::generate(),
            project_id: ProjectId::generate(),
            isolation: Isolation::Local,
            path: ws_path,
            branch: None,
            base_ref: None,
            state: WorkspaceState::Active,
            owned: false,
            created_at: "2026-11-25T00:00:00.000Z".to_owned(),
        };
        store.put_workspace(&workspace).expect("put workspace");
        let (sessions_tx, sessions) = mpsc::unbounded_channel();
        let manager = new_manager(&store, &paths, options, sessions_tx);
        Self {
            _dir: dir,
            paths,
            store,
            workspace,
            manager,
            sessions,
        }
    }

    async fn next_session(&mut self) -> SessionHandle {
        timeout(WAIT, self.sessions.recv())
            .await
            .expect("timed out waiting for a session start")
            .expect("adapter dropped")
    }

    async fn create(&self, prompt: &str) -> Agent {
        self.manager
            .create(AgentCreateParams {
                workspace_id: self.workspace.id.clone(),
                title: "t".to_owned(),
                provider: Some(ProviderKind::Codex),
                profile: None,
                model: Some("m1".to_owned()),
                mode: None,
                initial_prompt: prompt.to_owned(),
                labels: BTreeMap::new(),
                parent_id: None,
            })
            .await
            .expect("create")
    }

    /// Create an agent and bring it to `Running` with the initial prompt sent.
    async fn running(&mut self, prompt: &str) -> (Agent, SessionHandle) {
        let agent = self.create(prompt).await;
        let mut session = self.next_session().await;
        assert_eq!(session.next_call().await, Call::SendUser(prompt.to_owned()));
        self.wait_state(&agent.id, AgentState::Running).await;
        (agent, session)
    }

    async fn send(&self, id: &AgentId, text: &str, behavior: SendBehavior) -> SendOutcome {
        self.manager
            .send(AgentSendParams {
                agent_id: id.clone(),
                text: text.to_owned(),
                behavior,
            })
            .await
            .expect("send")
    }

    /// Poll the stored record until it reaches `state`.
    async fn wait_state(&self, id: &AgentId, state: AgentState) -> Agent {
        let poll = async {
            loop {
                let agent = self.manager.get(id).expect("get");
                if agent.state == state {
                    return agent;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        timeout(WAIT, poll)
            .await
            .unwrap_or_else(|_| panic!("agent never reached {state:?}"))
    }

    fn events(&self, id: &AgentId) -> Vec<EventKind> {
        self.manager
            .events_page(&AgentEventsParams {
                agent_id: id.clone(),
                since_seq: 0,
                limit: 1000,
                tail: None,
            })
            .expect("events page")
            .into_iter()
            .map(|e| e.kind)
            .collect()
    }

    fn queue_texts(&self, id: &AgentId) -> Vec<String> {
        let path = self
            .paths
            .state_dir
            .join("agents")
            .join(id.as_str())
            .join("queue.json");
        let bytes = std::fs::read(&path).expect("queue.json exists");
        let v: serde_json::Value = serde_json::from_slice(&bytes).expect("queue json");
        assert_eq!(v["version"], 1);
        v["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|i| i["text"].as_str().expect("text").to_owned())
            .collect()
    }
}

fn new_manager(
    store: &Arc<Store>,
    paths: &DaemonPaths,
    options: Options,
    sessions: mpsc::UnboundedSender<SessionHandle>,
) -> AgentManager {
    let adapter: Arc<dyn ProviderAdapter> = Arc::new(ScriptedAdapter {
        steer_supported: options.steer_supported,
        fail_start: options.fail_start,
        start_hangs: options.start_hangs,
        interrupt_hangs: options.interrupt_hangs,
        sessions,
    });
    let mut adapters = HashMap::new();
    adapters.insert(ProviderKind::Codex, adapter);
    AgentManager::new(
        Arc::clone(store),
        paths.clone(),
        DaemonConfig::default(),
        adapters,
    )
    .with_timeouts(ProviderTimeouts {
        start: TEST_PROVIDER_TIMEOUT,
        call: TEST_PROVIDER_TIMEOUT,
    })
}

fn state_changes(events: &[EventKind]) -> Vec<AgentState> {
    events
        .iter()
        .filter_map(|k| match k {
            EventKind::StateChanged { state } => Some(*state),
            _ => None,
        })
        .collect()
}

fn permission_request(id: &str) -> ProviderEvent {
    ProviderEvent::PermissionRequested {
        provider_request_id: id.to_owned(),
        kind: PermissionKind::Command,
        summary: format!("run {id}"),
        raw: serde_json::json!({"cmd": "ls"}),
    }
}

fn turn_completed() -> ProviderEvent {
    ProviderEvent::TurnCompleted { usage: None }
}

#[tokio::test]
async fn create_runs_initial_prompt_then_idle() {
    let mut h = Harness::new(DEFAULT);
    let agent = h.create("hello").await;
    assert_eq!(agent.state, AgentState::Starting);
    assert_eq!(agent.model.as_deref(), Some("m1"));
    assert!(!agent.auto_approve);

    let mut session = h.next_session().await;
    assert_eq!(session.spec.agent_id, agent.id);
    assert_eq!(session.spec.cwd, h.workspace.path);
    assert_eq!(session.spec.model.as_deref(), Some("m1"));
    assert_eq!(session.spec.resume_session_id, None);
    assert_eq!(
        session.next_call().await,
        Call::SendUser("hello".to_owned())
    );
    h.wait_state(&agent.id, AgentState::Running).await;

    session
        .emit(ProviderEvent::SessionStarted {
            session_id: "sess-1".to_owned(),
        })
        .await;
    session
        .emit(ProviderEvent::Event(EventKind::AssistantText {
            text: "hi".to_owned(),
            final_: true,
        }))
        .await;
    session.emit(turn_completed()).await;
    let idle = h.wait_state(&agent.id, AgentState::Idle).await;
    assert_eq!(idle.provider_session_id.as_deref(), Some("sess-1"));

    let events = h.events(&agent.id);
    assert_eq!(
        events,
        vec![
            EventKind::StateChanged {
                state: AgentState::Starting
            },
            EventKind::UserMessage {
                text: "hello".to_owned(),
                behavior: SendBehavior::Queue
            },
            EventKind::TurnStarted,
            EventKind::StateChanged {
                state: AgentState::Running
            },
            EventKind::AssistantText {
                text: "hi".to_owned(),
                final_: true
            },
            EventKind::TurnCompleted { usage: None },
            EventKind::StateChanged {
                state: AgentState::Idle
            },
        ]
    );
    assert_eq!(h.manager.list().expect("list").len(), 1);
}

#[tokio::test]
async fn create_validates_workspace_provider_and_mode() {
    let h = Harness::new(DEFAULT);
    let params = |workspace_id: WorkspaceId, provider, mode: Option<&str>| AgentCreateParams {
        workspace_id,
        title: "t".to_owned(),
        provider: Some(provider),
        profile: None,
        model: None,
        mode: mode.map(str::to_owned),
        initial_prompt: "p".to_owned(),
        labels: BTreeMap::new(),
        parent_id: None,
    };
    let ws = h.workspace.id.clone();

    let err = h
        .manager
        .create(params(WorkspaceId::generate(), ProviderKind::Codex, None))
        .await
        .expect_err("unknown workspace");
    assert!(matches!(err, AgentError::NotFound(_)), "{err:?}");

    let err = h
        .manager
        .create(params(ws.clone(), ProviderKind::Claude, None))
        .await
        .expect_err("no claude adapter");
    assert!(matches!(err, AgentError::InvalidParams(_)), "{err:?}");

    let err = h
        .manager
        .create(params(ws.clone(), ProviderKind::Codex, Some("bogus")))
        .await
        .expect_err("bad mode");
    assert!(matches!(err, AgentError::InvalidParams(_)), "{err:?}");

    let agent = h
        .manager
        .create(params(ws.clone(), ProviderKind::Codex, Some("full-access")))
        .await
        .expect("auto-approve mode");
    assert!(agent.auto_approve);

    let mut archived = h.workspace.clone();
    archived.state = WorkspaceState::Archived;
    h.store.put_workspace(&archived).expect("archive ws");
    let err = h
        .manager
        .create(params(ws, ProviderKind::Codex, None))
        .await
        .expect_err("archived workspace");
    assert!(matches!(err, AgentError::InvalidState { .. }), "{err:?}");
}

#[tokio::test]
async fn queue_is_drained_after_turn() {
    let mut h = Harness::new(DEFAULT);
    let (agent, mut session) = h.running("first").await;

    assert_eq!(
        h.send(&agent.id, "second", SendBehavior::Queue).await,
        SendOutcome::Queued
    );
    assert_eq!(
        h.send(&agent.id, "third", SendBehavior::Queue).await,
        SendOutcome::Queued
    );
    session.assert_no_call();
    assert_eq!(h.queue_texts(&agent.id), vec!["second", "third"]);

    session.emit(turn_completed()).await;
    assert_eq!(
        session.next_call().await,
        Call::SendUser("second".to_owned())
    );
    assert_eq!(
        h.manager.get(&agent.id).expect("get").state,
        AgentState::Running
    );
    session.emit(turn_completed()).await;
    assert_eq!(
        session.next_call().await,
        Call::SendUser("third".to_owned())
    );
    session.emit(turn_completed()).await;
    h.wait_state(&agent.id, AgentState::Idle).await;
    assert!(h.queue_texts(&agent.id).is_empty());

    // Idle: a message is sent right away.
    assert_eq!(
        h.send(&agent.id, "fourth", SendBehavior::Queue).await,
        SendOutcome::Sent
    );
    assert_eq!(
        session.next_call().await,
        Call::SendUser("fourth".to_owned())
    );
    h.wait_state(&agent.id, AgentState::Running).await;

    let events = h.events(&agent.id);
    let sent: Vec<&str> = events
        .iter()
        .filter_map(|k| match k {
            EventKind::UserMessage { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(sent, vec!["first", "second", "third", "fourth"]);
    // Running is never left while the queue drains.
    assert_eq!(
        state_changes(&events),
        vec![
            AgentState::Starting,
            AgentState::Running,
            AgentState::Idle,
            AgentState::Running
        ]
    );
}

#[tokio::test]
async fn steer_unsupported_falls_back_to_interrupt() {
    let mut h = Harness::new(Options {
        steer_supported: false,
        ..DEFAULT
    });
    let (agent, mut session) = h.running("first").await;

    assert_eq!(
        h.send(&agent.id, "change", SendBehavior::Steer).await,
        SendOutcome::InterruptedAndSent
    );
    assert_eq!(session.next_call().await, Call::Steer("change".to_owned()));
    assert_eq!(session.next_call().await, Call::Interrupt);
    assert_eq!(
        session.next_call().await,
        Call::SendUser("change".to_owned())
    );
    assert_eq!(
        h.manager.get(&agent.id).expect("get").state,
        AgentState::Running
    );

    assert_eq!(
        h.send(&agent.id, "stop", SendBehavior::Interrupt).await,
        SendOutcome::InterruptedAndSent
    );
    assert_eq!(session.next_call().await, Call::Interrupt);
    assert_eq!(session.next_call().await, Call::SendUser("stop".to_owned()));

    assert!(h.events(&agent.id).contains(&EventKind::UserMessage {
        text: "change".to_owned(),
        behavior: SendBehavior::Steer
    }));
}

#[tokio::test]
async fn steer_supported_returns_steered() {
    let mut h = Harness::new(DEFAULT);
    let (agent, mut session) = h.running("first").await;
    assert_eq!(
        h.send(&agent.id, "nudge", SendBehavior::Steer).await,
        SendOutcome::Steered
    );
    assert_eq!(session.next_call().await, Call::Steer("nudge".to_owned()));
    session.assert_no_call();
    assert_eq!(
        h.manager.get(&agent.id).expect("get").state,
        AgentState::Running
    );
}

#[tokio::test]
async fn permission_roundtrip_returns_to_running() {
    let mut h = Harness::new(DEFAULT);
    let (agent, mut session) = h.running("first").await;

    session.emit(permission_request("req-1")).await;
    h.wait_state(&agent.id, AgentState::WaitingPermission).await;
    let pending = h.manager.pending_permissions(Some(agent.id.clone()));
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].agent_id, agent.id);
    assert_eq!(pending[0].kind, PermissionKind::Command);
    assert_eq!(pending[0].summary, "run req-1");
    assert_eq!(h.manager.pending_permissions(None).len(), 1);
    assert!(
        h.manager
            .pending_permissions(Some(AgentId::generate()))
            .is_empty()
    );
    let perm_id = pending[0].id.clone();

    h.manager
        .respond_permission(PermissionRespondParams {
            id: perm_id.clone(),
            decision: Decision::Allow,
        })
        .await
        .expect("respond");
    assert_eq!(
        session.next_call().await,
        Call::Respond("req-1".to_owned(), Decision::Allow)
    );
    h.wait_state(&agent.id, AgentState::Running).await;
    assert!(h.manager.pending_permissions(None).is_empty());

    let err = h
        .manager
        .respond_permission(PermissionRespondParams {
            id: perm_id.clone(),
            decision: Decision::Deny,
        })
        .await
        .expect_err("already resolved");
    assert!(matches!(err, AgentError::NotFound(_)), "{err:?}");

    let events = h.events(&agent.id);
    assert!(events.iter().any(|k| matches!(
        k,
        EventKind::PermissionRequested { permission } if permission.id == perm_id
    )));
    assert!(events.contains(&EventKind::PermissionResolved {
        id: perm_id,
        resolution: Resolution::Decided(Decision::Allow)
    }));
}

#[tokio::test]
async fn interrupt_cancels_pending_permissions() {
    let mut h = Harness::new(DEFAULT);
    let (agent, mut session) = h.running("first").await;
    session.emit(permission_request("req-1")).await;
    session.emit(permission_request("req-2")).await;
    let poll = async {
        while h.manager.pending_permissions(None).len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    timeout(WAIT, poll).await.expect("two pending permissions");
    assert_eq!(
        h.manager.get(&agent.id).expect("get").state,
        AgentState::WaitingPermission
    );

    h.manager.interrupt(&agent.id).await.expect("interrupt");
    assert_eq!(session.next_call().await, Call::Interrupt);
    assert_eq!(
        h.manager.get(&agent.id).expect("get").state,
        AgentState::Idle
    );
    assert!(h.manager.pending_permissions(None).is_empty());
    let cancelled = h
        .events(&agent.id)
        .into_iter()
        .filter(|k| {
            matches!(
                k,
                EventKind::PermissionResolved {
                    resolution: Resolution::Cancelled,
                    ..
                }
            )
        })
        .count();
    assert_eq!(cancelled, 2);

    // Idle cannot be interrupted.
    let err = h.manager.interrupt(&agent.id).await.expect_err("idle");
    assert!(matches!(err, AgentError::InvalidState { .. }), "{err:?}");
}

#[tokio::test]
async fn crash_marks_failed_and_keeps_queue() {
    let mut h = Harness::new(DEFAULT);
    let (agent, session) = h.running("first").await;
    assert_eq!(
        h.send(&agent.id, "later", SendBehavior::Queue).await,
        SendOutcome::Queued
    );
    session.emit(permission_request("req-1")).await;
    h.wait_state(&agent.id, AgentState::WaitingPermission).await;

    session
        .emit(ProviderEvent::Exited {
            success: false,
            stderr_tail: "boom tail".to_owned(),
        })
        .await;
    let failed = h.wait_state(&agent.id, AgentState::Failed).await;
    assert!(
        failed
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("boom tail")),
        "{failed:?}"
    );
    assert!(h.manager.pending_permissions(None).is_empty());
    assert_eq!(h.queue_texts(&agent.id), vec!["later"]);
    let events = h.events(&agent.id);
    assert!(events.iter().any(|k| matches!(
        k,
        EventKind::PermissionResolved {
            resolution: Resolution::Cancelled,
            ..
        }
    )));
    assert!(
        events
            .iter()
            .any(|k| matches!(k, EventKind::Error { message } if message.contains("boom tail")))
    );
}

#[tokio::test]
async fn resume_uses_provider_session_id() {
    let mut h = Harness::new(DEFAULT);
    let (agent, session) = h.running("first").await;
    session
        .emit(ProviderEvent::SessionStarted {
            session_id: "sess-9".to_owned(),
        })
        .await;
    assert_eq!(
        h.send(&agent.id, "next", SendBehavior::Queue).await,
        SendOutcome::Queued
    );
    session
        .emit(ProviderEvent::Exited {
            success: false,
            stderr_tail: String::new(),
        })
        .await;
    h.wait_state(&agent.id, AgentState::Failed).await;

    // Queue present: the head is sent and the agent runs.
    let resumed = h.manager.resume(&agent.id).await.expect("resume");
    assert_eq!(resumed.state, AgentState::Starting);
    let mut session = h.next_session().await;
    assert_eq!(session.spec.resume_session_id.as_deref(), Some("sess-9"));
    assert_eq!(session.spec.cwd, h.workspace.path);
    assert_eq!(session.next_call().await, Call::SendUser("next".to_owned()));
    let running = h.wait_state(&agent.id, AgentState::Running).await;
    assert_eq!(running.last_error, None);
    assert!(h.queue_texts(&agent.id).is_empty());

    // Resuming a running agent is not allowed.
    let err = h.manager.resume(&agent.id).await.expect_err("running");
    assert!(matches!(err, AgentError::InvalidState { .. }), "{err:?}");

    // Empty queue: resume ends Idle without sending anything.
    session
        .emit(ProviderEvent::Exited {
            success: true,
            stderr_tail: String::new(),
        })
        .await;
    h.wait_state(&agent.id, AgentState::Failed).await;
    h.manager.resume(&agent.id).await.expect("resume again");
    let mut session = h.next_session().await;
    assert_eq!(session.spec.resume_session_id.as_deref(), Some("sess-9"));
    h.wait_state(&agent.id, AgentState::Idle).await;
    session.assert_no_call();
}

#[tokio::test]
async fn send_to_failed_agent_is_invalid_state() {
    let h = Harness::new(Options {
        fail_start: true,
        ..DEFAULT
    });
    let agent = h.create("first").await;
    let failed = h.wait_state(&agent.id, AgentState::Failed).await;
    assert!(
        failed
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("scripted start failure")),
        "{failed:?}"
    );

    for behavior in [
        SendBehavior::Queue,
        SendBehavior::Steer,
        SendBehavior::Interrupt,
    ] {
        let err = h
            .manager
            .send(AgentSendParams {
                agent_id: agent.id.clone(),
                text: "x".to_owned(),
                behavior,
            })
            .await
            .expect_err("failed agent");
        assert!(
            matches!(&err, AgentError::InvalidState { state } if state == "failed"),
            "{err:?}"
        );
    }
    let err = h.manager.interrupt(&agent.id).await.expect_err("failed");
    assert!(matches!(err, AgentError::InvalidState { .. }), "{err:?}");

    let err = h
        .manager
        .send(AgentSendParams {
            agent_id: AgentId::generate(),
            text: "x".to_owned(),
            behavior: SendBehavior::Queue,
        })
        .await
        .expect_err("unknown agent");
    assert!(matches!(err, AgentError::NotFound(_)), "{err:?}");
}

#[tokio::test]
async fn archive_shuts_down_live_and_stopped_agents() {
    let mut h = Harness::new(DEFAULT);
    let (agent, mut session) = h.running("first").await;
    let archived = h.manager.archive(&agent.id).await.expect("archive live");
    assert_eq!(archived.state, AgentState::Archived);
    assert_eq!(session.next_call().await, Call::Shutdown);
    assert_eq!(
        h.manager.get(&agent.id).expect("get").state,
        AgentState::Archived
    );
    let err = h.manager.archive(&agent.id).await.expect_err("twice");
    assert!(matches!(err, AgentError::InvalidState { .. }), "{err:?}");
    let err = h.manager.resume(&agent.id).await.expect_err("archived");
    assert!(matches!(err, AgentError::InvalidState { .. }), "{err:?}");

    // A failed (not live) agent is archived without a session.
    let (other, session) = h.running("second").await;
    session
        .emit(ProviderEvent::Exited {
            success: false,
            stderr_tail: String::new(),
        })
        .await;
    h.wait_state(&other.id, AgentState::Failed).await;
    h.manager.archive(&other.id).await.expect("archive failed");
    assert_eq!(
        state_changes(&h.events(&other.id)).last(),
        Some(&AgentState::Archived)
    );
}

#[tokio::test]
async fn restart_recovery_marks_running_as_stopped() {
    let dir = tempdir().expect("tempdir");
    let paths = DaemonPaths::under(dir.path());
    let store = Arc::new(Store::open(&paths.state_dir).expect("store"));
    let mut ids = Vec::new();
    for state in [
        AgentState::Starting,
        AgentState::Running,
        AgentState::WaitingPermission,
        AgentState::Idle,
        AgentState::Stopped,
        AgentState::Failed,
        AgentState::Archived,
    ] {
        let agent = Agent {
            id: AgentId::generate(),
            workspace_id: WorkspaceId::generate(),
            title: "t".to_owned(),
            provider: ProviderKind::Codex,
            model: None,
            mode: None,
            auto_approve: false,
            stop_when_idle: false,
            provider_session_id: Some("sess".to_owned()),
            state,
            parent_id: None,
            labels: BTreeMap::new(),
            created_at: "2026-11-25T00:00:00.000Z".to_owned(),
            last_error: None,
        };
        store.put_agent(&agent).expect("put agent");
        ids.push((agent.id, state));
    }

    let (sessions_tx, _sessions) = mpsc::unbounded_channel();
    let manager = new_manager(&store, &paths, DEFAULT, sessions_tx);
    manager.recover_on_startup().expect("recover");

    for (id, before) in ids {
        let after = manager.get(&id).expect("get").state;
        let expected = match before {
            AgentState::Starting
            | AgentState::Running
            | AgentState::WaitingPermission
            | AgentState::Idle => AgentState::Stopped,
            other => other,
        };
        assert_eq!(after, expected, "from {before:?}");
        let events: Vec<AgentEvent> = manager
            .events_page(&AgentEventsParams {
                agent_id: id.clone(),
                since_seq: 0,
                limit: 10,
                tail: None,
            })
            .expect("events");
        let stopped_event = events.iter().any(|e| {
            e.kind
                == EventKind::StateChanged {
                    state: AgentState::Stopped,
                }
        });
        assert_eq!(stopped_event, expected != before, "event for {before:?}");
    }
}

#[tokio::test]
async fn slow_subscriber_does_not_block_other_agents() {
    let mut h = Harness::new(DEFAULT);
    // Never read from this receiver.
    let _stalled = h.manager.subscribe(None);

    let (noisy, noisy_session) = h.running("noisy").await;
    let (quiet, quiet_session) = h.running("quiet").await;
    for i in 0..3000 {
        noisy_session
            .emit(ProviderEvent::Event(EventKind::AssistantText {
                text: format!("chunk {i}"),
                final_: false,
            }))
            .await;
    }
    quiet_session.emit(turn_completed()).await;
    h.wait_state(&quiet.id, AgentState::Idle).await;

    noisy_session.emit(turn_completed()).await;
    h.wait_state(&noisy.id, AgentState::Idle).await;
    let noisy_events = h.events(&noisy.id).len();
    // Starting, prompt, Running, 3000 chunks, TurnCompleted, Idle; the page
    // limit caps the read at 1000.
    assert_eq!(noisy_events, 1000);

    // A fresh subscriber sees live events.
    let mut live = h.manager.subscribe(Some(quiet.id.clone()));
    assert_eq!(
        h.send(&quiet.id, "again", SendBehavior::Queue).await,
        SendOutcome::Sent
    );
    let event = timeout(WAIT, async {
        loop {
            let event = live.recv().await.expect("live event");
            if event.agent_id == quiet.id {
                return event;
            }
        }
    })
    .await
    .expect("live event in time");
    assert_eq!(
        event.kind,
        EventKind::UserMessage {
            text: "again".to_owned(),
            behavior: SendBehavior::Queue
        }
    );
}

#[tokio::test]
async fn shutdown_all_stops_live_sessions() {
    let mut h = Harness::new(DEFAULT);
    let (a, mut sa) = h.running("a").await;
    let (b, mut sb) = h.running("b").await;
    h.manager.shutdown_all(Duration::from_secs(2)).await;
    assert_eq!(sa.next_call().await, Call::Shutdown);
    assert_eq!(sb.next_call().await, Call::Shutdown);
    for id in [&a.id, &b.id] {
        assert_eq!(h.manager.get(id).expect("get").state, AgentState::Stopped);
    }
}

/// Task 13 drives the manager from per-connection tasks: its futures must be `Send`.
#[test]
fn manager_futures_are_send() {
    fn assert_send<T: Send>(_: &T) {}
    fn check(manager: &AgentManager, params: AgentCreateParams, send: AgentSendParams) {
        assert_send(&manager.create(params));
        assert_send(&manager.send(send));
        assert_send(&manager.resume(&AgentId::generate()));
        assert_send(&manager.archive(&AgentId::generate()));
        assert_send(&manager.shutdown_all(Duration::from_secs(1)));
    }
    let _ = check;
}

/// Give the actor time to apply anything still buffered.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(100)).await;
}

#[tokio::test]
async fn stale_turn_completed_after_interrupt_is_ignored() {
    let mut h = Harness::new(DEFAULT);
    let (agent, mut session) = h.running("first").await;
    assert_eq!(
        h.send(&agent.id, "queued", SendBehavior::Queue).await,
        SendOutcome::Queued
    );

    // The old turn ends just as the interrupt arrives: its TurnCompleted is
    // buffered before `interrupt` returns.
    session
        .interrupt_emits
        .lock()
        .expect("emits lock")
        .push(turn_completed());
    assert_eq!(
        h.send(&agent.id, "now", SendBehavior::Interrupt).await,
        SendOutcome::InterruptedAndSent
    );
    assert_eq!(session.next_call().await, Call::Interrupt);
    assert_eq!(session.next_call().await, Call::SendUser("now".to_owned()));
    settle().await;
    // The new turn is still running; the interrupt discarded the queue.
    session.assert_no_call();
    assert_eq!(
        h.manager.get(&agent.id).expect("get").state,
        AgentState::Running
    );
    assert!(h.queue_texts(&agent.id).is_empty());
    assert!(
        h.events(&agent.id)
            .contains(&EventKind::TurnCompleted { usage: None })
    );

    // The new turn's own completion idles the agent (nothing queued).
    session.emit(turn_completed()).await;
    h.wait_state(&agent.id, AgentState::Idle).await;
    session.assert_no_call();
    assert_eq!(
        h.send(&agent.id, "again", SendBehavior::Queue).await,
        SendOutcome::Sent
    );
    assert_eq!(
        session.next_call().await,
        Call::SendUser("again".to_owned())
    );

    // Plain interrupt: the stale completion does not idle the next turn.
    session
        .interrupt_emits
        .lock()
        .expect("emits lock")
        .push(turn_completed());
    h.manager.interrupt(&agent.id).await.expect("interrupt");
    assert_eq!(session.next_call().await, Call::Interrupt);
    assert_eq!(
        h.send(&agent.id, "after", SendBehavior::Queue).await,
        SendOutcome::Sent
    );
    assert_eq!(
        session.next_call().await,
        Call::SendUser("after".to_owned())
    );
    settle().await;
    assert_eq!(
        h.manager.get(&agent.id).expect("get").state,
        AgentState::Running
    );
}

#[tokio::test]
async fn stale_permission_request_after_interrupt_is_dropped() {
    let mut h = Harness::new(DEFAULT);
    let (agent, mut session) = h.running("first").await;
    session
        .interrupt_emits
        .lock()
        .expect("emits lock")
        .push(permission_request("old"));
    h.manager.interrupt(&agent.id).await.expect("interrupt");
    assert_eq!(session.next_call().await, Call::Interrupt);
    settle().await;
    assert_eq!(
        h.manager.get(&agent.id).expect("get").state,
        AgentState::Idle
    );
    assert!(h.manager.pending_permissions(None).is_empty());

    // Same when the interrupt is part of a send.
    assert_eq!(
        h.send(&agent.id, "go", SendBehavior::Queue).await,
        SendOutcome::Sent
    );
    assert_eq!(session.next_call().await, Call::SendUser("go".to_owned()));
    session
        .interrupt_emits
        .lock()
        .expect("emits lock")
        .push(permission_request("old-2"));
    assert_eq!(
        h.send(&agent.id, "redo", SendBehavior::Interrupt).await,
        SendOutcome::InterruptedAndSent
    );
    settle().await;
    assert_eq!(
        h.manager.get(&agent.id).expect("get").state,
        AgentState::Running
    );
    assert!(h.manager.pending_permissions(None).is_empty());
    assert!(
        !h.events(&agent.id)
            .iter()
            .any(|k| matches!(k, EventKind::PermissionRequested { .. }))
    );
}

#[tokio::test]
async fn wedged_interrupt_fails_agent_and_archive_succeeds() {
    let mut h = Harness::new(Options {
        interrupt_hangs: true,
        ..DEFAULT
    });
    let (agent, mut session) = h.running("first").await;
    let err = timeout(WAIT, h.manager.interrupt(&agent.id))
        .await
        .expect("interrupt is bounded")
        .expect_err("timed out");
    assert!(matches!(err, AgentError::Provider(_)), "{err:?}");
    assert_eq!(session.next_call().await, Call::Interrupt);
    let failed = h.wait_state(&agent.id, AgentState::Failed).await;
    assert!(
        failed
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("interrupt timed out")),
        "{failed:?}"
    );
    assert_eq!(session.next_call().await, Call::Shutdown);
    let archived = timeout(WAIT, h.manager.archive(&agent.id))
        .await
        .expect("archive in time")
        .expect("archive");
    assert_eq!(archived.state, AgentState::Archived);
}

#[tokio::test]
async fn hung_start_fails_agent() {
    let h = Harness::new(Options {
        start_hangs: true,
        ..DEFAULT
    });
    let agent = h.create("first").await;
    let failed = h.wait_state(&agent.id, AgentState::Failed).await;
    assert!(
        failed
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("start timed out")),
        "{failed:?}"
    );
}

#[tokio::test]
async fn failed_start_keeps_initial_prompt_for_resume() {
    for options in [
        Options {
            fail_start: true,
            ..DEFAULT
        },
        Options {
            start_hangs: true,
            ..DEFAULT
        },
    ] {
        let mut h = Harness::new(options);
        let agent = h.create("recover this prompt").await;
        h.wait_state(&agent.id, AgentState::Failed).await;
        assert_eq!(h.queue_texts(&agent.id), ["recover this prompt"]);
        // A failed resume must leave exactly the original pending item.
        h.manager.resume(&agent.id).await.expect("resume");
        h.wait_state(&agent.id, AgentState::Failed).await;
        assert_eq!(h.queue_texts(&agent.id), ["recover this prompt"]);

        let (tx, rx) = mpsc::unbounded_channel();
        h.manager = new_manager(&h.store, &h.paths, DEFAULT, tx);
        h.sessions = rx;
        h.manager.resume(&agent.id).await.expect("resume");
        let mut session = h.next_session().await;
        assert_eq!(session.spec.resume_session_id, None);
        assert_eq!(
            session.next_call().await,
            Call::SendUser("recover this prompt".to_owned())
        );
        h.wait_state(&agent.id, AgentState::Running).await;
        assert!(h.queue_texts(&agent.id).is_empty());
        session.emit(turn_completed()).await;
        h.wait_state(&agent.id, AgentState::Idle).await;
        session.assert_no_call();
    }
}

#[tokio::test]
async fn events_tail_param_returns_last_n() {
    let mut h = Harness::new(DEFAULT);
    let (agent, session) = h.running("first").await;
    for _ in 0..1100 {
        session
            .emit(ProviderEvent::Event(EventKind::AssistantText {
                text: "sample".to_owned(),
                final_: false,
            }))
            .await;
    }
    session.emit(turn_completed()).await;
    h.wait_state(&agent.id, AgentState::Idle).await;
    let end = dgc_daemon::handlers::agent_events(
        &h.manager,
        serde_json::json!({"agent_id": agent.id, "since_seq": 1100}),
    )
    .expect("agent.events");
    let last = end["events"]
        .as_array()
        .expect("events")
        .last()
        .expect("last event")["seq"]
        .as_u64()
        .expect("seq");
    let request = |tail| {
        let response = dgc_daemon::handlers::agent_events(
            &h.manager,
            serde_json::json!({"agent_id": agent.id, "since_seq": last + 1, "limit": 1, "tail": tail}),
        ).expect("agent.events");
        serde_json::from_value::<Vec<AgentEvent>>(response["events"].clone()).expect("events")
    };
    assert_eq!(
        request(3).iter().map(|event| event.seq).collect::<Vec<_>>(),
        (last - 2..=last).collect::<Vec<_>>()
    );
    assert_eq!(request(2000).len(), 1000);
    assert!(request(0).is_empty());
}

#[tokio::test]
async fn buffered_turn_completed_is_applied_before_a_later_command() {
    let mut h = Harness::new(DEFAULT);
    let (agent, mut session) = h.running("first").await;
    // The provider finished the turn before the user's message arrived, so
    // the message starts a new turn instead of being queued. Repeated because
    // an unordered choice would pass by chance half of the time.
    for i in 0..20 {
        session.emit(turn_completed()).await;
        let text = format!("next {i}");
        assert_eq!(
            h.send(&agent.id, &text, SendBehavior::Queue).await,
            SendOutcome::Sent,
            "iteration {i}"
        );
        assert_eq!(session.next_call().await, Call::SendUser(text));
    }
}

#[tokio::test]
async fn scheduled_agent_handles_a_waiting_send_before_stopping() {
    let mut h = Harness::new(DEFAULT);
    let agent = h
        .manager
        .create_with(
            AgentCreateParams {
                workspace_id: h.workspace.id.clone(),
                title: "scheduled agent".into(),
                provider: Some(ProviderKind::Codex),
                profile: None,
                model: None,
                mode: None,
                initial_prompt: "scheduled turn".into(),
                labels: BTreeMap::new(),
                parent_id: None,
            },
            CreateOptions {
                stop_when_idle: true,
            },
        )
        .await
        .expect("create");
    let mut session = h.next_session().await;
    assert_eq!(
        session.next_call().await,
        Call::SendUser("scheduled turn".into())
    );
    h.wait_state(&agent.id, AgentState::Running).await;

    // In this current-thread runtime both messages are buffered before
    // the actor next runs. It handles TurnCompleted first.
    session.emit(turn_completed()).await;
    assert_eq!(
        h.send(&agent.id, "follow-up", SendBehavior::Queue).await,
        SendOutcome::Sent
    );
    assert_eq!(
        session.next_call().await,
        Call::SendUser("follow-up".into())
    );

    session.emit(turn_completed()).await;
    assert_eq!(session.next_call().await, Call::Shutdown);
    h.wait_state(&agent.id, AgentState::Stopped).await;
}

#[tokio::test]
async fn a_huge_prompt_is_stored_in_bounded_chunks() {
    let mut h = Harness::new(DEFAULT);
    let prompt = "p".repeat(2 * 1024 * 1024);
    let (agent, mut session) = h.running(&prompt).await;
    let followup = "q".repeat(2 * 1024 * 1024);
    session
        .emit(ProviderEvent::TurnCompleted { usage: None })
        .await;
    h.wait_state(&agent.id, AgentState::Idle).await;
    assert_eq!(
        h.send(&agent.id, &followup, SendBehavior::Queue).await,
        SendOutcome::Sent
    );
    assert_eq!(session.next_call().await, Call::SendUser(followup.clone()));

    let mut events = Vec::new();
    loop {
        let since = events.last().map_or(0, |e: &AgentEvent| e.seq);
        let page = h
            .manager
            .events_page(&AgentEventsParams {
                agent_id: agent.id.clone(),
                since_seq: since,
                limit: 1000,
                tail: None,
            })
            .expect("events page");
        if page.is_empty() {
            break;
        }
        events.extend(page);
    }
    let mut texts: Vec<String> = Vec::new();
    for event in &events {
        let size = serde_json::to_vec(event).expect("serialize").len();
        assert!(size < dgc_protocol::MAX_MESSAGE_BYTES / 2, "{size} bytes");
        if let EventKind::UserMessage { text, .. } = &event.kind {
            texts.push(text.clone());
        }
    }
    assert!(texts.len() >= 16, "{} user chunks", texts.len());
    assert_eq!(texts.concat(), format!("{prompt}{followup}"));
}

#[tokio::test]
async fn interrupt_discards_the_queue_and_records_it() {
    let mut h = Harness::new(DEFAULT);
    let (agent, mut session) = h.running("first").await;
    assert_eq!(
        h.send(&agent.id, "X", SendBehavior::Queue).await,
        SendOutcome::Queued
    );
    assert_eq!(h.queue_texts(&agent.id), vec!["X"]);

    h.manager.interrupt(&agent.id).await.expect("interrupt");
    assert_eq!(session.next_call().await, Call::Interrupt);
    assert!(h.queue_texts(&agent.id).is_empty(), "queue.json is cleared");
    assert!(h.events(&agent.id).contains(&EventKind::QueueDiscarded {
        text: "X".to_owned(),
        behavior: SendBehavior::Queue
    }));

    // A later turn does not pick X up.
    assert_eq!(
        h.send(&agent.id, "Y", SendBehavior::Queue).await,
        SendOutcome::Sent
    );
    assert_eq!(session.next_call().await, Call::SendUser("Y".to_owned()));
    session.emit(turn_completed()).await;
    h.wait_state(&agent.id, AgentState::Idle).await;
    settle().await;
    session.assert_no_call();
}

#[tokio::test]
async fn interrupting_send_discards_the_queue_before_sending() {
    let mut h = Harness::new(DEFAULT);
    let (agent, mut session) = h.running("first").await;
    assert_eq!(
        h.send(&agent.id, "X", SendBehavior::Queue).await,
        SendOutcome::Queued
    );
    assert_eq!(
        h.send(&agent.id, "Y", SendBehavior::Interrupt).await,
        SendOutcome::InterruptedAndSent
    );
    assert_eq!(session.next_call().await, Call::Interrupt);
    assert_eq!(session.next_call().await, Call::SendUser("Y".to_owned()));
    assert!(h.queue_texts(&agent.id).is_empty());
    session.emit(turn_completed()).await;
    h.wait_state(&agent.id, AgentState::Idle).await;
    settle().await;
    session.assert_no_call();

    let events = h.events(&agent.id);
    let discarded = events
        .iter()
        .position(|e| {
            *e == EventKind::QueueDiscarded {
                text: "X".to_owned(),
                behavior: SendBehavior::Queue,
            }
        })
        .expect("discard recorded");
    let sent = events
        .iter()
        .position(|e| {
            *e == EventKind::UserMessage {
                text: "Y".to_owned(),
                behavior: SendBehavior::Interrupt,
            }
        })
        .expect("Y recorded");
    assert!(discarded < sent, "the queue is cleared before Y is sent");
}

/// Kinds of the user-message and turn-start events, in log order, with the
/// message text for user messages.
fn turn_markers(events: &[EventKind]) -> Vec<String> {
    events
        .iter()
        .filter_map(|k| match k {
            EventKind::UserMessage { text, .. } => Some(format!("user:{text}")),
            EventKind::TurnStarted => Some("turn_started".to_owned()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn turn_started_follows_each_user_message() {
    let mut h = Harness::new(DEFAULT);
    // Initial prompt.
    let (agent, mut session) = h.running("first").await;
    assert_eq!(
        turn_markers(&h.events(&agent.id)),
        vec!["user:first", "turn_started"]
    );

    // A steer joins the running turn: no new turn.
    assert_eq!(
        h.send(&agent.id, "nudge", SendBehavior::Steer).await,
        SendOutcome::Steered
    );
    assert_eq!(session.next_call().await, Call::Steer("nudge".to_owned()));

    // A queued message starts a turn when the previous one completes.
    assert_eq!(
        h.send(&agent.id, "second", SendBehavior::Queue).await,
        SendOutcome::Queued
    );
    session.emit(turn_completed()).await;
    assert_eq!(
        session.next_call().await,
        Call::SendUser("second".to_owned())
    );
    session.emit(turn_completed()).await;
    h.wait_state(&agent.id, AgentState::Idle).await;

    // Idle send.
    assert_eq!(
        h.send(&agent.id, "third", SendBehavior::Queue).await,
        SendOutcome::Sent
    );
    assert_eq!(
        session.next_call().await,
        Call::SendUser("third".to_owned())
    );

    // Interrupting send.
    assert_eq!(
        h.send(&agent.id, "fourth", SendBehavior::Interrupt).await,
        SendOutcome::InterruptedAndSent
    );
    assert_eq!(session.next_call().await, Call::Interrupt);
    assert_eq!(
        session.next_call().await,
        Call::SendUser("fourth".to_owned())
    );

    assert_eq!(
        turn_markers(&h.events(&agent.id)),
        vec![
            "user:first",
            "turn_started",
            "user:nudge",
            "user:second",
            "turn_started",
            "user:third",
            "turn_started",
            "user:fourth",
            "turn_started",
        ]
    );
}

#[tokio::test]
async fn turn_started_follows_the_queue_head_sent_after_resume() {
    let mut h = Harness::new(DEFAULT);
    let (agent, session) = h.running("first").await;
    assert_eq!(
        h.send(&agent.id, "later", SendBehavior::Queue).await,
        SendOutcome::Queued
    );
    // The provider dies; the queue stays persisted.
    session
        .emit(ProviderEvent::Exited {
            success: false,
            stderr_tail: "boom".to_owned(),
        })
        .await;
    h.wait_state(&agent.id, AgentState::Failed).await;

    h.manager.resume(&agent.id).await.expect("resume");
    let mut session = h.next_session().await;
    assert_eq!(
        session.next_call().await,
        Call::SendUser("later".to_owned())
    );
    h.wait_state(&agent.id, AgentState::Running).await;
    assert_eq!(
        turn_markers(&h.events(&agent.id)),
        vec!["user:first", "turn_started", "user:later", "turn_started"]
    );
}

#[tokio::test]
async fn a_chunked_prompt_records_one_turn_start_after_the_last_chunk() {
    let mut h = Harness::new(DEFAULT);
    let prompt = "p".repeat(2 * 1024 * 1024);
    let (agent, _session) = h.running(&prompt).await;
    let mut events: Vec<AgentEvent> = Vec::new();
    loop {
        let since = events.last().map_or(0, |e| e.seq);
        let page = h
            .manager
            .events_page(&AgentEventsParams {
                agent_id: agent.id.clone(),
                since_seq: since,
                limit: 1000,
                tail: None,
            })
            .expect("events page");
        if page.is_empty() {
            break;
        }
        events.extend(page);
    }
    let kinds: Vec<EventKind> = events.into_iter().map(|e| e.kind).collect();
    let markers = turn_markers(&kinds);
    assert!(markers.len() >= 9, "{} markers", markers.len());
    assert_eq!(markers.last().map(String::as_str), Some("turn_started"));
    assert_eq!(markers.iter().filter(|m| *m == "turn_started").count(), 1);
}

#[tokio::test]
async fn event_log_handles_are_bounded() {
    let mut h = Harness::new(DEFAULT);
    // Three agents stay live and keep their handles.
    let mut live = Vec::new();
    for i in 0..3 {
        live.push(h.running(&format!("live {i}")).await);
    }
    let mut archived = Vec::new();
    let mut sessions = Vec::new();
    for i in 0..100 {
        let agent = h.create(&format!("a{i}")).await;
        sessions.push(h.next_session().await);
        h.manager.archive(&agent.id).await.expect("archive");
        archived.push(agent);
    }
    // Archiving drops the handle.
    assert!(
        h.manager.open_log_handles() <= 3 + 64,
        "{} handles",
        h.manager.open_log_handles()
    );
    // Reading non-live agents re-opens logs but stays bounded.
    for agent in &archived {
        assert!(!h.events(&agent.id).is_empty(), "archived log is readable");
        assert!(
            h.manager.open_log_handles() <= 3 + 64,
            "{} handles",
            h.manager.open_log_handles()
        );
    }
    assert!(h.manager.open_log_handles() >= 3, "live handles are kept");

    // Live agents still append through their cached handle.
    let (agent, session) = &live[0];
    session.emit(turn_completed()).await;
    h.wait_state(&agent.id, AgentState::Idle).await;
    assert!(
        h.events(&agent.id)
            .contains(&EventKind::TurnCompleted { usage: None })
    );
    drop(sessions);
}

async fn handoff_rpc(
    h: &Harness,
    params: serde_json::Value,
) -> Result<serde_json::Value, dgc_protocol::rpc::RpcError> {
    dgc_daemon::handlers::agent_handoff(&h.manager, params).await
}

fn stored_handoff_source(h: &Harness, provider: ProviderKind, events: Vec<EventKind>) -> AgentId {
    let id = AgentId::generate();
    h.store
        .put_agent(&Agent {
            id: id.clone(),
            workspace_id: h.workspace.id.clone(),
            title: "Source title".to_owned(),
            provider,
            model: Some("source-only-model".to_owned()),
            mode: Some("source-only-mode".to_owned()),
            auto_approve: false,
            stop_when_idle: false,
            provider_session_id: Some("source-only-session".to_owned()),
            state: AgentState::Stopped,
            parent_id: None,
            labels: BTreeMap::new(),
            created_at: String::new(),
            last_error: None,
        })
        .expect("store source");
    let dir = h.paths.state_dir.join("agents").join(id.as_str());
    std::fs::create_dir(&dir).expect("agent dir");
    let mut log = dgc_daemon::events_log::EventLog::open(&dir, id.clone(), 8 * 1024 * 1024)
        .expect("open log");
    log.append(EventKind::StateChanged {
        state: AgentState::Starting,
    })
    .expect("state event");
    for event in events {
        log.append(event).expect("append event");
    }
    id
}

fn instruction(text: impl Into<String>) -> EventKind {
    EventKind::UserMessage {
        text: text.into(),
        behavior: SendBehavior::Queue,
    }
}

fn answer(text: impl Into<String>, final_: bool) -> EventKind {
    EventKind::AssistantText {
        text: text.into(),
        final_,
    }
}

async fn handoff_preview(h: &Harness, id: &AgentId) -> String {
    let result = handoff_rpc(h, serde_json::json!({"agent_id": id, "dry_run": true}))
        .await
        .expect("handoff preview");
    result["prompt"].as_str().expect("prompt").to_owned()
}

fn git_in_workspace(h: &Harness, args: &[&str]) {
    let result = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args(args)
        .current_dir(&h.workspace.path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git");
    assert!(result.status.success(), "{:?}", result);
}

#[tokio::test]
async fn handoff_prompt_contains_instructions_answer_git_and_log_path() {
    let h = Harness::new(DEFAULT);
    git_in_workspace(&h, &["init", "-q"]);
    let file = h.workspace.path.join("edited.txt");
    std::fs::write(&file, "before\n").expect("tracked file");
    git_in_workspace(&h, &["add", "edited.txt"]);
    std::fs::write(&file, "after\n").expect("edit tracked file");
    std::fs::write(h.workspace.path.join("untracked.txt"), "data").expect("untracked file");

    let mut events = vec![
        instruction("first instruction"),
        EventKind::TurnStarted,
        instruction("first instruction"),
        EventKind::TurnStarted,
        answer("old answer", true),
    ];
    for text in ["old instruction", "one", "two", "three", "three"] {
        events.extend([instruction(text), EventKind::TurnStarted]);
    }
    events.extend([answer("last answer ", false), answer("continued", true)]);
    let id = stored_handoff_source(&h, ProviderKind::Claude, events);
    let prompt = handoff_preview(&h, &id).await;
    let log = h
        .paths
        .state_dir
        .join("agents")
        .join(id.as_str())
        .join("events.jsonl");
    let mut after = 0;
    for expected in [
        "You are continuing work started by another coding agent (claude)",
        "Source title",
        "first instruction",
        "one",
        "two",
        "three",
        "last answer continued",
        "?? untracked.txt",
        "edited.txt |",
        &format!(
            "The full log of the previous agent is at {}.",
            log.display()
        ),
    ] {
        after += prompt[after..]
            .find(expected)
            .unwrap_or_else(|| panic!("expected {expected:?} after byte {after} in {prompt}"))
            + expected.len();
    }
    assert_eq!(prompt.matches("three").count(), 1);
    assert!(!prompt.contains("old answer"));
    assert!(!prompt.contains("old instruction"));
}

#[tokio::test]
async fn handoff_prompt_is_bounded_and_strips_control_chars() {
    let h = Harness::new(DEFAULT);
    // The JSON-escaped initial prompt crosses both chunk and normal
    // forward-page boundaries, yet fits after controls are removed.
    let initial = format!(
        "FIRST\u{7}\t\n{}{}END_FIRST",
        "\u{1}".repeat(140_000),
        "初".repeat(4800)
    );
    let mut events: Vec<_> = dgc_protocol::chunk_text(&initial)
        .into_iter()
        .map(instruction)
        .collect();
    events.extend([
        EventKind::TurnStarted,
        instruction("新".repeat(3000)),
        EventKind::TurnStarted,
        answer(format!("OLD_ANSWER{}", "文".repeat(4000)), false),
        answer("END_ANSWER\u{1b}", true),
    ]);
    let id = stored_handoff_source(&h, ProviderKind::Claude, events);
    let prompt = handoff_preview(&h, &id).await;
    assert!(prompt.len() <= 32 * 1024, "{}", prompt.len());
    assert!(prompt.contains("FIRST\t\n"));
    assert!(prompt.contains(&format!("FIRST\t\n{}END_FIRST", "初".repeat(4800))));
    assert_eq!(prompt.matches("END_FIRST").count(), 1);
    assert!(prompt.contains("…(earlier text omitted)"));
    assert!(prompt.contains("END_ANSWER"));
    assert!(!prompt.contains("OLD_ANSWER"));
    assert!(
        !prompt
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
    );
    assert!(prompt.ends_with("events.jsonl.\n"));
}

#[tokio::test]
async fn handoff_prompt_excludes_env_and_account_data() {
    let h = Harness::new(DEFAULT);
    git_in_workspace(&h, &["init", "-q"]);
    std::fs::write(h.workspace.path.join(".env"), "TOKEN=credential-sentinel")
        .expect("untracked credentials file");
    let id = stored_handoff_source(
        &h,
        ProviderKind::Claude,
        vec![
            instruction("first"),
            EventKind::TurnStarted,
            EventKind::RateLimited {
                detail: serde_json::json!({"reason": "limit-sentinel", "account": "account-sentinel"}),
            },
            EventKind::Error {
                message: "error-sentinel".to_owned(),
            },
            EventKind::ToolResult {
                summary: "tool-sentinel".to_owned(),
                is_error: false,
            },
        ],
    );
    let prompt = handoff_preview(&h, &id).await;
    for excluded in ["sentinel", "source-only", "TOKEN="] {
        assert!(!prompt.contains(excluded), "{prompt}");
    }
}

#[tokio::test]
async fn handoff_defaults_to_the_other_provider() {
    let mut h = Harness::new(DEFAULT);
    let id = stored_handoff_source(&h, ProviderKind::Claude, vec![instruction("first")]);
    let result = handoff_rpc(&h, serde_json::json!({"agent_id": id, "dry_run": false}))
        .await
        .expect("handoff");
    let mut target = h.next_session().await;
    assert_eq!(result["agent"]["provider"], "codex");
    assert!(result["agent"]["model"].is_null());
    assert!(result["agent"]["mode"].is_null());
    assert_eq!(
        target.next_call().await,
        Call::SendUser(result["prompt"].as_str().expect("prompt").to_owned())
    );
    h.manager.shutdown_all(WAIT).await;
}

#[tokio::test]
async fn handoff_dry_run_creates_nothing() {
    let mut h = Harness::new(DEFAULT);
    let (source, mut session) = h.running("first").await;
    let result = handoff_rpc(
        &h,
        serde_json::json!({"agent_id": source.id, "provider": "codex", "dry_run": true}),
    )
    .await
    .expect("handoff");
    assert!(result["agent"].is_null());
    assert!(result["prompt"].as_str().expect("prompt").contains("first"));
    assert_eq!(h.manager.list().expect("list").len(), 1);
    assert!(h.sessions.try_recv().is_err());
    session.assert_no_call();
    h.manager.shutdown_all(WAIT).await;
}

#[tokio::test]
async fn handoff_dry_run_checks_target_availability_before_reading_logs() {
    let h = Harness::new(DEFAULT);
    let id = stored_handoff_source(&h, ProviderKind::Codex, vec![instruction("first")]);
    let path = h
        .paths
        .state_dir
        .join("agents")
        .join(id.as_str())
        .join("events.jsonl");
    std::fs::remove_file(&path).expect("remove source log");
    std::fs::create_dir(&path).expect("make source log unreadable as a file");

    let expected = dgc_daemon::handlers::agent_create(
        &h.manager,
        serde_json::json!({
            "workspace_id": h.workspace.id, "provider": "claude",
            "title": "test", "initial_prompt": "test"
        }),
    )
    .await
    .expect_err("target adapter is unavailable");
    assert_eq!(expected.code, dgc_protocol::rpc::INVALID_PARAMS);

    let error = handoff_rpc(&h, serde_json::json!({"agent_id": id, "dry_run": true}))
        .await
        .expect_err("preview must fail for the same unavailable adapter");
    assert_eq!(error, expected);
}

#[tokio::test]
async fn handoff_creates_agent_in_same_workspace_with_label() {
    let mut h = Harness::new(DEFAULT);
    let (source, mut source_session) = h.running("first").await;
    let result = handoff_rpc(
        &h,
        serde_json::json!({
            "agent_id": source.id, "provider": "codex", "mode": "full-access", "dry_run": false
        }),
    )
    .await
    .expect("handoff");
    let mut target = h.next_session().await;
    let agent: Agent = serde_json::from_value(result["agent"].clone()).expect("agent");
    assert_ne!(agent.id, source.id);
    assert_eq!(agent.title, "handoff: t");
    assert_eq!(agent.workspace_id, source.workspace_id);
    assert_eq!(
        agent.labels.get("handoff_from").map(String::as_str),
        Some(source.id.as_str())
    );
    assert!(agent.parent_id.is_none());
    assert!(agent.auto_approve);
    assert_eq!(target.spec.cwd, h.workspace.path);
    assert_eq!(
        target.next_call().await,
        Call::SendUser(result["prompt"].as_str().expect("prompt").to_owned())
    );
    assert_eq!(
        h.manager.get(&source.id).expect("source").state,
        AgentState::Running
    );
    source_session.assert_no_call();
    h.manager.shutdown_all(WAIT).await;
}

#[tokio::test]
async fn handoff_requires_active_source_and_workspace() {
    let mut h = Harness::new(DEFAULT);
    let id = stored_handoff_source(&h, ProviderKind::Claude, vec![]);
    let path = h
        .paths
        .state_dir
        .join("agents")
        .join(id.as_str())
        .join("events.jsonl");
    std::fs::remove_file(&path).expect("remove incomplete source log");
    let params = serde_json::json!({"agent_id": id, "dry_run": true});
    let result = handoff_rpc(&h, params.clone())
        .await
        .expect("preview without log");
    assert!(
        result["prompt"]
            .as_str()
            .expect("prompt")
            .contains("unavailable in the retained event log")
    );
    assert!(!path.exists(), "preview created the missing source log");

    let missing = serde_json::json!({"agent_id": AgentId::generate(), "dry_run": true});
    assert_eq!(
        handoff_rpc(&h, missing)
            .await
            .expect_err("unknown agent")
            .code,
        dgc_protocol::rpc::NOT_FOUND
    );

    h.workspace.state = WorkspaceState::Archived;
    h.store
        .put_workspace(&h.workspace)
        .expect("archive workspace");
    assert_eq!(
        handoff_rpc(&h, params.clone())
            .await
            .expect_err("archived workspace")
            .code,
        dgc_protocol::rpc::INVALID_STATE
    );

    h.workspace.state = WorkspaceState::Active;
    h.store
        .put_workspace(&h.workspace)
        .expect("activate workspace");
    h.manager.archive(&id).await.expect("archive agent");
    assert_eq!(
        handoff_rpc(&h, params)
            .await
            .expect_err("archived source")
            .code,
        dgc_protocol::rpc::INVALID_STATE
    );
}

#[tokio::test]
async fn handoff_refuses_changed_preview_without_creating_an_agent() {
    let h = Harness::new(DEFAULT);
    git_in_workspace(&h, &["init", "-q"]);
    let id = stored_handoff_source(&h, ProviderKind::Claude, vec![instruction("first")]);
    let preview = handoff_preview(&h, &id).await;
    std::fs::write(h.workspace.path.join("new.txt"), "").expect("change workspace");
    let result = handoff_rpc(
        &h,
        serde_json::json!({
            "agent_id": id, "dry_run": false, "expected_prompt": preview
        }),
    )
    .await
    .expect_err("changed preview");
    assert_eq!(result.code, dgc_protocol::rpc::INVALID_STATE);
    assert_eq!(result.message, "handoff_changed");
    assert_eq!(h.manager.list().expect("agents").len(), 1);
}

#[tokio::test]
async fn handoff_creates_with_the_exact_matching_preview() {
    let mut h = Harness::new(DEFAULT);
    let id = stored_handoff_source(&h, ProviderKind::Claude, vec![instruction("first")]);
    let preview = handoff_preview(&h, &id).await;
    let result = handoff_rpc(
        &h,
        serde_json::json!({
            "agent_id": id, "dry_run": false, "expected_prompt": preview
        }),
    )
    .await
    .expect("matching preview");
    let mut session = h.next_session().await;
    assert_eq!(result["prompt"], preview);
    assert_eq!(session.next_call().await, Call::SendUser(preview));
    h.manager.shutdown_all(WAIT).await;
}

#[tokio::test]
async fn handoff_keeps_last_answer_before_a_failed_or_limited_turn() {
    let h = Harness::new(DEFAULT);
    let id = stored_handoff_source(
        &h,
        ProviderKind::Claude,
        vec![
            instruction("initial"),
            EventKind::TurnStarted,
            answer("previous ", false),
            answer("real answer", true),
            EventKind::TurnCompleted { usage: None },
            instruction("next"),
            EventKind::TurnStarted,
            answer("old adapter 429 plan text", true),
            EventKind::RateLimited {
                detail: serde_json::json!({"reason": "http_429"}),
            },
            EventKind::TurnCompleted { usage: None },
            instruction("retry"),
            EventKind::TurnStarted,
            answer("partial failed answer", true),
            EventKind::Error {
                message: "synthetic plan error text".to_owned(),
            },
            EventKind::TurnCompleted { usage: None },
            instruction("retry again"),
            EventKind::TurnStarted,
            EventKind::RateLimited {
                detail: serde_json::json!({"reason": "http_429"}),
            },
        ],
    );
    let prompt = handoff_preview(&h, &id).await;
    assert!(
        prompt.contains("Last assistant answer\nprevious real answer"),
        "{prompt}"
    );
    for excluded in [
        "old adapter 429 plan text",
        "partial failed answer",
        "synthetic plan error text",
    ] {
        assert!(!prompt.contains(excluded), "{prompt}");
    }
}

#[tokio::test]
async fn handoff_keeps_a_completed_answer_after_a_late_failure() {
    let h = Harness::new(DEFAULT);
    for late_failure in [
        EventKind::RateLimited {
            detail: serde_json::json!({"reason": "rejected"}),
        },
        EventKind::Error {
            message: "late error".to_owned(),
        },
    ] {
        let id = stored_handoff_source(
            &h,
            ProviderKind::Claude,
            vec![
                instruction("initial"),
                EventKind::TurnStarted,
                answer("completed answer", true),
                EventKind::TurnCompleted { usage: None },
                late_failure,
                instruction("next"),
                EventKind::TurnStarted,
            ],
        );
        let prompt = handoff_preview(&h, &id).await;
        assert!(
            prompt.contains("Last assistant answer\ncompleted answer"),
            "{prompt}"
        );
    }
}

#[tokio::test]
async fn handoff_truncates_a_100_kib_first_instruction() {
    let h = Harness::new(DEFAULT);
    // Two ASCII characters before the multibyte text make the 16 KiB cutoff
    // fall inside a character.
    let initial = format!("ss{} unrepeated suffix", "界".repeat(100 * 1024 / 3));
    let id = stored_handoff_source(
        &h,
        ProviderKind::Claude,
        vec![
            instruction(initial),
            EventKind::TurnStarted,
            answer("answer", true),
        ],
    );
    let prompt = handoff_preview(&h, &id).await;
    let prefix = format!(
        "ss{}…(truncated; the full instruction is in the log below)",
        "界".repeat((16 * 1024 - 2) / 3)
    );
    assert!(
        prompt.contains(&prefix),
        "first instruction was not clipped at 16 KiB"
    );
    assert!(!prompt.contains("unrepeated suffix"));
    assert!(prompt.contains("Last assistant answer\nanswer"));
    assert!(prompt.len() <= 32 * 1024);
}

#[tokio::test]
async fn handoff_git_output_ignores_forced_color() {
    let h = Harness::new(DEFAULT);
    git_in_workspace(&h, &["init", "-q"]);
    for key in ["color.ui", "color.status", "color.diff"] {
        git_in_workspace(&h, &["config", key, "always"]);
    }
    std::fs::write(h.workspace.path.join("a.txt"), "before\n").expect("tracked file");
    git_in_workspace(&h, &["add", "a.txt"]);
    std::fs::write(h.workspace.path.join("a.txt"), "after\n").expect("changed file");
    let id = stored_handoff_source(&h, ProviderKind::Claude, vec![instruction("initial")]);
    let prompt = handoff_preview(&h, &id).await;
    assert!(prompt.contains("AM a.txt"), "{prompt}");
    assert!(prompt.contains("a.txt | 2 +-"), "{prompt}");
    assert!(
        !prompt.contains("[31m") && !prompt.contains("[32m") && !prompt.contains("[m"),
        "{prompt}"
    );
}

#[tokio::test]
async fn handoff_git_failures_name_the_command() {
    let h = Harness::new(DEFAULT);
    let id = stored_handoff_source(&h, ProviderKind::Claude, vec![instruction("initial")]);
    std::fs::remove_dir(&h.workspace.path).expect("remove workspace directory");
    let prompt = handoff_preview(&h, &id).await;
    assert!(
        prompt.contains("git status --short\n(git status unavailable:"),
        "{prompt}"
    );
    assert!(
        prompt.contains("git diff --stat\n(git diff unavailable:"),
        "{prompt}"
    );
}

#[tokio::test]
async fn handoff_reading_a_cold_log_does_not_block_unrelated_appends() {
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::mpsc::channel;
    use std::time::Instant;

    let h = Harness::new(DEFAULT);
    let workspace_id = h.workspace.id.clone();
    let source = stored_handoff_source(&h, ProviderKind::Claude, vec![instruction("initial")]);
    let fifo = h
        .paths
        .state_dir
        .join("agents")
        .join(source.as_str())
        .join("events.1.jsonl");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo")
            .success()
    );

    let manager = Arc::new(h.manager);
    let handoff_manager = Arc::clone(&manager);
    let (opened_signal, opened) = tokio::sync::oneshot::channel();
    let (ready, reader_is_waiting) = channel();
    let (release, released) = channel();
    let (heartbeat, tick) = channel();
    let writer = std::thread::spawn(move || {
        let deadline = Instant::now() + WAIT;
        let _writer = loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&fifo)
            {
                Ok(writer) => break writer,
                Err(err)
                    if err.raw_os_error() == Some(libc::ENXIO) && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(err) => panic!("open FIFO for handoff: {err}"),
            }
        };
        std::fs::remove_file(&fifo).expect("unlink FIFO after reader opened it");
        opened_signal.send(()).expect("notify handoff runtime");
        ready.send(()).expect("notify test");
        released
            .recv_timeout(WAIT)
            .expect("release the cold reader");
    });
    let preview = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(async {
                tokio::spawn(async move {
                    opened.await.expect("reader is blocked in file I/O");
                    heartbeat.send(()).expect("notify test");
                });
                dgc_daemon::handlers::agent_handoff(
                    &handoff_manager,
                    serde_json::json!({
                        "agent_id": source, "dry_run": true
                    }),
                )
                .await
            })
    });
    reader_is_waiting
        .recv_timeout(WAIT)
        .expect("cold reader opened the FIFO");
    let other_manager = Arc::clone(&manager);
    let (appended, mut append_result) = tokio::sync::oneshot::channel();
    let other_writer = tokio::task::spawn_blocking(move || {
        let result =
            tokio::runtime::Handle::current().block_on(other_manager.create(AgentCreateParams {
                workspace_id,
                title: "other agent".to_owned(),
                provider: Some(ProviderKind::Codex),
                profile: None,
                mode: None,
                model: None,
                initial_prompt: "unrelated append".to_owned(),
                labels: BTreeMap::new(),
                parent_id: None,
            }));
        appended.send(result).expect("report creation");
    });
    let did_append = timeout(Duration::from_millis(500), &mut append_result).await;
    let runtime_responsive = tick.recv_timeout(Duration::from_millis(500)).is_ok();
    release.send(()).expect("release writer");
    writer.join().expect("writer thread");
    preview.join().expect("preview thread").expect("preview");
    other_writer.await.expect("other writer");
    manager.shutdown_all(WAIT).await;
    let appended = matches!(did_append, Ok(Ok(Ok(_))));
    assert!(
        runtime_responsive && appended,
        "cold read blocked runtime: {}, blocked another append: {}",
        !runtime_responsive,
        !appended
    );
}
