//! Provider adapter abstraction: how the agent manager drives one provider CLI
//! (Claude Code, Codex) without knowing its wire format.
//!
//! An adapter starts sessions; a session accepts user input and control
//! requests and reports normalized [`ProviderEvent`]s on its channel. The
//! traits return [`BoxFut`] instead of using `async fn` so they stay
//! dyn-compatible without `async-trait`.
//!
//! Contract for implementations:
//! - Error messages (`ProviderError`) must not contain prompt or agent output
//!   text; they are stored in `Agent::last_error` and may be logged.
//! - `ProviderEvent::Exited` is emitted once when the provider process ends
//!   for any reason other than [`ProviderSession::shutdown`]; the manager then
//!   marks the agent failed. Closing the event channel without it counts as an
//!   unexpected exit.
//! - After [`ProviderSession::interrupt`] returns, the adapter must not emit
//!   `TurnCompleted` or `PermissionRequested` for the interrupted turn: the
//!   manager treats the agent as idle (or already running the next turn) at
//!   that point. Such events buffered before `interrupt` returned are
//!   discarded by the manager (`TurnCompleted` is recorded without a state
//!   change, `PermissionRequested` is dropped). Permission requests
//!   outstanding at interrupt are treated as cancelled; the adapter answers
//!   them to the provider if its protocol needs that.
//! - `start` and every session method must complete in bounded time. The
//!   manager enforces bounds (`agent::ProviderTimeouts`): a call that exceeds
//!   its bound fails the agent and the session is shut down.

pub mod claude;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use dgc_protocol::event::EventKind;
use dgc_protocol::ids::AgentId;
use dgc_protocol::model::{Decision, PermissionKind, ProviderKind};
use serde_json::Value;
use tokio::sync::mpsc;

/// Boxed `Send` future returned by the provider traits.
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a session is started with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSpec {
    pub agent_id: AgentId,
    /// Working directory (the workspace path).
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub mode: Option<String>,
    /// Provider session (Claude session id / Codex thread id) to continue.
    pub resume_session_id: Option<String>,
}

/// Normalized output of a provider session.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderEvent {
    /// The provider assigned (or confirmed) its session id.
    SessionStarted { session_id: String },
    /// A normalized event recorded as-is in the agent's log.
    Event(EventKind),
    /// The provider asks the user to approve an action.
    PermissionRequested {
        provider_request_id: String,
        kind: PermissionKind,
        summary: String,
        raw: Value,
    },
    /// The current turn ended.
    TurnCompleted { usage: Option<Value> },
    /// The provider process ended unexpectedly.
    Exited { success: bool, stderr_tail: String },
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("failed to start provider: {0}")]
    Spawn(String),
    #[error("provider I/O failed: {0}")]
    Io(String),
    #[error("provider protocol error: {0}")]
    Protocol(String),
    #[error("provider session has exited")]
    Exited,
    /// The named call did not complete within its bound.
    #[error("provider {0} timed out")]
    TimedOut(&'static str),
}

pub trait ProviderAdapter: Send + Sync {
    fn kind(&self) -> ProviderKind;

    /// Check `mode`; `Ok(true)` when it lets the provider act without asking.
    fn validate_mode(&self, mode: Option<&str>) -> Result<bool, String>;

    /// Start a new session, or resume `spec.resume_session_id`. Must not hang:
    /// a provider that cannot start returns an error in bounded time.
    fn start(&self, spec: AgentSpec)
    -> BoxFut<'_, Result<Box<dyn ProviderSession>, ProviderError>>;
}

pub trait ProviderSession: Send {
    /// Start a new turn with `text` (the agent is idle).
    fn send_user<'a>(&'a mut self, text: &'a str) -> BoxFut<'a, Result<(), ProviderError>>;

    /// Add `text` to the running turn. `Ok(false)` when the provider cannot
    /// steer; nothing was sent then.
    fn steer<'a>(&'a mut self, text: &'a str) -> BoxFut<'a, Result<bool, ProviderError>>;

    /// Stop the running turn (see the module contract).
    fn interrupt(&mut self) -> BoxFut<'_, Result<(), ProviderError>>;

    fn respond_permission<'a>(
        &'a mut self,
        provider_request_id: &'a str,
        d: Decision,
    ) -> BoxFut<'a, Result<(), ProviderError>>;

    /// The session's event stream.
    fn events(&mut self) -> &mut mpsc::Receiver<ProviderEvent>;

    /// End the session and its process; no `Exited` event is expected after.
    fn shutdown(&mut self) -> BoxFut<'_, Result<(), ProviderError>>;
}
