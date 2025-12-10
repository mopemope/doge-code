//! Compaction Module
//! This module handles conversation history compaction when context limits are reached.

use crate::llm::{self, CompactParams, CompactResult};
use crate::llm::types::ChatMessage;
use crate::config::AppConfig;
use crate::llm::client_core::OpenAIClient;
use crate::tools::FsTools;
use anyhow::Result;
use std::sync::mpsc;
use tracing::{info, warn, error};

/// Handles conversation history compaction when context limits are approached or exceeded.
/// This function checks if compaction is needed and performs the compaction process.
pub async fn handle_compaction(
    client: &OpenAIClient,
    model: &str,
    fs: &FsTools,
    messages: &mut Vec<ChatMessage>,
    ui_tx: &Option<mpsc::Sender<String>>,
    cfg: &AppConfig,
) -> Result<bool> {
    // Send status message to UI
    if let Some(tx) = ui_tx {
        let _ = tx.send("::status:compacting:Context limits approaching, summarizing history...".to_string());
    }

    // Prepare compaction parameters
    let params = CompactParams {
        client: client.clone(),
        model: model.to_string(),
        fs_tools: fs.clone(),
        history: messages.clone(),
        cfg: cfg.clone(),
    };

    // Perform compaction
    match llm::compact_conversation_history(params).await {
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
                error!("History compaction failed: {:?}", compact_result.metadata.error_message);
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
pub fn should_compact(
    current_tokens: u32,
    cfg: &AppConfig,
    messages: &Vec<ChatMessage>,
) -> bool {
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
    
    #[test]
    fn test_should_compact() {
        // Test compaction logic
        // This would need proper setup with mock configuration
    }

    #[test]
    fn test_should_not_compact() {
        // Test when compaction should not occur
        // This would need proper setup with mock configuration
    }
}