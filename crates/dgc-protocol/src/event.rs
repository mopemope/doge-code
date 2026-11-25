//! Agent events: the append-only record of what an agent did.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{AgentId, PermissionId};
use crate::model::{AgentState, PendingPermission, Resolution, SendBehavior};
use crate::truncate_summary;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEvent {
    pub agent_id: AgentId,
    pub seq: u64,
    /// RFC3339 UTC.
    pub at: String,
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    TurnStarted,
    AssistantText {
        text: String,
        #[serde(rename = "final")]
        final_: bool,
    },
    ToolCall {
        name: String,
        summary: String,
    },
    ToolResult {
        summary: String,
        is_error: bool,
    },
    PermissionRequested {
        permission: PendingPermission,
    },
    PermissionResolved {
        id: PermissionId,
        resolution: Resolution,
    },
    TurnCompleted {
        usage: Option<Value>,
    },
    RateLimited {
        detail: Value,
    },
    Error {
        message: String,
    },
    StateChanged {
        state: AgentState,
    },
    UserMessage {
        text: String,
        behavior: SendBehavior,
    },
}

impl EventKind {
    /// `ToolCall` with `summary` cut to the 4 KiB limit.
    pub fn tool_call(name: impl Into<String>, summary: &str) -> Self {
        Self::ToolCall {
            name: name.into(),
            summary: truncate_summary(summary),
        }
    }

    /// `ToolResult` with `summary` cut to the 4 KiB limit.
    pub fn tool_result(summary: &str, is_error: bool) -> Self {
        Self::ToolResult {
            summary: truncate_summary(summary),
            is_error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::AgentId;
    use crate::model::{AgentState, SendBehavior};
    use serde_json::json;

    #[test]
    fn event_kind_serializes_with_type_tag() {
        let v = serde_json::to_value(EventKind::AssistantText {
            text: "a".to_owned(),
            final_: true,
        })
        .unwrap();
        assert_eq!(
            v,
            json!({"type": "assistant_text", "text": "a", "final": true})
        );
        let back: EventKind = serde_json::from_value(v).unwrap();
        assert_eq!(
            back,
            EventKind::AssistantText {
                text: "a".to_owned(),
                final_: true
            }
        );
    }

    #[test]
    fn unit_and_struct_variants_use_snake_case_tags() {
        assert_eq!(
            serde_json::to_value(EventKind::TurnStarted).unwrap(),
            json!({"type": "turn_started"})
        );
        let v = serde_json::to_value(EventKind::StateChanged {
            state: AgentState::Idle,
        })
        .unwrap();
        assert_eq!(v, json!({"type": "state_changed", "state": "idle"}));
        let v = serde_json::to_value(EventKind::UserMessage {
            text: "x".to_owned(),
            behavior: SendBehavior::Steer,
        })
        .unwrap();
        assert_eq!(
            v,
            json!({"type": "user_message", "text": "x", "behavior": "steer"})
        );
    }

    #[test]
    fn tool_summaries_are_truncated_to_4k() {
        let long = "y".repeat(10_000);
        match EventKind::tool_call("Bash", &long) {
            EventKind::ToolCall { name, summary } => {
                assert_eq!(name, "Bash");
                assert_eq!(summary.len(), 4096);
            }
            other => panic!("unexpected {other:?}"),
        }
        match EventKind::tool_result(&long, true) {
            EventKind::ToolResult { summary, is_error } => {
                assert_eq!(summary.len(), 4096);
                assert!(is_error);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn agent_event_roundtrip() {
        let ev = AgentEvent {
            agent_id: AgentId::generate(),
            seq: 3,
            at: "2026-11-25T00:00:00Z".to_owned(),
            kind: EventKind::TurnCompleted {
                usage: Some(json!({"input_tokens": 1})),
            },
        };
        let back: AgentEvent = serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert_eq!(back, ev);
    }
}
