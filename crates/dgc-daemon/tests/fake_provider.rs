//! Integration tests for the scenario-replay `dgc-fake-provider` binary.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::Duration;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fake")
        .join(name)
}

fn spawn(scenario: &Path, extra: &[&str], timeout_ms: u64) -> Child {
    Command::new(env!("CARGO_BIN_EXE_dgc-fake-provider"))
        .arg(scenario)
        .args(extra)
        .env("DGC_FAKE_TIMEOUT_MS", timeout_ms.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn dgc-fake-provider")
}

fn write_line(stdin: &mut ChildStdin, line: &str) {
    writeln!(stdin, "{line}").expect("write to fake provider stdin");
    stdin.flush().expect("flush fake provider stdin");
}

fn read_line(reader: &mut impl BufRead) -> String {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .expect("read fake provider stdout");
    line.trim_end().to_owned()
}

fn scenario_in(dir: &tempfile::TempDir, body: &str) -> PathBuf {
    let path = dir.path().join("scenario.jsonl");
    std::fs::write(&path, body).expect("write scenario");
    path
}

#[test]
fn replays_send_and_expect() {
    let mut child = spawn(&fixture("echo.jsonl"), &[], 5_000);
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));

    assert_eq!(read_line(&mut stdout), r#"{"type":"ready"}"#);
    write_line(&mut stdin, r#"{"type":"hello","extra":{"ignored":true}}"#);
    assert_eq!(read_line(&mut stdout), r#"{"n":[1,2],"type":"world"}"#);
    drop(stdin);

    let out = child.wait_with_output().expect("wait");
    assert_eq!(out.status.code(), Some(7));
    assert!(String::from_utf8_lossy(&out.stderr).contains("scenario says bye"));
}

#[test]
fn capture_substitutes_request_id() {
    let mut child = spawn(&fixture("capture.jsonl"), &[], 5_000);
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));

    write_line(
        &mut stdin,
        r#"{"type":"control_request","request_id":"req-42","request":{"args":[{"id":9}]}}"#,
    );
    assert_eq!(
        read_line(&mut stdout),
        r#"{"response":{"id":{"id":9},"request_id":"req-42"},"type":"control_response"}"#
    );
    // The captured value must now be required by later expects.
    write_line(&mut stdin, r#"{"echo":"req-other"}"#);
    drop(stdin);

    let out = child.wait_with_output().expect("wait");
    assert_eq!(
        out.status.code(),
        Some(3),
        "captured value must match exactly"
    );
}

#[test]
fn mismatch_exits_3() {
    let mut child = spawn(&fixture("echo.jsonl"), &[], 5_000);
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));

    assert_eq!(read_line(&mut stdout), r#"{"type":"ready"}"#);
    write_line(&mut stdin, r#"{"type":"goodbye"}"#);
    drop(stdin);

    let out = child.wait_with_output().expect("wait");
    assert_eq!(out.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("/type"), "diff names the path: {stderr}");
    assert!(
        stderr.contains("goodbye"),
        "diff shows received line: {stderr}"
    );
}

#[test]
fn expect_args_mismatch_exits_3() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scenario = scenario_in(
        &dir,
        "{\"expect_args\":[\"--resume\",\"${ANY}\",\"--json\"]}\n{\"send\":{\"ok\":true}}\n",
    );

    // Ordered subsequence with a wildcard matches.
    let mut ok = spawn(
        &scenario,
        &["-p", "--resume", "sess-1", "x", "--json"],
        5_000,
    );
    let mut stdout = BufReader::new(ok.stdout.take().expect("stdout"));
    assert_eq!(read_line(&mut stdout), r#"{"ok":true}"#);
    drop(ok.stdin.take());
    assert_eq!(ok.wait().expect("wait").code(), Some(0));

    // Missing `--json` fails before anything is sent.
    let bad = spawn(&scenario, &["--resume", "sess-1"], 5_000);
    let out = bad.wait_with_output().expect("wait");
    assert_eq!(out.status.code(), Some(3));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--json"));
}

#[test]
fn expect_timeout_exits_4() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scenario = scenario_in(&dir, "{\"expect\":{\"never\":true}}\n");
    let mut child = spawn(&scenario, &[], 200);
    // Hold stdin open so only the timeout can end the run.
    let _stdin = child.stdin.take().expect("stdin");
    let started = std::time::Instant::now();
    let out = child.wait_with_output().expect("wait");
    assert_eq!(out.status.code(), Some(4));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn expect_eof_waits_for_stdin_close() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scenario = scenario_in(&dir, "{\"expect_eof\":true}\n{\"exit\":5}\n");
    let mut child = spawn(&scenario, &[], 5_000);
    let mut stdin = child.stdin.take().expect("stdin");

    // Input before EOF is ignored and the process keeps waiting.
    write_line(&mut stdin, "ignored");
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        child.try_wait().expect("try_wait").is_none(),
        "still waiting for EOF"
    );

    drop(stdin);
    assert_eq!(child.wait().expect("wait").code(), Some(5));
}
