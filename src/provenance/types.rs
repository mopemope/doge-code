use serde::{Deserialize, Serialize};

/// Current on-disk schema version for all provenance events.
pub const PROVENANCE_SCHEMA_VERSION: u32 = 1;

/// Versioned envelope wrapping every provenance event.
///
/// `event_id` is a UUIDv7 string. Query ordering is `(timestamp, event_id)`;
/// never rely on filesystem iteration order or on `event_id` lexical order
/// alone across processes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceEventEnvelope {
    pub schema_version: u32,
    pub event_id: String,
    pub session_id: String,
    pub timestamp: String,
    pub event: ProvenanceEvent,
}

impl ProvenanceEventEnvelope {
    pub fn event_type(&self) -> ProvenanceEventType {
        match &self.event {
            ProvenanceEvent::PlanChanged(_) => ProvenanceEventType::PlanChanged,
            ProvenanceEvent::ChangeCommitted(_) => ProvenanceEventType::ChangeCommitted,
            ProvenanceEvent::VerificationObserved(_) => ProvenanceEventType::VerificationObserved,
        }
    }

    /// Canonical change identity for a `ChangeCommitted` event.
    ///
    /// This is the envelope `event_id`, which is what
    /// `VerificationObserved.observed_change_ids` references. The embedded
    /// `transaction_id` is retained as metadata; when empty at write time the
    /// store backfills it with the generated `event_id` so both agree.
    pub fn change_id(&self) -> Option<&str> {
        match &self.event {
            ProvenanceEvent::ChangeCommitted(_) => Some(self.event_id.as_str()),
            _ => None,
        }
    }

    pub fn plan_item_id(&self) -> Option<&str> {
        match &self.event {
            ProvenanceEvent::PlanChanged(_) => None,
            ProvenanceEvent::ChangeCommitted(e) => e.plan_item_id.as_deref(),
            ProvenanceEvent::VerificationObserved(e) => e.plan_item_id.as_deref(),
        }
    }
}

/// Tagged provenance event. The `type` string is part of the durable contract
/// and must not be renamed casually.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProvenanceEvent {
    PlanChanged(PlanChangedEvent),
    ChangeCommitted(ChangeCommittedEvent),
    VerificationObserved(VerificationObservedEvent),
}

/// Filter value for reads. Mirrors [`ProvenanceEvent`] tag strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceEventType {
    PlanChanged,
    ChangeCommitted,
    VerificationObserved,
}

impl ProvenanceEventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PlanChanged => "plan_changed",
            Self::ChangeCommitted => "change_committed",
            Self::VerificationObserved => "verification_observed",
        }
    }
}

/// One plan write expressed as per-item transitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanChangedEvent {
    pub changes: Vec<PlanItemTransition>,
}

/// Transition of a single plan item.
///
/// New item: `before_status = None`. Deleted item: `after_status = None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItemTransition {
    pub plan_item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_status: Option<String>,
}

/// A committed semantic transaction (v1: transactional semantic edit only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeCommittedEvent {
    pub transaction_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_item_id: Option<String>,
    pub change_kind: ChangeKind,
    /// Project-relative `/`-normalized path. Never an absolute path.
    pub file: String,
    pub symbol_id: String,
    pub before_fingerprint: String,
    pub after_fingerprint: String,
    pub diff: String,
    pub diff_hash: String,
    pub lines_added: usize,
    pub lines_removed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    SemanticEdit,
}

/// An observed verification command run.
///
/// This records that a command was started and finished against a workspace
/// snapshot; it never claims the implementation is proven or guaranteed
/// correct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationObservedEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_item_id: Option<String>,
    pub verification_kind: VerificationKind,
    pub source: VerificationSource,
    pub command: CommandEvidence,
    pub outcome: VerificationOutcome,
    #[serde(default)]
    pub observed_change_ids: Vec<String>,
    #[serde(default)]
    pub stdout_excerpt: String,
    #[serde(default)]
    pub stderr_excerpt: String,
    #[serde(default)]
    pub output_digest: String,
    #[serde(default)]
    pub output_truncated: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationKind {
    Test,
    Build,
    Lint,
    TypeCheck,
    FormatCheck,
    SyntaxCheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationSource {
    ExecuteProcess,
    TuiTest,
    TuiLint,
}

/// Structured argv evidence. Never a shell string; never includes env values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandEvidence {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationOutcome {
    pub success: bool,
    pub status: String,
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub timed_out: bool,
}

/// Snapshot taken before a verification command starts.
///
/// Later changes must never be attributed to an already-running command.
#[derive(Debug, Clone, Default)]
pub struct VerificationContext {
    pub plan_item_id: Option<String>,
    pub observed_change_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_envelope_change_id_points_at_event_id() {
        let env = ProvenanceEventEnvelope {
            schema_version: PROVENANCE_SCHEMA_VERSION,
            event_id: "evt-1".to_string(),
            session_id: "sess".to_string(),
            timestamp: "2026-01-01T00:00:00+00:00".to_string(),
            event: ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                transaction_id: "evt-1".to_string(),
                plan_item_id: Some("step-2".to_string()),
                change_kind: ChangeKind::SemanticEdit,
                file: "src/auth.rs".to_string(),
                symbol_id: "sym-v1-abc".to_string(),
                before_fingerprint: "fp-v1-a".to_string(),
                after_fingerprint: "fp-v1-b".to_string(),
                diff: "diff".to_string(),
                diff_hash: "blake3:abc".to_string(),
                lines_added: 1,
                lines_removed: 0,
            }),
        };
        assert_eq!(env.change_id(), Some("evt-1"));
        assert_eq!(env.plan_item_id(), Some("step-2"));
        assert_eq!(env.event_type(), ProvenanceEventType::ChangeCommitted);
    }

    #[test]
    fn test_event_tag_strings_are_stable() {
        let json = serde_json::to_value(ProvenanceEvent::PlanChanged(PlanChangedEvent {
            changes: vec![],
        }))
        .unwrap();
        assert_eq!(json["type"], "plan_changed");
        let json = serde_json::to_value(ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
            transaction_id: "t".to_string(),
            plan_item_id: None,
            change_kind: ChangeKind::SemanticEdit,
            file: "a.rs".to_string(),
            symbol_id: "s".to_string(),
            before_fingerprint: "a".to_string(),
            after_fingerprint: "b".to_string(),
            diff: String::new(),
            diff_hash: "blake3:x".to_string(),
            lines_added: 0,
            lines_removed: 0,
        }))
        .unwrap();
        assert_eq!(json["type"], "change_committed");
    }

    #[test]
    fn test_verification_kind_snake_case() {
        let kind = serde_json::to_value(VerificationKind::TypeCheck).unwrap();
        assert_eq!(kind, serde_json::Value::String("type_check".to_string()));
        let kind = serde_json::to_value(VerificationKind::FormatCheck).unwrap();
        assert_eq!(kind, serde_json::Value::String("format_check".to_string()));
        let src = serde_json::to_value(VerificationSource::ExecuteProcess).unwrap();
        assert_eq!(
            src,
            serde_json::Value::String("execute_process".to_string())
        );
    }
}
