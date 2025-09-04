use serde::{Deserialize, Serialize};

/// 計画統計情報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStatistics {
    pub total_plans: usize,
    pub active_plans: usize,
    pub completed_plans: usize,
    pub failed_plans: usize,
    pub cancelled_plans: usize,
    pub average_completion_time: f64, // 秒
}

impl Default for PlanStatistics {
    fn default() -> Self {
        Self {
            total_plans: 0,
            active_plans: 0,
            completed_plans: 0,
            failed_plans: 0,
            cancelled_plans: 0,
            average_completion_time: 0.0,
        }
    }
}
