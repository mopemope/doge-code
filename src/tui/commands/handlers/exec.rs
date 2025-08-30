use crate::planning::create_task_plan;
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
            if task_description.is_empty() {
                ui.push_log("Usage: /plan <task description>");
                return;
            }

            ui.push_log(format!("> /plan {}", task_description));

            // Use tokio runtime for asynchronous processing
            let task_analyzer = &self.task_analyzer;
            let task_desc = task_description.to_string();

            // Analysis is performed synchronously
            match task_analyzer.analyze(&task_desc) {
                Ok(classification) => {
                    ui.push_log("[ANALYSIS] Analyzing task...");
                    ui.push_log(format!(
                        "[PLANNING] Task classification: {:?}",
                        classification.task_type
                    ));
                    ui.push_log(format!(
                        "[TARGET] Complexity: {:.1}/1.0",
                        classification.complexity_score
                    ));
                    ui.push_log(format!(
                        "[STATS] Estimated steps: {}",
                        classification.estimated_steps
                    ));
                    ui.push_log(format!(
                        "[WARNING] Risk level: {:?}",
                        classification.risk_level
                    ));
                    ui.push_log(format!(
                        "[TOOLS] Required tools: {}",
                        classification.required_tools.join(", ")
                    ));
                    ui.push_log(format!(
                        "[CONFIRMED] Confidence: {:.1}%",
                        classification.confidence * 100.0
                    ));

                    // Decomposition is performed asynchronously in a separate thread
                    let rt = tokio::runtime::Handle::current();
                    let task_analyzer_clone = task_analyzer.clone();
                    let ui_tx = self.ui_tx.clone();
                    let plan_manager = self.plan_manager.clone();

                    rt.spawn(async move {
                        match task_analyzer_clone.decompose(&classification, &task_desc).await {
                            Ok(steps) => {
                                if let Some(tx) = ui_tx {
                                    let _ = tx.send(format!("[PLAN] Execution plan ({} steps):", steps.len()));

                                    for (i, step) in steps.iter().enumerate() {
                                        let step_icon = match step.step_type {
                                            crate::planning::StepType::Analysis => "[ANALYSIS]",
                                            crate::planning::StepType::Planning => "[PLANNING]",
                                            crate::planning::StepType::Implementation => "[IMPLEMENTATION]",
                                            crate::planning::StepType::Validation => "[VALIDATION]",
                                            crate::planning::StepType::Cleanup => "[CLEANUP]",
                                        };

                                        let _ = tx.send(format!(
                                            "  {}. {} {} ({}s)",
                                            i + 1,
                                            step_icon,
                                            step.description,
                                            step.estimated_duration
                                        ));

                                        if !step.dependencies.is_empty() {
                                            let _ = tx.send(format!("     Dependencies: {}", step.dependencies.join(", ")));
                                        }

                                        if !step.required_tools.is_empty() {
                                            let _ = tx.send(format!("     Tools: {}", step.required_tools.join(", ")));
                                        }
                                    }

                                    let plan = create_task_plan(
                                        task_desc,
                                        classification,
                                        steps,
                                    );

                                    // Register the plan
                                    if let Ok(plan_manager) = plan_manager.lock() {
                                        match plan_manager.register_plan(plan.clone()) {
                                            Ok(plan_id) => {
                                                let _ = tx.send(format!("[TIMER] Total estimated time: {}s", plan.total_estimated_duration));
                                                let _ = tx.send(format!("[ID] Plan ID: {}", plan_id));
                                                let _ = tx.send("[INFO] How to execute:".to_string());
                                                let _ = tx.send("   /execute        - Execute the latest plan".to_string());
                                                let _ = tx.send(format!("   /execute {}  - Execute this plan", plan_id));
                                                let _ = tx.send("   Or give instructions like 'execute this plan'".to_string());

                                                info!("Generated and registered plan with ID: {}", plan_id);
                                            }
                                            Err(e) => {
                                                let _ = tx.send(format!("[ERROR] Failed to register plan: {}", e));
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                if let Some(tx) = ui_tx {
                                    let _ = tx.send(format!("[ERROR] Failed to decompose steps: {}", e));
                                }
                            }
                        }
                    });
                }
                Err(e) => {
                    ui.push_log(format!("[ERROR] Failed to analyze task: {}", e));
                }
            }
            return;
        }

        // Handle /execute command for plan execution
        if let Some(rest) = line.strip_prefix("/execute") {
            let plan_id = rest.trim();
            ui.push_log(format!("> /execute {}", plan_id));
            self.handle_execute_command(plan_id, ui);
            return;
        }

        // Handle /plans command to list plans
        if line == "/plans" {
            ui.push_log("> /plans");
            self.handle_plans_command(ui);
            return;
        }

        if !line.starts_with('/') {
            let rest = line;
            self.last_user_prompt = Some(rest.to_string());
            ui.push_log(format!("> {rest}"));

            // Auto-detect plan execution
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
                self.execute_plan_async(&plan_id, ui);
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
                    rt.spawn(async move {
                        // Notify that request sending has started
                        if let Some(tx) = &tx {
                            let _ = tx.send("::status:sending".into());
                        }

                        let res = crate::llm::run_agent_loop(
                            &c,
                            &model,
                            &fs,
                            msgs,
                            tx.clone(),
                            Some(cancel_token),
                        )
                        .await;
                        // Get token usage after the agent loop completes
                        let tokens_used = c.get_prompt_tokens_used();
                        match res {
                            Ok((updated_messages, _final_msg)) => {
                                if let Some(tx) = tx {
                                    // run_agent_loop already sends the final assistant content as a
                                    // "::status:done:<content>" message. Avoid duplicating it here.
                                    // Only send token usage update.
                                    let _ = tx.send(format!("::tokens:{}", tokens_used));
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
                                    let _ = sm.update_current_session_with_history(&history);
                                }
                            }
                            Err(e) => {
                                if let Some(tx) = tx {
                                    let _ = tx.send(format!("LLM error: {e}"));
                                    let _ = tx.send("::status:error".into());
                                    // Send token usage update even on error
                                    let _ = tx.send(format!("::tokens:{}", tokens_used));
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
                                    let _ = sm.update_current_session_with_history(&history);
                                }
                            }
                        }
                    });
                }
                None => ui.push_log("OPENAI_API_KEY not set; cannot call LLM."),
            }
        } else {
            ui.push_log(format!("> {line}"));
        }
    }

    /// Handle /execute command
    fn handle_execute_command(&mut self, plan_id: &str, ui: &mut TuiApp) {
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
                self.execute_plan_async(&plan_id, ui);
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
                self.execute_plan_async(plan_id, ui);
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
                        crate::planning::PlanStatus::Created => "[CREATED]",
                        crate::planning::PlanStatus::Running => "[RUNNING]",
                        crate::planning::PlanStatus::Paused => "[PAUSED]",
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
                        crate::planning::PlanStatus::Completed => "[COMPLETED]",
                        crate::planning::PlanStatus::Failed => "[FAILED]",
                        crate::planning::PlanStatus::Cancelled => "[CANCELLED]",
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
    fn execute_plan_async(&mut self, plan_id: &str, ui: &mut TuiApp) {
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

        ui.push_log("[INFO] Starting plan execution...");

        let rt = tokio::runtime::Handle::current();
        let client = self.client.as_ref().unwrap().clone();
        let model = self.cfg.model.clone();
        let fs_tools = self.tools.clone();
        let ui_tx = self.ui_tx.clone();
        let plan_manager = self.plan_manager.clone();
        let plan_id = plan_id.to_string();
        let plan = plan_execution.plan;

        rt.spawn(async move {
            let executor =
                crate::planning::step_executor::TaskExecutor::new(client, model, fs_tools);

            match executor.execute_plan(plan, ui_tx.clone()).await {
                Ok(result) => {
                    if let Ok(pm) = plan_manager.lock() {
                        let _ = pm.complete_execution(&plan_id, result.clone());
                    }

                    if let Some(tx) = ui_tx {
                        if result.success {
                            let _ = tx.send(
                                "[SUCCESS] Plan execution completed successfully!".to_string(),
                            );
                        } else {
                            let _ = tx.send(format!(
                                "[WARNING] Plan execution partially completed: {}",
                                result.final_message
                            ));
                        }
                        let _ = tx.send(format!(
                            "[TIMER] Execution time: {}s",
                            result.total_duration
                        ));
                        let _ = tx.send(format!(
                            "[CONFIRMED] Completed steps: {}/{}",
                            result.completed_steps.len(),
                            result.completed_steps.len()
                        ));
                    }
                }
                Err(e) => {
                    if let Ok(pm) = plan_manager.lock() {
                        let _ = pm.cancel_execution(&plan_id);
                    }

                    if let Some(tx) = ui_tx {
                        let _ = tx.send(format!("[ERROR] Plan execution failed: {}", e));
                    }
                }
            }
        });
    }
}
