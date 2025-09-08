use crate::tui::commands::core::TuiExecutor;
use crate::tui::state::Status;
use crate::tui::view::TuiApp;

impl TuiExecutor {
    /// Handle /compact command to summarize conversation history
    ///
    /// This version runs the compaction asynchronously on the tokio runtime and
    /// reports progress via the UI channel so the TUI remains responsive. The
    /// UI status is set to Processing to prevent dispatching further instructions
    /// until compaction completes.
    pub fn handle_compact_command(&mut self, ui: &mut TuiApp) {
        // Ensure we have an LLM client
        if self.client.is_none() {
            ui.push_log("[ERROR] LLM client is not configured. Cannot compact conversation.");
            return;
        }

        // Get conversation history
        let history = match self.conversation_history.lock() {
            Ok(h) => h.clone(),
            Err(_) => {
                ui.push_log("[ERROR] Failed to access conversation history.");
                return;
            }
        };

        // Check if we have any conversation to compact
        if history.is_empty() {
            ui.push_log("[INFO] No conversation history to compact.");
            return;
        }

        // Update UI and status to indicate work has started
        ui.push_log("[Command] Compacting conversation history...");
        ui.status = Status::Processing;

        // Ensure ui_tx is set so background task can report back
        if self.ui_tx.is_none() {
            self.ui_tx = ui.sender();
        }

        // Prepare parameters and clones for the async task
        let mut client = self.client.as_ref().unwrap().clone();
        let model = self.cfg.model.clone();
        let fs_tools = self.tools.clone();
        client.reason_enable = false;
        let cfg = self.cfg.clone();
        let params = crate::llm::CompactParams {
            client,
            model,
            fs_tools,
            history,
            cfg,
        };

        let conversation_history = self.conversation_history.clone();
        let session_manager = self.session_manager.clone();
        let ui_tx = self.ui_tx.clone();

        // Spawn an async task to perform compaction and report results via ui_tx
        let rt = tokio::runtime::Handle::current();
        rt.spawn(async move {
            if let Some(tx) = &ui_tx {
                let _ = tx.send("::status:preparing".to_string());
            }

            // Run compaction
            match crate::llm::compact_conversation_history(params).await {
                Ok(result) => {
                    if result.metadata.success {
                        // Update conversation history with just the compacted message
                        if let Ok(mut history) = conversation_history.lock() {
                            history.clear();
                            history.push(result.compacted_message.clone());

                            // Also save conversation history to session
                            if let Ok(mut sm) = session_manager.lock()
                                && let Err(e) = sm.update_current_session_with_history(&history)
                            {
                                tracing::error!(
                                    ?e,
                                    "Failed to update session with conversation history"
                                );
                            }
                        }

                        if let Some(tx) = &ui_tx {
                            let _ = tx.send(
                                "[SUCCESS] Conversation history has been compacted.".to_string(),
                            );
                        }
                    } else if let Some(tx) = &ui_tx {
                        let _ = tx.send(format!(
                            "[ERROR] Failed to compact conversation: {}",
                            result
                                .metadata
                                .error_message
                                .unwrap_or_else(|| "Unknown error".to_string())
                        ));
                    }
                }
                Err(e) => {
                    if let Some(tx) = &ui_tx {
                        let _ = tx.send(format!("[ERROR] Failed to compact conversation: {}", e));
                    }
                }
            }

            // Signal completion to the UI loop
            if let Some(tx) = &ui_tx {
                let _ = tx.send("::status:done".to_string());
            }
        });
    }
}
