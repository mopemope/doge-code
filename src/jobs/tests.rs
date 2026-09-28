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
fn test_job_id_display_and_parse() {
    assert_eq!(JobId(12).to_string(), "job-12");
    assert_eq!(JobId::parse_arg("12"), Some(JobId(12)));
    assert_eq!(JobId::parse_arg("job-12"), Some(JobId(12)));
    assert_eq!(JobId::parse_arg("  job-7  "), Some(JobId(7)));
    assert_eq!(JobId::parse_arg("nope"), None);
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
async fn test_completion_hook_fires_after_foreground_release() {
    use crate::jobs::JobCompletion;
    use std::sync::Mutex;

    // The hook must observe the producer as terminal with the foreground
    // slot already released, so a successor spawn inside a follow-up cannot
    // race its own producer.
    type Observed = Vec<(JobCompletion, Option<JobId>, Option<JobStatus>)>;
    let manager = JobManager::new();
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let probe = manager.clone();
    let observed_clone = observed.clone();
    manager.set_completion_hook(Arc::new(move |completion: JobCompletion| {
        let foreground = probe.foreground_id();
        let status = probe.get_snapshot(completion.id).map(|s| s.status);
        observed_clone
            .lock()
            .unwrap()
            .push((completion, foreground, status));
    }));

    let id = manager
        .spawn(foreground_spec("a", WorkspaceAccess::None), |_ctx| async {
            JobRunOutcome::Completed
        })
        .unwrap();
    assert_eq!(wait_for_terminal(&manager, id).await, JobStatus::Completed);

    {
        let observed = observed.lock().unwrap();
        assert_eq!(observed.len(), 1, "hook must fire exactly once");
        let (completion, foreground_at_hook, status_at_hook) = &observed[0];
        assert_eq!(completion.id, id);
        assert_eq!(completion.kind, JobKind::AgentTurn);
        assert_eq!(completion.scope, JobScope::Foreground);
        assert_eq!(completion.status, JobStatus::Completed);
        assert_eq!(
            *foreground_at_hook, None,
            "foreground reservation must be released before the hook fires"
        );
        assert_eq!(
            *status_at_hook,
            Some(JobStatus::Completed),
            "producer must be terminal before the hook fires"
        );
    }
    // The freed slot is immediately reusable. Note: the `observed` guard
    // above must be dropped before spawning/waiting below. The completion
    // hook runs synchronously on the finishing job task and locks the same
    // mutex; holding the guard across the await deadlocks the
    // current-thread test runtime when the successor job fires the hook.
    let next = manager
        .spawn(foreground_spec("b", WorkspaceAccess::None), |_ctx| async {
            JobRunOutcome::Completed
        })
        .unwrap();
    assert_eq!(
        wait_for_terminal(&manager, next).await,
        JobStatus::Completed
    );
}

#[tokio::test]
async fn test_completion_hook_reports_cancellation() {
    use crate::jobs::JobCompletion;
    use std::sync::Mutex;

    let manager = JobManager::new();
    let observed: Arc<Mutex<Vec<JobCompletion>>> = Arc::new(Mutex::new(Vec::new()));
    let observed_clone = observed.clone();
    manager.set_completion_hook(Arc::new(move |completion: JobCompletion| {
        observed_clone.lock().unwrap().push(completion);
    }));

    let id = manager
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
    wait_for_status(&manager, id, JobStatus::Running).await;
    manager.cancel(id);
    assert_eq!(wait_for_terminal(&manager, id).await, JobStatus::Cancelled);

    let observed = observed.lock().unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].id, id);
    assert_eq!(observed[0].status, JobStatus::Cancelled);
}

#[tokio::test]
async fn test_completion_hook_reports_infrastructure_failure() {
    use crate::jobs::JobCompletion;
    use std::sync::Mutex;

    let manager = JobManager::new();
    let observed: Arc<Mutex<Vec<JobCompletion>>> = Arc::new(Mutex::new(Vec::new()));
    let observed_clone = observed.clone();
    manager.set_completion_hook(Arc::new(move |completion: JobCompletion| {
        observed_clone.lock().unwrap().push(completion);
    }));

    let id = manager
        .spawn(foreground_spec("a", WorkspaceAccess::None), |_ctx| async {
            JobRunOutcome::Failed {
                message: "boom".to_string(),
            }
        })
        .unwrap();
    assert_eq!(wait_for_terminal(&manager, id).await, JobStatus::Failed);

    let observed = observed.lock().unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].id, id);
    assert_eq!(observed[0].status, JobStatus::Failed);
    assert_eq!(observed[0].error.as_deref(), Some("boom"));
}
