use crate::jobs::types::{
    CancelJobResult, JobContext, JobId, JobKind, JobRunOutcome, JobScope, JobSnapshot, JobSpec,
    JobStartError, JobStatus, WorkspaceAccess, bound_error, format_elapsed,
};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

const SHUTDOWN_FALLBACK_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

struct JobRecord {
    id: JobId,
    kind: JobKind,
    scope: JobScope,
    workspace_access: WorkspaceAccess,
    status: JobStatus,
    label: String,
    started_at: Instant,
    error: Option<String>,
    token: CancellationToken,
    abort_handle: Option<tokio::task::AbortHandle>,
}

impl JobRecord {
    fn snapshot(&self) -> JobSnapshot {
        JobSnapshot {
            id: self.id,
            kind: self.kind,
            scope: self.scope,
            workspace_access: self.workspace_access,
            status: self.status,
            label: self.label.clone(),
            elapsed_ms: self.started_at.elapsed().as_millis(),
            error: self.error.clone(),
        }
    }
}

struct JobState {
    active: HashMap<JobId, JobRecord>,
    foreground_job: Option<JobId>,
    recent: VecDeque<JobSnapshot>,
}

impl JobState {
    fn new() -> Self {
        Self {
            active: HashMap::new(),
            foreground_job: None,
            recent: VecDeque::new(),
        }
    }
}

struct JobManagerInner {
    state: std::sync::Mutex<JobState>,
    tracker: TaskTracker,
    shutdown: CancellationToken,
    workspace_gate: Arc<tokio::sync::RwLock<()>>,
    next_id: AtomicU64,
    accepting: AtomicBool,
}

/// Central ownership for user-visible long-running work.
///
/// Protocol-agnostic: no TUI, MCP, or ACP types leak into this module.
#[derive(Clone)]
pub struct JobManager {
    inner: Arc<JobManagerInner>,
}

impl Default for JobManager {
    fn default() -> Self {
        Self::new()
    }
}

impl JobManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(JobManagerInner {
                state: std::sync::Mutex::new(JobState::new()),
                tracker: TaskTracker::new(),
                shutdown: CancellationToken::new(),
                workspace_gate: Arc::new(tokio::sync::RwLock::new(())),
                next_id: AtomicU64::new(1),
                accepting: AtomicBool::new(true),
            }),
        }
    }

    /// Spawn a job. Reservation (foreground check, id allocation, record
    /// insert) is atomic under the state mutex; no `.await` is held.
    pub fn spawn<F, Fut>(&self, spec: JobSpec, run: F) -> Result<JobId, JobStartError>
    where
        F: FnOnce(JobContext) -> Fut + Send + 'static,
        Fut: Future<Output = JobRunOutcome> + Send + 'static,
    {
        if !self.inner.accepting.load(Ordering::SeqCst) {
            return Err(JobStartError::ShuttingDown);
        }

        let id;
        let token;
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if !self.inner.accepting.load(Ordering::SeqCst) {
                return Err(JobStartError::ShuttingDown);
            }
            if spec.scope == JobScope::Foreground
                && let Some(active_id) = state.foreground_job
            {
                if let Some(record) = state.active.get(&active_id) {
                    return Err(JobStartError::ForegroundBusy {
                        active: record.snapshot(),
                    });
                }
                // Stale reservation (should not happen; finish_job releases
                // it). Clear defensively so one leaked id cannot wedge the
                // foreground slot forever.
                state.foreground_job = None;
            }
            id = JobId(self.inner.next_id.fetch_add(1, Ordering::SeqCst));
            token = self.inner.shutdown.child_token();
            let record = JobRecord {
                id,
                kind: spec.kind,
                scope: spec.scope,
                workspace_access: spec.workspace_access,
                status: JobStatus::Starting,
                label: spec.label.clone(),
                started_at: Instant::now(),
                error: None,
                token: token.clone(),
                abort_handle: None,
            };
            if spec.scope == JobScope::Foreground {
                state.foreground_job = Some(id);
            }
            state.active.insert(id, record);
        }

        tracing::info!(
            job_id = %id,
            job_kind = %spec.kind,
            scope = %spec.scope,
            workspace_access = %spec.workspace_access,
            "job started"
        );

        let this = self.clone();
        let gate = self.inner.workspace_gate.clone();
        let workspace_access = spec.workspace_access;
        let ctx = JobContext {
            id,
            cancellation: token.clone(),
        };
        let join_handle = self.inner.tracker.spawn(async move {
            this.run_job_task(id, workspace_access, gate, ctx, run)
                .await;
        });
        // Save the abort handle for the shutdown fallback only. Dropping
        // the JoinHandle detaches it; the TaskTracker still tracks the task.
        let abort_handle = join_handle.abort_handle();
        // If shutdown() ran concurrently past close/wait, this task may no
        // longer be waited on. Abort it now (forced abort is permitted in
        // the shutdown path) and record the terminal state here: the
        // aborted task never reaches finish_job itself, and finish_job is
        // a no-op when the record is already gone, so exactly one of the
        // two paths records the outcome.
        let shutdown_race = {
            let mut race = false;
            if let Ok(mut state) = self.inner.state.lock()
                && let Some(record) = state.active.get_mut(&id)
            {
                record.abort_handle = Some(abort_handle.clone());
                race = !self.inner.accepting.load(Ordering::SeqCst);
            }
            race
        };
        if shutdown_race {
            abort_handle.abort();
            self.finish_job(id, JobRunOutcome::Cancelled);
        }

        Ok(id)
    }

    async fn run_job_task<F, Fut>(
        &self,
        id: JobId,
        workspace_access: WorkspaceAccess,
        gate: Arc<tokio::sync::RwLock<()>>,
        ctx: JobContext,
        run: F,
    ) where
        F: FnOnce(JobContext) -> Fut + Send + 'static,
        Fut: Future<Output = JobRunOutcome> + Send + 'static,
    {
        let token = ctx.cancellation.clone();

        match workspace_access {
            WorkspaceAccess::None => {
                if token.is_cancelled() {
                    self.finish_job(id, JobRunOutcome::Cancelled);
                    return;
                }
                self.set_status(id, JobStatus::Running);
                let outcome = run_guarded(ctx, run).await;
                self.finish_job(id, outcome);
            }
            WorkspaceAccess::ReadOnly => {
                self.set_status(id, JobStatus::WaitingForWorkspace);
                let gate_clone = gate.clone();
                let acquired = tokio::select! {
                    biased;
                    _ = token.cancelled() => None,
                    guard = gate_clone.read_owned() => Some(guard),
                };
                let Some(_read_guard) = acquired else {
                    self.finish_job(id, JobRunOutcome::Cancelled);
                    return;
                };
                if token.is_cancelled() {
                    self.finish_job(id, JobRunOutcome::Cancelled);
                    return;
                }
                self.set_status(id, JobStatus::Running);
                let outcome = run_guarded(ctx, run).await;
                self.finish_job(id, outcome);
            }
            WorkspaceAccess::Write => {
                self.set_status(id, JobStatus::WaitingForWorkspace);
                let gate_clone = gate.clone();
                let acquired = tokio::select! {
                    biased;
                    _ = token.cancelled() => None,
                    guard = gate_clone.write_owned() => Some(guard),
                };
                let Some(_write_guard) = acquired else {
                    self.finish_job(id, JobRunOutcome::Cancelled);
                    return;
                };
                if token.is_cancelled() {
                    self.finish_job(id, JobRunOutcome::Cancelled);
                    return;
                }
                self.set_status(id, JobStatus::Running);
                let outcome = run_guarded(ctx, run).await;
                self.finish_job(id, outcome);
            }
        }
    }

    fn set_status(&self, id: JobId, status: JobStatus) {
        if let Ok(mut state) = self.inner.state.lock()
            && let Some(record) = state.active.get_mut(&id)
        {
            // Do not overwrite a terminal state or step backwards from
            // Cancelling to Running.
            if record.status.is_terminal() {
                return;
            }
            if record.status == JobStatus::Cancelling && status == JobStatus::Running {
                return;
            }
            record.status = status;
        }
    }

    /// Cooperative cancel. Never aborts the task; the job observes its
    /// `CancellationToken` and returns `JobRunOutcome::Cancelled` after
    /// cleaning up (process-tree termination, reap, etc.).
    pub fn cancel(&self, id: JobId) -> CancelJobResult {
        let token = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(record) = state.active.get_mut(&id) {
                if record.status.is_terminal() {
                    return CancelJobResult::AlreadyFinished { id };
                }
                if record.status != JobStatus::Cancelling {
                    record.status = JobStatus::Cancelling;
                }
                tracing::info!(job_id = %id, "job cancelling");
                record.token.clone()
            } else if state.recent.iter().any(|snapshot| snapshot.id == id) {
                return CancelJobResult::AlreadyFinished { id };
            } else {
                return CancelJobResult::NotFound { id };
            }
        };
        token.cancel();
        CancelJobResult::Cancelled { id }
    }

    /// Cancel the current foreground job (running preferred over waiting).
    pub fn cancel_foreground(&self) -> Option<CancelJobResult> {
        let id = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.foreground_job?
        };
        Some(self.cancel(id))
    }

    pub fn foreground_id(&self) -> Option<JobId> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .foreground_job
    }

    pub fn get_snapshot(&self, id: JobId) -> Option<JobSnapshot> {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(record) = state.active.get(&id) {
            return Some(record.snapshot());
        }
        state.recent.iter().find(|s| s.id == id).cloned()
    }

    /// Active snapshots first (oldest first), then recent terminal history
    /// (newest first).
    pub fn snapshots(&self) -> Vec<JobSnapshot> {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut active: Vec<JobSnapshot> = state.active.values().map(JobRecord::snapshot).collect();
        active.sort_by_key(|s| s.id.0);
        let mut out = active;
        // Recent is stored newest-first; keep that order after actives.
        out.extend(state.recent.iter().cloned());
        out
    }

    pub fn active_count(&self) -> usize {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .len()
    }

    fn finish_job(&self, id: JobId, outcome: JobRunOutcome) {
        let snapshot = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let Some(mut record) = state.active.remove(&id) else {
                return;
            };
            let (status, error) = match outcome {
                JobRunOutcome::Completed => (JobStatus::Completed, None),
                JobRunOutcome::Cancelled => (JobStatus::Cancelled, None),
                JobRunOutcome::Failed { message } => {
                    (JobStatus::Failed, Some(bound_error(&message)))
                }
            };
            record.status = status;
            record.error = error.clone();
            if state.foreground_job == Some(id) {
                state.foreground_job = None;
            }
            let mut terminal = record.snapshot();
            // Ensure the terminal status/error rendered in history matches
            // the outcome even though the record was removed from active.
            terminal.status = status;
            terminal.error = error;
            state.recent.push_front(terminal.clone());
            while state.recent.len() > crate::jobs::types::MAX_RECENT_JOBS {
                state.recent.pop_back();
            }
            terminal
        };
        tracing::info!(
            job_id = %id,
            job_kind = %snapshot.kind,
            status = %snapshot.status,
            elapsed = %format_elapsed(snapshot.elapsed_ms),
            "job finished"
        );
    }

    /// Graceful shutdown: stop accepting, cancel the root token (all child
    /// job tokens), close the tracker, wait out the grace period, then force
    /// abort anything still stuck as a last resort.
    pub async fn shutdown(&self, grace: std::time::Duration) {
        self.inner.accepting.store(false, Ordering::SeqCst);
        self.inner.shutdown.cancel();
        self.inner.tracker.close();
        let drained = tokio::time::timeout(grace, self.inner.tracker.wait())
            .await
            .is_ok();
        if drained {
            return;
        }
        // Fallback: abort stuck tasks. Cooperative cancellation remains the
        // primary path; this only fires when a job ignores its token past
        // the grace period during process shutdown.
        let handles: Vec<tokio::task::AbortHandle> = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state
                .active
                .values()
                .filter_map(|record| record.abort_handle.clone())
                .collect()
        };
        let stuck = handles.len();
        for handle in &handles {
            handle.abort();
        }
        if stuck > 0 {
            tracing::warn!(stuck, "force-aborted jobs stuck past shutdown grace period");
        }
        let _ = tokio::time::timeout(SHUTDOWN_FALLBACK_WAIT, self.inner.tracker.wait()).await;
    }
}

/// Run the user future while converting panics into infrastructure failures.
async fn run_guarded<F, Fut>(ctx: JobContext, run: F) -> JobRunOutcome
where
    F: FnOnce(JobContext) -> Fut + Send + 'static,
    Fut: Future<Output = JobRunOutcome> + Send + 'static,
{
    let future = run(ctx);
    let result = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(future)).await;
    match result {
        Ok(outcome) => outcome,
        Err(_) => {
            // Never surface panic payloads (may contain sensitive content).
            tracing::error!("job task panicked; recording terminal failure");
            JobRunOutcome::Failed {
                message: "job panicked".to_string(),
            }
        }
    }
}
