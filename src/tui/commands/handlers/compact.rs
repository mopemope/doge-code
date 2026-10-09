use crate::jobs::{JobKind, JobRunOutcome, JobScope, JobSpec, WorkspaceAccess};
use crate::llm::usage_attribution::SessionUsageCheckpoint;
use crate::tui::commands::core::TuiExecutor;
use crate::tui::state::Status;
use crate::tui::view::TuiApp;

impl TuiExecutor {
    /// Reserve the foreground through JobManager before changing UI or starting
    /// a provider request. The detached candidate never checkpoints itself.
    pub fn handle_compact_command(&mut self, ui: &mut TuiApp) {
        if let Err(error) = self.ensure_session_idle() {
            ui.push_log(format!("[ERROR] Cannot compact: {error}"));
            return;
        }
        let client = match self.conversation_client() {
            Ok(client) => client,
            Err(error) => {
                ui.push_log(format!("[ERROR] Cannot bind compaction session: {error}"));
                return;
            }
        };
        let Some(client) = client else {
            ui.push_log("[ERROR] LLM client is not configured. Cannot compact conversation.");
            return;
        };
        // ChatGPT Responses uses automatic server-side native compaction;
        // the local Chat Completions text summarizer never runs here and no
        // provider request is issued.
        if client.native_responses_compaction_enabled() {
            ui.push_log("[INFO] ChatGPT Responses uses automatic native compaction; /compact does not run the local text summarizer for this provider.");
            return;
        }
        let snapshot = (|| -> anyhow::Result<_> {
            // Match the existing agent checkpoint lock order.
            let history =
                crate::utils::safe_std_lock(&self.conversation_history, "conversation_history")?;
            let manager = crate::utils::safe_std_lock(&self.session_manager, "session_manager")?;
            let session = manager
                .current_session
                .clone()
                .ok_or_else(|| anyhow::anyhow!("no current session for compaction"))?;
            Ok((history.snapshot(), session))
        })();
        let (original, session) = match snapshot {
            Ok(snapshot) => snapshot,
            Err(error) => {
                ui.push_log(format!("[ERROR] Cannot compact: {error}"));
                return;
            }
        };
        let durable = crate::llm::durable_conversation_messages(original.clone());
        if durable.is_empty() {
            ui.push_log("[INFO] No conversation history to compact.");
            return;
        }
        if self.ui_tx.is_none() {
            self.set_ui_tx(ui.sender());
        }
        // The attribution clone shares the usage ledger with `client`, so
        // snapshots before/after the summarizer observe the same provider
        // activity as the candidate.
        let usage_client = client.clone();
        let mut candidate = crate::llm::tool_execution::history::HistoryManager::with_observations(
            client,
            durable,
            None,
            self.tools.clone(),
            self.cfg.clone(),
            session.observations.clone(),
            session.unseen_tool_results.clone(),
        )
        .without_checkpoints();
        let history = self.conversation_history.clone();
        let manager = self.session_manager.clone();
        let tx = self.ui_tx.clone();
        let jobs = self.jobs.clone();
        let _ = session;
        let spawn = self.jobs.spawn(
            JobSpec::new(
                JobKind::Compact,
                JobScope::Foreground,
                WorkspaceAccess::None,
                "Compact conversation",
            ),
            move |ctx| async move {
                let token = ctx.cancellation_token();
                // Capture the usage checkpoint immediately before the local
                // text summarizer starts: expected session id + shared-client
                // ledger snapshot.
                let checkpoint = (|| -> anyhow::Result<SessionUsageCheckpoint> {
                    let mgr =
                        crate::utils::safe_std_lock(&manager, "session_manager")?;
                    let id = mgr
                        .current_session_id()
                        .ok_or_else(|| anyhow::anyhow!("no current session for compaction"))?;
                    Ok(SessionUsageCheckpoint::new(
                        id,
                        usage_client.usage_snapshot(),
                    ))
                })();
                let checkpoint = match checkpoint {
                    Ok(checkpoint) => checkpoint,
                    Err(error) => {
                        return JobRunOutcome::Failed {
                            message: error.to_string(),
                        };
                    }
                };
                let result = candidate.compact_manually(token.clone()).await;
                let usage_after = usage_client.usage_snapshot();
                // Cancellation wins over failed/late provider responses.
                // Usage for an already-sent attempt is still persisted below;
                // only the conversation is left unchanged here.
                if token.is_cancelled() {
                    if let Err(error) = attribute_outside_commit(&manager, checkpoint, &usage_after)
                    {
                        return JobRunOutcome::Failed {
                            message: error.to_string(),
                        };
                    }
                    return JobRunOutcome::Cancelled;
                }
                let compacted = match result {
                    Ok(compacted) => compacted,
                    Err(error) => {
                        if let Err(attrib_error) =
                            attribute_outside_commit(&manager, checkpoint, &usage_after)
                        {
                            return JobRunOutcome::Failed {
                                message: attrib_error.to_string(),
                            };
                        }
                        return JobRunOutcome::Failed {
                            message: error.to_string(),
                        };
                    }
                };
                if !compacted {
                    // No safe prefix: no provider request, empty delta skips save.
                    if let Err(error) = attribute_outside_commit(&manager, checkpoint, &usage_after)
                    {
                        return JobRunOutcome::Failed {
                            message: error.to_string(),
                        };
                    }
                    if let Some(tx) = &tx {
                        let _ = tx.send(
                            "[INFO] No safe history prefix to compact; conversation unchanged."
                                .into(),
                        );
                    }
                    return JobRunOutcome::Completed;
                }
                // Success path: attribute usage and commit history together
                // inside the synchronous section so shutdown cannot
                // terminalize an in-progress disk-first commit as cancelled.
                // Usage save failure never adopts the summary; history save
                // failure keeps the persisted usage and the pre-compaction
                // conversation (no rollback of consumed usage).
                let payload = candidate.persistable();
                jobs.finish_synchronous_commit(ctx.id, || {
                    match commit_usage_and_candidate(
                        &history,
                        &manager,
                        &original,
                        checkpoint,
                        &usage_after,
                        payload,
                        &token,
                    ) {
                        Ok(None) => JobRunOutcome::Cancelled,
                        Err(error) => JobRunOutcome::Failed {
                            message: error.to_string(),
                        },
                        Ok(Some(outcome)) => {
                            if let Some(tx) = &tx {
                                let notice = match outcome {
                                    crate::session::store::SessionSaveOutcome::Durable =>
                                        "[SUCCESS] Conversation history has been compacted and saved.".to_string(),
                                    crate::session::store::SessionSaveOutcome::DurabilityUnconfirmed { message } =>
                                        format!("[WARN] Compacted history was applied to disk and memory, but crash durability is unconfirmed: {message}"),
                                };
                                let _ = tx.send(notice);
                            }
                            // Cancellation after successful commit cannot undo it.
                            JobRunOutcome::Completed
                        }
                    }
                })
            },
        );
        match spawn {
            Ok(id) => {
                ui.push_log(format!(
                    "[Command] Compacting conversation history ({id})..."
                ));
                ui.status = Status::Thinking;
            }
            Err(error) => ui.push_log(format!("[ERROR] Cannot compact: {error}")),
        }
    }

    /// Completion is sent by JobManager only after foreground release. A late
    /// notice for this job must never reset a newer foreground job to Ready.
    pub(crate) fn handle_compact_completed(&self, producer: &str, ui: &mut TuiApp) -> bool {
        let Some(id) = crate::jobs::JobId::parse_arg(producer) else {
            return false;
        };
        let Some(job) = self.jobs.get_snapshot(id) else {
            return false;
        };
        if job.kind != JobKind::Compact || !job.status.is_terminal() {
            return false;
        }
        if let Some(error) = job.error {
            ui.push_log(format!("[ERROR] Compaction failed: {error}"));
        } else if job.status == crate::jobs::JobStatus::Cancelled {
            ui.push_log(format!(
                "[INFO] Compaction cancelled ({id}); conversation unchanged."
            ));
        }
        if self.jobs.foreground_id().is_none() {
            ui.status = if job.status == crate::jobs::JobStatus::Failed {
                Status::Error
            } else {
                Status::Ready
            };
            ui.detailed_status = None;
            if let Some(started) = ui.processing_start_time.take() {
                ui.last_elapsed_time = Some(crate::jobs::types::format_elapsed(
                    started.elapsed().as_millis(),
                ));
            }
        }
        ui.dirty = true;
        true
    }
}

/// Attribute usage outside the synchronous commit (failure/cancel/no-op
/// paths). Empty deltas skip the save. Failures leave the complete payload
/// Unsaved for retry without reapplying.
fn attribute_outside_commit(
    manager: &std::sync::Mutex<crate::session::SessionManager>,
    checkpoint: SessionUsageCheckpoint,
    after: &crate::llm::usage_ledger::UsageLedger,
) -> anyhow::Result<()> {
    let mut guard = crate::utils::safe_std_lock(manager, "session_manager")?;
    checkpoint.finish(after, &mut guard).map(|_| ())
}

/// Success path: usage attribution + disk-first history commit.
///
/// Both saves run inside `finish_synchronous_commit` (holding the job-state
/// lock) so shutdown cannot report a saved commit as cancelled. Usage is
/// attributed first; the history commit then validates against the post-usage
/// session snapshot so the usage update is never misclassified as a
/// concurrent mutation.
#[allow(clippy::too_many_arguments)]
fn commit_usage_and_candidate(
    history: &std::sync::Mutex<crate::llm::ChatHistory>,
    manager: &std::sync::Mutex<crate::session::SessionManager>,
    original: &[crate::llm::ChatMessage],
    checkpoint: SessionUsageCheckpoint,
    after: &crate::llm::usage_ledger::UsageLedger,
    payload: (
        Vec<crate::llm::ChatMessage>,
        crate::llm::observation::ObservationStore,
        std::collections::BTreeSet<String>,
    ),
    token: &tokio_util::sync::CancellationToken,
) -> anyhow::Result<Option<crate::session::store::SessionSaveOutcome>> {
    let mut history = crate::utils::safe_std_lock(history, "conversation_history")?;
    let mut manager = crate::utils::safe_std_lock(manager, "session_manager")?;
    // Attribute first so a discarded summary still persists the attempt.
    // Wrong-session refuses without mutating; save failure leaves Unsaved
    // payload and never adopts the summary.
    // Test-only: the shutdown-barrier fixture blocks the disk-first history
    // commit; the usage save must not consume its rendezvous. Take the
    // barrier for the usage save and restore it for the history commit so
    // the original two-wait contract holds.
    #[cfg(test)]
    let saved_barrier = manager.store.before_sync_barrier.take();
    let attrib_result = checkpoint.finish(after, &mut manager);
    #[cfg(test)]
    {
        manager.store.before_sync_barrier = saved_barrier;
    }
    attrib_result.map(|_| ())?;
    if token.is_cancelled() {
        return Ok(None);
    }
    anyhow::ensure!(
        serde_json::to_value(history.snapshot())? == serde_json::to_value(original)?,
        "conversation changed during compaction; summary discarded"
    );
    let post_usage = manager
        .current_session
        .clone()
        .ok_or_else(|| anyhow::anyhow!("no current session after usage attribution"))?;
    let (messages, observations, unseen) = payload;
    let outcome = manager.commit_compacted_history(&post_usage, &messages, observations, unseen)?;
    history.replace(messages);
    Ok(Some(outcome))
}

/// Both locks remain held across save and adoption: runtime edits cannot sneak
/// between validation and replacement. No awaits or callbacks run under them.
///
/// `expected` must be the post-usage session snapshot: usage attribution runs
/// before this commit, so comparing against the pre-request snapshot would
/// misclassify the usage update as a concurrent mutation. History equality
/// still guards the runtime conversation.
///
/// Only exercised by unit tests since the live path uses
/// [`commit_usage_and_candidate`].
#[cfg(test)]
fn commit_candidate(
    history: &std::sync::Mutex<crate::llm::ChatHistory>,
    manager: &std::sync::Mutex<crate::session::SessionManager>,
    original: &[crate::llm::ChatMessage],
    session: &crate::session::SessionData,
    payload: (
        Vec<crate::llm::ChatMessage>,
        crate::llm::observation::ObservationStore,
        std::collections::BTreeSet<String>,
    ),
    token: &tokio_util::sync::CancellationToken,
) -> anyhow::Result<Option<crate::session::store::SessionSaveOutcome>> {
    let mut history = crate::utils::safe_std_lock(history, "conversation_history")?;
    let mut manager = crate::utils::safe_std_lock(manager, "session_manager")?;
    if token.is_cancelled() {
        return Ok(None);
    }
    anyhow::ensure!(
        serde_json::to_value(history.snapshot())? == serde_json::to_value(original)?,
        "conversation changed during compaction; summary discarded"
    );
    let (messages, observations, unseen) = payload;
    let outcome = manager.commit_compacted_history(session, &messages, observations, unseen)?;
    history.replace(messages);
    Ok(Some(outcome))
}

#[cfg(test)]
#[path = "compact_tests.rs"]
mod tests;
