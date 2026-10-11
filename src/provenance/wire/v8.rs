//! v8 adds read-only execution identity to the frozen v7 observation.
//! Other payloads reuse frozen v4 wire types. Snapshot format version one is
//! frozen in features::verification_snapshot::model; it must evolve explicitly.
use super::{v4, v7};
use crate::features::verification_context::identity::ExecutionIdentity;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V8Envelope {
    pub schema_version: u32,
    pub event_id: String,
    pub session_id: String,
    pub timestamp: String,
    pub event: V8Event,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum V8Event {
    DirectiveObserved(v4::V4DirectiveObserved),
    RequirementChanged(v4::V4RequirementChanged),
    PlanChanged(v4::V4PlanChanged),
    ChangeCommitted(v4::V4ChangeCommitted),
    VerificationObserved(V8VerificationObserved),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V8VerificationObserved {
    #[serde(flatten)]
    pub observation: v7::V7VerificationObserved,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_identity: Option<ExecutionIdentity>,
}
