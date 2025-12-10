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
pub const COMPACT_PROMPT: &str = r#"Summarize the conversation history into a concise, dense Markdown snapshot.
This snapshot will be the agent's memory. It MUST contain all context needed to resume work.

Structure:
# Goal
Single sentence objective.

# Key Info
Critical facts, commands, or constraints (e.g., "Tests: `npm test`").

# Files
List accessed files with status (READ/MODIFIED/CREATED) and key insights.
CWD: <cwd>

# History
Concise summary of actions and outcomes. Focus on what was done and what failed.
"#;

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
    // Send the summarization request to the LLM using chat_once (no tool usage needed/allowed for compaction)
    match params.client.chat_once(&params.model, msgs, None).await {
        Ok(final_msg) => {
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
        assert!(COMPACT_PROMPT.contains("Summarize the conversation history"));
        assert!(COMPACT_PROMPT.contains("# Goal"));
        assert!(COMPACT_PROMPT.contains("# Key Info"));
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
