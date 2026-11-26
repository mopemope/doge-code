//! Terminal UI for `dgc-next`.
//!
//! This module currently holds the pure model: [`AppState`] and
//! [`update::update`], which turns an [`AppEvent`] into state changes and
//! [`Effect`]s for the runtime to execute. Rendering and I/O live elsewhere.
// removed in M2 Task 4 (the runtime wires these items up)
#![allow(dead_code)]

pub mod input;
pub mod state;
pub mod update;

use dgc_protocol::event::AgentEvent;
use dgc_protocol::ids::{AgentId, PermissionId};
use dgc_protocol::model::{Agent, PendingPermission, Project, Workspace};
use input::KeyInput;

/// State of the connection to the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Connected,
    /// The reason is stored escaped (safe to show).
    Disconnected {
        reason: String,
    },
    /// Trying to (re)connect; the initial state before the first connection.
    Reconnecting,
}

/// What an RPC call was for, so [`AppEvent::RpcDone`] can update the state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestTag {
    Send { agent_id: AgentId },
    Respond { permission_id: PermissionId },
    CreateAgent,
    CreateWorkspace,
    Interrupt { agent_id: AgentId },
    Resume { agent_id: AgentId },
    Archive { agent_id: AgentId },
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppEvent {
    Key(KeyInput),
    Snapshot {
        projects: Vec<Project>,
        workspaces: Vec<Workspace>,
        agents: Vec<Agent>,
        permissions: Vec<PendingPermission>,
    },
    /// A live event from the subscription.
    AgentEvent(AgentEvent),
    /// One `agent.events` page; `done` when the page was empty.
    History {
        agent_id: AgentId,
        events: Vec<AgentEvent>,
        done: bool,
    },
    RpcDone {
        request: RequestTag,
        result: Result<serde_json::Value, String>,
    },
    Connection(ConnectionState),
    /// The subscription ended because it fell behind.
    Lagged,
    Resize {
        width: u16,
        height: u16,
    },
    Tick,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    LoadSnapshot,
    LoadHistory {
        agent_id: AgentId,
        since_seq: u64,
    },
    Call {
        request: RequestTag,
        method: &'static str,
        params: serde_json::Value,
    },
    Resubscribe,
    Quit,
}
