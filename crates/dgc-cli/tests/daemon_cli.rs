//! End-to-end tests for `dgc-next daemon ...`. Every test runs against its own
//! `DGC_STATE_ROOT` and stops the daemon it started, even when it panics.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use dgc_daemon::lock::{InstanceLock, LockError};
use dgc_daemon::paths::DaemonPaths;
use dgc_protocol::methods::DaemonStatus;
use tempfile::{TempDir, tempdir};

struct TestEnv {
    dir: TempDir,
    paths: DaemonPaths,
}

impl TestEnv {
    fn new() -> Self {
        let dir = tempdir().expect("tempdir");
        let paths = DaemonPaths::under(dir.path());
        Self { dir, paths }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dgc-next"));
        command
            .args(args)
            .env("DGC_STATE_ROOT", self.dir.path())
            .env_remove("DGC_LOG")
            .stdin(Stdio::null());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("dgc-next should start")
    }

    fn lock_pid(&self) -> Option<i32> {
        std::fs::read_to_string(self.paths.state_dir.join("daemon.lock"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = self.command(&["daemon", "stop"]).output();
        // Fallback: if the lock is still held, the daemon ignored `stop`; kill
        // the recorded holder. Acquiring the lock proves the pid file is not
        // stale, so an unrelated process with a reused pid is never signalled.
        if !self.paths.state_dir.exists() {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match InstanceLock::acquire(&self.paths.state_dir) {
                Err(LockError::AlreadyRunning) => {
                    if let Some(pid) = self.lock_pid() {
                        // SAFETY: plain kill(2) on a pid read from the lock file.
                        unsafe { libc::kill(pid, libc::SIGKILL) };
                    }
                }
                _ => return,
            }
            if Instant::now() > deadline {
                return;
            }
            sleep(Duration::from_millis(50));
        }
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status {:?}\nstdout: {}\nstderr: {}",
        output.status,
        stdout(output),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn status_json(output: &Output) -> DaemonStatus {
    assert_success(output);
    serde_json::from_str(stdout(output).trim()).expect("stdout is DaemonStatus JSON")
}

fn socket_gone(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_err()
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
    assert!(text.contains("protocol_version: 1"), "{text}");
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
            .expect("dgc-next should start")
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
    assert_eq!(status.protocol_version, 1);
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
