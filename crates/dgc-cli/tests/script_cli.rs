//! Workspace script commands through the CLI and daemon socket.

mod common;

use std::fs::{self, File};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::thread::sleep;
use std::time::{Duration, Instant};

use common::{TestEnv, assert_success, stdout};
use dgc_protocol::PROTOCOL_VERSION;
use dgc_protocol::ids::{ProjectId, WorkspaceId};
use dgc_protocol::methods::{HELLO, SCRIPT_LIST, SCRIPT_LOGS, SCRIPT_TRUST, WORKSPACE_LIST};
use dgc_protocol::rpc::{INVALID_PARAMS, INVALID_STATE, Request, Response, RpcError};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::time::timeout;

const COMMAND: &str = "printf 'hello'";

struct ScriptEnv {
    env: TestEnv,
    workspace: String,
    project_path: PathBuf,
}

impl ScriptEnv {
    fn new(command: &str) -> Self {
        let env = TestEnv::new();
        let project_path = env.dir.path().join("script-project");
        fs::create_dir_all(project_path.join("sub")).expect("create project directories");
        let added = parse_json(&run(
            &env,
            &[
                "project",
                "add",
                project_path.to_str().expect("project path"),
                "--json",
            ],
        ));
        let created = parse_json(&run(
            &env,
            &[
                "workspace",
                "create",
                "--project",
                added["id"].as_str().expect("project id"),
                "--json",
            ],
        ));
        fs::write(
            project_path.join("dgc.json"),
            json!({"version": 1, "scripts": {"hello": {"command": command, "cwd": "sub"}}})
                .to_string(),
        )
        .expect("write dgc.json");
        Self {
            env,
            workspace: created["id"].as_str().expect("workspace id").to_owned(),
            project_path,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut script = vec!["script"];
        script.extend_from_slice(args);
        script.extend(["--workspace", &self.workspace]);
        command(&self.env, &script)
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("run dgc script")
    }

    fn start(&self) {
        assert_success(&self.run(&["trust", "--yes"]));
        assert_success(&self.run(&["start", "hello"]));
    }
}

fn command(env: &TestEnv, args: &[&str]) -> Command {
    let mut command = env.command(args);
    command.env("PATH", "/usr/bin:/bin");
    command
}

fn run(env: &TestEnv, args: &[&str]) -> Output {
    command(env, args).output().expect("run dgc")
}

fn parse_json(output: &Output) -> Value {
    assert_success(output);
    let text = stdout(output);
    assert_eq!(text.lines().count(), 1, "{text:?}");
    serde_json::from_str(&text).expect("one JSON value")
}

fn failure(output: &Output) -> String {
    assert!(!output.status.success(), "stdout: {}", stdout(output));
    String::from_utf8(output.stderr.clone()).expect("stderr is UTF-8")
}

#[test]
fn script_trust_shows_commands_and_requires_yes_without_tty() {
    let env = ScriptEnv::new("printf '\u{1b}\u{202e}'\ntrue");
    let input = env.env.dir.path().join("answer");
    fs::write(&input, "y\n").expect("write non-terminal input");
    let output = env
        .command(&["trust"])
        .stdin(File::open(&input).expect("open non-terminal input"))
        .output()
        .expect("run dgc script trust");
    let error = failure(&output);
    assert!(error.contains("--yes"), "{error}");
    let preview = stdout(&output);
    for field in [
        "script-project",
        env.project_path.join("dgc.json").to_str().expect("path"),
        "hello",
        "sub",
        "printf '\\x1b\\u{202e}'\\ntrue",
    ] {
        assert!(preview.contains(field), "{preview:?} lacks {field:?}");
    }
    assert!(!preview.contains(['\u{1b}', '\u{202e}']), "{preview:?}");
    assert!(!preview.contains("\"version\""), "{preview:?}");
    assert_eq!(
        parse_json(&env.run(&["ls", "--json"]))["trust"],
        "untrusted"
    );
    assert_success(&env.run(&["trust", "--yes"]));
    assert_eq!(parse_json(&env.run(&["ls", "--json"]))["trust"], "trusted");
}

#[tokio::test]
async fn script_trust_refuses_when_file_changed() {
    // Synthetic responses let the file change deterministically after the snapshot.
    let env = TestEnv::new();
    let workspace = WorkspaceId::generate();
    let content = "{ \"version\": 1, \"scripts\": {\"hello\": {\"command\": \"true\"}} }\n";
    let path = env.dir.path().join("project\u{202e}");
    let list = json!({
        "project_name": "project\u{1b}", "file": "ok", "error": null,
        "trust": "untrusted", "content": content,
        "scripts": [{"name": "hello", "in_file": true, "command": "true", "state": "not_running"}],
    });
    let workspaces = json!({"workspaces": [{
        "id": workspace, "project_id": ProjectId::generate(), "path": path,
        "isolation": "local", "state": "active", "owned": false, "created_at": ""
    }]});
    for (code, reason, expected) in [
        (INVALID_STATE, "script_file_changed", "review"),
        (INVALID_PARAMS, "missing", "missing"),
        (INVALID_PARAMS, "not_regular", "not_regular"),
        (INVALID_PARAMS, "too_large", "too_large"),
        (INVALID_PARAMS, "invalid", "invalid"),
    ] {
        let output = exchange(
            &env,
            &[
                "script",
                "trust",
                "--workspace",
                workspace.as_str(),
                "--yes",
            ],
            vec![
                (
                    SCRIPT_LIST,
                    json!({"workspace_id": workspace}),
                    Ok(list.clone()),
                ),
                (
                    WORKSPACE_LIST,
                    json!({"project_id": null}),
                    Ok(workspaces.clone()),
                ),
                (
                    SCRIPT_TRUST,
                    json!({"workspace_id": workspace, "expected_content": content}),
                    Err(RpcError::new(code, reason)),
                ),
            ],
        )
        .await;
        let error = failure(&output);
        assert!(error.contains(expected), "{error}");
        let preview = stdout(&output);
        assert!(preview.contains("project\\x1b"), "{preview:?}");
        assert!(preview.contains("project\\u{202e}/dgc.json"), "{preview:?}");
        assert!(!preview.contains(['\u{1b}', '\u{202e}']), "{preview:?}");
    }
}

#[test]
fn script_start_untrusted_fails_with_hint() {
    let env = ScriptEnv::new(COMMAND);
    let error = failure(&env.run(&["start", "hello"]));
    assert!(error.contains("script_untrusted"), "{error}");
    let hint = format!("dgc script trust --workspace {}", env.workspace);
    assert!(error.contains(&hint), "{error}");
    assert_eq!(
        parse_json(&env.run(&["ls", "--json"]))["trust"],
        "untrusted"
    );
}

#[test]
fn script_logs_escapes_control_and_bidi() {
    let env = ScriptEnv::new(r"printf 'hello\t\033[2J\r\007\302\233\342\200\256\nlast'");
    env.start();
    let deadline = Instant::now() + Duration::from_secs(5);
    while parse_json(&env.run(&["ls", "--json"]))["scripts"][0]["state"] != "exited" {
        assert!(Instant::now() < deadline, "script did not exit");
        sleep(Duration::from_millis(20));
    }
    let output = env.run(&["logs", "hello"]);
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        "hello\t\\x1b[2J\\r\\x07\\u{9b}\\u{202e}\nlast"
    );
}

#[tokio::test]
async fn script_logs_follow_ends_when_script_exits() {
    let env = TestEnv::new();
    let workspace = WorkspaceId::generate();
    let output = exchange(
        &env,
        &[
            "script",
            "logs",
            "hello",
            "--workspace",
            workspace.as_str(),
            "--follow",
        ],
        vec![
            log_page(&workspace, 0, 0, 3, true, "あ"),
            log_page(&workspace, 3, 3, 3, true, ""),
            log_page(&workspace, 3, 9, 15, false, "final\n"),
            log_page(&workspace, 15, 15, 18, false, "end"),
            log_page(&workspace, 18, 18, 18, false, ""),
        ],
    )
    .await;
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        "あ\n… output skipped (rotated) …\nfinal\nend"
    );
}

#[tokio::test]
async fn script_logs_follow_stops_on_reset_or_not_running() {
    let env = TestEnv::new();
    let workspace = WorkspaceId::generate();
    let (_, params, _) = log_page(&workspace, 5, 0, 0, false, "");
    for response in [
        Ok(json!({"data": "", "start_offset": 0, "next_offset": 0, "running": false})),
        Err(RpcError::new(INVALID_STATE, "not_running")),
    ] {
        let output = exchange(
            &env,
            &[
                "script",
                "logs",
                "hello",
                "--workspace",
                workspace.as_str(),
                "--follow",
            ],
            vec![
                log_page(&workspace, 0, 0, 5, true, "first"),
                (SCRIPT_LOGS, params.clone(), response),
            ],
        )
        .await;
        let error = failure(&output);
        assert_eq!(error.lines().count(), 1, "{error:?}");
        assert!(error.contains("stopped following"), "{error}");
        assert_eq!(stdout(&output), "first");
    }
}

#[test]
fn script_ls_json() {
    let env = ScriptEnv::new("exec sleep 30");
    let json = parse_json(&env.run(&["ls", "--json"]));
    assert_eq!(json["project_name"], "script-project");
    assert_eq!(json["trust"], "untrusted");
    let script = &json["scripts"][0];
    assert_eq!(script["name"], "hello");
    assert_eq!(script["cwd"], "sub");
    assert_eq!(script["command"], "exec sleep 30");
    assert_eq!(script["state"], "not_running");
    assert_eq!(script["in_file"], true);

    env.start();
    fs::remove_file(env.project_path.join("dgc.json")).expect("remove definition after launch");
    let json = parse_json(&env.run(&["ls", "--json"]));
    assert_eq!(json["scripts"][0]["state"], "running");
    assert_eq!(json["scripts"][0]["in_file"], false);
    for (state, args) in [("running", None), ("exited", Some(["stop", "hello"]))] {
        if let Some(args) = args {
            assert_success(&env.run(&args));
        }
        let output = env.run(&["ls"]);
        assert_success(&output);
        let listing = stdout(&output);
        assert!(listing.contains(state), "{listing:?}");
        assert!(listing.contains("(not in dgc.json)"), "{listing:?}");
    }
}

type Step = (&'static str, Value, Result<Value, RpcError>);

fn log_page(
    workspace: &WorkspaceId,
    offset: u64,
    start: u64,
    next: u64,
    running: bool,
    data: &str,
) -> Step {
    (
        SCRIPT_LOGS,
        json!({"workspace_id": workspace, "name": "hello", "offset": offset, "max_bytes": null}),
        Ok(json!({"data": data, "start_offset": start, "next_offset": next, "running": running})),
    )
}

async fn exchange(env: &TestEnv, args: &[&str], steps: Vec<Step>) -> Output {
    fs::create_dir_all(env.paths.socket.parent().expect("socket dir")).expect("mkdir");
    let listener = UnixListener::bind(&env.paths.socket).expect("bind synthetic daemon socket");
    let server = async {
        let (stream, _) = listener.accept().await.expect("accept CLI");
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        let hello = (
            HELLO,
            Value::Null,
            Ok(json!({"protocol_version": PROTOCOL_VERSION, "daemon_pid": 1})),
        );
        for (method, params, result) in std::iter::once(hello).chain(steps) {
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("read request");
            let request: Request = serde_json::from_str(&line).expect("request JSON");
            assert_eq!(request.method, method);
            if method != HELLO {
                assert_eq!(request.params, params);
            }
            let response = match result {
                Ok(value) => Response::ok(request.id, value),
                Err(error) => Response::err(request.id, error),
            };
            let reply = serde_json::to_string(&response).expect("response JSON");
            write
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .expect("send response");
        }
        let mut trailing = String::new();
        assert_eq!(
            reader.read_line(&mut trailing).await.expect("read EOF"),
            0,
            "unexpected next RPC: {trailing}"
        );
    };
    let mut command = tokio::process::Command::from(command(env, args));
    let output = command.kill_on_drop(true).output();
    let output = timeout(Duration::from_secs(8), async {
        tokio::pin!(server, output);
        tokio::select! {
            () = &mut server => output.await.expect("run dgc script"),
            output = &mut output => {
                let output = output.expect("run dgc script");
                assert!(!String::from_utf8_lossy(&output.stderr).contains("unrecognized subcommand"), "{}", String::from_utf8_lossy(&output.stderr));
                server.await;
                output
            }
        }
    })
    .await;
    fs::remove_file(&env.paths.socket).expect("unlink synthetic daemon socket");
    output.expect("script RPC exchange should finish")
}
