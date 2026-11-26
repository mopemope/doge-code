//! ClaudeAdapter driven against `dgc-fake-provider` replaying scenarios that
//! were converted from real Claude Code 2.1.332 recordings
//! (`tests/fixtures/claude/scenarios/`; synthetic ones say so in their header).
//! The fake checks every line the
//! adapter writes and its argv; a mismatch makes it exit 3 (timeout: 4), which
//! the tests see as an unexpected `Exited` carrying the fake's stderr.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dgc_daemon::agent::AgentManager;
use dgc_daemon::config::DaemonConfig;
use dgc_daemon::paths::DaemonPaths;
use dgc_daemon::provider::claude::ClaudeAdapter;
use dgc_daemon::provider::{AgentSpec, ProviderAdapter, ProviderEvent, ProviderSession};
use dgc_daemon::store::Store;
use dgc_protocol::event::EventKind;
use dgc_protocol::ids::{AgentId, ProjectId, WorkspaceId};
use dgc_protocol::methods::{AgentCreateParams, AgentEventsParams};
use dgc_protocol::model::{
    Agent, AgentState, Decision, Isolation, PermissionKind, ProviderKind, Workspace, WorkspaceState,
};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};
use tokio::time::timeout;

/// Longer than the fake's own 10 s wait, so a stuck exchange shows up as the
/// fake's timeout exit (with its stderr) rather than a bare test timeout.
const EVENT_WAIT: Duration = Duration::from_secs(15);
const RESUME_SESSION: &str = "4fb8c693-c0ce-4b19-b4d7-6af541c81886";

fn scenario(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude/scenarios")
        .join(format!("{name}.jsonl"))
}

fn fake_adapter(name: &str) -> ClaudeAdapter {
    ClaudeAdapter::with_program(
        PathBuf::from(env!("CARGO_BIN_EXE_dgc-fake-provider")),
        vec![scenario(name).display().to_string()],
    )
}

fn spec(dir: &TempDir, model: Option<&str>, resume: Option<&str>) -> AgentSpec {
    AgentSpec {
        agent_id: AgentId::generate(),
        cwd: dir.path().to_path_buf(),
        model: model.map(str::to_owned),
        mode: None,
        resume_session_id: resume.map(str::to_owned),
    }
}

async fn start(adapter: &ClaudeAdapter, spec: AgentSpec) -> Box<dyn ProviderSession> {
    match timeout(EVENT_WAIT, adapter.start(spec)).await {
        Ok(Ok(session)) => session,
        Ok(Err(err)) => panic!("start failed: {err}"),
        Err(_) => panic!("start did not finish"),
    }
}

/// The next event; an `Exited` here means the fake rejected the exchange.
async fn next_event(session: &mut Box<dyn ProviderSession>) -> ProviderEvent {
    let event = timeout(EVENT_WAIT, session.events().recv())
        .await
        .expect("timed out waiting for a provider event")
        .expect("event channel closed");
    if let ProviderEvent::Exited {
        success,
        stderr_tail,
    } = &event
    {
        panic!("fake provider exited (success: {success}); stderr:\n{stderr_tail}");
    }
    event
}

/// Events up to and including the next `TurnCompleted`.
async fn turn_events(session: &mut Box<dyn ProviderSession>) -> Vec<ProviderEvent> {
    let mut events = Vec::new();
    loop {
        let event = next_event(session).await;
        let done = matches!(event, ProviderEvent::TurnCompleted { .. });
        events.push(event);
        if done {
            return events;
        }
    }
}

/// Events up to and including the next `PermissionRequested`.
async fn until_permission(session: &mut Box<dyn ProviderSession>) -> Vec<ProviderEvent> {
    let mut events = Vec::new();
    loop {
        let event = next_event(session).await;
        let done = matches!(event, ProviderEvent::PermissionRequested { .. });
        events.push(event);
        if done {
            return events;
        }
    }
}

fn session_started(id: &str) -> ProviderEvent {
    ProviderEvent::SessionStarted {
        session_id: id.to_owned(),
    }
}

fn text(text: &str) -> ProviderEvent {
    ProviderEvent::Event(EventKind::AssistantText {
        text: text.to_owned(),
        final_: true,
    })
}

fn usage_output_tokens(event: &ProviderEvent) -> u64 {
    match event {
        ProviderEvent::TurnCompleted { usage: Some(usage) } => usage["output_tokens"]
            .as_u64()
            .expect("usage.output_tokens"),
        other => panic!("expected TurnCompleted with usage, got {other:?}"),
    }
}

/// Shut down and check that no `Exited` follows.
async fn shutdown_quietly(mut session: Box<dyn ProviderSession>) {
    timeout(EVENT_WAIT, session.shutdown())
        .await
        .expect("shutdown did not finish")
        .expect("shutdown");
    if let Ok(Some(event)) = timeout(Duration::from_millis(200), session.events().recv()).await {
        assert!(
            !matches!(event, ProviderEvent::Exited { .. }),
            "Exited after shutdown: {event:?}"
        );
    }
}

#[test]
fn claude_validate_mode() {
    let adapter = ClaudeAdapter::new(PathBuf::from("claude"));
    assert_eq!(adapter.kind(), ProviderKind::Claude);
    assert_eq!(adapter.validate_mode(None), Ok(false));
    for mode in ["bypassPermissions", "auto", "acceptEdits"] {
        assert_eq!(adapter.validate_mode(Some(mode)), Ok(true), "{mode}");
    }
    for mode in ["manual", "dontAsk", "plan"] {
        assert_eq!(adapter.validate_mode(Some(mode)), Ok(false), "{mode}");
    }
    for mode in ["default", "", "BYPASSPERMISSIONS", "workspace-write"] {
        assert!(adapter.validate_mode(Some(mode)).is_err(), "{mode}");
    }
}

#[tokio::test]
async fn claude_simple_turn() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("simple_turn");
    let mut session = start(&adapter, spec(&dir, Some("haiku"), None)).await;

    // Steering is unsupported and must not write anything (the fake would
    // reject an unexpected line).
    assert!(!session.steer("ignored").await.expect("steer"));
    session
        .send_user("Reply with exactly the word: pong")
        .await
        .expect("send_user");
    let events = turn_events(&mut session).await;
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(
        events[0],
        session_started("a8b19be3-3df1-4ce8-8371-d551a1ec21dd")
    );
    assert_eq!(events[1], text("pong"));
    assert_eq!(usage_output_tokens(&events[2]), 4);
    shutdown_quietly(session).await;
}

async fn rate_limit_turns(name: &str, count: usize) -> Vec<Vec<ProviderEvent>> {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter(name);
    let mut session = start(&adapter, spec(&dir, Some("haiku"), None)).await;
    let mut turns = Vec::new();
    for _ in 0..count {
        session
            .send_user("Reply with exactly the word: pong")
            .await
            .expect("send_user");
        turns.push(turn_events(&mut session).await);
    }
    shutdown_quietly(session).await;
    turns
}

fn rate_limited_details(events: &[ProviderEvent]) -> Vec<&Value> {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderEvent::Event(EventKind::RateLimited { detail }) => Some(detail),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn claude_rate_limit_rejected_status_emits_rate_limited() {
    let turns = rate_limit_turns("rate_limited_status", 2).await;
    assert_eq!(
        rate_limited_details(&turns[0]).first().copied(),
        Some(&json!({"provider": "claude", "reason": "rejected", "resets_at": 1700000000}))
    );
    assert_eq!(
        rate_limited_details(&turns[1]).first().copied(),
        Some(&json!({"provider": "claude", "reason": "allowed_warning"}))
    );
}

#[tokio::test]
async fn claude_429_result_emits_rate_limited() {
    let turns = rate_limit_turns("rate_limited_429", 2).await;
    for turn in &turns {
        assert_eq!(
            rate_limited_details(turn),
            [&json!({"provider": "claude", "reason": "http_429"})]
        );
    }
    assert!(
        turns[0]
            .iter()
            .any(|event| matches!(event, ProviderEvent::Event(EventKind::Error { .. }))),
        "{:?}",
        turns[0]
    );
}

#[tokio::test]
async fn claude_allowed_status_is_ignored() {
    let turns = rate_limit_turns("simple_turn", 1).await;
    assert!(rate_limited_details(&turns[0]).is_empty(), "{turns:?}");
}

#[tokio::test]
async fn rate_limited_is_emitted_once_per_turn() {
    let turns = rate_limit_turns("rate_limited_status", 2).await;
    for turn in &turns {
        assert_eq!(rate_limited_details(turn).len(), 1, "{turn:?}");
    }
}

#[tokio::test]
async fn claude_permission_allow() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("permission_allow");
    let mut session = start(&adapter, spec(&dir, Some("haiku"), None)).await;
    session
        .send_user(
            "Use the Bash tool to run exactly this command: touch hello.txt\n\
             Do not use any other tool. If the command is denied, reply with the single word \
             denied and stop. Otherwise reply with the single word done.",
        )
        .await
        .expect("send_user");

    let events = until_permission(&mut session).await;
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(
        events[0],
        session_started("0db50f53-6482-4f6b-81c4-f6d70eb258b3")
    );
    assert_eq!(
        events[1],
        ProviderEvent::Event(EventKind::tool_call("Bash", "touch hello.txt"))
    );
    let ProviderEvent::PermissionRequested {
        provider_request_id,
        kind,
        summary,
        raw,
    } = &events[2]
    else {
        panic!("expected PermissionRequested, got {:?}", events[2]);
    };
    assert_eq!(provider_request_id, "fea6759a-ad91-49ba-b9b6-b55ac5acaf7c");
    assert_eq!(*kind, PermissionKind::Command);
    assert!(summary.contains("touch hello.txt"), "{summary}");
    assert_eq!(raw["tool_name"], "Bash");

    // The scenario expects `allow` with the original input as updatedInput.
    session
        .respond_permission(provider_request_id, Decision::Allow)
        .await
        .expect("respond");
    let events = turn_events(&mut session).await;
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(
        events[0],
        ProviderEvent::Event(EventKind::tool_result(
            "(Bash completed with no output)",
            false
        ))
    );
    assert_eq!(events[1], text("done"));
    assert_eq!(usage_output_tokens(&events[2]), 209);
    shutdown_quietly(session).await;
}

#[tokio::test]
async fn claude_permission_deny() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("permission_deny");
    let mut session = start(&adapter, spec(&dir, Some("haiku"), None)).await;
    session
        .send_user(
            "Use the Bash tool to run exactly this command: touch hello.txt\n\
             Do not use any other tool. If the command is denied, reply with the single word \
             denied and stop. Otherwise reply with the single word done.",
        )
        .await
        .expect("send_user");

    let events = until_permission(&mut session).await;
    let Some(ProviderEvent::PermissionRequested {
        provider_request_id,
        kind,
        ..
    }) = events.last()
    else {
        panic!("expected PermissionRequested, got {events:?}");
    };
    assert_eq!(*kind, PermissionKind::Command);
    session
        .respond_permission(provider_request_id, Decision::Deny)
        .await
        .expect("respond");
    let events = turn_events(&mut session).await;
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(
        events[0],
        ProviderEvent::Event(EventKind::tool_result("Denied by dgc user", true))
    );
    assert_eq!(events[1], text("denied"));
    assert!(matches!(events[2], ProviderEvent::TurnCompleted { .. }));
    // An unknown request id is a protocol error, and nothing is written.
    assert!(
        session
            .respond_permission("no-such-request", Decision::Allow)
            .await
            .is_err()
    );
    shutdown_quietly(session).await;
}

#[tokio::test]
async fn claude_interrupt() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("interrupt");
    let mut session = start(&adapter, spec(&dir, Some("haiku"), None)).await;
    session
        .send_user("Write a 1500-word story about a dog who learns to sail. Plain text, no tools.")
        .await
        .expect("send_user");
    assert_eq!(
        next_event(&mut session).await,
        session_started("1590b64f-8c2c-4416-b4af-ea380b4d23b9")
    );
    timeout(EVENT_WAIT, session.interrupt())
        .await
        .expect("interrupt did not finish")
        .expect("interrupt");

    // The interrupted turn's `result` (error_during_execution) arrives after
    // the interrupt returned and must be swallowed: the only TurnCompleted is
    // the next turn's, and no Error event appears.
    session
        .send_user("Reply with exactly the word: ok")
        .await
        .expect("send_user");
    let events = turn_events(&mut session).await;
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(
        events[0],
        session_started("1590b64f-8c2c-4416-b4af-ea380b4d23b9")
    );
    assert_eq!(events[1], text("ok"));
    assert_eq!(usage_output_tokens(&events[2]), 162);
    shutdown_quietly(session).await;
}

/// Synthetic: the CLI answers the interrupt but the interrupted (started)
/// turn never sends a `result`. The next turn's `system/init` closes the
/// swallow window, so its `result` still completes the turn.
#[tokio::test]
async fn claude_interrupt_without_result_does_not_swallow_next_turn() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("interrupt_without_result");
    let mut session = start(&adapter, spec(&dir, Some("haiku"), None)).await;
    session
        .send_user("Write a 1500-word story about a dog who learns to sail. Plain text, no tools.")
        .await
        .expect("send_user");
    assert_eq!(
        next_event(&mut session).await,
        session_started("1590b64f-8c2c-4416-b4af-ea380b4d23b9")
    );
    timeout(EVENT_WAIT, session.interrupt())
        .await
        .expect("interrupt did not finish")
        .expect("interrupt");
    session
        .send_user("Reply with exactly the word: ok")
        .await
        .expect("send_user");
    let events = turn_events(&mut session).await;
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(events[1], text("ok"));
    assert_eq!(usage_output_tokens(&events[2]), 162);
    shutdown_quietly(session).await;
}

/// Synthetic: interrupting after the turn's `result` sends nothing (the
/// scenario expects the next user message instead) and succeeds.
#[tokio::test]
async fn claude_interrupt_when_idle_sends_nothing() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("interrupt_when_idle");
    let mut session = start(&adapter, spec(&dir, Some("haiku"), None)).await;
    session
        .send_user("Reply with exactly the word: pong")
        .await
        .expect("send_user");
    turn_events(&mut session).await;
    timeout(EVENT_WAIT, session.interrupt())
        .await
        .expect("interrupt did not finish")
        .expect("interrupt");
    session
        .send_user("Reply with exactly the word: pong")
        .await
        .expect("send_user");
    // The fake answers the expected user line with system/init; a stray
    // interrupt request would have made it exit 3 instead.
    assert_eq!(
        next_event(&mut session).await,
        session_started("a8b19be3-3df1-4ce8-8371-d551a1ec21dd")
    );
    shutdown_quietly(session).await;
}

/// Synthetic: a permission request outstanding at interrupt is denied to the
/// CLI before the interrupt is sent, and the interrupted turn's result is
/// swallowed.
#[tokio::test]
async fn claude_interrupt_denies_pending_permission() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("interrupt_denies_pending_permission");
    let mut session = start(&adapter, spec(&dir, Some("haiku"), None)).await;
    session
        .send_user(
            "Use the Bash tool to run exactly this command: touch hello.txt\n\
             Do not use any other tool. If the command is denied, reply with the single word \
             denied and stop. Otherwise reply with the single word done.",
        )
        .await
        .expect("send_user");
    let events = until_permission(&mut session).await;
    assert!(
        matches!(
            events.last(),
            Some(ProviderEvent::PermissionRequested { .. })
        ),
        "{events:?}"
    );
    timeout(EVENT_WAIT, session.interrupt())
        .await
        .expect("interrupt did not finish")
        .expect("interrupt");
    // The request was cancelled: answering it now is an error.
    assert!(
        session
            .respond_permission("fea6759a-ad91-49ba-b9b6-b55ac5acaf7c", Decision::Allow)
            .await
            .is_err()
    );
    session
        .send_user("Reply with exactly the word: ok")
        .await
        .expect("send_user");
    let events = turn_events(&mut session).await;
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(
        events[0],
        session_started("0db50f53-6482-4f6b-81c4-f6d70eb258b3")
    );
    assert_eq!(events[1], text("ok"));
    assert_eq!(usage_output_tokens(&events[2]), 162);
    shutdown_quietly(session).await;
}

#[tokio::test]
async fn claude_resume_passes_session_id() {
    let dir = tempdir().expect("tempdir");
    // The scenario's expect_args require `--resume <id>`.
    let adapter = fake_adapter("resume");
    let mut session = start(&adapter, spec(&dir, Some("haiku"), Some(RESUME_SESSION))).await;
    session
        .send_user("What was the code word? Reply with just the word.")
        .await
        .expect("send_user");
    let events = turn_events(&mut session).await;
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(events[0], session_started(RESUME_SESSION));
    assert_eq!(events[1], text("BLUEFISH"));
    assert!(matches!(events[2], ProviderEvent::TurnCompleted { .. }));
    shutdown_quietly(session).await;
}

#[tokio::test]
async fn claude_invalid_model_reports_error_and_completes_turn() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("error");
    let mut session = start(&adapter, spec(&dir, Some("no-such-model-dgc"), None)).await;
    session
        .send_user("Reply with exactly the word: pong")
        .await
        .expect("send_user");
    let events = turn_events(&mut session).await;
    assert_eq!(events.len(), 4, "{events:?}");
    assert_eq!(
        events[0],
        session_started("e2d34243-cafe-49fd-a253-96503054c673")
    );
    assert!(matches!(
        events[1],
        ProviderEvent::Event(EventKind::AssistantText { .. })
    ));
    let ProviderEvent::Event(EventKind::Error { message }) = &events[2] else {
        panic!("expected Error, got {:?}", events[2]);
    };
    assert!(
        message.contains("issue with the selected model"),
        "{message}"
    );
    assert!(matches!(events[3], ProviderEvent::TurnCompleted { .. }));
    shutdown_quietly(session).await;
}

#[tokio::test]
async fn claude_unexpected_exit_emits_exited_once() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("crash_mid_turn");
    let mut session = start(&adapter, spec(&dir, Some("haiku"), None)).await;
    session
        .send_user("Reply with exactly the word: pong")
        .await
        .expect("send_user");
    assert_eq!(
        next_event(&mut session).await,
        session_started("a8b19be3-3df1-4ce8-8371-d551a1ec21dd")
    );
    let event = timeout(EVENT_WAIT, session.events().recv())
        .await
        .expect("timed out waiting for Exited")
        .expect("event channel closed without Exited");
    let ProviderEvent::Exited {
        success,
        stderr_tail,
    } = event
    else {
        panic!("expected Exited, got {event:?}");
    };
    assert!(!success);
    assert!(stderr_tail.contains("synthetic crash"), "{stderr_tail}");
    // Exactly once: the channel then closes.
    let rest = timeout(EVENT_WAIT, session.events().recv())
        .await
        .expect("event channel stayed open after Exited");
    assert!(rest.is_none(), "{rest:?}");
}

/// AgentManager with only the Claude adapter, and one active workspace.
struct Harness {
    _dir: TempDir,
    manager: AgentManager,
    workspace: Workspace,
}

impl Harness {
    fn new(adapter: ClaudeAdapter) -> Self {
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
        let adapter: Arc<dyn ProviderAdapter> = Arc::new(adapter);
        let mut adapters = HashMap::new();
        adapters.insert(ProviderKind::Claude, adapter);
        let manager = AgentManager::new(store, paths, DaemonConfig::default(), adapters);
        Self {
            _dir: dir,
            manager,
            workspace,
        }
    }

    async fn create(&self, model: Option<&str>, prompt: &str) -> Agent {
        self.manager
            .create(AgentCreateParams {
                workspace_id: self.workspace.id.clone(),
                title: "claude".to_owned(),
                provider: ProviderKind::Claude,
                model: model.map(str::to_owned),
                mode: None,
                initial_prompt: prompt.to_owned(),
                labels: BTreeMap::new(),
                parent_id: None,
            })
            .await
            .expect("create")
    }

    async fn wait_state(&self, id: &AgentId, state: AgentState, within: Duration) -> Agent {
        let poll = async {
            loop {
                let agent = self.manager.get(id).expect("get");
                if agent.state == state {
                    return agent;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        match timeout(within, poll).await {
            Ok(agent) => agent,
            Err(_) => {
                let agent = self.manager.get(id).expect("get");
                panic!(
                    "agent never reached {state:?}: {agent:?}\nevents: {:?}",
                    self.events(id)
                );
            }
        }
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

    fn error_messages(&self, id: &AgentId) -> Vec<String> {
        self.events(id)
            .into_iter()
            .filter_map(|kind| match kind {
                EventKind::Error { message } => Some(message),
                _ => None,
            })
            .collect()
    }
}

#[tokio::test]
async fn claude_manager_simple_turn() {
    let h = Harness::new(fake_adapter("simple_turn"));
    let agent = h
        .create(Some("haiku"), "Reply with exactly the word: pong")
        .await;
    let idle = h.wait_state(&agent.id, AgentState::Idle, EVENT_WAIT).await;
    assert_eq!(
        idle.provider_session_id.as_deref(),
        Some("a8b19be3-3df1-4ce8-8371-d551a1ec21dd")
    );
    assert_eq!(idle.last_error, None);
    let events = h.events(&agent.id);
    assert!(
        events.contains(&EventKind::AssistantText {
            text: "pong".to_owned(),
            final_: true,
        }),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|kind| matches!(kind, EventKind::TurnCompleted { .. })),
        "{events:?}"
    );
    assert!(h.error_messages(&agent.id).is_empty(), "{events:?}");

    let archived = timeout(EVENT_WAIT, h.manager.archive(&agent.id))
        .await
        .expect("archive did not finish")
        .expect("archive");
    assert_eq!(archived.state, AgentState::Archived);
}

#[tokio::test]
async fn claude_missing_binary_fails_agent() {
    let h = Harness::new(ClaudeAdapter::new(PathBuf::from(
        "/nonexistent/dgc-test/claude",
    )));
    let agent = h.create(None, "hello").await;
    let failed = h
        .wait_state(&agent.id, AgentState::Failed, Duration::from_secs(5))
        .await;
    let last_error = failed.last_error.expect("last_error");
    assert!(
        last_error.contains("failed to start provider"),
        "{last_error}"
    );
    assert!(!h.error_messages(&agent.id).is_empty());
}

#[tokio::test]
async fn claude_login_error_surfaces_stderr_tail() {
    let h = Harness::new(fake_adapter("login_error"));
    let agent = h.create(None, "hello").await;
    let failed = h
        .wait_state(&agent.id, AgentState::Failed, Duration::from_secs(5))
        .await;
    let last_error = failed.last_error.expect("last_error");
    assert!(last_error.contains("Not logged in"), "{last_error}");
    let errors = h.error_messages(&agent.id);
    assert!(
        errors.iter().any(|m| m.contains("Not logged in")),
        "{errors:?}"
    );
}
