use std::collections::BTreeMap;

use serde::Serialize;

use crate::provenance::{
    ChangeCommittedEvent, DirectiveOrigin, VerificationKind, VerificationObligationRef,
    VerificationOutcome, VerificationSource,
};
use crate::tools::plan::PlanItem;

#[derive(Debug, Clone, Copy, Default, clap::ValueEnum)]
pub enum ReportFormat {
    #[default]
    Markdown,
    Json,
}

#[derive(Debug, Serialize)]
pub struct EvidenceReport {
    pub schema_version: u32,
    pub generator: String,
    pub generated_at: String,
    pub session: ReportSession,
    pub scope: Scope,
    pub repository: Repository,
    pub snapshot: Snapshot,
    pub directives: Vec<Directive>,
    pub requirements: Vec<Requirement>,
    pub plan: Vec<PlanItem>,
    pub plan_available: bool,
    pub obligations: Vec<Obligation>,
    pub changes: Vec<Change>,
    pub verifications: Vec<Verification>,
    pub workspace_comparison: Vec<WorkspaceChange>,
    pub review_handoff: Vec<ReviewChange>,
    pub summary: Summary,
    pub warnings: Vec<ReportWarning>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ReportSession {
    pub id: String,
    pub updated_at: String,
    pub provenance_incomplete: bool,
    pub provenance_record_failures: u64,
}

#[derive(Debug, Serialize)]
pub struct Scope {
    pub comparison: String,
    pub project_relative_to_git_root: Option<String>,
    pub excluded_workspace_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryState {
    Ready,
    Unborn,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Repository {
    pub state: RepositoryState,
    pub head_oid: Option<String>,
    pub requested_base: Option<String>,
    pub base_oid: Option<String>,
    pub comparison_available: bool,
    pub comparison_method: String,
    pub project_relative_to_git_root: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Text,
    Binary,
    Missing,
    Symlink,
    Directory,
    Unsupported,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileEntry {
    pub path: String,
    pub kind: FileKind,
    pub exists: Option<bool>,
    pub byte_len: Option<u64>,
    pub content_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    pub manifest_version: u32,
    pub scope: String,
    pub consistency: String,
    pub complete: bool,
    pub manifest_digest: String,
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    NoLinkedWork,
    PlannedNoActiveChange,
    ActiveUnverified,
    NoLinkedChange,
    Pending,
    ObservedPassing,
    ObservedFailing,
    Stale,
    Diverged,
    Reverted,
    Mixed,
    Unavailable,
}

impl EvidenceState {
    pub fn label(self) -> &'static str {
        match self {
            Self::NoLinkedWork => "no_linked_work",
            Self::PlannedNoActiveChange => "planned_no_active_change",
            Self::ActiveUnverified => "active_unverified",
            Self::NoLinkedChange => "no_linked_change",
            Self::Pending => "pending",
            Self::ObservedPassing => "observed_passing",
            Self::ObservedFailing => "observed_failing",
            Self::Stale => "stale",
            Self::Diverged => "diverged",
            Self::Reverted => "reverted",
            Self::Mixed => "mixed",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Directive {
    pub id: String,
    pub timestamp: String,
    pub origin: DirectiveOrigin,
    pub raw_input_hash: String,
    pub effective_instruction_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_instruction: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Requirement {
    pub id: String,
    pub statement: String,
    pub status: crate::provenance::RequirementStatus,
    pub source_directive_ids: Vec<String>,
    pub linked_plan_item_ids: Vec<String>,
    pub active_change_ids: Vec<String>,
    pub observed_passing_change_ids: Vec<String>,
    pub unobserved_change_ids: Vec<String>,
    pub diverged_change_ids: Vec<String>,
    pub reverted_change_ids: Vec<String>,
    pub verification_ids: Vec<String>,
    pub evidence_state: EvidenceState,
    pub obligation_states: BTreeMap<String, usize>,
}

#[derive(Debug, Serialize)]
pub struct Obligation {
    pub id: String,
    pub plan_item_id: String,
    pub kind: VerificationKind,
    pub binding_hash: String,
    pub state: EvidenceState,
    pub active_change_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeState {
    Active,
    Superseded,
    Diverged,
    Missing,
    Reverted,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileMatch {
    Matched,
    Different,
    NotRecorded,
    Unavailable,
}

#[derive(Debug, Serialize)]
pub struct Change {
    pub id: String,
    pub timestamp: String,
    pub lifecycle_state: ChangeState,
    pub current_file_match: FileMatch,
    pub recorded: ChangeCommittedEvent,
}

#[derive(Debug, Serialize)]
pub struct Verification {
    pub structured_test_result:
        Option<Box<crate::features::structured_test_results::StructuredTestResult>>,
    pub test_count_unit: Option<&'static str>,
    pub execution_context: Option<Box<crate::features::verification_context::ExecutionContext>>,
    pub execution_workspace:
        Option<Box<crate::features::verification_snapshot::ExecutionWorkspace>>,
    pub current_code_state: crate::features::verification_snapshot::CurrentComparison,
    pub id: String,
    pub timestamp: String,
    pub directive_id: Option<String>,
    pub plan_item_id: Option<String>,
    pub requirement_ids: Vec<String>,
    pub source: VerificationSource,
    pub kind: VerificationKind,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub outcome: VerificationOutcome,
    pub observed_change_ids: Vec<String>,
    pub matched_obligations: Vec<VerificationObligationRef>,
    pub output_digest: String,
    pub output_truncated: bool,
    pub test_count: Option<u64>,
    pub execution_environment: Option<String>,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_excerpt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_excerpt: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Attribution {
    SessionLinked,
    Unattributed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceChange {
    pub path: String,
    pub differs_from_base: bool,
    pub staged: bool,
    pub unstaged: bool,
    pub untracked: bool,
    pub unmerged: bool,
    pub attribution: Attribution,
    pub session_change_ids: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Summary {
    pub requirement_states: BTreeMap<String, usize>,
    pub obligation_states: BTreeMap<String, usize>,
    pub change_states: BTreeMap<String, usize>,
    pub verification_successes: usize,
    pub verification_failures: usize,
    pub unattributed_files: usize,
    pub record_collection_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportWarning {
    pub code: WarningCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningCode {
    ProvenanceLoad,
    ProvenanceIncomplete,
    NoProvenance,
    InvalidEvent,
    InvalidPath,
    PlanUnavailable,
    RequirementHistory,
    WorkspaceUnavailable,
    FileUnavailable,
}

pub fn warn(warnings: &mut Vec<ReportWarning>, code: WarningCode, message: impl Into<String>) {
    warnings.push(ReportWarning {
        code,
        message: message.into(),
    });
}

/// Derived links for review; these observations never establish readiness.
#[derive(Debug, Serialize)]
pub struct ReviewChange {
    pub change_id: String,
    pub matching_successful_observation_ids: Vec<String>,
    pub failed_observation_ids: Vec<String>,
    pub other_successful_observation_ids: Vec<String>,
}
