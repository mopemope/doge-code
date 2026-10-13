//! Shared failure categories for agent operations and provider adapters.
//! No provider implementation, UI or execution dependencies.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentErrorKind {
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
    #[error("invalid tool arguments")]
    InvalidToolArguments,
    #[error("request cancelled")]
    Cancelled,
    #[error("context length exceeded")]
    ContextLengthExceeded,
    #[error("incomplete model response")]
    Incomplete,
    #[error("unknown error")]
    Unknown,
}
