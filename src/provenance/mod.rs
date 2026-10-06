//! Plan-to-Evidence Provenance Graph v6.
//!
//! Doge-observed development provenance: which user directive motivated which
//! agent-structured requirement, which plan step was active when a workspace
//! mutation committed, and which verification commands observed those changes
//! afterwards.
//!
//! Durable state lives under the session directory
//! (`.doge/sessions/<id>/provenance/v6/events/<uuid>.json` for new writes;
//! legacy v1/v2/v3 under `provenance/v1/events/`, `provenance/v2/events/` and
//! `provenance/v3/events/` and `provenance/v4/events/` and `provenance/v5/events/` remain readable but are never written or physically
//! migrated), never in `repomap.sqlite` (rebuildable analysis cache) nor the
//! legacy `action_log`.
//! One event is one JSON file; writes use sibling-temp + no-clobber
//! persistence. A single corrupt event never fails the whole query.
//!
//! Observed user directives and agent-interpreted requirements are distinct
//! provenance nodes. Never treat an inferred requirement as a verbatim user
//! directive.

pub mod context;
pub mod obligations;
pub mod query;
pub mod requirements;
pub mod store;
#[cfg(test)]
mod traceability_tests;
pub mod types;
pub mod verification;
pub mod wire;

pub use context::ProvenanceAttribution;
pub use query::{
    ActiveChangeState, ProvenanceCoverage, ResolvedChangeState, active_change_ids,
    compute_coverage, resolve_active_states,
};
pub use store::{ProvenanceLoadResult, ProvenanceStore};
pub use types::{
    ChangeCommittedEvent, ChangeKind, ChangeTarget, CommandEvidence, DirectiveObservedEvent,
    DirectiveOrigin, FileStateEvidence, LEGACY_PROVENANCE_SCHEMA_VERSION,
    PROVENANCE_SCHEMA_VERSION, PlanChangedEvent, PlanItemTransition, ProvenanceEvent,
    ProvenanceEventEnvelope, ProvenanceEventType, RequirementChangedEvent, RequirementSnapshot,
    RequirementStatus, RequirementTransition, V2_PROVENANCE_SCHEMA_VERSION,
    V3_PROVENANCE_SCHEMA_VERSION, VerificationCommandMatcher, VerificationContext,
    VerificationKind, VerificationObligation, VerificationObligationRef, VerificationObservedEvent,
    VerificationOutcome, VerificationSource, directive_content_hash, file_content_hash,
};
pub use verification::{
    VerificationRecordInput, build_verification_event, capture_verification_context,
    capture_verification_context_full, classify_verification, current_in_progress_plan_item,
    diff_hash_for,
};
