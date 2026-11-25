use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;
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

async fn wait_for_socket(path: &Path) {
    for _ in 0..500 {
        // A stale socket file may already exist, so wait for a live listener.
        if UnixStream::connect(path).await.is_ok() {
            return;
        }
        sleep(Duration::from_millis(10)).await;
    }
    panic!("socket {} never appeared", path.display());
}

async fn start() -> Running {
    let dir = tempdir().expect("tempdir");
    start_in(dir).await
}

async fn start_in(dir: TempDir) -> Running {
    let paths = DaemonPaths::under(dir.path());
    let (shutdown, _rx) = watch::channel(false);
    let task = tokio::spawn(run_daemon(
        paths.clone(),
        DaemonConfig::default(),
        shutdown.clone(),
    ));
    wait_for_socket(&paths.socket).await;
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
}

impl Client {
    async fn connect(socket: &Path) -> Self {
        let stream = UnixStream::connect(socket).await.expect("connect");
        let (r, w) = stream.into_split();
        Self {
            reader: BufReader::new(r),
            writer: w,
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
    let mut client = Client::connect(&daemon.paths.socket).await;
    let response = client
        .request(
            1,
            "hello",
            json!({"protocol_version": PROTOCOL_VERSION + 1, "client": "test"}),
        )
        .await;
    assert_eq!(error_code(&response), i64::from(rpc::VERSION_MISMATCH));
    client.assert_closed_silently().await;
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
    assert!(status.providers.is_empty());
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
