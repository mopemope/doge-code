//! v2 wire schema (current read + write).
//!
//! Shapes here are intentionally separate structs from the canonical
//! `super::types` representation so durable evolution stays explicit.
//! Conversion between wire and canonical is total and field-by-field.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V2Envelope {
    pub schema_version: u32,
    pub event_id: String,
    pub session_id: String,
    pub timestamp: String,
    pub event: V2Event,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum V2Event {
    PlanChanged(V2PlanChanged),
    ChangeCommitted(V2ChangeCommitted),
    VerificationObserved(V2VerificationObserved),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V2PlanChanged {
    #[serde(default)]
    pub changes: Vec<V2PlanItemTransition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V2PlanItemTransition {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V2ChangeCommitted {
    #[serde(default)]
    pub transaction_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_item_id: Option<String>,
    pub change_kind: V2ChangeKind,
    pub file: String,
    pub target: V2ChangeTarget,
    pub before: V2FileStateEvidence,
    pub after: V2FileStateEvidence,
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
pub enum V2ChangeKind {
    SemanticEdit,
    TextEdit,
    ApplyPatch,
    FileWrite,
    Undo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum V2ChangeTarget {
    File,
    SemanticSymbol {
        symbol_id: String,
        before_fingerprint: String,
        after_fingerprint: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V2FileStateEvidence {
    #[serde(default)]
    pub exists: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_len: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V2VerificationObserved {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_item_id: Option<String>,
    pub verification_kind: V2VerificationKind,
    pub source: V2VerificationSource,
    pub command: V2CommandEvidence,
    pub outcome: V2VerificationOutcome,
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
pub enum V2VerificationKind {
    Test,
    Build,
    Lint,
    TypeCheck,
    FormatCheck,
    SyntaxCheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum V2VerificationSource {
    ExecuteProcess,
    TuiTest,
    TuiLint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V2CommandEvidence {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V2VerificationOutcome {
    pub success: bool,
    pub status: String,
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub timed_out: bool,
}
