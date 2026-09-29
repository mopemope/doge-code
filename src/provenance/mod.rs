//! Plan-to-Evidence Provenance Graph v1.
//!
//! Doge-observed development provenance: which plan step was active when a
//! semantic transaction committed, and which verification commands observed
//! those changes afterwards.
//!
//! Durable state lives under the session directory
//! (`.doge/sessions/<id>/provenance/v1/events/<uuid>.json`), never in
//! `repomap.sqlite` (rebuildable analysis cache) nor the legacy `action_log`.
//! One event is one JSON file; writes are atomic via a sibling temp file +
//! `persist_noclobber`. A single corrupt event never fails the whole query.

pub mod query;
pub mod store;
pub mod types;
pub mod verification;

pub use query::{
    ActiveChangeState, ProvenanceCoverage, ResolvedChangeState, active_change_ids,
    compute_coverage, resolve_active_states,
};
pub use store::{ProvenanceLoadResult, ProvenanceStore};
pub use types::{
    ChangeCommittedEvent, ChangeKind, CommandEvidence, PROVENANCE_SCHEMA_VERSION, PlanChangedEvent,
    PlanItemTransition, ProvenanceEvent, ProvenanceEventEnvelope, ProvenanceEventType,
    VerificationContext, VerificationKind, VerificationObservedEvent, VerificationOutcome,
    VerificationSource,
};
pub use verification::{
    VerificationRecordInput, build_verification_event, capture_verification_context,
    classify_verification, current_in_progress_plan_item, diff_hash_for,
};
