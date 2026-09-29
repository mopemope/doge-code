//! Plan-to-Evidence Provenance Graph v2.
//!
//! Doge-observed development provenance: which plan step was active when a
//! workspace mutation committed, and which verification commands observed
//! those changes afterwards.
//!
//! Durable state lives under the session directory
//! (`.doge/sessions/<id>/provenance/v2/events/<uuid>.json` for new writes;
//! legacy v1 under `provenance/v1/events/` remains readable but is never
//! written or physically migrated), never in `repomap.sqlite` (rebuildable
//! analysis cache) nor the legacy `action_log`.
//! One event is one JSON file; writes use sibling-temp + no-clobber
//! persistence. A single corrupt event never fails the whole query.

pub mod query;
pub mod store;
pub mod types;
pub mod verification;
pub mod wire;

pub use query::{
    ActiveChangeState, ProvenanceCoverage, ResolvedChangeState, active_change_ids,
    compute_coverage, resolve_active_states,
};
pub use store::{ProvenanceLoadResult, ProvenanceStore};
pub use types::{
    ChangeCommittedEvent, ChangeKind, ChangeTarget, CommandEvidence, FileStateEvidence,
    LEGACY_PROVENANCE_SCHEMA_VERSION, PROVENANCE_SCHEMA_VERSION, PlanChangedEvent,
    PlanItemTransition, ProvenanceEvent, ProvenanceEventEnvelope, ProvenanceEventType,
    VerificationContext, VerificationKind, VerificationObservedEvent, VerificationOutcome,
    VerificationSource, file_content_hash,
};
pub use verification::{
    VerificationRecordInput, build_verification_event, capture_verification_context,
    classify_verification, current_in_progress_plan_item, diff_hash_for,
};
