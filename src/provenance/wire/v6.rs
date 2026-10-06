//! v6 adds frozen minimal execution context to the frozen v5 observation.
//! Other payloads reuse frozen v4 wire types. Snapshot format version one is
//! frozen in features::verification_snapshot::model; it must evolve explicitly.
use super::{v4, v5};
use crate::features::verification_context::ExecutionContext;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V6Envelope {
    pub schema_version: u32,
    pub event_id: String,
    pub session_id: String,
    pub timestamp: String,
    pub event: V6Event,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum V6Event {
    DirectiveObserved(v4::V4DirectiveObserved),
    RequirementChanged(v4::V4RequirementChanged),
    PlanChanged(v4::V4PlanChanged),
    ChangeCommitted(v4::V4ChangeCommitted),
    VerificationObserved(V6VerificationObserved),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V6VerificationObserved {
    #[serde(flatten)]
    pub observation: v5::V5VerificationObserved,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_context: Option<Box<ExecutionContext>>,
}
