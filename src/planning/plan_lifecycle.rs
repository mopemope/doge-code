use crate::planning::plan_execution::PlanExecution;
use crate::planning::plan_status::PlanStatus;
use crate::planning::task_types::*;
use anyhow::{Result, anyhow};
use std::collections::HashMap;

#[derive(Debug)]
pub struct PlanLifecycleManager {
    /// Active plans (in memory)
    active_plans: HashMap<String, PlanExecution>,
    /// Recent plans (history)
    recent_plans: Vec<PlanExecution>,
    /// Maximum history retention count
    max_history: usize,
}

impl PlanLifecycleManager {
    pub fn new(max_history: usize) -> Self {
        Self {
            active_plans: HashMap::new(),
            recent_plans: Vec::new(),
            max_history,
        }
    }

    /// Register new plan
    pub fn register_plan(&mut self, plan: TaskPlan) -> String {
        let plan_id = plan.id.clone();

        let execution = PlanExecution {
            plan,
            status: PlanStatus::Created,
            current_step_index: 0,
            completed_steps: Vec::new(),
            start_time: None,
            end_time: None,
            error_message: None,
            pause_reason: None,
        };

        // メモリに保存
        self.active_plans.insert(plan_id.clone(), execution);

        plan_id
    }

    /// 計画を取得
    pub fn get_plan(&self, plan_id: &str) -> Option<PlanExecution> {
        self.active_plans.get(plan_id).cloned()
    }

    /// アクティブな計画一覧を取得
    pub fn list_active_plans(&self) -> Vec<PlanExecution> {
        self.active_plans.values().cloned().collect()
    }

    /// Get recent plan history
    pub fn get_recent_plans(&self) -> Vec<PlanExecution> {
        self.recent_plans.clone()
    }

    /// Start plan execution
    pub fn start_execution(&mut self, plan_id: &str) -> Result<()> {
        if let Some(execution) = self.active_plans.get_mut(plan_id) {
            if execution.status != PlanStatus::Created && execution.status != PlanStatus::Paused {
                return Err(anyhow!(
                    "Plan {} is not in a startable state: {:?}",
                    plan_id,
                    execution.status
                ));
            }

            execution.status = PlanStatus::Running;
            execution.start_time = Some(chrono::Utc::now());
            execution.error_message = None;
            execution.pause_reason = None;
        } else {
            return Err(anyhow!("Plan {} not found", plan_id));
        }

        Ok(())
    }

    /// Pause plan execution
    pub fn pause_execution(&mut self, plan_id: &str, reason: Option<String>) -> Result<()> {
        if let Some(execution) = self.active_plans.get_mut(plan_id) {
            if execution.status != PlanStatus::Running {
                return Err(anyhow!("Plan {} is not running", plan_id));
            }

            execution.status = PlanStatus::Paused;
            execution.pause_reason = reason;
        } else {
            return Err(anyhow!("Plan {} not found", plan_id));
        }

        Ok(())
    }

    /// Complete plan execution
    pub fn complete_execution(&mut self, plan_id: &str, result: ExecutionResult) -> Result<()> {
        if let Some(mut execution) = self.active_plans.remove(plan_id) {
            execution.status = if result.success {
                PlanStatus::Completed
            } else {
                PlanStatus::Failed
            };
            execution.end_time = Some(chrono::Utc::now());
            execution.completed_steps = result.completed_steps;
            if !result.success {
                execution.error_message = Some(result.final_message);
            }

            // 履歴に追加
            self.recent_plans.push(execution.clone());

            // 履歴サイズ制限
            if self.recent_plans.len() > self.max_history {
                self.recent_plans.remove(0);
            }
        } else {
            return Err(anyhow!("Plan {} not found", plan_id));
        }

        Ok(())
    }

    /// Cancel plan execution
    pub fn cancel_execution(&mut self, plan_id: &str) -> Result<()> {
        if let Some(mut execution) = self.active_plans.remove(plan_id) {
            execution.status = PlanStatus::Cancelled;
            execution.end_time = Some(chrono::Utc::now());

            // 履歴に追加
            self.recent_plans.push(execution.clone());

            if self.recent_plans.len() > self.max_history {
                self.recent_plans.remove(0);
            }
        } else {
            return Err(anyhow!("Plan {} not found", plan_id));
        }

        Ok(())
    }

    /// Record step completion
    pub fn record_step_completion(&mut self, plan_id: &str, step_result: StepResult) -> Result<()> {
        if let Some(execution) = self.active_plans.get_mut(plan_id) {
            execution.completed_steps.push(step_result);
            execution.current_step_index += 1;
        } else {
            return Err(anyhow!("Plan {} not found", plan_id));
        }

        Ok(())
    }

    /// 最新の計画を取得（最後に作成された計画）
    pub fn get_latest_plan(&self) -> Option<PlanExecution> {
        // 最新の作成時刻の計画を探す
        self.active_plans
            .values()
            .max_by_key(|execution| execution.plan.created_at)
            .cloned()
    }

    /// Search for executable plans (keyword matching)
    pub fn find_executable_plan(&self, user_input: &str) -> Option<PlanExecution> {
        // ユーザー入力に基づいて計画を検索
        let input_lower = user_input.to_lowercase();

        // 実行関連のキーワードをチェック
        let execution_keywords = [
            "実行",
            "execute",
            "run",
            "開始",
            "start",
            "実施",
            "進める",
            "やって",
            "計画",
            "plan",
            "上記",
            "これ",
            "それ",
            "この計画",
            "その計画",
        ];

        let has_execution_keyword = execution_keywords
            .iter()
            .any(|keyword| input_lower.contains(keyword));

        if !has_execution_keyword {
            return None;
        }

        // 最新の Created 状態の計画を返す
        self.active_plans
            .values()
            .filter(|execution| execution.status == PlanStatus::Created)
            .max_by_key(|execution| execution.plan.created_at)
            .cloned()
    }

    /// Get statistics
    pub fn get_statistics(&self) -> crate::planning::plan_statistics::PlanStatistics {
        let mut completed_plans = 0;
        let mut failed_plans = 0;
        let mut cancelled_plans = 0;
        let mut total_duration = 0i64;
        let mut completed_count = 0;

        for execution in self.recent_plans.iter() {
            match execution.status {
                PlanStatus::Completed => {
                    completed_plans += 1;
                    if let (Some(start), Some(end)) = (execution.start_time, execution.end_time) {
                        total_duration += (end - start).num_seconds();
                        completed_count += 1;
                    }
                }
                PlanStatus::Failed => failed_plans += 1,
                PlanStatus::Cancelled => cancelled_plans += 1,
                _ => {}
            }
        }

        let total_plans = self.active_plans.len() + self.recent_plans.len();
        let active_plans = self.active_plans.len();
        let average_completion_time = if completed_count > 0 {
            total_duration as f64 / completed_count as f64
        } else {
            0.0
        };

        crate::planning::plan_statistics::PlanStatistics {
            total_plans,
            active_plans,
            completed_plans,
            failed_plans,
            cancelled_plans,
            average_completion_time,
        }
    }
}
