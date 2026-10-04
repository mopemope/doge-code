use crate::jobs::types::{
    CancelJobResult, JobCompletion, JobContext, JobId, JobKind, JobRunOutcome, JobScope,
    JobSnapshot, JobSpec, JobStartError, JobStatus, WorkspaceAccess, bound_error, format_elapsed,
};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

const SHUTDOWN_FALLBACK_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// Post-terminal completion hook. Invoked exactly once per terminalized
/// job, strictly after foreground ownership is released and terminal
/// history is recorded. Never carries domain payloads: subscribers key
/// their own deferred work by `JobCompletion.id`.
pub type JobCompletionHook = Arc<dyn Fn(JobCompletion) + Send + Sync>;

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
    completion_hook: std::sync::Mutex<Option<JobCompletionHook>>,
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
                completion_hook: std::sync::Mutex::new(None),
            }),
        }
    }

    /// Install the post-terminal completion hook. The hook fires exactly
    /// once per terminalized job, after foreground release. Replaces any
    /// previously installed hook.
    pub fn set_completion_hook(&self, hook: JobCompletionHook) {
        *self
            .inner
            .completion_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(hook);
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
        // two paths records the outcome. The shutdown flag is checked
        // independently of whether the record is still present: the
        // shutdown fallback may have already terminalized (removed) this
        // record, in which case the handle must still be aborted to avoid
        // leaking a running task past shutdown.
        let shutdown_race = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(record) = state.active.get_mut(&id) {
                record.abort_handle = Some(abort_handle.clone());
            }
            !self.inner.accepting.load(Ordering::SeqCst)
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
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(record) = state.active.get_mut(&id) {
            // Monotonic visibility: terminal states never leave, and
            // Cancelling never regresses into execution/waiting states
            // (a task racing cancellation must not step back to
            // WaitingForWorkspace or Running).
            if !record.status.allows_transition(status) {
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

    /// Serialize a short synchronous irreversible commit and terminalization
    /// with cancellation/shutdown bookkeeping. No awaits or JobManager calls
    /// are allowed in `commit`. A shutdown cannot report a saved commit as
    /// cancelled or return while this commit is still mutating the session.
    pub(crate) fn finish_synchronous_commit(
        &self,
        id: JobId,
        commit: impl FnOnce() -> JobRunOutcome,
    ) -> JobRunOutcome {
        let (outcome, finished) = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let Some(record) = state.active.get(&id) else {
                return JobRunOutcome::Cancelled;
            };
            let outcome = if record.status == JobStatus::Cancelling || record.token.is_cancelled() {
                JobRunOutcome::Cancelled
            } else {
                commit()
            };
            let finished = Self::finish_locked(&mut state, id, outcome.clone());
            (outcome, finished)
        };
        if let Some((snapshot, completion)) = finished {
            self.publish_completion(snapshot, completion);
        }
        outcome
    }

    fn finish_locked(
        state: &mut JobState,
        id: JobId,
        outcome: JobRunOutcome,
    ) -> Option<(JobSnapshot, JobCompletion)> {
        let mut record = state.active.remove(&id)?;
        let (status, error) = match &outcome {
            JobRunOutcome::Completed => (JobStatus::Completed, None),
            JobRunOutcome::Cancelled => (JobStatus::Cancelled, None),
            JobRunOutcome::Failed { message } => (JobStatus::Failed, Some(bound_error(message))),
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
        let completion = JobCompletion {
            id: terminal.id,
            kind: terminal.kind,
            scope: terminal.scope,
            outcome,
        };
        Some((terminal, completion))
    }

    fn finish_job(&self, id: JobId, outcome: JobRunOutcome) {
        let finished = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            Self::finish_locked(&mut state, id, outcome)
        };
        if let Some((snapshot, completion)) = finished {
            self.publish_completion(snapshot, completion);
        }
    }

    fn publish_completion(&self, snapshot: JobSnapshot, completion: JobCompletion) {
        let id = snapshot.id;
        tracing::info!(
            job_id = %id,
            job_kind = %snapshot.kind,
            status = %snapshot.status,
            elapsed = %format_elapsed(snapshot.elapsed_ms),
            "job finished"
        );
        // Eligibility signal for deferred successors. Fires strictly after
        // foreground release above, outside the state lock. `finish_job`
        // removes the active record first, so a completion racing shutdown
        // terminalization delivers exactly one hook call per JobId.
        let hook = self
            .inner
            .completion_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(hook) = hook {
            hook(completion);
        }
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
        let (stuck, handles): (usize, Vec<tokio::task::AbortHandle>) = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            // Count every still-active record as stuck (some may not have a
            // stored abort handle yet; the leftover sweep below re-aborts
            // with current handles and terminalizes regardless).
            let stuck = state.active.len();
            let handles = state
                .active
                .values()
                .filter_map(|record| record.abort_handle.clone())
                .collect();
            (stuck, handles)
        };
        for handle in &handles {
            handle.abort();
        }
        if stuck > 0 {
            tracing::warn!(stuck, "force-aborted jobs stuck past shutdown grace period");
        }
        let _ = tokio::time::timeout(SHUTDOWN_FALLBACK_WAIT, self.inner.tracker.wait()).await;
        // A force-aborted future never reaches `run_job_task -> finish_job`,
        // so terminalize every record still active through the same
        // bookkeeping authority that clears foreground ownership and creates
        // terminal history. `finish_job` removes the active record first, so
        // a normal completion racing this fallback cannot double-terminalize
        // (exactly one path records the outcome per JobId). Forced abort is
        // never reported as successful completion: leftovers become
        // `Cancelled`. Collect all still-active records (not just the
        // pre-abort snapshot) so a spawn racing shutdown cannot leak, and
        // re-abort their current handles to cover handles stored after the
        // first snapshot.
        let leftover: Vec<(JobId, Option<tokio::task::AbortHandle>)> = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state
                .active
                .iter()
                .map(|(id, record)| (*id, record.abort_handle.clone()))
                .collect()
        };
        if !leftover.is_empty() {
            tracing::warn!(
                count = leftover.len(),
                "terminalizing jobs still active after forced abort"
            );
        }
        for (id, handle) in &leftover {
            // Abort is idempotent: covers handles stored after the first
            // sweep and late-spawned records alike.
            if let Some(handle) = handle {
                handle.abort();
            }
            self.finish_job(*id, JobRunOutcome::Cancelled);
        }
        // The second sweep may have aborted handles stored after the first
        // snapshot (or a spawn-race task aborted by the spawn guard), so
        // drain once more. Bounded by the same fallback timeout; aborts are
        // not ignorable, so this converges while keeping shutdown bounded.
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
