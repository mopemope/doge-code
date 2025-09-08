//! Module for compacting conversation history using LLM summarization.
//!
//! This module provides functionality to compact conversation history by summarizing
//! it into a structured format that preserves essential information while reducing
//! token usage for future LLM interactions.

use crate::config::AppConfig;
use crate::llm::{self, OpenAIClient};
use crate::tools::FsTools;
use anyhow::Result;

/// The prompt used for compacting conversation history
pub const COMPACT_PROMPT: &str = r#"You are the component that summarizes internal chat history into a given structure.

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

/// Parameters for compacting conversation history
pub struct CompactParams {
    /// The LLM client to use for summarization
    pub client: OpenAIClient,
    /// The model to use for summarization
    pub model: String,
    /// The file system tools
    pub fs_tools: FsTools,
    /// The conversation history to compact
    pub history: Vec<llm::types::ChatMessage>,
    /// The application config
    pub cfg: AppConfig,
}

/// Result of compacting conversation history
pub struct CompactResult {
    /// The compacted message containing the summary
    pub compacted_message: llm::types::ChatMessage,
    /// Any additional metadata about the compaction
    pub metadata: CompactMetadata,
}

/// Metadata about the compaction process
pub struct CompactMetadata {
    /// Whether the compaction was successful
    pub success: bool,
    /// Any error message if the compaction failed
    pub error_message: Option<String>,
}

/// Compacts conversation history by summarizing it using an LLM.
///
/// This function takes a conversation history and uses an LLM to summarize it
/// into a structured format that preserves essential information while reducing
/// token usage for future interactions.
///
/// # Arguments
///
/// * `params` - The parameters for compacting the conversation history
///
/// # Returns
///
/// A result containing the compacted message or an error
pub async fn compact_conversation_history(params: CompactParams) -> Result<CompactResult> {
    // Build messages for the summarization request
    let mut msgs = Vec::new();

    // Add system prompt for summarization
    msgs.push(llm::types::ChatMessage {
        role: "system".into(),
        content: Some(COMPACT_PROMPT.to_string()),
        tool_calls: vec![],
        tool_call_id: None,
    });

    // Add the conversation history to be summarized
    msgs.extend(params.history.clone());

    // Send the summarization request to the LLM using run_agent_loop
    match llm::run_agent_loop(
        &params.client,
        &params.model,
        &params.fs_tools,
        msgs,
        None, // No UI sender for this function
        None, // No cancellation token for this operation
        None, // No session manager for compact history
        &params.cfg,
    )
    .await
    {
        Ok((_updated_messages, final_msg)) => {
            // Extract the summary from the final message
            if !final_msg.content.is_empty() {
                // Create a new compacted message with the summary
                let compacted_message = llm::types::ChatMessage {
                    role: "user".into(),
                    content: Some(final_msg.content.clone()),
                    tool_calls: vec![],
                    tool_call_id: None,
                };

                Ok(CompactResult {
                    compacted_message,
                    metadata: CompactMetadata {
                        success: true,
                        error_message: None,
                    },
                })
            } else {
                // Handle case where response has no content
                Ok(CompactResult {
                    compacted_message: llm::types::ChatMessage {
                        role: "user".into(),
                        content: Some("".to_string()),
                        tool_calls: vec![],
                        tool_call_id: None,
                    },
                    metadata: CompactMetadata {
                        success: false,
                        error_message: Some(
                            "Received empty response from LLM during compaction.".to_string(),
                        ),
                    },
                })
            }
        }
        Err(e) => {
            // Handle error
            Ok(CompactResult {
                compacted_message: llm::types::ChatMessage {
                    role: "user".into(),
                    content: Some("".to_string()),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
                metadata: CompactMetadata {
                    success: false,
                    error_message: Some(format!("Failed to compact conversation: {}", e)),
                },
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::ChatMessage;

    #[test]
    fn test_compact_prompt_constant() {
        // Ensure the compact prompt contains expected content
        assert!(
            COMPACT_PROMPT.contains("You are the component that summarizes internal chat history")
        );
        assert!(COMPACT_PROMPT.contains("<state_snapshot>"));
    }

    #[test]
    fn test_compact_result_struct() {
        let message = ChatMessage {
            role: "user".to_string(),
            content: Some("test content".to_string()),
            tool_calls: vec![],
            tool_call_id: None,
        };

        let metadata = CompactMetadata {
            success: true,
            error_message: None,
        };

        let result = CompactResult {
            compacted_message: message.clone(),
            metadata,
        };

        assert_eq!(result.compacted_message.role, "user");
        assert_eq!(
            result.compacted_message.content,
            Some("test content".to_string())
        );
        assert!(result.metadata.success);
    }

    #[test]
    fn test_compact_metadata_struct() {
        let metadata = CompactMetadata {
            success: false,
            error_message: Some("test error".to_string()),
        };

        assert!(!metadata.success);
        assert_eq!(metadata.error_message, Some("test error".to_string()));
    }
}
