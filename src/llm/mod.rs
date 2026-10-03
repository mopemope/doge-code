mod chat_with_tools;
pub mod client_core;
mod compact_history;
pub mod context_budget;
mod history;
mod message_utils;
pub mod observation;
pub mod prompt_cache;
mod prompts;
pub mod reasoning;
pub(crate) mod retry;
pub mod runtime_context;
mod stream;
mod stream_tools;
mod symbol_edit;
mod tool_catalog;
mod tool_def;
pub mod tool_execution;
pub(crate) mod tool_runtime;
pub mod types;
pub mod usage_ledger;

pub use chat_with_tools::*;
pub use client_core::*;
pub use history::*;
pub use message_utils::*;
pub use prompts::*;
pub use symbol_edit::*;
pub use tool_catalog::*;
pub use tool_def::*;
pub use types::*;

pub use tool_execution::{run_agent_loop, run_agent_streaming_once};

// Re-export the compact_history module components
pub use compact_history::{
    CompactMetadata, CompactParams, CompactResult, compact_conversation_history,
    compact_conversation_history_ref,
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
    #[error("authentication error")]
    Authentication,
    #[error("deserialization error")]
    Deserialize,
    #[error("request cancelled")]
    Cancelled,
    #[error("context length exceeded")]
    ContextLengthExceeded,
    #[error("incomplete model response")]
    Incomplete,
    #[error("unknown error")]
    Unknown,
}
