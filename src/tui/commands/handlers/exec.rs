use crate::planning::plan_status::PlanStatus;
use crate::tui::view::TuiApp;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::tui::commands::core::TuiExecutor;

impl TuiExecutor {
    pub fn handle_dispatch_rest(&mut self, line: &str, ui: &mut TuiApp) {
        if let Some(rest) = line.strip_prefix("/open ") {
            let path_arg = rest.trim();
            if path_arg.is_empty() {
                ui.push_log("usage: /open <path>");
                return;
            }
            // Resolve to absolute path; allow project-internal paths and absolute paths
            let p = std::path::Path::new(path_arg);
            let abs = if p.is_absolute() {
                p.to_path_buf()
            } else {
                self.cfg.project_root.join(p)
            };
            if !abs.exists() {
                ui.push_log(format!("not found: {}", abs.display()));
                return;
            }
            // Leave TUI alt screen temporarily while spawning editor in blocking mode
            use crossterm::{cursor, execute, terminal};
            let mut stdout = std::io::stdout();
            let _ = execute!(stdout, terminal::LeaveAlternateScreen, cursor::Show);
            let _ = terminal::disable_raw_mode();

            // Choose editor from $EDITOR, then $VISUAL, else fallback list
            let editor = std::env::var("EDITOR")
                .ok()
                .or_else(|| std::env::var("VISUAL").ok())
                .unwrap_or_else(|| "vi".to_string());
            let status = std::process::Command::new(&editor).arg(&abs).status();

            // Re-enter TUI
            let _ = terminal::enable_raw_mode();
            let _ = execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide);

            match status {
                Ok(s) if s.success() => ui.push_log(format!("opened: {}", abs.display())),
                Ok(s) => ui.push_log(format!("editor exited with status {s}")),
                Err(e) => ui.push_log(format!("failed to launch editor: {e}")),
            }
            return;
        }

        if let Some(rest) = line.strip_prefix("/session ") {
            match self.handle_session_command(rest.trim(), ui) {
                Ok(_) => {} // No-op on success
                Err(e) => ui.push_log(format!("Error handling session command: {}", e)),
            }
            return;
        }

        // Handle /tokens command to show token usage
        if line == "/tokens" {
            if let Some(client) = &self.client {
                // Show prompt tokens cumulative (header should display prompt token total)
                let tokens_used = client.get_prompt_tokens_used();
                ui.push_log(format!("Total prompt tokens used: {}", tokens_used));
            } else {
                ui.push_log("No LLM client available.");
            }
            return;
        }

        // Handle /plan command for task analysis and planning
        if let Some(rest) = line.strip_prefix("/plan ") {
            let task_description = rest.trim();
            self.handle_plan_command(task_description, ui);
            return;
        }

        // Handle /execute command for plan execution
        if let Some(rest) = line.strip_prefix("/execute") {
            let mut plan_id = "".to_string();
            let mut background = false;

            let parts = rest.split_whitespace();
            for part in parts {
                if part == "--background" || part == "-b" {
                    background = true;
                } else {
                    plan_id = part.to_string();
                }
            }

            ui.push_log(format!(
                "> /execute {} {}",
                plan_id,
                if background { "--background" } else { "" }
            ));
            self.handle_execute_command(&plan_id, background, ui);
            return;
        }

        // Handle /plans command to list plans
        if line == "/plans" {
            ui.push_log("> /plans");
            self.handle_plans_command(ui);
            return;
        }

        // Handle /compact command to summarize conversation history
        if line == "/compact" {
            ui.push_log("> /compact");
            self.handle_compact_command(ui);
            return;
        }

        if !line.starts_with('/') {
            {
                let mut sm = self.session_manager.lock().unwrap();
                if sm.current_session.is_none() {
                    match sm.create_session() {
                        Ok(()) => {
                            // if let Some(info) = sm.current_session_info() {
                            //     ui.push_log(format!(
                            //         "[INFO] No active session. New session created.\n{}",
                            //         info
                            //     ));
                            // }
                        }
                        Err(e) => {
                            ui.push_log(format!(
                                "[ERROR] Failed to create session automatically: {}",
                                e
                            ));
                            return;
                        }
                    }
                }
            }
            let rest = line;
            self.last_user_prompt = Some(rest.to_string());
            ui.push_log(format!("> {rest}"));
            let plan_to_execute = {
                if let Ok(plan_manager) = self.plan_manager.lock() {
                    plan_manager.find_executable_plan(rest)
                } else {
                    None
                }
            };

            if let Some(plan_execution) = plan_to_execute {
                ui.push_log(format!(
                    "[TARGET] Executable plan detected: {}",
                    plan_execution.plan.original_request
                ));
                ui.push_log(format!("[ID] Plan ID: {}", plan_execution.plan.id));

                // Start plan execution
                let plan_id = plan_execution.plan.id.clone();
                self.execute_plan_async(&plan_id, false, ui);
                return;
            }

            match self.client.as_ref() {
                Some(c) => {
                    let rt = tokio::runtime::Handle::current();
                    let model = self.cfg.model.clone();
                    let content = rest.to_string();
                    let c = c.clone();
                    let tx = self.ui_tx.clone();
                    // Prepare a fresh line for the final output
                    ui.push_log(String::new());
                    let (cancel_tx, mut cancel_rx) = watch::channel(false);
                    self.cancel_tx = Some(cancel_tx);

                    let cancel_token = CancellationToken::new();
                    let child_token = cancel_token.clone();

                    // Bridge from watch::Receiver to CancellationToken
                    tokio::spawn(async move {
                        if cancel_rx.changed().await.is_ok() && *cancel_rx.borrow() {
                            info!("Cancellation signal received, cancelling token.");
                            child_token.cancel();
                        }
                    });

                    // Notify that LLM request preparation has started
                    if let Some(tx) = &self.ui_tx {
                        let _ = tx.send("::status:preparing".into());
                    }

                    // Build initial messages with optional system prompt + user
                    let mut msgs = Vec::new();
                    // Load system prompt
                    let sys_prompt = crate::tui::commands::prompt::build_system_prompt(&self.cfg);
                    msgs.push(crate::llm::ChatMessage {
                        role: "system".into(),
                        content: Some(sys_prompt),
                        tool_calls: vec![],
                        tool_call_id: None,
                    });

                    // Add existing conversation history
                    if let Ok(history) = self.conversation_history.lock() {
                        msgs.extend(history.clone());
                    }

                    msgs.push(crate::llm::ChatMessage {
                        role: "user".into(),
                        content: Some(content.clone()),
                        tool_calls: vec![],
                        tool_call_id: None,
                    });
                    let fs = self.tools.clone();
                    let conversation_history = self.conversation_history.clone();
                    let session_manager = self.session_manager.clone();
                    let cfg = self.cfg.clone();
                    rt.spawn(async move {
                        // Notify that request sending has started
                        if let Some(tx) = &tx {
                            let _ = tx.send("::status:sending".into());
                        }

                        // Increment request count in session
                        {
                            let mut sm = session_manager.lock().unwrap();
                            if let Err(e) = sm.update_current_session_with_request_count() {
                                tracing::error!(?e, "Failed to update session with request count");
                            }
                        }

                        let res = crate::llm::run_agent_loop(
                            &c,
                            &model,
                            &fs,
                            msgs,
                            tx.clone(),
                            Some(cancel_token),
                            Some(session_manager.clone()),
                            &cfg,
                        )
                        .await;
                        // Get token usage after the agent loop completes
                        let tokens_used = c.get_prompt_tokens_used();
                        let total_tokens = c.get_tokens_used();
                        match res {
                            Ok((updated_messages, _final_msg)) => {
                                if let Some(tx) = tx {
                                    // run_agent_loop already sends the final assistant content as a
                                    // "::status:done:<content>" message. Avoid duplicating it here.
                                    // Only send token usage update (prompt + total)
                                    let _ = tx.send(format!(
                                        "::tokens:prompt:{},total:{}",
                                        tokens_used,
                                        total_tokens
                                    ));
                                }
                                // Update conversation history (save all messages except system messages)
                                if let Ok(mut history) = conversation_history.lock() {
                                    // Extract new messages that are not system messages
                                    let new_messages: Vec<_> = updated_messages
                                        .into_iter()
                                        .filter(|msg| msg.role != "system")
                                        .collect();

                                    // Clear existing history and replace with new messages
                                    history.clear();
                                    history.extend(new_messages);

                                    // Also save conversation history to session
                                    let mut sm = session_manager.lock().unwrap();
                                    if let Err(e) = sm.update_current_session_with_history(&history) {
                                        tracing::error!(?e, "Failed to update session with conversation history");
                                    }

                                    // Update token count in session
                                    if let Err(e) = sm.update_current_session_with_token_count(total_tokens as u64) {
                                        tracing::error!(?e, "Failed to update session with token count");
                                    }
                                }
                            }
                            Err(e) => {
                                if let Some(tx) = tx {
                                    let _ = tx.send(format!("LLM error: {e}"));
                                    let _ = tx.send("::status:error".into());
                                    // Send token usage update even on error
                                    let _ = tx.send(format!(
                                        "::tokens:prompt:{},total:{}",
                                        tokens_used,
                                        total_tokens
                                    ));
                                }
                                // Update conversation history on error (only user input)
                                if let Ok(mut history) = conversation_history.lock() {
                                    history.push(crate::llm::ChatMessage {
                                        role: "user".into(),
                                        content: Some(content.clone()),
                                        tool_calls: vec![],
                                        tool_call_id: None,
                                    });

                                    // Also save conversation history to session
                                    let mut sm = session_manager.lock().unwrap();
                                    if let Err(e) = sm.update_current_session_with_history(&history) {
                                        tracing::error!(?e, "Failed to update session with conversation history on error");
                                    }

                                    // Update token count in session even on error
                                    if let Err(e) = sm.update_current_session_with_token_count(total_tokens as u64) {
                                        tracing::error!(?e, "Failed to update session with token count on error");
                                    }
                                }
                            }
                        }
                    });
                }
                None => ui.push_log("OPENAI_API_KEY not set; cannot call LLM."),
            }
        } else {
            // Check if it's a custom command
            if line.starts_with('/') {
                self.handle_custom_command(line, ui);
            } else {
                ui.push_log(format!("> {line}"));
            }
        }
    }

    /// Handle /execute command
    fn handle_execute_command(&mut self, plan_id: &str, background: bool, ui: &mut TuiApp) {
        if plan_id.is_empty() {
            // Execute the latest plan
            let latest_plan = {
                if let Ok(plan_manager) = self.plan_manager.lock() {
                    plan_manager.get_latest_plan()
                } else {
                    None
                }
            };

            if let Some(plan_execution) = latest_plan {
                let plan_id = plan_execution.plan.id.clone();
                ui.push_log(format!(
                    "[TARGET] Executing latest plan: {}",
                    plan_execution.plan.original_request
                ));
                self.execute_plan_async(&plan_id, background, ui);
            } else {
                ui.push_log(
                    "[ERROR] No executable plan found. Please analyze a task with /plan first.",
                );
            }
        } else {
            // Execute the specified plan
            let plan_exists = {
                if let Ok(plan_manager) = self.plan_manager.lock() {
                    plan_manager.get_plan(plan_id).is_some()
                } else {
                    false
                }
            };

            if plan_exists {
                ui.push_log(format!("[TARGET] Executing plan: {}", plan_id));
                self.execute_plan_async(plan_id, background, ui);
            } else {
                ui.push_log(format!("[ERROR] Plan ID '{}' not found.", plan_id));
            }
        }
    }

    /// Handle /plans command
    fn handle_plans_command(&mut self, ui: &mut TuiApp) {
        if let Ok(plan_manager) = self.plan_manager.lock() {
            let active_plans = plan_manager.list_active_plans();
            let recent_plans = plan_manager.get_recent_plans();
            let stats = plan_manager.get_statistics();

            ui.push_log("[STATS] Plan statistics:");
            ui.push_log(format!("   Total plans: {}", stats.total_plans));
            ui.push_log(format!("   Active: {}", stats.active_plans));
            ui.push_log(format!("   Completed: {}", stats.completed_plans));
            ui.push_log(format!("   Failed: {}", stats.failed_plans));
            ui.push_log(format!("   Cancelled: {}", stats.cancelled_plans));
            if stats.average_completion_time > 0.0 {
                ui.push_log(format!(
                    "   Average completion time: {:.1}s",
                    stats.average_completion_time
                ));
            }

            if !active_plans.is_empty() {
                ui.push_log("\n[ACTIVE] Active plans:");
                for plan_execution in &active_plans {
                    let status_icon = match plan_execution.status {
                        PlanStatus::Created => "[CREATED]",
                        PlanStatus::Running => "[RUNNING]",
                        PlanStatus::Paused => "[PAUSED]",
                        _ => "[UNKNOWN]",
                    };
                    ui.push_log(format!(
                        "   {} {} - {} ({} steps)",
                        status_icon,
                        &plan_execution.plan.id[..8],
                        plan_execution.plan.original_request,
                        plan_execution.plan.steps.len()
                    ));
                }
            }

            if !recent_plans.is_empty() {
                ui.push_log("\n[HISTORY] Recent plan history:");
                for plan_execution in recent_plans.iter().rev().take(5) {
                    let status_icon = match plan_execution.status {
                        PlanStatus::Completed => "[COMPLETED]",
                        PlanStatus::Failed => "[FAILED]",
                        PlanStatus::Cancelled => "[CANCELLED]",
                        _ => "[UNKNOWN]",
                    };
                    ui.push_log(format!(
                        "   {} {} - {}",
                        status_icon,
                        &plan_execution.plan.id[..8],
                        plan_execution.plan.original_request
                    ));
                }
            }

            if active_plans.is_empty() && recent_plans.is_empty() {
                ui.push_log("[INFO] No plans available. Create a new plan with /plan <task>.");
            }
        } else {
            ui.push_log("[ERROR] Cannot access plan management system.");
        }
    }

    /// Execute plan asynchronously
    fn execute_plan_async(&mut self, plan_id: &str, background: bool, ui: &mut TuiApp) {
        if self.client.is_none() {
            ui.push_log("[ERROR] LLM client is not configured.");
            return;
        }

        let plan_execution = {
            if let Ok(plan_manager) = self.plan_manager.lock() {
                plan_manager.get_plan(plan_id)
            } else {
                ui.push_log("[ERROR] Cannot access plan management system.");
                return;
            }
        };

        let Some(plan_execution) = plan_execution else {
            ui.push_log(format!("[ERROR] Plan ID '{}' not found.", plan_id));
            return;
        };

        // Start execution
        if let Ok(plan_manager) = self.plan_manager.lock()
            && let Err(e) = plan_manager.start_execution(plan_id)
        {
            ui.push_log(format!("[ERROR] Failed to start plan execution: {}", e));
            return;
        }

        if background {
            ui.push_log("[INFO] Starting plan execution in background...".to_string());
        } else {
            ui.push_log("[INFO] Starting plan execution...".to_string());
        }

        let rt = tokio::runtime::Handle::current();
        let client = self.client.as_ref().unwrap().clone();
        let model = self.cfg.model.clone();
        let fs_tools = self.tools.clone();
        let ui_tx = self.ui_tx.clone();
        let plan_manager = self.plan_manager.clone();
        let plan_id = plan_id.to_string();
        let plan = plan_execution.plan;
        let cfg = self.cfg.clone();

        rt.spawn(async move {
            let executor =
                crate::planning::step_executor::TaskExecutor::new(client, model, fs_tools, cfg);

            match executor.execute_plan(plan, background, ui_tx.clone()).await {
                Ok(result) => {
                    if let Ok(pm) = plan_manager.lock() {
                        let _ = pm.complete_execution(&plan_id, result.clone());
                    }

                    if let Some(tx) = ui_tx {
                        if result.success {
                            let _ = tx.send(
                                format!("[SUCCESS] Plan {} completed successfully!", &plan_id[..8])
                                    .to_string(),
                            );
                        } else {
                            let _ = tx.send(format!(
                                "[WARNING] Plan {} partially completed: {}",
                                &plan_id[..8],
                                result.final_message
                            ));
                        }
                        // Always send summary to UI regardless of background mode
                        let _ = tx.send(format!(
                            "[SUMMARY] Plan {}: {}/{} steps confirmed. Total time: {}s.",
                            &plan_id[..8],
                            result.completed_steps.len(),
                            result.completed_steps.len(), // This seems to be the same number, might need to check logic
                            result.total_duration
                        ));
                    }
                }
                Err(e) => {
                    if let Ok(pm) = plan_manager.lock() {
                        let _ = pm.cancel_execution(&plan_id);
                    }

                    if let Some(tx) = ui_tx {
                        let _ = tx.send(format!("[ERROR] Plan {} failed: {}", &plan_id[..8], e));
                    }
                }
            }
        });
    }
}
