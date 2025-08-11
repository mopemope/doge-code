mod chat_with_tools;
mod client_core;
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
pub use tool_execution::*;
pub use types::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmErrorKind {
    RateLimited,
    Server,
    Network,
    Timeout,
    Client,
    Deserialize,
    Unknown,
}

#[allow(dead_code)]
pub fn classify_error(status: Option<StatusCode>, err: &anyhow::Error) -> LlmErrorKind {
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
