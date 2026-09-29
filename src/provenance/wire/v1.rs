//! Frozen v1 wire schema (read-only legacy).
//!
//! This mirrors the exact v1 JSON written under
//! `.doge/sessions/<id>/provenance/v1/events/`. Do not modify these shapes
//! to "accept" newer data; v1 files are converted into the canonical v2
//! representation on read.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V1Envelope {
    pub schema_version: u32,
    pub event_id: String,
    pub session_id: String,
    pub timestamp: String,
    pub event: V1Event,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum V1Event {
    PlanChanged(V1PlanChanged),
    ChangeCommitted(V1ChangeCommitted),
    VerificationObserved(V1VerificationObserved),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V1PlanChanged {
    #[serde(default)]
    pub changes: Vec<V1PlanItemTransition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V1PlanItemTransition {
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

/// v1 change: transactional semantic edit only, no whole-file hashes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V1ChangeCommitted {
    #[serde(default)]
    pub transaction_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_item_id: Option<String>,
    pub change_kind: V1ChangeKind,
    pub file: String,
    pub symbol_id: String,
    pub before_fingerprint: String,
    pub after_fingerprint: String,
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
pub enum V1ChangeKind {
    SemanticEdit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V1VerificationObserved {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_item_id: Option<String>,
    pub verification_kind: V1VerificationKind,
    pub source: V1VerificationSource,
    pub command: V1CommandEvidence,
    pub outcome: V1VerificationOutcome,
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
pub enum V1VerificationKind {
    Test,
    Build,
    Lint,
    TypeCheck,
    FormatCheck,
    SyntaxCheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum V1VerificationSource {
    ExecuteProcess,
    TuiTest,
    TuiLint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V1CommandEvidence {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V1VerificationOutcome {
    pub success: bool,
    pub status: String,
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub timed_out: bool,
}
