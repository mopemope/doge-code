//! End-to-end tests for the agent and permission commands of `dgc`.
//!
//! The daemon runs real `ClaudeAdapter`/`CodexAdapter`s whose configured
//! executables are small wrapper scripts: they answer `--version` themselves
//! and otherwise `exec` the scenario-replay fake provider
//! (`tests/fixtures/scenarios/`).

mod common;

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::thread::sleep;
use std::time::{Duration, Instant};

use common::{
    CLAUDE_VERSION, CODEX_VERSION, TestEnv, assert_success, scenario, stdout, write_wrapper,
};
use serde_json::Value;

const WAIT: Duration = Duration::from_secs(15);

/// Run git hermetically; panics on failure.
fn git(cwd: &Path, args: &[&str]) {
    // git must not look above the temp dir (this host has a stray /tmp/.git).
    let ceiling = fs::canonicalize(std::env::temp_dir()).expect("temp dir");
    let output = std::process::Command::new("git")
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
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CEILING_DIRECTORIES", ceiling)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git should start");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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
        let project = json(&add.output().expect("dgc should start"));
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

    /// Like [`Self::with_workspace`], but the project is a git repository
    /// and the workspace a dgc-owned worktree on branch `agent-work`.
    /// Returns the worktree path too.
    fn with_worktree_workspace(mut self) -> (Self, PathBuf) {
        let repo = self.env.dir.path().join("repo");
        fs::create_dir(&repo).expect("mkdir repo");
        let repo = fs::canonicalize(repo).expect("canon");
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let project = json(&self.run(&["--json", "project", "add", repo.to_str().expect("utf8")]));
        let workspace = json(&self.run(&[
            "--json",
            "workspace",
            "create",
            "--project",
            project["id"].as_str().expect("project id"),
            "--worktree",
            "--branch",
            "agent-work",
        ]));
        self.workspace = workspace["id"].as_str().expect("workspace id").to_owned();
        let path = PathBuf::from(workspace["path"].as_str().expect("path"));
        (self, path)
    }

    fn run(&self, args: &[&str]) -> Output {
        self.env.run(args)
    }

    /// `dgc run --workspace W --provider P <extra..>`.
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
fn run_with_profile() {
    let a = AgentEnv::new("claude_turn", None);
    let help = a.run(&["run", "--help"]);
    assert_success(&help);
    assert!(stdout(&help).contains("--profile"), "{}", stdout(&help));
    fs::OpenOptions::new()
        .append(true)
        .open(&a.env.paths.config_file)
        .expect("open config")
        .write_all(
            br#"
[profiles.fast]
provider = "claude"
model = "haiku"
mode = "bypassPermissions"
notes = "Quick\nreview\u001b[2J\u202e"
"#,
        )
        .expect("append profile");
    let empty = a.env.dir.path().join("empty-path");
    fs::create_dir(&empty).expect("mkdir");
    let a = a.with_workspace(Some(&empty));
    let output = a.run(&[
        "--json",
        "run",
        "--workspace",
        &a.workspace,
        "--profile",
        "fast",
        "Say pong",
    ]);
    let agent = json(&output);
    assert_eq!(agent["provider"], "claude");
    assert_eq!(agent["model"], "haiku");
    assert_eq!(agent["mode"], "bypassPermissions");
    assert_eq!(agent["auto_approve"], true);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("warning:") && stderr.contains("auto-approve"),
        "{stderr}"
    );
    let id = agent["id"].as_str().expect("id");
    a.wait_state(id, "idle");

    let human = a.run(&["profile", "ls"]);
    assert_success(&human);
    let human = stdout(&human);
    assert!(human.contains("auto-approve"), "{human}");
    assert!(human.contains(r"Quick\nreview\x1b[2J\u{202e}"), "{human:?}");
    let output = a.run(&["profile", "ls", "--json"]);
    assert!(!stdout(&output).contains('\u{202e}'));
    let listed = json(&output);
    assert_eq!(listed["profiles"][0]["name"], "fast");
    assert_eq!(
        listed["profiles"][0]["notes"],
        "Quick\nreview\u{1b}[2J\u{202e}"
    );
    assert_eq!(listed["profiles"][0]["auto_approve"], true);
}

#[test]
fn run_with_mcp_sets_flag() {
    let a = AgentEnv::new("claude_turn", None);
    fs::OpenOptions::new()
        .append(true)
        .open(&a.env.paths.config_file)
        .expect("open config")
        .write_all(
            br#"
[profiles.enabled]
provider = "claude"
mcp = true

[profiles.disabled]
provider = "claude"
mcp = false
"#,
        )
        .expect("append profiles");
    let a = a.with_workspace(None);
    let run = |profile: &str, flags: &[&str]| {
        let mut args = vec![
            "--json",
            "run",
            "--workspace",
            &a.workspace,
            "--profile",
            profile,
        ];
        args.extend_from_slice(flags);
        args.push("Say pong");
        json(&a.run(&args))
    };
    assert_eq!(run("disabled", &["--mcp"])["mcp"], true);
    assert_eq!(run("enabled", &[])["mcp"], true);
    assert_eq!(run("disabled", &[])["mcp"], false);
}

fn create_child(a: &AgentEnv, parent: &str, title: &str, mcp: bool) -> Value {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async {
            let mut client = dgc_client::Client::connect(&a.env.paths)
                .await
                .expect("connect to test daemon");
            client
                .call(
                    "agent.create",
                    &serde_json::json!({
                        "workspace_id": a.workspace,
                        "provider": "claude",
                        "title": title,
                        "initial_prompt": "Say pong",
                        "parent_id": parent,
                        "mcp": mcp,
                    }),
                )
                .await
                .expect("create child")
        })
}

#[test]
fn ls_nests_children_under_parent() {
    let a = AgentEnv::new("claude_turn", None).with_workspace(None);
    let parent = created_id(&a.run_agent("claude", &["--title", "root", "Say pong"]));
    let sibling = created_id(&a.run_agent("claude", &["--title", "other", "Say pong"]));
    let child = create_child(&a, &parent, "child", true);
    let child_id = child["id"].as_str().expect("child id");
    let grandchild = create_child(&a, child_id, "grandchild", false);
    let grandchild_id = grandchild["id"].as_str().expect("grandchild id");

    let listed = a.run(&["ls"]);
    assert_success(&listed);
    let listed = stdout(&listed);
    let rows: Vec<_> = listed.lines().collect();
    assert_eq!(rows.len(), 4, "{listed}");
    assert!(
        rows[0].starts_with(&parent) && rows[0].ends_with("depth=0"),
        "{listed}"
    );
    assert!(
        rows[1].starts_with(&format!("  {child_id}")) && rows[1].ends_with("  mcp  depth=1"),
        "{listed}"
    );
    assert!(
        rows[2].starts_with(&format!("    {grandchild_id}")) && rows[2].ends_with("depth=2"),
        "{listed}"
    );
    assert!(rows[3].starts_with(&sibling), "{listed}");

    let shown = a.run(&["show", child_id]);
    assert_success(&shown);
    assert!(stdout(&shown).contains(&format!("parent_id: {parent}")));
    assert_success(&a.run(&["archive", &parent]));
    let listed = stdout(&a.run(&["ls"]));
    let orphan = listed
        .lines()
        .find(|line| line.contains(child_id))
        .expect("child survives a hidden parent");
    let short = &parent[parent.len() - 6..];
    assert!(
        orphan.starts_with("  agt_")
            && orphan.ends_with(&format!("↳ parent agt_…{short}  depth=1")),
        "{orphan}"
    );
    let including_archived = stdout(&a.run(&["ls", "--all"]));
    assert!(
        !including_archived.contains("↳ parent"),
        "{including_archived}"
    );
}

#[test]
fn ls_json_has_parent_depth_mcp() {
    let a = AgentEnv::new("claude_turn", None).with_workspace(None);
    let parent = created_id(&a.run_agent("claude", &["Say pong"]));
    let child_title = "child\u{202e}";
    let child = create_child(&a, &parent, child_title, true);
    let output = a.run(&["ls", "--json"]);
    let listed = json(&output);
    let agents = listed["agents"].as_array().expect("agents");
    let root = agents.iter().find(|a| a["id"] == parent).expect("root");
    let nested = agents
        .iter()
        .find(|a| a["id"] == child["id"])
        .expect("child");
    assert_eq!(root.get("parent_id"), Some(&Value::Null));
    assert_eq!(root["depth"], 0);
    assert_eq!(root["mcp"], false);
    assert_eq!(nested["parent_id"], parent);
    assert_eq!(nested["depth"], 1);
    assert_eq!(nested["mcp"], true);
    assert_eq!(nested["title"], child_title);
    assert!(
        !stdout(&output).contains('\u{202e}'),
        "bidi control must be escaped"
    );
}

#[test]
fn profile_ls_shows_mcp() {
    let a = AgentEnv::new("claude_turn", None);
    fs::OpenOptions::new()
        .append(true)
        .open(&a.env.paths.config_file)
        .expect("open config")
        .write_all(
            br#"
[profiles.enabled]
provider = "claude"
mcp = true

[profiles.plain]
provider = "claude"
"#,
        )
        .expect("append profiles");
    let output = a.run(&["profile", "ls"]);
    assert_success(&output);
    let human = stdout(&output);
    let rows: Vec<_> = human.lines().collect();
    assert_eq!(rows.len(), 2, "{human}");
    assert!(rows[0].starts_with("enabled") && rows[0].split_whitespace().any(|col| col == "mcp"));
    assert!(rows[1].starts_with("plain") && !rows[1].split_whitespace().any(|col| col == "mcp"));

    let listed = json(&a.run(&["profile", "ls", "--json"]));
    assert_eq!(listed["profiles"][0]["mcp"], true);
    assert_eq!(listed["profiles"][1]["mcp"], false);
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
fn logs_tail_prints_the_most_recent_events() {
    let a = AgentEnv::new("claude_turn", None).with_workspace(None);
    let id = created_id(&a.run_agent("claude", &["--follow", "Say pong"]));
    let all = a.logs_json(&[&id]);
    let tail = &all[all.len() - 2..];
    assert_eq!(a.logs_json(&[&id, "--tail", "2", "--since", "999"]), tail);
    assert!(a.logs_json(&[&id, "--tail", "0"]).is_empty());
    let result = a.run(&["logs", &id, "--tail", "2"]);
    assert_success(&result);
    let human = stdout(&result);
    assert!(
        human.starts_with(&format!("[{}] ", tail[0]["seq"])),
        "{human}"
    );
    assert_eq!(human.lines().count(), 2);
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
fn refused_workspace_archive_leaves_agents_alone_until_forced() {
    let (a, worktree) = AgentEnv::new("claude_turn", None).with_worktree_workspace();
    let id = created_id(&a.run_agent("claude", &["--follow", "Say pong"]));
    assert_eq!(a.agent(&id)["state"], "idle");
    fs::write(worktree.join("agent-output.txt"), "work").expect("write");

    let refused = a.run(&["workspace", "archive", &a.workspace]);
    assert!(!refused.status.success(), "dirty worktree must be refused");
    assert!(
        stderr(&refused).contains("uncommitted changes"),
        "{}",
        stderr(&refused)
    );
    assert_eq!(a.agent(&id)["state"], "idle", "agents are not archived");
    assert!(worktree.join("agent-output.txt").is_file());

    let forced = json(&a.run(&["--json", "workspace", "archive", "--force", &a.workspace]));
    assert_eq!(forced["state"], "archived");
    assert_eq!(forced["worktree_removed"], true);
    assert_eq!(forced["branch_kept"], false);
    assert_eq!(a.agent(&id)["state"], "archived");
    assert!(!worktree.exists());
}

#[test]
fn logs_pages_through_more_than_one_message_worth_of_events() {
    // A scenario like claude_turn whose reply is 400 assistant messages of
    // ~4 KiB: one 1000-event page would exceed the 1 MiB message limit.
    let a = AgentEnv::new("claude_turn", None);
    let generated = a.env.dir.path().join("many.jsonl");
    let mut lines = vec![
        r#"{"expect":{"type":"control_request","request_id":"${ANY}","request":{"subtype":"initialize"}}}"#.to_owned(),
        r#"{"capture":"init","path":"/request_id"}"#.to_owned(),
        r#"{"send":{"type":"control_response","response":{"subtype":"success","request_id":"${init}","response":{}}}}"#.to_owned(),
        r#"{"expect":{"type":"user"}}"#.to_owned(),
        r#"{"send":{"type":"system","subtype":"init","session_id":"sess-many"}}"#.to_owned(),
    ];
    for n in 0..400 {
        let text = format!("msg-{n:03} {}", "x".repeat(4096));
        lines.push(
            serde_json::json!({"send": {"type": "assistant", "message": {"content": [{"type": "text", "text": text}]}}})
                .to_string(),
        );
    }
    lines.push(r#"{"send":{"type":"result","subtype":"success","is_error":false,"session_id":"sess-many","usage":{"output_tokens":1}}}"#.to_owned());
    lines.push(r#"{"expect_eof":true}"#.to_owned());
    fs::write(&generated, lines.join("\n") + "\n").expect("write scenario");
    write_wrapper(
        &a.env.dir.path().join("bin"),
        "claude",
        CLAUDE_VERSION,
        &generated,
    );
    let a = a.with_workspace(None);

    let id = created_id(&a.run_agent("claude", &["Talk a lot"]));
    a.wait_state(&id, "idle");
    let output = a.run(&["logs", &id]);
    assert_success(&output);
    let text = stdout(&output);
    for n in 0..400 {
        assert!(
            text.contains(&format!("assistant: msg-{n:03} ")),
            "message {n} missing"
        );
    }
    let events = a.logs_json(&[&id]);
    assert!(events.len() > 400, "{} events", events.len());
}

#[test]
fn permit_ls_escapes_control_characters_and_marks_truncation() {
    // The Bash command starts with a carriage return and an erase-line
    // escape that would hide "rm -rf ~" on a terminal, and is 10 KiB long.
    let command = format!("\rrm -rf ~\x1b[2K{}", "#".repeat(10 * 1024));
    let a = AgentEnv::new("claude_turn", None);
    let generated = a.env.dir.path().join("spoof.jsonl");
    let mut lines = vec![
        r#"{"expect":{"type":"control_request","request_id":"${ANY}","request":{"subtype":"initialize"}}}"#.to_owned(),
        r#"{"capture":"init","path":"/request_id"}"#.to_owned(),
        r#"{"send":{"type":"control_response","response":{"subtype":"success","request_id":"${init}","response":{}}}}"#.to_owned(),
        r#"{"expect":{"type":"user"}}"#.to_owned(),
        r#"{"send":{"type":"system","subtype":"init","session_id":"sess-spoof"}}"#.to_owned(),
    ];
    lines.push(
        serde_json::json!({"send": {"type": "control_request", "request_id": "perm-1",
            "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": command}}}})
        .to_string(),
    );
    lines.push(r#"{"expect_eof":true}"#.to_owned());
    fs::write(&generated, lines.join("\n") + "\n").expect("write scenario");
    write_wrapper(
        &a.env.dir.path().join("bin"),
        "claude",
        CLAUDE_VERSION,
        &generated,
    );
    let a = a.with_workspace(None);

    let id = created_id(&a.run_agent("claude", &["Do it"]));
    a.wait_permission(&id);
    let human = stdout(&a.run(&["permit", "ls"]));
    assert!(
        human.contains("Bash: \\rrm -rf ~\\x1b[2K###"),
        "control characters must be shown escaped: {human:?}"
    );
    assert!(
        !human.contains('\r') && !human.contains('\x1b'),
        "{human:?}"
    );
    assert!(
        human.contains("…(truncated, "),
        "a cut summary must say so: {human:?}"
    );
    let logs = stdout(&a.run(&["logs", &id]));
    assert!(!logs.contains('\r') && !logs.contains('\x1b'), "{logs:?}");
    assert!(logs.contains("\\rrm -rf ~\\x1b[2K"), "{logs:?}");
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
            .expect("dgc should start"),
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

#[test]
fn slow_provider_probe_does_not_break_autostart() {
    let env = TestEnv::new();
    let bin = env.dir.path().join("bin");
    fs::create_dir(&bin).expect("mkdir bin");
    // `--version` hangs past the daemon's 5 s probe bound.
    // The daemon's PATH is empty below, so `sleep` is named absolutely.
    let sleep_bin = ["/usr/bin/sleep", "/bin/sleep"]
        .into_iter()
        .find(|p| Path::new(p).is_file())
        .expect("a sleep binary");
    let claude = bin.join("claude");
    fs::write(&claude, format!("#!/bin/sh\nexec {sleep_bin} 6\n")).expect("write wrapper");
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o700)).expect("chmod");
    let config_dir = env.paths.config_file.parent().expect("config dir");
    fs::create_dir_all(config_dir).expect("mkdir config");
    fs::write(
        &env.paths.config_file,
        format!("claude_path = \"{}\"\n", claude.display()),
    )
    .expect("write config");
    let empty = env.dir.path().join("empty-path");
    fs::create_dir(&empty).expect("mkdir");

    let started = Instant::now();
    let child = env
        .command(&["--json", "daemon", "status"])
        .env("PATH", &empty)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("dgc should start");
    // The socket is bound before the probes run, well within the 5 s bound.
    while !env.paths.socket.exists() {
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the socket must appear before the provider probe ends"
        );
        sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().expect("dgc should finish");
    let elapsed = started.elapsed();
    let status = json(&output);
    assert!(elapsed < Duration::from_secs(15), "took {elapsed:?}");
    let claude_status = &status["providers"][0];
    assert_eq!(claude_status["kind"], "claude");
    // Found but its version is unknown: usable, untested.
    assert_eq!(claude_status["path"], claude.to_str().expect("utf8"));
    assert_eq!(claude_status["version"], Value::Null);
    assert_eq!(claude_status["tested"], false);
}

#[test]
fn handoff_dry_run_prints_prompt() {
    let a = AgentEnv::new("claude_turn", Some("codex_untrusted")).with_workspace(None);
    let id = created_id(&a.run_agent("claude", &["--follow", "Say pong"]));
    let output = a.run(&["handoff", &id, "--dry-run"]);
    assert_success(&output);
    let prompt = stdout(&output);
    assert!(prompt.contains("Say pong"), "{prompt}");
    assert!(prompt.contains("pong"));
    assert!(prompt.contains("events.jsonl"));
    let result = json(&a.run(&["handoff", &id, "--dry-run", "--json"]));
    assert!(result["agent"].is_null());
    assert_eq!(
        result["prompt"].as_str().expect("prompt").trim_end(),
        prompt.trim_end()
    );
    assert_eq!(
        json(&a.run(&["ls", "--json"]))["agents"]
            .as_array()
            .expect("agents")
            .len(),
        1
    );
}

#[test]
fn follow_prints_handoff_suggestion_on_rate_limit() {
    let a = AgentEnv::new(
        "../../../../dgc-daemon/tests/fixtures/claude/scenarios/rate_limited_429",
        None,
    )
    .with_workspace(None);
    let prompt = "Reply with exactly the word: pong";
    let output = a.run_agent("claude", &["--model", "haiku", "--follow", prompt]);
    let text = stdout(&output);
    let id = text.lines().next().expect("agent id");
    let suggestion =
        format!("rate limited by claude (http_429) — hand off with: dgc handoff {id} --to codex");
    assert_eq!(text.matches(&suggestion).count(), 1, "{text}");
    let output = a.run_agent(
        "claude",
        &["--model", "haiku", "--follow", "--json", prompt],
    );
    let text = stdout(&output);
    assert!(!text.contains("hand off with"), "{text}");
    assert!(
        text.lines().any(|line| {
            let value: Value = serde_json::from_str(line).expect("JSON line");
            value["kind"]["type"] == "rate_limited"
        }),
        "{text}"
    );
}

#[test]
fn handoff_with_profile_and_explicit_overrides() {
    let a = AgentEnv::new("claude_turn", Some("codex_untrusted"));
    let mut config = fs::OpenOptions::new()
        .append(true)
        .open(&a.env.paths.config_file)
        .expect("open config");
    writeln!(
        config,
        "\n[profiles.continue]\nprovider = \"codex\"\nmodel = \"profile-model\"\nmode = \"full-access\""
    )
    .expect("write profile");
    drop(config);
    let a = a.with_workspace(None);
    let id = created_id(&a.run_agent("claude", &["--follow", "Say pong"]));
    let conflict = a.run(&[
        "handoff",
        &id,
        "--profile",
        "continue",
        "--to",
        "claude",
        "--dry-run",
    ]);
    assert!(!conflict.status.success());
    assert!(stderr(&conflict).contains("profile continue is for codex"));
    let output = a.run(&[
        "handoff",
        &id,
        "--profile",
        "continue",
        "--model",
        "explicit-model",
        "--mode",
        "untrusted",
        "--json",
        "--follow",
    ]);
    assert_success(&output);
    let text = stdout(&output);
    let mut lines = text.lines();
    let result: Value = serde_json::from_str(lines.next().expect("creation result")).expect("JSON");
    assert_eq!(result["agent"]["provider"], "codex");
    assert_eq!(result["agent"]["model"], "explicit-model");
    assert_eq!(result["agent"]["mode"], "untrusted");
    assert_eq!(result["agent"]["workspace_id"], a.workspace);
    assert_eq!(result["agent"]["labels"]["handoff_from"], id);
    assert!(lines.any(|line| {
        let event: Value = serde_json::from_str(line).expect("event JSON");
        event["kind"]["text"] == "pong"
    }));
    let output = a.run(&["handoff", &id, "--profile", "continue", "--json"]);
    assert_success(&output);
    assert!(stderr(&output).contains("runs in auto-approve mode full-access"));
    assert_eq!(json(&output)["agent"]["model"], "profile-model");
    assert_eq!(json(&output)["agent"]["auto_approve"], true);
}
