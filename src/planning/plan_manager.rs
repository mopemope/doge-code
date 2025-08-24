use crate::planning::task_types::*;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::info;

/// Plan execution status
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanStatus {
    /// 作成済み、実行待ち
    Created,
    /// 実行中
    Running,
    /// 一時停止中
    Paused,
    /// 正常完了
    Completed,
    /// エラーで失敗
    Failed,
    /// ユーザーによりキャンセル
    Cancelled,
}

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

/// Plan management system
#[derive(Debug)]
pub struct PlanManager {
    /// Active plans (in memory)
    active_plans: Arc<Mutex<HashMap<String, PlanExecution>>>,
    /// Recent plans (history)
    recent_plans: Arc<Mutex<Vec<PlanExecution>>>,
    /// Currently executing plan ID
    current_execution: Arc<Mutex<Option<String>>>,
    /// Directory to save plans
    plans_dir: PathBuf,
    /// Maximum history retention count
    max_history: usize,
}

impl PlanManager {
    /// 新しい計画管理システムを作成
    pub fn new() -> Result<Self> {
        let plans_dir = Self::get_plans_directory()?;
        std::fs::create_dir_all(&plans_dir)?;

        Ok(Self {
            active_plans: Arc::new(Mutex::new(HashMap::new())),
            recent_plans: Arc::new(Mutex::new(Vec::new())),
            current_execution: Arc::new(Mutex::new(None)),
            plans_dir,
            max_history: 50,
        })
    }

    /// Get plan save directory
    fn get_plans_directory() -> Result<PathBuf> {
        let config_dir =
            dirs::config_dir().ok_or_else(|| anyhow!("Could not determine config directory"))?;
        Ok(config_dir.join("doge-code").join("plans"))
    }

    /// Register new plan
    pub fn register_plan(&self, plan: TaskPlan) -> Result<String> {
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
        {
            let mut active_plans = self.active_plans.lock().unwrap();
            active_plans.insert(plan_id.clone(), execution.clone());
        }

        // ディスクに永続化
        self.save_plan_to_disk(&execution)?;

        info!("Registered new plan: {}", plan_id);
        Ok(plan_id)
    }

    /// 計画を取得
    pub fn get_plan(&self, plan_id: &str) -> Option<PlanExecution> {
        let active_plans = self.active_plans.lock().unwrap();
        active_plans.get(plan_id).cloned()
    }

    /// アクティブな計画一覧を取得
    pub fn list_active_plans(&self) -> Vec<PlanExecution> {
        let active_plans = self.active_plans.lock().unwrap();
        active_plans.values().cloned().collect()
    }

    /// Get recent plan history
    pub fn get_recent_plans(&self) -> Vec<PlanExecution> {
        let recent_plans = self.recent_plans.lock().unwrap();
        recent_plans.clone()
    }

    /// Get currently executing plan ID
    pub fn get_current_execution(&self) -> Option<String> {
        let current = self.current_execution.lock().unwrap();
        current.clone()
    }

    /// Start plan execution
    pub fn start_execution(&self, plan_id: &str) -> Result<()> {
        {
            let mut active_plans = self.active_plans.lock().unwrap();
            if let Some(execution) = active_plans.get_mut(plan_id) {
                if execution.status != PlanStatus::Created && execution.status != PlanStatus::Paused
                {
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
        }

        // 現在の実行を設定
        {
            let mut current = self.current_execution.lock().unwrap();
            *current = Some(plan_id.to_string());
        }

        info!("Started execution of plan: {}", plan_id);
        Ok(())
    }

    /// Pause plan execution
    pub fn pause_execution(&self, plan_id: &str, reason: Option<String>) -> Result<()> {
        {
            let mut active_plans = self.active_plans.lock().unwrap();
            if let Some(execution) = active_plans.get_mut(plan_id) {
                if execution.status != PlanStatus::Running {
                    return Err(anyhow!("Plan {} is not running", plan_id));
                }

                execution.status = PlanStatus::Paused;
                execution.pause_reason = reason;
            } else {
                return Err(anyhow!("Plan {} not found", plan_id));
            }
        }

        // 現在の実行をクリア
        {
            let mut current = self.current_execution.lock().unwrap();
            if current.as_ref() == Some(&plan_id.to_string()) {
                *current = None;
            }
        }

        info!("Paused execution of plan: {}", plan_id);
        Ok(())
    }

    /// Complete plan execution
    pub fn complete_execution(&self, plan_id: &str, result: ExecutionResult) -> Result<()> {
        let execution = {
            let mut active_plans = self.active_plans.lock().unwrap();
            if let Some(mut execution) = active_plans.remove(plan_id) {
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
                execution
            } else {
                return Err(anyhow!("Plan {} not found", plan_id));
            }
        };

        // 履歴に追加
        {
            let mut recent_plans = self.recent_plans.lock().unwrap();
            recent_plans.push(execution.clone());

            // 履歴サイズ制限
            if recent_plans.len() > self.max_history {
                recent_plans.remove(0);
            }
        }

        // 現在の実行をクリア
        {
            let mut current = self.current_execution.lock().unwrap();
            if current.as_ref() == Some(&plan_id.to_string()) {
                *current = None;
            }
        }

        // ディスクに保存
        self.save_plan_to_disk(&execution)?;

        info!(
            "Completed execution of plan: {} (success: {})",
            plan_id, result.success
        );
        Ok(())
    }

    /// Cancel plan execution
    pub fn cancel_execution(&self, plan_id: &str) -> Result<()> {
        let execution = {
            let mut active_plans = self.active_plans.lock().unwrap();
            if let Some(mut execution) = active_plans.remove(plan_id) {
                execution.status = PlanStatus::Cancelled;
                execution.end_time = Some(chrono::Utc::now());
                execution
            } else {
                return Err(anyhow!("Plan {} not found", plan_id));
            }
        };

        // 履歴に追加
        {
            let mut recent_plans = self.recent_plans.lock().unwrap();
            recent_plans.push(execution.clone());

            if recent_plans.len() > self.max_history {
                recent_plans.remove(0);
            }
        }

        // 現在の実行をクリア
        {
            let mut current = self.current_execution.lock().unwrap();
            if current.as_ref() == Some(&plan_id.to_string()) {
                *current = None;
            }
        }

        info!("Cancelled execution of plan: {}", plan_id);
        Ok(())
    }

    /// Record step completion
    pub fn record_step_completion(&self, plan_id: &str, step_result: StepResult) -> Result<()> {
        let execution_clone = {
            let mut active_plans = self.active_plans.lock().unwrap();
            if let Some(execution) = active_plans.get_mut(plan_id) {
                execution.completed_steps.push(step_result);
                execution.current_step_index += 1;
                execution.clone()
            } else {
                return Err(anyhow!("Plan {} not found", plan_id));
            }
        };

        // ディスクに保存
        self.save_plan_to_disk(&execution_clone)?;
        Ok(())
    }

    /// 最新の計画を取得（最後に作成された計画）
    pub fn get_latest_plan(&self) -> Option<PlanExecution> {
        let active_plans = self.active_plans.lock().unwrap();

        // 最新の作成時刻の計画を探す
        active_plans
            .values()
            .max_by_key(|execution| execution.plan.created_at)
            .cloned()
    }

    /// Search for executable plans (keyword matching)
    pub fn find_executable_plan(&self, user_input: &str) -> Option<PlanExecution> {
        let active_plans = self.active_plans.lock().unwrap();

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
        active_plans
            .values()
            .filter(|execution| execution.status == PlanStatus::Created)
            .max_by_key(|execution| execution.plan.created_at)
            .cloned()
    }

    /// 計画をディスクに保存
    fn save_plan_to_disk(&self, execution: &PlanExecution) -> Result<()> {
        let file_path = self.plans_dir.join(format!("{}.json", execution.plan.id));
        let json_data = serde_json::to_string_pretty(execution)?;
        std::fs::write(file_path, json_data)?;
        Ok(())
    }

    /// Load plan from disk
    pub fn load_plan_from_disk(&self, plan_id: &str) -> Result<PlanExecution> {
        let file_path = self.plans_dir.join(format!("{}.json", plan_id));
        let json_data = std::fs::read_to_string(file_path)?;
        let execution: PlanExecution = serde_json::from_str(&json_data)?;
        Ok(execution)
    }

    /// Clean up old plan files
    pub fn cleanup_old_plans(&self, days: u64) -> Result<usize> {
        let cutoff_time = chrono::Utc::now() - chrono::Duration::days(days as i64);
        let mut cleaned_count = 0;

        if let Ok(entries) = std::fs::read_dir(&self.plans_dir) {
            for entry in entries.flatten() {
                if let Ok(metadata) = entry.metadata()
                    && let Ok(created) = metadata.created()
                {
                    let created_time = chrono::DateTime::<chrono::Utc>::from(created);
                    if created_time < cutoff_time && std::fs::remove_file(entry.path()).is_ok() {
                        cleaned_count += 1;
                    }
                }
            }
        }

        info!("Cleaned up {} old plan files", cleaned_count);
        Ok(cleaned_count)
    }

    /// Get statistics
    pub fn get_statistics(&self) -> PlanStatistics {
        let active_plans = self.active_plans.lock().unwrap();
        let recent_plans = self.recent_plans.lock().unwrap();

        let mut stats = PlanStatistics {
            total_plans: active_plans.len() + recent_plans.len(),
            active_plans: active_plans.len(),
            completed_plans: 0,
            failed_plans: 0,
            cancelled_plans: 0,
            average_completion_time: 0.0,
        };

        let mut total_duration = 0i64;
        let mut completed_count = 0;

        for execution in recent_plans.iter() {
            match execution.status {
                PlanStatus::Completed => {
                    stats.completed_plans += 1;
                    if let (Some(start), Some(end)) = (execution.start_time, execution.end_time) {
                        total_duration += (end - start).num_seconds();
                        completed_count += 1;
                    }
                }
                PlanStatus::Failed => stats.failed_plans += 1,
                PlanStatus::Cancelled => stats.cancelled_plans += 1,
                _ => {}
            }
        }

        if completed_count > 0 {
            stats.average_completion_time = total_duration as f64 / completed_count as f64;
        }

        stats
    }
}

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

impl Default for PlanManager {
    fn default() -> Self {
        Self::new().expect("Failed to create PlanManager")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_plan() -> TaskPlan {
        TaskPlan {
            id: "test-plan-123".to_string(),
            original_request: "Test task".to_string(),
            classification: TaskClassification {
                task_type: TaskType::SimpleCodeEdit,
                complexity_score: 0.5,
                estimated_steps: 3,
                risk_level: RiskLevel::Low,
                required_tools: vec!["fs_read".to_string(), "edit".to_string()],
                confidence: 0.9,
            },
            steps: vec![
                TaskStep::new(
                    "step1".to_string(),
                    "Test step 1".to_string(),
                    StepType::Analysis,
                    vec!["fs_read".to_string()],
                ),
                TaskStep::new(
                    "step2".to_string(),
                    "Test step 2".to_string(),
                    StepType::Implementation,
                    vec!["edit".to_string()],
                )
                .with_dependencies(vec!["step1".to_string()]),
            ],
            total_estimated_duration: 600,
            created_at: chrono::Utc::now(),
        }
    }

    fn create_test_plan_manager() -> Result<(PlanManager, TempDir)> {
        // テスト用の一時ディレクトリを使用
        let temp_dir = TempDir::new()?;
        let plans_dir = temp_dir.path().join("plans");
        std::fs::create_dir_all(&plans_dir)?;

        let manager = PlanManager {
            active_plans: Arc::new(Mutex::new(HashMap::new())),
            recent_plans: Arc::new(Mutex::new(Vec::new())),
            current_execution: Arc::new(Mutex::new(None)),
            plans_dir,
            max_history: 50,
        };

        Ok((manager, temp_dir))
    }

    #[test]
    fn test_register_plan() {
        let (manager, _temp_dir) = create_test_plan_manager().unwrap();
        let plan = create_test_plan();
        let plan_id = plan.id.clone();

        let result = manager.register_plan(plan);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), plan_id);

        // 計画が登録されていることを確認
        let retrieved_plan = manager.get_plan(&plan_id);
        assert!(retrieved_plan.is_some());
        assert_eq!(retrieved_plan.unwrap().plan.id, plan_id);
    }

    #[test]
    fn test_plan_execution_lifecycle() {
        let (manager, _temp_dir) = create_test_plan_manager().unwrap();
        let plan = create_test_plan();
        let plan_id = plan.id.clone();

        // 計画を登録
        manager.register_plan(plan).unwrap();

        // 実行開始
        assert!(manager.start_execution(&plan_id).is_ok());
        assert_eq!(manager.get_current_execution(), Some(plan_id.clone()));

        // 実行状態を確認
        let execution = manager.get_plan(&plan_id).unwrap();
        assert_eq!(execution.status, PlanStatus::Running);

        // 実行完了
        let result = ExecutionResult {
            plan_id: plan_id.clone(),
            success: true,
            completed_steps: vec![],
            total_duration: 300,
            final_message: "Success".to_string(),
        };
        assert!(manager.complete_execution(&plan_id, result).is_ok());

        // 現在の実行がクリアされていることを確認
        assert_eq!(manager.get_current_execution(), None);

        // 履歴に移動していることを確認
        let recent_plans = manager.get_recent_plans();
        assert_eq!(recent_plans.len(), 1);
        assert_eq!(recent_plans[0].status, PlanStatus::Completed);
    }

    #[test]
    fn test_find_executable_plan() {
        let (manager, _temp_dir) = create_test_plan_manager().unwrap();
        let plan = create_test_plan();

        // 計画を登録
        manager.register_plan(plan).unwrap();

        // 実行可能な計画を検索
        let found_plan = manager.find_executable_plan("この計画を実行してください");
        assert!(found_plan.is_some());

        let found_plan = manager.find_executable_plan("execute this plan");
        assert!(found_plan.is_some());

        let found_plan = manager.find_executable_plan("何か別のこと");
        assert!(found_plan.is_none());
    }

    #[test]
    fn test_get_latest_plan() {
        let (manager, _temp_dir) = create_test_plan_manager().unwrap();

        // 最初は計画がない
        assert!(manager.get_latest_plan().is_none());

        // 計画を追加
        let mut plan1 = create_test_plan();
        plan1.id = "plan1".to_string();
        plan1.created_at = chrono::Utc::now() - chrono::Duration::minutes(10);

        let mut plan2 = create_test_plan();
        plan2.id = "plan2".to_string();
        plan2.created_at = chrono::Utc::now();

        manager.register_plan(plan1).unwrap();
        manager.register_plan(plan2).unwrap();

        // 最新の計画を取得
        let latest = manager.get_latest_plan().unwrap();
        assert_eq!(latest.plan.id, "plan2");
    }

    #[test]
    fn test_statistics() {
        let (manager, _temp_dir) = create_test_plan_manager().unwrap();
        let plan = create_test_plan();
        let plan_id = plan.id.clone();

        // 初期統計
        let stats = manager.get_statistics();
        assert_eq!(stats.total_plans, 0);
        assert_eq!(stats.active_plans, 0);

        // 計画を登録
        manager.register_plan(plan).unwrap();

        let stats = manager.get_statistics();
        assert_eq!(stats.total_plans, 1);
        assert_eq!(stats.active_plans, 1);

        // 実行完了
        manager.start_execution(&plan_id).unwrap();
        let result = ExecutionResult {
            plan_id: plan_id.clone(),
            success: true,
            completed_steps: vec![],
            total_duration: 300,
            final_message: "Success".to_string(),
        };
        manager.complete_execution(&plan_id, result).unwrap();

        let stats = manager.get_statistics();
        assert_eq!(stats.total_plans, 1);
        assert_eq!(stats.active_plans, 0);
        assert_eq!(stats.completed_plans, 1);
    }

    #[test]
    fn test_pause_and_cancel() {
        let (manager, _temp_dir) = create_test_plan_manager().unwrap();
        let plan = create_test_plan();
        let plan_id = plan.id.clone();

        // 計画を登録して実行開始
        manager.register_plan(plan).unwrap();
        manager.start_execution(&plan_id).unwrap();

        // 一時停止
        assert!(
            manager
                .pause_execution(&plan_id, Some("User requested".to_string()))
                .is_ok()
        );
        let execution = manager.get_plan(&plan_id).unwrap();
        assert_eq!(execution.status, PlanStatus::Paused);

        // キャンセル
        assert!(manager.cancel_execution(&plan_id).is_ok());

        // 履歴に移動していることを確認
        let recent_plans = manager.get_recent_plans();
        assert_eq!(recent_plans.len(), 1);
        assert_eq!(recent_plans[0].status, PlanStatus::Cancelled);
    }
}
