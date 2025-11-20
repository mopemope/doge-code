use crate::tui::view::TuiApp;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::tui::commands::core::TuiExecutor;

impl TuiExecutor {
    pub fn handle_dispatch_rest(&mut self, line: &str, ui: &mut TuiApp) {
        if let Some(rest) = line.strip_prefix("/session ") {
            match self.handle_session_command(rest.trim(), ui) {
                Ok(_) => {} // No-op on success
                Err(e) => ui.push_log(format!("Error handling session command: {}", e)),
            }
            return;
        }

        if !line.starts_with('/') {
            {
                let mut sm = self.session_manager.lock().unwrap();
                if sm.current_session.is_none() {
                    // Use the incoming user line as the initial prompt/title for the new session
                    match sm.create_session(Some(line.to_string())) {
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

                    self.enforce_plan_context(&mut msgs, &content, Some(ui));

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
                            Ok((updated_messages, final_msg)) => {
                                // Execute hooks after the agent loop completes
                                let _final_assistant_msg = crate::llm::ChatMessage {
                                    role: "assistant".into(),
                                    content: Some(final_msg.content.clone()),
                                    tool_calls: vec![],
                                    tool_call_id: None,
                                };

                                // This requires hook_manager to be cloned, which is complex
                                // For now, execute hooks in a different way or skip in async context
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
}
