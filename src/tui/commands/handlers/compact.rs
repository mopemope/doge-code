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

        // Prepare the summarization prompt
        let summarization_prompt = r#"You are the component that summarizes internal chat history into a given structure.

When the conversation history grows too large, you will be invoked to distill the entire history into a concise, structured XML snapshot. This snapshot is CRITICAL, as it will become the agent's *only* memory of the past. The agent will resume its work based solely on this snapshot. All crucial details, plans, errors, and user directives MUST be preserved.

First, you will think through the entire history in a private <scratchpad>. Review the user's overall goal, the agent's actions, tool outputs, file modifications, and any unresolved questions. Identify every piece of information that is essential for future actions.

After your reasoning is complete, generate the final <state_snapshot> XML object. Be incredibly dense with information. Omit any irrelevant conversational filler.

The structure MUST be as follows:

<state_snapshot>
    <overall_goal>
        <!-- A single, concise sentence describing the user's high-level objective. -->
        <!-- Example: "Refactor the authentication service to use a new JWT library." -->
    </overall_goal>

    <key_knowledge>
        <!-- Crucial facts, conventions, and constraints the agent must remember based on the conversation history and interaction with the user. Use bullet points. -->
        <!-- Example:
         - Build Command: \`npm run build\`
         - Testing: Tests are run with \`npm test\`. Test files must end in \`.test.ts\`.
         - API Endpoint: The primary API endpoint is \`https://api.example.com/v2\`.

        -->
    </key_knowledge>

    <file_system_state>
        <!-- List files that have been created, read, modified, or deleted. Note their status and critical learnings. -->
        <!-- Example:
         - CWD: \`/home/user/project/src\`
         - READ: \`package.json\` - Confirmed 'axios' is a dependency.
         - MODIFIED: \`services/auth.ts\` - Replaced 'jsonwebtoken' with 'jose'.
         - CREATED: \`tests/new-feature.test.ts\` - Initial test structure for the new feature.
        -->
    </file_system_state>

    <recent_actions>
        <!-- A summary of the last few significant agent actions and their outcomes. Focus on facts. -->
        <!-- Example:
         - Ran \`grep 'old_function'\` which returned 3 results in 2 files.
         - Ran \`npm run test\`, which failed due to a snapshot mismatch in \`UserProfile.test.ts\`.
         - Ran \`ls -F static/\` and discovered image assets are stored as \`.webp\`.
        -->
    </recent_actions>

    <current_plan>
        <!-- The agent's step-by-step plan. Mark completed steps. -->
        <!-- Example:
         1. [DONE] Identify all files using the deprecated 'UserAPI'.
         2. [IN PROGRESS] Refactor \`src/components/UserProfile.tsx\` to use the new 'ProfileAPI'.
         3. [TODO] Refactor the remaining files.
         4. [TODO] Update tests to reflect the API change.
        -->
    </current_plan>
</state_snapshot>"#;

        // Build messages for the summarization request
        let mut msgs = Vec::new();

        // Add system prompt for summarization
        msgs.push(crate::llm::ChatMessage {
            role: "system".into(),
            content: Some(summarization_prompt.to_string()),
            tool_calls: vec![],
            tool_call_id: None,
        });

        // Add the conversation history to be summarized
        msgs.extend(history.clone());

        // Get client and model info
        let client = self.client.as_ref().unwrap().clone();
        let model = self.cfg.model.clone();
        let ui_tx = self.ui_tx.clone();
        let fs = self.tools.clone();
        let conversation_history = self.conversation_history.clone();
        let session_manager = self.session_manager.clone();

        // Spawn async task to perform the summarization
        let rt = tokio::runtime::Handle::current();
        rt.spawn(async move {
            // Send the summarization request to the LLM using run_agent_loop
            match crate::llm::run_agent_loop(
                &client,
                &model,
                &fs,
                msgs,
                ui_tx.clone(),
                None, // No cancellation token for this operation
            )
            .await
            {
                Ok((_updated_messages, final_msg)) => {
                    // Extract the summary from the final message
                    if !final_msg.content.is_empty() {
                        // Create a new compacted message with the summary
                        let compacted_message = crate::llm::ChatMessage {
                            role: "user".into(),
                            content: Some(final_msg.content.clone()),
                            tool_calls: vec![],
                            tool_call_id: None,
                        };

                        // Update conversation history with just the compacted message
                        if let Ok(mut history) = conversation_history.lock() {
                            history.clear();
                            history.push(compacted_message);

                            // Also save conversation history to session
                            let mut sm = session_manager.lock().unwrap();
                            let _ = sm.update_current_session_with_history(&history);
                        }

                        // Notify UI of success
                        if let Some(tx) = ui_tx {
                            let _ = tx.send(
                                "[SUCCESS] Conversation history has been compacted.".to_string(),
                            );
                            // Don't send the full content as it might be too long for the UI
                        }
                    } else {
                        // Handle case where response has no content
                        if let Some(tx) = ui_tx {
                            let _ = tx.send(
                                "[ERROR] Received empty response from LLM during compaction."
                                    .to_string(),
                            );
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
