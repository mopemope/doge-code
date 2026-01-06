//! Compaction Module
//! This module handles conversation history compaction when context limits are reached.

use crate::config::AppConfig;
use crate::llm;
use crate::llm::client_core::OpenAIClient;
use crate::llm::types::ChatMessage;
use crate::tools::FsTools;
use anyhow::Result;
use std::sync::mpsc;
use tracing::{error, info};

/// Handles conversation history compaction when context limits are approached or exceeded.
/// This function checks if compaction is needed and performs the compaction process.
#[allow(dead_code)]
pub async fn handle_compaction(
    client: &OpenAIClient,
    model: &str,
    _fs: &FsTools,
    messages: &mut Vec<ChatMessage>,
    ui_tx: &Option<mpsc::Sender<String>>,
    _cfg: &AppConfig,
) -> Result<bool> {
    // Send status message to UI
    if let Some(tx) = ui_tx {
        let _ = tx.send(
            "::status:compacting:Context limits approaching, summarizing history...".to_string(),
        );
    }

    // Perform compaction
    match llm::compact_conversation_history_ref(client, model, messages.as_slice()).await {
        Ok(compact_result) => {
            if compact_result.metadata.success {
                info!("History compaction successful");

                // Preserve System Prompt if present
                let system_prompt = messages.iter().find(|m| m.role == "system").cloned();
                messages.clear();
                if let Some(sys) = system_prompt {
                    messages.push(sys);
                }
                messages.push(compact_result.compacted_message);

                // Inform UI
                if let Some(tx) = ui_tx {
                    let _ = tx.send("::status:waiting:History compacted. Retrying...".to_string());
                }

                Ok(true)
            } else {
                error!(
                    "History compaction failed: {:?}",
                    compact_result.metadata.error_message
                );
                Ok(false)
            }
        }
        Err(compact_err) => {
            error!("Error during history compaction: {}", compact_err);
            Ok(false)
        }
    }
}

/// Checks if compaction should be performed based on current token usage and configuration.
#[allow(dead_code)]
pub fn should_compact(current_tokens: u32, cfg: &AppConfig, messages: &Vec<ChatMessage>) -> bool {
    let threshold = cfg.auto_compact_prompt_token_threshold_for_current_model();
    let context_limit = cfg.get_context_window_size().unwrap_or(128_000);
    let safety_limit = (context_limit as f64 * 0.9) as u32;
    let effective_limit = std::cmp::min(threshold, safety_limit);

    // Only compact if we are over the limit AND we have enough history
    current_tokens > effective_limit && messages.len() > 2
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::llm::types::ChatMessage;

    fn create_dummy_messages(count: usize) -> Vec<ChatMessage> {
        (0..count)
            .map(|i| ChatMessage {
                role: if i % 2 == 0 {
                    "user".into()
                } else {
                    "assistant".into()
                },
                content: Some(format!("msg {}", i)),
                tool_calls: vec![],
                tool_call_id: None,
            })
            .collect()
    }

    #[test]
    fn test_should_compact() {
        let mut cfg = AppConfig::default();
        // Set a low threshold for testing
        cfg.auto_compact_prompt_token_threshold = 1000;
        cfg.model = "gpt-4".to_string(); // Has default context window

        let messages = create_dummy_messages(3); // > 2 messages

        // Case 1: Tokens > threshold -> Compact
        assert!(should_compact(1001, &cfg, &messages));

        // Case 2: Tokens <= threshold -> Do not compact
        assert!(!should_compact(1000, &cfg, &messages));
    }

    #[test]
    fn test_should_not_compact_not_enough_messages() {
        let mut cfg = AppConfig::default();
        cfg.auto_compact_prompt_token_threshold = 1000;

        let messages = create_dummy_messages(2); // <= 2 messages

        // Even with high token count, should not compact if history is too short
        assert!(!should_compact(2000, &cfg, &messages));
    }

    #[test]
    fn test_should_compact_safety_limit() {
        let mut cfg = AppConfig::default();
        // Set threshold very high
        cfg.auto_compact_prompt_token_threshold = 100_000;
        // Model with small context (gpt-4 -> 8192)
        cfg.model = "gpt-4".to_string();
        // 8192 * 0.9 = 7372.8 -> 7372 safety limit

        let messages = create_dummy_messages(10);

        // Usage below safety limit
        assert!(!should_compact(7000, &cfg, &messages));

        // Usage above safety limit (7372) matches effective limit logic
        assert!(should_compact(7400, &cfg, &messages));
    }
}
