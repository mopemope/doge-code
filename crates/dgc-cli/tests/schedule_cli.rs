//! Schedule and heartbeat commands through the CLI and its daemon socket.

mod common;

use std::fs;
use std::process::Output;
use std::time::{SystemTime, UNIX_EPOCH};

use common::{CLAUDE_VERSION, TestEnv, assert_success, scenario, stdout, write_wrapper};
use dgc_daemon::schedule::localtime::format_local;
use dgc_daemon::store::Store;
use dgc_protocol::PROTOCOL_VERSION;
use dgc_protocol::ids::{AgentId, ScheduleId, WorkspaceId};
use dgc_protocol::methods::{HELLO, SCHEDULE_CREATE};
use dgc_protocol::model::{Agent, AgentState, ProviderKind};
use dgc_protocol::rpc::{Request, Response};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

const CRON: &str = "0 0 1 1 *";

struct ScheduleEnv {
    env: TestEnv,
    workspace: String,
}

impl ScheduleEnv {
    fn new() -> Self {
        let env = TestEnv::new();
        let bin = env.dir.path().join("bin");
        fs::create_dir(&bin).expect("create bin");
        let claude = write_wrapper(&bin, "claude", CLAUDE_VERSION, &scenario("claude_turn"));
        fs::create_dir_all(env.paths.config_file.parent().expect("config dir"))
            .expect("create config dir");
        fs::write(
            &env.paths.config_file,
            format!(
                "claude_path = {:?}\n[profiles.review]\nprovider = \"claude\"\nmodel = \"review-model\"\n",
                claude.to_str().expect("provider path")
            ),
        )
        .expect("write config");
        let dir = env.dir.path().join("project");
        fs::create_dir(&dir).expect("create project dir");
        let project = parse_json(&run(
            &env,
            &[
                "project",
                "add",
                dir.to_str().expect("project path"),
                "--json",
            ],
        ));
        let workspace = parse_json(&run(
            &env,
            &["workspace", "create", "--project", id(&project), "--json"],
        ));
        Self {
            env,
            workspace: id(&workspace).to_owned(),
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        run(&self.env, args)
    }

    fn create(&self, args: &[&str], prompt: &str) -> Output {
        let mut command = vec![
            "schedule",
            "create",
            "--cron",
            CRON,
            "--workspace",
            &self.workspace,
        ];
        command.extend_from_slice(args);
        command.push(prompt);
        self.run(&command)
    }
}

fn run(env: &TestEnv, args: &[&str]) -> Output {
    env.command(args)
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("dgc should start")
}

fn parse_json(output: &Output) -> Value {
    assert_success(output);
    let text = stdout(output);
    assert_eq!(text.lines().count(), 1, "{text:?}");
    serde_json::from_str(&text).expect("stdout is one JSON value")
}

fn id(entity: &Value) -> &str {
    entity["id"].as_str().expect("entity id")
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_secs()
        .try_into()
        .expect("Unix seconds fit in i64")
}

#[test]
fn schedule_create_ls_show_rm_round_trip() {
    let env = ScheduleEnv::new();
    let created = env.create(
        &[
            "--provider",
            "claude",
            "--name",
            "yearly review",
            "--max-runs",
            "1",
        ],
        "Review the workspace",
    );
    assert_success(&created);
    let schedule_id = stdout(&created);
    let schedule_id = schedule_id.trim();
    assert!(schedule_id.starts_with("sch_"));

    let show = parse_json(&env.run(&["schedule", "show", schedule_id, "--json"]));
    let next = show["next_run_at"].as_i64().expect("next run");
    let listed = env.run(&["schedule", "ls"]);
    assert_success(&listed);
    let row = stdout(&listed);
    for field in [
        schedule_id,
        "yearly review",
        CRON,
        "active",
        &format_local(next),
        "0/1",
        &env.workspace,
        "claude",
    ] {
        assert!(row.contains(field), "{row:?} lacks {field:?}");
    }

    assert_success(&env.run(&["schedule", "pause", schedule_id]));
    let paused = parse_json(&env.run(&["schedule", "show", schedule_id, "--json"]));
    assert_eq!(paused["state"], "paused");
    assert_success(&env.run(&["schedule", "resume", schedule_id]));
    let resumed = parse_json(&env.run(&["schedule", "show", schedule_id, "--json"]));
    assert_eq!(resumed["state"], "active");
    assert_success(&env.run(&["schedule", "pause", schedule_id]));
    let result = parse_json(&env.run(&["schedule", "run", schedule_id, "--json"]));
    assert_eq!(result["outcome"]["kind"], "started");
    let agent = result["outcome"]["agent_id"]
        .as_str()
        .expect("launched agent");
    let shown = env.run(&["schedule", "show", schedule_id]);
    assert_success(&shown);
    let details = stdout(&shown);
    for field in [
        "Review the workspace",
        "manual",
        "ended",
        "next_run_at: -",
        "1/1",
        &format_local(result["at"].as_i64().expect("execution time")),
        "started",
        agent,
    ] {
        assert!(details.contains(field), "{details:?} lacks {field:?}");
    }

    assert_success(&env.run(&["schedule", "rm", schedule_id]));
    let empty = parse_json(&env.run(&["schedule", "ls", "--json"]));
    assert_eq!(empty["schedules"], json!([]));
    assert!(!env.run(&["schedule", "show", schedule_id]).status.success());
}

#[tokio::test]
async fn schedule_create_warns_on_auto_approve() {
    // Synthetic schedule.create response: exercise the warning without
    // configuring any provider or enabling an auto-approve launch mode.
    let env = TestEnv::new();
    fs::create_dir_all(env.paths.socket.parent().expect("socket dir")).expect("mkdir");
    let listener = UnixListener::bind(&env.paths.socket).expect("listen");
    let schedule = ScheduleId::generate();
    let workspace = WorkspaceId::generate();
    let response = json!({
        "id": schedule,
        "target": {
            "kind": "new_agent", "workspace_id": workspace, "provider": "claude",
            "mode": "auto", "auto_approve": true
        },
        "prompt": "Review the workspace", "cron": CRON, "state": "active", "runs": 0,
        "recent_runs": [], "created_at": "2026-11-27T00:00:00Z"
    });
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept CLI");
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        for (method, result) in [
            (
                HELLO,
                json!({"protocol_version": PROTOCOL_VERSION, "daemon_pid": 1}),
            ),
            (SCHEDULE_CREATE, response),
        ] {
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("read request");
            let request: Request = serde_json::from_str(&line).expect("request JSON");
            assert_eq!(request.method, method);
            let reply = serde_json::to_string(&Response::ok(request.id, result)).expect("reply");
            write
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .expect("write reply");
        }
    });
    let output = tokio::process::Command::from(env.command(&[
        "schedule",
        "create",
        "--cron",
        CRON,
        "--workspace",
        workspace.as_str(),
        "--profile",
        "review",
        "--json",
        "Review the workspace",
    ]))
    .env("PATH", "/usr/bin:/bin")
    .output()
    .await
    .expect("dgc should start");
    fs::remove_file(&env.paths.socket).expect("unlink synthetic daemon socket");
    assert_eq!(id(&parse_json(&output)), schedule.as_str());
    let warning = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        warning,
        format!(
            "warning: schedule {schedule} runs in auto-approve mode auto: the provider acts without asking for permission\n"
        )
    );
    server.await.expect("finish synthetic RPC exchange");
}

#[test]
fn schedule_paused_next_run_is_hidden_in_text() {
    let env = ScheduleEnv::new();
    let created = parse_json(&env.create(&["--provider", "claude", "--json"], "Check in"));
    let schedule_id = id(&created);
    let next = created["next_run_at"].as_i64().expect("next run");
    assert_success(&env.run(&["schedule", "pause", schedule_id]));

    let listed = env.run(&["schedule", "ls"]);
    let shown = env.run(&["schedule", "show", schedule_id]);
    for output in [&listed, &shown] {
        assert_success(output);
    }
    assert!(
        stdout(&listed).contains("  paused  -  "),
        "{}",
        stdout(&listed)
    );
    assert!(
        stdout(&shown).lines().any(|line| line == "next_run_at: -"),
        "{}",
        stdout(&shown)
    );
    let listed = parse_json(&env.run(&["schedule", "ls", "--json"]));
    let shown = parse_json(&env.run(&["schedule", "show", schedule_id, "--json"]));
    assert_eq!(listed["schedules"][0]["next_run_at"], next);
    assert_eq!(shown["next_run_at"], next);
}

#[test]
fn expires_in_parses_minutes_hours_days_and_rejects_others() {
    let env = ScheduleEnv::new();
    for (duration, seconds) in [("2m", 120), ("3h", 10_800), ("4d", 345_600)] {
        let start = unix_now();
        let created = parse_json(&env.create(
            &["--provider", "claude", "--expires-in", duration, "--json"],
            "Review the workspace",
        ));
        let expiry = created["expires_at"].as_i64().expect("expiry timestamp");
        assert!((start + seconds..=unix_now() + seconds).contains(&expiry));
    }
    for invalid in [
        "m",
        "0m",
        "-1m",
        "+1m",
        "1s",
        "1H",
        "1.5h",
        "1h30m",
        "１２m",
        "9223372036854775807d",
        "153722867280912930m",
    ] {
        let output = env.create(
            &["--provider", "claude", &format!("--expires-in={invalid}")],
            "Review the workspace",
        );
        assert!(!output.status.success(), "accepted {invalid}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("--expires-in"));
    }
}

#[test]
fn heartbeat_create_ls_rm_round_trip() {
    let env = ScheduleEnv::new();
    let store = Store::open(&env.env.paths.state_dir).expect("open temporary store");
    // A synthetic persisted agent supplies the display flag without launching
    // a provider. Removing the record exercises agent.get's NOT_FOUND path.
    let agent = Agent {
        id: AgentId::generate(),
        workspace_id: env.workspace.parse().expect("workspace id"),
        title: "Fixture agent".to_owned(),
        provider: ProviderKind::Claude,
        model: None,
        mode: Some("auto".to_owned()),
        auto_approve: true,
        stop_when_idle: false,
        provider_session_id: None,
        state: AgentState::Stopped,
        parent_id: None,
        mcp: false,
        depth: 0,
        labels: Default::default(),
        created_at: "2026-11-27T00:00:00Z".to_owned(),
        last_error: None,
    };
    store.put_agent(&agent).expect("seed agent");
    let created = env.run(&[
        "heartbeat",
        "create",
        agent.id.as_str(),
        "--cron",
        CRON,
        "--name",
        "check in",
        "--max-runs",
        "2",
        "--expires-in",
        "3h",
        "Check in",
    ]);
    assert_success(&created);
    let schedule_id = stdout(&created);
    let schedule_id = schedule_id.trim();
    assert_eq!(
        String::from_utf8_lossy(&created.stderr),
        format!(
            "warning: schedule {schedule_id} runs in auto-approve mode auto: the provider acts without asking for permission\n"
        )
    );
    let listed = parse_json(&env.run(&["heartbeat", "ls", "--json"]));
    assert_eq!(id(&listed["schedules"][0]), schedule_id);
    assert_eq!(
        listed["schedules"][0]["target"]["agent_id"],
        agent.id.as_str()
    );
    let separate = parse_json(&env.run(&["schedule", "ls", "--json"]));
    assert_eq!(separate["schedules"], json!([]));
    for cmd in [
        &["heartbeat", "ls"][..],
        &["heartbeat", "show", schedule_id][..],
    ] {
        let marked = env.run(cmd);
        assert_success(&marked);
        let text = stdout(&marked);
        assert!(
            text.contains(agent.id.as_str()) && text.contains("auto-approve"),
            "{text:?}"
        );
        if cmd[1] == "show" {
            assert!(text.contains("Check in"), "{text:?}");
        }
    }

    assert!(store.remove_agent(&agent.id).expect("remove fixture agent"));
    for cmd in [
        &["heartbeat", "ls"][..],
        &["heartbeat", "show", schedule_id][..],
    ] {
        let missing = env.run(cmd);
        assert_success(&missing);
        let text = stdout(&missing);
        assert!(
            text.contains(agent.id.as_str()) && !text.contains("auto-approve"),
            "{text:?}"
        );
    }
    assert_success(&env.run(&["heartbeat", "rm", schedule_id]));
    let empty = parse_json(&env.run(&["heartbeat", "ls", "--json"]));
    assert_eq!(empty["schedules"], json!([]));
}

#[test]
fn schedule_output_escapes_prompt_and_name() {
    let env = ScheduleEnv::new();
    let created = env.create(
        &[
            "--provider",
            "claude",
            "--name",
            "first\nsecond\u{1b}\u{202e}",
        ],
        "first\nsecond\u{9b}\r\u{2067}",
    );
    assert_success(&created);
    let schedule_id = stdout(&created);
    let listed = env.run(&["schedule", "ls"]);
    let shown = env.run(&["schedule", "show", schedule_id.trim()]);
    for output in [&listed, &shown] {
        assert_success(output);
        let text = stdout(output);
        assert!(text.contains(r"first\nsecond\x1b\u{202e}"), "{text:?}");
        assert!(!text.contains(['\u{1b}', '\u{202e}', '\u{9b}', '\r', '\u{2067}']));
    }
    assert_eq!(stdout(&listed).lines().count(), 1);
    assert!(stdout(&shown).contains("first\n  second\\u{9b}\\r\\u{2067}"));
}

#[test]
fn schedule_json_output_is_machine_readable() {
    let env = ScheduleEnv::new();
    let name = "nightly\u{202e}";
    let prompt = "Review\nworkspace\u{9b}";
    let created = parse_json(&env.create(
        &[
            "--profile",
            "review",
            "--model",
            "override",
            "--mode",
            "plan",
            "--name",
            name,
            "--json",
        ],
        prompt,
    ));
    let schedule_id = id(&created);
    assert_eq!(created["name"], name);
    assert_eq!(created["prompt"], prompt);
    assert_eq!(created["target"]["provider"], "claude");
    assert_eq!(created["target"]["model"], "override");
    assert_eq!(created["target"]["mode"], "plan");
    assert_eq!(created["target"]["auto_approve"], false);
    let listed = parse_json(&env.run(&["schedule", "ls", "--json"]));
    let row = &listed["schedules"][0];
    assert_eq!(id(row), schedule_id);
    assert_eq!(row["name"], name);
    assert!(row["next_run_at"].is_i64());
    assert!(row.get("prompt").is_none());
    let shown = env.run(&["schedule", "show", schedule_id, "--json"]);
    assert!(!stdout(&shown).contains(['\u{9b}', '\u{202e}']));
    assert_eq!(parse_json(&shown), created);
    assert_eq!(
        parse_json(&env.run(&["schedule", "rm", schedule_id, "--json"])),
        json!({})
    );
}
