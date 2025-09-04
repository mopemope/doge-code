use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::info;

impl TuiExecutor {
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
                                // Only send token usage update (prompt + total)
                                let _ = tx.send(format!(
                                    "::tokens:prompt:{},total:{}",
                                    tokens_used,
                                    c.get_tokens_used()
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
                                let _ = sm.update_current_session_with_history(&history);
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
                                    c.get_tokens_used()
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
                                let _ = sm.update_current_session_with_history(&history);
                            }
                        }
                    }
                });
            }
            None => ui.push_log("OPENAI_API_KEY not set; cannot call LLM."),
        }
    }
}