use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use dgc_daemon::config::DaemonConfig;
use dgc_daemon::lock::LockError;
use dgc_daemon::paths::DaemonPaths;
use dgc_daemon::{DaemonError, run_daemon};
use dgc_protocol::methods::DaemonStatus;
use dgc_protocol::{MAX_MESSAGE_BYTES, PROTOCOL_VERSION, rpc};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

const WAIT: Duration = Duration::from_secs(10);

struct Running {
    _dir: TempDir,
    paths: DaemonPaths,
    shutdown: watch::Sender<bool>,
    task: JoinHandle<Result<(), DaemonError>>,
}

impl Running {
    async fn stop(self) -> Result<(), DaemonError> {
        self.shutdown.send_replace(true);
        timeout(WAIT, self.task)
            .await
            .expect("daemon did not stop in time")
            .expect("daemon task panicked")
    }
}

async fn wait_for_socket(path: &Path, task: &mut JoinHandle<Result<(), DaemonError>>) {
    let connect = async {
        // A stale socket file may already exist, so wait for a live listener.
        while UnixStream::connect(path).await.is_err() {
            sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::select! {
        biased;
        result = task => match result {
            Ok(Err(err)) => panic!("daemon failed before its socket was ready: {err}"),
            Err(err) => panic!("daemon task failed before its socket was ready: {err}"),
            Ok(Ok(())) => panic!("daemon stopped before its socket was ready"),
        },
        result = timeout(WAIT, connect) => {
            assert!(result.is_ok(), "socket {} never appeared", path.display());
        }
    }
}

async fn start() -> Running {
    let dir = tempdir().expect("tempdir");
    start_in(dir).await
}

/// Provider paths that do not exist, so tests never probe the host's real
/// `claude`/`codex` on `PATH`.
fn hermetic_config() -> DaemonConfig {
    DaemonConfig {
        claude_path: Some(PathBuf::from("/nonexistent/dgc-test/claude")),
        codex_path: Some(PathBuf::from("/nonexistent/dgc-test/codex")),
        ..DaemonConfig::default()
    }
}

async fn start_in(dir: TempDir) -> Running {
    start_with(dir, hermetic_config()).await
}

async fn start_with(dir: TempDir, config: DaemonConfig) -> Running {
    let paths = DaemonPaths::under(dir.path());
    let (shutdown, _rx) = watch::channel(false);
    let mut task = tokio::spawn(run_daemon(paths.clone(), config, shutdown.clone()));
    wait_for_socket(&paths.socket, &mut task).await;
    Running {
        _dir: dir,
        paths,
        shutdown,
        task,
    }
}

struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    next_id: u64,
}

impl Client {
    async fn connect(socket: &Path) -> Self {
        let stream = UnixStream::connect(socket).await.expect("connect");
        let (r, w) = stream.into_split();
        Self {
            reader: BufReader::new(r),
            writer: w,
            next_id: 100,
        }
    }

    async fn send_raw(&mut self, text: &str) {
        self.writer.write_all(text.as_bytes()).await.expect("write");
    }

    async fn send(&mut self, value: Value) {
        self.send_raw(&format!("{value}\n")).await;
    }

    async fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        self.recv().await
    }

    async fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = timeout(WAIT, self.reader.read_line(&mut line))
            .await
            .expect("timed out waiting for a response")
            .expect("read");
        assert!(n > 0, "connection closed while a response was expected");
        serde_json::from_str(&line).expect("response is JSON")
    }

    /// Send a request and read until its response. Notifications read
    /// meanwhile are returned with it.
    async fn call_collect(&mut self, method: &str, params: Value) -> (Value, Vec<Value>) {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        let mut notes = Vec::new();
        loop {
            let message = self.recv().await;
            if message.get("id").is_some() {
                assert_eq!(message["id"], id, "response to another request: {message}");
                return (message, notes);
            }
            notes.push(message);
        }
    }

    /// `call_collect` for a call that must succeed; returns its result.
    async fn call_ok(&mut self, method: &str, params: Value) -> Value {
        let (response, _) = self.call_collect(method, params).await;
        assert!(response["error"].is_null(), "{method} failed: {response}");
        response["result"].clone()
    }

    async fn hello(&mut self) -> Value {
        self.request(
            1,
            "hello",
            json!({"protocol_version": PROTOCOL_VERSION, "client": "test"}),
        )
        .await
    }

    /// Assert the server closed the connection without writing anything more.
    async fn assert_closed_silently(&mut self) {
        let mut rest = Vec::new();
        let res = timeout(WAIT, self.reader.read_to_end(&mut rest))
            .await
            .expect("timed out waiting for the server to close");
        match res {
            Ok(_) => {}
            Err(err) => assert!(
                matches!(
                    err.kind(),
                    io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
                ),
                "unexpected read error: {err}"
            ),
        }
        assert!(rest.is_empty(), "unexpected bytes after close: {rest:?}");
    }
}

fn error_code(response: &Value) -> i64 {
    response["error"]["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("no error code in {response}"))
}

#[tokio::test]
async fn socket_is_0600_and_dir_0700() {
    let daemon = start().await;
    let socket_mode = std::fs::metadata(&daemon.paths.socket)
        .expect("socket metadata")
        .permissions()
        .mode();
    assert_eq!(socket_mode & 0o777, 0o600);
    let parent = daemon.paths.socket.parent().expect("socket parent");
    let dir_mode = std::fs::metadata(parent)
        .expect("dir metadata")
        .permissions()
        .mode();
    assert_eq!(dir_mode & 0o777, 0o700);
    daemon.stop().await.expect("clean stop");
}

#[tokio::test]
async fn first_request_must_be_hello() {
    let daemon = start().await;
    let mut client = Client::connect(&daemon.paths.socket).await;
    let response = client.request(5, "daemon.status", json!({})).await;
    assert_eq!(error_code(&response), i64::from(rpc::INVALID_REQUEST));
    assert_eq!(response["id"], 5);
    client.assert_closed_silently().await;
    daemon.stop().await.expect("clean stop");
}

#[tokio::test]
async fn garbage_before_hello_is_invalid_request_and_closes() {
    let daemon = start().await;
    let mut client = Client::connect(&daemon.paths.socket).await;
    client.send_raw("this is not json\n").await;
    let response = client.recv().await;
    assert_eq!(error_code(&response), i64::from(rpc::INVALID_REQUEST));
    client.assert_closed_silently().await;
    daemon.stop().await.expect("clean stop");
}

#[tokio::test]
async fn hello_returns_protocol_version_and_pid() {
    let daemon = start().await;
    let mut client = Client::connect(&daemon.paths.socket).await;
    let response = client.hello().await;
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"]["protocol_version"], PROTOCOL_VERSION);
    assert_eq!(response["result"]["daemon_pid"], std::process::id());
    daemon.stop().await.expect("clean stop");
}

#[tokio::test]
async fn version_mismatch_is_rejected() {
    let daemon = start().await;
    for version in [1, PROTOCOL_VERSION + 1] {
        let mut client = Client::connect(&daemon.paths.socket).await;
        let response = client
            .request(
                1,
                "hello",
                json!({"protocol_version": version, "client": "test"}),
            )
            .await;
        assert_eq!(error_code(&response), i64::from(rpc::VERSION_MISMATCH));
        client.assert_closed_silently().await;
    }
    daemon.stop().await.expect("clean stop");
}

#[tokio::test]
async fn oversized_message_closes_connection() {
    let daemon = start().await;
    let mut client = Client::connect(&daemon.paths.socket).await;
    client.hello().await;

    let mut line = vec![b'a'; MAX_MESSAGE_BYTES + 1];
    line.push(b'\n');
    // The server may reset the connection before the whole line is written.
    let _ = client.writer.write_all(&line).await;
    client.assert_closed_silently().await;

    // The daemon itself keeps serving other connections.
    let mut other = Client::connect(&daemon.paths.socket).await;
    assert!(other.hello().await["result"].is_object());
    daemon.stop().await.expect("clean stop");
}

#[tokio::test]
async fn message_at_the_limit_is_accepted() {
    let daemon = start().await;
    let mut client = Client::connect(&daemon.paths.socket).await;
    client.hello().await;
    // A request padded with whitespace to exactly MAX_MESSAGE_BYTES (without
    // the newline) must still be served.
    let base = r#"{"jsonrpc":"2.0","id":2,"method":"daemon.status","params":{}}"#;
    let padded = format!("{base}{}", " ".repeat(MAX_MESSAGE_BYTES - base.len()));
    assert_eq!(padded.len(), MAX_MESSAGE_BYTES);
    client.send_raw(&format!("{padded}\n")).await;
    let response = client.recv().await;
    assert_eq!(response["id"], 2);
    assert!(response["result"].is_object());
    daemon.stop().await.expect("clean stop");
}

#[tokio::test]
async fn status_reports_pid_and_version() {
    let daemon = start().await;
    let mut client = Client::connect(&daemon.paths.socket).await;
    client.hello().await;
    let response = client.request(2, "daemon.status", json!({})).await;
    let status: DaemonStatus =
        serde_json::from_value(response["result"].clone()).expect("DaemonStatus");
    assert_eq!(status.pid, std::process::id());
    assert_eq!(status.protocol_version, PROTOCOL_VERSION);
    assert!(status.started_at.ends_with('Z'), "{}", status.started_at);
    // Both providers are reported; the hermetic config points at nothing.
    let kinds: Vec<_> = status.providers.iter().map(|p| p.kind).collect();
    assert_eq!(
        kinds,
        [
            dgc_protocol::model::ProviderKind::Claude,
            dgc_protocol::model::ProviderKind::Codex
        ]
    );
    assert!(
        status
            .providers
            .iter()
            .all(|p| p.path.is_none() && !p.tested)
    );
    assert_eq!(status.path_env, std::env::var("PATH").unwrap_or_default());
    daemon.stop().await.expect("clean stop");
}

#[tokio::test]
async fn malformed_json_after_hello_keeps_connection_open() {
    let daemon = start().await;
    let mut client = Client::connect(&daemon.paths.socket).await;
    client.hello().await;
    client.send_raw("{not json\n").await;
    let response = client.recv().await;
    assert_eq!(error_code(&response), i64::from(rpc::PARSE_ERROR));
    let status = client.request(3, "daemon.status", json!({})).await;
    assert_eq!(status["id"], 3);
    assert!(status["result"].is_object());
    daemon.stop().await.expect("clean stop");
}

#[tokio::test]
async fn wrong_jsonrpc_version_is_invalid_request() {
    let daemon = start().await;
    let mut client = Client::connect(&daemon.paths.socket).await;
    client.hello().await;
    client
        .send(json!({"jsonrpc": "1.0", "id": 9, "method": "daemon.status"}))
        .await;
    let response = client.recv().await;
    assert_eq!(error_code(&response), i64::from(rpc::INVALID_REQUEST));
    assert_eq!(response["id"], 9);
    // The connection stays usable.
    let status = client.request(10, "daemon.status", json!({})).await;
    assert!(status["result"].is_object());
    daemon.stop().await.expect("clean stop");
}

#[tokio::test]
async fn unknown_method_is_not_found_and_echo_is_truncated() {
    let daemon = start().await;
    let mut client = Client::connect(&daemon.paths.socket).await;
    client.hello().await;
    let long_name = "x".repeat(500);
    let response = client.request(4, &long_name, json!({})).await;
    assert_eq!(error_code(&response), i64::from(rpc::METHOD_NOT_FOUND));
    let message = response["error"]["message"].as_str().expect("message");
    assert!(!message.contains(&"x".repeat(65)), "echo not truncated");
    assert!(message.contains(&"x".repeat(64)));
    daemon.stop().await.expect("clean stop");
}

#[tokio::test]
async fn second_hello_is_rejected_but_connection_stays_open() {
    let daemon = start().await;
    let mut client = Client::connect(&daemon.paths.socket).await;
    client.hello().await;
    let again = client.hello().await;
    assert!(again["error"].is_object());
    let status = client.request(2, "daemon.status", json!({})).await;
    assert!(status["result"].is_object());
    daemon.stop().await.expect("clean stop");
}

#[tokio::test]
async fn shutdown_removes_socket_file() {
    let daemon = start().await;
    let socket = daemon.paths.socket.clone();
    assert!(socket.exists());
    daemon.stop().await.expect("clean stop");
    assert!(!socket.exists(), "socket file left behind");
}

#[tokio::test]
async fn shutdown_rpc_replies_then_stops_the_daemon() {
    let daemon = start().await;
    let socket = daemon.paths.socket.clone();
    let mut client = Client::connect(&socket).await;
    client.hello().await;
    let response = client.request(2, "daemon.shutdown", json!({})).await;
    assert_eq!(response["id"], 2);
    assert!(response["result"].is_object());
    let result = timeout(WAIT, daemon.task)
        .await
        .expect("daemon did not stop after daemon.shutdown")
        .expect("daemon task panicked");
    result.expect("clean stop");
    assert!(!socket.exists());
}

#[tokio::test]
async fn second_daemon_is_refused_while_the_first_runs() {
    let daemon = start().await;
    let (shutdown, _rx) = watch::channel(false);
    let err = run_daemon(daemon.paths.clone(), DaemonConfig::default(), shutdown)
        .await
        .expect_err("second daemon must not start");
    assert!(
        matches!(err, DaemonError::Lock(LockError::AlreadyRunning)),
        "{err:?}"
    );
    // The running daemon is unaffected and still owns its socket.
    let mut client = Client::connect(&daemon.paths.socket).await;
    assert!(client.hello().await["result"].is_object());
    daemon.stop().await.expect("clean stop");
}

#[tokio::test]
async fn stale_socket_file_is_replaced() {
    let dir = tempdir().expect("tempdir");
    let paths = DaemonPaths::under(dir.path());
    std::fs::create_dir_all(paths.socket.parent().expect("parent")).expect("mkdir");
    // Bind and drop: the socket file stays behind, as after a crash.
    drop(std::os::unix::net::UnixListener::bind(&paths.socket).expect("bind stale"));
    assert!(
        std::fs::symlink_metadata(&paths.socket)
            .expect("stale socket")
            .file_type()
            .is_socket()
    );

    let daemon = start_in(dir).await;
    let mut client = Client::connect(&daemon.paths.socket).await;
    assert!(client.hello().await["result"].is_object());
    daemon.stop().await.expect("clean stop");
}

#[tokio::test]
async fn regular_file_at_socket_path_is_not_removed() {
    let dir = tempdir().expect("tempdir");
    let paths = DaemonPaths::under(dir.path());
    std::fs::create_dir_all(paths.socket.parent().expect("parent")).expect("mkdir");
    std::fs::write(&paths.socket, "precious").expect("write file");
    let (shutdown, _rx) = watch::channel(false);
    let err = run_daemon(paths.clone(), DaemonConfig::default(), shutdown)
        .await
        .expect_err("must refuse");
    assert!(matches!(err, DaemonError::SocketPathOccupied(_)), "{err:?}");
    assert_eq!(std::fs::read(&paths.socket).expect("read"), b"precious");
}

#[cfg(target_os = "linux")]
fn socket_inodes_of_this_process() -> Vec<u64> {
    let mut inodes = Vec::new();
    for entry in std::fs::read_dir("/proc/self/fd").expect("read /proc/self/fd") {
        let Ok(entry) = entry else { continue };
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        let target = target.to_string_lossy().into_owned();
        if let Some(inode) = target
            .strip_prefix("socket:[")
            .and_then(|rest| rest.strip_suffix(']'))
            .and_then(|n| n.parse().ok())
        {
            inodes.push(inode);
        }
    }
    inodes
}

#[cfg(target_os = "linux")]
fn tcp_listen_inodes(table: &str) -> Vec<u64> {
    let text = std::fs::read_to_string(table).unwrap_or_default();
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            // sl local rem st tx:rx tr:when retrnsmt uid timeout inode
            (fields.get(3) == Some(&"0A")).then(|| fields.get(9)?.parse().ok())?
        })
        .collect()
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn no_tcp_listener_is_opened() {
    let daemon = start().await;
    // Hold a connection so the daemon is demonstrably serving.
    let mut client = Client::connect(&daemon.paths.socket).await;
    assert!(client.hello().await["result"].is_object());

    let ours = socket_inodes_of_this_process();
    assert!(!ours.is_empty(), "expected the daemon's unix sockets");
    for table in ["/proc/self/net/tcp", "/proc/self/net/tcp6"] {
        let listening = tcp_listen_inodes(table);
        for inode in &ours {
            assert!(
                !listening.contains(inode),
                "socket inode {inode} is a TCP LISTEN entry in {table}"
            );
        }
    }
    daemon.stop().await.expect("clean stop");
}

// --- agents, permissions and subscriptions ----------------------------------

const CLAUDE_INIT: [&str; 3] = [
    r#"{"expect":{"type":"control_request","request_id":"${ANY}","request":{"subtype":"initialize"}}}"#,
    r#"{"capture":"init","path":"/request_id"}"#,
    r#"{"send":{"type":"control_response","response":{"subtype":"success","request_id":"${init}","response":{}}}}"#,
];

/// A Claude Code scenario that answers each prompt (turn) with the given
/// assistant messages, each a list of text blocks.
fn claude_scenario(turns: &[Vec<Vec<String>>]) -> String {
    let mut lines: Vec<String> = CLAUDE_INIT.iter().map(|l| (*l).to_owned()).collect();
    for messages in turns {
        lines.push(json!({"expect": {"type": "user"}}).to_string());
        lines.push(
            json!({"send": {"type": "system", "subtype": "init", "session_id": "sess-srv"}})
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
                            "session_id": "sess-srv"}})
            .to_string(),
        );
    }
    lines.push(json!({"expect_eof": true}).to_string());
    lines.join("\n") + "\n"
}

fn text_turn(texts: &[&str]) -> Vec<Vec<String>> {
    vec![texts.iter().map(|t| (*t).to_owned()).collect()]
}

/// Write `scenario` and a 0700 `claude` wrapper (answering `--version`
/// itself) into `dir`; returns the wrapper path.
fn claude_wrapper(dir: &Path, scenario: &str) -> PathBuf {
    let scenario_path = dir.join("scenario.jsonl");
    std::fs::write(&scenario_path, scenario).expect("write scenario");
    let wrapper = dir.join("claude");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"--version\" ]; then echo '2.1.332 (Claude Code)'; exit 0; fi\n\
             DGC_FAKE_TIMEOUT_MS=60000 exec '{}' '{}' \"$@\"\n",
            env!("CARGO_BIN_EXE_dgc-fake-provider"),
            scenario_path.display()
        ),
    )
    .expect("write wrapper");
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700))
        .expect("chmod wrapper");
    wrapper
}

/// A daemon whose Claude is the fake replaying `scenario` (codex is
/// unavailable), plus a greeted client and one local workspace.
async fn start_agents(scenario: &str) -> (Running, Client, String) {
    start_agents_with_profiles(scenario, "").await
}

async fn start_agents_with_profiles(scenario: &str, profiles: &str) -> (Running, Client, String) {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, profiles).expect("write config");
    let config = DaemonConfig {
        claude_path: Some(claude_wrapper(dir.path(), scenario)),
        codex_path: hermetic_config().codex_path,
        ..DaemonConfig::load(&path).expect("load config")
    };
    let project_dir = std::fs::canonicalize(dir.path())
        .expect("canon")
        .join("project");
    std::fs::create_dir(&project_dir).expect("mkdir project");
    let daemon = start_with(dir, config).await;
    let mut client = Client::connect(&daemon.paths.socket).await;
    client.hello().await;
    let project = client
        .call_ok("project.add", json!({"path": project_dir}))
        .await;
    let workspace = client
        .call_ok(
            "workspace.create",
            json!({"project_id": project["id"], "isolation": "local"}),
        )
        .await;
    let ws = workspace["id"].as_str().expect("workspace id").to_owned();
    (daemon, client, ws)
}

#[tokio::test]
async fn invalid_profile_mode_fails_startup() {
    let dir = tempdir().expect("tempdir");
    let paths = DaemonPaths::under(dir.path());
    std::fs::create_dir_all(paths.config_file.parent().expect("config dir")).expect("mkdir");
    std::fs::write(
        &paths.config_file,
        "claude_path = '/nonexistent/dgc-test/claude'\n\
         codex_path = '/nonexistent/dgc-test/codex'\n\
         [profiles.wrong-mode]\nprovider = 'codex'\nmode = 'acceptEdits'\n",
    )
    .expect("write config");
    let loaded = DaemonConfig::load(&paths.config_file).expect_err("load accepted invalid mode");
    let text = std::fs::read_to_string(&paths.config_file).expect("read config");
    let config = toml::from_str(&text).expect("deserialize settings before semantic validation");
    // Also exercise the public startup function with a directly constructed
    // config. Pre-signal shutdown in case startup accepts it.
    let (shutdown, _rx) = watch::channel(true);
    let startup = timeout(WAIT, run_daemon(paths.clone(), config, shutdown))
        .await
        .expect("daemon did not return")
        .expect_err("daemon accepted invalid mode");
    for message in [loaded.to_string(), startup.to_string()] {
        assert!(
            message.contains("wrong-mode") && message.contains("acceptEdits"),
            "{message}"
        );
        assert!(message.contains("Codex mode"), "{message}");
    }
    assert!(!paths.socket.exists());
}

const PROFILES: &str = "
[profiles.z-safe]
provider = 'codex'
mode = 'read-only'

[profiles.a-fast]
provider = 'claude'
model = 'haiku'
mode = 'bypassPermissions'
notes = 'Fast review'
";

#[tokio::test]
async fn profile_list_reports_auto_approve() {
    let scenario = claude_scenario(&[text_turn(&["one"])]);
    let (daemon, mut client, _) = start_agents_with_profiles(&scenario, PROFILES).await;
    let (response, _) = client.call_collect("profile.list", json!({})).await;
    daemon.stop().await.expect("stop");
    assert_eq!(
        response["result"],
        json!({"profiles": [
            {"name": "a-fast", "provider": "claude", "model": "haiku",
             "mode": "bypassPermissions", "notes": "Fast review", "auto_approve": true},
            {"name": "z-safe", "provider": "codex", "model": null,
             "mode": "read-only", "notes": null, "auto_approve": false}
        ]}),
        "{response}"
    );
}

#[tokio::test]
async fn create_with_profile_applies_fields_and_explicit_flags_win() {
    let scenario = claude_scenario(&[text_turn(&["one"])]);
    let (daemon, mut client, ws) = start_agents_with_profiles(&scenario, PROFILES).await;
    let (inherited, _) = client
        .call_collect(
            "agent.create",
            json!({"workspace_id": ws, "title": "inherited", "initial_prompt": "hello",
                   "profile": "a-fast"}),
        )
        .await;
    let (overridden, _) = client
        .call_collect(
            "agent.create",
            json!({"workspace_id": ws, "title": "overridden", "initial_prompt": "hello",
                   "profile": "a-fast", "provider": "claude", "model": "sonnet", "mode": "manual"}),
        )
        .await;
    daemon.stop().await.expect("stop");
    for (response, model, mode, auto_approve) in [
        (inherited, "haiku", "bypassPermissions", true),
        (overridden, "sonnet", "manual", false),
    ] {
        let agent = &response["result"];
        assert_eq!(agent["provider"], "claude", "{response}");
        assert_eq!(agent["model"], model);
        assert_eq!(agent["mode"], mode);
        assert_eq!(agent["auto_approve"], auto_approve);
    }
}

#[tokio::test]
async fn profile_provider_conflict_is_invalid_params() {
    let scenario = claude_scenario(&[text_turn(&["one"])]);
    let (daemon, mut client, ws) = start_agents_with_profiles(&scenario, PROFILES).await;
    let mut errors = Vec::new();
    for (fields, message) in [
        (
            json!({"profile": "a-fast", "provider": "codex"}),
            "profile a-fast is for claude",
        ),
        (json!({"profile": "absent"}), "unknown profile"),
        (json!({}), "provider"),
    ] {
        let mut params = json!({"workspace_id": ws, "title": "t", "initial_prompt": "hello"});
        params
            .as_object_mut()
            .expect("params")
            .extend(fields.as_object().expect("fields").clone());
        let (response, _) = client.call_collect("agent.create", params).await;
        errors.push((response, message));
    }
    let listed = client.call_ok("agent.list", json!({})).await;
    daemon.stop().await.expect("stop");
    for (response, message) in errors {
        assert_eq!(
            error_code(&response),
            i64::from(rpc::INVALID_PARAMS),
            "{response}"
        );
        assert!(
            response["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains(message)),
            "{response}"
        );
    }
    assert_eq!(listed["agents"], json!([]));
}

async fn create_agent(client: &mut Client, ws: &str) -> String {
    let agent = client
        .call_ok(
            "agent.create",
            json!({"workspace_id": ws, "title": "t", "provider": "claude",
                   "initial_prompt": "hello"}),
        )
        .await;
    agent["id"].as_str().expect("agent id").to_owned()
}

async fn wait_state(client: &mut Client, agent: &str, state: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let got = client.call_ok("agent.get", json!({"id": agent})).await;
        if got["state"] == state {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "agent never reached {state}: {got}"
        );
        sleep(Duration::from_millis(20)).await;
    }
}

fn event_notes(notes: &[Value]) -> Vec<Value> {
    notes
        .iter()
        .filter(|n| n["method"] == "event")
        .map(|n| n["params"].clone())
        .collect()
}

fn is_state(event: &Value, state: &str) -> bool {
    event["kind"]["type"] == "state_changed" && event["kind"]["state"] == state
}

/// Read notifications until `until` matches an event; returns all events read.
async fn read_events_until(client: &mut Client, until: impl Fn(&Value) -> bool) -> Vec<Value> {
    let mut events = Vec::new();
    loop {
        let message = client.recv().await;
        assert_eq!(message["method"], "event", "unexpected message {message}");
        let event = message["params"].clone();
        let done = until(&event);
        events.push(event);
        if done {
            return events;
        }
    }
}

fn seqs(events: &[Value]) -> Vec<u64> {
    events
        .iter()
        .map(|e| e["seq"].as_u64().expect("seq"))
        .collect()
}

#[tokio::test]
async fn agent_and_permission_methods_map_results_and_errors() {
    let scenario = claude_scenario(&[text_turn(&["one"])]);
    let (daemon, mut client, ws) = start_agents(&scenario).await;

    let (missing, _) = client
        .call_collect(
            "agent.create",
            json!({"workspace_id": ws, "title": "t", "provider": "codex",
                   "initial_prompt": "x"}),
        )
        .await;
    assert_eq!(error_code(&missing), i64::from(rpc::INVALID_PARAMS));
    assert!(
        missing["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("not available")),
        "{missing}"
    );
    let (bad, _) = client
        .call_collect("agent.get", json!({"id": "nope"}))
        .await;
    assert_eq!(error_code(&bad), i64::from(rpc::INVALID_PARAMS));
    let unknown = dgc_protocol::ids::AgentId::generate();
    let (unknown, _) = client
        .call_collect("agent.get", json!({"id": unknown}))
        .await;
    assert_eq!(error_code(&unknown), i64::from(rpc::NOT_FOUND));

    let agent = create_agent(&mut client, &ws).await;
    wait_state(&mut client, &agent, "idle").await;

    let listed = client.call_ok("agent.list", json!({})).await;
    assert_eq!(listed["agents"][0]["id"], agent.as_str());
    let page = client
        .call_ok("agent.events", json!({"agent_id": agent, "limit": 2}))
        .await;
    assert_eq!(seqs(page["events"].as_array().expect("events")), [1, 2]);
    let permissions = client.call_ok("permission.list", json!({})).await;
    assert_eq!(permissions, json!({"permissions": []}));
    let (idle, _) = client
        .call_collect("agent.interrupt", json!({"id": agent}))
        .await;
    assert_eq!(error_code(&idle), i64::from(rpc::INVALID_STATE));
    let (unknown_perm, _) = client
        .call_collect(
            "permission.respond",
            json!({"id": dgc_protocol::ids::PermissionId::generate(), "decision": "allow"}),
        )
        .await;
    assert_eq!(error_code(&unknown_perm), i64::from(rpc::NOT_FOUND));

    let (no_agent, _) = client
        .call_collect("subscribe", json!({"since_seq": 3}))
        .await;
    assert_eq!(error_code(&no_agent), i64::from(rpc::INVALID_PARAMS));
    assert_eq!(client.call_ok("unsubscribe", json!({})).await, json!({}));

    let archived = client.call_ok("agent.archive", json!({"id": agent})).await;
    assert_eq!(archived["state"], "archived");
    let active = client.call_ok("agent.list", json!({})).await;
    assert_eq!(active, json!({"agents": []}));
    let all = client
        .call_ok("agent.list", json!({"include_archived": true}))
        .await;
    assert_eq!(all["agents"][0]["state"], "archived");
    daemon.stop().await.expect("clean stop");
}

#[tokio::test]
async fn subscribe_backfills_across_byte_limited_pages() {
    // 400 messages of ~4 KiB: the stored backlog spans several pages cut by
    // size (well under the 1000-event limit each).
    let texts: Vec<String> = (0..400)
        .map(|n| format!("m{n:03} {}", "x".repeat(4096)))
        .collect();
    let blocks: Vec<Vec<String>> = texts.iter().map(|t| vec![t.clone()]).collect();
    let scenario = claude_scenario(&[blocks]);
    let (daemon, mut control, ws) = start_agents(&scenario).await;
    let agent = create_agent(&mut control, &ws).await;
    wait_state(&mut control, &agent, "idle").await;

    let mut sub = Client::connect(&daemon.paths.socket).await;
    sub.hello().await;
    let (_, notes) = sub
        .call_collect("subscribe", json!({"agent_id": agent, "since_seq": 0}))
        .await;
    let mut events = event_notes(&notes);
    if !events.iter().any(|e| is_state(e, "idle")) {
        events.extend(read_events_until(&mut sub, |e| is_state(e, "idle")).await);
    }
    let expected: Vec<u64> = (1..=events.len() as u64).collect();
    assert_eq!(seqs(&events), expected, "no gaps or duplicates");
    let got = events
        .iter()
        .filter(|e| e["kind"]["type"] == "assistant_text")
        .count();
    assert_eq!(got, 400);
    daemon.stop().await.expect("clean stop");
}

#[tokio::test]
async fn subscribe_backfills_then_streams_live() {
    let scenario = claude_scenario(&[text_turn(&["one"]), text_turn(&["two"])]);
    let (daemon, mut control, ws) = start_agents(&scenario).await;
    let agent = create_agent(&mut control, &ws).await;
    wait_state(&mut control, &agent, "idle").await;

    let mut sub = Client::connect(&daemon.paths.socket).await;
    sub.hello().await;
    let (response, notes) = sub
        .call_collect("subscribe", json!({"agent_id": agent, "since_seq": 0}))
        .await;
    assert_eq!(response["result"], json!({}));
    // Clients buffer only a bounded number of notifications while waiting
    // for a response, so the backfill must follow the response.
    assert!(
        notes.is_empty(),
        "events before the subscribe response: {notes:?}"
    );
    let mut events = read_events_until(&mut sub, |e| is_state(e, "idle")).await;
    let backfilled = events.len();

    let sent = control
        .call_ok("agent.send", json!({"agent_id": agent, "text": "next"}))
        .await;
    assert_eq!(sent, json!({"outcome": "sent"}));
    events.extend(read_events_until(&mut sub, |e| is_state(e, "idle")).await);

    let expected: Vec<u64> = (1..=events.len() as u64).collect();
    assert_eq!(seqs(&events), expected, "no gaps or duplicates");
    assert!(events.len() > backfilled);
    let texts: Vec<&Value> = events
        .iter()
        .filter(|e| e["kind"]["type"] == "assistant_text")
        .map(|e| &e["kind"]["text"])
        .collect();
    assert_eq!(texts, [&json!("one"), &json!("two")]);

    // A later subscriber resumes after the seq it already has.
    let last = events.len() as u64;
    let mut late = Client::connect(&daemon.paths.socket).await;
    late.hello().await;
    let (_, notes) = late
        .call_collect(
            "subscribe",
            json!({"agent_id": agent, "since_seq": last - 2}),
        )
        .await;
    let mut tail = event_notes(&notes);
    while tail.len() < 2 {
        tail.extend(read_events_until(&mut late, |_| true).await);
    }
    assert_eq!(seqs(&tail), [last - 1, last]);
    daemon.stop().await.expect("clean stop");
}

#[tokio::test]
async fn lagged_subscriber_receives_lagged_notice() {
    // Turn 2 emits 4000 events of ~1 KiB: far more than the socket buffer
    // plus the connection's 1024-entry outbound queue.
    let block = "x".repeat(1000);
    let burst: Vec<Vec<String>> = (0..40).map(|_| vec![block.clone(); 100]).collect();
    let scenario = claude_scenario(&[text_turn(&["ready"]), burst]);
    let (daemon, mut control, ws) = start_agents(&scenario).await;
    let agent = create_agent(&mut control, &ws).await;
    wait_state(&mut control, &agent, "idle").await;

    let mut sub = Client::connect(&daemon.paths.socket).await;
    sub.hello().await;
    let (_, notes) = sub
        .call_collect("subscribe", json!({"agent_id": agent, "since_seq": 0}))
        .await;
    let mut events = event_notes(&notes);
    // From here `sub` stops reading.
    control
        .call_ok("agent.send", json!({"agent_id": agent, "text": "go"}))
        .await;
    // A client that never reads must not block anyone else.
    let mut other = Client::connect(&daemon.paths.socket).await;
    other.hello().await;
    let status = timeout(
        Duration::from_secs(2),
        other.call_ok("daemon.status", json!({})),
    )
    .await
    .expect("daemon.status must not wait for a stuck subscriber");
    assert!(status["pid"].is_number());
    wait_state(&mut control, &agent, "idle").await;
    timeout(
        Duration::from_secs(2),
        other.call_ok("daemon.status", json!({})),
    )
    .await
    .expect("daemon.status after the burst");

    let lagged = loop {
        let message = sub.recv().await;
        match message["method"].as_str() {
            Some("event") => events.push(message["params"].clone()),
            Some("lagged") => break message["params"].clone(),
            _ => panic!("unexpected message {message}"),
        }
    };
    let delivered = seqs(&events);
    let expected: Vec<u64> = (1..=delivered.len() as u64).collect();
    assert_eq!(
        delivered, expected,
        "events before the notice are contiguous"
    );
    let total = control
        .call_ok(
            "agent.events",
            json!({"agent_id": agent, "since_seq": delivered.len()}),
        )
        .await;
    assert!(
        !total["events"].as_array().expect("events").is_empty(),
        "the subscriber must have missed events"
    );
    assert_eq!(lagged["agent_id"], agent.as_str());
    assert_eq!(lagged["next_seq"], delivered.len() as u64 + 1);
    // The subscription ended with the notice: the next message is a response.
    let (response, notes) = sub.call_collect("daemon.status", json!({})).await;
    assert!(response["result"]["pid"].is_number());
    assert!(notes.is_empty(), "nothing after lagged: {notes:?}");
    daemon.stop().await.expect("clean stop");
}

#[tokio::test]
async fn restart_stops_agents_and_shutdown_stops_live_sessions() {
    let scenario = claude_scenario(&[text_turn(&["one"])]);
    let (daemon, mut client, ws) = start_agents(&scenario).await;
    let agent = create_agent(&mut client, &ws).await;
    wait_state(&mut client, &agent, "idle").await;
    let state_dir = daemon.paths.state_dir.clone();
    let socket = daemon.paths.socket.clone();
    let dir = daemon._dir;
    daemon.shutdown.send_replace(true);
    timeout(WAIT, daemon.task)
        .await
        .expect("daemon did not stop in time")
        .expect("daemon task panicked")
        .expect("clean stop");
    assert!(!socket.exists());

    let store = dgc_daemon::store::Store::open(&state_dir).expect("store");
    let id: dgc_protocol::ids::AgentId = agent.parse().expect("agent id");
    let mut stored = store.get_agent(&id).expect("read").expect("agent");
    assert_eq!(stored.state, dgc_protocol::model::AgentState::Stopped);

    // A record left `running` by a crash is stopped at the next start.
    stored.state = dgc_protocol::model::AgentState::Running;
    store.put_agent(&stored).expect("write");
    drop(store);
    let daemon = start_with(dir, hermetic_config()).await;
    let mut client = Client::connect(&daemon.paths.socket).await;
    client.hello().await;
    let got = client.call_ok("agent.get", json!({"id": agent})).await;
    assert_eq!(got["state"], "stopped");
    daemon.stop().await.expect("clean stop");
}

#[tokio::test]
#[should_panic(expected = "exists and is not a socket")]
async fn wait_for_socket_reports_a_failed_daemon_start() {
    let dir = tempdir().expect("tempdir");
    let paths = DaemonPaths::under(dir.path());
    std::fs::create_dir_all(paths.socket.parent().expect("socket parent")).expect("run dir");
    std::fs::write(&paths.socket, "regular file").expect("create conflicting file");
    start_in(dir).await;
}

#[tokio::test]
async fn old_protocol_hello_is_refused() {
    let daemon = start().await;
    let mut client = Client::connect(&daemon.paths.socket).await;
    let response = client
        .request(1, "hello", json!({"protocol_version": 2, "client": "test"}))
        .await;
    assert_eq!(error_code(&response), i64::from(rpc::VERSION_MISMATCH));
    client.assert_closed_silently().await;
    daemon.stop().await.expect("clean stop");
}

#[tokio::test]
async fn schedule_rpcs_round_trip() {
    let (daemon, mut client, ws) = start_agents(include_str!(
        "fixtures/claude/scenarios/schedule/complete.jsonl"
    ))
    .await;
    let created = client
        .call_ok(
            "schedule.create",
            json!({
                "name": "Nightly",
                "target": {"kind": "new_agent", "workspace_id": ws, "provider": "claude"},
                "prompt": "hello",
                "cron": "@daily"
            }),
        )
        .await;
    let params = json!({"id": created["id"]});
    assert!(
        created["id"]
            .as_str()
            .expect("schedule ID")
            .starts_with("sch_")
    );
    assert_eq!(created["state"], "active");
    assert_eq!(
        client.call_ok("schedule.get", params.clone()).await,
        created
    );
    let listed = client
        .call_ok("schedule.list", json!({"kind": "new_agent"}))
        .await;
    let entries = listed["schedules"].as_array().expect("schedule entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["id"], created["id"]);
    assert_eq!(entries[0]["target"], created["target"]);
    assert!(entries[0].get("prompt").is_none());
    assert_eq!(
        client
            .call_ok("schedule.list", json!({"kind": "agent"}))
            .await["schedules"],
        json!([])
    );
    let (invalid_kind, _) = client
        .call_collect("schedule.list", json!({"kind": "invalid"}))
        .await;
    assert_eq!(error_code(&invalid_kind), i64::from(rpc::INVALID_PARAMS));

    assert_eq!(
        client.call_ok("schedule.pause", params.clone()).await["state"],
        "paused"
    );
    let resumed = client.call_ok("schedule.resume", params.clone()).await;
    assert_eq!(resumed["state"], "active");

    let run = client.call_ok("schedule.run", params.clone()).await;
    assert_eq!(run["outcome"]["kind"], "started");
    let after = client.call_ok("schedule.get", params.clone()).await;
    assert_eq!(after["runs"], 1);
    assert_eq!(after["next_run_at"], resumed["next_run_at"]);
    assert_eq!(after["recent_runs"], json!([run]));

    assert_eq!(
        client.call_ok("schedule.remove", params.clone()).await,
        json!({})
    );
    assert_eq!(
        client.call_ok("schedule.list", json!({})).await,
        json!({"schedules": []})
    );
    let (missing, _) = client.call_collect("schedule.get", params).await;
    assert_eq!(error_code(&missing), i64::from(rpc::NOT_FOUND));
    daemon.stop().await.expect("clean stop");
}
