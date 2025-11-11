use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Custom command information
#[derive(Debug, Clone)]
pub struct CustomCommand {
    pub description: String,
    pub content: String,
    pub scope: crate::tui::commands::handlers::dispatch::CommandScope,
    pub namespace: Option<String>,
}

/// Load custom commands from project and user directories
pub fn load_custom_commands(project_root: &Path) -> HashMap<String, CustomCommand> {
    let mut commands = HashMap::new();

    // Load project commands (.doge/commands/)
    let project_commands_dir = project_root.join(".doge").join("commands");
    if project_commands_dir.exists() {
        load_commands_from_directory(
            &project_commands_dir,
            crate::tui::commands::handlers::dispatch::CommandScope::Project,
            &mut commands,
        );
    }

    // Load user commands (~/.config/doge-code/commands/)
    if let Some(home_dir) = dirs::home_dir() {
        let user_commands_dir = home_dir.join(".config").join("doge-code").join("commands");
        if user_commands_dir.exists() {
            load_commands_from_directory(
                &user_commands_dir,
                crate::tui::commands::handlers::dispatch::CommandScope::User,
                &mut commands,
            );
        }
    }

    commands
}

/// Load commands from a specific directory
fn load_commands_from_directory(
    dir: &Path,
    scope: crate::tui::commands::handlers::dispatch::CommandScope,
    commands: &mut HashMap<String, CustomCommand>,
) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
                // Get command name from file name (without extension)
                if let Some(file_name) = path.file_stem().and_then(|s| s.to_str()) {
                    // Read file content
                    if let Ok(content) = fs::read_to_string(&path) {
                        // Use first line as description, rest as content
                        let lines: Vec<&str> = content.lines().collect();
                        let description = if !lines.is_empty() {
                            lines[0].to_string()
                        } else {
                            format!("Custom command: {}", file_name)
                        };

                        // Determine namespace from relative path
                        let namespace = path
                            .parent()
                            .and_then(|parent| parent.strip_prefix(dir).ok())
                            .and_then(|rel_path| {
                                if rel_path.components().count() > 0 {
                                    Some(rel_path.to_string_lossy().to_string())
                                } else {
                                    None
                                }
                            });

                        let full_content = content;

                        commands.insert(
                            file_name.to_string(),
                            CustomCommand {
                                description,
                                content: full_content,
                                scope: scope.clone(),
                                namespace,
                            },
                        );
                    }
                }
            } else if path.is_dir() {
                // Recursively load from subdirectories (namespaces)
                load_commands_from_directory(&path, scope.clone(), commands);
            }
        }
    }
}

impl TuiExecutor {
    /// Handle custom slash commands
    pub fn handle_custom_command(&mut self, line: &str, ui: &mut TuiApp) {
        // Parse command and arguments
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            return;
        }

        let command_name = parts[0].trim_start_matches('/');
        let args = &parts[1..];

        // Check if command exists
        if let Some(command) = self.custom_commands.get(command_name) {
            // Process command content with arguments
            let processed_content = process_command_content(&command.content, args);

            // Add to conversation history as user input
            if let Ok(mut history) = self.conversation_history.lock() {
                history.push(crate::llm::types::ChatMessage {
                    role: "user".into(),
                    content: Some(processed_content.clone()),
                    tool_calls: vec![],
                    tool_call_id: None,
                });
            }

            // Display in UI
            ui.push_log(format!("> {}", line));

            // Send to LLM for processing
            self.send_to_llm(ui, processed_content);
        } else {
            ui.push_log(format!("Unknown command: /{}", command_name));
        }
    }

    /// Send message to LLM for processing
    pub fn send_to_llm(&mut self, ui: &mut TuiApp, content: String) {
        match self.client.as_ref() {
            Some(c) => {
                let rt = tokio::runtime::Handle::current();
                let model = self.cfg.model.clone();
                let c = c.clone();
                let tx = self.ui_tx.clone();
                // Prepare a fresh line for the final output
                ui.push_log(String::new());
                let (cancel_tx, mut cancel_rx) = watch::channel(false);
                self.cancel_tx = Some(cancel_tx);

                let cancel_token = CancellationToken::new();
                // Start timer for processing
                ui.processing_start_time = Some(std::time::Instant::now());
                ui.last_elapsed_time = None;
                ui.dirty = true;

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
                msgs.push(crate::llm::types::ChatMessage {
                    role: "system".into(),
                    content: Some(sys_prompt),
                    tool_calls: vec![],
                    tool_call_id: None,
                });

                // Add existing conversation history
                if let Ok(history) = self.conversation_history.lock() {
                    msgs.extend(history.clone());
                }

                msgs.push(crate::llm::types::ChatMessage {
                    role: "user".into(),
                    content: Some(content.clone()),
                    tool_calls: vec![],
                    tool_call_id: None,
                });
                let fs = self.tools.clone();
                let conversation_history = self.conversation_history.clone();
                let session_manager = self.session_manager.clone();
                let cfg = self.cfg.clone();
                let _repomap = self.repomap.clone();
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
                        &cfg,
                        None, // Pass None instead of self
                    )
                    .await;
                    // Get token usage after the agent loop completes
                    let tokens_used = c.get_prompt_tokens_used();
                    let total_tokens = c.get_tokens_used();
                    match res {
                        Ok((updated_messages, _final_msg)) => {
                            // Execute hooks after the agent loop completes
                            // This would require cloning hook_manager which is complex in async context

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
                                history.push(crate::llm::types::ChatMessage {
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
    }
}

/// Process command content by replacing placeholders with arguments
fn process_command_content(content: &str, args: &[&str]) -> String {
    let mut processed = content.to_string();

    // Replace $ARGUMENTS with all arguments joined by space
    if !args.is_empty() {
        let all_args = args.join(" ");
        processed = processed.replace("$ARGUMENTS", &all_args);

        // Replace $1, $2, etc. with specific arguments
        for (i, arg) in args.iter().enumerate() {
            let placeholder = format!("${}", i + 1);
            processed = processed.replace(&placeholder, arg);
        }
    }

    processed
}
