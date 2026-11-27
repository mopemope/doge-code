//! End-to-end tests for `dgc daemon ...`. The harness in `common` gives each
//! test its own `DGC_STATE_ROOT` and stops the daemon it started.

mod common;

use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use common::{TestEnv, assert_success, stdout};
use dgc_daemon::lock::InstanceLock;
use dgc_daemon::paths::DaemonPaths;
use dgc_protocol::PROTOCOL_VERSION;
use dgc_protocol::methods::DaemonStatus;
use dgc_protocol::rpc::{Request, Response, RpcError, VERSION_MISMATCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};

fn status_json(output: &Output) -> DaemonStatus {
    assert_success(output);
    serde_json::from_str(stdout(output).trim()).expect("stdout is DaemonStatus JSON")
}

fn socket_gone(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_err()
}

struct MismatchedDaemon(Child);

impl MismatchedDaemon {
    fn spawn(env: &TestEnv, lock: &str) -> Self {
        let mut child = Self(
            Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", "mismatched_daemon_test_helper", "--nocapture"])
                .env("DGC_STATE_ROOT", env.dir.path())
                .env("DGC_TEST_MISMATCHED_DAEMON", lock)
                .stdout(Stdio::null())
                .spawn()
                .expect("start mismatched daemon"),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while socket_gone(&env.paths.socket) {
            assert!(
                child.0.try_wait().expect("poll fake daemon").is_none(),
                "fake daemon exited during startup"
            );
            assert!(
                Instant::now() < deadline,
                "mismatched daemon did not listen"
            );
            sleep(Duration::from_millis(20));
        }
        assert_eq!(env.lock_pid(), i32::try_from(child.0.id()).ok());
        child
    }
}

impl Drop for MismatchedDaemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn mismatched_daemon_test_helper() {
    let Ok(lock) = std::env::var("DGC_TEST_MISMATCHED_DAEMON") else {
        return;
    };
    let root = std::env::var_os("DGC_STATE_ROOT").expect("test state root");
    let paths = DaemonPaths::under(Path::new(&root));
    std::fs::create_dir_all(paths.socket.parent().expect("socket parent")).expect("socket dir");
    std::fs::create_dir_all(&paths.state_dir).expect("state dir");
    let _lock = if lock == "held" {
        Some(InstanceLock::acquire(&paths.state_dir).expect("daemon lock"))
    } else {
        std::fs::write(
            paths.state_dir.join("daemon.lock"),
            format!("{}\n", std::process::id()),
        )
        .expect("stale pid");
        None
    };

    let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");
    let listener = UnixListener::bind(&paths.socket).expect("daemon socket");
    let (mut stream, _) = listener.accept().await.expect("client");
    let mut line = String::new();
    BufReader::new(&mut stream)
        .read_line(&mut line)
        .await
        .expect("hello request line");
    let hello: Request = serde_json::from_str(&line).expect("hello request");
    assert_eq!(hello.method, "hello");
    let reply = Response::err(hello.id, RpcError::new(VERSION_MISMATCH, "daemon speaks 1"));
    let line = format!(
        "{}\n",
        serde_json::to_string(&reply).expect("hello response")
    );
    stream
        .write_all(line.as_bytes())
        .await
        .expect("refuse hello");
    drop(stream);
    sigterm.recv().await.expect("receive SIGTERM");
    std::fs::remove_file(&paths.socket).expect("remove daemon socket");
}

#[test]
fn daemon_stop_signals_a_mismatched_daemon_holding_the_lock() {
    let env = TestEnv::new();
    let mut daemon = MismatchedDaemon::spawn(&env, "held");

    let stopped = env.run(&["daemon", "stop"]);
    assert_success(&stopped);
    assert!(stdout(&stopped).contains("daemon stopped"));
    assert!(socket_gone(&env.paths.socket));
    assert!(
        daemon
            .0
            .wait()
            .expect("fake daemon exited after SIGTERM")
            .success()
    );
    InstanceLock::acquire(&env.paths.state_dir).expect("instance lock released");
}

#[test]
fn daemon_stop_does_not_signal_a_mismatched_daemon_with_a_free_lock() {
    let env = TestEnv::new();
    let mut daemon = MismatchedDaemon::spawn(&env, "free");
    let pid = env.lock_pid().expect("stale pid");

    let stopped = env.run(&["daemon", "stop"]);
    assert!(!stopped.status.success());
    let error = String::from_utf8_lossy(&stopped.stderr);
    assert!(
        error.contains("no daemon holds the instance lock"),
        "{error}"
    );
    assert!(
        daemon.0.try_wait().expect("poll helper").is_none(),
        "stale pid was signalled"
    );
    assert_eq!(
        env.lock_pid(),
        Some(pid),
        "the lock probe changed the recorded pid"
    );
}

#[test]
fn status_autostarts_daemon() {
    let env = TestEnv::new();
    assert!(socket_gone(&env.paths.socket));

    let output = env.run(&["daemon", "status"]);
    assert_success(&output);
    let text = stdout(&output);
    let pid = env.lock_pid().expect("daemon.lock records the daemon pid");
    assert!(text.contains(&format!("pid: {pid}")), "{text}");
    assert!(
        text.contains(&format!("protocol_version: {PROTOCOL_VERSION}")),
        "{text}"
    );
    assert!(text.contains("started_at: "), "{text}");
    assert!(text.contains("PATH: "), "{text}");
    assert!(env.paths.socket.exists());
    assert!(
        env.paths.state_dir.join("daemon.log").exists(),
        "daemon output goes to daemon.log"
    );

    // A second status reuses the running daemon.
    let again = env.run(&["--json", "daemon", "status"]);
    assert_eq!(i64::from(status_json(&again).pid), i64::from(pid));
}

#[test]
fn concurrent_autostart_yields_single_daemon() {
    let env = TestEnv::new();
    let spawn = || {
        env.command(&["--json", "daemon", "status"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("dgc should start")
    };
    let first = spawn();
    let second = spawn();
    let first = status_json(&first.wait_with_output().expect("first finishes"));
    let second = status_json(&second.wait_with_output().expect("second finishes"));
    assert_eq!(first.pid, second.pid);
    assert_eq!(
        env.lock_pid().expect("lock pid"),
        i32::try_from(first.pid).expect("pid fits")
    );
}

#[test]
fn stop_removes_socket() {
    let env = TestEnv::new();
    assert_success(&env.run(&["daemon", "status"]));
    assert!(env.paths.socket.exists());

    let stopped = env.run(&["daemon", "stop"]);
    assert_success(&stopped);
    assert!(socket_gone(&env.paths.socket));

    let again = env.run(&["daemon", "stop"]);
    assert_success(&again);
    assert!(stdout(&again).contains("daemon is not running"));

    // The lock is free again, so an immediate autostart succeeds.
    assert_success(&env.run(&["daemon", "status"]));
}

#[test]
fn json_output_is_parseable() {
    let env = TestEnv::new();
    let output = env.run(&["--json", "daemon", "status"]);
    assert_success(&output);
    let text = stdout(&output);
    assert_eq!(text.trim_end().lines().count(), 1, "one JSON line: {text}");
    let status: DaemonStatus = serde_json::from_str(text.trim()).expect("DaemonStatus JSON");
    assert_eq!(status.protocol_version, PROTOCOL_VERSION);
    assert!(!status.started_at.is_empty());
}

#[test]
fn start_reports_already_running() {
    let env = TestEnv::new();
    let first = env.run(&["daemon", "start"]);
    assert_success(&first);
    let pid = env.lock_pid().expect("lock pid");
    assert!(env.paths.socket.exists());

    let second = env.run(&["daemon", "start"]);
    assert_success(&second);
    assert!(
        stdout(&second).contains(&format!("already running (pid {pid})")),
        "{}",
        stdout(&second)
    );
}

#[test]
fn run_detach_returns_with_daemon_up() {
    let env = TestEnv::new();
    assert_success(&env.run(&["daemon", "run", "--detach"]));
    assert!(env.paths.socket.exists());
    assert_success(&env.run(&["daemon", "status"]));
}

#[test]
fn foreground_run_stops_on_sigterm() {
    let env = TestEnv::new();
    let mut child = env
        .command(&["daemon", "run"])
        .stderr(Stdio::null())
        .spawn()
        .expect("daemon run starts");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !env.paths.socket.exists() {
        assert!(Instant::now() < deadline, "socket never appeared");
        sleep(Duration::from_millis(20));
    }
    let pid = i32::try_from(child.id()).expect("pid fits");
    // SAFETY: plain kill(2) on our own child.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    let status = child.wait().expect("child exits");
    assert!(status.success(), "{status:?}");
    assert!(socket_gone(&env.paths.socket));
}
