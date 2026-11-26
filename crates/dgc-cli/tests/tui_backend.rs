//! The TUI runtime against a real daemon: `tui::backend::Backend` driven
//! directly, and `tui::Runtime` / `tui::run_loop` on a ratatui `TestBackend`.
//! No real terminal and no real provider: agents replay fake-provider
//! scenarios through wrapper scripts (see `common`).
//!
//! `dgc-cli` has no library target, so the TUI sources are compiled into this
//! test crate with `#[path]` (their own unit tests run here a second time).

mod common;

#[allow(dead_code)]
#[path = "../src/output.rs"]
mod output;
#[allow(dead_code)]
#[path = "../src/tui/mod.rs"]
mod tui;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use common::{CLAUDE_VERSION, TestEnv, assert_success, scenario, stdout, write_wrapper};
use dgc_client::Client;
use dgc_daemon::paths::DaemonPaths;
use dgc_protocol::PROTOCOL_VERSION;
use dgc_protocol::event::{AgentEvent, EventKind};
use dgc_protocol::ids::AgentId;
use dgc_protocol::ids::PermissionId;
use dgc_protocol::methods::{
    AGENT_LIST, AGENT_SEND, HELLO, MAX_EVENTS_LIMIT, PERMISSION_LIST, PERMISSION_RESPOND,
    PROJECT_LIST, SUBSCRIBE, WORKSPACE_LIST,
};
use dgc_protocol::model::AgentState;
use dgc_protocol::rpc::{METHOD_NOT_FOUND, Request, Response, RpcError};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use tui::backend::{Backend, CallTimeouts};
use tui::input::KeyInput;
use tui::state::AppState;
use tui::{AppEvent, ConnectionState, Effect, RequestTag, Runtime, TermEvent};

const WAIT: Duration = Duration::from_secs(30);

/// A state root whose Claude replays `scenario`, plus a `dgc-next` wrapper
/// that pins `DGC_STATE_ROOT`: a daemon the backend spawns lives there too,
/// never in the user's directories.
struct Harness {
    env: TestEnv,
    exe: PathBuf,
}

impl Harness {
    fn new(scenario: &Path) -> Self {
        Self::create(TestEnv::new(), scenario)
    }

    /// A harness whose Claude plays `turns` (each a list of assistant
    /// messages, each a list of text blocks).
    fn with_turns(turns: &[Vec<Vec<String>>]) -> Self {
        let env = TestEnv::new();
        let path = env.dir.path().join("scenario.jsonl");
        fs::write(&path, claude_scenario(turns)).expect("write scenario");
        Self::create(env, &path)
    }

    fn create(env: TestEnv, scenario: &Path) -> Self {
        let bin = env.dir.path().join("bin");
        fs::create_dir(&bin).expect("mkdir bin");
        let claude = write_wrapper(&bin, "claude", CLAUDE_VERSION, scenario);
        let config_dir = env.paths.config_file.parent().expect("config dir");
        fs::create_dir_all(config_dir).expect("mkdir config");
        fs::write(
            &env.paths.config_file,
            format!("claude_path = \"{}\"\n", claude.display()),
        )
        .expect("write config");
        let exe = bin.join("dgc-next");
        fs::write(
            &exe,
            format!(
                "#!/bin/sh\nunset DGC_LOG\nDGC_STATE_ROOT='{}' exec '{}' \"$@\"\n",
                env.dir.path().display(),
                env!("CARGO_BIN_EXE_dgc-next")
            ),
        )
        .expect("write dgc-next wrapper");
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).expect("chmod");
        Self { env, exe }
    }

    fn run(&self, args: &[&str]) -> Output {
        self.env.run(args)
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        assert_success(&output);
        serde_json::from_str(stdout(&output).trim()).expect("stdout is one JSON value")
    }

    /// Register a project and create its local workspace; returns the
    /// workspace id.
    fn add_workspace(&self) -> String {
        let project_dir = self.env.dir.path().join("project");
        fs::create_dir(&project_dir).expect("mkdir project");
        let project_dir = fs::canonicalize(project_dir).expect("canon");
        let project = self.json(&[
            "--json",
            "project",
            "add",
            project_dir.to_str().expect("utf8"),
        ]);
        let workspace = self.json(&[
            "--json",
            "workspace",
            "create",
            "--project",
            project["id"].as_str().expect("project id"),
        ]);
        workspace["id"].as_str().expect("workspace id").to_owned()
    }

    /// `dgc-next run` a Claude agent; returns its id.
    fn run_agent(&self, workspace: &str, prompt: &str) -> AgentId {
        let output = self.run(&[
            "run",
            "--workspace",
            workspace,
            "--provider",
            "claude",
            prompt,
        ]);
        assert_success(&output);
        stdout(&output).trim().parse().expect("agent id")
    }

    fn wait_state(&self, id: &AgentId, state: &str) {
        let deadline = Instant::now() + WAIT;
        loop {
            let agent = self.json(&["--json", "show", id.as_str()]);
            if agent["state"] == state {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "agent never reached {state}: {agent}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn wait_permission(&self, id: &AgentId) {
        let deadline = Instant::now() + WAIT;
        loop {
            let list = self.json(&["--json", "permit", "ls"]);
            let pending = list["permissions"].as_array().expect("permissions");
            if pending.iter().any(|p| p["agent_id"] == id.as_str()) {
                return;
            }
            assert!(Instant::now() < deadline, "no permission request: {list}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn start_backend(&self) -> (mpsc::UnboundedSender<Effect>, mpsc::Receiver<AppEvent>) {
        Backend::start(self.env.paths.clone(), self.exe.clone())
    }
}

const CLAUDE_INIT: [&str; 3] = [
    r#"{"expect":{"type":"control_request","request_id":"${ANY}","request":{"subtype":"initialize"}}}"#,
    r#"{"capture":"init","path":"/request_id"}"#,
    r#"{"send":{"type":"control_response","response":{"subtype":"success","request_id":"${init}","response":{}}}}"#,
];

fn claude_scenario(turns: &[Vec<Vec<String>>]) -> String {
    let mut lines: Vec<String> = CLAUDE_INIT.iter().map(|l| (*l).to_owned()).collect();
    for messages in turns {
        lines.push(json!({"expect": {"type": "user"}}).to_string());
        lines.push(
            json!({"send": {"type": "system", "subtype": "init", "session_id": "sess-tui"}})
                .to_string(),
        );
        for blocks in messages {
            let content: Vec<Value> = blocks
                .iter()
                .map(|text| json!({"type": "text", "text": text}))
                .collect();
            lines.push(
                json!({"send": {"type": "assistant", "message": {"content": content}}}).to_string(),
            );
        }
        lines.push(
            json!({"send": {"type": "result", "subtype": "success", "is_error": false,
                            "session_id": "sess-tui"}})
            .to_string(),
        );
    }
    lines.push(json!({"expect_eof": true}).to_string());
    lines.join("\n") + "\n"
}

fn text_turn(texts: &[&str]) -> Vec<Vec<String>> {
    vec![texts.iter().map(|t| (*t).to_owned()).collect()]
}

/// 4000 events of ~1 KiB: more than one `agent.events` page holds (by count
/// and by size), and far more than a subscriber that stops reading can
/// buffer (socket, the daemon's 1024-entry queue, the client's channels).
fn burst_turn() -> Vec<Vec<String>> {
    let block = "x".repeat(1000);
    (0..40).map(|_| vec![block.clone(); 100]).collect()
}

async fn next_event(rx: &mut mpsc::Receiver<AppEvent>) -> AppEvent {
    timeout(WAIT, rx.recv())
        .await
        .expect("the backend sends an event in time")
        .expect("the backend is running")
}

/// Skip events until `pick` returns a value.
async fn wait_for<T>(
    rx: &mut mpsc::Receiver<AppEvent>,
    mut pick: impl FnMut(&AppEvent) -> Option<T>,
) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "the expected event never came");
        let event = next_event(rx).await;
        if let Some(found) = pick(&event) {
            return found;
        }
    }
}

/// Discard events until none arrives for `quiet`.
async fn drain(rx: &mut mpsc::Receiver<AppEvent>, quiet: Duration) -> Vec<AppEvent> {
    let mut seen = Vec::new();
    while let Ok(Some(event)) = timeout(quiet, rx.recv()).await {
        seen.push(event);
    }
    seen
}

fn text_of(event: &AgentEvent) -> Option<&str> {
    match &event.kind {
        EventKind::AssistantText { text, .. } => Some(text),
        _ => None,
    }
}

fn snapshot_of(event: &AppEvent) -> Option<AppEvent> {
    matches!(event, AppEvent::Snapshot { .. }).then(|| event.clone())
}

fn is_disconnected(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::Connection(ConnectionState::Disconnected { .. })
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backend_snapshot_and_live_events() {
    let h = Harness::new(&scenario("claude_turn"));
    // Nothing runs yet: the first connection starts the daemon.
    let (fx, mut rx) = h.start_backend();
    assert_eq!(
        next_event(&mut rx).await,
        AppEvent::Connection(ConnectionState::Connected)
    );

    let workspace = h.add_workspace();
    let agent = h.run_agent(&workspace, "hello");
    let reply = wait_for(&mut rx, |event| match event {
        AppEvent::AgentEvent(e) if e.agent_id == agent => text_of(e).map(str::to_owned),
        _ => None,
    })
    .await;
    assert_eq!(reply, "pong");

    fx.send(Effect::LoadSnapshot).expect("backend alive");
    let AppEvent::Snapshot {
        projects,
        workspaces,
        agents,
        permissions,
    } = wait_for(&mut rx, snapshot_of).await
    else {
        unreachable!()
    };
    assert_eq!(projects.len(), 1);
    assert!(workspaces.iter().any(|w| w.id.as_str() == workspace));
    assert!(agents.iter().any(|a| a.id == agent));
    assert!(permissions.is_empty());

    // An RPC result comes back tagged with its request.
    let request = RequestTag::Send {
        agent_id: agent.clone(),
    };
    fx.send(Effect::Call {
        request: request.clone(),
        method: AGENT_SEND,
        params: json!({"agent_id": agent, "text": "more"}),
    })
    .expect("backend alive");
    let result = wait_for(&mut rx, |event| match event {
        AppEvent::RpcDone { request: r, result } if *r == request => Some(result.clone()),
        _ => None,
    })
    .await;
    assert!(result.is_ok(), "{result:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agents_created_by_cli_appear_in_snapshot() {
    let h = Harness::new(&scenario("claude_permission"));
    let workspace = h.add_workspace();
    let agent = h.run_agent(&workspace, "make a file");
    h.wait_permission(&agent);

    let (fx, mut rx) = h.start_backend();
    let mut state = AppState::default();
    let connected = next_event(&mut rx).await;
    assert_eq!(connected, AppEvent::Connection(ConnectionState::Connected));
    // What update asks for on Connected goes to the backend.
    for effect in tui::update::update(&mut state, connected) {
        fx.send(effect).expect("backend alive");
    }
    let snapshot = wait_for(&mut rx, snapshot_of).await;
    let AppEvent::Snapshot {
        agents,
        permissions,
        ..
    } = &snapshot
    else {
        unreachable!()
    };
    assert!(agents.iter().any(|a| a.id == agent));
    assert!(permissions.iter().any(|p| p.agent_id == agent));

    tui::update::update(&mut state, snapshot);
    let row = state
        .rows()
        .into_iter()
        .find(|r| r.label == "make a file")
        .expect("the CLI's agent is in the tree");
    assert!(row.waiting_permission, "the permission mark is shown");
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    terminal
        .draw(|frame| tui::render::render(&state, frame))
        .expect("draw");
    let screen: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(screen.contains("make a file"), "{screen}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backend_reports_disconnect_and_reconnects() {
    let h = Harness::new(&scenario("claude_turn"));
    assert_success(&h.run(&["daemon", "start"]));
    let (fx, mut rx) = h.start_backend();
    assert_eq!(
        next_event(&mut rx).await,
        AppEvent::Connection(ConnectionState::Connected)
    );

    assert_success(&h.run(&["daemon", "stop"]));
    let down = next_event(&mut rx).await;
    let AppEvent::Connection(ConnectionState::Disconnected { reason }) = &down else {
        panic!("expected Disconnected, got {down:?}");
    };
    assert!(!reason.is_empty());

    // While disconnected, requests fail at once instead of waiting.
    let agent_id = AgentId::generate();
    let send = RequestTag::Send {
        agent_id: agent_id.clone(),
    };
    fx.send(Effect::Call {
        request: send.clone(),
        method: AGENT_SEND,
        params: json!({"agent_id": agent_id, "text": "hi"}),
    })
    .expect("backend alive");
    fx.send(Effect::LoadHistory {
        agent_id: agent_id.clone(),
        since_seq: 0,
        generation: 4,
    })
    .expect("backend alive");
    // Retries run every second for a while; none of them restarts the
    // daemon the user stopped, and Disconnected is reported only once.
    let seen = drain(&mut rx, Duration::from_millis(2500)).await;
    assert!(
        seen.iter().any(|e| matches!(
            e,
            AppEvent::RpcDone { request, result: Err(_) } if *request == send
        )),
        "{seen:?}"
    );
    assert!(seen.contains(&AppEvent::History {
        agent_id: agent_id.clone(),
        events: vec![],
        done: true,
        generation: 4,
    }));
    assert!(seen.iter().any(|e| matches!(
        e,
        AppEvent::RpcDone { request: RequestTag::LoadHistory { agent_id: a }, result: Err(_) }
            if *a == agent_id
    )));
    assert!(!seen.iter().any(is_disconnected), "{seen:?}");
    assert!(!seen.iter().any(|e| matches!(e, AppEvent::Connection(_))));
    let err = Client::connect(&h.env.paths)
        .await
        .err()
        .expect("the backend must not restart a stopped daemon");
    assert!(err.is_not_running(), "{err:?}");

    assert_success(&h.run(&["daemon", "start"]));
    let up = wait_for(&mut rx, |event| match event {
        AppEvent::Connection(state) => Some(state.clone()),
        _ => None,
    })
    .await;
    assert_eq!(up, ConnectionState::Connected);
    fx.send(Effect::LoadSnapshot).expect("backend alive");
    wait_for(&mut rx, snapshot_of).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backend_resubscribes_after_lagged() {
    let h = Harness::with_turns(&[text_turn(&["ready"]), burst_turn(), text_turn(&["after"])]);
    let (fx, mut rx) = h.start_backend();
    assert_eq!(
        next_event(&mut rx).await,
        AppEvent::Connection(ConnectionState::Connected)
    );
    let workspace = h.add_workspace();
    let agent = h.run_agent(&workspace, "start");
    h.wait_state(&agent, "idle");
    drain(&mut rx, Duration::from_millis(300)).await;

    // Stop reading while the burst runs: the daemon ends the subscription.
    assert_success(&h.run(&["send", agent.as_str(), "go"]));
    h.wait_state(&agent, "idle");
    let mut before_lag = Vec::new();
    loop {
        match next_event(&mut rx).await {
            AppEvent::Lagged => break,
            AppEvent::AgentEvent(event) => before_lag.push(event.seq),
            other => panic!("unexpected event before Lagged: {other:?}"),
        }
    }
    let expected: Vec<u64> = (before_lag[0]..before_lag[0] + before_lag.len() as u64).collect();
    assert_eq!(
        before_lag, expected,
        "events before the notice are in order"
    );
    assert!(
        before_lag.len() < 4000,
        "the subscriber missed events ({} delivered)",
        before_lag.len()
    );

    fx.send(Effect::Resubscribe).expect("backend alive");
    // Give the new subscription a moment, then make the agent talk again.
    sleep(Duration::from_millis(500)).await;
    assert_success(&h.run(&["send", agent.as_str(), "again"]));
    let mut after = Vec::new();
    let reply = wait_for(&mut rx, |event| {
        after.push(event.clone());
        match event {
            AppEvent::AgentEvent(e) if e.agent_id == agent => text_of(e).map(str::to_owned),
            _ => None,
        }
    })
    .await;
    assert_eq!(reply, "after");
    assert!(!after.iter().any(is_disconnected), "{after:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backend_history_pages_until_done() {
    let h = Harness::with_turns(&[burst_turn()]);
    let workspace = h.add_workspace();
    let agent = h.run_agent(&workspace, "go");
    h.wait_state(&agent, "idle");

    let (fx, mut rx) = h.start_backend();
    assert_eq!(
        next_event(&mut rx).await,
        AppEvent::Connection(ConnectionState::Connected)
    );
    let mut seqs = Vec::new();
    let mut pages = Vec::new();
    loop {
        fx.send(Effect::LoadHistory {
            agent_id: agent.clone(),
            since_seq: seqs.last().copied().unwrap_or(0),
            generation: 7,
        })
        .expect("backend alive");
        let (events, done, generation) = wait_for(&mut rx, |event| match event {
            AppEvent::History {
                agent_id,
                events,
                done,
                generation,
            } if *agent_id == agent => Some((events.clone(), *done, *generation)),
            _ => None,
        })
        .await;
        assert_eq!(generation, 7, "the page carries the request's generation");
        assert_eq!(
            done,
            events.is_empty(),
            "only an empty page ends the history"
        );
        pages.push(events.len());
        if done {
            break;
        }
        seqs.extend(events.iter().map(|e| e.seq));
    }
    let expected: Vec<u64> = (1..=seqs.len() as u64).collect();
    assert_eq!(seqs, expected, "every event exactly once, in order");
    assert!(seqs.len() > 4000, "{} events", seqs.len());
    // The first page is cut by size: fewer than the limit, yet not the end.
    assert!(
        pages[0] > 0 && pages[0] < MAX_EVENTS_LIMIT as usize,
        "{pages:?}"
    );

    // A failed load ends its chain and reports the error.
    let unknown = AgentId::generate();
    fx.send(Effect::LoadHistory {
        agent_id: unknown.clone(),
        since_seq: 0,
        generation: 9,
    })
    .expect("backend alive");
    let first = next_event(&mut rx).await;
    let second = next_event(&mut rx).await;
    let both = [first, second];
    assert!(both.contains(&AppEvent::History {
        agent_id: unknown.clone(),
        events: vec![],
        done: true,
        generation: 9,
    }));
    assert!(
        both.iter().any(|e| matches!(
            e,
            AppEvent::RpcDone { request: RequestTag::LoadHistory { agent_id }, result: Err(_) }
                if *agent_id == unknown
        )),
        "{both:?}"
    );
    // The connection is still fine.
    fx.send(Effect::LoadSnapshot).expect("backend alive");
    let event = next_event(&mut rx).await;
    assert!(matches!(event, AppEvent::Snapshot { .. }), "{event:?}");
}

// ---- the runtime loop ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quitting_the_tui_leaves_the_daemon_and_agents_running() {
    let h = Harness::new(&scenario("claude_turn"));
    let workspace = h.add_workspace();
    let agent = h.run_agent(&workspace, "hello");
    h.wait_state(&agent, "idle");
    let pid = h.env.lock_pid().expect("daemon pid");

    let (fx, daemon_events) = h.start_backend();
    let terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    let mut runtime = Runtime::new(terminal, fx, tui::update::update);
    let (keys, key_events) = mpsc::unbounded_channel();
    let typist = tokio::spawn(async move {
        sleep(Duration::from_millis(1500)).await;
        keys.send(TermEvent::Key(KeyInput::char('q')))
            .expect("loop alive");
        // Keep the channel open: the loop must stop on `q`, not on EOF.
        sleep(WAIT).await;
        drop(keys);
    });
    timeout(WAIT, tui::run_loop(&mut runtime, key_events, daemon_events))
        .await
        .expect("q ends the loop")
        .expect("the loop ends cleanly");
    typist.abort();
    // The snapshot was on screen before quitting.
    assert!(
        runtime.state().agent_count() == 1,
        "the loop showed the daemon's agents"
    );
    drop(runtime);

    sleep(Duration::from_millis(300)).await;
    assert_eq!(h.env.lock_pid(), Some(pid), "the daemon keeps running");
    let mut client = Client::connect(&h.env.paths)
        .await
        .expect("the daemon still answers");
    let shown: Value = client
        .call("agent.get", &json!({"id": agent}))
        .await
        .expect("agent.get");
    let state: AgentState = serde_json::from_value(shown["state"].clone()).expect("state");
    assert_eq!(state, AgentState::Idle, "the agent was not stopped");
}

// ---- the command ----

#[test]
fn tui_needs_a_terminal_and_has_no_json_mode() {
    let env = TestEnv::new();
    for args in [&["tui"][..], &[][..]] {
        let output = env
            .command(args)
            .env("DGC_LOG", "trace")
            .output()
            .expect("run");
        assert!(!output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "error: the TUI needs a terminal\n",
            "only the error, no log lines ({args:?})"
        );
        assert!(output.stdout.is_empty());
    }
    for args in [&["--json", "tui"][..], &["--json"][..]] {
        let output = env.run(args);
        assert!(!output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "error: the TUI has no JSON mode\n"
        );
    }
    assert!(env.lock_pid().is_none(), "no daemon was started");
    assert!(!env.paths.socket.exists());
}

// ---- misbehaving daemons ----

/// What a stand-in daemon answers to a method: `None` never answers.
type Answer = fn(&str) -> Option<Result<Value, RpcError>>;

/// A daemon stand-in on `paths.socket` that completes `hello` and
/// `subscribe` on every connection and answers the rest with `answer`.
fn fake_daemon(paths: &DaemonPaths, answer: Answer) -> tokio::task::JoinHandle<()> {
    slow_fake_daemon(paths, answer, |_| Duration::ZERO)
}

/// [`fake_daemon`] that waits `delay(method)` before answering a method.
fn slow_fake_daemon(
    paths: &DaemonPaths,
    answer: Answer,
    delay: fn(&str) -> Duration,
) -> tokio::task::JoinHandle<()> {
    fs::create_dir_all(paths.socket.parent().expect("socket dir")).expect("mkdir");
    let listener = UnixListener::bind(&paths.socket).expect("bind");
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let request: Request = serde_json::from_str(&line).expect("request");
                    let reply = match request.method.as_str() {
                        HELLO => Some(Ok(
                            json!({"protocol_version": PROTOCOL_VERSION, "daemon_pid": 1}),
                        )),
                        SUBSCRIBE => Some(Ok(json!({}))),
                        method => {
                            sleep(delay(method)).await;
                            answer(method)
                        }
                    };
                    let response = match reply {
                        Some(Ok(value)) => Response::ok(request.id, value),
                        Some(Err(error)) => Response::err(request.id, error),
                        None => continue,
                    };
                    let mut text = serde_json::to_string(&response).expect("encode");
                    text.push('\n');
                    if write.write_all(text.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    })
}

/// Never used: the stand-in daemon is already listening.
fn no_exe() -> PathBuf {
    PathBuf::from("/nonexistent/dgc-next")
}

#[tokio::test]
async fn backend_reports_a_snapshot_it_cannot_load() {
    // An older daemon that does not know `permission.list`.
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::under(dir.path());
    let server = fake_daemon(&paths, |method| match method {
        PROJECT_LIST => Some(Ok(json!({"projects": []}))),
        WORKSPACE_LIST => Some(Ok(json!({"workspaces": []}))),
        AGENT_LIST => Some(Ok(json!({"agents": []}))),
        PERMISSION_LIST => Some(Err(RpcError::new(
            METHOD_NOT_FOUND,
            "unknown method permission.list",
        ))),
        _ => Some(Ok(json!({}))),
    });
    let (fx, mut rx) = Backend::start(paths, no_exe());
    let mut state = AppState::default();
    let connected = next_event(&mut rx).await;
    assert_eq!(connected, AppEvent::Connection(ConnectionState::Connected));
    for effect in tui::update::update(&mut state, connected) {
        fx.send(effect).expect("backend alive");
    }
    let event = next_event(&mut rx).await;
    let AppEvent::Connection(ConnectionState::Disconnected { reason }) = &event else {
        panic!("the failure must reach the UI, got {event:?}");
    };
    assert!(reason.contains("cannot load the snapshot"), "{reason}");
    assert!(reason.contains("permission.list"), "{reason}");
    tui::update::update(&mut state, event.clone());
    assert!(
        matches!(
            &state.status.connection,
            ConnectionState::Disconnected { reason } if reason.contains("cannot load the snapshot")
        ),
        "{:?}",
        state.status.connection
    );
    server.abort();
}

#[tokio::test]
async fn backend_waits_a_second_before_reconnecting_after_a_lost_snapshot() {
    // A daemon that answers `hello` but fails a list call: every connection
    // is lost right after `Connected`. Reconnecting at once would loop as
    // fast as round trips allow.
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::under(dir.path());
    let server = fake_daemon(&paths, |method| match method {
        PROJECT_LIST => Some(Err(RpcError::new(METHOD_NOT_FOUND, "no projects today"))),
        _ => Some(Ok(json!({}))),
    });
    let (fx, mut rx) = Backend::start(paths, no_exe());
    let mut state = AppState::default();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut connected = 0;
    let mut disconnected = 0;
    while let Ok(Some(event)) = timeout(
        deadline.saturating_duration_since(Instant::now()),
        rx.recv(),
    )
    .await
    {
        match &event {
            AppEvent::Connection(ConnectionState::Connected) => connected += 1,
            event if is_disconnected(event) => disconnected += 1,
            _ => {}
        }
        for effect in tui::update::update(&mut state, event) {
            fx.send(effect).expect("backend alive");
        }
    }
    assert!(disconnected >= 1, "the lost snapshot is reported");
    assert!(
        (1..=3).contains(&connected),
        "reconnects wait a second each: {connected} connections in 2 s"
    );
    server.abort();
}

#[tokio::test]
async fn backend_gives_up_on_a_daemon_that_does_not_answer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::under(dir.path());
    let server = fake_daemon(&paths, |_| None);
    let limit = Duration::from_millis(300);
    let timeouts = CallTimeouts {
        read: limit,
        mutating: limit,
    };
    let (fx, mut rx) = Backend::start_with_timeouts(paths, no_exe(), timeouts);
    assert_eq!(
        next_event(&mut rx).await,
        AppEvent::Connection(ConnectionState::Connected)
    );
    let request = RequestTag::Respond {
        permission_id: PermissionId::generate(),
    };
    fx.send(Effect::Call {
        request: request.clone(),
        method: PERMISSION_RESPOND,
        params: json!({}),
    })
    .expect("backend alive");
    let started = Instant::now();
    let answer = next_event(&mut rx).await;
    let AppEvent::RpcDone {
        request: answered,
        result: Err(message),
    } = &answer
    else {
        panic!("the call must fail, got {answer:?}");
    };
    assert_eq!(*answered, request);
    assert!(message.contains("did not answer"), "{message}");
    let event = next_event(&mut rx).await;
    let AppEvent::Connection(ConnectionState::Disconnected { reason }) = &event else {
        panic!("expected Disconnected, got {event:?}");
    };
    assert!(reason.contains("did not answer"), "{reason}");
    assert!(started.elapsed() < Duration::from_secs(5));
    server.abort();
}

#[tokio::test]
async fn backend_gives_mutating_calls_longer_than_reads() {
    // The daemon's own limits on a send (provider calls, git) are longer
    // than a read's: a slow but healthy send must not count as a lost
    // connection, or the user resends and the text is delivered twice.
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = DaemonPaths::under(dir.path());
    let server = slow_fake_daemon(
        &paths,
        |method| match method {
            AGENT_SEND => Some(Ok(json!({"outcome": "queued"}))),
            AGENT_LIST => Some(Ok(json!({"agents": []}))),
            _ => Some(Ok(json!({}))),
        },
        |method| match method {
            AGENT_SEND | AGENT_LIST => Duration::from_millis(800),
            _ => Duration::ZERO,
        },
    );
    let timeouts = CallTimeouts {
        read: Duration::from_millis(300),
        mutating: Duration::from_secs(5),
    };
    let (fx, mut rx) = Backend::start_with_timeouts(paths, no_exe(), timeouts);
    assert_eq!(
        next_event(&mut rx).await,
        AppEvent::Connection(ConnectionState::Connected)
    );
    let request = RequestTag::Send {
        agent_id: AgentId::generate(),
    };
    fx.send(Effect::Call {
        request: request.clone(),
        method: AGENT_SEND,
        params: json!({}),
    })
    .expect("backend alive");
    assert_eq!(
        next_event(&mut rx).await,
        AppEvent::RpcDone {
            request: request.clone(),
            result: Ok(json!({"outcome": "queued"})),
        },
        "a send that outlives the read limit still succeeds"
    );
    // The same delay on a read is too long.
    fx.send(Effect::Call {
        request: request.clone(),
        method: AGENT_LIST,
        params: json!({}),
    })
    .expect("backend alive");
    let answer = next_event(&mut rx).await;
    assert!(
        matches!(&answer, AppEvent::RpcDone { result: Err(message), .. } if message.contains("did not answer")),
        "{answer:?}"
    );
    assert!(is_disconnected(&next_event(&mut rx).await));
    server.abort();
}

#[test]
fn calls_that_change_something_get_the_long_limit() {
    use dgc_protocol::methods::{
        AGENT_ARCHIVE, AGENT_CREATE, AGENT_EVENTS, AGENT_INTERRUPT, AGENT_RESUME,
        WORKSPACE_ARCHIVE, WORKSPACE_CREATE,
    };
    use tui::backend::CALL_TIMEOUTS;
    for method in [
        AGENT_SEND,
        AGENT_CREATE,
        AGENT_INTERRUPT,
        AGENT_RESUME,
        AGENT_ARCHIVE,
        PERMISSION_RESPOND,
        WORKSPACE_CREATE,
        WORKSPACE_ARCHIVE,
    ] {
        assert_eq!(
            CALL_TIMEOUTS.of(method),
            Duration::from_secs(90),
            "{method}"
        );
    }
    for method in [
        PROJECT_LIST,
        WORKSPACE_LIST,
        AGENT_LIST,
        PERMISSION_LIST,
        AGENT_EVENTS,
        SUBSCRIBE,
    ] {
        assert_eq!(
            CALL_TIMEOUTS.of(method),
            Duration::from_secs(15),
            "{method}"
        );
    }
}
