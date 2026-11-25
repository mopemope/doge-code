//! Method names, notification names, and request parameter types.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, PermissionId, ProjectId, WorkspaceId};
use crate::model::{Decision, Isolation, ProviderKind, SendBehavior};

pub const HELLO: &str = "hello";
pub const DAEMON_STATUS: &str = "daemon.status";
pub const DAEMON_SHUTDOWN: &str = "daemon.shutdown";
pub const PROJECT_ADD: &str = "project.add";
pub const PROJECT_LIST: &str = "project.list";
pub const PROJECT_REMOVE: &str = "project.remove";
pub const WORKSPACE_CREATE: &str = "workspace.create";
pub const WORKSPACE_LIST: &str = "workspace.list";
pub const WORKSPACE_ARCHIVE: &str = "workspace.archive";
pub const AGENT_CREATE: &str = "agent.create";
pub const AGENT_LIST: &str = "agent.list";
pub const AGENT_GET: &str = "agent.get";
pub const AGENT_SEND: &str = "agent.send";
pub const AGENT_INTERRUPT: &str = "agent.interrupt";
pub const AGENT_ARCHIVE: &str = "agent.archive";
pub const AGENT_RESUME: &str = "agent.resume";
pub const AGENT_EVENTS: &str = "agent.events";
pub const PERMISSION_LIST: &str = "permission.list";
pub const PERMISSION_RESPOND: &str = "permission.respond";
pub const SUBSCRIBE: &str = "subscribe";
pub const UNSUBSCRIBE: &str = "unsubscribe";

/// Notification carrying one `AgentEvent` as its params.
pub const NOTIFY_EVENT: &str = "event";
/// Notification (`LaggedParams`) sent when a subscriber fell behind.
pub const NOTIFY_LAGGED: &str = "lagged";

/// Largest page `agent.events` returns.
pub const MAX_EVENTS_LIMIT: u32 = 1000;

fn default_events_limit() -> u32 {
    MAX_EVENTS_LIMIT
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloParams {
    pub protocol_version: u32,
    pub client: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectAddParams {
    /// Directory to register; the daemon canonicalizes it.
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRemoveParams {
    pub id: ProjectId,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorkspaceListParams {
    /// Only workspaces of this project; all projects when absent.
    #[serde(default)]
    pub project_id: Option<ProjectId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceArchiveParams {
    pub id: WorkspaceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCreateParams {
    pub project_id: ProjectId,
    pub isolation: Isolation,
    /// New branch name; required when `isolation` is `worktree`.
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub base_ref: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCreateParams {
    pub workspace_id: WorkspaceId,
    pub title: String,
    pub provider: ProviderKind,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    pub initial_prompt: String,
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub parent_id: Option<AgentId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSendParams {
    pub agent_id: AgentId,
    pub text: String,
    #[serde(default)]
    pub behavior: SendBehavior,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEventsParams {
    pub agent_id: AgentId,
    #[serde(default)]
    pub since_seq: u64,
    #[serde(default = "default_events_limit")]
    pub limit: u32,
}

impl AgentEventsParams {
    /// `limit` capped at [`MAX_EVENTS_LIMIT`].
    pub fn clamped_limit(&self) -> u32 {
        self.limit.min(MAX_EVENTS_LIMIT)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRespondParams {
    pub id: PermissionId,
    pub decision: Decision,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SubscribeParams {
    #[serde(default)]
    pub agent_id: Option<AgentId>,
    #[serde(default)]
    pub since_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaggedParams {
    pub agent_id: AgentId,
    pub next_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub kind: ProviderKind,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    /// True when the detected version is one dgc has been tested against.
    pub tested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub pid: u32,
    pub protocol_version: u32,
    pub started_at: String,
    pub providers: Vec<ProviderStatus>,
    pub path_env: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{AgentId, ProjectId};
    use crate::model::{Isolation, SendBehavior};

    #[test]
    fn method_names_are_wire_stable() {
        assert_eq!(HELLO, "hello");
        assert_eq!(DAEMON_STATUS, "daemon.status");
        assert_eq!(AGENT_EVENTS, "agent.events");
        assert_eq!(PERMISSION_RESPOND, "permission.respond");
        assert_eq!(NOTIFY_EVENT, "event");
        assert_eq!(NOTIFY_LAGGED, "lagged");
    }

    #[test]
    fn agent_send_behavior_defaults_to_queue() {
        let id = AgentId::generate();
        let p: AgentSendParams =
            serde_json::from_value(serde_json::json!({"agent_id": id, "text": "hi"})).unwrap();
        assert_eq!(p.behavior, SendBehavior::Queue);
    }

    #[test]
    fn agent_events_limit_defaults_and_clamps() {
        let id = AgentId::generate();
        let p: AgentEventsParams =
            serde_json::from_value(serde_json::json!({"agent_id": id})).unwrap();
        assert_eq!(p.since_seq, 0);
        assert_eq!(p.limit, MAX_EVENTS_LIMIT);
        let big = AgentEventsParams {
            limit: 5000,
            ..p.clone()
        };
        assert_eq!(big.clamped_limit(), 1000);
        let small = AgentEventsParams { limit: 10, ..p };
        assert_eq!(small.clamped_limit(), 10);
    }

    #[test]
    fn workspace_create_params_optional_fields_default_to_none() {
        let p: WorkspaceCreateParams = serde_json::from_value(
            serde_json::json!({"project_id": ProjectId::generate(), "isolation": "local"}),
        )
        .unwrap();
        assert_eq!(p.isolation, Isolation::Local);
        assert!(p.branch.is_none() && p.base_ref.is_none() && p.slug.is_none());
    }

    #[test]
    fn project_and_workspace_params_roundtrip() {
        let add: ProjectAddParams =
            serde_json::from_value(serde_json::json!({"path": "/work/repo"})).unwrap();
        assert_eq!(add.path, PathBuf::from("/work/repo"));
        let list: WorkspaceListParams = serde_json::from_str("{}").unwrap();
        assert!(list.project_id.is_none());
        let id = WorkspaceId::generate();
        let archive: WorkspaceArchiveParams =
            serde_json::from_value(serde_json::json!({"id": id})).unwrap();
        assert_eq!(archive.id, id);
        let pid = ProjectId::generate();
        let remove: ProjectRemoveParams =
            serde_json::from_value(serde_json::json!({"id": pid})).unwrap();
        assert_eq!(remove.id, pid);
    }

    #[test]
    fn subscribe_params_accept_empty_object() {
        let p: SubscribeParams = serde_json::from_str("{}").unwrap();
        assert!(p.agent_id.is_none() && p.since_seq.is_none());
    }
}
