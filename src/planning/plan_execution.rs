use crate::planning::plan_status::PlanStatus;
use crate::planning::task_types::*;
use serde::{Deserialize, Serialize};

/// Information of plan being executed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanExecution {
    pub plan: TaskPlan,
    pub status: PlanStatus,
    pub current_step_index: usize,
    pub completed_steps: Vec<StepResult>,
    pub start_time: Option<chrono::DateTime<chrono::Utc>>,
    pub end_time: Option<chrono::DateTime<chrono::Utc>>,
    pub error_message: Option<String>,
    pub pause_reason: Option<String>,
}
