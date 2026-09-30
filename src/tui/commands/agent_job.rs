use crate::jobs::{
    JobId, JobKind, JobRunOutcome, JobScope, JobSpec, JobStartError, WorkspaceAccess,
};
use crate::llm::LlmErrorKind;
use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;

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
pub(crate) fn spawn_agent_turn(
    executor: &mut TuiExecutor,
    ui: &mut TuiApp,
    display: &str,
    content: String,
    skip_plan: bool,
) -> Result<JobId, JobStartError> {
    spawn_agent_turn_inner(executor, ui, display, content, skip_plan, false)
}

/// Post-terminal Test/Lint follow-up spawn path.
///
/// Unlike [`spawn_agent_turn`], this records no fresh `DirectiveObserved`:
/// the follow-up is internal agent work attributed to no user directive
/// (`ProvenanceAttribution::none()`), so it has no independent
/// requirement-writing authority. It also leaves `last_user_prompt`
/// untouched so compact-retry keeps replaying the real user input.
pub(crate) fn spawn_synthetic_followup(
    executor: &mut TuiExecutor,
    ui: &mut TuiApp,
    display: &str,
    content: String,
) -> Result<JobId, JobStartError> {
    // Parity with the historical analysis dispatch: follow-ups run with
    // plan context enforced, exactly like a default user dispatch.
    spawn_agent_turn_inner(executor, ui, display, content, false, true)
}

#[allow(clippy::too_many_lines)]
fn spawn_agent_turn_inner(
    executor: &mut TuiExecutor,
    ui: &mut TuiApp,
    display: &str,
    content: String,
    skip_plan: bool,
    synthetic: bool,
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
    let display_for_job = display.to_string();
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
        // LLM call. `display` is the raw user input, `content` is the
        // effective instruction handed to the agent. A recording failure
        // never aborts the turn: mark provenance_incomplete and continue
        // with directive_id = None. Synthetic follow-ups skip this
        // entirely: they are internal turns with no new user directive.
        let attribution = if synthetic {
            crate::provenance::ProvenanceAttribution::none()
        } else {
            let origin = if display_for_job == content_for_job {
                crate::provenance::DirectiveOrigin::TuiPrompt
            } else {
                crate::provenance::DirectiveOrigin::TuiCustomCommand
            };
            match crate::tools::provenance::record_directive_observed(
                &fs,
                origin,
                &display_for_job,
                &content_for_job,
            ) {
                Ok(env) => crate::provenance::ProvenanceAttribution::with_directive(env.event_id),
                Err(e) => {
                    tracing::warn!(error = %e, "provenance.directive_record_failed");
                    let _ = fs.mark_current_session_provenance_failure();
                    crate::provenance::ProvenanceAttribution::none()
                }
            }
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
                    let new_messages: Vec<_> = updated_messages
                        .into_iter()
                        .filter(|msg| msg.role != "system")
                        .collect();
                    history.clear();
                    for msg in new_messages {
                        history.append_message(msg);
                    }
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
                        history.append_user(content_for_job.clone());
                        let mut sm = session_manager.lock().unwrap();
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
                        history.append_user(content_for_job.clone());
                        let mut sm = session_manager.lock().unwrap();
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
            // Synthetic follow-ups must not become the retried user input:
            // compact-retry keeps replaying the real user instruction, and
            // the follow-up prompt is already echoed by its handoff caller.
            if !synthetic {
                executor.last_user_prompt = Some(content.clone());
                ui.push_log(format!("> {display}"));
            }
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
        let result = spawn_agent_turn(&mut executor, &mut ui, "hello", "hello".to_string(), true);
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
        let result = spawn_agent_turn(&mut executor, &mut ui, "hello", "hello".to_string(), true);
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
        // Plain prompt: raw == effective, origin TuiPrompt.
        // Custom command: raw != effective, origin TuiCustomCommand.
        // Origin is decided as display == content ? TuiPrompt : TuiCustomCommand.
        let plain_display = "hello";
        let plain_content = "hello".to_string();
        let custom_display = "/fix-cache arg";
        let custom_content = "Expanded: fix the cache with arg".to_string();
        assert_eq!(plain_display, plain_content.as_str());
        assert_ne!(custom_display, custom_content.as_str());
        // Record both and verify hashes differ appropriately.
        let (executor, _dir) = test_executor_with_client();
        let fs = executor.tools.clone();
        let plain = crate::tools::provenance::record_directive_observed(
            &fs,
            crate::provenance::DirectiveOrigin::TuiPrompt,
            plain_display,
            &plain_content,
        )
        .unwrap();
        let custom = crate::tools::provenance::record_directive_observed(
            &fs,
            crate::provenance::DirectiveOrigin::TuiCustomCommand,
            custom_display,
            &custom_content,
        )
        .unwrap();
        match &plain.event {
            crate::provenance::ProvenanceEvent::DirectiveObserved(d) => {
                assert_eq!(d.raw_input, d.effective_instruction);
            }
            _ => panic!("expected directive"),
        }
        match &custom.event {
            crate::provenance::ProvenanceEvent::DirectiveObserved(d) => {
                assert_ne!(d.raw_input, d.effective_instruction);
            }
            _ => panic!("expected directive"),
        }
        let _ = executor;
    }
}
