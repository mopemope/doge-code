//! v3 wire schema (current read + write).
//!
//! v3 adds Directive-to-Evidence traceability:
//! - `directive_observed` (observed user instruction, never an inferred intent)
//! - `requirement_changed` (agent-structured interpretation of a directive)
//! - `directive_id` / `requirement_ids` attribution frozen on plan, change,
//!   and verification events.
//!
//! Shapes here are intentionally separate structs from the canonical
//! `super::types` representation so durable evolution stays explicit.
//! v1 and v2 are read-only; only v3 is written.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V3Envelope {
    pub schema_version: u32,
    pub event_id: String,
    pub session_id: String,
    pub timestamp: String,
    pub event: V3Event,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum V3Event {
    DirectiveObserved(V3DirectiveObserved),
    RequirementChanged(V3RequirementChanged),
    PlanChanged(V3PlanChanged),
    ChangeCommitted(V3ChangeCommitted),
    VerificationObserved(V3VerificationObserved),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V3DirectiveObserved {
    pub origin: V3DirectiveOrigin,
    pub raw_input: String,
    pub raw_input_hash: String,
    pub effective_instruction: String,
    pub effective_instruction_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum V3DirectiveOrigin {
    TuiPrompt,
    TuiCustomCommand,
    ExecRun,
    ExecAsk,
    ExecRewrite,
    SemanticEdit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V3RequirementChanged {
    pub directive_id: String,
    pub changes: Vec<V3RequirementTransition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V3RequirementTransition {
    pub requirement_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<V3RequirementSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<V3RequirementSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V3RequirementSnapshot {
    pub id: String,
    pub statement: String,
    pub status: V3RequirementStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum V3RequirementStatus {
    Active,
    Withdrawn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V3PlanChanged {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directive_id: Option<String>,
    #[serde(default)]
    pub changes: Vec<V3PlanItemTransition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V3PlanItemTransition {
    pub plan_item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_status: Option<String>,
    #[serde(default)]
    pub before_requirement_ids: Vec<String>,
    #[serde(default)]
    pub after_requirement_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V3ChangeCommitted {
    #[serde(default)]
    pub transaction_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directive_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_item_id: Option<String>,
    #[serde(default)]
    pub requirement_ids: Vec<String>,
    pub change_kind: V3ChangeKind,
    pub file: String,
    pub target: V3ChangeTarget,
    pub before: V3FileStateEvidence,
    pub after: V3FileStateEvidence,
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
pub enum V3ChangeKind {
    SemanticEdit,
    TextEdit,
    ApplyPatch,
    FileWrite,
    Undo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum V3ChangeTarget {
    File,
    SemanticSymbol {
        symbol_id: String,
        before_fingerprint: String,
        after_fingerprint: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V3FileStateEvidence {
    #[serde(default)]
    pub exists: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_len: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V3VerificationObserved {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directive_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_item_id: Option<String>,
    #[serde(default)]
    pub requirement_ids: Vec<String>,
    pub verification_kind: V3VerificationKind,
    pub source: V3VerificationSource,
    pub command: V3CommandEvidence,
    pub outcome: V3VerificationOutcome,
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
pub enum V3VerificationKind {
    Test,
    Build,
    Lint,
    TypeCheck,
    FormatCheck,
    SyntaxCheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum V3VerificationSource {
    ExecuteProcess,
    TuiTest,
    TuiLint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V3CommandEvidence {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V3VerificationOutcome {
    pub success: bool,
    pub status: String,
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub timed_out: bool,
}
