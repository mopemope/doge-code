use serde::{Deserialize, Serialize};

use super::wire::v1 as wire_v1;
use super::wire::v2 as wire_v2;
use super::wire::v3 as wire_v3;
use super::wire::v4 as wire_v4;

/// Current on-disk schema version for newly written provenance events.
pub const PROVENANCE_SCHEMA_VERSION: u32 = 6;
pub const V5_PROVENANCE_SCHEMA_VERSION: u32 = 5;
pub const V4_PROVENANCE_SCHEMA_VERSION: u32 = 4;
/// Legacy read-only schema version. v1 files are converted on read and are
/// never physically migrated.
pub const LEGACY_PROVENANCE_SCHEMA_VERSION: u32 = 1;
/// Previous read-only schema version. v2 files are converted on read and are
/// never physically migrated or rewritten.
pub const V2_PROVENANCE_SCHEMA_VERSION: u32 = 2;
/// Previous read-only schema version. v3 files are converted on read and are
/// never physically migrated or rewritten.
pub const V3_PROVENANCE_SCHEMA_VERSION: u32 = 3;

/// Versioned envelope wrapping every provenance event.
///
/// `event_id` is a UUIDv7 string. Query ordering is `(timestamp, event_id)`;
/// never rely on filesystem iteration order or on `event_id` lexical order
/// alone across processes.
///
/// The envelope `event_id` doubles as the canonical Directive ID for
/// `DirectiveObserved` events: `directive_id = event_id`.
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
            ProvenanceEvent::DirectiveObserved(_) => ProvenanceEventType::DirectiveObserved,
            ProvenanceEvent::RequirementChanged(_) => ProvenanceEventType::RequirementChanged,
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
            ProvenanceEvent::DirectiveObserved(_) => None,
            ProvenanceEvent::RequirementChanged(_) => None,
            ProvenanceEvent::PlanChanged(_) => None,
            ProvenanceEvent::ChangeCommitted(e) => e.plan_item_id.as_deref(),
            ProvenanceEvent::VerificationObserved(e) => e.plan_item_id.as_deref(),
        }
    }

    /// Attributed directive id, when the event carries one.
    pub fn directive_id(&self) -> Option<&str> {
        match &self.event {
            ProvenanceEvent::DirectiveObserved(_) => Some(self.event_id.as_str()),
            ProvenanceEvent::RequirementChanged(e) => Some(e.directive_id.as_str()),
            ProvenanceEvent::PlanChanged(e) => e.directive_id.as_deref(),
            ProvenanceEvent::ChangeCommitted(e) => e.directive_id.as_deref(),
            ProvenanceEvent::VerificationObserved(e) => e.directive_id.as_deref(),
        }
    }

    /// Frozen requirement ids carried by the event (empty for directives).
    pub fn requirement_ids(&self) -> &[String] {
        match &self.event {
            ProvenanceEvent::DirectiveObserved(_) => &[],
            ProvenanceEvent::RequirementChanged(_) => &[],
            ProvenanceEvent::PlanChanged(_) => &[],
            ProvenanceEvent::ChangeCommitted(e) => &e.requirement_ids,
            ProvenanceEvent::VerificationObserved(e) => &e.requirement_ids,
        }
    }
}

/// Tagged provenance event. The `type` string is part of the durable contract
/// and must not be renamed casually.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProvenanceEvent {
    DirectiveObserved(DirectiveObservedEvent),
    RequirementChanged(RequirementChangedEvent),
    PlanChanged(PlanChangedEvent),
    ChangeCommitted(ChangeCommittedEvent),
    VerificationObserved(VerificationObservedEvent),
}

/// Filter value for reads. Mirrors [`ProvenanceEvent`] tag strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceEventType {
    DirectiveObserved,
    RequirementChanged,
    PlanChanged,
    ChangeCommitted,
    VerificationObserved,
}

impl ProvenanceEventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DirectiveObserved => "directive_observed",
            Self::RequirementChanged => "requirement_changed",
            Self::PlanChanged => "plan_changed",
            Self::ChangeCommitted => "change_committed",
            Self::VerificationObserved => "verification_observed",
        }
    }
}

// ---------------------------------------------------------------------------
// Directive (observed fact, never an inferred intent)
// ---------------------------------------------------------------------------

/// Where an observed directive came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectiveOrigin {
    TuiPrompt,
    TuiCustomCommand,
    ExecRun,
    ExecAsk,
    ExecRewrite,
    SemanticEdit,
}

impl DirectiveOrigin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TuiPrompt => "tui_prompt",
            Self::TuiCustomCommand => "tui_custom_command",
            Self::ExecRun => "exec_run",
            Self::ExecAsk => "exec_ask",
            Self::ExecRewrite => "exec_rewrite",
            Self::SemanticEdit => "semantic_edit",
        }
    }
}

/// An observed user instruction.
///
/// `raw_input` is what the user typed (e.g. `/fix-cache` or a plain prompt);
/// `effective_instruction` is what was actually handed to the agent (custom
/// commands expand the raw input). For plain prompts they are usually equal.
/// Hashes are `blake3:<hex>` over exact UTF-8 bytes.
///
/// The canonical Directive ID is the envelope `event_id` (no separate id
/// field is generated).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectiveObservedEvent {
    pub origin: DirectiveOrigin,
    pub raw_input: String,
    pub raw_input_hash: String,
    pub effective_instruction: String,
    pub effective_instruction_hash: String,
}

impl DirectiveObservedEvent {
    /// First ~200 chars of the effective instruction for budgeted summaries.
    /// Never used for identity; hashes are authoritative.
    pub fn preview(&self) -> String {
        preview_text(&self.effective_instruction, 200)
    }
}

/// Exact-byte content hash: `blake3:<64 hex>` over UTF-8 bytes.
pub fn directive_content_hash(content: &str) -> String {
    format!("blake3:{}", blake3::hash(content.as_bytes()).to_hex())
}

fn preview_text(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{truncated}…")
    }
}

// ---------------------------------------------------------------------------
// Requirement (agent-structured interpretation of a directive)
// ---------------------------------------------------------------------------

/// Minimal v1 requirement lifecycle. There is intentionally no
/// `Satisfied`/`Verified`: a passing test never proves a requirement correct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementStatus {
    Active,
    Withdrawn,
}

/// One requirement snapshot in a transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementSnapshot {
    pub id: String,
    pub statement: String,
    pub status: RequirementStatus,
}

/// Single requirement transition: create (`before = None`), refine/withdraw/
/// reactivate (`before` + `after`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementTransition {
    pub requirement_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<RequirementSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<RequirementSnapshot>,
}

/// A batch of requirement transitions attributed to one directive.
///
/// `directive_id` is the envelope `event_id` of the `DirectiveObserved` that
/// motivated this structuring. Requirements must never be created without an
/// observed directive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementChangedEvent {
    pub directive_id: String,
    pub changes: Vec<RequirementTransition>,
}

// ---------------------------------------------------------------------------
// Plan / Change / Verification (v4 attribution with obligations)
// ---------------------------------------------------------------------------

/// Structured command matcher for a verification obligation.
///
/// Evidence attribution only (not a security boundary): exact-token prefix
/// match on `program` basename + `args_prefix`. No regex, glob, or shell
/// parsing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationCommandMatcher {
    pub program: String,
    #[serde(default)]
    pub args_prefix: Vec<String>,
}

/// What a plan step intends to observe (not a proof).
///
/// `command` is optional: when absent, `kind` + plan scope match; when
/// present, `kind` + basename + argv prefix must match.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationObligation {
    pub id: String,
    pub description: String,
    pub kind: VerificationKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<VerificationCommandMatcher>,
}

/// Frozen reference to a matched obligation.
///
/// `binding_hash` freezes the obligation definition + requirement scope at
/// capture time so later plan edits never rewrite historical attribution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationObligationRef {
    pub id: String,
    pub binding_hash: String,
}

/// One plan write expressed as per-item transitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanChangedEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directive_id: Option<String>,
    pub changes: Vec<PlanItemTransition>,
}

/// Transition of a single plan item.
///
/// New item: `before_status = None`. Deleted item: `after_status = None`.
/// Requirement-link-only and obligation-only edits also produce a transition.
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
    #[serde(default)]
    pub before_requirement_ids: Vec<String>,
    #[serde(default)]
    pub after_requirement_ids: Vec<String>,
    #[serde(default)]
    pub before_verification_obligations: Vec<VerificationObligation>,
    #[serde(default)]
    pub after_verification_obligations: Vec<VerificationObligation>,
}

/// A committed workspace mutation (v3 canonical).
///
/// This is a Doge-observed `before -> after` transaction, not an LLM
/// self-report: `before`/`after` are exact file states and `diff` is
/// generated from them. `directive_id` / `plan_item_id` / `requirement_ids`
/// are frozen at commit time and never reinterpreted when later plan links
/// move.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeCommittedEvent {
    pub transaction_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directive_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_item_id: Option<String>,
    #[serde(default)]
    pub requirement_ids: Vec<String>,
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
    directive_content_hash(content)
}

/// An observed verification command run.
///
/// This records that a command was started and finished against a workspace
/// snapshot; it never claims the implementation is proven or guaranteed
/// correct. `requirement_ids` are frozen at capture time (union of active
/// change requirement ids, falling back to the current plan item links).
/// `matched_obligations` freezes which obligations this run was attributed
/// to (id + binding hash), including for failed runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationObservedEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_context: Option<Box<crate::features::verification_context::ExecutionContext>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_workspace:
        Option<Box<crate::features::verification_snapshot::ExecutionWorkspace>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directive_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_item_id: Option<String>,
    #[serde(default)]
    pub requirement_ids: Vec<String>,
    pub verification_kind: VerificationKind,
    pub source: VerificationSource,
    pub command: CommandEvidence,
    pub outcome: VerificationOutcome,
    #[serde(default)]
    pub observed_change_ids: Vec<String>,
    #[serde(default)]
    pub matched_obligations: Vec<VerificationObligationRef>,
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
/// `requirement_ids` are computed at capture: union of active change
/// requirement ids, falling back to current plan item links when no active
/// change carries requirements. `matched_obligations` freezes obligation
/// attribution at the same pre-execution instant.
#[derive(Debug, Clone, Default)]
pub struct VerificationContext {
    pub execution_context: Option<crate::features::verification_context::ExecutionContext>,
    pub execution_workspace: Option<crate::features::verification_snapshot::ExecutionWorkspace>,
    pub directive_id: Option<String>,
    pub plan_item_id: Option<String>,
    pub requirement_ids: Vec<String>,
    pub observed_change_ids: Vec<String>,
    pub matched_obligations: Vec<VerificationObligationRef>,
}

// ---------------------------------------------------------------------------
// Wire conversions (explicit, no implicit serde evolution)
// ---------------------------------------------------------------------------

/// Convert a v1 wire envelope into the canonical representation.
///
/// v1 changes were semantic-edit only. Whole-file hashes did not exist, so
/// `before`/`after` carry `content_hash = None` with `exists = true`
/// (a semantic edit always rewrote an existing file). v1 predates
/// directives/requirements: `directive_id = None`, `requirement_ids = []`.
pub fn from_v1_wire(env: wire_v1::V1Envelope) -> ProvenanceEventEnvelope {
    let event = match env.event {
        wire_v1::V1Event::PlanChanged(p) => ProvenanceEvent::PlanChanged(PlanChangedEvent {
            directive_id: None,
            changes: p
                .changes
                .into_iter()
                .map(|c| PlanItemTransition {
                    plan_item_id: c.plan_item_id,
                    parent_id: c.parent_id,
                    content: c.content,
                    before_status: c.before_status,
                    after_status: c.after_status,
                    before_requirement_ids: Vec::new(),
                    after_requirement_ids: Vec::new(),
                    before_verification_obligations: Vec::new(),
                    after_verification_obligations: Vec::new(),
                })
                .collect(),
        }),
        wire_v1::V1Event::ChangeCommitted(c) => {
            ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                transaction_id: c.transaction_id,
                directive_id: None,
                plan_item_id: c.plan_item_id,
                requirement_ids: Vec::new(),
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
                directive_id: None,
                plan_item_id: v.plan_item_id,
                requirement_ids: Vec::new(),
                execution_context: None,
                execution_workspace: None,
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
                matched_obligations: Vec::new(),
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
///
/// v2 predates directives/requirements: `directive_id = None`,
/// `requirement_ids = []`. Existing semantics are unchanged.
pub fn from_v2_wire(env: wire_v2::V2Envelope) -> ProvenanceEventEnvelope {
    let event = match env.event {
        wire_v2::V2Event::PlanChanged(p) => ProvenanceEvent::PlanChanged(PlanChangedEvent {
            directive_id: None,
            changes: p
                .changes
                .into_iter()
                .map(|c| PlanItemTransition {
                    plan_item_id: c.plan_item_id,
                    parent_id: c.parent_id,
                    content: c.content,
                    before_status: c.before_status,
                    after_status: c.after_status,
                    before_requirement_ids: Vec::new(),
                    after_requirement_ids: Vec::new(),
                    before_verification_obligations: Vec::new(),
                    after_verification_obligations: Vec::new(),
                })
                .collect(),
        }),
        wire_v2::V2Event::ChangeCommitted(c) => {
            ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                transaction_id: c.transaction_id,
                directive_id: None,
                plan_item_id: c.plan_item_id,
                requirement_ids: Vec::new(),
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
                directive_id: None,
                plan_item_id: v.plan_item_id,
                requirement_ids: Vec::new(),
                execution_context: None,
                execution_workspace: None,
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
                matched_obligations: Vec::new(),
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

/// Convert a v3 wire envelope into the canonical representation.
pub fn from_v3_wire(env: wire_v3::V3Envelope) -> ProvenanceEventEnvelope {
    let event = match env.event {
        wire_v3::V3Event::DirectiveObserved(d) => {
            ProvenanceEvent::DirectiveObserved(DirectiveObservedEvent {
                origin: match d.origin {
                    wire_v3::V3DirectiveOrigin::TuiPrompt => DirectiveOrigin::TuiPrompt,
                    wire_v3::V3DirectiveOrigin::TuiCustomCommand => {
                        DirectiveOrigin::TuiCustomCommand
                    }
                    wire_v3::V3DirectiveOrigin::ExecRun => DirectiveOrigin::ExecRun,
                    wire_v3::V3DirectiveOrigin::ExecAsk => DirectiveOrigin::ExecAsk,
                    wire_v3::V3DirectiveOrigin::ExecRewrite => DirectiveOrigin::ExecRewrite,
                    wire_v3::V3DirectiveOrigin::SemanticEdit => DirectiveOrigin::SemanticEdit,
                },
                raw_input: d.raw_input,
                raw_input_hash: d.raw_input_hash,
                effective_instruction: d.effective_instruction,
                effective_instruction_hash: d.effective_instruction_hash,
            })
        }
        wire_v3::V3Event::RequirementChanged(r) => {
            ProvenanceEvent::RequirementChanged(RequirementChangedEvent {
                directive_id: r.directive_id,
                changes: r
                    .changes
                    .into_iter()
                    .map(|t| RequirementTransition {
                        requirement_id: t.requirement_id,
                        before: t.before.map(|s| RequirementSnapshot {
                            id: s.id,
                            statement: s.statement,
                            status: match s.status {
                                wire_v3::V3RequirementStatus::Active => RequirementStatus::Active,
                                wire_v3::V3RequirementStatus::Withdrawn => {
                                    RequirementStatus::Withdrawn
                                }
                            },
                        }),
                        after: t.after.map(|s| RequirementSnapshot {
                            id: s.id,
                            statement: s.statement,
                            status: match s.status {
                                wire_v3::V3RequirementStatus::Active => RequirementStatus::Active,
                                wire_v3::V3RequirementStatus::Withdrawn => {
                                    RequirementStatus::Withdrawn
                                }
                            },
                        }),
                    })
                    .collect(),
            })
        }
        wire_v3::V3Event::PlanChanged(p) => ProvenanceEvent::PlanChanged(PlanChangedEvent {
            directive_id: p.directive_id,
            changes: p
                .changes
                .into_iter()
                .map(|c| PlanItemTransition {
                    plan_item_id: c.plan_item_id,
                    parent_id: c.parent_id,
                    content: c.content,
                    before_status: c.before_status,
                    after_status: c.after_status,
                    before_requirement_ids: c.before_requirement_ids,
                    after_requirement_ids: c.after_requirement_ids,
                    before_verification_obligations: Vec::new(),
                    after_verification_obligations: Vec::new(),
                })
                .collect(),
        }),
        wire_v3::V3Event::ChangeCommitted(c) => {
            ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                transaction_id: c.transaction_id,
                directive_id: c.directive_id,
                plan_item_id: c.plan_item_id,
                requirement_ids: c.requirement_ids,
                change_kind: match c.change_kind {
                    wire_v3::V3ChangeKind::SemanticEdit => ChangeKind::SemanticEdit,
                    wire_v3::V3ChangeKind::TextEdit => ChangeKind::TextEdit,
                    wire_v3::V3ChangeKind::ApplyPatch => ChangeKind::ApplyPatch,
                    wire_v3::V3ChangeKind::FileWrite => ChangeKind::FileWrite,
                    wire_v3::V3ChangeKind::Undo => ChangeKind::Undo,
                },
                file: c.file,
                target: match c.target {
                    wire_v3::V3ChangeTarget::File => ChangeTarget::File,
                    wire_v3::V3ChangeTarget::SemanticSymbol {
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
        wire_v3::V3Event::VerificationObserved(v) => {
            ProvenanceEvent::VerificationObserved(VerificationObservedEvent {
                directive_id: v.directive_id,
                plan_item_id: v.plan_item_id,
                requirement_ids: v.requirement_ids,
                execution_context: None,
                execution_workspace: None,
                verification_kind: match v.verification_kind {
                    wire_v3::V3VerificationKind::Test => VerificationKind::Test,
                    wire_v3::V3VerificationKind::Build => VerificationKind::Build,
                    wire_v3::V3VerificationKind::Lint => VerificationKind::Lint,
                    wire_v3::V3VerificationKind::TypeCheck => VerificationKind::TypeCheck,
                    wire_v3::V3VerificationKind::FormatCheck => VerificationKind::FormatCheck,
                    wire_v3::V3VerificationKind::SyntaxCheck => VerificationKind::SyntaxCheck,
                },
                source: match v.source {
                    wire_v3::V3VerificationSource::ExecuteProcess => {
                        VerificationSource::ExecuteProcess
                    }
                    wire_v3::V3VerificationSource::TuiTest => VerificationSource::TuiTest,
                    wire_v3::V3VerificationSource::TuiLint => VerificationSource::TuiLint,
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
                matched_obligations: Vec::new(),
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

/// Convert a canonical envelope into its v3 wire form for persistence.
///
/// All new events are written as v3; v1/v2 are never written.
pub fn to_v3_wire(env: &ProvenanceEventEnvelope) -> wire_v3::V3Envelope {
    let event = match &env.event {
        ProvenanceEvent::DirectiveObserved(d) => {
            wire_v3::V3Event::DirectiveObserved(wire_v3::V3DirectiveObserved {
                origin: match d.origin {
                    DirectiveOrigin::TuiPrompt => wire_v3::V3DirectiveOrigin::TuiPrompt,
                    DirectiveOrigin::TuiCustomCommand => {
                        wire_v3::V3DirectiveOrigin::TuiCustomCommand
                    }
                    DirectiveOrigin::ExecRun => wire_v3::V3DirectiveOrigin::ExecRun,
                    DirectiveOrigin::ExecAsk => wire_v3::V3DirectiveOrigin::ExecAsk,
                    DirectiveOrigin::ExecRewrite => wire_v3::V3DirectiveOrigin::ExecRewrite,
                    DirectiveOrigin::SemanticEdit => wire_v3::V3DirectiveOrigin::SemanticEdit,
                },
                raw_input: d.raw_input.clone(),
                raw_input_hash: d.raw_input_hash.clone(),
                effective_instruction: d.effective_instruction.clone(),
                effective_instruction_hash: d.effective_instruction_hash.clone(),
            })
        }
        ProvenanceEvent::RequirementChanged(r) => {
            wire_v3::V3Event::RequirementChanged(wire_v3::V3RequirementChanged {
                directive_id: r.directive_id.clone(),
                changes: r
                    .changes
                    .iter()
                    .map(|t| wire_v3::V3RequirementTransition {
                        requirement_id: t.requirement_id.clone(),
                        before: t.before.as_ref().map(|s| wire_v3::V3RequirementSnapshot {
                            id: s.id.clone(),
                            statement: s.statement.clone(),
                            status: match s.status {
                                RequirementStatus::Active => wire_v3::V3RequirementStatus::Active,
                                RequirementStatus::Withdrawn => {
                                    wire_v3::V3RequirementStatus::Withdrawn
                                }
                            },
                        }),
                        after: t.after.as_ref().map(|s| wire_v3::V3RequirementSnapshot {
                            id: s.id.clone(),
                            statement: s.statement.clone(),
                            status: match s.status {
                                RequirementStatus::Active => wire_v3::V3RequirementStatus::Active,
                                RequirementStatus::Withdrawn => {
                                    wire_v3::V3RequirementStatus::Withdrawn
                                }
                            },
                        }),
                    })
                    .collect(),
            })
        }
        ProvenanceEvent::PlanChanged(p) => wire_v3::V3Event::PlanChanged(wire_v3::V3PlanChanged {
            directive_id: p.directive_id.clone(),
            changes: p
                .changes
                .iter()
                .map(|c| wire_v3::V3PlanItemTransition {
                    plan_item_id: c.plan_item_id.clone(),
                    parent_id: c.parent_id.clone(),
                    content: c.content.clone(),
                    before_status: c.before_status.clone(),
                    after_status: c.after_status.clone(),
                    before_requirement_ids: c.before_requirement_ids.clone(),
                    after_requirement_ids: c.after_requirement_ids.clone(),
                })
                .collect(),
        }),
        ProvenanceEvent::ChangeCommitted(c) => {
            wire_v3::V3Event::ChangeCommitted(wire_v3::V3ChangeCommitted {
                transaction_id: c.transaction_id.clone(),
                directive_id: c.directive_id.clone(),
                plan_item_id: c.plan_item_id.clone(),
                requirement_ids: c.requirement_ids.clone(),
                change_kind: match c.change_kind {
                    ChangeKind::SemanticEdit => wire_v3::V3ChangeKind::SemanticEdit,
                    ChangeKind::TextEdit => wire_v3::V3ChangeKind::TextEdit,
                    ChangeKind::ApplyPatch => wire_v3::V3ChangeKind::ApplyPatch,
                    ChangeKind::FileWrite => wire_v3::V3ChangeKind::FileWrite,
                    ChangeKind::Undo => wire_v3::V3ChangeKind::Undo,
                },
                file: c.file.clone(),
                target: match &c.target {
                    ChangeTarget::File => wire_v3::V3ChangeTarget::File,
                    ChangeTarget::SemanticSymbol {
                        symbol_id,
                        before_fingerprint,
                        after_fingerprint,
                    } => wire_v3::V3ChangeTarget::SemanticSymbol {
                        symbol_id: symbol_id.clone(),
                        before_fingerprint: before_fingerprint.clone(),
                        after_fingerprint: after_fingerprint.clone(),
                    },
                },
                before: wire_v3::V3FileStateEvidence {
                    exists: c.before.exists,
                    content_hash: c.before.content_hash.clone(),
                    byte_len: c.before.byte_len,
                },
                after: wire_v3::V3FileStateEvidence {
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
            wire_v3::V3Event::VerificationObserved(wire_v3::V3VerificationObserved {
                directive_id: v.directive_id.clone(),
                plan_item_id: v.plan_item_id.clone(),
                requirement_ids: v.requirement_ids.clone(),
                verification_kind: match v.verification_kind {
                    VerificationKind::Test => wire_v3::V3VerificationKind::Test,
                    VerificationKind::Build => wire_v3::V3VerificationKind::Build,
                    VerificationKind::Lint => wire_v3::V3VerificationKind::Lint,
                    VerificationKind::TypeCheck => wire_v3::V3VerificationKind::TypeCheck,
                    VerificationKind::FormatCheck => wire_v3::V3VerificationKind::FormatCheck,
                    VerificationKind::SyntaxCheck => wire_v3::V3VerificationKind::SyntaxCheck,
                },
                source: match v.source {
                    VerificationSource::ExecuteProcess => {
                        wire_v3::V3VerificationSource::ExecuteProcess
                    }
                    VerificationSource::TuiTest => wire_v3::V3VerificationSource::TuiTest,
                    VerificationSource::TuiLint => wire_v3::V3VerificationSource::TuiLint,
                },
                command: wire_v3::V3CommandEvidence {
                    program: v.command.program.clone(),
                    args: v.command.args.clone(),
                    cwd: v.command.cwd.clone(),
                },
                outcome: wire_v3::V3VerificationOutcome {
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
    wire_v3::V3Envelope {
        schema_version: V3_PROVENANCE_SCHEMA_VERSION,
        event_id: env.event_id.clone(),
        session_id: env.session_id.clone(),
        timestamp: env.timestamp.clone(),
        event,
    }
}

/// Convert a v4 wire envelope into the canonical representation.
pub fn from_v4_wire(env: wire_v4::V4Envelope) -> ProvenanceEventEnvelope {
    fn kind_from_v4(k: wire_v4::V4VerificationKind) -> VerificationKind {
        match k {
            wire_v4::V4VerificationKind::Test => VerificationKind::Test,
            wire_v4::V4VerificationKind::Build => VerificationKind::Build,
            wire_v4::V4VerificationKind::Lint => VerificationKind::Lint,
            wire_v4::V4VerificationKind::TypeCheck => VerificationKind::TypeCheck,
            wire_v4::V4VerificationKind::FormatCheck => VerificationKind::FormatCheck,
            wire_v4::V4VerificationKind::SyntaxCheck => VerificationKind::SyntaxCheck,
        }
    }
    fn obligation_from_v4(o: wire_v4::V4VerificationObligation) -> VerificationObligation {
        VerificationObligation {
            id: o.id,
            description: o.description,
            kind: kind_from_v4(o.kind),
            command: o.command.map(|c| VerificationCommandMatcher {
                program: c.program,
                args_prefix: c.args_prefix,
            }),
        }
    }
    let event = match env.event {
        wire_v4::V4Event::DirectiveObserved(d) => {
            ProvenanceEvent::DirectiveObserved(DirectiveObservedEvent {
                origin: match d.origin {
                    wire_v4::V4DirectiveOrigin::TuiPrompt => DirectiveOrigin::TuiPrompt,
                    wire_v4::V4DirectiveOrigin::TuiCustomCommand => {
                        DirectiveOrigin::TuiCustomCommand
                    }
                    wire_v4::V4DirectiveOrigin::ExecRun => DirectiveOrigin::ExecRun,
                    wire_v4::V4DirectiveOrigin::ExecAsk => DirectiveOrigin::ExecAsk,
                    wire_v4::V4DirectiveOrigin::ExecRewrite => DirectiveOrigin::ExecRewrite,
                    wire_v4::V4DirectiveOrigin::SemanticEdit => DirectiveOrigin::SemanticEdit,
                },
                raw_input: d.raw_input,
                raw_input_hash: d.raw_input_hash,
                effective_instruction: d.effective_instruction,
                effective_instruction_hash: d.effective_instruction_hash,
            })
        }
        wire_v4::V4Event::RequirementChanged(r) => {
            ProvenanceEvent::RequirementChanged(RequirementChangedEvent {
                directive_id: r.directive_id,
                changes: r
                    .changes
                    .into_iter()
                    .map(|t| RequirementTransition {
                        requirement_id: t.requirement_id,
                        before: t.before.map(|s| RequirementSnapshot {
                            id: s.id,
                            statement: s.statement,
                            status: match s.status {
                                wire_v4::V4RequirementStatus::Active => RequirementStatus::Active,
                                wire_v4::V4RequirementStatus::Withdrawn => {
                                    RequirementStatus::Withdrawn
                                }
                            },
                        }),
                        after: t.after.map(|s| RequirementSnapshot {
                            id: s.id,
                            statement: s.statement,
                            status: match s.status {
                                wire_v4::V4RequirementStatus::Active => RequirementStatus::Active,
                                wire_v4::V4RequirementStatus::Withdrawn => {
                                    RequirementStatus::Withdrawn
                                }
                            },
                        }),
                    })
                    .collect(),
            })
        }
        wire_v4::V4Event::PlanChanged(p) => ProvenanceEvent::PlanChanged(PlanChangedEvent {
            directive_id: p.directive_id,
            changes: p
                .changes
                .into_iter()
                .map(|c| PlanItemTransition {
                    plan_item_id: c.plan_item_id,
                    parent_id: c.parent_id,
                    content: c.content,
                    before_status: c.before_status,
                    after_status: c.after_status,
                    before_requirement_ids: c.before_requirement_ids,
                    after_requirement_ids: c.after_requirement_ids,
                    before_verification_obligations: c
                        .before_verification_obligations
                        .into_iter()
                        .map(obligation_from_v4)
                        .collect(),
                    after_verification_obligations: c
                        .after_verification_obligations
                        .into_iter()
                        .map(obligation_from_v4)
                        .collect(),
                })
                .collect(),
        }),
        wire_v4::V4Event::ChangeCommitted(c) => {
            ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                transaction_id: c.transaction_id,
                directive_id: c.directive_id,
                plan_item_id: c.plan_item_id,
                requirement_ids: c.requirement_ids,
                change_kind: match c.change_kind {
                    wire_v4::V4ChangeKind::SemanticEdit => ChangeKind::SemanticEdit,
                    wire_v4::V4ChangeKind::TextEdit => ChangeKind::TextEdit,
                    wire_v4::V4ChangeKind::ApplyPatch => ChangeKind::ApplyPatch,
                    wire_v4::V4ChangeKind::FileWrite => ChangeKind::FileWrite,
                    wire_v4::V4ChangeKind::Undo => ChangeKind::Undo,
                },
                file: c.file,
                target: match c.target {
                    wire_v4::V4ChangeTarget::File => ChangeTarget::File,
                    wire_v4::V4ChangeTarget::SemanticSymbol {
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
        wire_v4::V4Event::VerificationObserved(v) => {
            ProvenanceEvent::VerificationObserved(VerificationObservedEvent {
                directive_id: v.directive_id,
                plan_item_id: v.plan_item_id,
                requirement_ids: v.requirement_ids,
                execution_context: None,
                execution_workspace: None,
                verification_kind: kind_from_v4(v.verification_kind),
                source: match v.source {
                    wire_v4::V4VerificationSource::ExecuteProcess => {
                        VerificationSource::ExecuteProcess
                    }
                    wire_v4::V4VerificationSource::TuiTest => VerificationSource::TuiTest,
                    wire_v4::V4VerificationSource::TuiLint => VerificationSource::TuiLint,
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
                matched_obligations: v
                    .matched_obligations
                    .into_iter()
                    .map(|r| VerificationObligationRef {
                        id: r.id,
                        binding_hash: r.binding_hash,
                    })
                    .collect(),
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

/// Convert a canonical envelope into its v4 wire form for persistence.
///
/// All new events are written as v4; v1/v2/v3 are never written.
pub fn to_v4_wire(env: &ProvenanceEventEnvelope) -> wire_v4::V4Envelope {
    fn kind_to_v4(k: VerificationKind) -> wire_v4::V4VerificationKind {
        match k {
            VerificationKind::Test => wire_v4::V4VerificationKind::Test,
            VerificationKind::Build => wire_v4::V4VerificationKind::Build,
            VerificationKind::Lint => wire_v4::V4VerificationKind::Lint,
            VerificationKind::TypeCheck => wire_v4::V4VerificationKind::TypeCheck,
            VerificationKind::FormatCheck => wire_v4::V4VerificationKind::FormatCheck,
            VerificationKind::SyntaxCheck => wire_v4::V4VerificationKind::SyntaxCheck,
        }
    }
    fn obligation_to_v4(o: &VerificationObligation) -> wire_v4::V4VerificationObligation {
        wire_v4::V4VerificationObligation {
            id: o.id.clone(),
            description: o.description.clone(),
            kind: kind_to_v4(o.kind),
            command: o
                .command
                .as_ref()
                .map(|c| wire_v4::V4VerificationCommandMatcher {
                    program: c.program.clone(),
                    args_prefix: c.args_prefix.clone(),
                }),
        }
    }
    let event = match &env.event {
        ProvenanceEvent::DirectiveObserved(d) => {
            wire_v4::V4Event::DirectiveObserved(wire_v4::V4DirectiveObserved {
                origin: match d.origin {
                    DirectiveOrigin::TuiPrompt => wire_v4::V4DirectiveOrigin::TuiPrompt,
                    DirectiveOrigin::TuiCustomCommand => {
                        wire_v4::V4DirectiveOrigin::TuiCustomCommand
                    }
                    DirectiveOrigin::ExecRun => wire_v4::V4DirectiveOrigin::ExecRun,
                    DirectiveOrigin::ExecAsk => wire_v4::V4DirectiveOrigin::ExecAsk,
                    DirectiveOrigin::ExecRewrite => wire_v4::V4DirectiveOrigin::ExecRewrite,
                    DirectiveOrigin::SemanticEdit => wire_v4::V4DirectiveOrigin::SemanticEdit,
                },
                raw_input: d.raw_input.clone(),
                raw_input_hash: d.raw_input_hash.clone(),
                effective_instruction: d.effective_instruction.clone(),
                effective_instruction_hash: d.effective_instruction_hash.clone(),
            })
        }
        ProvenanceEvent::RequirementChanged(r) => {
            wire_v4::V4Event::RequirementChanged(wire_v4::V4RequirementChanged {
                directive_id: r.directive_id.clone(),
                changes: r
                    .changes
                    .iter()
                    .map(|t| wire_v4::V4RequirementTransition {
                        requirement_id: t.requirement_id.clone(),
                        before: t.before.as_ref().map(|s| wire_v4::V4RequirementSnapshot {
                            id: s.id.clone(),
                            statement: s.statement.clone(),
                            status: match s.status {
                                RequirementStatus::Active => wire_v4::V4RequirementStatus::Active,
                                RequirementStatus::Withdrawn => {
                                    wire_v4::V4RequirementStatus::Withdrawn
                                }
                            },
                        }),
                        after: t.after.as_ref().map(|s| wire_v4::V4RequirementSnapshot {
                            id: s.id.clone(),
                            statement: s.statement.clone(),
                            status: match s.status {
                                RequirementStatus::Active => wire_v4::V4RequirementStatus::Active,
                                RequirementStatus::Withdrawn => {
                                    wire_v4::V4RequirementStatus::Withdrawn
                                }
                            },
                        }),
                    })
                    .collect(),
            })
        }
        ProvenanceEvent::PlanChanged(p) => wire_v4::V4Event::PlanChanged(wire_v4::V4PlanChanged {
            directive_id: p.directive_id.clone(),
            changes: p
                .changes
                .iter()
                .map(|c| wire_v4::V4PlanItemTransition {
                    plan_item_id: c.plan_item_id.clone(),
                    parent_id: c.parent_id.clone(),
                    content: c.content.clone(),
                    before_status: c.before_status.clone(),
                    after_status: c.after_status.clone(),
                    before_requirement_ids: c.before_requirement_ids.clone(),
                    after_requirement_ids: c.after_requirement_ids.clone(),
                    before_verification_obligations: c
                        .before_verification_obligations
                        .iter()
                        .map(obligation_to_v4)
                        .collect(),
                    after_verification_obligations: c
                        .after_verification_obligations
                        .iter()
                        .map(obligation_to_v4)
                        .collect(),
                })
                .collect(),
        }),
        ProvenanceEvent::ChangeCommitted(c) => {
            wire_v4::V4Event::ChangeCommitted(wire_v4::V4ChangeCommitted {
                transaction_id: c.transaction_id.clone(),
                directive_id: c.directive_id.clone(),
                plan_item_id: c.plan_item_id.clone(),
                requirement_ids: c.requirement_ids.clone(),
                change_kind: match c.change_kind {
                    ChangeKind::SemanticEdit => wire_v4::V4ChangeKind::SemanticEdit,
                    ChangeKind::TextEdit => wire_v4::V4ChangeKind::TextEdit,
                    ChangeKind::ApplyPatch => wire_v4::V4ChangeKind::ApplyPatch,
                    ChangeKind::FileWrite => wire_v4::V4ChangeKind::FileWrite,
                    ChangeKind::Undo => wire_v4::V4ChangeKind::Undo,
                },
                file: c.file.clone(),
                target: match &c.target {
                    ChangeTarget::File => wire_v4::V4ChangeTarget::File,
                    ChangeTarget::SemanticSymbol {
                        symbol_id,
                        before_fingerprint,
                        after_fingerprint,
                    } => wire_v4::V4ChangeTarget::SemanticSymbol {
                        symbol_id: symbol_id.clone(),
                        before_fingerprint: before_fingerprint.clone(),
                        after_fingerprint: after_fingerprint.clone(),
                    },
                },
                before: wire_v4::V4FileStateEvidence {
                    exists: c.before.exists,
                    content_hash: c.before.content_hash.clone(),
                    byte_len: c.before.byte_len,
                },
                after: wire_v4::V4FileStateEvidence {
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
            wire_v4::V4Event::VerificationObserved(wire_v4::V4VerificationObserved {
                directive_id: v.directive_id.clone(),
                plan_item_id: v.plan_item_id.clone(),
                requirement_ids: v.requirement_ids.clone(),
                verification_kind: kind_to_v4(v.verification_kind),
                source: match v.source {
                    VerificationSource::ExecuteProcess => {
                        wire_v4::V4VerificationSource::ExecuteProcess
                    }
                    VerificationSource::TuiTest => wire_v4::V4VerificationSource::TuiTest,
                    VerificationSource::TuiLint => wire_v4::V4VerificationSource::TuiLint,
                },
                command: wire_v4::V4CommandEvidence {
                    program: v.command.program.clone(),
                    args: v.command.args.clone(),
                    cwd: v.command.cwd.clone(),
                },
                outcome: wire_v4::V4VerificationOutcome {
                    success: v.outcome.success,
                    status: v.outcome.status.clone(),
                    exit_code: v.outcome.exit_code,
                    timed_out: v.outcome.timed_out,
                },
                observed_change_ids: v.observed_change_ids.clone(),
                matched_obligations: v
                    .matched_obligations
                    .iter()
                    .map(|r| wire_v4::V4VerificationObligationRef {
                        id: r.id.clone(),
                        binding_hash: r.binding_hash.clone(),
                    })
                    .collect(),
                stdout_excerpt: v.stdout_excerpt.clone(),
                stderr_excerpt: v.stderr_excerpt.clone(),
                output_digest: v.output_digest.clone(),
                output_truncated: v.output_truncated,
                warnings: v.warnings.clone(),
            })
        }
    };
    wire_v4::V4Envelope {
        schema_version: V4_PROVENANCE_SCHEMA_VERSION,
        event_id: env.event_id.clone(),
        session_id: env.session_id.clone(),
        timestamp: env.timestamp.clone(),
        event,
    }
}

/// Explicit v5 conversion through unchanged v4 payload adapters.
pub fn from_v5_wire(env: super::wire::v5::V5Envelope) -> ProvenanceEventEnvelope {
    use super::wire::v5::V5Event;
    let (event, workspace) = match env.event {
        V5Event::DirectiveObserved(v) => (wire_v4::V4Event::DirectiveObserved(v), None),
        V5Event::RequirementChanged(v) => (wire_v4::V4Event::RequirementChanged(v), None),
        V5Event::PlanChanged(v) => (wire_v4::V4Event::PlanChanged(v), None),
        V5Event::ChangeCommitted(v) => (wire_v4::V4Event::ChangeCommitted(v), None),
        V5Event::VerificationObserved(v) => (
            wire_v4::V4Event::VerificationObserved(v.observation),
            v.execution_workspace,
        ),
    };
    let mut canonical = from_v4_wire(wire_v4::V4Envelope {
        schema_version: 4,
        event_id: env.event_id,
        session_id: env.session_id,
        timestamp: env.timestamp,
        event,
    });
    if let ProvenanceEvent::VerificationObserved(v) = &mut canonical.event {
        v.execution_workspace = workspace;
    }
    canonical
}
pub fn to_v5_wire(env: &ProvenanceEventEnvelope) -> super::wire::v5::V5Envelope {
    use super::wire::v5::{V5Envelope, V5Event, V5VerificationObserved};
    let legacy = to_v4_wire(env);
    let event = match legacy.event {
        wire_v4::V4Event::DirectiveObserved(v) => V5Event::DirectiveObserved(v),
        wire_v4::V4Event::RequirementChanged(v) => V5Event::RequirementChanged(v),
        wire_v4::V4Event::PlanChanged(v) => V5Event::PlanChanged(v),
        wire_v4::V4Event::ChangeCommitted(v) => V5Event::ChangeCommitted(v),
        wire_v4::V4Event::VerificationObserved(v) => {
            V5Event::VerificationObserved(V5VerificationObserved {
                observation: v,
                execution_workspace: match &env.event {
                    ProvenanceEvent::VerificationObserved(c) => c.execution_workspace.clone(),
                    _ => None,
                },
            })
        }
    };
    V5Envelope {
        schema_version: V5_PROVENANCE_SCHEMA_VERSION,
        event_id: legacy.event_id,
        session_id: legacy.session_id,
        timestamp: legacy.timestamp,
        event,
    }
}

/// v6 composes the frozen v5 adapter; old formats never acquire host metadata.
pub fn from_v6_wire(env: super::wire::v6::V6Envelope) -> ProvenanceEventEnvelope {
    use super::wire::{v5, v6};
    let (event, context) = match env.event {
        v6::V6Event::DirectiveObserved(v) => (v5::V5Event::DirectiveObserved(v), None),
        v6::V6Event::RequirementChanged(v) => (v5::V5Event::RequirementChanged(v), None),
        v6::V6Event::PlanChanged(v) => (v5::V5Event::PlanChanged(v), None),
        v6::V6Event::ChangeCommitted(v) => (v5::V5Event::ChangeCommitted(v), None),
        v6::V6Event::VerificationObserved(v) => (
            v5::V5Event::VerificationObserved(v.observation),
            v.execution_context,
        ),
    };
    let mut canonical = from_v5_wire(v5::V5Envelope {
        schema_version: V5_PROVENANCE_SCHEMA_VERSION,
        event_id: env.event_id,
        session_id: env.session_id,
        timestamp: env.timestamp,
        event,
    });
    if let ProvenanceEvent::VerificationObserved(v) = &mut canonical.event {
        v.execution_context = context;
    }
    canonical
}
pub fn to_v6_wire(env: &ProvenanceEventEnvelope) -> super::wire::v6::V6Envelope {
    use super::wire::{v5, v6};
    let previous = to_v5_wire(env);
    let event = match previous.event {
        v5::V5Event::DirectiveObserved(v) => v6::V6Event::DirectiveObserved(v),
        v5::V5Event::RequirementChanged(v) => v6::V6Event::RequirementChanged(v),
        v5::V5Event::PlanChanged(v) => v6::V6Event::PlanChanged(v),
        v5::V5Event::ChangeCommitted(v) => v6::V6Event::ChangeCommitted(v),
        v5::V5Event::VerificationObserved(v) => {
            v6::V6Event::VerificationObserved(v6::V6VerificationObserved {
                observation: v,
                execution_context: match &env.event {
                    ProvenanceEvent::VerificationObserved(c) => c.execution_context.clone(),
                    _ => None,
                },
            })
        }
    };
    v6::V6Envelope {
        schema_version: PROVENANCE_SCHEMA_VERSION,
        event_id: previous.event_id,
        session_id: previous.session_id,
        timestamp: previous.timestamp,
        event,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn semantic_change() -> ChangeCommittedEvent {
        ChangeCommittedEvent {
            transaction_id: "evt-1".to_string(),
            directive_id: Some("d1".to_string()),
            plan_item_id: Some("step-2".to_string()),
            requirement_ids: vec!["r1".to_string()],
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
        assert_eq!(env.directive_id(), Some("d1"));
        assert_eq!(env.requirement_ids(), &["r1".to_string()]);
    }

    #[test]
    fn test_event_tag_strings_are_stable() {
        let json = serde_json::to_value(ProvenanceEvent::PlanChanged(PlanChangedEvent {
            directive_id: None,
            changes: vec![],
        }))
        .unwrap();
        assert_eq!(json["type"], "plan_changed");
        let json =
            serde_json::to_value(ProvenanceEvent::ChangeCommitted(semantic_change())).unwrap();
        assert_eq!(json["type"], "change_committed");
        let json =
            serde_json::to_value(ProvenanceEvent::DirectiveObserved(DirectiveObservedEvent {
                origin: DirectiveOrigin::TuiPrompt,
                raw_input: "hi".to_string(),
                raw_input_hash: directive_content_hash("hi"),
                effective_instruction: "hi".to_string(),
                effective_instruction_hash: directive_content_hash("hi"),
            }))
            .unwrap();
        assert_eq!(json["type"], "directive_observed");
        let json = serde_json::to_value(ProvenanceEvent::RequirementChanged(
            RequirementChangedEvent {
                directive_id: "d1".to_string(),
                changes: vec![],
            },
        ))
        .unwrap();
        assert_eq!(json["type"], "requirement_changed");
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
    fn test_directive_hash_format() {
        let h = directive_content_hash("hello");
        assert!(h.starts_with("blake3:"));
        assert_eq!(h.len(), "blake3:".len() + 64);
        assert_eq!(h, directive_content_hash("hello"));
        assert_ne!(h, directive_content_hash("hello "));
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
                assert_eq!(c.directive_id, None);
                assert!(c.requirement_ids.is_empty());
            }
            _ => panic!("expected change"),
        }
    }

    #[test]
    fn test_v2_conversion_defaults_attribution() {
        let v2 = wire_v2::V2Envelope {
            schema_version: 2,
            event_id: "evt-v2".to_string(),
            session_id: "s".to_string(),
            timestamp: "2026-01-01T00:00:00+00:00".to_string(),
            event: wire_v2::V2Event::PlanChanged(wire_v2::V2PlanChanged { changes: vec![] }),
        };
        let canonical = from_v2_wire(v2);
        match &canonical.event {
            ProvenanceEvent::PlanChanged(p) => {
                assert_eq!(p.directive_id, None);
                assert!(p.changes.is_empty());
            }
            _ => panic!("expected plan"),
        }
    }

    #[test]
    fn test_v3_roundtrip_preserves_attribution() {
        let env = ProvenanceEventEnvelope {
            schema_version: PROVENANCE_SCHEMA_VERSION,
            event_id: "evt-v3".to_string(),
            session_id: "s".to_string(),
            timestamp: "2026-01-01T00:00:00+00:00".to_string(),
            event: ProvenanceEvent::ChangeCommitted(semantic_change()),
        };
        let wire = to_v3_wire(&env);
        let back = from_v3_wire(wire);
        match &back.event {
            ProvenanceEvent::ChangeCommitted(c) => {
                assert_eq!(c.directive_id.as_deref(), Some("d1"));
                assert_eq!(c.requirement_ids, vec!["r1".to_string()]);
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
