//! User-visible subprocess checks, isolated from user configuration and APIs.
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::tempdir;

fn command(root: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dgc"))
        .args(args)
        .current_dir(root)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("run dgc subprocess")
}

fn save_session(root: &Path, id: &str) -> Vec<u8> {
    let dir = root.join(".doge/sessions").join(id);
    fs::create_dir_all(&dir).expect("session directory");
    let value = serde_json::json!({
        "meta":{"id":id,"created_at":"2026-10-02T00:00:00Z","title":"private session title"},
        "timestamp":"2026-10-02T00:00:00Z","conversation":[{"role":"user","content":"PRIVATE-CONVERSATION"}],
        "token_count":0,"requests":0,"tool_calls":0,"lines_edited":0,
        "tool_call_successes":{},"tool_call_failures":{},"changed_files":[]
    });
    let bytes = serde_json::to_vec(&value).expect("session JSON");
    fs::write(dir.join("session.json"), &bytes).expect("session fixture");
    bytes
}

#[test]
fn evidence_cli_json_markdown_and_read_only_startup() {
    let root = tempdir().expect("project");
    let home = tempdir().expect("home");
    let id = "01900000-1111-7000-8000-111111111111";
    let before = save_session(root.path(), id);
    // A project .env/config that would be read by normal startup is ignored.
    fs::write(root.path().join(".env"), "OPENAI_API_KEY=DO-NOT-READ\n").expect("env fixture");
    fs::write(root.path().join(".doge/config.toml"), "invalid TOML").expect("config fixture");
    let output = command(
        root.path(),
        home.path(),
        &["session", "evidence", "01900000", "--format", "json"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout is only JSON");
    assert_eq!(json["schema_version"], 2);
    assert_eq!(json["session"]["id"], id);
    assert!(
        json["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|w| w["code"] == "no_provenance")
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("PRIVATE-CONVERSATION"));
    assert!(!root.path().join("debug.log").exists());
    assert!(!root.path().join(".doge/repomap.sqlite").exists());
    assert!(!home.path().join("config").exists());
    assert_eq!(
        before,
        fs::read(
            root.path()
                .join(".doge/sessions")
                .join(id)
                .join("session.json")
        )
        .expect("unchanged")
    );
    let output = command(
        root.path(),
        home.path(),
        &["session", "evidence", id, "--include-content"],
    );
    assert!(output.status.success());
    let markdown = String::from_utf8(output.stdout).expect("Markdown UTF-8");
    assert!(markdown.starts_with("# Dgc evidence report"));
    assert!(markdown.contains("not a correctness proof"));
}

#[test]
fn evidence_cli_invalid_ids_flags_and_base_have_no_partial_stdout() {
    let root = tempdir().expect("project");
    let home = tempdir().expect("home");
    let output = command(
        root.path(),
        home.path(),
        &["session", "evidence", "unknown", "--format", "json"],
    );
    assert!(!output.status.success() && output.stdout.is_empty());
    assert!(!root.path().join(".doge").exists());
    let first = "01900000-1111-7000-8000-111111111111";
    let second = "01900000-2222-7000-8000-222222222222";
    save_session(root.path(), first);
    save_session(root.path(), second);
    for args in [
        vec!["session", "evidence", "01900000", "--format", "json"],
        vec!["session", "evidence", "../../etc", "--format", "json"],
        vec!["session", "evidence", first, "--format", "xml"],
        vec!["session", "evidence", first, "--base", "main"],
        vec!["session", "evidence"],
    ] {
        let output = command(root.path(), home.path(), &args);
        assert!(!output.status.success(), "unexpected success: {args:?}");
        assert!(output.stdout.is_empty(), "partial output: {args:?}");
    }
}
