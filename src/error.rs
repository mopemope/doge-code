//! Centralized error definitions for Doge-Code
//!
//! This module defines a single error enum that is used throughout the
//! application. It replaces the ad‑hoc use of `anyhow::Error` with a
//! type‑safe, `thiserror` based enum.

use thiserror::Error;

/// Top‑level error type for the Doge‑Code application.
#[derive(Error, Debug)]
pub enum DogecodeError {
    /// Errors originating from configuration handling.
    #[error("Configuration error: {0}")]
    Config(#[from] crate::config::AppConfigError),

    /// Errors that occur while executing commands.
    #[error("Execution error: {0}")]
    Execution(#[from] crate::exec::ExecError),

    /// Errors coming from the tool subsystem.
    #[error("Tool error: {0}")]
    Tool(#[from] crate::tools::ToolError),

    /// Wrapper for any other error that does not have a dedicated variant.
    #[error("Unexpected error: {0}")]
    Other(#[from] anyhow::Error),
}
