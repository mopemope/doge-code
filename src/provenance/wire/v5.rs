//! v5 adds the explicitly versioned execution-workspace record to verification.
//! Other payloads reuse frozen v4 wire types. Snapshot format version one is
//! frozen in features::verification_snapshot::model; it must evolve explicitly.
use super::v4;
use crate::features::verification_snapshot::ExecutionWorkspace;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V5Envelope {
    pub schema_version: u32,
    pub event_id: String,
    pub session_id: String,
    pub timestamp: String,
    pub event: V5Event,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum V5Event {
    DirectiveObserved(v4::V4DirectiveObserved),
    RequirementChanged(v4::V4RequirementChanged),
    PlanChanged(v4::V4PlanChanged),
    ChangeCommitted(v4::V4ChangeCommitted),
    VerificationObserved(V5VerificationObserved),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V5VerificationObserved {
    #[serde(flatten)]
    pub observation: v4::V4VerificationObserved,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_workspace: Option<Box<ExecutionWorkspace>>,
}
