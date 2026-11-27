use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dgc_daemon::agent::AgentManager;
use dgc_daemon::config::DaemonConfig;
use dgc_daemon::handlers;
use dgc_daemon::paths::DaemonPaths;
use dgc_daemon::script::ScriptManager;
use dgc_daemon::script::file::{ScriptFile, ScriptFileError};
use dgc_daemon::store::Store;
use dgc_daemon::workspace::WorkspaceManager;
use dgc_protocol::methods::{
    ScriptFileState, ScriptState, ScriptStatus, ScriptTrustState, WorkspaceCreateParams,
};
use dgc_protocol::model::{Isolation, Project, Workspace};
use dgc_protocol::rpc::{INVALID_PARAMS, INVALID_STATE};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};
use tokio::time::{sleep, timeout};

const WAIT: Duration = Duration::from_secs(8);
const CONTENT: &str = r#"{"version":1,"scripts":{"demo":{"command":"printf trusted"}}}"#;

fn content(command: &str) -> String {
    json!({"version": 1, "scripts": {"demo": {"command": command}}}).to_string()
}

fn write(root: &Path, bytes: impl AsRef<[u8]>) {
    fs::write(root.join("dgc.json"), bytes).expect("write dgc.json");
}

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Env {
    _root: TempDir,
    paths: DaemonPaths,
    workspaces: Arc<WorkspaceManager>,
    project: Project,
    ws: Workspace,
    scripts: ScriptManager,
}

impl Env {
    async fn new() -> Self {
        let root = tempdir().expect("tempdir");
        let paths = DaemonPaths::under(root.path());
        let repo = root.path().join("repo");
        fs::create_dir(&repo).expect("repo");
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "initial"]);
        let store = Arc::new(Store::open(&paths.state_dir).expect("store"));
        let workspaces = Arc::new(WorkspaceManager::new(store, paths.worktrees_dir.clone()));
        let project = workspaces
            .add_project(&fs::canonicalize(repo).expect("path"))
            .await
            .expect("project");
        let ws = workspaces
            .create_workspace(WorkspaceCreateParams {
                project_id: project.id.clone(),
                isolation: Isolation::Local,
                branch: None,
                base_ref: None,
                slug: None,
            })
            .await
            .expect("local workspace");
        let scripts =
            ScriptManager::new(&paths.state_dir, Arc::clone(&workspaces)).expect("script manager");
        Self {
            _root: root,
            paths,
            workspaces,
            project,
            ws,
            scripts,
        }
    }

    async fn worktree(&self) -> Workspace {
        self.workspaces
            .create_workspace(WorkspaceCreateParams {
                project_id: self.project.id.clone(),
                isolation: Isolation::Worktree,
                branch: Some("script-worktree".to_owned()),
                base_ref: None,
                slug: None,
            })
            .await
            .expect("worktree")
    }

    async fn trust(&self, body: &str) {
        write(&self.ws.path, body);
        self.scripts
            .trust(&self.ws.id, body)
            .await
            .expect("trust script");
    }

    async fn start(&self, command: &str) {
        self.trust(&content(command)).await;
        self.scripts
            .start(&self.ws.id, "demo")
            .await
            .expect("start script");
    }

    async fn exited(&self) -> ScriptStatus {
        timeout(WAIT, async {
            loop {
                let listed = self.scripts.list(&self.ws.id).await.expect("list");
                let status = listed.scripts.into_iter().next().expect("script");
                if status.state == ScriptState::Exited {
                    return status;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("script exit")
    }

    fn log_path(&self) -> PathBuf {
        self.paths
            .state_dir
            .join("scripts")
            .join(self.ws.id.as_str())
            .join("demo.log")
    }

    async fn child_pid(&self, root: &Path) -> i32 {
        timeout(WAIT, async {
            loop {
                if let Ok(pid) = fs::read_to_string(root.join("child.pid"))
                    && let Ok(pid) = pid.trim().parse()
                {
                    return pid;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("script child PID")
    }
}

fn assert_private(path: &Path) {
    assert_eq!(
        fs::metadata(path).expect("metadata").permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn script_file_parses_and_rejects_unknown_keys_bad_names_and_limits() {
    let root = tempdir().expect("tempdir");
    fs::create_dir(root.path().join("sub")).expect("subdir");
    let valid = json!({"version":1,"scripts":{"a-9_Z":{"command":"é".repeat(2048), "cwd":"sub"}}})
        .to_string();
    write(root.path(), &valid);
    let parsed = ScriptFile::read(root.path()).expect("valid script file");
    assert_eq!(parsed.bytes, valid.as_bytes());
    assert_eq!(parsed.scripts["a-9_Z"].command.len(), 4096);
    assert_eq!(
        parsed.scripts["a-9_Z"].cwd.as_deref(),
        Some(Path::new("sub"))
    );

    let too_many: serde_json::Map<String, Value> = (0..33)
        .map(|n| (format!("s{n}"), json!({"command":"true"})))
        .collect();
    for invalid in [
        json!({"version":2, "scripts":{}}),
        json!({"version":1, "scripts":{}, "unknown":true}),
        json!({"version":1, "scripts":{"a":{"command":"true","unknown":true}}}),
        json!({"version":1, "scripts":{"../../bad":{"command":"true"}}}),
        json!({"version":1, "scripts":{"":{"command":"true"}}}),
        json!({"version":1, "scripts":{"a".repeat(33):{"command":"true"}}}),
        json!({"version":1, "scripts":{"a":{"command":""}}}),
        json!({"version":1, "scripts":{"a":{"command":"x".repeat(4097)}}}),
        json!({"version":1, "scripts":too_many}),
    ] {
        write(root.path(), invalid.to_string());
        assert!(matches!(
            ScriptFile::read(root.path()),
            Err(ScriptFileError::Invalid { .. })
        ));
    }
    write(root.path(), [0xff]);
    assert!(matches!(
        ScriptFile::read(root.path()),
        Err(ScriptFileError::Invalid { .. })
    ));
    // The second spelling decodes to the same key and is far below the first
    // definition in the source a user sees.
    let duplicate = [
        r#"{"version":1,"scripts":{"test":{"command":"printf first"},"#,
        &"\n".repeat(60 * 1024),
        r#""\u0074est":{"command":"printf second"}}}"#,
    ]
    .concat();
    write(root.path(), duplicate);
    assert!(matches!(
        ScriptFile::read(root.path()),
        Err(ScriptFileError::Invalid {
            reason: "duplicate script name"
        })
    ));
    let padded = format!("{}{}", CONTENT, " ".repeat(65_536 - CONTENT.len()));
    write(root.path(), &padded);
    assert!(ScriptFile::read(root.path()).is_ok());
    write(root.path(), format!("{padded} "));
    assert!(matches!(
        ScriptFile::read(root.path()),
        Err(ScriptFileError::TooLarge)
    ));
}

#[test]
fn script_file_symlink_is_refused() {
    let root = tempdir().expect("tempdir");
    assert!(matches!(
        ScriptFile::read(root.path()),
        Err(ScriptFileError::Missing)
    ));
    fs::write(root.path().join("real.json"), CONTENT).expect("target");
    symlink("real.json", root.path().join("dgc.json")).expect("symlink");
    assert!(matches!(
        ScriptFile::read(root.path()),
        Err(ScriptFileError::NotRegular)
    ));
    fs::remove_file(root.path().join("dgc.json")).expect("unlink");
    fs::create_dir(root.path().join("dgc.json")).expect("directory");
    assert!(matches!(
        ScriptFile::read(root.path()),
        Err(ScriptFileError::NotRegular)
    ));
}

#[test]
fn script_cwd_cannot_escape_workspace() {
    let root = tempdir().expect("workspace");
    let outside = tempdir().expect("outside");
    symlink(outside.path(), root.path().join("outer")).expect("symlink outside");
    fs::create_dir(root.path().join("sub")).expect("subdir");
    symlink("sub", root.path().join("inner")).expect("symlink inside");

    for cwd in [
        Path::new(".."),
        Path::new("sub/../sub"),
        outside.path(),
        Path::new("outer"),
    ] {
        write(
            root.path(),
            json!({"version":1,"scripts":{"demo":{"command":"true", "cwd":cwd}}}).to_string(),
        );
        assert!(
            matches!(
                ScriptFile::read(root.path()),
                Err(ScriptFileError::Invalid { .. })
            ),
            "accepted {cwd:?}"
        );
    }
    for cwd in [".", "sub", "inner"] {
        write(
            root.path(),
            json!({"version":1,"scripts":{"demo":{"command":"true", "cwd":cwd}}}).to_string(),
        );
        assert!(ScriptFile::read(root.path()).is_ok(), "rejected {cwd}");
    }
}

#[tokio::test]
async fn untrusted_and_changed_files_cannot_start() {
    let env = Env::new().await;
    write(&env.ws.path, CONTENT);
    assert_eq!(
        env.scripts.list(&env.ws.id).await.expect("list").trust,
        ScriptTrustState::Untrusted
    );
    let untrusted = env
        .scripts
        .start(&env.ws.id, "demo")
        .await
        .expect_err("untrusted");
    assert_eq!(
        (untrusted.code(), untrusted.to_string().as_str()),
        (INVALID_STATE, "script_untrusted")
    );
    env.trust(CONTENT).await;
    let unknown = env
        .scripts
        .start(&env.ws.id, "unknown")
        .await
        .expect_err("unknown");
    assert_eq!(unknown.code(), INVALID_PARAMS);
    write(&env.ws.path, format!("{CONTENT}\n"));
    assert_eq!(
        env.scripts.list(&env.ws.id).await.expect("list").trust,
        ScriptTrustState::Changed
    );
    let changed = env
        .scripts
        .start(&env.ws.id, "demo")
        .await
        .expect_err("changed");
    assert_eq!(
        (changed.code(), changed.to_string().as_str()),
        (INVALID_STATE, "script_untrusted")
    );
}

#[tokio::test]
async fn trust_requires_expected_content() {
    let env = Env::new().await;
    write(&env.ws.path, format!("{CONTENT}\n"));
    let changed = env
        .scripts
        .trust(&env.ws.id, CONTENT)
        .await
        .expect_err("different bytes");
    assert_eq!(
        (changed.code(), changed.to_string().as_str()),
        (INVALID_STATE, "script_file_changed")
    );
    assert_eq!(
        env.scripts.list(&env.ws.id).await.expect("list").trust,
        ScriptTrustState::Untrusted
    );
}

#[tokio::test]
async fn trust_is_per_project_content_and_shared_by_worktrees() {
    let env = Env::new().await;
    let worktree = env.worktree().await;
    env.trust(CONTENT).await;
    write(&worktree.path, CONTENT);
    let restarted =
        ScriptManager::new(&env.paths.state_dir, Arc::clone(&env.workspaces)).expect("reopen");
    let listed = restarted.list(&worktree.id).await.expect("list");
    assert_eq!(listed.trust, ScriptTrustState::Trusted);
    assert_eq!(listed.project_name, env.project.name);
    assert_eq!(listed.scripts[0].state, ScriptState::NotRunning);
    let path = env
        .paths
        .state_dir
        .join("script-trust")
        .join(format!("{}.json", env.project.id));
    assert_private(&path);
    let record: Value =
        serde_json::from_slice(&fs::read(path).expect("trust record")).expect("JSON");
    assert_eq!(record["version"], 1);

    let other_path = env
        .paths
        .state_dir
        .parent()
        .expect("temp root")
        .join("other");
    fs::create_dir(&other_path).expect("another project");
    git(&other_path, &["init", "-q", "-b", "main"]);
    let other = env
        .workspaces
        .add_project(&fs::canonicalize(other_path).expect("path"))
        .await
        .expect("add other");
    let other_ws = env
        .workspaces
        .create_workspace(WorkspaceCreateParams {
            project_id: other.id,
            isolation: Isolation::Local,
            branch: None,
            base_ref: None,
            slug: None,
        })
        .await
        .expect("other workspace");
    write(&other_ws.path, CONTENT);
    assert_eq!(
        restarted.list(&other_ws.id).await.expect("other").trust,
        ScriptTrustState::Untrusted
    );
}

#[tokio::test]
async fn trust_keeps_last_16_contents() {
    let env = Env::new().await;
    for number in 0..17 {
        env.trust(&content(&format!("printf {number}"))).await;
    }
    for number in 0..17 {
        write(&env.ws.path, content(&format!("printf {number}")));
        let expected = if number == 0 {
            ScriptTrustState::Changed
        } else {
            ScriptTrustState::Trusted
        };
        assert_eq!(
            env.scripts.list(&env.ws.id).await.expect("list").trust,
            expected,
            "content {number}"
        );
    }
}

#[tokio::test]
async fn start_runs_the_bytes_it_checked() {
    let mut env = Env::new().await;
    env.scripts = env
        .scripts
        .with_before_spawn(|root| write(root, content("printf changed")));
    env.trust(CONTENT).await;
    env.scripts
        .start(&env.ws.id, "demo")
        .await
        .expect("start checked content");
    assert_eq!(env.exited().await.exit_code, Some(0));
    assert_eq!(fs::read(env.log_path()).expect("log"), b"trusted");
    assert_eq!(
        env.scripts.list(&env.ws.id).await.expect("list").trust,
        ScriptTrustState::Changed
    );
}

#[tokio::test]
async fn script_output_goes_to_private_log_and_logs_pages_by_offset() {
    let env = Env::new().await;
    // The read on null stdin fails, then a single stream writes invalid UTF-8 and NUL.
    env.start(r#"if read -r line; then exit 9; fi; printf 'ab\377\000cd'; printf stderr >&2"#)
        .await;
    assert_eq!(env.exited().await.exit_code, Some(0));
    let path = env.log_path();
    let workspace_dir = path.parent().expect("workspace log directory");
    assert_eq!(
        fs::metadata(workspace_dir)
            .expect("workspace log directory")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_private(&path);
    let bytes = fs::read(&path).expect("log");
    assert!(bytes.windows(6).any(|chunk| chunk == b"ab\xff\0cd"));
    assert!(bytes.windows(6).any(|chunk| chunk == b"stderr"));
    let page = env
        .scripts
        .logs(&env.ws.id, "demo", Some(0), Some(5))
        .await
        .expect("first page");
    assert_eq!(page.data, String::from_utf8_lossy(&bytes[..5]));
    assert_eq!(page.next_offset, 5);
    let tail = env
        .scripts
        .logs(&env.ws.id, "demo", None, Some(3))
        .await
        .expect("last page");
    assert_eq!(
        tail.data,
        String::from_utf8_lossy(&bytes[bytes.len() - 3..])
    );
    assert_eq!(tail.next_offset, bytes.len() as u64);
    assert!(!tail.running);

    fs::remove_file(&path).expect("unlink log");
    let target = env.ws.path.join("untouched");
    fs::write(&target, b"private target").expect("target");
    symlink(&target, &path).expect("symlink log");
    assert!(
        env.scripts
            .logs(&env.ws.id, "demo", None, None)
            .await
            .is_err()
    );
    assert!(env.scripts.start(&env.ws.id, "demo").await.is_err());
    assert_eq!(fs::read(&target).expect("target"), b"private target");
}

#[tokio::test]
async fn script_logs_preserves_utf8_across_pages() {
    let mut env = Env::new().await;
    env.scripts = env.scripts.with_log_limit(3);
    env.start(r#"printf 'A\343\201\202Z'"#).await;
    env.exited().await;
    let first = env
        .scripts
        .logs(&env.ws.id, "demo", Some(0), Some(2))
        .await
        .expect("first page");
    let second = env
        .scripts
        .logs(&env.ws.id, "demo", Some(first.next_offset), Some(4))
        .await
        .expect("next page");
    assert_eq!((first.data.as_str(), first.next_offset), ("A", 1));
    assert_eq!((second.data.as_str(), second.next_offset), ("あZ", 5));
    assert_eq!(first.data + &second.data, "AあZ");
}

#[tokio::test]
async fn script_logs_tail_starts_at_utf8_boundary() {
    let env = Env::new().await;
    env.start(r#"printf 'A\343\201\202Z\200!'"#).await;
    env.exited().await;
    let tail = env
        .scripts
        .logs(&env.ws.id, "demo", None, Some(5))
        .await
        .expect("tail");
    let invalid = env
        .scripts
        .logs(&env.ws.id, "demo", None, Some(2))
        .await
        .expect("invalid tail");
    assert_eq!((tail.data.as_str(), tail.next_offset), ("Z�!", 7));
    assert_eq!(
        serde_json::to_value(&tail).expect("result")["start_offset"],
        4
    );
    assert_eq!((invalid.data.as_str(), invalid.next_offset), ("�!", 7));
}

#[tokio::test]
async fn script_logs_waits_for_complete_utf8_at_live_eof() {
    let env = Env::new().await;
    env.start(r#"printf '\377A\343\201'; while [ ! -f ready ]; do sleep 0.01; done; printf '\202Z\343\201'"#).await;
    timeout(WAIT, async {
        while fs::read(env.log_path()).expect("log").len() < 4 {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("partial output");
    let first = env
        .scripts
        .logs(&env.ws.id, "demo", Some(0), None)
        .await
        .expect("live page");
    let tail = env
        .scripts
        .logs(&env.ws.id, "demo", None, Some(1))
        .await
        .expect("live tail starts in partial character");
    fs::write(env.ws.path.join("ready"), []).expect("let script finish");
    env.exited().await;
    let last = env
        .scripts
        .logs(&env.ws.id, "demo", Some(first.next_offset), None)
        .await
        .expect("final page");
    assert!(first.running);
    assert_eq!((first.data.as_str(), first.next_offset), ("�A", 2));
    assert_eq!((tail.data.as_str(), tail.next_offset), ("", 2));
    assert_eq!(
        serde_json::to_value(&tail).expect("result")["start_offset"],
        2
    );
    assert!(!last.running);
    assert_eq!((last.data.as_str(), last.next_offset), ("あZ�", 8));
}

#[tokio::test]
async fn script_list_keeps_runs_when_script_file_changes_or_disappears() {
    let env = Env::new().await;
    env.start("exec sleep 60").await;
    write(&env.ws.path, r#"{"version":1,"scripts":{}}"#);
    let absent = env
        .scripts
        .list(&env.ws.id)
        .await
        .expect("valid file without demo");
    fs::remove_file(env.ws.path.join("dgc.json")).expect("remove config");
    let missing = env.scripts.list(&env.ws.id).await.expect("missing file");
    write(&env.ws.path, b"invalid");
    let invalid = env.scripts.list(&env.ws.id).await.expect("invalid file");
    env.scripts
        .stop(&env.ws.id, "demo")
        .await
        .expect("stop without definition");
    let exited = env.scripts.list(&env.ws.id).await.expect("exited run");

    for (listing, file_state) in [
        (absent, ScriptFileState::Ok),
        (missing, ScriptFileState::Missing),
        (invalid, ScriptFileState::Invalid),
    ] {
        assert_eq!(listing.file, file_state);
        let [status] = listing.scripts.as_slice() else {
            panic!("expected one run while file is {file_state:?}");
        };
        assert_eq!(status.name, "demo");
        assert_eq!(status.state, ScriptState::Running);
        assert!(status.pid.is_some());
        assert_eq!(status.command, "");
        assert!(status.cwd.is_none());
        assert_eq!(
            serde_json::to_value(status).expect("status")["in_file"],
            false
        );
    }
    assert_eq!(exited.scripts[0].state, ScriptState::Exited);
}

#[tokio::test]
async fn script_trust_reports_file_errors_by_kind() {
    let env = Env::new().await;
    let mut errors = Vec::new();
    let error = env
        .scripts
        .trust(&env.ws.id, CONTENT)
        .await
        .expect_err("missing");
    errors.push((error.code(), error.to_string()));
    let path = env.ws.path.join("dgc.json");
    fs::create_dir(&path).expect("directory in place of file");
    let error = env
        .scripts
        .trust(&env.ws.id, CONTENT)
        .await
        .expect_err("not regular");
    errors.push((error.code(), error.to_string()));
    fs::remove_dir(&path).expect("remove directory");
    write(&env.ws.path, " ".repeat(65_537));
    let error = env
        .scripts
        .trust(&env.ws.id, CONTENT)
        .await
        .expect_err("too large");
    errors.push((error.code(), error.to_string()));
    write(
        &env.ws.path,
        r#"{"version":1,"scripts":{"demo":{"command":"private","unexpected":true}}}"#,
    );
    let error = env
        .scripts
        .trust(&env.ws.id, CONTENT)
        .await
        .expect_err("invalid");
    errors.push((error.code(), error.to_string()));

    assert_eq!(
        errors,
        ["missing", "not_regular", "too_large", "invalid"]
            .map(|kind| (INVALID_PARAMS, kind.to_owned()))
    );
    write(&env.ws.path, CONTENT);
    assert_eq!(
        env.scripts.list(&env.ws.id).await.expect("list").trust,
        ScriptTrustState::Untrusted
    );
}

#[tokio::test]
async fn script_env_has_workspace_vars() {
    let env = Env::new().await;
    fs::create_dir(env.ws.path.join("sub")).expect("subdir");
    env.trust(&json!({"version":1,"scripts":{"demo":{
        "cwd":"sub",
        "command":r#"printf '%s\n' "$DGC_WORKSPACE_ID" "$DGC_WORKSPACE_PATH" "$DGC_SCRIPT_NAME" "$PWD""#
    }}}).to_string()).await;
    env.scripts.start(&env.ws.id, "demo").await.expect("start");
    env.exited().await;
    let expected = format!(
        "{}\n{}\ndemo\n{}\n",
        env.ws.id,
        env.ws.path.display(),
        env.ws.path.join("sub").display()
    );
    assert_eq!(fs::read_to_string(env.log_path()).expect("log"), expected);
}

const GROUP_COMMAND: &str =
    r#"trap 'wait; exit 0' TERM; sleep 60 & printf '%s\n' "$!" > child.pid; wait"#;

#[tokio::test]
async fn stop_kills_the_process_group() {
    let env = Env::new().await;
    env.start(GROUP_COMMAND).await;
    let child = env.child_pid(&env.ws.path).await;
    let running = env
        .scripts
        .start(&env.ws.id, "demo")
        .await
        .expect_err("already running");
    assert_eq!(
        (running.code(), running.to_string().as_str()),
        (INVALID_STATE, "script_running")
    );
    env.scripts
        .stop(&env.ws.id, "demo")
        .await
        .expect("stop process group");
    let status = env.exited().await;
    // The shell's TERM handler reaps the child if the signal reached it.
    assert_eq!(status.exit_code, Some(0));
    // SAFETY: signal 0 only tests the PID's existence.
    assert_eq!(unsafe { libc::kill(child, 0) }, -1);
}

#[tokio::test]
async fn shutdown_sigkills_term_ignoring_script_group_after_grace() {
    let env = Env::new().await;
    env.start(r#"trap '' TERM; printf '%s\n' "$$" > child.pid; exec sleep 60"#)
        .await;
    let pgid = env.child_pid(&env.ws.path).await;
    let ready = &env.ws.path.join("member.ready");
    // Keep the member as this test's child so we can reap it and read its
    // terminating signal, while the daemon still owns the process group.
    let mut member = tokio::process::Command::new("/bin/sh")
        .args(["-c", r#"trap '' TERM; touch "$1"; exec sleep 60"#, "sh"])
        .arg(ready)
        .process_group(pgid)
        .kill_on_drop(true)
        .spawn()
        .expect("spawn group member");
    timeout(WAIT, async {
        while !ready.exists() {
            assert!(member.try_wait().expect("poll group member").is_none());
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("group member armed");

    let grace = Duration::from_millis(100);
    let start = Instant::now();
    env.scripts.shutdown_all(grace).await;
    let elapsed = start.elapsed();
    let member_status = timeout(WAIT, member.wait())
        .await
        .expect("group member was left alive")
        .expect("reap group member");
    assert!(elapsed >= grace, "shutdown ended after {elapsed:?}");
    assert_eq!(member_status.signal(), Some(libc::SIGKILL));
    assert_eq!(env.exited().await.signal, Some(libc::SIGKILL));
}

#[tokio::test]
async fn exit_code_and_signal_are_reported() {
    let env = Env::new().await;
    env.start("exit 23").await;
    let status = env.exited().await;
    assert_eq!(
        (status.exit_code, status.signal, status.pid),
        (Some(23), None, None)
    );
    assert!(
        status
            .started_at
            .zip(status.ended_at)
            .is_some_and(|(start, end)| start <= end)
    );
    env.start("kill -TERM $$").await;
    let status = env.exited().await;
    assert_eq!(
        (status.exit_code, status.signal),
        (None, Some(libc::SIGTERM))
    );
}

#[tokio::test]
async fn log_rotates_once_past_limit() {
    let mut env = Env::new().await;
    env.scripts = env.scripts.with_log_limit(8);
    let path = env.log_path();
    let rotated = path.with_extension("log.1");
    env.start("printf 12345678").await;
    env.exited().await;
    assert!(!rotated.exists(), "exactly at the limit");
    env.start("printf 9abcdefghi").await;
    env.exited().await;
    assert_eq!(fs::read(&rotated).expect("rotated log"), b"9abcdefg");
    assert_eq!(fs::read(&path).expect("active log"), b"hi");
    assert_private(&path);
    assert_private(&rotated);
    let page = env
        .scripts
        .logs(&env.ws.id, "demo", Some(13), Some(5))
        .await
        .expect("page across rotation");
    assert_eq!(page.data, "efghi");
    assert_eq!(page.next_offset, 18);
    let expired = env
        .scripts
        .logs(&env.ws.id, "demo", Some(0), None)
        .await
        .expect("retained data only");
    assert_eq!(expired.data, "9abcdefghi");
    assert_eq!(expired.next_offset, 18);
    assert_eq!(
        serde_json::to_value(&page).expect("result")["start_offset"],
        13
    );
    assert_eq!(
        serde_json::to_value(&expired).expect("result")["start_offset"],
        8
    );
    assert_eq!(
        fs::read_dir(path.parent().expect("log dir"))
            .expect("directory")
            .count(),
        2
    );
}

#[tokio::test]
async fn archive_stops_running_scripts() {
    let env = Env::new().await;
    fs::write(env.ws.path.join(".gitignore"), "dgc.json\nchild.pid\n").expect("ignore test files");
    git(&env.ws.path, &["add", ".gitignore"]);
    git(&env.ws.path, &["commit", "-q", "-m", "ignore readiness"]);
    let ws = env.worktree().await;
    // The TERM trap leaves an untracked file, so only a safety check after stopping sees it.
    let body = content(
        r#"trap 'wait; touch dirty; exit 0' TERM; sleep 60 & printf '%s\n' "$!" > child.pid; wait"#,
    );
    write(&ws.path, &body);
    env.scripts.trust(&ws.id, &body).await.expect("trust");
    env.scripts.start(&ws.id, "demo").await.expect("start");
    env.child_pid(&ws.path).await;
    let store = Store::open(&env.paths.state_dir).expect("store");

    let agents = AgentManager::new(
        Arc::new(store),
        env.paths.clone(),
        DaemonConfig::default(),
        Default::default(),
    );
    let refused =
        handlers::workspace_archive(&env.workspaces, &agents, &env.scripts, json!({"id":ws.id}))
            .await
            .expect_err("dirty worktree");
    assert_eq!(refused.code, INVALID_STATE);
    assert!(ws.path.join("dirty").exists());
    assert_eq!(
        env.scripts
            .list(&ws.id)
            .await
            .expect("workspace still active")
            .scripts[0]
            .state,
        ScriptState::Exited
    );

    handlers::workspace_archive(
        &env.workspaces,
        &agents,
        &env.scripts,
        json!({"id":ws.id,"force":true}),
    )
    .await
    .expect("force archive");
    assert_eq!(
        env.scripts.list(&ws.id).await.expect_err("archived").code(),
        INVALID_STATE
    );
}

#[tokio::test]
async fn shutdown_stops_scripts() {
    let env = Env::new().await;
    env.start(GROUP_COMMAND).await;
    let child = env.child_pid(&env.ws.path).await;
    env.scripts.shutdown_all(Duration::from_millis(500)).await;
    assert_eq!(env.exited().await.exit_code, Some(0));
    // SAFETY: signal 0 only tests the PID's existence.
    assert_eq!(unsafe { libc::kill(child, 0) }, -1);
    assert_eq!(
        env.scripts
            .start(&env.ws.id, "demo")
            .await
            .expect_err("shut down")
            .code(),
        INVALID_STATE
    );
}
