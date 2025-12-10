//! Error Handling Module
//!
//! This module provides custom error types and handling utilities for the agent loop.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentLoopError {
    #[error("LLM communication error: {0}")]
    Llm(String),

    #[error("Max iterations reached: {0}")]
    MaxIterations(usize),

    #[error("Diff collection failed: {0}")]
    DiffCollection(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Unknown error: {0}")]
    Unknown(String),
}

impl From<anyhow::Error> for AgentLoopError {
    fn from(err: anyhow::Error) -> Self {
        AgentLoopError::Unknown(err.to_string())
    }
}

impl From<std::io::Error> for AgentLoopError {
    fn from(err: std::io::Error) -> Self {
        AgentLoopError::Unknown(err.to_string())
    }
}

impl From<serde_json::Error> for AgentLoopError {
    fn from(err: serde_json::Error) -> Self {
        AgentLoopError::Serialization(err.to_string())
    }
}

/// Handles agent errors by logging and optionally sending to UI
pub fn handle_agent_error(error: &AgentLoopError, ui_tx: &Option<std::sync::mpsc::Sender<String>>) {
    use tracing::error;

    match error {
        AgentLoopError::Llm(msg) => {
            error!(error = msg, "LLM communication error");
            if let Some(tx) = ui_tx {
                let _ = tx.send(format!("::error:llm:{}", msg));
            }
        }
        AgentLoopError::MaxIterations(iterations) => {
            error!(iterations = iterations, "Max iterations reached");
            if let Some(tx) = ui_tx {
                let _ = tx.send(format!("::error:max_iterations:{}", iterations));
            }
        }
        AgentLoopError::DiffCollection(msg) => {
            error!(error = msg, "Diff collection failed");
            if let Some(tx) = ui_tx {
                let _ = tx.send(format!("::error:diff_collection:{}", msg));
            }
        }
        AgentLoopError::Serialization(msg) => {
            error!(error = msg, "Serialization error");
            if let Some(tx) = ui_tx {
                let _ = tx.send(format!("::error:serialization:{}", msg));
            }
        }
        AgentLoopError::Unknown(msg) => {
            error!(error = msg, "Unknown error");
            if let Some(tx) = ui_tx {
                let _ = tx.send(format!("::error:unknown:{}", msg));
            }
        }
    }
}

/// Returns a hint string for a given error message, or None if no hint is available.
pub fn get_error_hint(err_str: &str) -> Option<&'static str> {
    let err_lower = err_str.to_lowercase();

    if err_lower.contains("no such file") || err_lower.contains("not found") {
        Some(
            "Hint: File not found. Use `find_file` to locate it or `fs_list` to check the directory structure.",
        )
    } else if err_lower.contains("context bounds") || err_lower.contains("patch failed") {
        Some(
            "Hint: Patch failed due to context mismatch. Use `fs_read` to get the FRESH content of the file, then regenerate the patch.",
        )
    } else if err_lower.contains("json") || err_lower.contains("parse error") {
        Some(
            "Hint: JSON serialization/parsing failed. Ensure arguments are valid JSON. Avoid using unescaped quotes in strings.",
        )
    } else if err_lower.contains("syntax") {
        Some(
            "Hint: Syntax error detected. Read the code and the error message carefully to fix the syntax.",
        )
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_conversion() {
        let anyhow_err = anyhow::anyhow!("test error");
        let agent_err = AgentLoopError::from(anyhow_err);
        assert!(matches!(agent_err, AgentLoopError::Unknown(_)));
    }

    #[test]
    fn test_get_error_hint() {
        assert_eq!(
            get_error_hint("No such file or directory"),
            Some(
                "Hint: File not found. Use `find_file` to locate it or `fs_list` to check the directory structure."
            )
        );
        assert_eq!(
            get_error_hint("command not found"),
            Some(
                "Hint: File not found. Use `find_file` to locate it or `fs_list` to check the directory structure."
            )
        );
        assert_eq!(
            get_error_hint("patch failed: hunk #1"),
            Some(
                "Hint: Patch failed due to context mismatch. Use `fs_read` to get the FRESH content of the file, then regenerate the patch."
            )
        );
        assert_eq!(get_error_hint("some random error"), None);

        // Test new hints
        assert!(
            get_error_hint("invalid json")
                .unwrap()
                .contains("JSON serialization/parsing failed")
        );
        assert!(
            get_error_hint("Parse Error")
                .unwrap()
                .contains("JSON serialization/parsing failed")
        );
        assert!(
            get_error_hint("syntax error")
                .unwrap()
                .contains("Syntax error detected")
        );
    }
}
