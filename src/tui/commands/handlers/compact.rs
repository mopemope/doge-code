use crate::jobs::{JobKind, JobRunOutcome, JobScope, JobSpec, WorkspaceAccess};
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
        let Some(client) = self.client.clone() else {
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
        let spawn = self.jobs.spawn(
            JobSpec::new(
                JobKind::Compact,
                JobScope::Foreground,
                WorkspaceAccess::None,
                "Compact conversation",
            ),
            move |ctx| async move {
                let token = ctx.cancellation_token();
                let result = candidate.compact_manually(token.clone()).await;
                // Cancellation wins over failed/late provider responses, until
                // the synchronous disk-first commit below has succeeded.
                if token.is_cancelled() {
                    return JobRunOutcome::Cancelled;
                }
                let compacted = match result {
                    Ok(compacted) => compacted,
                    Err(error) => {
                        return JobRunOutcome::Failed {
                            message: error.to_string(),
                        };
                    }
                };
                if !compacted {
                    if let Some(tx) = &tx {
                        let _ = tx.send(
                            "[INFO] No safe history prefix to compact; conversation unchanged."
                                .into(),
                        );
                    }
                    return JobRunOutcome::Completed;
                }
                let payload = candidate.persistable();
                jobs.finish_synchronous_commit(ctx.id, || {
                    match commit_candidate(&history, &manager, &original, &session, payload, &token)
                    {
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

/// Both locks remain held across save and adoption: runtime edits cannot sneak
/// between validation and replacement. No awaits or callbacks run under them.
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
