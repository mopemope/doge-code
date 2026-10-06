//! v7 adds bounded structured results to the frozen v6 observation.
//! Other payloads reuse frozen v4 wire types. Snapshot format version one is
//! frozen in features::verification_snapshot::model; it must evolve explicitly.
use super::{v4, v6};
use crate::features::structured_test_results::StructuredTestResult;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V7Envelope {
    pub schema_version: u32,
    pub event_id: String,
    pub session_id: String,
    pub timestamp: String,
    pub event: V7Event,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum V7Event {
    DirectiveObserved(v4::V4DirectiveObserved),
    RequirementChanged(v4::V4RequirementChanged),
    PlanChanged(v4::V4PlanChanged),
    ChangeCommitted(v4::V4ChangeCommitted),
    VerificationObserved(V7VerificationObserved),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V7VerificationObserved {
    #[serde(flatten)]
    pub observation: v6::V6VerificationObserved,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_test_result: Option<Box<StructuredTestResult>>,
}
