//! Shared harness for the `dgc` end-to-end tests. Every test runs against
//! its own `DGC_STATE_ROOT` and stops the daemon it started, even when it panics.
#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use dgc_daemon::lock::{InstanceLock, LockError};
use dgc_daemon::paths::DaemonPaths;
use tempfile::{TempDir, tempdir};

pub const CLAUDE_VERSION: &str = "2.1.332 (Claude Code)";
pub const CODEX_VERSION: &str = "codex-cli 0.172.0";

/// The fake provider is built next to `dgc` by a workspace build.
pub fn fake_provider() -> PathBuf {
    let fake = Path::new(env!("CARGO_BIN_EXE_dgc")).with_file_name("dgc-fake-provider");
    assert!(
        fake.is_file(),
        "{} is missing: run the tests with --workspace (scripts/verify.sh does) so dgc-daemon's \
         fake provider is built",
        fake.display()
    );
    fake
}

/// A scenario under `tests/fixtures/scenarios`.
pub fn scenario(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/scenarios")
        .join(format!("{name}.jsonl"))
}

/// A 0700 script that prints `version` for `--version` and otherwise replays
/// `scenario` with the fake provider.
pub fn write_wrapper(dir: &Path, name: &str, version: &str, scenario: &Path) -> PathBuf {
    let path = dir.join(name);
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"--version\" ]; then echo '{version}'; exit 0; fi\n\
         DGC_FAKE_TIMEOUT_MS=60000 exec '{}' '{}' \"$@\"\n",
        fake_provider().display(),
        scenario.display()
    );
    fs::write(&path, script).expect("write wrapper");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("chmod wrapper");
    path
}

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
        let mut command = Command::new(env!("CARGO_BIN_EXE_dgc"));
        command
            .args(args)
            .env("DGC_STATE_ROOT", self.dir.path())
            .env_remove("DGC_LOG")
            .stdin(Stdio::null());
        command
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("dgc should start")
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
