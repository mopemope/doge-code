use std::fs;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use dgc_daemon::events_log::{EventLog, EventLogError};
use dgc_protocol::event::{AgentEvent, EventKind};
use dgc_protocol::ids::AgentId;
use tempfile::tempdir;

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

fn text(n: usize) -> EventKind {
    EventKind::AssistantText {
        text: format!("message {n}"),
        final_: true,
    }
}

fn seqs(events: &[AgentEvent]) -> Vec<u64> {
    events.iter().map(|e| e.seq).collect()
}

#[test]
fn append_assigns_monotonic_seq() {
    let dir = tempdir().expect("tempdir");
    let agent_dir = dir.path().join("agt");
    let id = AgentId::generate();
    let mut log = EventLog::open(&agent_dir, id.clone(), 1 << 20).expect("open");
    assert_eq!(log.last_seq(), 0);
    assert!(log.read_page(0, 10).expect("read").is_empty());

    let a = log.append(EventKind::TurnStarted).expect("append");
    let b = log.append(text(1)).expect("append");
    assert_eq!((a.seq, b.seq), (1, 2));
    assert_eq!(a.agent_id, id);
    assert!(a.at.ends_with('Z') && a.at.len() == 24, "at = {}", a.at);
    assert_eq!(log.last_seq(), 2);

    let all = log.read_page(0, 10).expect("read");
    assert_eq!(all, vec![a, b]);
    assert_eq!(mode_of(&agent_dir), 0o700);
    assert_eq!(mode_of(&agent_dir.join("events.jsonl")), 0o600);
}

#[test]
fn reopen_restores_last_seq() {
    let dir = tempdir().expect("tempdir");
    let id = AgentId::generate();
    {
        let mut log = EventLog::open(dir.path(), id.clone(), 1 << 20).expect("open");
        log.append(EventKind::TurnStarted).expect("append");
        log.append(text(1)).expect("append");
    }
    let mut log = EventLog::open(dir.path(), id, 1 << 20).expect("reopen");
    assert_eq!(log.last_seq(), 2);
    let next = log.append(text(2)).expect("append");
    assert_eq!(next.seq, 3);
    assert_eq!(seqs(&log.read_page(0, 10).expect("read")), vec![1, 2, 3]);
}

#[test]
fn reopen_restores_last_seq_from_rotated_file_when_current_is_empty() {
    let dir = tempdir().expect("tempdir");
    let id = AgentId::generate();
    {
        let mut log = EventLog::open(dir.path(), id.clone(), 1 << 20).expect("open");
        for n in 0..5 {
            log.append(text(n)).expect("append");
        }
    }
    // Crash right after rotation: events.1.jsonl holds the data, current is empty.
    fs::rename(
        dir.path().join("events.jsonl"),
        dir.path().join("events.1.jsonl"),
    )
    .expect("rename");
    fs::write(dir.path().join("events.jsonl"), b"").expect("empty current");

    let mut log = EventLog::open(dir.path(), id, 1 << 20).expect("reopen");
    assert_eq!(log.last_seq(), 5);
    assert_eq!(log.append(text(5)).expect("append").seq, 6);
    assert_eq!(
        seqs(&log.read_page(0, 10).expect("read")),
        vec![1, 2, 3, 4, 5, 6]
    );
}

#[test]
fn rotation_keeps_one_generation() {
    let dir = tempdir().expect("tempdir");
    let mut log = EventLog::open(dir.path(), AgentId::generate(), 1024).expect("open");
    for n in 0..100 {
        let ev = log.append(text(n)).expect("append");
        assert_eq!(ev.seq, n as u64 + 1);
    }
    let mut names: Vec<String> = fs::read_dir(dir.path())
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["events.1.jsonl", "events.jsonl"]);
    for name in &names {
        assert!(
            fs::metadata(dir.path().join(name)).expect("meta").len() <= 1024,
            "{name} exceeds max_bytes"
        );
    }

    let all = log.read_page(0, 1000).expect("read");
    let got = seqs(&all);
    assert_eq!(*got.last().expect("non-empty"), 100);
    assert!(got.len() < 100, "oldest generation must have been dropped");
    for pair in got.windows(2) {
        assert_eq!(pair[1], pair[0] + 1, "seq must be contiguous");
    }
}

#[test]
fn read_page_spans_rotated_file() {
    let dir = tempdir().expect("tempdir");
    let mut log = EventLog::open(dir.path(), AgentId::generate(), 1024).expect("open");
    for n in 0..30 {
        log.append(text(n)).expect("append");
    }
    let rotated = fs::read_to_string(dir.path().join("events.1.jsonl")).expect("rotated");
    let current = fs::read_to_string(dir.path().join("events.jsonl")).expect("current");
    let last_in_rotated = rotated
        .lines()
        .last()
        .map(|l| serde_json::from_str::<AgentEvent>(l).expect("line").seq)
        .expect("rotated non-empty");
    assert!(!current.is_empty());

    let from = last_in_rotated - 2;
    let page = log.read_page(from, 5).expect("read");
    assert_eq!(seqs(&page), (from + 1..=from + 5).collect::<Vec<_>>());
    assert!(page.iter().any(|e| e.seq <= last_in_rotated));
    assert!(page.iter().any(|e| e.seq > last_in_rotated));

    assert!(log.read_page(30, 10).expect("read").is_empty());
    assert!(log.read_page(0, 0).expect("read").is_empty());
}

#[test]
fn corrupt_line_is_skipped() {
    let dir = tempdir().expect("tempdir");
    let id = AgentId::generate();
    {
        let mut log = EventLog::open(dir.path(), id.clone(), 1 << 20).expect("open");
        log.append(text(1)).expect("append");
    }
    let path = dir.path().join("events.jsonl");
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open");
    f.write_all(b"{this is not json\n\xff\xfe\n\n")
        .expect("write");
    drop(f);
    let mut log = EventLog::open(dir.path(), id, 1 << 20).expect("reopen");
    assert_eq!(log.last_seq(), 1);
    log.append(text(2)).expect("append");
    assert_eq!(seqs(&log.read_page(0, 10).expect("read")), vec![1, 2]);
}

#[test]
fn read_tail_returns_last_n_across_rotation() {
    let dir = tempdir().expect("tempdir");
    let mut log = EventLog::open(dir.path(), AgentId::generate(), 1 << 20).expect("open");
    let first = log.append(text(1)).expect("append");
    let second = log.append(text(2)).expect("append");
    let current = dir.path().join("events.jsonl");
    fs::rename(&current, dir.path().join("events.1.jsonl")).expect("rotate");
    fs::write(&current, b"").expect("new log");
    let mut log = EventLog::open(dir.path(), first.agent_id.clone(), 1 << 20).expect("reopen");
    // This event spans several reverse-read blocks, with multibyte UTF-8.
    let third = log
        .append(EventKind::AssistantText {
            text: "🐕".repeat(40_000),
            final_: true,
        })
        .expect("append");
    fs::OpenOptions::new()
        .append(true)
        .open(&current)
        .expect("open for corrupt line")
        .write_all(b"bad line\n\xff\n\n{\"partial\":")
        .expect("write");
    assert!(log.read_tail(0).expect("empty").is_empty());
    assert_eq!(log.read_tail(2).expect("tail"), vec![second, third]);
    assert_eq!(seqs(&log.read_tail(10).expect("tail")), [1, 2, 3]);
}

#[test]
fn read_tail_does_not_scan_whole_file() {
    let dir = tempdir().expect("tempdir");
    let mut log = EventLog::open(dir.path(), AgentId::generate(), 16 << 20).expect("open");
    for _ in 0..128 {
        log.append(EventKind::AssistantText {
            text: "x".repeat(64 << 10),
            final_: true,
        })
        .expect("append");
    }
    assert!(
        fs::metadata(dir.path().join("events.jsonl"))
            .expect("log")
            .len()
            >= 8 << 20
    );
    let (events, bytes) = log.read_tail_with_bytes(3).expect("tail");
    assert_eq!(seqs(&events), [126, 127, 128]);
    assert!(bytes > 0 && bytes < 1 << 20, "read {bytes} bytes");
}

#[test]
fn truncated_last_line_does_not_swallow_next_event() {
    let dir = tempdir().expect("tempdir");
    let id = AgentId::generate();
    {
        let mut log = EventLog::open(dir.path(), id.clone(), 1 << 20).expect("open");
        log.append(text(1)).expect("append");
    }
    let path = dir.path().join("events.jsonl");
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open");
    f.write_all(b"{\"agent_id\":\"trunc").expect("write");
    drop(f);
    let mut log = EventLog::open(dir.path(), id, 1 << 20).expect("reopen");
    log.append(text(2)).expect("append");
    assert_eq!(seqs(&log.read_page(0, 10).expect("read")), vec![1, 2]);
}

#[test]
fn oversized_single_line_is_still_written() {
    let dir = tempdir().expect("tempdir");
    let mut log = EventLog::open(dir.path(), AgentId::generate(), 64).expect("open");
    let big = EventKind::AssistantText {
        text: "x".repeat(500),
        final_: false,
    };
    assert_eq!(log.append(big.clone()).expect("append").seq, 1);
    assert_eq!(log.append(big).expect("append").seq, 2);
    assert_eq!(seqs(&log.read_page(0, 10).expect("read")), vec![1, 2]);
}

#[test]
fn symlinked_current_file_is_refused() {
    let dir = tempdir().expect("tempdir");
    let target = dir.path().join("elsewhere");
    fs::write(&target, b"").expect("target");
    let agent_dir = dir.path().join("agt");
    fs::create_dir(&agent_dir).expect("mkdir");
    symlink(&target, agent_dir.join("events.jsonl")).expect("symlink");
    let err =
        EventLog::open(&agent_dir, AgentId::generate(), 1 << 20).expect_err("must refuse symlink");
    assert!(matches!(err, EventLogError::Symlink { .. }), "{err}");
    assert_eq!(fs::read(&target).expect("read"), b"");
}

#[test]
fn symlinked_rotated_file_is_refused_on_read() {
    let dir = tempdir().expect("tempdir");
    let mut log = EventLog::open(dir.path(), AgentId::generate(), 1 << 20).expect("open");
    log.append(text(1)).expect("append");
    let target = dir.path().join("elsewhere");
    fs::write(&target, b"secret\n").expect("target");
    symlink(&target, dir.path().join("events.1.jsonl")).expect("symlink");
    let err = log.read_page(0, 10).expect_err("must refuse symlink");
    assert!(matches!(err, EventLogError::Symlink { .. }), "{err}");
    let err = log.read_tail(10).expect_err("tail must refuse symlink");
    assert!(matches!(err, EventLogError::Symlink { .. }), "{err}");
}

#[test]
fn read_page_stays_within_the_byte_budget() {
    use dgc_protocol::methods::MAX_EVENTS_PAGE_BYTES;
    let dir = tempdir().expect("tempdir");
    let mut log = EventLog::open(dir.path(), AgentId::generate(), 64 << 20).expect("open");
    for n in 0..400 {
        log.append(EventKind::AssistantText {
            text: format!("{n:04}{}", "x".repeat(4096)),
            final_: true,
        })
        .expect("append");
    }
    let first = log.read_page(0, 1000).expect("read");
    assert!(!first.is_empty());
    assert!(first.len() < 400, "a full page would exceed the budget");
    let size = serde_json::to_vec(&first).expect("serialize").len();
    assert!(size <= MAX_EVENTS_PAGE_BYTES, "{size} bytes");
    let tail = log.read_tail(1000).expect("tail");
    assert!(!tail.is_empty());
    assert!(tail.len() < 400, "the tail must fit the budget too");
    assert_eq!(
        seqs(&tail),
        (401 - tail.len() as u64..=400).collect::<Vec<_>>()
    );
    assert!(serde_json::to_vec(&tail).expect("serialize").len() <= MAX_EVENTS_PAGE_BYTES);

    // Paging on from the last seq returns every event exactly once.
    let mut all = first;
    loop {
        let since = all.last().map_or(0, |e| e.seq);
        let page = log.read_page(since, 1000).expect("read");
        if page.is_empty() {
            break;
        }
        assert!(serde_json::to_vec(&page).expect("serialize").len() <= MAX_EVENTS_PAGE_BYTES);
        all.extend(page);
    }
    assert_eq!(seqs(&all), (1..=400).collect::<Vec<u64>>());
}

#[test]
fn read_page_returns_an_oversized_event_alone() {
    use dgc_protocol::methods::MAX_EVENTS_PAGE_BYTES;
    let dir = tempdir().expect("tempdir");
    let mut log = EventLog::open(dir.path(), AgentId::generate(), 64 << 20).expect("open");
    let big = EventKind::AssistantText {
        text: "y".repeat(MAX_EVENTS_PAGE_BYTES + 10),
        final_: true,
    };
    log.append(big.clone()).expect("append");
    log.append(text(2)).expect("append");
    assert_eq!(seqs(&log.read_page(0, 10).expect("read")), vec![1]);
    assert_eq!(seqs(&log.read_page(1, 10).expect("read")), vec![2]);
    assert_eq!(seqs(&log.read_tail(10).expect("tail")), vec![2]);
    log.append(big).expect("append");
    assert_eq!(seqs(&log.read_tail(10).expect("oversized tail")), vec![3]);
}
