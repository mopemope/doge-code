use serde::{Deserialize, Serialize};

use super::wire::v1 as wire_v1;
use super::wire::v2 as wire_v2;

/// Current on-disk schema version for newly written provenance events.
pub const PROVENANCE_SCHEMA_VERSION: u32 = 2;
/// Legacy read-only schema version. v1 files are converted on read and are
/// never physically migrated.
pub const LEGACY_PROVENANCE_SCHEMA_VERSION: u32 = 1;

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

/// A committed workspace mutation (v2 canonical).
///
/// This is a Doge-observed `before -> after` transaction, not an LLM
/// self-report: `before`/`after` are exact file states and `diff` is
/// generated from them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeCommittedEvent {
    pub transaction_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_item_id: Option<String>,
    pub change_kind: ChangeKind,
    /// Project-relative `/`-normalized path. Never an absolute path.
    pub file: String,
    pub target: ChangeTarget,
    pub before: FileStateEvidence,
    pub after: FileStateEvidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predecessor_change_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverts_change_id: Option<String>,
    #[serde(default)]
    pub diff: String,
    #[serde(default)]
    pub diff_hash: String,
    #[serde(default)]
    pub lines_added: usize,
    #[serde(default)]
    pub lines_removed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    SemanticEdit,
    TextEdit,
    ApplyPatch,
    FileWrite,
    Undo,
}

impl ChangeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SemanticEdit => "semantic_edit",
            Self::TextEdit => "text_edit",
            Self::ApplyPatch => "apply_patch",
            Self::FileWrite => "file_write",
            Self::Undo => "undo",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum ChangeTarget {
    File,
    SemanticSymbol {
        symbol_id: String,
        before_fingerprint: String,
        after_fingerprint: String,
    },
}

/// Exact whole-file state evidence.
///
/// `content_hash` is `blake3:<hex>` over the exact UTF-8 bytes (no newline
/// normalization; `LF != CRLF`). It is distinct from the semantic
/// `ContentFingerprint`, which may normalize for stale detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileStateEvidence {
    #[serde(default)]
    pub exists: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_len: Option<u64>,
}

impl FileStateEvidence {
    pub fn missing() -> Self {
        Self {
            exists: false,
            content_hash: None,
            byte_len: None,
        }
    }

    pub fn state_matches(&self, other: &Self) -> bool {
        if self.exists != other.exists {
            return false;
        }
        if !self.exists {
            return true;
        }
        self.content_hash == other.content_hash
    }
}

/// Exact-byte file identity hash: `blake3:<64 hex>` over UTF-8 bytes.
pub fn file_content_hash(content: &str) -> String {
    format!("blake3:{}", blake3::hash(content.as_bytes()).to_hex())
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

// ---------------------------------------------------------------------------
// Wire conversions (explicit, no implicit serde evolution)
// ---------------------------------------------------------------------------

/// Convert a v1 wire envelope into the canonical representation.
///
/// v1 changes were semantic-edit only. Whole-file hashes did not exist, so
/// `before`/`after` carry `content_hash = None` with `exists = true`
/// (a semantic edit always rewrote an existing file).
pub fn from_v1_wire(env: wire_v1::V1Envelope) -> ProvenanceEventEnvelope {
    let event = match env.event {
        wire_v1::V1Event::PlanChanged(p) => ProvenanceEvent::PlanChanged(PlanChangedEvent {
            changes: p
                .changes
                .into_iter()
                .map(|c| PlanItemTransition {
                    plan_item_id: c.plan_item_id,
                    parent_id: c.parent_id,
                    content: c.content,
                    before_status: c.before_status,
                    after_status: c.after_status,
                })
                .collect(),
        }),
        wire_v1::V1Event::ChangeCommitted(c) => {
            ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                transaction_id: c.transaction_id,
                plan_item_id: c.plan_item_id,
                change_kind: ChangeKind::SemanticEdit,
                file: c.file,
                target: ChangeTarget::SemanticSymbol {
                    symbol_id: c.symbol_id,
                    before_fingerprint: c.before_fingerprint,
                    after_fingerprint: c.after_fingerprint,
                },
                before: FileStateEvidence {
                    exists: true,
                    content_hash: None,
                    byte_len: None,
                },
                after: FileStateEvidence {
                    exists: true,
                    content_hash: None,
                    byte_len: None,
                },
                predecessor_change_id: None,
                reverts_change_id: None,
                diff: c.diff,
                diff_hash: c.diff_hash,
                lines_added: c.lines_added,
                lines_removed: c.lines_removed,
            })
        }
        wire_v1::V1Event::VerificationObserved(v) => {
            ProvenanceEvent::VerificationObserved(VerificationObservedEvent {
                plan_item_id: v.plan_item_id,
                verification_kind: match v.verification_kind {
                    wire_v1::V1VerificationKind::Test => VerificationKind::Test,
                    wire_v1::V1VerificationKind::Build => VerificationKind::Build,
                    wire_v1::V1VerificationKind::Lint => VerificationKind::Lint,
                    wire_v1::V1VerificationKind::TypeCheck => VerificationKind::TypeCheck,
                    wire_v1::V1VerificationKind::FormatCheck => VerificationKind::FormatCheck,
                    wire_v1::V1VerificationKind::SyntaxCheck => VerificationKind::SyntaxCheck,
                },
                source: match v.source {
                    wire_v1::V1VerificationSource::ExecuteProcess => {
                        VerificationSource::ExecuteProcess
                    }
                    wire_v1::V1VerificationSource::TuiTest => VerificationSource::TuiTest,
                    wire_v1::V1VerificationSource::TuiLint => VerificationSource::TuiLint,
                },
                command: CommandEvidence {
                    program: v.command.program,
                    args: v.command.args,
                    cwd: v.command.cwd,
                },
                outcome: VerificationOutcome {
                    success: v.outcome.success,
                    status: v.outcome.status,
                    exit_code: v.outcome.exit_code,
                    timed_out: v.outcome.timed_out,
                },
                observed_change_ids: v.observed_change_ids,
                stdout_excerpt: v.stdout_excerpt,
                stderr_excerpt: v.stderr_excerpt,
                output_digest: v.output_digest,
                output_truncated: v.output_truncated,
                warnings: v.warnings,
            })
        }
    };
    ProvenanceEventEnvelope {
        schema_version: PROVENANCE_SCHEMA_VERSION,
        event_id: env.event_id,
        session_id: env.session_id,
        timestamp: env.timestamp,
        event,
    }
}

/// Convert a v2 wire envelope into the canonical representation.
pub fn from_v2_wire(env: wire_v2::V2Envelope) -> ProvenanceEventEnvelope {
    let event = match env.event {
        wire_v2::V2Event::PlanChanged(p) => ProvenanceEvent::PlanChanged(PlanChangedEvent {
            changes: p
                .changes
                .into_iter()
                .map(|c| PlanItemTransition {
                    plan_item_id: c.plan_item_id,
                    parent_id: c.parent_id,
                    content: c.content,
                    before_status: c.before_status,
                    after_status: c.after_status,
                })
                .collect(),
        }),
        wire_v2::V2Event::ChangeCommitted(c) => {
            ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                transaction_id: c.transaction_id,
                plan_item_id: c.plan_item_id,
                change_kind: match c.change_kind {
                    wire_v2::V2ChangeKind::SemanticEdit => ChangeKind::SemanticEdit,
                    wire_v2::V2ChangeKind::TextEdit => ChangeKind::TextEdit,
                    wire_v2::V2ChangeKind::ApplyPatch => ChangeKind::ApplyPatch,
                    wire_v2::V2ChangeKind::FileWrite => ChangeKind::FileWrite,
                    wire_v2::V2ChangeKind::Undo => ChangeKind::Undo,
                },
                file: c.file,
                target: match c.target {
                    wire_v2::V2ChangeTarget::File => ChangeTarget::File,
                    wire_v2::V2ChangeTarget::SemanticSymbol {
                        symbol_id,
                        before_fingerprint,
                        after_fingerprint,
                    } => ChangeTarget::SemanticSymbol {
                        symbol_id,
                        before_fingerprint,
                        after_fingerprint,
                    },
                },
                before: FileStateEvidence {
                    exists: c.before.exists,
                    content_hash: c.before.content_hash,
                    byte_len: c.before.byte_len,
                },
                after: FileStateEvidence {
                    exists: c.after.exists,
                    content_hash: c.after.content_hash,
                    byte_len: c.after.byte_len,
                },
                predecessor_change_id: c.predecessor_change_id,
                reverts_change_id: c.reverts_change_id,
                diff: c.diff,
                diff_hash: c.diff_hash,
                lines_added: c.lines_added,
                lines_removed: c.lines_removed,
            })
        }
        wire_v2::V2Event::VerificationObserved(v) => {
            ProvenanceEvent::VerificationObserved(VerificationObservedEvent {
                plan_item_id: v.plan_item_id,
                verification_kind: match v.verification_kind {
                    wire_v2::V2VerificationKind::Test => VerificationKind::Test,
                    wire_v2::V2VerificationKind::Build => VerificationKind::Build,
                    wire_v2::V2VerificationKind::Lint => VerificationKind::Lint,
                    wire_v2::V2VerificationKind::TypeCheck => VerificationKind::TypeCheck,
                    wire_v2::V2VerificationKind::FormatCheck => VerificationKind::FormatCheck,
                    wire_v2::V2VerificationKind::SyntaxCheck => VerificationKind::SyntaxCheck,
                },
                source: match v.source {
                    wire_v2::V2VerificationSource::ExecuteProcess => {
                        VerificationSource::ExecuteProcess
                    }
                    wire_v2::V2VerificationSource::TuiTest => VerificationSource::TuiTest,
                    wire_v2::V2VerificationSource::TuiLint => VerificationSource::TuiLint,
                },
                command: CommandEvidence {
                    program: v.command.program,
                    args: v.command.args,
                    cwd: v.command.cwd,
                },
                outcome: VerificationOutcome {
                    success: v.outcome.success,
                    status: v.outcome.status,
                    exit_code: v.outcome.exit_code,
                    timed_out: v.outcome.timed_out,
                },
                observed_change_ids: v.observed_change_ids,
                stdout_excerpt: v.stdout_excerpt,
                stderr_excerpt: v.stderr_excerpt,
                output_digest: v.output_digest,
                output_truncated: v.output_truncated,
                warnings: v.warnings,
            })
        }
    };
    ProvenanceEventEnvelope {
        schema_version: PROVENANCE_SCHEMA_VERSION,
        event_id: env.event_id,
        session_id: env.session_id,
        timestamp: env.timestamp,
        event,
    }
}

/// Convert a canonical envelope into its v2 wire form for persistence.
///
/// All new events are written as v2; v1 is never written.
pub fn to_v2_wire(env: &ProvenanceEventEnvelope) -> wire_v2::V2Envelope {
    let event = match &env.event {
        ProvenanceEvent::PlanChanged(p) => wire_v2::V2Event::PlanChanged(wire_v2::V2PlanChanged {
            changes: p
                .changes
                .iter()
                .map(|c| wire_v2::V2PlanItemTransition {
                    plan_item_id: c.plan_item_id.clone(),
                    parent_id: c.parent_id.clone(),
                    content: c.content.clone(),
                    before_status: c.before_status.clone(),
                    after_status: c.after_status.clone(),
                })
                .collect(),
        }),
        ProvenanceEvent::ChangeCommitted(c) => {
            wire_v2::V2Event::ChangeCommitted(wire_v2::V2ChangeCommitted {
                transaction_id: c.transaction_id.clone(),
                plan_item_id: c.plan_item_id.clone(),
                change_kind: match c.change_kind {
                    ChangeKind::SemanticEdit => wire_v2::V2ChangeKind::SemanticEdit,
                    ChangeKind::TextEdit => wire_v2::V2ChangeKind::TextEdit,
                    ChangeKind::ApplyPatch => wire_v2::V2ChangeKind::ApplyPatch,
                    ChangeKind::FileWrite => wire_v2::V2ChangeKind::FileWrite,
                    ChangeKind::Undo => wire_v2::V2ChangeKind::Undo,
                },
                file: c.file.clone(),
                target: match &c.target {
                    ChangeTarget::File => wire_v2::V2ChangeTarget::File,
                    ChangeTarget::SemanticSymbol {
                        symbol_id,
                        before_fingerprint,
                        after_fingerprint,
                    } => wire_v2::V2ChangeTarget::SemanticSymbol {
                        symbol_id: symbol_id.clone(),
                        before_fingerprint: before_fingerprint.clone(),
                        after_fingerprint: after_fingerprint.clone(),
                    },
                },
                before: wire_v2::V2FileStateEvidence {
                    exists: c.before.exists,
                    content_hash: c.before.content_hash.clone(),
                    byte_len: c.before.byte_len,
                },
                after: wire_v2::V2FileStateEvidence {
                    exists: c.after.exists,
                    content_hash: c.after.content_hash.clone(),
                    byte_len: c.after.byte_len,
                },
                predecessor_change_id: c.predecessor_change_id.clone(),
                reverts_change_id: c.reverts_change_id.clone(),
                diff: c.diff.clone(),
                diff_hash: c.diff_hash.clone(),
                lines_added: c.lines_added,
                lines_removed: c.lines_removed,
            })
        }
        ProvenanceEvent::VerificationObserved(v) => {
            wire_v2::V2Event::VerificationObserved(wire_v2::V2VerificationObserved {
                plan_item_id: v.plan_item_id.clone(),
                verification_kind: match v.verification_kind {
                    VerificationKind::Test => wire_v2::V2VerificationKind::Test,
                    VerificationKind::Build => wire_v2::V2VerificationKind::Build,
                    VerificationKind::Lint => wire_v2::V2VerificationKind::Lint,
                    VerificationKind::TypeCheck => wire_v2::V2VerificationKind::TypeCheck,
                    VerificationKind::FormatCheck => wire_v2::V2VerificationKind::FormatCheck,
                    VerificationKind::SyntaxCheck => wire_v2::V2VerificationKind::SyntaxCheck,
                },
                source: match v.source {
                    VerificationSource::ExecuteProcess => {
                        wire_v2::V2VerificationSource::ExecuteProcess
                    }
                    VerificationSource::TuiTest => wire_v2::V2VerificationSource::TuiTest,
                    VerificationSource::TuiLint => wire_v2::V2VerificationSource::TuiLint,
                },
                command: wire_v2::V2CommandEvidence {
                    program: v.command.program.clone(),
                    args: v.command.args.clone(),
                    cwd: v.command.cwd.clone(),
                },
                outcome: wire_v2::V2VerificationOutcome {
                    success: v.outcome.success,
                    status: v.outcome.status.clone(),
                    exit_code: v.outcome.exit_code,
                    timed_out: v.outcome.timed_out,
                },
                observed_change_ids: v.observed_change_ids.clone(),
                stdout_excerpt: v.stdout_excerpt.clone(),
                stderr_excerpt: v.stderr_excerpt.clone(),
                output_digest: v.output_digest.clone(),
                output_truncated: v.output_truncated,
                warnings: v.warnings.clone(),
            })
        }
    };
    wire_v2::V2Envelope {
        schema_version: PROVENANCE_SCHEMA_VERSION,
        event_id: env.event_id.clone(),
        session_id: env.session_id.clone(),
        timestamp: env.timestamp.clone(),
        event,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn semantic_change() -> ChangeCommittedEvent {
        ChangeCommittedEvent {
            transaction_id: "evt-1".to_string(),
            plan_item_id: Some("step-2".to_string()),
            change_kind: ChangeKind::SemanticEdit,
            file: "src/auth.rs".to_string(),
            target: ChangeTarget::SemanticSymbol {
                symbol_id: "sym-v1-abc".to_string(),
                before_fingerprint: "fp-v1-a".to_string(),
                after_fingerprint: "fp-v1-b".to_string(),
            },
            before: FileStateEvidence {
                exists: true,
                content_hash: Some("blake3:aaa".to_string()),
                byte_len: Some(10),
            },
            after: FileStateEvidence {
                exists: true,
                content_hash: Some("blake3:bbb".to_string()),
                byte_len: Some(12),
            },
            predecessor_change_id: None,
            reverts_change_id: None,
            diff: "diff".to_string(),
            diff_hash: "blake3:abc".to_string(),
            lines_added: 1,
            lines_removed: 0,
        }
    }

    #[test]
    fn test_envelope_change_id_points_at_event_id() {
        let env = ProvenanceEventEnvelope {
            schema_version: PROVENANCE_SCHEMA_VERSION,
            event_id: "evt-1".to_string(),
            session_id: "sess".to_string(),
            timestamp: "2026-01-01T00:00:00+00:00".to_string(),
            event: ProvenanceEvent::ChangeCommitted(semantic_change()),
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
        let json =
            serde_json::to_value(ProvenanceEvent::ChangeCommitted(semantic_change())).unwrap();
        assert_eq!(json["type"], "change_committed");
    }

    #[test]
    fn test_change_kind_snake_case_wire_values() {
        for (kind, wire) in [
            (ChangeKind::SemanticEdit, "semantic_edit"),
            (ChangeKind::TextEdit, "text_edit"),
            (ChangeKind::ApplyPatch, "apply_patch"),
            (ChangeKind::FileWrite, "file_write"),
            (ChangeKind::Undo, "undo"),
        ] {
            let v = serde_json::to_value(kind).unwrap();
            assert_eq!(v, serde_json::Value::String(wire.to_string()));
            assert_eq!(kind.as_str(), wire);
        }
    }

    #[test]
    fn test_change_target_scope_tags() {
        let file = serde_json::to_value(ChangeTarget::File).unwrap();
        assert_eq!(file["scope"], "file");
        let sym = serde_json::to_value(ChangeTarget::SemanticSymbol {
            symbol_id: "s".to_string(),
            before_fingerprint: "a".to_string(),
            after_fingerprint: "b".to_string(),
        })
        .unwrap();
        assert_eq!(sym["scope"], "semantic_symbol");
    }

    #[test]
    fn test_file_content_hash_is_exact_bytes() {
        // LF != CRLF for file identity (no normalization).
        assert_ne!(file_content_hash("a\n"), file_content_hash("a\r\n"));
        assert!(file_content_hash("x").starts_with("blake3:"));
        assert_eq!(file_content_hash("x").len(), "blake3:".len() + 64);
    }

    #[test]
    fn test_v1_semantic_conversion() {
        let v1 = wire_v1::V1Envelope {
            schema_version: 1,
            event_id: "evt-v1".to_string(),
            session_id: "s".to_string(),
            timestamp: "2026-01-01T00:00:00+00:00".to_string(),
            event: wire_v1::V1Event::ChangeCommitted(wire_v1::V1ChangeCommitted {
                transaction_id: String::new(),
                plan_item_id: None,
                change_kind: wire_v1::V1ChangeKind::SemanticEdit,
                file: "src/lib.rs".to_string(),
                symbol_id: "sym-1".to_string(),
                before_fingerprint: "a".to_string(),
                after_fingerprint: "b".to_string(),
                diff: "d".to_string(),
                diff_hash: "blake3:x".to_string(),
                lines_added: 1,
                lines_removed: 0,
            }),
        };
        let canonical = from_v1_wire(v1);
        match &canonical.event {
            ProvenanceEvent::ChangeCommitted(c) => {
                assert_eq!(c.change_kind, ChangeKind::SemanticEdit);
                match &c.target {
                    ChangeTarget::SemanticSymbol {
                        symbol_id,
                        before_fingerprint,
                        after_fingerprint,
                    } => {
                        assert_eq!(symbol_id, "sym-1");
                        assert_eq!(before_fingerprint, "a");
                        assert_eq!(after_fingerprint, "b");
                    }
                    _ => panic!("expected semantic target"),
                }
                assert_eq!(c.before.content_hash, None);
                assert_eq!(c.after.content_hash, None);
                assert!(c.before.exists);
                assert!(c.after.exists);
            }
            _ => panic!("expected change"),
        }
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
