use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;

impl TuiExecutor {
    /// Handle /compact command to summarize conversation history
    pub fn handle_compact_command(&mut self, ui: &mut TuiApp) {
        // Check if we have an LLM client
        if self.client.is_none() {
            ui.push_log("[ERROR] LLM client is not configured. Cannot compact conversation.");
            return;
        }

        // Get conversation history
        let history = {
            if let Ok(history) = self.conversation_history.lock() {
                history.clone()
            } else {
                ui.push_log("[ERROR] Failed to access conversation history.");
                return;
            }
        };

        // Check if we have any conversation to compact
        if history.is_empty() {
            ui.push_log("[INFO] No conversation history to compact.");
            return;
        }

        ui.push_log("[INFO] Compacting conversation history...");

        // Get client and model info
        let client = self.client.as_ref().unwrap().clone();
        let model = self.cfg.model.clone();
        let fs_tools = self.tools.clone();
        let ui_tx = self.ui_tx.clone();
        let conversation_history = self.conversation_history.clone();
        let session_manager = self.session_manager.clone();

        // Spawn async task to perform the summarization
        let rt = tokio::runtime::Handle::current();
        rt.spawn(async move {
            // Prepare parameters for compacting
            let params = crate::llm::CompactParams {
                client,
                model,
                fs_tools,
                history,
            };

            // Compact the conversation history
            match crate::llm::compact_conversation_history(params).await {
                Ok(result) => {
                    if result.metadata.success {
                        // Update conversation history with just the compacted message
                        if let Ok(mut history) = conversation_history.lock() {
                            history.clear();
                            history.push(result.compacted_message.clone());

                            // Also save conversation history to session
                            let mut sm = session_manager.lock().unwrap();
                            let _ = sm.update_current_session_with_history(&history);
                        }

                        // Notify UI of success
                        if let Some(tx) = ui_tx {
                            let _ = tx.send(
                                "[SUCCESS] Conversation history has been compacted.".to_string(),
                            );
                        }
                    } else {
                        // Handle case where compaction failed
                        if let Some(tx) = ui_tx {
                            let _ = tx.send(format!(
                                "[ERROR] Failed to compact conversation: {}",
                                result
                                    .metadata
                                    .error_message
                                    .unwrap_or_else(|| "Unknown error".to_string())
                            ));
                        }
                    }
                }
                Err(e) => {
                    // Handle error
                    if let Some(tx) = ui_tx {
                        let _ = tx.send(format!("[ERROR] Failed to compact conversation: {}", e));
                    }
                }
            }
        });
    }
}
