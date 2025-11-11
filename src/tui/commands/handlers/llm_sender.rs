use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::info;

impl TuiExecutor {
    /// Send message to LLM for processing
    pub fn send_to_llm(&mut self, ui: &mut TuiApp, content: String) {
        // Store the last user input for retrying after compact
        ui.last_user_input = Some(content.clone());
        
        // Start elapsed time tracking
        ui.processing_start_time = Some(std::time::Instant::now());
        
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

                    // Increment request count in session
                    {
                        let mut sm = session_manager.lock().unwrap();
                        if let Err(e) = sm.update_current_session_with_request_count() {
                            tracing::error!(?e, "Failed to update session with request count");
                        }
                    }

                    let cfg = self.cfg.clone();
                    let res = crate::llm::run_agent_loop(
                        &c,
                        &model,
                        &fs,
                        msgs,
                        tx.clone(),
                        Some(cancel_token),
                        &cfg,
                        Some(self),
                    )
                    .await;
                    // Get token usage after the agent loop completes
                    let tokens_used = c.get_prompt_tokens_used();
                    let total_tokens = c.get_tokens_used();
                    match res {
                        Ok((updated_messages, final_msg)) => {
                            // Execute hooks after the agent loop completes
                            let final_assistant_msg = crate::llm::types::ChatMessage {
                                role: "assistant".into(),
                                content: Some(final_msg.content.clone()),
                                tool_calls: vec![],
                                tool_call_id: None,
                            };
                            
                            // Access the hook manager through the executor (self)
                            // We need to get a closure with the necessary values for hook execution
                            let hook_manager = self.hook_manager.clone();
                            let cfg = self.cfg.clone();
                            let fs = self.tools.clone();
                            let repomap = self.repomap.clone();
                            
                            // Spawn a task to execute hooks without blocking the main flow
                            let hook_fut = async move {
                                if let Err(e) = hook_manager.execute_hooks(
                                    &updated_messages,
                                    &final_assistant_msg,
                                    &cfg,
                                    &fs,
                                    &repomap,
                                ).await {
                                    tracing::error!("Error executing hooks: {}", e);
                                }
                            };
                            
                            // Execute the hooks task
                            tokio::spawn(hook_fut);
                            // Store the final elapsed time string without taking the start time
                            if let Some(start_time) = ui.processing_start_time {
                                let elapsed = start_time.elapsed();
                                let elapsed_secs = elapsed.as_secs();
                                let hours = elapsed_secs / 3600;
                                let minutes = (elapsed_secs % 3600) / 60;
                                let seconds = elapsed_secs % 60;
                                ui.last_elapsed_time = Some(format!("{:02}:{:02}:{:02}", hours, minutes, seconds));
                                // Reset processing_start_time to stop the timer
                                ui.processing_start_time = None;
                            }

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

                                // If there are changed files in this session, trigger a repomap rebuild
                                if sm.current_session_has_changed_files() {
                                    if let Some(tx) = &tx {
                                        let _ = tx.send("::status:updating_repomap".to_string());
                                    }

                                    // Capture state for async repomap rebuild
                                    let repomap_clone = fs.repomap.clone();
                                    let project_root = cfg.project_root.clone();
                                    let ui_tx = tx.clone();
                                    let mut sm_for_clear = sm;

                                    tokio::spawn(async move {
                                        use crate::analysis::Analyzer;
                                        use tracing::{error, info, warn};

                                        info!("Starting post-session repomap rebuild");

                                        let mut analyzer = match Analyzer::new(&project_root).await {
                                            Ok(analyzer) => analyzer,
                                            Err(e) => {
                                                error!("Failed to create Analyzer for repomap rebuild: {:?}", e);
                                                if let Some(tx) = ui_tx.clone() {
                                                    let _ = tx.send(format!(
                                                        "[Failed to create analyzer for repomap rebuild: {}]",
                                                        e
                                                    ));
                                                }
                                                return;
                                            }
                                        };

                                        if let Err(e) = analyzer.clear_cache().await {
                                            warn!("Failed to clear cache before repomap rebuild: {}", e);
                                        }

                                        match analyzer.build_parallel().await {
                                            Ok(map) => {
                                                *repomap_clone.write().await = Some(map);
                                                if let Some(tx) = ui_tx {
                                                    let _ = tx.send("::status:repomap_updated".to_string());
                                                }
                                            }
                                            Err(e) => {
                                                error!("Failed to rebuild repomap: {:?}", e);
                                                if let Some(tx) = ui_tx {
                                                    let _ = tx.send(format!(
                                                        "[Failed to rebuild repomap: {}]",
                                                        e
                                                    ));
                                                }
                                            }
                                        }
                                    });

                                    if let Err(e) = sm_for_clear.clear_changed_files_from_current_session() {
                                        tracing::error!(
                                            ?e,
                                            "Failed to clear changed files from session after repomap rebuild trigger"
                                        );
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            // Store the final elapsed time string without taking the start time (even on error)
                            if let Some(start_time) = ui.processing_start_time {
                                let elapsed = start_time.elapsed();
                                let elapsed_secs = elapsed.as_secs();
                                let hours = elapsed_secs / 3600;
                                let minutes = (elapsed_secs % 3600) / 60;
                                let seconds = elapsed_secs % 60;
                                ui.last_elapsed_time = Some(format!("{:02}:{:02}:{:02}", hours, minutes, seconds));
                                // Reset processing_start_time to stop the timer
                                ui.processing_start_time = None;
                            }

                            // Check if the error is due to context length exceeded
                            if let Some(crate::llm::LlmErrorKind::ContextLengthExceeded) = e.downcast_ref::<crate::llm::LlmErrorKind>() {
                                if let Some(tx) = tx {
                                    let _ = tx.send("[INFO] Context length exceeded. Compacting conversation history...".to_string());
                                    // Send a special message to trigger the compact command in the UI
                                    let _ = tx.send("::trigger_compact".to_string());
                                }
                            }
                            
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

                                // Check for changed files and trigger repomap update if needed (even on error)
                                if sm.current_session_has_changed_files() {
                                    if let Some(tx) = &tx {
                                        let _ = tx.send("::status:updating_repomap".to_string());
                                    }
                                    
                                    // Get changed files
                                    let changed_files = sm.get_changed_files_from_current_session();
                                    
                                    // Update repomap with changed files - skipped to avoid duplicate analyzer creation
                                    // The main analyzer handles the full repomap which should include changes
                                    
                                    // Clear changed files from session
                                    if let Err(e) = sm.clear_changed_files_from_current_session() {
                                        tracing::error!(?e, "Failed to clear changed files from session");
                                    }
                                    
                                    if let Some(tx) = &tx {
                                        let _ = tx.send("::status:repomap_updated".to_string());
                                    }
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