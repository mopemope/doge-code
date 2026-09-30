//! v4 wire schema (current read + write).
//!
//! v4 adds Verification Obligations:
//! - `PlanChanged` transitions carry before/after obligation snapshots.
//! - `VerificationObserved` carries matched obligation refs (id + binding hash).
//!
//! Shapes here are intentionally separate structs from the canonical
//! `super::types` representation so durable evolution stays explicit.
//! v1/v2/v3 are read-only; only v4 is written.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V4Envelope {
    pub schema_version: u32,
    pub event_id: String,
    pub session_id: String,
    pub timestamp: String,
    pub event: V4Event,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum V4Event {
    DirectiveObserved(V4DirectiveObserved),
    RequirementChanged(V4RequirementChanged),
    PlanChanged(V4PlanChanged),
    ChangeCommitted(V4ChangeCommitted),
    VerificationObserved(V4VerificationObserved),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V4DirectiveObserved {
    pub origin: V4DirectiveOrigin,
    pub raw_input: String,
    pub raw_input_hash: String,
    pub effective_instruction: String,
    pub effective_instruction_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum V4DirectiveOrigin {
    TuiPrompt,
    TuiCustomCommand,
    ExecRun,
    ExecAsk,
    ExecRewrite,
    SemanticEdit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V4RequirementChanged {
    pub directive_id: String,
    pub changes: Vec<V4RequirementTransition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V4RequirementTransition {
    pub requirement_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<V4RequirementSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<V4RequirementSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V4RequirementSnapshot {
    pub id: String,
    pub statement: String,
    pub status: V4RequirementStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum V4RequirementStatus {
    Active,
    Withdrawn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V4PlanChanged {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directive_id: Option<String>,
    #[serde(default)]
    pub changes: Vec<V4PlanItemTransition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V4PlanItemTransition {
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
    #[serde(default)]
    pub before_verification_obligations: Vec<V4VerificationObligation>,
    #[serde(default)]
    pub after_verification_obligations: Vec<V4VerificationObligation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct V4VerificationCommandMatcher {
    pub program: String,
    #[serde(default)]
    pub args_prefix: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct V4VerificationObligation {
    pub id: String,
    pub description: String,
    pub kind: V4VerificationKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<V4VerificationCommandMatcher>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct V4VerificationObligationRef {
    pub id: String,
    pub binding_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V4ChangeCommitted {
    #[serde(default)]
    pub transaction_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directive_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_item_id: Option<String>,
    #[serde(default)]
    pub requirement_ids: Vec<String>,
    pub change_kind: V4ChangeKind,
    pub file: String,
    pub target: V4ChangeTarget,
    pub before: V4FileStateEvidence,
    pub after: V4FileStateEvidence,
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
pub enum V4ChangeKind {
    SemanticEdit,
    TextEdit,
    ApplyPatch,
    FileWrite,
    Undo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum V4ChangeTarget {
    File,
    SemanticSymbol {
        symbol_id: String,
        before_fingerprint: String,
        after_fingerprint: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V4FileStateEvidence {
    #[serde(default)]
    pub exists: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_len: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V4VerificationObserved {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directive_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_item_id: Option<String>,
    #[serde(default)]
    pub requirement_ids: Vec<String>,
    pub verification_kind: V4VerificationKind,
    pub source: V4VerificationSource,
    pub command: V4CommandEvidence,
    pub outcome: V4VerificationOutcome,
    #[serde(default)]
    pub observed_change_ids: Vec<String>,
    #[serde(default)]
    pub matched_obligations: Vec<V4VerificationObligationRef>,
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
pub enum V4VerificationKind {
    Test,
    Build,
    Lint,
    TypeCheck,
    FormatCheck,
    SyntaxCheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum V4VerificationSource {
    ExecuteProcess,
    TuiTest,
    TuiLint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V4CommandEvidence {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V4VerificationOutcome {
    pub success: bool,
    pub status: String,
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub timed_out: bool,
}
