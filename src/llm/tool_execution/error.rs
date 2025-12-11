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
            "Hint: The file was not found. \n1. Use `fs_list` to verify the directory structure.\n2. Use `find_file` to search for the file if you are unsure of the path.",
        )
    } else if err_lower.contains("context bounds")
        || err_lower.contains("patch failed")
        || err_lower.contains("hunk")
    {
        Some(
            "Hint: Patch failed due to context mismatch.\n1. The file content may have changed. Use `fs_read` to get the FRESH content.\n2. Rewrite the patch using the exact lines from the fresh content as context.",
        )
    } else if err_lower.contains("json")
        || err_lower.contains("parse error")
        || err_lower.contains("invalid request")
    {
        Some(
            "Hint: JSON serialization/parsing failed.\n1. Check if you are using unescaped quotes inside strings.\n2. Ensure the arguments match the tool schema exactly.\n3. Wrap your step-by-step thinking in <thinking> tags to calm down and format correct JSON.",
        )
    } else if err_lower.contains("syntax") {
        Some(
            "Hint: Syntax error detected in the code you wrote.\n1. Read the error message carefully.\n2. If it's a bracket mismatch, check the nesting.\n3. Fix the code immediately.",
        )
    } else if err_lower.contains("timeout") {
        Some(
            "Hint: The operation timed out.\n1. If you are reading a huge file, try reading it in chunks or use `grep_search` to find what you need.\n2. If it's a network request, try again.",
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
                "Hint: The file was not found. \n1. Use `fs_list` to verify the directory structure.\n2. Use `find_file` to search for the file if you are unsure of the path."
            )
        );
        assert_eq!(
            get_error_hint("command not found"),
            Some(
                "Hint: The file was not found. \n1. Use `fs_list` to verify the directory structure.\n2. Use `find_file` to search for the file if you are unsure of the path."
            )
        );
        assert_eq!(
            get_error_hint("patch failed: hunk #1"),
            Some(
                "Hint: Patch failed due to context mismatch.\n1. The file content may have changed. Use `fs_read` to get the FRESH content.\n2. Rewrite the patch using the exact lines from the fresh content as context."
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
        assert!(
            get_error_hint("timeout")
                .unwrap()
                .contains("operation timed out")
        );
    }
}
