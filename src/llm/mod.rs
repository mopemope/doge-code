mod chat_with_tools;
pub mod client_core;
mod compact_history;
mod history;
mod stream;
mod stream_tools;
mod tool_def;
mod tool_execution;
mod tool_runtime;
pub mod types;

use reqwest::StatusCode;

pub use chat_with_tools::*;
pub use client_core::*;
pub use history::*;
pub use tool_def::*;
pub use types::*;

pub use tool_execution::{run_agent_loop, run_agent_streaming_once};

// Re-export the compact_history module components
pub use compact_history::{
    CompactMetadata, CompactParams, CompactResult, compact_conversation_history,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LlmErrorKind {
    #[error("rate limited")]
    RateLimited,
    #[error("server error")]
    Server,
    #[error("network error")]
    Network,
    #[error("timeout")]
    Timeout,
    #[error("client error")]
    Client,
    #[error("deserialization error")]
    Deserialize,
    #[error("request cancelled")]
    Cancelled,
    #[error("context length exceeded")]
    ContextLengthExceeded,
    #[error("unknown error")]
    Unknown,
}

#[allow(dead_code)]
pub fn classify_error(status: Option<StatusCode>, err: &anyhow::Error) -> LlmErrorKind {
    if let Some(e) = err.downcast_ref::<LlmErrorKind>() {
        return e.clone();
    }
    if let Some(st) = status {
        if st == StatusCode::TOO_MANY_REQUESTS {
            return LlmErrorKind::RateLimited;
        }
        if st.is_server_error() {
            return LlmErrorKind::Server;
        }
        if st.is_client_error() {
            return LlmErrorKind::Client;
        }
    }
    if let Some(e) = err.downcast_ref::<reqwest::Error>() {
        if e.is_timeout() {
            return LlmErrorKind::Timeout;
        }
        if e.is_connect() || e.is_body() || e.is_request() {
            return LlmErrorKind::Network;
        }
    }
    LlmErrorKind::Unknown
}
