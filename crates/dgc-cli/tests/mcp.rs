//! Drive MCP over the executable's stdio with an isolated daemon and fake Claude.
mod common;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener as StdListener;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use common::{CLAUDE_VERSION, TestEnv, assert_success, scenario, stdout, write_wrapper};
use dgc_client::Client;
use dgc_protocol::rpc::{INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;

const WAIT: Duration = Duration::from_secs(15);
const LATEST: &str = "2025-11-25";
const EARLIER: &str = "2025-06-18";
const PRIVATE: &str = "private-mcp-test-text";

struct Daemon {
    env: TestEnv,
    workspace: Value,
}

impl Daemon {
    fn new(scenario: impl FnOnce(&TestEnv) -> PathBuf) -> Self {
        let env = TestEnv::new();
        assert!(env.dir.path().is_absolute());
        let claude = write_wrapper(env.dir.path(), "claude", CLAUDE_VERSION, &scenario(&env));
        fs::create_dir_all(env.paths.config_file.parent().expect("config dir"))
            .expect("config dir");
        fs::write(
            &env.paths.config_file,
            format!("claude_path = {:?}\n", claude.to_str().expect("path")),
        )
        .expect("write config");
        // 'daemon start' returns after the hello handshake and provider probe.
        assert_success(&env.run(&["daemon", "start"]));
        let project = env.dir.path().join("project");
        fs::create_dir(&project).expect("create project");
        let added = env.run(&["--json", "project", "add", project.to_str().expect("path")]);
        assert_success(&added);
        let added: Value = serde_json::from_str(&stdout(&added)).expect("project JSON");
        let mut daemon = Self {
            env,
            workspace: Value::Null,
        };
        let workspace = daemon.rpc(
            "workspace.create",
            json!({"project_id":added["id"], "isolation":"local"}),
        );
        daemon.workspace = workspace["id"].clone();
        daemon
    }

    fn idle() -> Self {
        Self::new(|_| scenario("claude_turn"))
    }

    fn running() -> Self {
        Self::new(|env| {
            fake_scenario(
                env,
                vec![
                    json!({"expect":{"type":"user"}}),
                    json!({"send":{"type":"system","subtype":"init","session_id":"mcp-test-running"}}),
                    json!({"expect_eof":true}),
                ],
            )
        })
    }

    fn rpc(&self, method: &str, params: Value) -> Value {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
            .block_on(async {
                tokio::time::timeout(WAIT, async {
                    let mut client = Client::connect(&self.env.paths)
                        .await
                        .expect("daemon ready");
                    client.call(method, &params).await.expect("daemon RPC")
                })
                .await
                .expect("daemon RPC deadline")
            })
    }

    fn agent(&self, parent: Option<&Value>, mcp: bool) -> Value {
        self.rpc(
            "agent.create",
            json!({
                "workspace_id": self.workspace, "title":"test agent",
                "provider":"claude", "initial_prompt":"start", "parent_id":parent, "mcp":mcp
            }),
        )["id"]
            .clone()
    }

    fn get(&self, id: &Value) -> Value {
        self.rpc("agent.get", json!({"id":id}))
    }

    fn wait_for_event(&self, id: &Value, kind: Value) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
            .block_on(async {
                tokio::time::timeout(WAIT, async {
                    let client = Client::connect(&self.env.paths)
                        .await
                        .expect("daemon ready");
                    let mut events = client
                        .subscribe(dgc_protocol::methods::SubscribeParams {
                            agent_id: Some(serde_json::from_value(id.clone()).expect("agent id")),
                            since_seq: Some(0),
                        })
                        .await
                        .expect("subscribe");
                    while let Some(event) = events.recv().await {
                        if event.params["kind"] == kind {
                            return;
                        }
                    }
                    panic!("subscription closed before expected event");
                })
                .await
                .expect("event deadline");
            });
    }

    fn mcp(&self, id: &Value) -> Mcp {
        Mcp::initialized(&self.env, id.as_str().expect("agent id"))
    }
}

/// Generate a synthetic Claude scenario in the isolated state root.
fn fake_scenario(env: &TestEnv, steps: Vec<Value>) -> PathBuf {
    let path = env.dir.path().join("mcp-synthetic.jsonl");
    let mut file = fs::File::create(&path).expect("create scenario");
    for step in [
        json!({"expect":{"type":"control_request","request":{"subtype":"initialize"}}}),
        json!({"capture":"init","path":"/request_id"}),
        json!({"send":{"type":"control_response","response":{"subtype":"success","request_id":"${init}","response":{}}}}),
    ]
    .into_iter()
    .chain(steps)
    {
        writeln!(file, "{step}").expect("write scenario");
    }
    path
}

fn assistant_text(text: &str) -> Value {
    json!({"send":{"type":"assistant","message":{"content":[{"type":"text","text":text}]}}})
}

struct Mcp {
    child: Child,
    input: Option<ChildStdin>,
    lines: Receiver<Vec<u8>>,
    reader: Option<std::thread::JoinHandle<()>>,
    next_id: u64,
}

impl Mcp {
    fn start(env: &TestEnv, agent: &str) -> Self {
        let mut child = env
            .command(&["mcp", "--agent", agent])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn stdio server");
        let input = child.stdin.take().expect("stdin");
        let mut output = BufReader::new(child.stdout.take().expect("stdout"));
        let (send, lines) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            loop {
                let mut line = Vec::new();
                if output.read_until(b'\n', &mut line).expect("read stdout") == 0
                    || send.send(line).is_err()
                {
                    return;
                }
            }
        });
        Self {
            child,
            input: Some(input),
            lines,
            reader: Some(reader),
            next_id: 1,
        }
    }

    fn initialized(env: &TestEnv, agent: &str) -> Self {
        let mut mcp = Self::start(env, agent);
        mcp.initialize(LATEST);
        mcp.write(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        mcp
    }

    fn write(&mut self, value: &Value) {
        self.raw(value.to_string().as_bytes());
    }

    fn raw(&mut self, bytes: &[u8]) {
        let input = self.input.as_mut().expect("MCP stdin");
        input.write_all(bytes).expect("write request");
        input.write_all(b"\n").expect("write delimiter");
    }

    fn read(&self) -> Value {
        let line = self
            .lines
            .recv_timeout(WAIT)
            .expect("MCP response deadline");
        assert_eq!(line.last(), Some(&b'\n'));
        let message: Value = serde_json::from_slice(&line).expect("one JSON message per line");
        assert_eq!(message["jsonrpc"], "2.0");
        assert_ne!(
            message.get("result").is_some(),
            message.get("error").is_some()
        );
        message
    }

    fn begin(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.write(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        id
    }

    fn finish(&self, id: u64) -> Value {
        let response = self.read();
        assert_eq!(response["id"], id);
        response
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.begin(method, params);
        self.finish(id)
    }

    fn initialize(&mut self, version: &str) -> Value {
        self.request(
            "initialize",
            json!({"protocolVersion":version,"capabilities":{},"clientInfo":{"name":"test","version":"1"}}),
        )["result"].clone()
    }

    fn call(&mut self, name: &str, args: Value) -> Value {
        self.request("tools/call", json!({"name":name,"arguments":args}))["result"].clone()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn tool_text(result: &Value) -> &str {
    assert_eq!(
        result["content"].as_array().expect("content blocks").len(),
        1
    );
    assert_eq!(result["content"][0]["type"], "text");
    result["content"][0]["text"].as_str().expect("text")
}

fn tool_json(result: &Value) -> Value {
    assert_ne!(result["isError"], true, "{result}");
    serde_json::from_str(tool_text(result)).expect("tool JSON")
}

fn assert_tool_error(result: &Value) {
    assert_eq!(result["isError"], true, "{result}");
    assert!(!tool_text(result).contains(PRIVATE), "{result}");
}

#[test]
fn initialize_negotiates_version_and_lists_tools() {
    let daemon = Daemon::idle();
    let parent = daemon.agent(None, true);
    for (requested, expected) in [(EARLIER, EARLIER), (LATEST, LATEST), ("2026-07-28", LATEST)] {
        let mut mcp = Mcp::start(&daemon.env, parent.as_str().expect("agent id"));
        let init = mcp.initialize(requested);
        assert_eq!(init["protocolVersion"], expected);
        assert_eq!(init["capabilities"], json!({"tools":{}}));
        assert_eq!(init["serverInfo"]["name"], "dgc");
        mcp.write(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        let response = mcp.request("tools/list", json!({}));
        let tools = response["result"]["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 7);
        for (name, required) in [
            ("create_agent", json!(["prompt"])),
            ("send_agent", json!(["agent_id", "text"])),
            ("get_agent", json!(["agent_id"])),
            ("wait_agent", json!(["agent_id"])),
            ("list_agents", json!([])),
            ("interrupt_agent", json!(["agent_id"])),
            ("archive_agent", json!(["agent_id"])),
        ] {
            let tool = tools.iter().find(|tool| tool["name"] == name).expect(name);
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert_eq!(tool["inputSchema"]["required"], required);
        }
        let wait = tools
            .iter()
            .find(|tool| tool["name"] == "wait_agent")
            .expect("wait");
        assert_eq!(
            wait["inputSchema"]["properties"]["timeout_secs"]["default"],
            60
        );
        assert_eq!(
            wait["inputSchema"]["properties"]["timeout_secs"]["maximum"],
            300
        );
    }
}

#[test]
fn unknown_method_and_notifications() {
    let daemon = Daemon::idle();
    let parent = daemon.agent(None, true);
    let mut mcp = daemon.mcp(&parent);
    mcp.write(&json!({"jsonrpc":"2.0","method":"unknown"}));
    mcp.write(&json!({"jsonrpc":"2.0","method":"tools/call","params":{
        "name":"create_agent","arguments":{"prompt":"ignored notification"}
    }}));
    mcp.write(&json!({"jsonrpc":"2.0","id":"request-2","method":"unknown"}));
    let response = mcp.read();
    assert_eq!(response["id"], "request-2");
    assert_eq!(response["error"]["code"], METHOD_NOT_FOUND);
    assert_eq!(mcp.request("ping", json!({}))["result"], json!({}));
    assert_eq!(
        mcp.request("server/discover", json!({}))["error"]["code"],
        METHOD_NOT_FOUND
    );
    assert_eq!(
        tool_json(&mcp.call("list_agents", json!({}))),
        json!({"agents":[]})
    );
}

#[test]
fn create_agent_makes_child_in_parent_workspace_with_parent_provider() {
    let daemon = Daemon::idle();
    let parent = daemon.agent(None, true);
    let mut mcp = daemon.mcp(&parent);
    let result = tool_json(&mcp.call("create_agent", json!({"prompt":"child\nnext line"})));
    assert_eq!(result["provider"], "claude");
    assert_eq!(result["mode"], Value::Null);
    assert_eq!(result["auto_approve"], false);
    let child = daemon.get(&result["agent_id"]);
    assert_eq!(child["workspace_id"], daemon.workspace);
    assert_eq!(child["parent_id"], parent);
    assert_eq!(child["title"], "child");
    assert_eq!(child["depth"], 1);
    assert_eq!(child["mcp"], false);

    let result = tool_json(&mcp.call("create_agent", json!({
        "prompt":"another","title":"explicit","provider":"claude","model":"test-model","mode":"manual","mcp":true
    })));
    assert_eq!(result["mode"], "manual");
    let child = daemon.get(&result["agent_id"]);
    assert_eq!(child["title"], "explicit");
    assert_eq!(child["model"], "test-model");
    assert_eq!(child["mcp"], true);
}

#[test]
fn create_agent_refusals_are_tool_errors_without_prompt_text() {
    let daemon = Daemon::idle();
    let root = daemon.agent(None, true);
    let parent = daemon.agent(Some(&root), true);
    let deep = daemon.agent(Some(&parent), true);
    let mut mcp = daemon.mcp(&deep);
    for args in [
        json!({"prompt":PRIVATE,"mode":PRIVATE}),
        json!({"prompt":PRIVATE,"mode":"bypassPermissions"}),
        json!({"prompt":PRIVATE,"mcp":true}),
    ] {
        let result = mcp.call("create_agent", args);
        assert_tool_error(&result);
        assert!(tool_text(&result).contains(&INVALID_PARAMS.to_string()));
    }
    for args in [
        json!({"prompt":PRIVATE,"provider":PRIVATE}),
        json!({"prompt":PRIVATE,"workspace_id":daemon.workspace}),
    ] {
        assert_tool_error(&mcp.call("create_agent", args));
    }
}

#[test]
fn tools_refuse_non_descendants() {
    let daemon = Daemon::idle();
    let parent = daemon.agent(None, true);
    let caller = daemon.agent(Some(&parent), true);
    let sibling = daemon.agent(Some(&parent), false);
    let unrelated = daemon.agent(None, false);
    let mut mcp = daemon.mcp(&caller);
    let missing = json!(dgc_protocol::ids::AgentId::generate());
    for id in [&caller, &parent, &sibling, &unrelated, &missing] {
        if id != &missing {
            daemon.wait_for_event(id, json!({"type":"state_changed","state":"idle"}));
        }
        for tool in [
            "send_agent",
            "get_agent",
            "wait_agent",
            "interrupt_agent",
            "archive_agent",
        ] {
            let args = match tool {
                "send_agent" => json!({"agent_id":id,"text":PRIVATE}),
                "wait_agent" => json!({"agent_id":id,"timeout_secs":0}),
                _ => json!({"agent_id":id}),
            };
            assert_tool_error(&mcp.call(tool, args));
        }
        if id != &missing {
            assert_eq!(daemon.get(id)["state"], "idle");
            let tail = daemon.rpc("agent.events", json!({"agent_id":id,"tail":50}));
            assert!(!tail.to_string().contains(PRIVATE));
        }
    }
}

#[test]
fn wait_agent_returns_last_answer_bounded() {
    let answer = format!("{}é\n", "あ".repeat(5461));
    let daemon = Daemon::new(|env| {
        fake_scenario(
            env,
            vec![
                json!({"expect":{"type":"user"}}),
                json!({"send":{"type":"system","subtype":"init","session_id":"mcp-test-answer"}}),
                assistant_text(&answer),
                json!({"send":{"type":"result","subtype":"success","is_error":false,"session_id":"mcp-test-answer"}}),
                json!({"expect":{"type":"user"}}),
                assistant_text("unfinished answer"),
                json!({"expect_eof":true}),
            ],
        )
    });
    let parent = daemon.agent(None, true);
    let child = daemon.agent(Some(&parent), false);
    let mut mcp = daemon.mcp(&parent);
    let first = tool_json(&mcp.call("wait_agent", json!({"agent_id":child})));
    assert_eq!(
        first,
        json!({
            "state":"idle","title":"test agent","provider":"claude","last_answer":"あ".repeat(5461),
            "pending_permission":false,"timed_out":false
        })
    );
    tool_json(&mcp.call("send_agent", json!({"agent_id":child,"text":"next"})));
    daemon.wait_for_event(
        &child,
        json!({"type":"assistant_text","text":"unfinished answer","final":true}),
    );
    let second = tool_json(&mcp.call("get_agent", json!({"agent_id":child})));
    assert_eq!(second["state"], "running");
    assert_eq!(second["last_answer"], first["last_answer"]);
}

#[test]
fn wait_agent_times_out() {
    let daemon = Daemon::running();
    let parent = daemon.agent(None, true);
    let child = daemon.agent(Some(&parent), false);
    daemon.wait_for_event(&child, json!({"type":"state_changed","state":"running"}));
    let mut mcp = daemon.mcp(&parent);
    let before = Instant::now();
    let result = tool_json(&mcp.call("wait_agent", json!({"agent_id":child,"timeout_secs":1})));
    assert!(before.elapsed() >= Duration::from_secs(1));
    assert_eq!(result["state"], "running");
    assert_eq!(result["timed_out"], true);
    assert_eq!(result["last_answer"], "");
    assert_tool_error(&mcp.call("wait_agent", json!({"agent_id":child,"timeout_secs":-1})));
}

// Forward this test's daemon traffic while observing when wait has read the
// running state. The archive and shutdown therefore require a live update.
struct WaitProbe {
    env: TestEnv,
    observations: Receiver<Observation>,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl WaitProbe {
    fn new(daemon_socket: &Path) -> Self {
        let env = TestEnv::new();
        assert!(env.dir.path().is_absolute());
        fs::create_dir_all(env.paths.socket.parent().expect("socket dir")).expect("socket dir");
        let listener = StdListener::bind(&env.paths.socket).expect("bind wait probe");
        listener
            .set_nonblocking(true)
            .expect("nonblocking wait probe");
        let daemon_socket = daemon_socket.to_owned();
        let (observed, observations) = mpsc::channel();
        let (stop, stopped) = oneshot::channel();
        let thread = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("probe runtime")
                .block_on(async {
                    let listener = UnixListener::from_std(listener).expect("async wait probe");
                    tokio::select! {
                        result = async {
                            loop {
                                let (client, _) = listener.accept().await?;
                                tokio::spawn(relay(client, daemon_socket.clone(), observed.clone()));
                            }
                        } => { let _: std::io::Result<()> = result; }
                        _ = stopped => {}
                    }
                });
        });
        Self {
            env,
            observations,
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    fn wait_for_running(&self, agent_id: &Value) {
        let deadline = Instant::now() + WAIT;
        let mut subscribed = false;
        loop {
            let seen = self
                .observations
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("running snapshot from wait");
            match seen {
                Observation::Request(request)
                    if request["method"] == "subscribe"
                        && request["params"]["agent_id"] == *agent_id =>
                {
                    subscribed = true
                }
                Observation::Running(id) if subscribed && id == *agent_id => return,
                _ => {}
            }
        }
    }

    fn wait_for_subscription_close(&self, agent_id: &Value) {
        let deadline = Instant::now() + WAIT;
        loop {
            let observed = self
                .observations
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("cancelled subscription closes");
            if matches!(observed, Observation::SubscriptionClosed(id) if id == *agent_id) {
                return;
            }
        }
    }
}

impl Drop for WaitProbe {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            thread.join().expect("join wait probe");
        }
    }
}

enum Observation {
    Request(Value),
    Running(Value),
    SubscriptionClosed(Value),
}

async fn relay(
    client: UnixStream,
    daemon_socket: PathBuf,
    observed: mpsc::Sender<Observation>,
) -> std::io::Result<()> {
    let daemon = UnixStream::connect(daemon_socket).await?;
    let (caller, mut output) = client.into_split();
    let (replies, mut input) = daemon.into_split();
    let mut replies = AsyncBufReader::new(replies);
    let mut requests = AsyncBufReader::new(caller);
    let mut subscribed_agent = None;
    let result = tokio::select! {
        result = async {
            let mut line = Vec::new();
            while requests.read_until(b'\n', &mut line).await? != 0 {
                if let Ok(request) = serde_json::from_slice::<Value>(&line) {
                    if request["method"] == "subscribe" {
                        subscribed_agent = Some(request["params"]["agent_id"].clone());
                    }
                    let _ = observed.send(Observation::Request(request));
                }
                input.write_all(&line).await?;
                line.clear();
            }
            Ok(())
        } => result,
        result = async {
            let mut line = Vec::new();
            while replies.read_until(b'\n', &mut line).await? != 0 {
                output.write_all(&line).await?;
                if let Ok(response) = serde_json::from_slice::<Value>(&line)
                    && response["result"]["state"] == "running"
                {
                    let _ = observed.send(Observation::Running(response["result"]["id"].clone()));
                }
                line.clear();
            }
            Ok(())
        } => result,
    };
    if let Some(agent) = subscribed_agent {
        let _ = observed.send(Observation::SubscriptionClosed(agent));
    }
    result
}

#[test]
fn wait_agent_ends_on_daemon_stop_and_archive() {
    let daemon = Daemon::running();
    let parent = daemon.agent(None, true);
    let probe = WaitProbe::new(&daemon.env.paths.socket);
    let mut mcp = Mcp::initialized(&probe.env, parent.as_str().expect("agent id"));
    let child = daemon.agent(Some(&parent), false);
    daemon.wait_for_event(&child, json!({"type":"state_changed","state":"running"}));
    let id = mcp.begin(
        "tools/call",
        json!({"name":"wait_agent","arguments":{"agent_id":child}}),
    );
    probe.wait_for_running(&child);
    daemon.rpc("agent.archive", json!({"id":child}));
    let archived = tool_json(&mcp.finish(id)["result"]);
    assert_eq!(archived["state"], "archived");
    assert_eq!(archived["timed_out"], false);

    let child = daemon.agent(Some(&parent), false);
    daemon.wait_for_event(&child, json!({"type":"state_changed","state":"running"}));
    let id = mcp.begin(
        "tools/call",
        json!({"name":"wait_agent","arguments":{"agent_id":child}}),
    );
    probe.wait_for_running(&child);
    assert_success(&daemon.env.run(&["daemon", "stop"]));
    assert_tool_error(&mcp.finish(id)["result"]);
}

#[test]
fn send_interrupt_archive_descendant() {
    let daemon = Daemon::new(|env| {
        fake_scenario(
            env,
            vec![
                json!({"expect":{"type":"user"}}),
                json!({"send":{"type":"system","subtype":"init","session_id":"mcp-test-interrupt"}}),
                json!({"expect":{"type":"control_request","request":{"subtype":"interrupt"}}}),
                json!({"capture":"interrupt","path":"/request_id"}),
                json!({"send":{"type":"control_response","response":{"subtype":"success","request_id":"${interrupt}","response":{}}}}),
                json!({"expect_eof":true}),
            ],
        )
    });
    let root = daemon.agent(None, true);
    let child = daemon.agent(Some(&root), false);
    let mut mcp = daemon.mcp(&root);
    assert_eq!(
        tool_json(&mcp.call(
            "send_agent",
            json!({"agent_id":child,"text":"queued","behavior":"queue"})
        )),
        json!({})
    );
    assert_eq!(
        tool_json(&mcp.call("interrupt_agent", json!({"agent_id":child}))),
        json!({})
    );
    assert_eq!(daemon.get(&child)["state"], "idle");
    let tail = daemon.rpc("agent.events", json!({"agent_id":child,"tail":50}));
    assert!(
        tail["events"]
            .as_array()
            .expect("events")
            .iter()
            .any(|event| {
                event["kind"]
                    == json!({"type":"queue_discarded","text":"queued","behavior":"queue"})
            })
    );
    assert_eq!(
        tool_json(&mcp.call("archive_agent", json!({"agent_id":child}))),
        json!({})
    );
    assert_eq!(daemon.get(&child)["state"], "archived");
}

#[test]
fn list_agents_returns_only_descendants() {
    let daemon = Daemon::idle();
    let root = daemon.agent(None, true);
    let caller = daemon.agent(Some(&root), true);
    let _sibling = daemon.agent(Some(&root), false);
    let _unrelated = daemon.agent(None, false);
    let child = daemon.agent(Some(&caller), false);
    let grandchild = daemon.agent(Some(&child), false);
    daemon.rpc("agent.archive", json!({"id":child}));
    let probe = WaitProbe::new(&daemon.env.paths.socket);
    let mut mcp = Mcp::initialized(&probe.env, caller.as_str().expect("agent id"));
    let result = tool_json(&mcp.call("list_agents", json!({})));
    let methods: Vec<_> = probe
        .observations
        .try_iter()
        .filter_map(|event| match event {
            Observation::Request(request) => request["method"].as_str().map(str::to_owned),
            _ => None,
        })
        .collect();
    assert_eq!(methods, ["hello", "agent.list"]);
    let agents = result["agents"].as_array().expect("agents");
    let ids: Vec<&Value> = agents.iter().map(|agent| &agent["agent_id"]).collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&&child));
    assert!(ids.contains(&&grandchild));
    assert_eq!(
        tool_json(&mcp.call("get_agent", json!({"agent_id":grandchild})))["title"],
        "test agent"
    );
}

#[test]
fn mcp_does_not_autostart_daemon() {
    let env = TestEnv::new();
    assert!(env.dir.path().is_absolute());
    let agent = dgc_protocol::ids::AgentId::generate();
    let mut mcp = Mcp::initialized(&env, agent.as_str());
    assert_tool_error(&mcp.call("list_agents", json!({})));
    assert!(!env.paths.socket.exists());
    assert!(env.lock_pid().is_none());
}

#[test]
fn stdout_carries_only_json_rpc() {
    let daemon = Daemon::new(|_| scenario("claude_permission"));
    let root = daemon.agent(None, true);
    let child = daemon.agent(Some(&root), false);
    let mut mcp = daemon.mcp(&root);
    mcp.raw(b"{bad JSON");
    assert_eq!(mcp.read()["error"]["code"], PARSE_ERROR);
    mcp.write(&json!([{"jsonrpc":"2.0","id":20,"method":"ping"}]));
    assert_eq!(mcp.read()["error"]["code"], INVALID_REQUEST);
    mcp.write(&json!({"jsonrpc":"2.0","id":null,"method":"ping"}));
    assert_eq!(mcp.read()["error"]["code"], INVALID_REQUEST);
    mcp.raw(b"{\r\"jsonrpc\":\"2.0\",\"id\":21,\"method\":\"ping\"}");
    assert_eq!(mcp.read()["error"]["code"], PARSE_ERROR);
    let waiting = tool_json(&mcp.call("wait_agent", json!({"agent_id":child})));
    assert_eq!(waiting["state"], "waiting_permission");
    assert_eq!(waiting["pending_permission"], true);
    assert_eq!(waiting["last_answer"], "");
    assert_tool_error(&mcp.call("create_agent", json!({"prompt":PRIVATE,"mode":PRIVATE})));

    // Give the server EOF and check for stray output through process exit.
    drop(mcp.input.take());
    assert!(matches!(
        mcp.lines.recv_timeout(WAIT),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
    assert!(mcp.child.wait().expect("MCP exits on EOF").success());
    let mut stderr = String::new();
    use std::io::Read;
    mcp.child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut stderr)
        .expect("read stderr");
    assert!(!stderr.contains(PRIVATE), "{stderr}");
}

#[test]
fn mcp_wait_authorizes_before_subscribing_and_reads_after_it() {
    let daemon = Daemon::running();
    let parent = daemon.agent(None, true);
    let child = daemon.agent(Some(&parent), false);
    let probe = WaitProbe::new(&daemon.env.paths.socket);
    let mut mcp = Mcp::initialized(&probe.env, parent.as_str().expect("agent id"));
    let result = tool_json(&mcp.call("wait_agent", json!({"agent_id":child,"timeout_secs":0})));
    assert_eq!(result["timed_out"], true);
    let order: Vec<_> = probe
        .observations
        .try_iter()
        .filter_map(|event| match event {
            Observation::Request(request) if request["method"] == "subscribe" => Some("subscribe"),
            Observation::Request(request)
                if request["method"] == "agent.get" && request["params"]["id"] == child =>
            {
                Some("get")
            }
            _ => None,
        })
        .collect();
    assert!(order.starts_with(&["get", "subscribe", "get"]), "{order:?}");
}

#[test]
fn mcp_wait_is_concurrent_and_can_be_cancelled() {
    let daemon = Daemon::running();
    let parent = daemon.agent(None, true);
    let child = daemon.agent(Some(&parent), false);
    let probe = WaitProbe::new(&daemon.env.paths.socket);
    let mut mcp = Mcp::initialized(&probe.env, parent.as_str().expect("agent id"));
    let wait_id = mcp.begin(
        "tools/call",
        json!({
            "name":"wait_agent","arguments":{"agent_id":child,"timeout_secs":u64::MAX}
        }),
    );
    probe.wait_for_running(&child);
    assert_eq!(mcp.request("ping", json!({}))["result"], json!({}));
    mcp.write(
        &json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{
            "requestId":wait_id,"reason":PRIVATE
        }}),
    );
    assert_eq!(mcp.request("ping", json!({}))["result"], json!({}));
    probe.wait_for_subscription_close(&child);
    let result = tool_json(&mcp.call("get_agent", json!({"agent_id":child})));
    assert_eq!(result["state"], "running");
    daemon.rpc("agent.archive", json!({"id":child}));
    assert_eq!(
        tool_json(&mcp.call("get_agent", json!({"agent_id":child})))["state"],
        "archived"
    );
    assert_eq!(mcp.request("ping", json!({}))["result"], json!({}));
}

#[test]
fn mcp_eof_cancels_pending_wait() {
    let daemon = Daemon::running();
    let parent = daemon.agent(None, true);
    let child = daemon.agent(Some(&parent), false);
    let probe = WaitProbe::new(&daemon.env.paths.socket);
    let mut mcp = Mcp::initialized(&probe.env, parent.as_str().expect("agent id"));
    mcp.begin(
        "tools/call",
        json!({
            "name":"wait_agent","arguments":{"agent_id":child,"timeout_secs":300}
        }),
    );
    probe.wait_for_running(&child);
    drop(mcp.input.take());
    assert!(matches!(
        mcp.lines.recv_timeout(WAIT),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
    assert!(mcp.child.wait().expect("MCP exits on EOF").success());
    assert_eq!(daemon.get(&child)["state"], "running");
}

#[test]
fn mcp_recovers_from_oversized_and_unterminated_frames() {
    let env = TestEnv::new();
    let agent = dgc_protocol::ids::AgentId::generate();
    let mut mcp = Mcp::initialized(&env, agent.as_str());
    let limit = dgc_protocol::MAX_MESSAGE_BYTES;
    let mut frame = vec![b' '; limit * 3];
    let embedded_request = br#"{"jsonrpc":"2.0","id":"embedded","method":"ping"}"#;
    frame.extend_from_slice(embedded_request);
    mcp.raw(&frame);
    let response = mcp.read();
    assert_eq!(response["error"]["code"], PARSE_ERROR);
    assert_eq!(response["id"], Value::Null);
    assert_eq!(mcp.request("ping", json!({}))["result"], json!({}));
    mcp.input
        .as_mut()
        .expect("stdin")
        .write_all(embedded_request)
        .expect("write partial frame");
    drop(mcp.input.take());
    let response = mcp.read();
    assert_eq!(response["error"]["code"], PARSE_ERROR);
    assert_eq!(response["id"], Value::Null);
    assert!(matches!(
        mcp.lines.recv_timeout(WAIT),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
    assert!(
        mcp.child
            .wait()
            .expect("MCP exits after parse error")
            .success()
    );
}

#[test]
fn malformed_tools_call_uses_json_rpc_error() {
    let env = TestEnv::new();
    let agent = dgc_protocol::ids::AgentId::generate();
    let mut mcp = Mcp::initialized(&env, agent.as_str());
    for params in [
        json!({"name":PRIVATE}),
        json!({}),
        json!({"name":42}),
        json!({"name":"list_agents","arguments":null}),
        json!({"name":"list_agents","arguments":[]}),
    ] {
        let response = mcp.request("tools/call", params);
        assert_eq!(response["error"]["code"], INVALID_PARAMS, "{response}");
        assert!(!response.to_string().contains(PRIVATE));
    }
}

#[test]
fn mcp_reads_last_answer_across_event_pages() {
    // Both turns exceed the daemon's page budget. The second stays in progress.
    let block = "x".repeat(160 * 1024);
    let daemon = Daemon::new(|env| {
        let mut steps = vec![
            json!({"expect":{"type":"user"}}),
            json!({"send":{"type":"system","subtype":"init","session_id":"mcp-test-long"}}),
            assistant_text("beginning of completed answer"),
        ];
        steps.extend((0..6).map(|_| assistant_text(&block)));
        steps.extend([
            json!({"send":{"type":"result","subtype":"success","is_error":false,"session_id":"mcp-test-long"}}),
            json!({"expect":{"type":"user"}}),
        ]);
        steps.extend((0..6).map(|_| assistant_text(&block)));
        steps.push(assistant_text("unfinished marker"));
        steps.push(json!({"expect_eof":true}));
        fake_scenario(env, steps)
    });
    let parent = daemon.agent(None, true);
    let child = daemon.agent(Some(&parent), false);
    let mut mcp = daemon.mcp(&parent);
    let first = tool_json(&mcp.call("wait_agent", json!({"agent_id":child})));
    let expected = format!("beginning of completed answer\n{block}");
    assert_eq!(first["last_answer"], &expected[..16 * 1024]);
    tool_json(&mcp.call("send_agent", json!({"agent_id":child,"text":"next"})));
    daemon.wait_for_event(
        &child,
        json!({"type":"assistant_text","text":"unfinished marker","final":true}),
    );
    let second = tool_json(&mcp.call("get_agent", json!({"agent_id":child})));
    assert_eq!(second["state"], "running");
    assert_eq!(second["last_answer"], first["last_answer"]);
}

#[test]
fn mcp_skips_completed_turn_with_error_before_tail_page() {
    let chunk = "failed output".repeat(13 * 1024);
    let daemon = Daemon::new(|env| {
        let success = json!({
            "send":{"type":"result","subtype":"success","is_error":false,"session_id":"mcp-test-before-error"}
        });
        let mut steps = vec![
            json!({"expect":{"type":"user"}}),
            json!({"send":{"type":"system","subtype":"init","session_id":"mcp-test-before-error"}}),
            assistant_text("previous clean answer"),
            success.clone(),
            json!({"expect":{"type":"user"}}),
            json!({"send":{"type":"assistant","is_api_error_message":true,"error":"synthetic error before long output"}}),
        ];
        steps.extend((0..6).map(|_| assistant_text(&chunk)));
        steps.push(success);
        steps.push(json!({"expect_eof":true}));
        fake_scenario(env, steps)
    });
    let parent = daemon.agent(None, true);
    let child = daemon.agent(Some(&parent), false);
    let mut mcp = daemon.mcp(&parent);
    let first = tool_json(&mcp.call("wait_agent", json!({"agent_id":child})));
    assert_eq!(first["last_answer"], "previous clean answer");
    tool_json(&mcp.call("send_agent", json!({"agent_id":child,"text":"next"})));
    let result = tool_json(&mcp.call("wait_agent", json!({"agent_id":child})));
    assert_eq!(result["state"], "idle");
    assert_eq!(result["last_answer"], "previous clean answer");
    let tail = daemon.rpc("agent.events", json!({"agent_id":child,"tail":1000}));
    assert!(
        !tail["events"]
            .as_array()
            .expect("tail")
            .iter()
            .any(|event| event["kind"]["type"] == "error")
    );
}
