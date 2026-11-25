use std::path::PathBuf;
use std::time::Duration;

use dgc_daemon::process::{ChildIo, ChildProcess, ChildSpec, LineEvent};
use tempfile::tempdir;
use tokio::io::AsyncWriteExt;
use tokio::time::{sleep, timeout};

fn sh(script: &str) -> ChildSpec {
    ChildSpec {
        program: PathBuf::from("/bin/sh"),
        args: vec!["-c".to_string(), script.to_string()],
        cwd: std::env::temp_dir(),
    }
}

async fn next(io: &mut ChildIo) -> LineEvent {
    timeout(Duration::from_secs(20), io.lines.recv())
        .await
        .expect("timed out waiting for a line event")
        .expect("line channel closed before Eof")
}

fn pid_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

async fn wait_gone(pid: i32) -> bool {
    for _ in 0..40 {
        if !pid_alive(pid) {
            return true;
        }
        sleep(Duration::from_millis(50)).await;
    }
    !pid_alive(pid)
}

#[tokio::test]
async fn lines_are_delivered_in_order() {
    let (mut child, mut io) =
        ChildProcess::spawn(sh("printf 'one\\ntwo\\r\\n\\nlast'")).expect("spawn");
    assert_eq!(next(&mut io).await, LineEvent::Line("one".into()));
    assert_eq!(next(&mut io).await, LineEvent::Line("two\r".into()));
    assert_eq!(next(&mut io).await, LineEvent::Line(String::new()));
    assert_eq!(next(&mut io).await, LineEvent::Line("last".into()));
    assert_eq!(next(&mut io).await, LineEvent::Eof);
    assert!(io.lines.recv().await.is_none(), "Eof is sent once");
    assert!(child.wait().await.expect("wait").success());
}

#[tokio::test]
async fn invalid_utf8_is_delivered_lossily() {
    let (_child, mut io) = ChildProcess::spawn(sh("printf 'a\\377b\\n'")).expect("spawn");
    assert_eq!(next(&mut io).await, LineEvent::Line("a\u{fffd}b".into()));
}

#[tokio::test]
async fn oversized_line_is_reported_and_reading_continues() {
    let (_child, mut io) = ChildProcess::spawn(sh(
        "head -c 17825792 /dev/zero | tr '\\0' a; printf '\\nok\\n'",
    ))
    .expect("spawn");
    assert_eq!(
        next(&mut io).await,
        LineEvent::Oversized { bytes: 17825792 }
    );
    assert_eq!(next(&mut io).await, LineEvent::Line("ok".into()));
    assert_eq!(next(&mut io).await, LineEvent::Eof);
}

#[tokio::test]
async fn line_of_exactly_the_limit_is_delivered() {
    let limit = 16 * 1024 * 1024;
    let (_child, mut io) = ChildProcess::spawn(sh(&format!(
        "head -c {limit} /dev/zero | tr '\\0' a; printf '\\n'"
    )))
    .expect("spawn");
    match next(&mut io).await {
        LineEvent::Line(line) => assert_eq!(line.len(), limit),
        other => panic!("expected a line, got {other:?}"),
    }
}

#[tokio::test]
async fn stdin_reaches_the_child() {
    let (mut child, mut io) = ChildProcess::spawn(sh("read x; echo \"got:$x\"")).expect("spawn");
    io.stdin.write_all(b"hello\n").await.expect("write");
    io.stdin.flush().await.expect("flush");
    assert_eq!(next(&mut io).await, LineEvent::Line("got:hello".into()));
    assert!(child.wait().await.expect("wait").success());
}

#[tokio::test]
async fn cwd_is_applied() {
    let dir = tempdir().expect("tempdir");
    let mut spec = sh("pwd -P");
    spec.cwd = dir.path().to_path_buf();
    let (_child, mut io) = ChildProcess::spawn(spec).expect("spawn");
    let expected = dir.path().canonicalize().expect("canonicalize");
    assert_eq!(
        next(&mut io).await,
        LineEvent::Line(expected.display().to_string())
    );
}

#[tokio::test]
async fn spawn_failure_is_an_error() {
    let spec = ChildSpec {
        program: PathBuf::from("/nonexistent/program"),
        args: Vec::new(),
        cwd: std::env::temp_dir(),
    };
    assert!(ChildProcess::spawn(spec).is_err());
}

#[tokio::test]
async fn terminate_kills_grandchildren() {
    let (mut child, mut io) = ChildProcess::spawn(sh("sleep 100 & echo $!; wait")).expect("spawn");
    let LineEvent::Line(pid) = next(&mut io).await else {
        panic!("expected the grandchild pid line");
    };
    let pid: i32 = pid.trim().parse().expect("pid");
    assert!(pid_alive(pid), "grandchild should be running");

    let status = child
        .terminate(Duration::from_secs(3))
        .await
        .expect("terminate");
    assert!(!status.success());
    assert!(wait_gone(pid).await, "grandchild {pid} survived terminate");
}

#[tokio::test]
async fn terminate_escalates_to_sigkill_after_grace() {
    let (mut child, mut io) =
        ChildProcess::spawn(sh("trap '' TERM; echo ready; while :; do sleep 1; done"))
            .expect("spawn");
    assert_eq!(next(&mut io).await, LineEvent::Line("ready".into()));
    let started = std::time::Instant::now();
    let status = child
        .terminate(Duration::from_millis(300))
        .await
        .expect("terminate");
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert!(!status.success());
}

#[tokio::test]
async fn terminate_kills_descendants_that_outlive_the_leader() {
    // The leader exits on SIGTERM; the background child ignores it. The pid is
    // printed only after the trap is installed, so SIGTERM cannot race it.
    let dir = tempdir().expect("tempdir");
    let mut spec = sh("(trap '' TERM; touch ready; exec sleep 100) & pid=$!; \
         while [ ! -e ready ]; do sleep 0.01; done; echo $pid; wait");
    spec.cwd = dir.path().to_path_buf();
    let (mut child, mut io) = ChildProcess::spawn(spec).expect("spawn");
    let LineEvent::Line(pid) = next(&mut io).await else {
        panic!("expected pid line");
    };
    let pid: i32 = pid.trim().parse().expect("pid");
    child
        .terminate(Duration::from_secs(3))
        .await
        .expect("terminate");
    assert!(wait_gone(pid).await, "descendant {pid} survived terminate");
}

#[tokio::test]
async fn terminate_on_exited_child_returns_status() {
    let (mut child, _io) = ChildProcess::spawn(sh("exit 3")).expect("spawn");
    let first = child.wait().await.expect("wait");
    assert_eq!(first.code(), Some(3));
    let second = child
        .terminate(Duration::from_secs(1))
        .await
        .expect("terminate");
    assert_eq!(second.code(), Some(3));
    assert_eq!(child.wait().await.expect("wait again").code(), Some(3));
}

#[tokio::test]
async fn drop_kills_the_process_group() {
    let (child, mut io) = ChildProcess::spawn(sh("sleep 100 & echo $!; wait")).expect("spawn");
    let LineEvent::Line(pid) = next(&mut io).await else {
        panic!("expected pid line");
    };
    let pid: i32 = pid.trim().parse().expect("pid");
    drop(child);
    assert!(wait_gone(pid).await, "grandchild {pid} survived drop");
}

#[tokio::test]
async fn stderr_tail_keeps_last_4k() {
    let (mut child, mut io) = ChildProcess::spawn(sh(
        "head -c 10000 /dev/zero | tr '\\0' a >&2; printf 'END' >&2",
    ))
    .expect("spawn");
    child.wait().await.expect("wait");
    assert_eq!(next(&mut io).await, LineEvent::Eof);
    let mut tail = String::new();
    for _ in 0..100 {
        tail = io.stderr_tail.lock().expect("lock").contents();
        if tail.ends_with("END") {
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(tail.len(), 4096);
    assert!(tail.ends_with("aaaEND"));
    assert!(tail.starts_with('a'));
}

#[tokio::test]
async fn backpressure_does_not_drop_lines() {
    // More lines than the channel capacity, consumed slowly after a pause.
    let (_child, mut io) = ChildProcess::spawn(sh(
        "i=0; while [ $i -lt 1000 ]; do echo $i; i=$((i+1)); done",
    ))
    .expect("spawn");
    sleep(Duration::from_millis(500)).await;
    for expected in 0..1000 {
        assert_eq!(next(&mut io).await, LineEvent::Line(expected.to_string()));
    }
    assert_eq!(next(&mut io).await, LineEvent::Eof);
}
