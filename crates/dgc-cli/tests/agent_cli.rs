//! End-to-end tests for the agent and permission commands of `dgc-next`.
//!
//! The daemon runs real `ClaudeAdapter`/`CodexAdapter`s whose configured
//! executables are small wrapper scripts: they answer `--version` themselves
//! and otherwise `exec` the scenario-replay fake provider
//! (`tests/fixtures/scenarios/`).

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::thread::sleep;
use std::time::{Duration, Instant};

use common::{TestEnv, assert_success, stdout};
use serde_json::Value;

const CLAUDE_VERSION: &str = "2.1.332 (Claude Code)";
const CODEX_VERSION: &str = "codex-cli 0.172.0";
const WAIT: Duration = Duration::from_secs(15);

/// The fake provider is built next to `dgc-next` by a workspace build.
fn fake_provider() -> PathBuf {
    let fake = Path::new(env!("CARGO_BIN_EXE_dgc-next")).with_file_name("dgc-fake-provider");
    assert!(
        fake.is_file(),
        "{} is missing: run the tests with --workspace (scripts/verify.sh does) so dgc-daemon's \
         fake provider is built",
        fake.display()
    );
    fake
}

fn scenario(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/scenarios")
        .join(format!("{name}.jsonl"))
}

/// A 0700 script that prints `version` for `--version` and otherwise replays
/// `scenario` with the fake provider.
fn write_wrapper(dir: &Path, name: &str, version: &str, scenario: &Path) -> PathBuf {
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

fn json(output: &Output) -> Value {
    assert_success(output);
    serde_json::from_str(stdout(output).trim()).expect("stdout is one JSON value")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A daemon state root with both providers configured and one local workspace.
struct AgentEnv {
    env: TestEnv,
    workspace: String,
}

impl AgentEnv {
    /// `codex` may be `None`: no `codex_path` is configured then.
    fn new(claude: &str, codex: Option<&str>) -> Self {
        let env = TestEnv::new();
        let bin = env.dir.path().join("bin");
        fs::create_dir(&bin).expect("mkdir bin");
        let claude = write_wrapper(&bin, "claude", CLAUDE_VERSION, &scenario(claude));
        let mut config = format!("claude_path = \"{}\"\n", claude.display());
        if let Some(codex) = codex {
            let codex = write_wrapper(&bin, "codex", CODEX_VERSION, &scenario(codex));
            config.push_str(&format!("codex_path = \"{}\"\n", codex.display()));
        }
        let config_dir = env.paths.config_file.parent().expect("config dir");
        fs::create_dir_all(config_dir).expect("mkdir config");
        fs::write(&env.paths.config_file, config).expect("write config");
        Self {
            env,
            workspace: String::new(),
        }
    }

    /// Register a project directory and create its local workspace. The
    /// first command starts the daemon, so `path_env` (if any) becomes the
    /// daemon's `PATH`.
    fn with_workspace(mut self, path_env: Option<&Path>) -> Self {
        let project_dir = self.env.dir.path().join("project");
        fs::create_dir(&project_dir).expect("mkdir project");
        let project_dir = fs::canonicalize(project_dir).expect("canon");
        let mut add = self.env.command(&[
            "--json",
            "project",
            "add",
            project_dir.to_str().expect("utf8"),
        ]);
        if let Some(path_env) = path_env {
            add.env("PATH", path_env);
        }
        let project = json(&add.output().expect("dgc-next should start"));
        let workspace = json(&self.run(&[
            "--json",
            "workspace",
            "create",
            "--project",
            project["id"].as_str().expect("project id"),
        ]));
        self.workspace = workspace["id"].as_str().expect("workspace id").to_owned();
        self
    }

    fn run(&self, args: &[&str]) -> Output {
        self.env.run(args)
    }

    /// `dgc-next run --workspace W --provider P <extra..>`.
    fn run_agent(&self, provider: &str, extra: &[&str]) -> Output {
        let mut args = vec![
            "run",
            "--workspace",
            &self.workspace,
            "--provider",
            provider,
        ];
        args.extend_from_slice(extra);
        self.run(&args)
    }

    fn agent(&self, id: &str) -> Value {
        json(&self.run(&["--json", "show", id]))
    }

    fn wait_state(&self, id: &str, state: &str) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let agent = self.agent(id);
            if agent["state"] == state {
                return agent;
            }
            assert!(
                Instant::now() < deadline,
                "agent never reached {state}: {agent}\nlogs:\n{}",
                stdout(&self.run(&["logs", id]))
            );
            sleep(Duration::from_millis(50));
        }
    }

    fn wait_permission(&self, id: &str) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let listed = json(&self.run(&["--json", "permit", "ls", "--agent", id]));
            if let Some(first) = listed["permissions"].as_array().and_then(|p| p.first()) {
                return first.clone();
            }
            assert!(
                Instant::now() < deadline,
                "no permission request appeared\nlogs:\n{}",
                stdout(&self.run(&["logs", id]))
            );
            sleep(Duration::from_millis(50));
        }
    }

    fn logs_json(&self, args: &[&str]) -> Vec<Value> {
        let output = self.run(&[&["--json", "logs"], args].concat());
        assert_success(&output);
        stdout(&output)
            .lines()
            .map(|line| serde_json::from_str(line).expect("event JSON line"))
            .collect()
    }
}

/// The agent id printed on the first stdout line of `run`.
fn created_id(output: &Output) -> String {
    let text = stdout(output);
    let id = text.lines().next().expect("run prints the agent id").trim();
    assert!(id.starts_with("agt_"), "unexpected first line: {id}");
    id.to_owned()
}

#[test]
fn run_follow_prints_assistant_text() {
    let a = AgentEnv::new("claude_turn", None).with_workspace(None);
    let output = a.run_agent("claude", &["--title", "ping", "--follow", "Say pong"]);
    assert_success(&output);
    let text = stdout(&output);
    let id = created_id(&output);
    assert!(
        text.lines()
            .any(|l| l.starts_with('[') && l.ends_with("] assistant: pong")),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.ends_with("] user (queue): Say pong")),
        "{text}"
    );
    assert!(text.trim_end().ends_with("state: idle"), "{text}");
    let agent = a.agent(&id);
    assert_eq!(agent["title"], "ping");
    assert_eq!(agent["state"], "idle");
    assert_eq!(agent["provider_session_id"], "sess-cli-1");
}

#[test]
fn permit_allow_unblocks_agent() {
    let a = AgentEnv::new("claude_permission", None).with_workspace(None);
    let output = a.run_agent("claude", &["Touch hello.txt"]);
    assert_success(&output);
    let id = created_id(&output);

    let permission = a.wait_permission(&id);
    assert_eq!(permission["kind"], "command");
    assert_eq!(permission["summary"], "Bash: touch hello.txt");
    a.wait_state(&id, "waiting_permission");
    let perm_id = permission["id"].as_str().expect("permission id");

    let human = stdout(&a.run(&["permit", "ls"]));
    assert!(human.starts_with(perm_id), "{human}");

    assert_success(&a.run(&["permit", "allow", perm_id]));
    a.wait_state(&id, "idle");
    let listed = json(&a.run(&["--json", "permit", "ls"]));
    assert_eq!(listed["permissions"], serde_json::json!([]));

    let logs = stdout(&a.run(&["logs", &id]));
    assert!(
        logs.contains(&format!(
            "permission requested {perm_id} (command): Bash: touch hello.txt"
        )),
        "{logs}"
    );
    assert!(
        logs.contains(&format!("permission resolved {perm_id}: allow")),
        "{logs}"
    );
    assert!(logs.contains("] tool: Bash touch hello.txt"), "{logs}");
    assert!(logs.contains("] assistant: done"), "{logs}");
}

#[test]
fn send_queue_then_drain() {
    let a = AgentEnv::new("claude_queue", None).with_workspace(None);
    let id = created_id(&a.run_agent("claude", &["Touch hello.txt"]));
    let permission = a.wait_permission(&id);

    // The turn is blocked on the permission, so the message is queued.
    let sent = a.run(&["send", &id, "second"]);
    assert_success(&sent);
    assert_eq!(stdout(&sent).trim(), "queued");

    let perm_id = permission["id"].as_str().expect("permission id");
    assert_success(&a.run(&["permit", "allow", perm_id]));
    a.wait_state(&id, "idle");

    let events = a.logs_json(&[&id]);
    let texts: Vec<&str> = events
        .iter()
        .filter(|e| e["kind"]["type"] == "assistant_text")
        .filter_map(|e| e["kind"]["text"].as_str())
        .collect();
    assert_eq!(texts, ["done", "drained"]);

    // Sending to an idle agent starts a turn at once.
    let sent = a.run(&["--json", "send", &id, "--behavior", "steer", "third"]);
    assert_eq!(json(&sent), serde_json::json!({"outcome": "sent"}));
}

#[test]
fn logs_since_replays_history() {
    let a = AgentEnv::new("claude_turn", None).with_workspace(None);
    let id = created_id(&a.run_agent("claude", &["--follow", "Say pong"]));

    let all = a.logs_json(&[&id]);
    let seqs: Vec<u64> = all
        .iter()
        .map(|e| e["seq"].as_u64().expect("seq"))
        .collect();
    let expected: Vec<u64> = (1..=seqs.len() as u64).collect();
    assert_eq!(seqs, expected, "history is complete and ordered");
    assert!(all.len() > 3);

    let tail = a.logs_json(&["--since", "3", &id]);
    assert_eq!(tail, all[3..].to_vec());

    let human = stdout(&a.run(&["logs", "--since", "3", &id]));
    assert!(human.starts_with("[4] "), "{human}");
    assert_eq!(human.lines().count(), all.len() - 3);
}

#[test]
fn auto_approve_mode_warns() {
    let a = AgentEnv::new("claude_bypass", Some("codex_untrusted")).with_workspace(None);

    let output = a.run_agent(
        "claude",
        &[
            "--json",
            "--mode",
            "bypassPermissions",
            "--follow",
            "Say pong",
        ],
    );
    assert_success(&output);
    let text = stdout(&output);
    let first: Value = serde_json::from_str(text.lines().next().expect("agent line"))
        .expect("first line is the agent JSON");
    assert_eq!(first["auto_approve"], true);
    assert_eq!(first["mode"], "bypassPermissions");
    let last: Value =
        serde_json::from_str(text.lines().last().expect("event line")).expect("event JSON line");
    assert_eq!(
        last["kind"],
        serde_json::json!({"type": "state_changed", "state": "idle"})
    );
    let warning = stderr(&output);
    assert!(
        warning.contains("warning:") && warning.contains("auto-approve"),
        "{warning}"
    );
    assert!(
        warning.contains(first["id"].as_str().expect("id")),
        "{warning}"
    );

    // Codex `untrusted` asks before commands: no warning.
    let output = a.run_agent("codex", &["--mode", "untrusted", "--follow", "Say pong"]);
    assert_success(&output);
    assert!(
        stdout(&output).contains("] assistant: pong"),
        "{}",
        stdout(&output)
    );
    assert!(
        !stderr(&output).contains("auto-approve"),
        "{}",
        stderr(&output)
    );

    let listed = stdout(&a.run(&["ls"]));
    assert_eq!(
        listed
            .lines()
            .filter(|l| l.contains("auto-approve"))
            .count(),
        1,
        "{listed}"
    );
}

#[test]
fn archive_workspace_archives_agents() {
    let a = AgentEnv::new("claude_turn", None).with_workspace(None);
    let id = created_id(&a.run_agent("claude", &["--follow", "Say pong"]));
    assert_eq!(a.agent(&id)["state"], "idle");

    let archived = json(&a.run(&["--json", "workspace", "archive", &a.workspace]));
    assert_eq!(archived["state"], "archived");
    assert_eq!(a.agent(&id)["state"], "archived");

    let active = json(&a.run(&["--json", "ls"]));
    assert_eq!(active["agents"], serde_json::json!([]));
    let all = json(&a.run(&["--json", "ls", "--all"]));
    assert_eq!(all["agents"][0]["id"], id.as_str());

    let resumed = a.run(&["resume", &id]);
    assert!(!resumed.status.success(), "archived agents cannot resume");
}

#[test]
fn status_lists_provider_versions() {
    let a = AgentEnv::new("claude_turn", None);
    // The daemon's PATH holds no codex, and none is configured.
    let empty = a.env.dir.path().join("empty-path");
    fs::create_dir(&empty).expect("mkdir");
    let a = a.with_workspace(Some(&empty));

    let status = json(&a.run(&["--json", "daemon", "status"]));
    assert_eq!(status["path_env"], empty.to_str().expect("utf8"));
    let providers = status["providers"].as_array().expect("providers");
    assert_eq!(providers.len(), 2, "{status}");
    let claude = &providers[0];
    assert_eq!(claude["kind"], "claude");
    assert_eq!(
        claude["path"],
        a.env.dir.path().join("bin/claude").to_str().expect("utf8")
    );
    assert_eq!(claude["version"], "2.1.332");
    assert_eq!(claude["tested"], true);
    let codex = &providers[1];
    assert_eq!(codex["kind"], "codex");
    assert_eq!(codex["path"], Value::Null);
    assert_eq!(codex["version"], Value::Null);
    assert_eq!(codex["tested"], false);

    let human = stdout(&a.run(&["daemon", "status"]));
    assert!(human.contains("claude: "), "{human}");
    assert!(human.contains("2.1.332 (tested)"), "{human}");
    assert!(human.contains("codex: not found"), "{human}");

    let refused = a.run_agent("codex", &["hello"]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("not available"),
        "{}",
        stderr(&refused)
    );
}

#[test]
fn daemon_log_excludes_prompt_text() {
    let a = AgentEnv::new("claude_turn", None);
    // Debug logging makes the check meaningful; the daemon inherits DGC_LOG
    // from the command that starts it.
    let project_dir = a.env.dir.path().join("project");
    fs::create_dir(&project_dir).expect("mkdir");
    let added = json(
        &a.env
            .command(&[
                "--json",
                "project",
                "add",
                project_dir.to_str().expect("utf8"),
            ])
            .env("DGC_LOG", "debug")
            .output()
            .expect("dgc-next should start"),
    );
    let workspace = json(&a.run(&[
        "--json",
        "workspace",
        "create",
        "--project",
        added["id"].as_str().expect("id"),
    ]));
    let ws = workspace["id"].as_str().expect("id");

    let output = a.run(&[
        "run",
        "--workspace",
        ws,
        "--provider",
        "claude",
        "--follow",
        "Say pong SECRET-PROMPT-MARKER",
    ]);
    assert_success(&output);
    let id = created_id(&output);
    assert_success(&a.run(&["send", &id, "again SECRET-PROMPT-MARKER"]));
    assert_success(&a.run(&["logs", &id]));
    assert_success(&a.run(&["daemon", "stop"]));

    let log = fs::read_to_string(a.env.paths.state_dir.join("daemon.log")).expect("daemon.log");
    assert!(
        log.contains("DEBUG"),
        "debug logging was not enabled:\n{log}"
    );
    assert!(log.contains(&id), "the log records agent ids:\n{log}");
    assert!(!log.contains("SECRET-PROMPT-MARKER"), "{log}");
    assert!(
        !log.contains("pong"),
        "agent output leaked into the log:\n{log}"
    );
}
