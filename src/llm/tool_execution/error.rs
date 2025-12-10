//! Error Handling Module
//!
//! This module provides custom error types and handling utilities for the agent loop.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentLoopError {
    #[error("Git operation failed: {0}")]
    GitError(String),

    #[error("LLM communication error: {0}")]
    LLMError(String),

    #[error("Tool execution failed: {0}")]
    ToolError(String),

    #[error("Max iterations reached: {0}")]
    MaxIterationsError(usize),

    #[error("Context length exceeded")]
    ContextLengthExceeded,

    #[error("Diff collection failed: {0}")]
    DiffCollectionError(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("Compaction failed: {0}")]
    CompactionError(String),

    #[error("Unknown error: {0}")]
    UnknownError(String),
}

impl From<anyhow::Error> for AgentLoopError {
    fn from(err: anyhow::Error) -> Self {
        AgentLoopError::UnknownError(err.to_string())
    }
}

impl From<std::io::Error> for AgentLoopError {
    fn from(err: std::io::Error) -> Self {
        AgentLoopError::UnknownError(err.to_string())
    }
}

impl From<serde_json::Error> for AgentLoopError {
    fn from(err: serde_json::Error) -> Self {
        AgentLoopError::SerializationError(err.to_string())
    }
}

/// Handles agent errors by logging and optionally sending to UI
pub fn handle_agent_error(error: &AgentLoopError, ui_tx: &Option<std::sync::mpsc::Sender<String>>) {
    use tracing::error;

    match error {
        AgentLoopError::GitError(msg) => {
            error!(error = msg, "Git operation failed");
            if let Some(tx) = ui_tx {
                let _ = tx.send(format!("::error:git:{}", msg));
            }
        }
        AgentLoopError::LLMError(msg) => {
            error!(error = msg, "LLM communication error");
            if let Some(tx) = ui_tx {
                let _ = tx.send(format!("::error:llm:{}", msg));
            }
        }
        AgentLoopError::ToolError(msg) => {
            error!(error = msg, "Tool execution failed");
            if let Some(tx) = ui_tx {
                let _ = tx.send(format!("::error:tool:{}", msg));
            }
        }
        AgentLoopError::MaxIterationsError(iterations) => {
            error!(iterations = iterations, "Max iterations reached");
            if let Some(tx) = ui_tx {
                let _ = tx.send(format!("::error:max_iterations:{}", iterations));
            }
        }
        AgentLoopError::ContextLengthExceeded => {
            error!("Context length exceeded");
            if let Some(tx) = ui_tx {
                let _ = tx.send("::error:context_length_exceeded".to_string());
            }
        }
        AgentLoopError::DiffCollectionError(msg) => {
            error!(error = msg, "Diff collection failed");
            if let Some(tx) = ui_tx {
                let _ = tx.send(format!("::error:diff_collection:{}", msg));
            }
        }
        AgentLoopError::SerializationError(msg) => {
            error!(error = msg, "Serialization error");
            if let Some(tx) = ui_tx {
                let _ = tx.send(format!("::error:serialization:{}", msg));
            }
        }
        AgentLoopError::CompactionError(msg) => {
            error!(error = msg, "Compaction error");
            if let Some(tx) = ui_tx {
                let _ = tx.send(format!("::error:compaction:{}", msg));
            }
        }
        AgentLoopError::UnknownError(msg) => {
            error!(error = msg, "Unknown error");
            if let Some(tx) = ui_tx {
                let _ = tx.send(format!("::error:unknown:{}", msg));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_conversion() {
        let anyhow_err = anyhow::anyhow!("test error");
        let agent_err = AgentLoopError::from(anyhow_err);
        assert!(matches!(agent_err, AgentLoopError::UnknownError(_)));
    }
}
