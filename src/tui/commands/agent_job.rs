use crate::jobs::{
    JobId, JobKind, JobRunOutcome, JobScope, JobSpec, JobStartError, WorkspaceAccess,
};
use crate::llm::LlmErrorKind;
use crate::provenance::DirectiveOrigin;
use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;

/// Explicit provenance contract for starting an AgentTurn.
///
/// String equality between display and effective text must never decide
/// whether a turn is user-observed: internal follow-ups flow through the
/// same dispatch path with identical strings. Callers declare intent:
/// - `ObserveUserPrompt` / `ObserveUserCustomCommand`: a real user directive
///   was just typed; record one fresh `DirectiveObserved` with the exact raw
///   bytes (never synthesized text).
/// - `InheritDirective`: replay/continuation of an already observed
///   directive; reuse the id, record nothing new.
/// - `Internal`: synthetic work with no user directive (`none()`).
#[derive(Debug, Clone)]
pub enum AgentTurnProvenance {
    ObserveUserPrompt { raw_input: String },
    ObserveUserCustomCommand { raw_input: String },
    InheritDirective { directive_id: String },
    Internal,
}

impl AgentTurnProvenance {
    fn observed_origin(&self) -> Option<DirectiveOrigin> {
        match self {
            Self::ObserveUserPrompt { .. } => Some(DirectiveOrigin::TuiPrompt),
            Self::ObserveUserCustomCommand { .. } => Some(DirectiveOrigin::TuiCustomCommand),
            Self::InheritDirective { .. } | Self::Internal => None,
        }
    }

    fn observed_raw(&self) -> Option<&str> {
        match self {
            Self::ObserveUserPrompt { raw_input }
            | Self::ObserveUserCustomCommand { raw_input } => Some(raw_input.as_str()),
            Self::InheritDirective { .. } | Self::Internal => None,
        }
    }
}

/// Bounded label for agent turns (user instruction preview).
fn agent_label(content: &str) -> String {
    let flat: String = content.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::jobs::types::bound_label(&flat)
}

fn busy_message(active: &crate::jobs::JobSnapshot) -> String {
    format!(
        "[Job] {} is already running. Use /jobs or /cancel {}.",
        active.id, active.id
    )
}

/// Shared AgentTurn spawn path for plain prompts, `/quick`, and custom
/// commands. A fast foreground pre-check runs first so a busy rejection
/// returns before message building or history updates; the authoritative
/// reservation is the atomic check inside `JobManager::spawn`. All callers
/// run synchronously on the TUI thread, so the pre-check is exact in
/// practice and a busy rejection never pollutes conversation history.
///
/// Provenance is caller-declared via [`AgentTurnProvenance`]: only
/// `ObserveUser*` records a new `DirectiveObserved` (with the exact raw
/// bytes supplied by the caller); `InheritDirective` reuses an existing id
/// and `Internal` runs with `ProvenanceAttribution::none()`.
pub(crate) fn spawn_agent_turn(
    executor: &mut TuiExecutor,
    ui: &mut TuiApp,
    display: &str,
    content: String,
    skip_plan: bool,
    provenance: AgentTurnProvenance,
) -> Result<JobId, JobStartError> {
    // Fast pre-check to avoid UI/history side effects on the common busy
    // path. The authoritative check remains the atomic reservation inside
    // `JobManager::spawn`.
    if let Some(active_id) = executor.jobs.foreground_id()
        && let Some(active) = executor.jobs.get_snapshot(active_id)
    {
        ui.push_log(busy_message(&active));
        return Err(JobStartError::ForegroundBusy { active });
    }

    let client = match executor.client.clone() {
        Some(client) => client,
        None => {
            ui.push_log("OPENAI_API_KEY not set; cannot call LLM.");
            return Err(JobStartError::ShuttingDown);
        }
    };

    // Ensure a session exists (plain prompts already do this; custom
    // commands historically did not).
    {
        let mut session_manager = executor.session_manager.lock().unwrap();
        if session_manager.current_session.is_none()
            && let Err(e) = session_manager.create_session(Some(content.clone()))
        {
            ui.push_log(format!(
                "[ERROR] Failed to create session automatically: {e}"
            ));
            return Err(JobStartError::ShuttingDown);
        }
    }

    let model = executor.cfg.model.clone();
    let cfg = executor.cfg.clone();
    let fs = executor.tools.clone();
    let conversation_history = executor.conversation_history.clone();
    let session_manager = executor.session_manager.clone();
    let ui_tx = executor.ui_tx.clone();

    // Build messages (system + history + shell context + plan + user).
    let mut msgs = Vec::new();
    let sys_prompt = crate::tui::commands::prompt::build_system_prompt(&executor.cfg);
    msgs.push(crate::llm::ChatMessage {
        provider_state: None,
        role: "system".into(),
        content: Some(sys_prompt),
        tool_calls: vec![],
        tool_call_id: None,
    });
    if let Ok(history) = executor.conversation_history.lock() {
        msgs.extend(history.build_messages());
    }
    if !ui.shell_output_buffer.is_empty() {
        let lines: Vec<&str> = ui.shell_output_buffer.lines().rev().take(50).collect();
        let context = lines.into_iter().rev().collect::<Vec<_>>().join("\n");
        msgs.push(crate::llm::ChatMessage {
            provider_state: None,
            role: "system".into(),
            content: Some(format!(
                "Recent shell output (last 50 lines):\n```\n{context}\n```"
            )),
            tool_calls: vec![],
            tool_call_id: None,
        });
    }
    if !skip_plan {
        executor.enforce_plan_context(&mut msgs, &content, Some(ui));
    }
    msgs.push(crate::llm::ChatMessage {
        provider_state: None,
        role: "user".into(),
        content: Some(content.clone()),
        tool_calls: vec![],
        tool_call_id: None,
    });

    let spec = JobSpec::new(
        JobKind::AgentTurn,
        JobScope::Foreground,
        WorkspaceAccess::Write,
        agent_label(&content),
    );

    let content_for_job = content.clone();
    let provenance_for_job = provenance.clone();
    // Sequence pairing for the async `::directive_observed:<seq>:<id>`
    // delivery. Pre-computed (not yet committed): only an accepted spawn
    // commits it to `ui` below, and a rejected spawn never sends, so each
    // delivered id unambiguously pairs with its own turn's raw input.
    let observed_seq = if provenance.observed_raw().is_some() {
        ui.last_observed_seq + 1
    } else {
        0
    };
    let spawn_result = executor.jobs.spawn(spec, move |ctx| async move {
        let token = ctx.cancellation_token();
        if let Some(tx) = &ui_tx {
            let _ = tx.send("::status:sending".into());
        }
        {
            let mut sm = session_manager.lock().unwrap();
            if let Err(e) = sm.update_current_session_with_request_count() {
                tracing::error!(?e, "Failed to update session with request count");
            }
        }
        // Record the observed directive after the job started but before any
        // LLM call. Only an explicitly observed user directive records a new
        // event, using the caller-supplied raw bytes (never synthesized text)
        // as `raw_input` and the turn content as `effective_instruction`.
        // Inherited turns reuse the existing id; internal turns run with
        // `none()`. A recording failure never aborts the turn: mark
        // provenance_incomplete and continue with directive_id = None.
        let attribution = match &provenance_for_job {
            AgentTurnProvenance::ObserveUserPrompt { raw_input }
            | AgentTurnProvenance::ObserveUserCustomCommand { raw_input } => {
                let origin = provenance_for_job
                    .observed_origin()
                    .expect("observed variant has an origin");
                match crate::tools::provenance::record_directive_observed(
                    &fs,
                    origin,
                    raw_input,
                    &content_for_job,
                ) {
                    Ok(env) => {
                        if let Some(tx) = &ui_tx {
                            let _ = tx.send(format!(
                                "::directive_observed:{observed_seq}:{}",
                                env.event_id
                            ));
                        }
                        crate::provenance::ProvenanceAttribution::with_directive(env.event_id)
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "provenance.directive_record_failed");
                        let _ = fs.mark_current_session_provenance_failure();
                        crate::provenance::ProvenanceAttribution::none()
                    }
                }
            }
            AgentTurnProvenance::InheritDirective { directive_id } => {
                crate::provenance::ProvenanceAttribution::with_directive(directive_id.clone())
            }
            AgentTurnProvenance::Internal => crate::provenance::ProvenanceAttribution::none(),
        };
        let res = crate::llm::run_agent_loop(
            &client,
            &model,
            &fs,
            msgs,
            ui_tx.clone(),
            Some(token),
            &cfg,
            None,
            attribution,
        )
        .await;
        let tokens_used = client.get_prompt_tokens_used();
        let total_tokens = client.get_total_tokens_used();
        match res {
            Ok((updated_messages, _final_msg)) => {
                if let Some(tx) = ui_tx.clone() {
                    let _ = tx.send(format!(
                        "::tokens:prompt:{tokens_used},total:{total_tokens}"
                    ));
                    let remaining = cfg
                        .get_context_window_size()
                        .map(|window| window.saturating_sub(tokens_used));
                    let _ = tx.send(match remaining {
                        Some(n) => format!("::update_remaining_tokens:{n}"),
                        None => "::update_remaining_tokens".to_string(),
                    });
                }
                if let Ok(mut history) = conversation_history.lock() {
                    // Canonical result: the agent loop's returned history is
                    // the new truth (compaction may have shortened/reordered
                    // it), projected to durable messages. Never a
                    // count/index-based delta.
                    let durable = crate::llm::durable_conversation_messages(updated_messages);
                    history.replace(durable);
                    let mut sm = session_manager.lock().unwrap();
                    let msgs_vec = history.build_messages();
                    if let Err(e) = sm.update_current_session_with_history(&msgs_vec) {
                        tracing::error!(?e, "Failed to update session with conversation history");
                    }
                    if let Err(e) = sm.update_current_session_with_token_count(total_tokens) {
                        tracing::error!(?e, "Failed to update session with token count");
                    }
                }
                JobRunOutcome::Completed
            }
            Err(e) => {
                let cancelled = matches!(
                    e.downcast_ref::<LlmErrorKind>(),
                    Some(LlmErrorKind::Cancelled)
                );
                if cancelled {
                    if let Some(tx) = ui_tx.clone() {
                        let _ = tx.send("::status:cancelled".into());
                        let _ = tx.send(format!(
                            "::tokens:prompt:{tokens_used},total:{total_tokens}"
                        ));
                    }
                    if let Ok(mut history) = conversation_history.lock() {
                        let mut sm = session_manager.lock().unwrap();
                        {
                            if let Some(session) = &sm.current_session {
                                match session.conversation_messages() {
                                    Ok(messages) => history.overwrite_messages(crate::llm::durable_conversation_messages(messages)),
                                    Err(error) => tracing::error!(%error, "could not restore canonical checkpoint"),
                                }
                            }
                        }
                        let msgs_vec = history.build_messages();
                        if let Err(e) = sm.update_current_session_with_history(&msgs_vec) {
                            tracing::error!(
                                ?e,
                                "Failed to update session with conversation history on cancel"
                            );
                        }
                        if let Err(e) = sm.update_current_session_with_token_count(total_tokens) {
                            tracing::error!(
                                ?e,
                                "Failed to update session with token count on cancel"
                            );
                        }
                    }
                    JobRunOutcome::Cancelled
                } else {
                    if let Some(tx) = ui_tx.clone() {
                        let _ = tx.send(format!("LLM error: {e}"));
                        let _ = tx.send("::status:error".into());
                        let _ = tx.send(format!(
                            "::tokens:prompt:{tokens_used},total:{total_tokens}"
                        ));
                    }
                    if let Ok(mut history) = conversation_history.lock() {
                        let mut sm = session_manager.lock().unwrap();
                        {
                            if let Some(session) = &sm.current_session {
                                match session.conversation_messages() {
                                    Ok(messages) => history.overwrite_messages(crate::llm::durable_conversation_messages(messages)),
                                    Err(error) => tracing::error!(%error, "could not restore canonical checkpoint"),
                                }
                            }
                        }
                        let msgs_vec = history.build_messages();
                        if let Err(e) = sm.update_current_session_with_history(&msgs_vec) {
                            tracing::error!(
                                ?e,
                                "Failed to update session with conversation history on error"
                            );
                        }
                        if let Err(e) = sm.update_current_session_with_token_count(total_tokens) {
                            tracing::error!(
                                ?e,
                                "Failed to update session with token count on error"
                            );
                        }
                    }
                    JobRunOutcome::Failed {
                        message: crate::jobs::types::bound_error(&e.to_string()),
                    }
                }
            }
        }
    });

    match spawn_result {
        Ok(id) => {
            // Only freshly observed user directives become the retry source.
            // Internal follow-ups and inherited replays must never overwrite
            // the last real user input (or they would corrupt compact retry
            // and `/edit-symbol` latest-instruction lookup).
            if let Some(raw) = provenance.observed_raw() {
                executor.last_user_prompt = Some(content.clone());
                ui.last_user_input = Some(raw.to_string());
                ui.last_observed_raw_input = Some(raw.to_string());
                ui.last_observed_effective_input = Some(content.clone());
                // The directive id arrives asynchronously via
                // `::directive_observed:<seq>:<id>`; a stale id from a
                // previous turn must not be mistaken for this turn's id.
                ui.last_observed_seq = observed_seq;
                ui.last_observed_directive_id = None;
            }
            ui.push_log(format!("> {display}"));
            ui.push_log(String::new());
            ui.processing_start_time = Some(std::time::Instant::now());
            ui.last_elapsed_time = None;
            ui.dirty = true;
            if let Some(tx) = &executor.ui_tx {
                let _ = tx.send("::status:preparing".into());
            }
            Ok(id)
        }
        Err(JobStartError::ForegroundBusy { active }) => {
            ui.push_log(busy_message(&active));
            Err(JobStartError::ForegroundBusy { active })
        }
        Err(JobStartError::ShuttingDown) => {
            // Avoid double-logging the missing-API-key path handled above.
            if executor.client.is_some() {
                ui.push_log("Job manager is shutting down.");
            }
            Err(JobStartError::ShuttingDown)
        }
    }
}

/// Post-terminal Test/Lint follow-up spawn path (deferred handoff).
///
/// Unlike [`spawn_agent_turn`], this records no fresh `DirectiveObserved`:
/// the follow-up is internal agent work attributed to no user directive
/// (`ProvenanceAttribution::none()`), so it has no independent
/// requirement-writing authority. It also leaves `last_user_prompt`
/// untouched so compact-retry keeps replaying the real user input.
///
/// Parity with the historical analysis dispatch: follow-ups run with
/// plan context enforced (`skip_plan = false`), exactly like a default
/// user dispatch. This differs from [`TuiExecutor::spawn_internal_followup`]
/// (which skips plan context) used by the immediate synthetic path.
pub(crate) fn spawn_synthetic_followup(
    executor: &mut TuiExecutor,
    ui: &mut TuiApp,
    display: &str,
    content: String,
) -> Result<JobId, JobStartError> {
    spawn_agent_turn(
        executor,
        ui,
        display,
        content,
        false,
        AgentTurnProvenance::Internal,
    )
}

impl TuiExecutor {
    /// Synthetic follow-up (test/lint analysis) with no user directive.
    /// Never records `DirectiveObserved`; runs with `none()` attribution so
    /// it cannot authorize `requirements_write`.
    pub(crate) fn spawn_internal_followup(
        &mut self,
        ui: &mut TuiApp,
        display: &str,
        content: String,
    ) -> Result<JobId, JobStartError> {
        spawn_agent_turn(
            self,
            ui,
            display,
            content,
            true,
            AgentTurnProvenance::Internal,
        )
    }

    /// Replay of an already observed user turn. Reuses the original
    /// directive id when known (no duplicate event); falls back to `none()`
    /// rather than fabricating a fresh observation.
    pub(crate) fn spawn_retry_turn(
        &mut self,
        ui: &mut TuiApp,
        display: &str,
        content: String,
        directive_id: Option<String>,
    ) -> Result<JobId, JobStartError> {
        let provenance = match directive_id {
            Some(id) => AgentTurnProvenance::InheritDirective { directive_id: id },
            None => AgentTurnProvenance::Internal,
        };
        spawn_agent_turn(self, ui, display, content, false, provenance)
    }

    /// Real user prompt augmented with a system note (diff rejection).
    /// `raw` is the exact typed bytes for `raw_input`; `effective` (note +
    /// raw) is what the agent sees. Synthesized text never enters raw input.
    pub(crate) fn spawn_augmented_user_prompt(
        &mut self,
        ui: &mut TuiApp,
        raw: &str,
        effective: String,
        skip_plan: bool,
    ) -> Result<JobId, JobStartError> {
        spawn_agent_turn(
            self,
            ui,
            raw,
            effective,
            skip_plan,
            AgentTurnProvenance::ObserveUserPrompt {
                raw_input: raw.to_string(),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::tui::state::LogEntry;

    fn log_contains(ui: &TuiApp, needle: &str) -> bool {
        ui.log.iter().any(|entry| match entry {
            LogEntry::Plain(text) | LogEntry::Markdown(text) => text.contains(needle),
        })
    }

    fn history_len(executor: &TuiExecutor) -> usize {
        executor
            .conversation_history
            .lock()
            .unwrap()
            .build_messages()
            .len()
    }

    fn test_executor_with_client() -> (TuiExecutor, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = AppConfig {
            project_root: dir.path().to_path_buf(),
            api_key: Some("test-key".to_string()),
            base_url: "http://127.0.0.1:1".to_string(),
            ..Default::default()
        };
        let executor = TuiExecutor::new(cfg).unwrap();
        // TuiExecutor::new leaks nothing; return dir alongside to keep it alive.
        // SAFETY: tests only – project_root is cloned into cfg.
        (executor, dir)
    }

    #[tokio::test]
    async fn test_busy_agent_turn_leaves_history_untouched() {
        let (mut executor, _dir) = test_executor_with_client();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());
        // Occupy the foreground slot with a blocking job.
        let blocker = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::AgentTurn,
                    JobScope::Foreground,
                    WorkspaceAccess::Write,
                    "blocker",
                ),
                |ctx| async move {
                    tokio::select! {
                        _ = ctx.cancellation.cancelled() => JobRunOutcome::Cancelled,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => JobRunOutcome::Completed,
                    }
                },
            )
            .unwrap();
        let before = history_len(&executor);
        let result = spawn_agent_turn(
            &mut executor,
            &mut ui,
            "second prompt",
            "second prompt".to_string(),
            true,
            AgentTurnProvenance::ObserveUserPrompt {
                raw_input: "second prompt".to_string(),
            },
        );
        assert!(matches!(result, Err(JobStartError::ForegroundBusy { .. })));
        assert_eq!(history_len(&executor), before);
        assert!(log_contains(&ui, "already running"));
        // No preparing status on busy path.
        executor.jobs.cancel(blocker);
    }

    #[tokio::test]
    async fn test_agent_turn_registers_foreground_job() {
        let (mut executor, _dir) = test_executor_with_client();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());
        // Point at an unreachable endpoint so the turn fails fast without
        // network access; reservation behavior is what matters here.
        let result = spawn_agent_turn(
            &mut executor,
            &mut ui,
            "hello",
            "hello".to_string(),
            true,
            AgentTurnProvenance::ObserveUserPrompt {
                raw_input: "hello".to_string(),
            },
        );
        // Either Ok (job running) or ShuttingDown; never a second foreground.
        if let Ok(id) = result {
            assert_eq!(executor.jobs.foreground_id(), Some(id));
            executor.jobs.cancel(id);
        }
    }

    #[tokio::test]
    async fn test_busy_rejection_records_no_directive() {
        let (mut executor, _dir) = test_executor_with_client();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());
        let blocker = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::AgentTurn,
                    JobScope::Foreground,
                    WorkspaceAccess::Write,
                    "blocker",
                ),
                |ctx| async move {
                    tokio::select! {
                        _ = ctx.cancellation.cancelled() => JobRunOutcome::Cancelled,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => JobRunOutcome::Completed,
                    }
                },
            )
            .unwrap();
        let result = spawn_agent_turn(
            &mut executor,
            &mut ui,
            "second prompt",
            "second prompt".to_string(),
            true,
            AgentTurnProvenance::ObserveUserPrompt {
                raw_input: "second prompt".to_string(),
            },
        );
        assert!(matches!(result, Err(JobStartError::ForegroundBusy { .. })));
        // Busy rejection must not record a directive.
        let events = crate::tools::provenance::load_current_events(&executor.tools)
            .unwrap()
            .map(|l| l.events)
            .unwrap_or_default();
        assert!(
            events.iter().all(|e| !matches!(
                e.event,
                crate::provenance::ProvenanceEvent::DirectiveObserved(_)
            )),
            "busy rejection must not create DirectiveObserved"
        );
        executor.jobs.cancel(blocker);
    }

    #[tokio::test]
    async fn test_missing_api_key_records_no_directive() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = AppConfig {
            project_root: dir.path().to_path_buf(),
            api_key: None,
            base_url: "http://127.0.0.1:1".to_string(),
            ..Default::default()
        };
        let mut executor = TuiExecutor::new(cfg).unwrap();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());
        let result = spawn_agent_turn(
            &mut executor,
            &mut ui,
            "hello",
            "hello".to_string(),
            true,
            AgentTurnProvenance::ObserveUserPrompt {
                raw_input: "hello".to_string(),
            },
        );
        assert!(matches!(result, Err(JobStartError::ShuttingDown)));
        assert!(log_contains(&ui, "OPENAI_API_KEY"));
        // No agent start means no directive.
        let events = crate::tools::provenance::load_current_events(&executor.tools)
            .unwrap()
            .map(|l| l.events)
            .unwrap_or_default();
        assert!(events.iter().all(|e| !matches!(
            e.event,
            crate::provenance::ProvenanceEvent::DirectiveObserved(_)
        )),);
    }

    #[tokio::test]
    async fn test_directive_origin_prompt_vs_custom() {
        // Provenance is caller-declared, never inferred from string
        // equality: identical strings can be internal, differing strings can
        // still be a plain prompt with augmentation.
        let plain = AgentTurnProvenance::ObserveUserPrompt {
            raw_input: "hello".to_string(),
        };
        assert!(plain.observed_raw().is_some());
        assert_eq!(
            plain.observed_origin(),
            Some(crate::provenance::DirectiveOrigin::TuiPrompt)
        );
        assert_eq!(plain.observed_raw(), Some("hello"));
        let custom = AgentTurnProvenance::ObserveUserCustomCommand {
            raw_input: "/fix-cache arg".to_string(),
        };
        assert!(custom.observed_raw().is_some());
        assert_eq!(
            custom.observed_origin(),
            Some(crate::provenance::DirectiveOrigin::TuiCustomCommand)
        );
        let internal = AgentTurnProvenance::Internal;
        assert_eq!(internal.observed_raw(), None);
        assert_eq!(internal.observed_origin(), None);
        let inherit = AgentTurnProvenance::InheritDirective {
            directive_id: "d1".to_string(),
        };
        assert_eq!(inherit.observed_raw(), None);
        assert_eq!(inherit.observed_origin(), None);
    }

    fn directive_events(executor: &TuiExecutor) -> Vec<crate::provenance::ProvenanceEventEnvelope> {
        crate::tools::provenance::load_current_events(&executor.tools)
            .unwrap()
            .map(|l| l.events)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| {
                matches!(
                    e.event,
                    crate::provenance::ProvenanceEvent::DirectiveObserved(_)
                )
            })
            .collect()
    }

    async fn wait_for_directives(
        executor: &TuiExecutor,
        expected: usize,
    ) -> Vec<crate::provenance::ProvenanceEventEnvelope> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let events = directive_events(executor);
            if events.len() >= expected || std::time::Instant::now() > deadline {
                return events;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn test_plain_prompt_records_exactly_one_tui_prompt() {
        let (mut executor, _dir) = test_executor_with_client();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());
        let result = spawn_agent_turn(
            &mut executor,
            &mut ui,
            "fix the cache",
            "fix the cache".to_string(),
            true,
            AgentTurnProvenance::ObserveUserPrompt {
                raw_input: "fix the cache".to_string(),
            },
        );
        let id = result.expect("turn must spawn");
        let events = wait_for_directives(&executor, 1).await;
        assert_eq!(events.len(), 1, "exactly one directive per user prompt");
        match &events[0].event {
            crate::provenance::ProvenanceEvent::DirectiveObserved(d) => {
                assert_eq!(d.origin, crate::provenance::DirectiveOrigin::TuiPrompt);
                assert_eq!(d.raw_input, "fix the cache");
                assert_eq!(d.effective_instruction, "fix the cache");
            }
            _ => panic!("expected directive"),
        }
        executor.jobs.cancel(id);
    }

    #[tokio::test]
    async fn test_custom_command_records_typed_raw_and_expanded_effective() {
        let (mut executor, _dir) = test_executor_with_client();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());
        let result = spawn_agent_turn(
            &mut executor,
            &mut ui,
            "/fix-cache arg",
            "Expanded: fix the cache with arg".to_string(),
            false,
            AgentTurnProvenance::ObserveUserCustomCommand {
                raw_input: "/fix-cache arg".to_string(),
            },
        );
        let id = result.expect("turn must spawn");
        let events = wait_for_directives(&executor, 1).await;
        assert_eq!(events.len(), 1);
        match &events[0].event {
            crate::provenance::ProvenanceEvent::DirectiveObserved(d) => {
                assert_eq!(
                    d.origin,
                    crate::provenance::DirectiveOrigin::TuiCustomCommand
                );
                assert_eq!(d.raw_input, "/fix-cache arg");
                assert_eq!(d.effective_instruction, "Expanded: fix the cache with arg");
            }
            _ => panic!("expected directive"),
        }
        // Retry tracking keeps raw typed bytes separate from the expansion.
        assert_eq!(ui.last_user_input.as_deref(), Some("/fix-cache arg"));
        assert_eq!(
            ui.last_observed_effective_input.as_deref(),
            Some("Expanded: fix the cache with arg")
        );
        executor.jobs.cancel(id);
    }

    #[tokio::test]
    async fn test_internal_followup_records_no_directive_and_has_no_authority() {
        let (mut executor, _dir) = test_executor_with_client();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());
        // Synthetic test/lint-style prompt: identical display/content must
        // NOT become a TuiPrompt observation.
        let synthetic = "Please analyze and fix the following lint issues in the codebase:\n\nIssue 1: unused variable";
        let result = executor.spawn_internal_followup(&mut ui, synthetic, synthetic.to_string());
        let id = result.expect("internal turn must spawn");
        // Give the job time to (incorrectly) record; then assert nothing did.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        assert!(
            directive_events(&executor).is_empty(),
            "synthetic follow-up must not create DirectiveObserved"
        );
        // Internal attribution authorizes nothing.
        let args = crate::tools::requirements::RequirementsWriteArgs {
            upserts: vec![crate::tools::requirements::RequirementInput {
                id: "r1".to_string(),
                statement: "do x".to_string(),
            }],
            withdraw_ids: vec![],
        };
        assert!(
            crate::tools::requirements::requirements_write(
                &executor.tools,
                args,
                &crate::provenance::ProvenanceAttribution::none()
            )
            .is_err(),
            "internal turn must not authorize requirements_write"
        );
        executor.jobs.cancel(id);
    }

    #[tokio::test]
    async fn test_retry_replay_reuses_directive_without_duplicate() {
        let (mut executor, _dir) = test_executor_with_client();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());
        // Observe the original user turn.
        let first = spawn_agent_turn(
            &mut executor,
            &mut ui,
            "fix the cache",
            "fix the cache".to_string(),
            true,
            AgentTurnProvenance::ObserveUserPrompt {
                raw_input: "fix the cache".to_string(),
            },
        )
        .expect("first turn must spawn");
        let events = wait_for_directives(&executor, 1).await;
        assert_eq!(events.len(), 1);
        let original_id = events[0].event_id.clone();
        executor.jobs.cancel(first);
        // Wait for the foreground slot to clear before replaying.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while executor.jobs.foreground_id().is_some() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        // Replay inherits the original id: no second event.
        let retry = executor.spawn_retry_turn(
            &mut ui,
            "fix the cache",
            "fix the cache".to_string(),
            Some(original_id.clone()),
        );
        let retry_id = retry.expect("retry must spawn");
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let events = directive_events(&executor);
        assert_eq!(
            events.len(),
            1,
            "retry must not duplicate DirectiveObserved"
        );
        assert_eq!(events[0].event_id, original_id);
        // Inherited attribution keeps requirement authority on the original.
        let args = crate::tools::requirements::RequirementsWriteArgs {
            upserts: vec![crate::tools::requirements::RequirementInput {
                id: "r1".to_string(),
                statement: "do x".to_string(),
            }],
            withdraw_ids: vec![],
        };
        assert!(
            crate::tools::requirements::requirements_write(
                &executor.tools,
                args,
                &crate::provenance::ProvenanceAttribution::with_directive(original_id)
            )
            .is_ok()
        );
        executor.jobs.cancel(retry_id);
    }

    #[tokio::test]
    async fn test_augmented_prompt_keeps_system_note_out_of_raw_input() {
        let (mut executor, _dir) = test_executor_with_client();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());
        let raw = "continue with the fix";
        let effective = format!(
            "[SYSTEM NOTE] The user rejected the previous changes and the affected files were reverted to their pre-change state. Take this into account when proceeding.\n\n{raw}"
        );
        let result = executor.spawn_augmented_user_prompt(&mut ui, raw, effective.clone(), true);
        let id = result.expect("turn must spawn");
        let events = wait_for_directives(&executor, 1).await;
        assert_eq!(events.len(), 1);
        match &events[0].event {
            crate::provenance::ProvenanceEvent::DirectiveObserved(d) => {
                assert_eq!(d.raw_input, raw);
                assert!(!d.raw_input.contains("SYSTEM NOTE"));
                assert_eq!(d.effective_instruction, effective);
            }
            _ => panic!("expected directive"),
        }
        executor.jobs.cancel(id);
    }

    #[tokio::test]
    async fn test_observed_turn_tracks_retry_source_but_internal_does_not() {
        let (mut executor, _dir) = test_executor_with_client();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());
        let id = spawn_agent_turn(
            &mut executor,
            &mut ui,
            "real instruction",
            "real instruction".to_string(),
            true,
            AgentTurnProvenance::ObserveUserPrompt {
                raw_input: "real instruction".to_string(),
            },
        )
        .expect("turn must spawn");
        assert_eq!(ui.last_user_input.as_deref(), Some("real instruction"));
        assert_eq!(
            ui.last_observed_raw_input.as_deref(),
            Some("real instruction")
        );
        // The observed turn tracks its effective instruction for retry
        // replay and bumps the directive sequence for id pairing.
        assert_eq!(
            ui.last_observed_effective_input.as_deref(),
            Some("real instruction")
        );
        assert_eq!(ui.last_observed_seq, 1);
        assert_eq!(ui.last_observed_directive_id, None);
        executor.jobs.cancel(id);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while executor.jobs.foreground_id().is_some() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        // Internal follow-up must not clobber the retry source.
        let inner = executor.spawn_internal_followup(&mut ui, "synthetic", "synthetic".to_string());
        let inner_id = inner.expect("internal must spawn");
        assert_eq!(ui.last_user_input.as_deref(), Some("real instruction"));
        assert_eq!(
            ui.last_observed_effective_input.as_deref(),
            Some("real instruction")
        );
        assert_eq!(ui.last_observed_seq, 1);
        executor.jobs.cancel(inner_id);
    }
}
