use super::*;
use crate::features::structured_test_results::{Counts, ResultFormat, StructuredTestResult};
fn result() -> TestResult {
    TestResult {
        structured_test_result: None,
        diagnostic_stdout: None,
        execution_observed: true,
        command: "test".into(),
        stdout: String::new(),
        stderr: String::new(),
        success: true,
        exit_code: Some(0),
        failed_tests: vec![],
        timed_out: false,
        output_truncated: false,
        warnings: vec![],
        cancelled: false,
    }
}
#[test]
fn zero_exit_is_not_known_test_success() {
    assert_eq!(classify(&result()), Outcome::Unknown);
}
#[test]
fn zero_skipped_incomplete_failed_timeout_cancel_and_spawn_are_distinct() {
    let mut r = result();
    for (counts, outcome) in [
        (Counts::default(), Outcome::ZeroTests),
        (
            Counts {
                skipped: 2,
                ..Default::default()
            },
            Outcome::Skipped,
        ),
        (
            Counts {
                passed: 1,
                skipped: 2,
                failed: 0,
            },
            Outcome::Passed,
        ),
        (
            Counts {
                failed: 1,
                ..Default::default()
            },
            Outcome::Failed,
        ),
    ] {
        r.structured_test_result = Some(Box::new(StructuredTestResult {
            format_version: 1,
            format: ResultFormat::GoTestJson,
            observation: ResultObservation::Complete {
                test_events: counts,
                packages: Counts::default(),
            },
        }));
        assert_eq!(classify(&r), outcome);
    }
    r.output_truncated = true;
    assert_eq!(classify(&r), Outcome::Incomplete);
    r.success = false;
    assert_eq!(classify(&r), Outcome::Failed);
    r.timed_out = true;
    assert_eq!(classify(&r), Outcome::TimedOut);
    r.cancelled = true;
    assert_eq!(classify(&r), Outcome::Cancelled);
    r.cancelled = false;
    r.timed_out = false;
    r.execution_observed = false;
    assert_eq!(classify(&r), Outcome::SpawnFailed);
}
fn recommendation() -> VerificationRecommendation {
    VerificationRecommendation {
        kind: "test".into(),
        program: "printf".into(),
        args: vec!["ok".into()],
        cwd: None,
        reason: "focused check".into(),
        confidence: "unknown".into(),
        coverage_scope: "file".into(),
    }
}
#[tokio::test]
async fn selection_rejects_changed_files_and_results_never_move_to_new_changes() {
    let dir = tempfile::tempdir().expect("root");
    let git = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .output()
        .expect("git");
    assert!(git.status.success());
    std::fs::write(dir.path().join("source.rs"), "fn a() {}\n").expect("source");
    let start = verification_snapshot::begin(dir.path(), Default::default(), None).await;
    let mut queue = VerificationQueue::default();
    queue.refresh(
        Some("one".into()),
        start.start.clone(),
        vec![recommendation()],
        None,
    );
    assert_eq!(
        queue
            .selection(
                &queue.selection_id(0).expect("ID"),
                &Some("one".into()),
                &start.start
            )
            .expect("select")
            .1
            .outcome,
        Outcome::Unexecuted
    );
    let mut changed = start.start.clone();
    changed.head_oid = Some("different-head".into());
    assert!(
        queue
            .lines(&changed)
            .iter()
            .any(|s| s.contains("candidate snapshot stale"))
    );
    queue.entries[0].outcome = Outcome::Passed;
    queue.entries[0].workspace = Some(verification_snapshot::finish(dir.path(), start, None).await);
    std::fs::write(dir.path().join("source.rs"), "fn b() {}\n").expect("change");
    let current = verification_snapshot::capture(dir.path(), Default::default(), None).await;
    assert!(
        queue
            .selection(
                &queue.selection_id(0).expect("ID"),
                &Some("one".into()),
                &current
            )
            .is_err()
    );
    assert!(
        queue
            .lines(&current)
            .iter()
            .any(|s| s.contains("stale for current changes"))
    );
    queue.refresh(
        Some("one".into()),
        current.clone(),
        vec![recommendation()],
        None,
    );
    assert!(
        queue
            .lines(&current)
            .iter()
            .any(|s| s.contains("stale for current changes"))
    );
    assert!(
        queue
            .selection(
                &queue.selection_id(0).expect("ID"),
                &Some("two".into()),
                &current
            )
            .is_err()
    );
    queue.refresh(Some("two".into()), current, vec![recommendation()], None);
    assert_eq!(queue.entries[0].outcome, Outcome::Unexecuted);
}
#[test]
fn unavailable_snapshot_never_authorizes_execution() {
    let unknown = Snapshot::unavailable(verification_snapshot::Diagnostic::GitUnavailable);
    let mut queue = VerificationQueue::default();
    queue.refresh(None, unknown.clone(), vec![recommendation()], None);
    assert!(
        queue
            .selection(&queue.selection_id(0).expect("ID"), &None, &unknown)
            .is_err()
    );
}
#[tokio::test]
async fn incomplete_planning_cannot_be_upgraded_by_complete_after_capture() {
    let root = tempfile::tempdir().expect("root");
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(root.path())
            .status()
            .expect("git")
            .success()
    );
    std::fs::write(root.path().join("source.rs"), "fn a() {}\n").expect("source");
    let complete = verification_snapshot::capture(root.path(), Default::default(), None).await;
    assert_eq!(
        complete.status,
        verification_snapshot::CaptureStatus::Complete
    );
    let unknown = Snapshot::unavailable(verification_snapshot::Diagnostic::GitUnavailable);
    let planned = bind_planning_snapshot(&unknown, complete.clone()).expect("inspectable");
    let mut queue = VerificationQueue::default();
    queue.refresh(None, planned, vec![recommendation()], None);
    assert!(
        queue
            .selection(&queue.selection_id(0).expect("ID"), &None, &complete)
            .is_err()
    );
}

#[tokio::test]
async fn selection_ids_bind_generation_invocation_and_duplicate_reasons() {
    let root = tempfile::tempdir().expect("root");
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(root.path())
            .status()
            .expect("git")
            .success()
    );
    std::fs::write(root.path().join("source.rs"), "fn a() {}\n").expect("source");
    let snapshot = verification_snapshot::capture(root.path(), Default::default(), None).await;
    let session = Some("one".into());
    let first = recommendation();
    let mut second = first.clone();
    second.args = vec!["other".into()];
    let mut duplicate = first.clone();
    duplicate.reason = "different reason".into();
    duplicate.coverage_scope = "package".into();
    let mut queue = VerificationQueue::default();
    queue.refresh(
        session.clone(),
        snapshot.clone(),
        vec![first.clone(), second.clone(), duplicate.clone()],
        None,
    );
    let original = queue.selection_id(0).expect("ID");
    let duplicate_id = queue.selection_id(2).expect("duplicate ID");
    assert_ne!(original, duplicate_id);
    assert_eq!(original, queue.selection_id(0).expect("stable display ID"));
    assert_eq!(
        queue
            .selection(&original, &session, &snapshot)
            .expect("first")
            .1
            .recommendation
            .reason,
        first.reason
    );
    let selected = queue
        .selection(&duplicate_id, &session, &snapshot)
        .expect("duplicate")
        .1;
    assert_eq!(selected.recommendation.reason, duplicate.reason);
    assert_eq!(selected.recommendation.coverage_scope, "package");
    assert!(
        queue
            .lines(&snapshot)
            .iter()
            .any(|line| line.ends_with(&format!("/verify run {original}")))
    );
    // Refresh invalidates even unchanged candidates; order and cardinality cannot
    // make a queued selection refer to a replacement command.
    for candidates in [
        vec![first.clone(), second.clone(), duplicate],
        vec![second.clone(), first.clone()],
        vec![first.clone()],
        vec![second.clone(), first.clone(), second],
    ] {
        let previous = queue.selection_id(0).expect("previous ID");
        queue.refresh(session.clone(), snapshot.clone(), candidates, None);
        assert!(queue.selection(&previous, &session, &snapshot).is_err());
        assert!(queue.selection(&original, &session, &snapshot).is_err());
        assert!(
            queue
                .selection(&queue.selection_id(0).expect("new ID"), &session, &snapshot)
                .is_ok()
        );
    }
    // Defensive binding also rejects a changed invocation without refresh.
    for field in 0..3 {
        queue.refresh(session.clone(), snapshot.clone(), vec![first.clone()], None);
        let id = queue.selection_id(0).expect("ID");
        match field {
            0 => queue.entries[0].recommendation.program.push_str("-changed"),
            1 => queue.entries[0].recommendation.args.push("changed".into()),
            _ => queue.entries[0].recommendation.cwd = Some("changed".into()),
        }
        assert!(queue.selection(&id, &session, &snapshot).is_err());
    }
    queue.refresh(session.clone(), snapshot.clone(), vec![first.clone()], None);
    let before_switch = queue.selection_id(0).expect("ID");
    assert!(
        queue
            .selection(&before_switch, &Some("two".into()), &snapshot)
            .is_err()
    );
    queue.refresh(
        Some("two".into()),
        snapshot.clone(),
        vec![first.clone()],
        None,
    );
    queue.refresh(session.clone(), snapshot.clone(), vec![first], None);
    assert!(
        queue
            .selection(&before_switch, &session, &snapshot)
            .is_err()
    );
    let before_edit = queue.selection_id(0).expect("ID");
    std::fs::write(root.path().join("source.rs"), "fn b() {}\n").expect("edit");
    let changed = verification_snapshot::capture(root.path(), Default::default(), None).await;
    assert!(queue.selection(&before_edit, &session, &changed).is_err());
}
