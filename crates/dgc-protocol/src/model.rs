//! Domain types exchanged over the socket.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{AgentId, PermissionId, ProjectId, WorkspaceId};
use crate::{MAX_PERMISSION_RAW_BYTES, truncate_summary};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Claude,
    Codex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    Local,
    Worktree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceState {
    Active,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Starting,
    Idle,
    Running,
    WaitingPermission,
    Stopped,
    Failed,
    Archived,
}

/// How `agent.send` treats a message when the agent is busy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendBehavior {
    #[default]
    Queue,
    Steer,
    Interrupt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendOutcome {
    Sent,
    Queued,
    Steered,
    InterruptedAndSent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionKind {
    Command,
    FileChange,
    Tool,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    AllowForSession,
    Deny,
}

/// How a permission request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    Decided(Decision),
    Cancelled,
}

/// Times are RFC3339 UTC strings produced by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub path: PathBuf,
    pub name: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub project_id: ProjectId,
    pub isolation: Isolation,
    pub path: PathBuf,
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub state: WorkspaceState,
    /// True when dgc created the directory and may remove it on archive.
    pub owned: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub id: AgentId,
    pub workspace_id: WorkspaceId,
    pub title: String,
    pub provider: ProviderKind,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub auto_approve: bool,
    pub provider_session_id: Option<String>,
    pub state: AgentState,
    pub parent_id: Option<AgentId>,
    pub labels: BTreeMap<String, String>,
    pub created_at: String,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingPermission {
    pub id: PermissionId,
    pub agent_id: AgentId,
    pub kind: PermissionKind,
    pub summary: String,
    /// Provider payload; replaced by `{"truncated":true}` past 64 KiB serialized.
    pub raw: Value,
    pub created_at: String,
}

impl PendingPermission {
    /// Build a request, enforcing the summary (4 KiB) and `raw` (64 KiB) limits.
    pub fn new(
        id: PermissionId,
        agent_id: AgentId,
        kind: PermissionKind,
        summary: String,
        raw: Value,
        created_at: String,
    ) -> Self {
        Self {
            id,
            agent_id,
            kind,
            summary: truncate_summary(&summary),
            raw: limit_raw(raw),
            created_at,
        }
    }
}

fn limit_raw(raw: Value) -> Value {
    // Serialization of a `Value` cannot fail; if it somehow did, drop the payload.
    match serde_json::to_vec(&raw) {
        Ok(bytes) if bytes.len() <= MAX_PERMISSION_RAW_BYTES => raw,
        _ => serde_json::json!({"truncated": true}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MAX_SUMMARY_BYTES;

    #[test]
    fn agent_state_roundtrip_snake_case() {
        let json = serde_json::to_string(&AgentState::WaitingPermission).expect("serialize");
        assert_eq!(json, "\"waiting_permission\"");
        let back: AgentState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, AgentState::WaitingPermission);
    }

    #[test]
    fn provider_and_isolation_serde_names() {
        assert_eq!(
            serde_json::to_string(&ProviderKind::Claude).unwrap(),
            "\"claude\""
        );
        assert_eq!(
            serde_json::to_string(&ProviderKind::Codex).unwrap(),
            "\"codex\""
        );
        assert_eq!(
            serde_json::to_string(&Isolation::Worktree).unwrap(),
            "\"worktree\""
        );
        assert_eq!(
            serde_json::to_string(&WorkspaceState::Archived).unwrap(),
            "\"archived\""
        );
    }

    #[test]
    fn send_behavior_defaults_to_queue() {
        assert_eq!(SendBehavior::default(), SendBehavior::Queue);
        assert_eq!(
            serde_json::to_string(&SendOutcome::InterruptedAndSent).unwrap(),
            "\"interrupted_and_sent\""
        );
    }

    #[test]
    fn resolution_serde_shape() {
        let decided = serde_json::to_value(Resolution::Decided(Decision::AllowForSession)).unwrap();
        assert_eq!(decided, serde_json::json!({"decided": "allow_for_session"}));
        let cancelled = serde_json::to_value(Resolution::Cancelled).unwrap();
        assert_eq!(cancelled, serde_json::json!("cancelled"));
        let back: Resolution = serde_json::from_value(decided).unwrap();
        assert_eq!(back, Resolution::Decided(Decision::AllowForSession));
    }

    #[test]
    fn permission_raw_is_truncated_over_64k() {
        let big = serde_json::json!({"blob": "x".repeat(MAX_PERMISSION_RAW_BYTES)});
        let p = PendingPermission::new(
            PermissionId::generate(),
            AgentId::generate(),
            PermissionKind::Command,
            "run".to_owned(),
            big,
            "2026-11-25T00:00:00Z".to_owned(),
        );
        assert_eq!(p.raw, serde_json::json!({"truncated": true}));
    }

    #[test]
    fn permission_raw_within_limit_is_kept_and_summary_is_cut() {
        let raw = serde_json::json!({"cmd": "ls"});
        let p = PendingPermission::new(
            PermissionId::generate(),
            AgentId::generate(),
            PermissionKind::Tool,
            "s".repeat(MAX_SUMMARY_BYTES + 10),
            raw.clone(),
            "2026-11-25T00:00:00Z".to_owned(),
        );
        assert_eq!(p.raw, raw);
        assert!(p.summary.len() <= MAX_SUMMARY_BYTES);
        assert!(p.summary.ends_with("bytes total)"), "{}", p.summary);
    }

    #[test]
    fn agent_roundtrip_with_optional_fields_absent() {
        let json = serde_json::json!({
            "id": AgentId::generate(),
            "workspace_id": WorkspaceId::generate(),
            "title": "t",
            "provider": "codex",
            "model": null,
            "mode": null,
            "auto_approve": false,
            "provider_session_id": null,
            "state": "starting",
            "parent_id": null,
            "labels": {"k": "v"},
            "created_at": "2026-11-25T00:00:00Z",
            "last_error": null
        });
        let agent: Agent = serde_json::from_value(json.clone()).expect("deserialize");
        assert_eq!(agent.labels.get("k").map(String::as_str), Some("v"));
        assert_eq!(serde_json::to_value(&agent).unwrap(), json);
    }
}
