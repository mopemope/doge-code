use crate::jobs::{
    JobContext, JobId, JobKind, JobManager, JobRunOutcome, JobScope, JobSpec, JobStartError,
    JobStatus, WorkspaceAccess,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

fn foreground_spec(label: &str, access: WorkspaceAccess) -> JobSpec {
    JobSpec::new(JobKind::AgentTurn, JobScope::Foreground, access, label)
}

fn background_spec(kind: JobKind, access: WorkspaceAccess, label: &str) -> JobSpec {
    JobSpec::new(kind, JobScope::Background, access, label)
}

async fn wait_for_status(manager: &JobManager, id: JobId, status: JobStatus) {
    for _ in 0..200 {
        if manager.get_snapshot(id).map(|s| s.status) == Some(status) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {id} to reach {status}");
}

async fn wait_for_terminal(manager: &JobManager, id: JobId) -> JobStatus {
    for _ in 0..200 {
        if let Some(snapshot) = manager.get_snapshot(id)
            && snapshot.status.is_terminal()
        {
            return snapshot.status;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {id} to finish");
}

#[tokio::test]
async fn test_starts_foreground_job() {
    let manager = JobManager::new();
    let id = manager
        .spawn(
            foreground_spec("hello", WorkspaceAccess::None),
            |_ctx| async { JobRunOutcome::Completed },
        )
        .unwrap();
    assert_eq!(wait_for_terminal(&manager, id).await, JobStatus::Completed);
}

#[tokio::test]
async fn test_rejects_second_foreground_job() {
    let manager = JobManager::new();
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let started_clone = started.clone();
    let release_clone = release.clone();
    let a = manager
        .spawn(
            foreground_spec("a", WorkspaceAccess::None),
            move |ctx| async move {
                started_clone.notify_one();
                tokio::select! {
                    _ = ctx.cancellation.cancelled() => JobRunOutcome::Cancelled,
                    _ = release_clone.notified() => JobRunOutcome::Completed,
                }
            },
        )
        .unwrap();
    started.notified().await;
    let err = manager
        .spawn(foreground_spec("b", WorkspaceAccess::None), |_ctx| async {
            JobRunOutcome::Completed
        })
        .unwrap_err();
    match err {
        JobStartError::ForegroundBusy { active } => assert_eq!(active.id, a),
        other => panic!("unexpected error: {other:?}"),
    }
    release.notify_one();
    assert_eq!(wait_for_terminal(&manager, a).await, JobStatus::Completed);
}

#[tokio::test]
async fn test_foreground_slot_released_on_completion() {
    let manager = JobManager::new();
    let a = manager
        .spawn(foreground_spec("a", WorkspaceAccess::None), |_ctx| async {
            JobRunOutcome::Completed
        })
        .unwrap();
    assert_eq!(wait_for_terminal(&manager, a).await, JobStatus::Completed);
    let b = manager
        .spawn(foreground_spec("b", WorkspaceAccess::None), |_ctx| async {
            JobRunOutcome::Completed
        })
        .unwrap();
    assert_eq!(wait_for_terminal(&manager, b).await, JobStatus::Completed);
}

#[tokio::test]
async fn test_foreground_slot_released_on_failure() {
    let manager = JobManager::new();
    let a = manager
        .spawn(foreground_spec("a", WorkspaceAccess::None), |_ctx| async {
            JobRunOutcome::Failed {
                message: "boom".to_string(),
            }
        })
        .unwrap();
    assert_eq!(wait_for_terminal(&manager, a).await, JobStatus::Failed);
    let snapshot = manager.get_snapshot(a).unwrap();
    assert_eq!(snapshot.error.as_deref(), Some("boom"));
    // Next foreground job must be accepted.
    let b = manager
        .spawn(foreground_spec("b", WorkspaceAccess::None), |_ctx| async {
            JobRunOutcome::Completed
        })
        .unwrap();
    assert_eq!(wait_for_terminal(&manager, b).await, JobStatus::Completed);
}

#[tokio::test]
async fn test_foreground_slot_released_on_cancel() {
    let manager = JobManager::new();
    let a = manager
        .spawn(
            foreground_spec("a", WorkspaceAccess::None),
            |ctx| async move {
                tokio::select! {
                    _ = ctx.cancellation.cancelled() => JobRunOutcome::Cancelled,
                    _ = tokio::time::sleep(Duration::from_secs(30)) => JobRunOutcome::Completed,
                }
            },
        )
        .unwrap();
    wait_for_status(&manager, a, JobStatus::Running).await;
    manager.cancel(a);
    assert_eq!(wait_for_terminal(&manager, a).await, JobStatus::Cancelled);
    let b = manager
        .spawn(foreground_spec("b", WorkspaceAccess::None), |_ctx| async {
            JobRunOutcome::Completed
        })
        .unwrap();
    assert_eq!(wait_for_terminal(&manager, b).await, JobStatus::Completed);
}

#[tokio::test]
async fn test_none_access_cancel_before_poll_skips_body() {
    // On the current-thread test runtime the spawned task cannot run before
    // the test task yields, so cancelling right after spawn deterministically
    // exercises the pre-cancel path of the `WorkspaceAccess::None` branch.
    let manager = JobManager::new();
    let ran = Arc::new(AtomicBool::new(false));
    let ran_clone = ran.clone();
    let id = manager
        .spawn(
            background_spec(JobKind::Test, WorkspaceAccess::None, "skip"),
            move |_ctx| async move {
                ran_clone.store(true, Ordering::SeqCst);
                JobRunOutcome::Completed
            },
        )
        .unwrap();
    manager.cancel(id);
    assert_eq!(wait_for_terminal(&manager, id).await, JobStatus::Cancelled);
    assert!(!ran.load(Ordering::SeqCst));
}

#[tokio::test]
async fn test_job_ids_are_unique() {
    let manager = JobManager::new();
    let mut ids = Vec::new();
    for i in 0..5 {
        let id = manager
            .spawn(
                background_spec(JobKind::Test, WorkspaceAccess::None, &format!("job {i}")),
                |_ctx| async { JobRunOutcome::Completed },
            )
            .unwrap();
        ids.push(id);
    }
    for id in &ids {
        wait_for_terminal(&manager, *id).await;
    }
    let mut sorted = ids.clone();
    sorted.sort_by_key(|id| id.0);
    sorted.dedup_by_key(|id| id.0);
    assert_eq!(sorted.len(), ids.len());
}

#[tokio::test]
async fn test_completed_jobs_are_retained() {
    let manager = JobManager::new();
    let id = manager
        .spawn(
            background_spec(JobKind::Test, WorkspaceAccess::None, "done"),
            |_ctx| async { JobRunOutcome::Completed },
        )
        .unwrap();
    assert_eq!(wait_for_terminal(&manager, id).await, JobStatus::Completed);
    let snapshots = manager.snapshots();
    assert!(snapshots.iter().any(|s| s.id == id));
}

#[tokio::test]
async fn test_recent_history_is_bounded() {
    let manager = JobManager::new();
    for i in 0..(crate::jobs::MAX_RECENT_JOBS + 10) {
        let id = manager
            .spawn(
                background_spec(JobKind::Test, WorkspaceAccess::None, &format!("job {i}")),
                |_ctx| async { JobRunOutcome::Completed },
            )
            .unwrap();
        wait_for_terminal(&manager, id).await;
    }
    let snapshots = manager.snapshots();
    assert!(snapshots.len() <= crate::jobs::MAX_RECENT_JOBS);
    assert_eq!(snapshots.len(), crate::jobs::MAX_RECENT_JOBS);
}

#[tokio::test]
async fn test_cancellation_transitions_to_cancelled() {
    let manager = JobManager::new();
    let id = manager
        .spawn(
            background_spec(JobKind::Test, WorkspaceAccess::None, "sleep"),
            |ctx| async move {
                loop {
                    tokio::select! {
                        _ = ctx.cancellation.cancelled() => return JobRunOutcome::Cancelled,
                        _ = tokio::time::sleep(Duration::from_millis(10)) => {}
                    }
                }
            },
        )
        .unwrap();
    wait_for_status(&manager, id, JobStatus::Running).await;
    manager.cancel(id);
    // Cancelling should be observable (or the job may finish fast).
    let status = wait_for_terminal(&manager, id).await;
    assert_eq!(status, JobStatus::Cancelled);
}

#[tokio::test]
async fn test_shutdown_cancels_child_tokens() {
    let manager = JobManager::new();
    let observed = Arc::new(AtomicBool::new(false));
    let observed_clone = observed.clone();
    let release = Arc::new(tokio::sync::Notify::new());
    let release_clone = release.clone();
    let _id = manager
        .spawn(
            background_spec(JobKind::Test, WorkspaceAccess::None, "wait"),
            |ctx| async move {
                ctx.cancellation.cancelled().await;
                observed_clone.store(true, Ordering::SeqCst);
                release_clone.notified().await;
                JobRunOutcome::Cancelled
            },
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    // Shutdown in background; the job waits on `release` so shutdown would
    // stall without the fallback abort path.
    let manager_clone = manager.clone();
    let shutdown = tokio::spawn(async move {
        manager_clone.shutdown(Duration::from_millis(200)).await;
    });
    // Give shutdown a moment to cancel the child token.
    for _ in 0..100 {
        if observed.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(observed.load(Ordering::SeqCst));
    release.notify_one();
    shutdown.await.unwrap();
    assert_eq!(manager.active_count(), 0);
}

#[tokio::test]
async fn test_foreground_busy_error() {
    let manager = JobManager::new();
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let started_clone = started.clone();
    let release_clone = release.clone();
    let a = manager
        .spawn(
            foreground_spec("a", WorkspaceAccess::None),
            move |ctx| async move {
                started_clone.notify_one();
                tokio::select! {
                    _ = ctx.cancellation.cancelled() => JobRunOutcome::Cancelled,
                    _ = release_clone.notified() => JobRunOutcome::Completed,
                }
            },
        )
        .unwrap();
    started.notified().await;
    let err = manager
        .spawn(foreground_spec("b", WorkspaceAccess::None), |_ctx| async {
            JobRunOutcome::Completed
        })
        .unwrap_err();
    assert!(matches!(err, JobStartError::ForegroundBusy { .. }));
    release.notify_one();
    wait_for_terminal(&manager, a).await;
}

#[tokio::test]
async fn test_readonly_jobs_run_concurrently() {
    let manager = JobManager::new();
    let concurrent = Arc::new(AtomicUsize::new(0));
    let max_seen = Arc::new(AtomicUsize::new(0));
    let mut ids = Vec::new();
    for _ in 0..2 {
        let c = concurrent.clone();
        let m = max_seen.clone();
        let id = manager
            .spawn(
                background_spec(JobKind::Test, WorkspaceAccess::ReadOnly, "read"),
                |_ctx| async move {
                    let now = c.fetch_add(1, Ordering::SeqCst) + 1;
                    // Record peak concurrency.
                    loop {
                        let prev = m.load(Ordering::SeqCst);
                        if now <= prev
                            || m.compare_exchange(prev, now, Ordering::SeqCst, Ordering::SeqCst)
                                .is_ok()
                        {
                            break;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    c.fetch_sub(1, Ordering::SeqCst);
                    JobRunOutcome::Completed
                },
            )
            .unwrap();
        ids.push(id);
    }
    for id in ids {
        wait_for_terminal(&manager, id).await;
    }
    assert_eq!(max_seen.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn test_write_jobs_do_not_overlap() {
    async fn peak_concurrency(access: WorkspaceAccess) -> usize {
        let manager = JobManager::new();
        let concurrent = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let mut ids = Vec::new();
        for _ in 0..2 {
            let c = concurrent.clone();
            let m = max_seen.clone();
            let id = manager
                .spawn(
                    background_spec(JobKind::Lint, access, "op"),
                    |_ctx| async move {
                        let now = c.fetch_add(1, Ordering::SeqCst) + 1;
                        loop {
                            let prev = m.load(Ordering::SeqCst);
                            if now <= prev
                                || m.compare_exchange(prev, now, Ordering::SeqCst, Ordering::SeqCst)
                                    .is_ok()
                            {
                                break;
                            }
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        c.fetch_sub(1, Ordering::SeqCst);
                        JobRunOutcome::Completed
                    },
                )
                .unwrap();
            ids.push(id);
        }
        for id in ids {
            for _ in 0..200 {
                if let Some(s) = manager.get_snapshot(id)
                    && s.status.is_terminal()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        max_seen.load(Ordering::SeqCst)
    }

    assert_eq!(peak_concurrency(WorkspaceAccess::Write).await, 1);
    // Write vs ReadOnly must also serialize (write lock excludes readers).
    let manager = JobManager::new();
    let concurrent = Arc::new(AtomicUsize::new(0));
    let max_seen = Arc::new(AtomicUsize::new(0));
    let mut ids = Vec::new();
    for access in [WorkspaceAccess::Write, WorkspaceAccess::ReadOnly] {
        let c = concurrent.clone();
        let m = max_seen.clone();
        let id = manager
            .spawn(
                background_spec(JobKind::Lint, access, "mixed"),
                |_ctx| async move {
                    let now = c.fetch_add(1, Ordering::SeqCst) + 1;
                    loop {
                        let prev = m.load(Ordering::SeqCst);
                        if now <= prev
                            || m.compare_exchange(prev, now, Ordering::SeqCst, Ordering::SeqCst)
                                .is_ok()
                        {
                            break;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    c.fetch_sub(1, Ordering::SeqCst);
                    JobRunOutcome::Completed
                },
            )
            .unwrap();
        ids.push(id);
    }
    for id in ids {
        wait_for_terminal(&manager, id).await;
    }
    assert_eq!(max_seen.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_workspace_lock_wait_is_cancellable() {
    let manager = JobManager::new();
    let release = Arc::new(tokio::sync::Notify::new());
    let release_clone = release.clone();
    let holder_started = Arc::new(tokio::sync::Notify::new());
    let holder_started_clone = holder_started.clone();
    // Holder takes the write lock.
    let holder = manager
        .spawn(
            background_spec(JobKind::Lint, WorkspaceAccess::Write, "holder"),
            |_ctx| async move {
                holder_started_clone.notify_one();
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(30)) => JobRunOutcome::Completed,
                    _ = release_clone.notified() => JobRunOutcome::Completed,
                }
            },
        )
        .unwrap();
    holder_started.notified().await;
    // Give the holder time to acquire Running.
    wait_for_status(&manager, holder, JobStatus::Running).await;

    let executed = Arc::new(AtomicBool::new(false));
    let executed_clone = executed.clone();
    let waiter = manager
        .spawn(
            background_spec(JobKind::Lint, WorkspaceAccess::Write, "waiter"),
            |_ctx| async move {
                executed_clone.store(true, Ordering::SeqCst);
                JobRunOutcome::Completed
            },
        )
        .unwrap();
    wait_for_status(&manager, waiter, JobStatus::WaitingForWorkspace).await;
    manager.cancel(waiter);
    assert_eq!(
        wait_for_terminal(&manager, waiter).await,
        JobStatus::Cancelled
    );
    assert!(!executed.load(Ordering::SeqCst));
    release.notify_one();
    wait_for_terminal(&manager, holder).await;
}

#[tokio::test]
async fn test_panic_becomes_failed_and_releases_foreground() {
    let manager = JobManager::new();
    let id = manager
        .spawn(
            foreground_spec("panic", WorkspaceAccess::None),
            |_ctx| async {
                panic!("boom");
                #[allow(unreachable_code)]
                JobRunOutcome::Completed
            },
        )
        .unwrap();
    assert_eq!(wait_for_terminal(&manager, id).await, JobStatus::Failed);
    let snapshot = manager.get_snapshot(id).unwrap();
    assert_eq!(snapshot.error.as_deref(), Some("job panicked"));
    // Foreground slot must be reusable.
    let next = manager
        .spawn(
            foreground_spec("next", WorkspaceAccess::None),
            |_ctx| async { JobRunOutcome::Completed },
        )
        .unwrap();
    assert_eq!(
        wait_for_terminal(&manager, next).await,
        JobStatus::Completed
    );
}

#[tokio::test]
async fn test_shutdown_drains_and_rejects_new_spawns() {
    let manager = JobManager::new();
    let id = manager
        .spawn(
            background_spec(JobKind::Test, WorkspaceAccess::None, "quick"),
            |_ctx| async { JobRunOutcome::Completed },
        )
        .unwrap();
    wait_for_terminal(&manager, id).await;
    manager.shutdown(Duration::from_secs(1)).await;
    let err = manager
        .spawn(
            background_spec(JobKind::Test, WorkspaceAccess::None, "late"),
            |_ctx| async { JobRunOutcome::Completed },
        )
        .unwrap_err();
    assert!(matches!(err, JobStartError::ShuttingDown));
    assert_eq!(manager.active_count(), 0);
}

#[test]
fn test_cancelling_transition_monotonicity() {
    use JobStatus::*;
    // Cancelling never regresses into execution/waiting states.
    assert!(!Cancelling.allows_transition(Starting));
    assert!(!Cancelling.allows_transition(WaitingForWorkspace));
    assert!(!Cancelling.allows_transition(Running));
    // Cancelling itself and terminal outcomes stay reachable.
    assert!(Cancelling.allows_transition(Cancelling));
    assert!(Cancelling.allows_transition(Cancelled));
    assert!(Cancelling.allows_transition(Completed));
    assert!(Cancelling.allows_transition(Failed));
    // Terminal states are frozen.
    for terminal in [Completed, Failed, Cancelled] {
        for next in [
            Starting,
            WaitingForWorkspace,
            Running,
            Cancelling,
            Completed,
            Failed,
            Cancelled,
        ] {
            assert_eq!(
                terminal.allows_transition(next),
                terminal == next,
                "{terminal} -> {next}"
            );
        }
    }
    // Valid startup/cancel edges are preserved.
    assert!(Starting.allows_transition(WaitingForWorkspace));
    assert!(Starting.allows_transition(Running));
    assert!(Starting.allows_transition(Cancelling));
    assert!(WaitingForWorkspace.allows_transition(Running));
    assert!(WaitingForWorkspace.allows_transition(Cancelling));
    assert!(Running.allows_transition(Cancelling));
    assert!(Running.allows_transition(Completed));
}

#[tokio::test(flavor = "current_thread")]
async fn test_cancel_before_workspace_acquisition_never_regresses() {
    // On the current-thread runtime a spawned task cannot run before the
    // test task yields, so spawn+cancel back-to-back deterministically puts
    // cancellation before the task's first set_status(WaitingForWorkspace).
    let manager = JobManager::new();
    let release = Arc::new(tokio::sync::Notify::new());
    let release_clone = release.clone();
    let holder_started = Arc::new(tokio::sync::Notify::new());
    let holder_started_clone = holder_started.clone();
    let holder = manager
        .spawn(
            background_spec(JobKind::Lint, WorkspaceAccess::Write, "holder"),
            |_ctx| async move {
                holder_started_clone.notify_one();
                release_clone.notified().await;
                JobRunOutcome::Completed
            },
        )
        .unwrap();
    holder_started.notified().await;
    wait_for_status(&manager, holder, JobStatus::Running).await;

    let executed = Arc::new(AtomicBool::new(false));
    let executed_clone = executed.clone();
    let waiter = manager
        .spawn(
            background_spec(JobKind::Lint, WorkspaceAccess::Write, "waiter"),
            |_ctx| async move {
                executed_clone.store(true, Ordering::SeqCst);
                JobRunOutcome::Completed
            },
        )
        .unwrap();
    // No yield between spawn and cancel: cancellation strictly precedes the
    // task's first status transition.
    manager.cancel(waiter);
    assert_eq!(
        manager.get_snapshot(waiter).map(|s| s.status),
        Some(JobStatus::Cancelling)
    );
    // Let the waiter task run while the gate is still held by the holder.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let status = manager.get_snapshot(waiter).map(|s| s.status);
    assert!(
        matches!(
            status,
            Some(JobStatus::Cancelling) | Some(JobStatus::Cancelled)
        ),
        "cancelled job regressed to {status:?}"
    );
    release.notify_one();
    assert_eq!(
        wait_for_terminal(&manager, waiter).await,
        JobStatus::Cancelled
    );
    assert!(!executed.load(Ordering::SeqCst));
    wait_for_terminal(&manager, holder).await;
}

#[tokio::test]
async fn test_shutdown_force_abort_terminalizes_non_cooperative_job() {
    let manager = JobManager::new();
    // Deliberately non-cooperative: ignores the cancellation token and
    // would outlive any grace period without the forced-abort fallback.
    let id = manager
        .spawn(
            foreground_spec("stubborn", WorkspaceAccess::None),
            |_ctx| async move {
                tokio::time::sleep(Duration::from_secs(30)).await;
                JobRunOutcome::Completed
            },
        )
        .unwrap();
    wait_for_status(&manager, id, JobStatus::Running).await;
    let started = std::time::Instant::now();
    manager.shutdown(Duration::from_millis(50)).await;
    // Shutdown stays bounded: grace + fallback wait, well under the 30s body.
    assert!(started.elapsed() < Duration::from_secs(10));
    // Forced abort is never reported as successful completion.
    assert_eq!(
        manager.get_snapshot(id).map(|s| s.status),
        Some(JobStatus::Cancelled)
    );
    assert_eq!(manager.active_count(), 0);
    assert_eq!(manager.foreground_id(), None);
    // Exactly one terminal history record for the JobId.
    let matches: Vec<_> = manager
        .snapshots()
        .into_iter()
        .filter(|s| s.id == id)
        .collect();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].status, JobStatus::Cancelled);
}

#[tokio::test]
async fn test_shutdown_force_abort_clears_background_and_foreground() {
    let manager = JobManager::new();
    // One stubborn foreground job (holds the foreground reservation) plus
    // one stubborn background job: both ignore cancellation and must be
    // force-terminalized together.
    let fg = manager
        .spawn(
            foreground_spec("stubborn-fg", WorkspaceAccess::None),
            |_ctx| async move {
                tokio::time::sleep(Duration::from_secs(30)).await;
                JobRunOutcome::Completed
            },
        )
        .unwrap();
    wait_for_status(&manager, fg, JobStatus::Running).await;
    let bg = manager
        .spawn(
            background_spec(JobKind::Test, WorkspaceAccess::None, "stubborn-bg"),
            |_ctx| async move {
                tokio::time::sleep(Duration::from_secs(30)).await;
                JobRunOutcome::Completed
            },
        )
        .unwrap();
    wait_for_status(&manager, bg, JobStatus::Running).await;
    manager.shutdown(Duration::from_millis(50)).await;
    assert_eq!(manager.active_count(), 0);
    assert_eq!(manager.foreground_id(), None);
    assert_eq!(
        manager.get_snapshot(fg).map(|s| s.status),
        Some(JobStatus::Cancelled)
    );
    assert_eq!(
        manager.get_snapshot(bg).map(|s| s.status),
        Some(JobStatus::Cancelled)
    );
    // No duplicate history entries across the whole manager.
    let snapshots = manager.snapshots();
    let mut ids: Vec<JobId> = snapshots.iter().map(|s| s.id).collect();
    ids.sort_by_key(|id| id.0);
    let before = ids.len();
    ids.dedup_by_key(|id| id.0);
    assert_eq!(before, ids.len());
}

#[tokio::test]
async fn test_normal_completion_racing_shutdown_is_not_double_terminalized() {
    // A job that already completed before shutdown must keep its outcome
    // and gain no duplicate history entry.
    let manager = JobManager::new();
    let id = manager
        .spawn(
            background_spec(JobKind::Test, WorkspaceAccess::None, "quick"),
            |_ctx| async { JobRunOutcome::Completed },
        )
        .unwrap();
    assert_eq!(wait_for_terminal(&manager, id).await, JobStatus::Completed);
    manager.shutdown(Duration::from_secs(2)).await;
    assert_eq!(manager.active_count(), 0);
    let matches: Vec<_> = manager
        .snapshots()
        .into_iter()
        .filter(|s| s.id == id)
        .collect();
    assert_eq!(matches.len(), 1, "duplicate terminal record for {id}");
    assert_eq!(matches[0].status, JobStatus::Completed);

    // Completion racing shutdown fallback: the job finishes cooperatively
    // right around the grace expiry, so some iterations drain and others hit
    // the force-abort path. Either outcome is legal, but it must be
    // terminalized exactly once and never resurrected as active.
    for _ in 0..20 {
        let manager = JobManager::new();
        let id = manager
            .spawn(
                background_spec(JobKind::Test, WorkspaceAccess::None, "racer"),
                |ctx| async move {
                    tokio::select! {
                        _ = ctx.cancellation.cancelled() => JobRunOutcome::Cancelled,
                        _ = tokio::time::sleep(Duration::from_millis(50)) => {
                            JobRunOutcome::Completed
                        }
                    }
                },
            )
            .unwrap();
        manager.shutdown(Duration::from_millis(50)).await;
        assert_eq!(manager.active_count(), 0);
        let matches: Vec<_> = manager
            .snapshots()
            .into_iter()
            .filter(|s| s.id == id)
            .collect();
        assert_eq!(matches.len(), 1, "duplicate terminal record for {id}");
        assert!(
            matches!(
                matches[0].status,
                JobStatus::Completed | JobStatus::Cancelled
            ),
            "unexpected status {:?} for {id}",
            matches[0].status
        );
    }
}

#[tokio::test]
async fn test_spawn_cancel_shutdown_stress() {
    for _ in 0..10 {
        let manager = JobManager::new();
        let mut ids = Vec::new();
        for i in 0..8 {
            let id = manager
                .spawn(
                    background_spec(JobKind::Test, WorkspaceAccess::None, &format!("job {i}")),
                    |ctx| async move {
                        tokio::select! {
                            _ = ctx.cancellation.cancelled() => JobRunOutcome::Cancelled,
                            _ = tokio::time::sleep(Duration::from_millis(20)) => {
                                JobRunOutcome::Completed
                            }
                        }
                    },
                )
                .unwrap();
            ids.push(id);
        }
        for (n, id) in ids.iter().enumerate() {
            if n % 2 == 0 {
                manager.cancel(*id);
            }
        }
        manager.shutdown(Duration::from_millis(100)).await;
        assert_eq!(manager.active_count(), 0);
        assert_eq!(manager.foreground_id(), None);
        for id in &ids {
            let status = manager.get_snapshot(*id).map(|s| s.status);
            assert!(
                matches!(
                    status,
                    Some(JobStatus::Completed) | Some(JobStatus::Cancelled)
                ),
                "unexpected status {status:?} for {id}"
            );
        }
        let snapshots = manager.snapshots();
        let mut seen: Vec<JobId> = snapshots.iter().map(|s| s.id).collect();
        seen.sort_by_key(|id| id.0);
        let before = seen.len();
        seen.dedup_by_key(|id| id.0);
        assert_eq!(before, seen.len(), "duplicate history entries");
    }
}

#[tokio::test]
async fn test_concurrent_spawn_during_shutdown_leaves_no_active() {
    // Spawn racing shutdown: hammer spawn attempts while a forced shutdown
    // is in flight. Every accepted id must end terminal exactly once, and
    // shutdown must leave no active or foreground records. This exercises
    // the spawn-race guard path where the fallback may have already removed
    // the record before the spawn task stores its abort handle.
    for _ in 0..10 {
        let manager = JobManager::new();
        // One stubborn job forces the fallback path.
        let _stubborn = manager
            .spawn(
                background_spec(JobKind::Test, WorkspaceAccess::None, "stubborn"),
                |_ctx| async move {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    JobRunOutcome::Completed
                },
            )
            .unwrap();
        let manager_clone = manager.clone();
        let shutdown =
            tokio::spawn(async move { manager_clone.shutdown(Duration::from_millis(20)).await });
        let mut ids = Vec::new();
        for i in 0..20 {
            if let Ok(id) = manager.spawn(
                background_spec(JobKind::Test, WorkspaceAccess::None, &format!("racer {i}")),
                |ctx| async move {
                    tokio::select! {
                        _ = ctx.cancellation.cancelled() => JobRunOutcome::Cancelled,
                        _ = tokio::time::sleep(Duration::from_millis(10)) => {
                            JobRunOutcome::Completed
                        }
                    }
                },
            ) {
                ids.push(id);
            }
        }
        shutdown.await.unwrap();
        assert_eq!(manager.active_count(), 0);
        assert_eq!(manager.foreground_id(), None);
        for id in &ids {
            let status = manager.get_snapshot(*id).map(|s| s.status);
            assert!(
                matches!(
                    status,
                    Some(JobStatus::Completed) | Some(JobStatus::Cancelled)
                ),
                "unexpected status {status:?} for {id}"
            );
            let matches: Vec<_> = manager
                .snapshots()
                .into_iter()
                .filter(|s| s.id == *id)
                .collect();
            assert_eq!(matches.len(), 1, "duplicate terminal record for {id}");
        }
        let snapshots = manager.snapshots();
        let mut seen: Vec<JobId> = snapshots.iter().map(|s| s.id).collect();
        seen.sort_by_key(|id| id.0);
        let before = seen.len();
        seen.dedup_by_key(|id| id.0);
        assert_eq!(before, seen.len(), "duplicate history entries");
    }
}

#[test]
fn test_job_id_display_and_parse() {
    assert_eq!(JobId(12).to_string(), "job-12");
    assert_eq!(JobId::parse_arg("12"), Some(JobId(12)));
    assert_eq!(JobId::parse_arg("job-12"), Some(JobId(12)));
    assert_eq!(JobId::parse_arg("  job-7  "), Some(JobId(7)));
    assert_eq!(JobId::parse_arg("nope"), None);
}

#[test]
fn test_semantic_edit_kind_display() {
    assert_eq!(JobKind::SemanticEdit.to_string(), "semantic_edit");
}

#[test]
fn test_snapshot_display_line_contains_fields() {
    let manager = JobManager::new();
    let spec = JobSpec::new(
        JobKind::AgentTurn,
        JobScope::Foreground,
        WorkspaceAccess::Write,
        "Fix login handler",
    );
    // Use a synchronous spawn that completes immediately; snapshot comes
    // from recent history.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let id = manager
            .spawn(spec, |_ctx: JobContext| async { JobRunOutcome::Completed })
            .unwrap();
        wait_for_terminal(&manager, id).await;
        let snapshot = manager.get_snapshot(id).unwrap();
        let line = snapshot.display_line();
        assert!(line.contains("job-"));
        assert!(line.contains("agent_turn"));
        assert!(line.contains("completed"));
        assert!(line.contains("Fix login handler"));
    });
}

#[tokio::test]
async fn test_semantic_edit_foreground_exclusivity() {
    let manager = JobManager::new();
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let started_clone = started.clone();
    let release_clone = release.clone();
    let a = manager
        .spawn(
            JobSpec::new(
                JobKind::SemanticEdit,
                JobScope::Foreground,
                WorkspaceAccess::Write,
                "semantic edit a",
            ),
            move |ctx| async move {
                started_clone.notify_one();
                tokio::select! {
                    _ = ctx.cancellation.cancelled() => JobRunOutcome::Cancelled,
                    _ = release_clone.notified() => JobRunOutcome::Completed,
                }
            },
        )
        .unwrap();
    started.notified().await;
    let err = manager
        .spawn(
            JobSpec::new(
                JobKind::SemanticEdit,
                JobScope::Foreground,
                WorkspaceAccess::Write,
                "semantic edit b",
            ),
            |_ctx| async { JobRunOutcome::Completed },
        )
        .unwrap_err();
    assert!(matches!(err, JobStartError::ForegroundBusy { .. }));
    release.notify_one();
    assert_eq!(wait_for_terminal(&manager, a).await, JobStatus::Completed);
}

#[tokio::test]
async fn test_semantic_edit_cancellation_leaves_file_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lib.rs");
    std::fs::write(&path, "fn foo() {}\n").unwrap();
    let before = std::fs::read_to_string(&path).unwrap();

    let manager = JobManager::new();
    let started = Arc::new(tokio::sync::Notify::new());
    let started_clone = started.clone();
    let path_clone = path.clone();
    let id = manager
        .spawn(
            JobSpec::new(
                JobKind::SemanticEdit,
                JobScope::Foreground,
                WorkspaceAccess::Write,
                "semantic edit cancel",
            ),
            move |ctx| async move {
                started_clone.notify_one();
                // Simulate LLM wait: cancellable sleep, no mutation.
                tokio::select! {
                    _ = ctx.cancellation.cancelled() => {
                        let content = std::fs::read_to_string(&path_clone)
                            .unwrap_or_default();
                        assert_eq!(content, "fn foo() {}\n");
                        JobRunOutcome::Cancelled
                    }
                    _ = tokio::time::sleep(Duration::from_secs(30)) => JobRunOutcome::Completed,
                }
            },
        )
        .unwrap();
    started.notified().await;
    wait_for_status(&manager, id, JobStatus::Running).await;
    manager.cancel(id);
    assert_eq!(wait_for_terminal(&manager, id).await, JobStatus::Cancelled);
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(before, after);
}

#[tokio::test]
async fn test_completion_hook_fires_after_foreground_release() {
    use std::sync::Mutex;
    /// What the hook observed: producer id, foreground slot at fire time,
    /// and the terminal outcome.
    type HookObservation = Option<(JobId, Option<JobId>, JobRunOutcome)>;
    let manager = JobManager::new();
    let seen: Arc<Mutex<HookObservation>> = Arc::new(Mutex::new(None));
    let seen_clone = seen.clone();
    let fired = Arc::new(tokio::sync::Notify::new());
    let fired_clone = fired.clone();
    let manager_clone = manager.clone();
    manager.set_completion_hook(Arc::new(move |completion: crate::jobs::JobCompletion| {
        // The hook must observe the foreground slot already released.
        let foreground = manager_clone.foreground_id();
        *seen_clone.lock().unwrap() = Some((completion.id, foreground, completion.outcome));
        fired_clone.notify_one();
    }));
    let id = manager
        .spawn(
            foreground_spec("producer", WorkspaceAccess::None),
            |_ctx| async { JobRunOutcome::Completed },
        )
        .unwrap();
    // Bounded wait for the hook notification (channel-style
    // synchronization, not a timing workaround: the hook fires
    // synchronously inside finish_job on the task thread).
    tokio::time::timeout(Duration::from_secs(5), fired.notified())
        .await
        .expect("completion hook must fire");
    assert_eq!(wait_for_terminal(&manager, id).await, JobStatus::Completed);
    let (hook_id, foreground_at_hook, outcome) = seen.lock().unwrap().clone().unwrap();
    assert_eq!(hook_id, id);
    assert_eq!(
        foreground_at_hook, None,
        "hook must fire only after foreground ownership is released"
    );
    assert_eq!(outcome, JobRunOutcome::Completed);
}

#[tokio::test]
async fn test_completion_hook_fires_once_per_job() {
    let count = Arc::new(AtomicUsize::new(0));
    let count_clone = count.clone();
    let manager = JobManager::new();
    manager.set_completion_hook(Arc::new(move |_completion: crate::jobs::JobCompletion| {
        count_clone.fetch_add(1, Ordering::SeqCst);
    }));
    let id = manager
        .spawn(
            foreground_spec("once", WorkspaceAccess::None),
            |_ctx| async { JobRunOutcome::Completed },
        )
        .unwrap();
    assert_eq!(wait_for_terminal(&manager, id).await, JobStatus::Completed);
    // Shutdown terminalization must not double-fire: the record is already
    // gone, so finish_job is a no-op.
    manager.shutdown(Duration::from_secs(2)).await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_completion_hook_reports_cancellation_outcome() {
    use std::sync::Mutex;
    let manager = JobManager::new();
    let seen: Arc<Mutex<Option<JobRunOutcome>>> = Arc::new(Mutex::new(None));
    let seen_clone = seen.clone();
    manager.set_completion_hook(Arc::new(move |completion: crate::jobs::JobCompletion| {
        *seen_clone.lock().unwrap() = Some(completion.outcome);
    }));
    let started = Arc::new(tokio::sync::Notify::new());
    let started_clone = started.clone();
    let id = manager
        .spawn(
            foreground_spec("cancellable", WorkspaceAccess::None),
            move |ctx| async move {
                started_clone.notify_one();
                tokio::select! {
                    _ = ctx.cancellation.cancelled() => JobRunOutcome::Cancelled,
                    _ = tokio::time::sleep(Duration::from_secs(30)) => JobRunOutcome::Completed,
                }
            },
        )
        .unwrap();
    started.notified().await;
    manager.cancel(id);
    assert_eq!(wait_for_terminal(&manager, id).await, JobStatus::Cancelled);
    assert_eq!(
        seen.lock().unwrap().clone(),
        Some(JobRunOutcome::Cancelled),
        "cancellation must be visible to the hook so subscribers can suppress follow-ups"
    );
}
