//! Deferred post-completion follow-up handoff for `/test` and `/lint`.
//!
//! Problem: producers run as foreground jobs on the shared [`JobManager`],
//! which rejects a second foreground reservation while one is active. The
//! old contract had producers send `::test_failures_analysis:` /
//! `::lint_issues:` / `::lint_command_output_analysis:` from inside the job
//! future, and the event loop dispatched the follow-up `AgentTurn`
//! immediately — while the producer still owned the foreground slot. The
//! follow-up was rejected as `ForegroundBusy` and lost.
//!
//! Contract: producers send one [`DeferredFollowup`] (`::defer_followup:`)
//! keyed by their own [`JobId`]. The manager fires its protocol-agnostic
//! completion hook after terminalization releases the active record and
//! foreground reservation; the TUI translates that into `::job_completed:`.
//! [`TuiExecutor`] stores the deferred payload on arrival and dispatches it
//! only when the completion signal for that exact producer arrives, the
//! producer snapshot is terminal `Completed`, and no foreground job is
//! active. No sleeps, yields, polling, or retries: both signals are channel
//! messages drained by the event loop in arrival order.
//!
//! [`JobManager`]: crate::jobs::JobManager
//! [`JobId`]: crate::jobs::JobId
//! [`TuiExecutor`]: crate::tui::commands::core::TuiExecutor

use crate::jobs::{JobCompletion, JobId, JobKind, JobScope, JobStatus};
use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;
use std::sync::Arc;

/// UI-channel prefix for a deferred follow-up payload (JSON [`DeferredFollowup`]).
pub const DEFER_FOLLOWUP_PREFIX: &str = "::defer_followup:";
/// UI-channel prefix for a post-terminal completion signal (`<job-id>` as u64).
pub const JOB_COMPLETED_PREFIX: &str = "::job_completed:";

/// Defensive prompt bound applied at store time. Producers already budget
/// their prompts; this only guards against malformed oversized payloads.
pub const FOLLOWUP_PROMPT_BUDGET_CHARS: usize = 32_000;

/// Bounded count of consumed producer ids kept for idempotence.
/// Evicted oldest-first (producer ids are monotonic): only the oldest entry
/// is dropped on overflow, so a recently consumed producer can never be
/// re-armed by a stale duplicate defer.
const MAX_CONSUMED_FOLLOWUPS: usize = 128;

/// Bounded count of pending deferred follow-ups. Each producer stores at
/// most one entry; a hard cap prevents an unbounded channel peer from
/// growing the map without bound.
const MAX_PENDING_FOLLOWUPS: usize = 128;

/// Defensive bound for the one-line dispatch notice. Producer notices are
/// short fixed strings; this only guards against forged oversized payloads
/// reaching the TUI log.
const FOLLOWUP_NOTICE_BUDGET_CHARS: usize = 500;

/// Which producer produced the follow-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowupKind {
    Test,
    Lint,
}

impl FollowupKind {
    fn label(self) -> &'static str {
        match self {
            FollowupKind::Test => "[test]",
            FollowupKind::Lint => "[lint]",
        }
    }
}

/// Structured follow-up payload produced by a `/test` or `/lint` job.
/// Scoped to the originating producer [`JobId`] (serialized as its raw u64).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DeferredFollowup {
    /// Originating producer job id (`JobId.0`).
    pub producer: u64,
    pub kind: FollowupKind,
    /// Bounded LLM prompt, dispatched verbatim after terminalization.
    pub prompt: String,
    /// One-line log emitted exactly once when the follow-up dispatches.
    pub notice: String,
}

/// Encode a follow-up as a UI-channel message.
pub fn defer_message(followup: &DeferredFollowup) -> String {
    format!(
        "{DEFER_FOLLOWUP_PREFIX}{}",
        serde_json::to_string(followup).unwrap_or_default()
    )
}

/// Encode a post-terminal completion signal as a UI-channel message.
pub fn completed_message(id: JobId) -> String {
    format!("{JOB_COMPLETED_PREFIX}{}", id.0)
}

/// True for UI-channel messages owned by the follow-up handoff.
pub fn is_followup_message(line: &str) -> bool {
    line.starts_with(DEFER_FOLLOWUP_PREFIX) || line.starts_with(JOB_COMPLETED_PREFIX)
}

impl TuiExecutor {
    /// Install the manager completion hook forwarding post-terminal signals
    /// to the current UI channel. Called before spawning `/test` and `/lint`
    /// producer jobs. Re-installing with an equivalent sender is harmless.
    pub(crate) fn ensure_completion_hook(&mut self) {
        let Some(tx) = self.ui_tx.clone() else {
            return;
        };
        let hook: Arc<dyn Fn(JobCompletion) + Send + Sync> = Arc::new(move |completion| {
            let _ = tx.send(completed_message(completion.id));
        });
        self.jobs.set_completion_hook(hook);
    }

    /// Route a `::defer_followup:` / `::job_completed:` message. Called from
    /// the slash-command dispatch so all [`JobManager`] access stays inside
    /// [`TuiExecutor`].
    ///
    /// [`JobManager`]: crate::jobs::JobManager
    pub(crate) fn handle_followup_message(&mut self, line: &str, ui: &mut TuiApp) {
        self.ensure_completion_hook();
        if let Some(payload) = line.strip_prefix(DEFER_FOLLOWUP_PREFIX) {
            self.store_deferred_followup(payload);
        } else if let Some(payload) = line.strip_prefix(JOB_COMPLETED_PREFIX) {
            self.drain_followup(payload, ui);
        }
    }

    /// Store an early follow-up without dispatching. Never touches
    /// conversation history or TUI status: the producer still owns the
    /// foreground slot at this point, so any immediate dispatch would be
    /// rejected as `ForegroundBusy` and lost.
    ///
    /// Validation (all rejections are bounded `tracing::warn!`s without the
    /// payload, or silent only for idempotent duplicates): the UI channel
    /// must be available (otherwise the completion signal could never
    /// arrive), the producer id must be valid, and the producer must be a
    /// known Test/Lint foreground job (active or recently terminal).
    /// Forged ids, non-Test/Lint kinds, non-foreground scopes, stale
    /// (evicted/unknown) producers, and empty prompts are dropped. The map
    /// stays bounded at [`MAX_PENDING_FOLLOWUPS`]; first payload wins.
    fn store_deferred_followup(&mut self, payload: &str) {
        let Ok(mut followup) = serde_json::from_str::<DeferredFollowup>(payload) else {
            tracing::warn!("ignoring malformed defer_followup payload");
            return;
        };
        if self.ui_tx.is_none() {
            tracing::warn!(
                producer = followup.producer,
                "dropping defer_followup: UI channel unavailable"
            );
            return;
        }
        if self.jobs.is_shutting_down() {
            // Teardown raced the defer: the completion signal could never
            // produce an AgentTurn, so do not queue what drain must drop.
            tracing::warn!(
                producer = followup.producer,
                "dropping defer_followup: job manager shutting down"
            );
            return;
        }
        if self.consumed_followups.contains(&followup.producer) {
            return;
        }
        if followup.producer == 0 {
            // JobIds start at 1; 0 is always forged.
            tracing::warn!("dropping defer_followup with invalid producer id 0");
            return;
        }
        match self.jobs.get_snapshot(JobId(followup.producer)) {
            Some(snapshot)
                if (snapshot.kind == JobKind::Test || snapshot.kind == JobKind::Lint)
                    && snapshot.scope == JobScope::Foreground => {}
            _ => {
                tracing::warn!(
                    producer = followup.producer,
                    "dropping defer_followup for unknown or ineligible producer"
                );
                return;
            }
        }
        if self.pending_followups.contains_key(&followup.producer) {
            // First payload wins; this also coalesces duplicate sends.
            return;
        }
        if self.pending_followups.len() >= MAX_PENDING_FOLLOWUPS {
            tracing::warn!(
                producer = followup.producer,
                "defer_followup map full; dropping follow-up"
            );
            return;
        }
        if followup.prompt.trim().is_empty() {
            tracing::warn!(
                producer = followup.producer,
                "dropping defer_followup with empty prompt"
            );
            return;
        }
        let prompt = crate::tools::budget::head_tail_truncate(
            &followup.prompt,
            FOLLOWUP_PROMPT_BUDGET_CHARS,
        )
        .text;
        followup.prompt = prompt;
        if followup.notice.chars().count() > FOLLOWUP_NOTICE_BUDGET_CHARS {
            followup.notice = followup
                .notice
                .chars()
                .take(FOLLOWUP_NOTICE_BUDGET_CHARS)
                .collect();
        }
        self.pending_followups.insert(followup.producer, followup);
    }

    /// Dispatch the stored follow-up for `producer` iff that exact job is
    /// terminal `Completed` and no foreground job is active. All other cases
    /// suppress without starting an `AgentTurn` and without touching
    /// conversation history.
    fn drain_followup(&mut self, payload: &str, ui: &mut TuiApp) {
        let Ok(producer_id) = payload.trim().parse::<u64>() else {
            return;
        };
        let Some(followup) = self.pending_followups.remove(&producer_id) else {
            // Stale or duplicate completion for an unknown producer.
            return;
        };
        let producer = JobId(producer_id);
        if self.jobs.is_shutting_down() {
            // Shutdown raced the completion signal: never start an AgentTurn
            // during teardown, and leave history/status untouched.
            self.mark_followup_consumed(producer_id);
            return;
        }
        match self.jobs.get_snapshot(producer) {
            Some(snapshot) if snapshot.status == JobStatus::Completed => {}
            Some(snapshot) if !snapshot.status.is_terminal() => {
                // Completion signal arrived before terminalization (not
                // possible via the manager hook); keep the follow-up pending
                // rather than losing it.
                self.pending_followups.insert(producer_id, followup);
                return;
            }
            Some(snapshot) => {
                self.mark_followup_consumed(producer_id);
                ui.push_log(format!(
                    "{} Follow-up suppressed (job {}).",
                    followup.kind.label(),
                    snapshot.status
                ));
                return;
            }
            None => {
                // Stale producer: no active record and no recent terminal
                // snapshot (evicted or never existed). Suppress and log
                // rather than dispatching against an unknown job.
                self.mark_followup_consumed(producer_id);
                ui.push_log(format!(
                    "{} Follow-up suppressed (producer job not found).",
                    followup.kind.label()
                ));
                return;
            }
        }
        if let Some(active_id) = self.jobs.foreground_id() {
            // Never displace an unrelated foreground job; a late follow-up
            // must not attach to a newer job. Any reservation counts as
            // busy, even if its snapshot is momentarily unavailable: a
            // foreground id with no corresponding snapshot is never treated
            // as a free slot.
            let detail = self
                .jobs
                .get_snapshot(active_id)
                .map(|active| active.id.to_string())
                .unwrap_or_else(|| active_id.to_string());
            self.mark_followup_consumed(producer_id);
            ui.push_log(format!(
                "{} Follow-up suppressed: foreground job {} is already running.",
                followup.kind.label(),
                detail
            ));
            return;
        }
        self.mark_followup_consumed(producer_id);
        if !followup.notice.is_empty() {
            ui.push_log(followup.notice.clone());
        }
        ui.last_user_input = Some(followup.prompt.clone());
        // Same path as a typed prompt (plan enforcement on): the foreground
        // slot is free and the producer is terminal, so the authoritative
        // reservation inside `JobManager::spawn` succeeds deterministically.
        // Called directly instead of `ui.dispatch` because the handler is
        // already borrowed by the current dispatch. `spawn_agent_turn` owns
        // the `> {prompt}` log line, so it is not duplicated here.
        self.handle_dispatch_rest(&followup.prompt, ui, false);
    }

    fn mark_followup_consumed(&mut self, producer_id: u64) {
        if self.consumed_followups.len() >= MAX_CONSUMED_FOLLOWUPS
            && !self.consumed_followups.contains(&producer_id)
            && let Some(oldest) = self.consumed_followups.iter().min().copied()
        {
            // Monotonic producer ids: evicting the smallest keeps the recent
            // window intact. Never `clear()`: that would re-arm stale
            // duplicates for still-recent producers and risk a second
            // AgentTurn for one producer result.
            self.consumed_followups.remove(&oldest);
        }
        self.consumed_followups.insert(producer_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::jobs::{JobKind, JobRunOutcome, JobScope, JobSpec, WorkspaceAccess};
    use crate::tui::commands::core::CommandHandler;
    use crate::tui::state::LogEntry;
    use std::sync::Arc;
    use std::time::Duration;

    fn test_executor() -> (TuiExecutor, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = AppConfig {
            project_root: dir.path().to_path_buf(),
            api_key: Some("test-key".to_string()),
            base_url: "http://127.0.0.1:1".to_string(),
            ..Default::default()
        };
        (TuiExecutor::new(cfg).unwrap(), dir)
    }

    fn log_texts(ui: &TuiApp) -> Vec<String> {
        ui.log
            .iter()
            .map(|entry| match entry {
                LogEntry::Plain(text) | LogEntry::Markdown(text) => text.clone(),
            })
            .collect()
    }

    fn history_len(executor: &TuiExecutor) -> usize {
        executor
            .conversation_history
            .lock()
            .unwrap()
            .build_messages()
            .len()
    }

    async fn wait_for_terminal(executor: &TuiExecutor, id: JobId) -> crate::jobs::JobStatus {
        for _ in 0..200 {
            if let Some(snapshot) = executor.jobs.get_snapshot(id)
                && snapshot.status.is_terminal()
            {
                return snapshot.status;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("timed out waiting for {id} to finish");
    }

    fn defer_for(producer: JobId, kind: FollowupKind) -> String {
        defer_message(&DeferredFollowup {
            producer: producer.0,
            kind,
            prompt: "Please analyze the test failures and provide fixes.".to_string(),
            notice: "[test] Sending failures to LLM...".to_string(),
        })
    }

    #[tokio::test]
    async fn test_early_followup_is_deferred_until_terminal() {
        let (mut executor, _dir) = test_executor();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());

        // Gated producer still owns the foreground slot.
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let started_clone = started.clone();
        let release_clone = release.clone();
        let producer = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::Test,
                    JobScope::Foreground,
                    WorkspaceAccess::ReadOnly,
                    "producer",
                ),
                move |_ctx| async move {
                    started_clone.notify_one();
                    release_clone.notified().await;
                    JobRunOutcome::Completed
                },
            )
            .unwrap();
        started.notified().await;
        assert_eq!(executor.jobs.foreground_id(), Some(producer));

        // Old behavior dispatched immediately and was rejected as
        // ForegroundBusy; the new contract stores without dispatching.
        let before_history = history_len(&executor);
        executor.handle(&defer_for(producer, FollowupKind::Test), &mut ui);
        assert!(executor.pending_followups.contains_key(&producer.0));
        assert_eq!(executor.jobs.foreground_id(), Some(producer));
        assert_eq!(history_len(&executor), before_history);
        assert!(
            !log_texts(&ui)
                .iter()
                .any(|line| line.contains("already running"))
        );

        // Producer terminal + completion signal drains exactly one follow-up.
        release.notify_one();
        assert_eq!(
            wait_for_terminal(&executor, producer).await,
            JobStatus::Completed
        );
        executor.handle(&completed_message(producer), &mut ui);
        assert!(!executor.pending_followups.contains_key(&producer.0));
        assert!(executor.consumed_followups.contains(&producer.0));
        let foreground = executor.jobs.foreground_id().expect("follow-up running");
        assert_ne!(foreground, producer);
        assert_eq!(
            executor.jobs.get_snapshot(foreground).unwrap().kind,
            JobKind::AgentTurn
        );
        let logs = log_texts(&ui);
        assert!(
            logs.iter()
                .any(|line| line.contains("[test] Sending failures to LLM..."))
        );
        assert_eq!(history_len(&executor), before_history);
        assert!(
            !logs.iter().any(|line| line.contains("already running")),
            "follow-up must not race its own producer: {logs:?}"
        );
        executor.jobs.cancel(foreground);
    }

    #[tokio::test]
    async fn test_preterminal_completion_keeps_followup_pending() {
        let (mut executor, _dir) = test_executor();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());

        let release = Arc::new(tokio::sync::Notify::new());
        let release_clone = release.clone();
        let producer = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::Test,
                    JobScope::Foreground,
                    WorkspaceAccess::ReadOnly,
                    "producer",
                ),
                move |_ctx| async move {
                    release_clone.notified().await;
                    JobRunOutcome::Completed
                },
            )
            .unwrap();
        executor.handle(&defer_for(producer, FollowupKind::Test), &mut ui);
        // Forged/early completion while the producer is still active must not
        // consume the follow-up.
        executor.handle(&completed_message(producer), &mut ui);
        assert!(executor.pending_followups.contains_key(&producer.0));
        assert!(!executor.consumed_followups.contains(&producer.0));
        assert_eq!(executor.jobs.foreground_id(), Some(producer));
        release.notify_one();
        executor.jobs.cancel(producer);
    }

    #[tokio::test]
    async fn test_cancellation_suppresses_followup() {
        let (mut executor, _dir) = test_executor();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());

        let producer = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::Test,
                    JobScope::Foreground,
                    WorkspaceAccess::ReadOnly,
                    "producer",
                ),
                |ctx| async move {
                    ctx.cancellation.cancelled().await;
                    JobRunOutcome::Cancelled
                },
            )
            .unwrap();
        let before_history = history_len(&executor);
        executor.handle(&defer_for(producer, FollowupKind::Test), &mut ui);
        executor.jobs.cancel(producer);
        assert_eq!(
            wait_for_terminal(&executor, producer).await,
            JobStatus::Cancelled
        );
        executor.handle(&completed_message(producer), &mut ui);
        assert!(executor.jobs.foreground_id().is_none());
        assert!(!executor.pending_followups.contains_key(&producer.0));
        assert_eq!(history_len(&executor), before_history);
        assert!(
            log_texts(&ui)
                .iter()
                .any(|line| line.contains("Follow-up suppressed"))
        );
    }

    #[tokio::test]
    async fn test_infrastructure_failure_suppresses_followup() {
        let (mut executor, _dir) = test_executor();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());

        let producer = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::Lint,
                    JobScope::Foreground,
                    WorkspaceAccess::Write,
                    "producer",
                ),
                |_ctx| async {
                    JobRunOutcome::Failed {
                        message: "spawn failed".to_string(),
                    }
                },
            )
            .unwrap();
        executor.handle(&defer_for(producer, FollowupKind::Lint), &mut ui);
        assert_eq!(
            wait_for_terminal(&executor, producer).await,
            JobStatus::Failed
        );
        executor.handle(&completed_message(producer), &mut ui);
        assert!(executor.jobs.foreground_id().is_none());
        assert!(
            log_texts(&ui)
                .iter()
                .any(|line| line.contains("Follow-up suppressed"))
        );
    }

    #[tokio::test]
    async fn test_shutdown_suppresses_queued_followup_without_history_changes() {
        let (mut executor, _dir) = test_executor();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());

        let producer = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::Test,
                    JobScope::Foreground,
                    WorkspaceAccess::ReadOnly,
                    "producer",
                ),
                |_ctx| async { JobRunOutcome::Completed },
            )
            .unwrap();
        assert_eq!(
            wait_for_terminal(&executor, producer).await,
            JobStatus::Completed
        );
        executor.handle(&defer_for(producer, FollowupKind::Test), &mut ui);
        assert!(executor.pending_followups.contains_key(&producer.0));

        let before_history = history_len(&executor);
        executor.jobs.shutdown(Duration::from_millis(50)).await;
        assert!(executor.jobs.is_shutting_down());

        executor.handle(&completed_message(producer), &mut ui);
        assert!(!executor.pending_followups.contains_key(&producer.0));
        assert!(executor.consumed_followups.contains(&producer.0));
        assert!(executor.jobs.foreground_id().is_none());
        assert_eq!(history_len(&executor), before_history);
    }

    #[tokio::test]
    async fn test_unrelated_foreground_job_is_not_displaced() {
        let (mut executor, _dir) = test_executor();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());

        // Producer already terminal; its follow-up is pending.
        let producer = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::Test,
                    JobScope::Foreground,
                    WorkspaceAccess::ReadOnly,
                    "producer",
                ),
                |_ctx| async { JobRunOutcome::Completed },
            )
            .unwrap();
        assert_eq!(
            wait_for_terminal(&executor, producer).await,
            JobStatus::Completed
        );
        executor.handle(&defer_for(producer, FollowupKind::Test), &mut ui);

        // An unrelated foreground job starts before the completion drains.
        let other = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::AgentTurn,
                    JobScope::Foreground,
                    WorkspaceAccess::Write,
                    "other",
                ),
                |ctx| async move {
                    tokio::select! {
                        _ = ctx.cancellation.cancelled() => JobRunOutcome::Cancelled,
                        _ = tokio::time::sleep(Duration::from_secs(30)) => JobRunOutcome::Completed,
                    }
                },
            )
            .unwrap();
        let before_history = history_len(&executor);
        executor.handle(&completed_message(producer), &mut ui);
        assert_eq!(executor.jobs.foreground_id(), Some(other));
        assert_eq!(history_len(&executor), before_history);
        assert!(
            log_texts(&ui)
                .iter()
                .any(|line| line.contains("already running"))
        );
        executor.jobs.cancel(other);
    }

    #[tokio::test]
    async fn test_stale_and_duplicate_events_are_ignored() {
        let (mut executor, _dir) = test_executor();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());

        // Completion for an unknown producer: no-op.
        executor.handle(&completed_message(JobId(999_999)), &mut ui);
        assert!(executor.jobs.foreground_id().is_none());

        // Duplicate defers coalesce to one pending entry.
        let producer = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::Lint,
                    JobScope::Foreground,
                    WorkspaceAccess::Write,
                    "producer",
                ),
                |_ctx| async { JobRunOutcome::Completed },
            )
            .unwrap();
        assert_eq!(
            wait_for_terminal(&executor, producer).await,
            JobStatus::Completed
        );
        executor.handle(&defer_for(producer, FollowupKind::Lint), &mut ui);
        executor.handle(&defer_for(producer, FollowupKind::Lint), &mut ui);
        assert_eq!(executor.pending_followups.len(), 1);

        // Malformed payload: ignored.
        executor.handle("::defer_followup:not-json", &mut ui);
        assert_eq!(executor.pending_followups.len(), 1);

        // Drain once, then duplicates are stale.
        executor.handle(&completed_message(producer), &mut ui);
        let foreground = executor.jobs.foreground_id().expect("follow-up running");
        executor.handle(&completed_message(producer), &mut ui);
        assert_eq!(executor.jobs.foreground_id(), Some(foreground));
        executor.handle(&defer_for(producer, FollowupKind::Lint), &mut ui);
        assert!(!executor.pending_followups.contains_key(&producer.0));
        executor.jobs.cancel(foreground);
    }

    #[tokio::test]
    async fn test_consumed_eviction_keeps_recent_window() {
        let (mut executor, _dir) = test_executor();
        for id in 1..=(MAX_CONSUMED_FOLLOWUPS as u64 + 10) {
            executor.mark_followup_consumed(id);
        }
        assert_eq!(executor.consumed_followups.len(), MAX_CONSUMED_FOLLOWUPS);
        // Recently consumed ids survive overflow; only the oldest are evicted.
        assert!(
            executor
                .consumed_followups
                .contains(&(MAX_CONSUMED_FOLLOWUPS as u64 + 10))
        );
        assert!(!executor.consumed_followups.contains(&1));
    }

    #[tokio::test]
    async fn test_full_handoff_starts_one_followup_without_busy() {
        for kind in [FollowupKind::Test, FollowupKind::Lint] {
            let (mut executor, _dir) = test_executor();
            let mut ui = TuiApp::new("test", None, "dark").unwrap();
            // Dedicated channel so the test can pump messages through the
            // same dispatch path the event loop uses.
            let (ui_tx, rx) = std::sync::mpsc::channel::<String>();
            executor.set_ui_tx(Some(ui_tx.clone()));
            executor.ensure_completion_hook();

            let (job_kind, access) = match kind {
                FollowupKind::Test => (JobKind::Test, WorkspaceAccess::ReadOnly),
                FollowupKind::Lint => (JobKind::Lint, WorkspaceAccess::Write),
            };
            let notice = match kind {
                FollowupKind::Test => "[test] Sending failures to LLM...",
                FollowupKind::Lint => "[lint] Sending output to LLM...",
            };
            // Mimic a real producer body: defer while still owning the
            // foreground slot, then return Completed.
            let producer = executor
                .jobs
                .spawn(
                    JobSpec::new(job_kind, JobScope::Foreground, access, "producer"),
                    move |ctx| async move {
                        let followup = DeferredFollowup {
                            producer: ctx.id.0,
                            kind,
                            prompt: "Fix the reported issues.".to_string(),
                            notice: notice.to_string(),
                        };
                        let _ = ui_tx.send(defer_message(&followup));
                        JobRunOutcome::Completed
                    },
                )
                .unwrap();
            let before_history = history_len(&executor);
            assert_eq!(
                wait_for_terminal(&executor, producer).await,
                JobStatus::Completed
            );
            // The hook fires after terminal visibility, so wait (bounded)
            // for its signal before pumping. Both sends happen sequentially
            // on the producer thread, so collected order is send order.
            let mut messages: Vec<String> = Vec::new();
            for _ in 0..200 {
                messages.extend(rx.try_iter());
                if messages.iter().any(|m| m == &completed_message(producer)) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            messages.extend(rx.try_iter());
            assert!(
                messages
                    .iter()
                    .any(|m| m.starts_with(DEFER_FOLLOWUP_PREFIX)),
                "producer must defer: {messages:?}"
            );
            assert!(
                messages.iter().any(|m| m == &completed_message(producer)),
                "hook must signal post-terminal completion: {messages:?}"
            );
            // Pump every channel message through dispatch, in order.
            for msg in &messages {
                executor.handle(msg, &mut ui);
            }
            let foreground = executor.jobs.foreground_id().expect("follow-up must start");
            assert_ne!(foreground, producer);
            assert_eq!(
                executor.jobs.get_snapshot(foreground).unwrap().kind,
                JobKind::AgentTurn
            );
            let logs = log_texts(&ui);
            assert!(
                !logs.iter().any(|line| line.contains("already running")),
                "follow-up must not race its own producer: {logs:?}"
            );
            assert!(logs.iter().any(|line| line.contains(notice)));
            assert_eq!(history_len(&executor), before_history);
            assert!(executor.pending_followups.is_empty());
            executor.jobs.cancel(foreground);
        }
    }

    #[tokio::test]
    async fn test_successful_followup_logs_prompt_exactly_once() {
        // M2: one successful follow-up must emit exactly one `> {prompt}`
        // line. `spawn_agent_turn` owns it; `drain_followup` only logs the
        // short notice and must not duplicate the prompt line.
        let (mut executor, _dir) = test_executor();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());

        let release = Arc::new(tokio::sync::Notify::new());
        let release_clone = release.clone();
        let producer = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::Test,
                    JobScope::Foreground,
                    WorkspaceAccess::ReadOnly,
                    "producer",
                ),
                move |_ctx| async move {
                    release_clone.notified().await;
                    JobRunOutcome::Completed
                },
            )
            .unwrap();
        let prompt = "Please analyze the test failures and provide fixes.".to_string();
        executor.handle(&defer_for(producer, FollowupKind::Test), &mut ui);
        release.notify_one();
        assert_eq!(
            wait_for_terminal(&executor, producer).await,
            JobStatus::Completed
        );
        executor.handle(&completed_message(producer), &mut ui);
        let foreground = executor.jobs.foreground_id().expect("follow-up running");

        let logs = log_texts(&ui);
        let target = format!("> {prompt}");
        assert_eq!(
            logs.iter().filter(|line| *line == &target).count(),
            1,
            "exactly one `> prompt` line expected: {logs:?}"
        );
        executor.jobs.cancel(foreground);
    }

    #[tokio::test]
    async fn test_store_rejects_unknown_and_ineligible_producers() {
        // M3: forged ids, non-Test/Lint kinds, non-foreground scopes, and
        // empty prompts are dropped with a bounded warning instead of being
        // stored. Fully deterministic: validation reads the snapshot
        // synchronously, so no timing is involved.
        fn defer_with(producer: u64, kind: FollowupKind, prompt: &str) -> String {
            defer_message(&DeferredFollowup {
                producer,
                kind,
                prompt: prompt.to_string(),
                notice: String::new(),
            })
        }

        // Unknown producer: no snapshot at all.
        {
            let (mut executor, _dir) = test_executor();
            let mut ui = TuiApp::new("test", None, "dark").unwrap();
            executor.set_ui_tx(ui.sender());
            executor.handle(&defer_with(999_999, FollowupKind::Test, "prompt"), &mut ui);
            assert!(executor.pending_followups.is_empty());
        }

        // Non-Test/Lint producer kind (AgentTurn foreground).
        {
            let (mut executor, _dir) = test_executor();
            let mut ui = TuiApp::new("test", None, "dark").unwrap();
            executor.set_ui_tx(ui.sender());
            let agent = executor
                .jobs
                .spawn(
                    JobSpec::new(
                        JobKind::AgentTurn,
                        JobScope::Foreground,
                        WorkspaceAccess::Write,
                        "agent",
                    ),
                    |_ctx| async { JobRunOutcome::Completed },
                )
                .unwrap();
            executor.handle(&defer_with(agent.0, FollowupKind::Test, "prompt"), &mut ui);
            assert!(
                executor.pending_followups.is_empty(),
                "AgentTurn producer must not arm a follow-up"
            );
            executor.jobs.cancel(agent);
        }

        // Non-foreground scope (background Test job).
        {
            let (mut executor, _dir) = test_executor();
            let mut ui = TuiApp::new("test", None, "dark").unwrap();
            executor.set_ui_tx(ui.sender());
            let bg = executor
                .jobs
                .spawn(
                    JobSpec::new(
                        JobKind::Test,
                        JobScope::Background,
                        WorkspaceAccess::ReadOnly,
                        "bg",
                    ),
                    |_ctx| async { JobRunOutcome::Completed },
                )
                .unwrap();
            executor.handle(&defer_with(bg.0, FollowupKind::Test, "prompt"), &mut ui);
            assert!(
                executor.pending_followups.is_empty(),
                "background producer must not arm a follow-up"
            );
            executor.jobs.cancel(bg);
        }

        // Empty prompt from an otherwise eligible producer.
        {
            let (mut executor, _dir) = test_executor();
            let mut ui = TuiApp::new("test", None, "dark").unwrap();
            executor.set_ui_tx(ui.sender());
            let producer = executor
                .jobs
                .spawn(
                    JobSpec::new(
                        JobKind::Lint,
                        JobScope::Foreground,
                        WorkspaceAccess::Write,
                        "producer",
                    ),
                    |_ctx| async { JobRunOutcome::Completed },
                )
                .unwrap();
            executor.handle(&defer_with(producer.0, FollowupKind::Lint, "   "), &mut ui);
            assert!(
                executor.pending_followups.is_empty(),
                "empty prompt must not be stored"
            );
            executor.jobs.cancel(producer);
        }

        // UI channel unavailable: nothing is stored because the completion
        // signal could never arrive. Calls the follow-up entry point
        // directly so `handle` cannot refill `ui_tx` from the TuiApp.
        {
            let (mut executor, _dir) = test_executor();
            let mut ui = TuiApp::new("test", None, "dark").unwrap();
            assert!(executor.ui_tx.is_none());
            let producer = executor
                .jobs
                .spawn(
                    JobSpec::new(
                        JobKind::Test,
                        JobScope::Foreground,
                        WorkspaceAccess::ReadOnly,
                        "producer",
                    ),
                    |_ctx| async { JobRunOutcome::Completed },
                )
                .unwrap();
            executor.handle_followup_message(
                &defer_with(producer.0, FollowupKind::Test, "prompt"),
                &mut ui,
            );
            assert!(
                executor.pending_followups.is_empty(),
                "must not store when the UI receiver is unavailable"
            );
            executor.jobs.cancel(producer);
        }
    }

    #[tokio::test]
    async fn test_store_rejects_defer_during_shutdown() {
        // M3: a defer arriving after shutdown started is dropped at store
        // time (drain would only drop it later). Deterministic: shutdown
        // waits for the producer task, so no timing is involved.
        let (mut executor, _dir) = test_executor();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());
        let producer = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::Test,
                    JobScope::Foreground,
                    WorkspaceAccess::ReadOnly,
                    "producer",
                ),
                |_ctx| async { JobRunOutcome::Completed },
            )
            .unwrap();
        executor.jobs.shutdown(Duration::from_secs(5)).await;
        assert!(executor.jobs.is_shutting_down());
        // The producer snapshot survives in recent history, so only the
        // shutdown gate can be responsible for the rejection below.
        assert!(executor.jobs.get_snapshot(producer).is_some());
        executor.handle(
            &defer_message(&DeferredFollowup {
                producer: producer.0,
                kind: FollowupKind::Test,
                prompt: "prompt".to_string(),
                notice: String::new(),
            }),
            &mut ui,
        );
        assert!(
            executor.pending_followups.is_empty(),
            "must not store when shutdown started"
        );
    }

    #[tokio::test]
    async fn test_pending_followups_bounded_at_cap() {
        // M3: the pending map never exceeds MAX_PENDING_FOLLOWUPS, even for
        // an otherwise eligible producer. Pre-fills the map directly so the
        // test is deterministic and needs no timing.
        let (mut executor, _dir) = test_executor();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());
        let producer = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::Test,
                    JobScope::Foreground,
                    WorkspaceAccess::ReadOnly,
                    "producer",
                ),
                |_ctx| async { JobRunOutcome::Completed },
            )
            .unwrap();
        for id in 1..=MAX_PENDING_FOLLOWUPS as u64 {
            executor.pending_followups.insert(
                1_000_000 + id,
                DeferredFollowup {
                    producer: 1_000_000 + id,
                    kind: FollowupKind::Test,
                    prompt: "prompt".to_string(),
                    notice: String::new(),
                },
            );
        }
        assert_eq!(executor.pending_followups.len(), MAX_PENDING_FOLLOWUPS);
        executor.handle(&defer_for(producer, FollowupKind::Test), &mut ui);
        assert_eq!(executor.pending_followups.len(), MAX_PENDING_FOLLOWUPS);
        assert!(
            !executor.pending_followups.contains_key(&producer.0),
            "capacity-exceeded payload must be dropped"
        );
        executor.jobs.cancel(producer);
    }

    #[tokio::test]
    async fn test_stale_producer_suppressed_with_log() {
        // M4: a pending follow-up whose producer has no corresponding
        // snapshot (evicted or never existed) is suppressed with a bounded
        // log line instead of dispatching. The stale state is forged by
        // pre-filling the map directly; no timing involved.
        let (mut executor, _dir) = test_executor();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());
        let stale = 999_999u64;
        executor.pending_followups.insert(
            stale,
            DeferredFollowup {
                producer: stale,
                kind: FollowupKind::Test,
                prompt: "prompt".to_string(),
                notice: String::new(),
            },
        );
        executor.handle(&completed_message(JobId(stale)), &mut ui);
        assert!(executor.pending_followups.is_empty());
        assert!(executor.consumed_followups.contains(&stale));
        assert!(executor.jobs.foreground_id().is_none());
        assert!(
            log_texts(&ui)
                .iter()
                .any(|line| line.contains("producer job not found")),
            "stale suppression must be logged: {:?}",
            log_texts(&ui)
        );
    }
}
