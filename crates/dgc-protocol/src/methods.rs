//! Method names, notification names, and request parameter types.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, PermissionId, ProjectId, WorkspaceId};
use crate::model::{Decision, Isolation, ProviderKind, SendBehavior, Workspace};

pub const HELLO: &str = "hello";
pub const DAEMON_STATUS: &str = "daemon.status";
pub const DAEMON_SHUTDOWN: &str = "daemon.shutdown";
pub const PROJECT_ADD: &str = "project.add";
pub const PROJECT_LIST: &str = "project.list";
pub const PROJECT_REMOVE: &str = "project.remove";
pub const WORKSPACE_CREATE: &str = "workspace.create";
pub const WORKSPACE_LIST: &str = "workspace.list";
pub const WORKSPACE_ARCHIVE: &str = "workspace.archive";
pub const PROFILE_LIST: &str = "profile.list";
pub const AGENT_HANDOFF: &str = "agent.handoff";
/// INVALID_STATE message when the current handoff differs from the preview.
pub const HANDOFF_CHANGED: &str = "handoff_changed";
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

/// Serialized size an `agent.events` page stays within (a single event
/// larger than this is still returned alone). A page can therefore hold fewer
/// than `limit` events while more exist: page on until an empty page.
pub const MAX_EVENTS_PAGE_BYTES: usize = 768 * 1024;

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
    /// Remove an owned worktree even when it has uncommitted changes or
    /// commits not in its base, and force-delete its branch. Without it such
    /// a workspace is refused (`INVALID_STATE`) and nothing is changed.
    #[serde(default)]
    pub force: bool,
}

/// `workspace.archive` result: the archived workspace (its fields at the top
/// level) plus what this call did on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceArchiveResult {
    #[serde(flatten)]
    pub workspace: Workspace,
    /// True when this call removed the owned worktree directory.
    pub worktree_removed: bool,
    /// True when the workspace records a branch that still exists after this
    /// call (git refused the safe delete, or the branch is not provably
    /// dgc's). False when no branch is recorded or it was deleted. Both flags
    /// are false when the workspace was already archived.
    pub branch_kept: bool,
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
    /// Required unless a launch profile supplies the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
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
pub struct ProfileInfo {
    pub name: String,
    pub provider: ProviderKind,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub notes: Option<String>,
    pub auto_approve: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileListResult {
    pub profiles: Vec<ProfileInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHandoffParams {
    pub agent_id: AgentId,
    pub provider: Option<ProviderKind>,
    pub profile: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub dry_run: bool,
    /// Compare the rebuilt prompt with this preview before launching.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_prompt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentHandoffResult {
    pub prompt: String,
    pub agent: Option<crate::model::Agent>,
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
    /// When set, return the newest events, ignoring `since_seq` and `limit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail: Option<u32>,
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

/// `agent.get` / `agent.interrupt` / `agent.archive` / `agent.resume`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentIdParams {
    pub id: AgentId,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgentListParams {
    /// Also list archived agents (default: only the others).
    #[serde(default)]
    pub include_archived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PermissionListParams {
    /// Only requests of this agent; all agents when absent.
    #[serde(default)]
    pub agent_id: Option<AgentId>,
}

/// `unsubscribe`: stop this connection's subscription to `agent_id`, or every
/// subscription of the connection when absent.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UnsubscribeParams {
    #[serde(default)]
    pub agent_id: Option<AgentId>,
}

/// A subscription fell behind and was ended. For a one-agent subscription,
/// `agent_id` is that agent and `next_seq` the first event it did not
/// receive: fetch from there with `agent.events` (or subscribe again with
/// `since_seq = next_seq - 1`). For an all-agents subscription `agent_id` is
/// null and `next_seq` 0: every tracked agent must be caught up from its own
/// last seen seq.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaggedParams {
    #[serde(default)]
    pub agent_id: Option<AgentId>,
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
        assert_eq!(p.tail, None);
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
        assert!(!archive.force, "force defaults to false");
        let pid = ProjectId::generate();
        let remove: ProjectRemoveParams =
            serde_json::from_value(serde_json::json!({"id": pid})).unwrap();
        assert_eq!(remove.id, pid);
    }

    #[test]
    fn agent_param_types_default_optional_fields() {
        let list: AgentListParams = serde_json::from_str("{}").unwrap();
        assert!(!list.include_archived);
        let perms: PermissionListParams = serde_json::from_str("{}").unwrap();
        assert!(perms.agent_id.is_none());
        let unsub: UnsubscribeParams = serde_json::from_str("{}").unwrap();
        assert!(unsub.agent_id.is_none());
        let id = AgentId::generate();
        let get: AgentIdParams = serde_json::from_value(serde_json::json!({"id": id})).unwrap();
        assert_eq!(get.id, id);
        let lagged = serde_json::to_value(LaggedParams {
            agent_id: None,
            next_seq: 0,
        })
        .unwrap();
        assert_eq!(lagged, serde_json::json!({"agent_id": null, "next_seq": 0}));
    }

    #[test]
    fn archive_result_flattens_the_workspace() {
        use crate::model::WorkspaceState;
        let workspace = Workspace {
            id: WorkspaceId::generate(),
            project_id: ProjectId::generate(),
            isolation: Isolation::Worktree,
            path: PathBuf::from("/w/a"),
            branch: Some("b".into()),
            base_ref: None,
            state: WorkspaceState::Archived,
            owned: true,
            created_at: String::new(),
        };
        let result = WorkspaceArchiveResult {
            workspace,
            worktree_removed: true,
            branch_kept: true,
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["state"], "archived");
        assert_eq!(value["branch"], "b");
        assert_eq!(value["worktree_removed"], true);
        assert_eq!(value["branch_kept"], true);
        let back: WorkspaceArchiveResult = serde_json::from_value(value).unwrap();
        assert_eq!(back, result);
    }

    #[test]
    fn subscribe_params_accept_empty_object() {
        let p: SubscribeParams = serde_json::from_str("{}").unwrap();
        assert!(p.agent_id.is_none() && p.since_seq.is_none());
    }
}
