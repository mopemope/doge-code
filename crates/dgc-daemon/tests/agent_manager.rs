//! AgentManager state machine driven through an in-test scripted provider.
//!
//! The test plays the provider: every started session is handed to the test,
//! which decides when the "provider" emits events and observes which session
//! methods the manager called.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use dgc_daemon::agent::{AgentError, AgentManager};
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
            let (events_tx, events_rx) = mpsc::channel(4096);
            let (calls_tx, calls_rx) = mpsc::unbounded_channel();
            self.sessions
                .send(SessionHandle {
                    spec,
                    events: events_tx,
                    calls: calls_rx,
                })
                .map_err(|_| ProviderError::Spawn("test gone".to_owned()))?;
            Ok(Box::new(ScriptedSession {
                steer_supported: self.steer_supported,
                events: events_rx,
                calls: calls_tx,
            }) as Box<dyn ProviderSession>)
        })
    }
}

struct ScriptedSession {
    steer_supported: bool,
    events: mpsc::Receiver<ProviderEvent>,
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
        Box::pin(async move { self.record(Call::Interrupt) })
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
}

const DEFAULT: Options = Options {
    steer_supported: true,
    fail_start: false,
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
                provider: ProviderKind::Codex,
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
        provider,
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
        fail_start: false,
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
        steer_supported: true,
        fail_start: true,
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
