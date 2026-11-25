//! Shared harness for the `dgc-next` end-to-end tests. Every test runs against
//! its own `DGC_STATE_ROOT` and stops the daemon it started, even when it panics.
#![allow(dead_code)]

use std::process::{Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use dgc_daemon::lock::{InstanceLock, LockError};
use dgc_daemon::paths::DaemonPaths;
use tempfile::{TempDir, tempdir};

pub struct TestEnv {
    pub dir: TempDir,
    pub paths: DaemonPaths,
}

impl TestEnv {
    pub fn new() -> Self {
        let dir = tempdir().expect("tempdir");
        let paths = DaemonPaths::under(dir.path());
        Self { dir, paths }
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dgc-next"));
        command
            .args(args)
            .env("DGC_STATE_ROOT", self.dir.path())
            .env_remove("DGC_LOG")
            .stdin(Stdio::null());
        command
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("dgc-next should start")
    }

    pub fn lock_pid(&self) -> Option<i32> {
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

pub fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8")
}

pub fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status {:?}\nstdout: {}\nstderr: {}",
        output.status,
        stdout(output),
        String::from_utf8_lossy(&output.stderr)
    );
}
