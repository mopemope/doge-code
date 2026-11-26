//! CodexAdapter driven against `dgc-fake-provider` replaying scenarios that
//! were converted from real codex-cli 0.172.0 recordings
//! (`tests/fixtures/codex/scenarios/`; synthetic ones say so in their header).
//! The fake checks every line the adapter writes and its argv; a mismatch makes
//! it exit 3 (timeout: 4), which the tests see as an unexpected `Exited`
//! carrying the fake's stderr.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dgc_daemon::agent::AgentManager;
use dgc_daemon::config::DaemonConfig;
use dgc_daemon::paths::DaemonPaths;
use dgc_daemon::provider::codex::CodexAdapter;
use dgc_daemon::provider::{AgentSpec, ProviderAdapter, ProviderEvent, ProviderSession};
use dgc_daemon::store::Store;
use dgc_protocol::event::EventKind;
use dgc_protocol::ids::{AgentId, ProjectId, WorkspaceId};
use dgc_protocol::methods::{AgentCreateParams, AgentEventsParams, AgentSendParams};
use dgc_protocol::model::{
    Agent, AgentState, Decision, Isolation, PermissionKind, ProviderKind, SendBehavior,
    SendOutcome, Workspace, WorkspaceState,
};
use serde_json::Value;
use tempfile::{TempDir, tempdir};
use tokio::time::timeout;

/// Longer than the fake's own 10 s wait, so a stuck exchange shows up as the
/// fake's timeout exit (with its stderr) rather than a bare test timeout.
const EVENT_WAIT: Duration = Duration::from_secs(15);
const MODEL: &str = "gpt-6-luna";

fn scenario(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codex/scenarios")
        .join(format!("{name}.jsonl"))
}

fn fake_adapter(name: &str) -> CodexAdapter {
    CodexAdapter::with_program(
        PathBuf::from(env!("CARGO_BIN_EXE_dgc-fake-provider")),
        vec![scenario(name).display().to_string()],
    )
}

fn spec(dir: &TempDir, mode: Option<&str>, resume: Option<&str>) -> AgentSpec {
    AgentSpec {
        agent_id: AgentId::generate(),
        cwd: dir.path().to_path_buf(),
        model: Some(MODEL.to_owned()),
        mode: mode.map(str::to_owned),
        resume_session_id: resume.map(str::to_owned),
    }
}

async fn start(adapter: &CodexAdapter, spec: AgentSpec) -> Box<dyn ProviderSession> {
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

/// Events up to and including the first one matching `done`.
async fn events_until(
    session: &mut Box<dyn ProviderSession>,
    done: impl Fn(&ProviderEvent) -> bool,
) -> Vec<ProviderEvent> {
    let mut events = Vec::new();
    loop {
        let event = next_event(session).await;
        let last = done(&event);
        events.push(event);
        if last {
            return events;
        }
    }
}

/// Events up to and including the next `TurnCompleted`.
async fn turn_events(session: &mut Box<dyn ProviderSession>) -> Vec<ProviderEvent> {
    events_until(session, |e| {
        matches!(e, ProviderEvent::TurnCompleted { .. })
    })
    .await
}

/// Events up to and including the next `PermissionRequested`.
async fn until_permission(session: &mut Box<dyn ProviderSession>) -> Vec<ProviderEvent> {
    events_until(session, |e| {
        matches!(e, ProviderEvent::PermissionRequested { .. })
    })
    .await
}

fn session_started(id: &str) -> ProviderEvent {
    ProviderEvent::SessionStarted {
        session_id: id.to_owned(),
    }
}

fn text(text: &str, final_: bool) -> ProviderEvent {
    ProviderEvent::Event(EventKind::AssistantText {
        text: text.to_owned(),
        final_,
    })
}

/// `usage.total.totalTokens` of a `TurnCompleted`.
fn usage_total_tokens(event: &ProviderEvent) -> u64 {
    match event {
        ProviderEvent::TurnCompleted { usage: Some(usage) } => usage["total"]["totalTokens"]
            .as_u64()
            .expect("usage.total.totalTokens"),
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
fn codex_validate_mode() {
    let adapter = CodexAdapter::new(PathBuf::from("codex"));
    assert_eq!(adapter.kind(), ProviderKind::Codex);
    assert_eq!(adapter.validate_mode(None), Ok(false));
    for mode in ["read-only", "workspace-write", "untrusted"] {
        assert_eq!(adapter.validate_mode(Some(mode)), Ok(false), "{mode}");
    }
    assert_eq!(adapter.validate_mode(Some("full-access")), Ok(true));
    for mode in [
        "",
        "never",
        "danger-full-access",
        "bypassPermissions",
        "READ-ONLY",
    ] {
        assert!(adapter.validate_mode(Some(mode)).is_err(), "{mode}");
    }
}

const SIMPLE_THREAD: &str = "01a211f1-bc33-7f61-8c4c-87addf26aa23";
const PONG: &str = "Reply with exactly the word: pong";

#[tokio::test]
async fn codex_simple_turn() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("simple_turn");
    let mut session = start(&adapter, spec(&dir, None, None)).await;
    session.send_user(PONG).await.expect("send_user");
    let events = turn_events(&mut session).await;
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(events[0], session_started(SIMPLE_THREAD));
    assert_eq!(events[1], text("pong", true));
    assert_eq!(usage_total_tokens(&events[2]), 23806);
    shutdown_quietly(session).await;
}

/// Routine `account/rateLimits/updated` notifications (sent with every model
/// reply, `rateLimitReachedType: null`) are not a reached limit: no
/// `RateLimited` event comes out of a turn that carried one.
#[tokio::test]
async fn codex_rate_limit_event_routine_update_is_dropped() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("simple_turn");
    let mut session = start(&adapter, spec(&dir, None, None)).await;
    session.send_user(PONG).await.expect("send_user");
    let events = turn_events(&mut session).await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ProviderEvent::Event(EventKind::RateLimited { .. }))),
        "{events:?}"
    );
    shutdown_quietly(session).await;
}

/// With no turn running, steer returns false and neither steer nor interrupt
/// writes anything: the fake expects turn/start next and would reject a
/// stray turn/steer or turn/interrupt.
#[tokio::test]
async fn codex_steer_without_active_turn_returns_false() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("simple_turn");
    let mut session = start(&adapter, spec(&dir, None, None)).await;
    assert!(!session.steer("ignored").await.expect("steer"));
    timeout(EVENT_WAIT, session.interrupt())
        .await
        .expect("interrupt did not finish")
        .expect("interrupt");
    session.send_user(PONG).await.expect("send_user");
    let events = turn_events(&mut session).await;
    assert_eq!(events[1], text("pong", true), "{events:?}");
    // The turn has ended: steering is refused again.
    assert!(!session.steer("ignored").await.expect("steer"));
    shutdown_quietly(session).await;
}

const TOUCH: &str = "Run the shell command `touch hello.txt` in the current directory, then reply with the single word done.";

#[tokio::test]
async fn codex_command_approval_accept() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("command_approval_accept");
    let mut session = start(&adapter, spec(&dir, Some("untrusted"), None)).await;
    session.send_user(TOUCH).await.expect("send_user");

    let events = until_permission(&mut session).await;
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(
        events[0],
        session_started("01a211f2-3872-7012-b4c1-2f886e8c2d11")
    );
    assert_eq!(
        events[1],
        ProviderEvent::Event(EventKind::tool_call(
            "commandExecution",
            "/usr/bin/bash -lc 'touch hello.txt'"
        ))
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
    assert_eq!(provider_request_id, "0");
    assert_eq!(*kind, PermissionKind::Command);
    assert!(summary.contains("touch hello.txt"), "{summary}");
    assert_eq!(raw["itemId"], "exec-7acfc6ea-b6be-498a-873b-32294f48186e");

    // The scenario expects {"id":0,"result":{"decision":"accept"}}.
    session
        .respond_permission(provider_request_id, Decision::Allow)
        .await
        .expect("respond");
    let events = turn_events(&mut session).await;
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(
        events[0],
        ProviderEvent::Event(EventKind::tool_result("completed (exit 0)", false))
    );
    assert_eq!(events[1], text("done", true));
    assert_eq!(usage_total_tokens(&events[2]), 42991);
    shutdown_quietly(session).await;
}

#[tokio::test]
async fn codex_file_change_decline() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("file_change_decline");
    let mut session = start(&adapter, spec(&dir, Some("read-only"), None)).await;
    session
        .send_user(
            "Use your file editing tool (apply_patch) to create a file named hello.txt \
             containing the text hi. Do not use shell commands. If the change is declined, \
             reply with the single word declined and stop.",
        )
        .await
        .expect("send_user");

    let events = until_permission(&mut session).await;
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(
        events[1],
        ProviderEvent::Event(EventKind::tool_call(
            "fileChange",
            "add /WORKSPACE/hello.txt"
        ))
    );
    let ProviderEvent::PermissionRequested {
        provider_request_id,
        kind,
        summary,
        ..
    } = &events[2]
    else {
        panic!("expected PermissionRequested, got {:?}", events[2]);
    };
    assert_eq!(*kind, PermissionKind::FileChange);
    // The diff is not in the request; the summary comes from item/started.
    assert!(summary.contains("/WORKSPACE/hello.txt"), "{summary}");

    // No availableDecisions on file changes: Deny → `decline`.
    session
        .respond_permission(provider_request_id, Decision::Deny)
        .await
        .expect("respond");
    let events = turn_events(&mut session).await;
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(
        events[0],
        ProviderEvent::Event(EventKind::tool_result("declined", true))
    );
    assert_eq!(events[1], text("declined", true));
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

const STORY: &str = "Write a 1500-word story about a dog who learns to sail. Plain text, no tools.";
const STORY_START: &str = "Mara had never been farther from shore than the end of the pier.";

#[tokio::test]
async fn codex_interrupt_uses_turn_id() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("interrupt");
    let mut session = start(&adapter, spec(&dir, None, None)).await;
    session.send_user(STORY).await.expect("send_user");
    // The scenario expects turn/interrupt with the recorded turn id.
    timeout(EVENT_WAIT, session.interrupt())
        .await
        .expect("interrupt did not finish")
        .expect("interrupt");

    // The interrupted turn's turn/completed (status interrupted) arrives after
    // the response and is dropped. The partial message never completed and is
    // flushed as non-final text when the interrupt is answered.
    let mut events = Vec::new();
    while let Ok(Some(event)) = timeout(Duration::from_millis(500), session.events().recv()).await {
        events.push(event);
    }
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(
        events[0],
        session_started("01a211f2-a71f-75e3-8a25-56d053ab0126")
    );
    let ProviderEvent::Event(EventKind::AssistantText { text, final_ }) = &events[1] else {
        panic!("expected AssistantText, got {:?}", events[1]);
    };
    assert!(!final_);
    assert!(text.starts_with(STORY_START), "{text}");
    assert_eq!(text.len(), 332);
    shutdown_quietly(session).await;
}

/// Synthetic: the interrupted turn's turn/completed arrives only after the
/// next turn has started. It is matched by turn id, so it is dropped while
/// the next turn's turn/completed is delivered.
#[tokio::test]
async fn codex_interrupt_suppresses_only_that_turn() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("interrupt_then_next_turn");
    let mut session = start(&adapter, spec(&dir, None, None)).await;
    session.send_user(STORY).await.expect("send_user");
    timeout(EVENT_WAIT, session.interrupt())
        .await
        .expect("interrupt did not finish")
        .expect("interrupt");
    session.send_user(PONG).await.expect("send_user");
    let events = turn_events(&mut session).await;
    let completed: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, ProviderEvent::TurnCompleted { .. }))
        .collect();
    assert_eq!(completed.len(), 1, "{events:?}");
    assert_eq!(usage_total_tokens(completed[0]), 23806);
    assert_eq!(events[events.len() - 2], text("pong", true), "{events:?}");
    shutdown_quietly(session).await;
}

/// Synthetic: an approval pending at interrupt is answered `cancel` before
/// turn/interrupt is sent, counts as cancelled (answering it later fails),
/// and the interrupted turn's turn/completed is dropped.
#[tokio::test]
async fn codex_interrupt_cancels_pending_approval() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("interrupt_cancels_pending_approval");
    let mut session = start(&adapter, spec(&dir, Some("untrusted"), None)).await;
    session.send_user(TOUCH).await.expect("send_user");
    let events = until_permission(&mut session).await;
    let Some(ProviderEvent::PermissionRequested {
        provider_request_id,
        ..
    }) = events.last()
    else {
        panic!("expected PermissionRequested, got {events:?}");
    };
    let request_id = provider_request_id.clone();
    timeout(EVENT_WAIT, session.interrupt())
        .await
        .expect("interrupt did not finish")
        .expect("interrupt");
    assert!(
        session
            .respond_permission(&request_id, Decision::Allow)
            .await
            .is_err()
    );
    session.send_user(PONG).await.expect("send_user");
    let events = turn_events(&mut session).await;
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0], text("pong", true));
    assert_eq!(usage_total_tokens(&events[1]), 23806);
    shutdown_quietly(session).await;
}

#[tokio::test]
async fn codex_steer_uses_expected_turn_id() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("steer");
    let mut session = start(&adapter, spec(&dir, None, None)).await;
    session
        .send_user(
            "Write a numbered list of 40 different fruits, one per line. Plain text, no tools.",
        )
        .await
        .expect("send_user");
    // The scenario expects turn/steer with expectedTurnId = the running turn.
    let steered = timeout(
        EVENT_WAIT,
        session.steer(
            "Change of plan: stop the list now and just reply with the single word steered.",
        ),
    )
    .await
    .expect("steer did not finish")
    .expect("steer");
    assert!(steered);

    // The steered-away message never gets item/completed: it is flushed as
    // non-final text when the next message starts, and does not leak into it.
    let events = turn_events(&mut session).await;
    assert_eq!(events.len(), 4, "{events:?}");
    assert_eq!(
        events[0],
        session_started("01a211f2-d8ae-7202-9b8f-f918f0c86ea6")
    );
    assert_eq!(
        events[1],
        text("1. Apple\n2. Apricot\n3. Avocado\n4. Banana", false)
    );
    assert_eq!(events[2], text("steered", true));
    assert_eq!(usage_total_tokens(&events[3]), 47692);
    shutdown_quietly(session).await;
}

const STEER_TEXT: &str =
    "Change of plan: stop the list now and just reply with the single word steered.";

/// Synthetic: Codex rejects the steer with -32600 "no active turn to steer"
/// before the turn's turn/completed has arrived. That is "cannot steer"
/// (`Ok(false)`), not a failure. The manager's fallback then interrupts (also
/// answered -32600) and starts the text as a new turn; the first turn's late
/// turn/completed is dropped.
#[tokio::test]
async fn codex_steer_rejected_on_ended_turn_returns_false() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("steer_rejected_turn_ended");
    let mut session = start(&adapter, spec(&dir, None, None)).await;
    session.send_user(PONG).await.expect("send_user");
    let steered = timeout(EVENT_WAIT, session.steer(STEER_TEXT))
        .await
        .expect("steer did not finish")
        .expect("steer");
    assert!(!steered);
    timeout(EVENT_WAIT, session.interrupt())
        .await
        .expect("interrupt did not finish")
        .expect("interrupt");
    session.send_user(STEER_TEXT).await.expect("send_user");
    let events = turn_events(&mut session).await;
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(events[0], session_started(SIMPLE_THREAD));
    assert_eq!(events[1], text("pong", true));
    assert_eq!(usage_total_tokens(&events[2]), 23806);
    shutdown_quietly(session).await;
}

#[tokio::test]
async fn codex_resume_calls_thread_resume() {
    let dir = tempdir().expect("tempdir");
    let thread = "01a211f3-13c3-7ca1-94f9-426849436da7";
    // The scenario expects thread/resume {threadId, model, excludeTurns: true,
    // approvalPolicy: "on-request", sandbox: "workspace-write"}: the default
    // mode's policy is sent even though no mode was given.
    let adapter = fake_adapter("resume");
    let mut session = start(&adapter, spec(&dir, None, Some(thread))).await;
    session
        .send_user("What was the code word? Reply with just the word.")
        .await
        .expect("send_user");
    let events = turn_events(&mut session).await;
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(events[0], session_started(thread));
    assert_eq!(events[1], text("BLUEFISH", true));
    let ProviderEvent::TurnCompleted { usage: Some(usage) } = &events[2] else {
        panic!("expected TurnCompleted with usage, got {:?}", events[2]);
    };
    assert_eq!(usage["last"]["outputTokens"], Value::from(7));
    shutdown_quietly(session).await;
}

/// Synthetic: a server request dgc does not handle is answered with a
/// JSON-RPC error -32601 (checked by the fake) and reported as an `Error`.
#[tokio::test]
async fn codex_unknown_server_request_is_rejected() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("unknown_server_request");
    let mut session = start(&adapter, spec(&dir, None, None)).await;
    session.send_user(PONG).await.expect("send_user");
    let events = turn_events(&mut session).await;
    assert_eq!(events.len(), 4, "{events:?}");
    let ProviderEvent::Event(EventKind::Error { message }) = &events[1] else {
        panic!("expected Error, got {:?}", events[1]);
    };
    assert!(message.contains("item/tool/requestUserInput"), "{message}");
    assert_eq!(events[2], text("pong", true));
    assert!(matches!(events[3], ProviderEvent::TurnCompleted { .. }));
    shutdown_quietly(session).await;
}

/// Synthetic (shapes from the 0.172.0 schema): `item/permissions/requestApproval`
/// becomes an `Other` permission request; `decision` is answered per the
/// response schema, which the scenario checks.
async fn permissions_request(scenario: &str, decision: Decision) {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter(scenario);
    let mut session = start(&adapter, spec(&dir, None, None)).await;
    session.send_user(PONG).await.expect("send_user");
    let events = until_permission(&mut session).await;
    let Some(ProviderEvent::PermissionRequested {
        provider_request_id,
        kind,
        summary,
        raw,
    }) = events.last()
    else {
        panic!("expected PermissionRequested, got {events:?}");
    };
    assert_eq!(provider_request_id, "0");
    assert_eq!(*kind, PermissionKind::Other);
    assert_eq!(
        summary,
        "permissions: network access, write /WORKSPACE/../outside, read tmpdir \
         (reason: need to fetch deps)"
    );
    assert_eq!(raw["permissions"]["network"]["enabled"], true);
    session
        .respond_permission(provider_request_id, decision)
        .await
        .expect("respond");
    let events = turn_events(&mut session).await;
    assert_eq!(events[0], text("pong", true), "{events:?}");
    assert!(matches!(events[1], ProviderEvent::TurnCompleted { .. }));
    shutdown_quietly(session).await;
}

#[tokio::test]
async fn codex_permissions_request_allow_for_session_grants_requested() {
    permissions_request("permissions_approval_session", Decision::AllowForSession).await;
}

#[tokio::test]
async fn codex_permissions_request_deny_grants_nothing() {
    permissions_request("permissions_approval_deny", Decision::Deny).await;
}

#[tokio::test]
async fn codex_failed_turn_reports_error_and_completes() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("error");
    let mut session = start(
        &adapter,
        AgentSpec {
            model: Some("no-such-model-dgc".to_owned()),
            ..spec(&dir, None, None)
        },
    )
    .await;
    session.send_user(PONG).await.expect("send_user");
    let events = turn_events(&mut session).await;
    assert_eq!(events.len(), 3, "{events:?}");
    let ProviderEvent::Event(EventKind::Error { message }) = &events[1] else {
        panic!("expected Error, got {:?}", events[1]);
    };
    assert!(message.contains("model is not supported"), "{message}");
    assert!(matches!(
        events[2],
        ProviderEvent::TurnCompleted { usage: None }
    ));
    shutdown_quietly(session).await;
}

#[tokio::test]
async fn codex_unexpected_exit_emits_exited_once() {
    let dir = tempdir().expect("tempdir");
    let adapter = fake_adapter("crash_mid_turn");
    let mut session = start(&adapter, spec(&dir, None, None)).await;
    session.send_user(PONG).await.expect("send_user");
    assert_eq!(
        next_event(&mut session).await,
        session_started(SIMPLE_THREAD)
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
    let rest = timeout(EVENT_WAIT, session.events().recv())
        .await
        .expect("event channel stayed open after Exited");
    assert!(rest.is_none(), "{rest:?}");
    // The session is gone: methods fail instead of hanging.
    assert!(session.send_user(PONG).await.is_err());
}

/// AgentManager with only the Codex adapter, and one active workspace.
struct Harness {
    _dir: TempDir,
    manager: AgentManager,
    workspace: Workspace,
}

impl Harness {
    fn new(adapter: CodexAdapter) -> Self {
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
        adapters.insert(ProviderKind::Codex, adapter);
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
                title: "codex".to_owned(),
                provider: ProviderKind::Codex,
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
async fn codex_manager_simple_turn() {
    let h = Harness::new(fake_adapter("simple_turn"));
    let agent = h.create(Some(MODEL), PONG).await;
    let idle = h.wait_state(&agent.id, AgentState::Idle, EVENT_WAIT).await;
    assert_eq!(idle.provider_session_id.as_deref(), Some(SIMPLE_THREAD));
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

/// Synthetic (see `codex_steer_rejected_on_ended_turn_returns_false`): a
/// steer send whose turn already ended falls back to interrupt-and-send and
/// the agent does not fail.
#[tokio::test]
async fn codex_manager_steer_on_ended_turn_sends_new_turn() {
    let h = Harness::new(fake_adapter("steer_rejected_turn_ended"));
    let agent = h.create(Some(MODEL), PONG).await;
    h.wait_state(&agent.id, AgentState::Running, EVENT_WAIT)
        .await;
    let outcome = timeout(
        EVENT_WAIT,
        h.manager.send(AgentSendParams {
            agent_id: agent.id.clone(),
            text: STEER_TEXT.to_owned(),
            behavior: SendBehavior::Steer,
        }),
    )
    .await
    .expect("send did not finish")
    .expect("send");
    assert_eq!(outcome, SendOutcome::InterruptedAndSent);
    let idle = h.wait_state(&agent.id, AgentState::Idle, EVENT_WAIT).await;
    assert_eq!(idle.last_error, None);
    let events = h.events(&agent.id);
    assert!(h.error_messages(&agent.id).is_empty(), "{events:?}");
    assert!(
        !events.contains(&EventKind::StateChanged {
            state: AgentState::Failed
        }),
        "{events:?}"
    );
    let completed = events
        .iter()
        .filter(|kind| matches!(kind, EventKind::TurnCompleted { .. }))
        .count();
    assert_eq!(completed, 1, "{events:?}");
    assert!(
        events.contains(&EventKind::AssistantText {
            text: "pong".to_owned(),
            final_: true,
        }),
        "{events:?}"
    );
    let archived = timeout(EVENT_WAIT, h.manager.archive(&agent.id))
        .await
        .expect("archive did not finish")
        .expect("archive");
    assert_eq!(archived.state, AgentState::Archived);
}

#[tokio::test]
async fn codex_missing_binary_fails_agent() {
    let h = Harness::new(CodexAdapter::new(PathBuf::from(
        "/nonexistent/dgc-test/codex",
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
