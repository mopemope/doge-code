use crate::planning::plan_execution::PlanExecution;
use crate::planning::plan_lifecycle::PlanLifecycleManager;
use crate::planning::plan_statistics::PlanStatistics;
use crate::planning::plan_storage::PlanStorage;
use crate::planning::task_types::*;
use anyhow::Result;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::info;

/// Plan management system
#[derive(Debug)]
pub struct PlanManager {
    /// Plan lifecycle manager
    lifecycle_manager: Arc<Mutex<PlanLifecycleManager>>,
    /// Currently executing plan ID
    current_execution: Arc<Mutex<Option<String>>>,
    /// Plan storage manager
    storage_manager: Arc<PlanStorage>,
}

impl PlanManager {
    /// 新しい計画管理システムを作成
    pub fn new(project_root: PathBuf) -> Result<Self> {
        let plans_dir = Self::get_plans_directory(project_root)?;
        std::fs::create_dir_all(&plans_dir)?;

        let lifecycle_manager = PlanLifecycleManager::new(50);
        let storage_manager = PlanStorage::new(plans_dir);

        Ok(Self {
            lifecycle_manager: Arc::new(Mutex::new(lifecycle_manager)),
            current_execution: Arc::new(Mutex::new(None)),
            storage_manager: Arc::new(storage_manager),
        })
    }

    /// Get plan save directory
    fn get_plans_directory(project_root: PathBuf) -> Result<PathBuf> {
        Ok(project_root.join(".doge").join("plans"))
    }

    /// Register new plan
    pub fn register_plan(&self, plan: TaskPlan) -> Result<String> {
        let plan_id = {
            let mut lifecycle_manager = self.lifecycle_manager.lock().unwrap();
            lifecycle_manager.register_plan(plan)
        };

        // ディスクに永続化
        {
            let lifecycle_manager = self.lifecycle_manager.lock().unwrap();
            if let Some(execution) = lifecycle_manager.get_plan(&plan_id) {
                self.storage_manager.save_plan_to_disk(&execution)?;
            }
        }

        info!("Registered new plan: {}", plan_id);
        Ok(plan_id)
    }

    /// 計画を取得
    pub fn get_plan(&self, plan_id: &str) -> Option<PlanExecution> {
        let lifecycle_manager = self.lifecycle_manager.lock().unwrap();
        lifecycle_manager.get_plan(plan_id)
    }

    /// アクティブな計画一覧を取得
    pub fn list_active_plans(&self) -> Vec<PlanExecution> {
        let lifecycle_manager = self.lifecycle_manager.lock().unwrap();
        lifecycle_manager.list_active_plans()
    }

    /// Get recent plan history
    pub fn get_recent_plans(&self) -> Vec<PlanExecution> {
        let lifecycle_manager = self.lifecycle_manager.lock().unwrap();
        lifecycle_manager.get_recent_plans()
    }

    /// Get currently executing plan ID
    pub fn get_current_execution(&self) -> Option<String> {
        let current = self.current_execution.lock().unwrap();
        current.clone()
    }

    /// Start plan execution
    pub fn start_execution(&self, plan_id: &str) -> Result<()> {
        {
            let mut lifecycle_manager = self.lifecycle_manager.lock().unwrap();
            lifecycle_manager.start_execution(plan_id)?;
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
            let mut lifecycle_manager = self.lifecycle_manager.lock().unwrap();
            lifecycle_manager.pause_execution(plan_id, reason)?;
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
        let result_clone = result.clone();
        {
            let mut lifecycle_manager = self.lifecycle_manager.lock().unwrap();
            lifecycle_manager.complete_execution(plan_id, result_clone)?;
        }

        // 現在の実行をクリア
        {
            let mut current = self.current_execution.lock().unwrap();
            if current.as_ref() == Some(&plan_id.to_string()) {
                *current = None;
            }
        }

        // ディスクに保存
        {
            let lifecycle_manager = self.lifecycle_manager.lock().unwrap();
            if let Some(execution) = lifecycle_manager.get_plan(plan_id) {
                self.storage_manager.save_plan_to_disk(&execution)?;
            } else {
                // If plan is no longer in active plans, it might be in recent plans
                let recent_plans = lifecycle_manager.get_recent_plans();
                if let Some(execution) = recent_plans.iter().find(|e| e.plan.id == plan_id) {
                    self.storage_manager.save_plan_to_disk(execution)?;
                }
            }
        }

        info!(
            "Completed execution of plan: {} (success: {})",
            plan_id, result.success
        );
        Ok(())
    }

    /// Cancel plan execution
    pub fn cancel_execution(&self, plan_id: &str) -> Result<()> {
        {
            let mut lifecycle_manager = self.lifecycle_manager.lock().unwrap();
            lifecycle_manager.cancel_execution(plan_id)?;
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
        {
            let mut lifecycle_manager = self.lifecycle_manager.lock().unwrap();
            lifecycle_manager.record_step_completion(plan_id, step_result)?;
        }

        // ディスクに保存
        {
            let lifecycle_manager = self.lifecycle_manager.lock().unwrap();
            if let Some(execution) = lifecycle_manager.get_plan(plan_id) {
                self.storage_manager.save_plan_to_disk(&execution)?;
            }
        }
        Ok(())
    }

    /// 最新の計画を取得（最後に作成された計画）
    pub fn get_latest_plan(&self) -> Option<PlanExecution> {
        let lifecycle_manager = self.lifecycle_manager.lock().unwrap();
        lifecycle_manager.get_latest_plan()
    }

    /// Search for executable plans (keyword matching)
    pub fn find_executable_plan(&self, user_input: &str) -> Option<PlanExecution> {
        let lifecycle_manager = self.lifecycle_manager.lock().unwrap();
        lifecycle_manager.find_executable_plan(user_input)
    }

    /// Load plan from disk
    pub fn load_plan_from_disk(&self, plan_id: &str) -> Result<PlanExecution> {
        self.storage_manager.load_plan_from_disk(plan_id)
    }

    /// Clean up old plan files
    pub fn cleanup_old_plans(&self, days: u64) -> Result<usize> {
        self.storage_manager.cleanup_old_plans(days)
    }

    /// Get statistics
    pub fn get_statistics(&self) -> PlanStatistics {
        let lifecycle_manager = self.lifecycle_manager.lock().unwrap();
        lifecycle_manager.get_statistics()
    }
}

impl Default for PlanManager {
    fn default() -> Self {
        let project_root = std::env::current_dir().expect("Failed to get current directory");
        Self::new(project_root).expect("Failed to create PlanManager")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::plan_status::PlanStatus;
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
        let project_root = temp_dir.path().to_path_buf();
        let plans_dir = project_root.join(".doge").join("plans");
        std::fs::create_dir_all(&plans_dir)?;

        let lifecycle_manager = PlanLifecycleManager::new(50);
        let storage_manager = PlanStorage::new(plans_dir);

        let manager = PlanManager {
            lifecycle_manager: Arc::new(Mutex::new(lifecycle_manager)),
            current_execution: Arc::new(Mutex::new(None)),
            storage_manager: Arc::new(storage_manager),
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

    #[test]
    fn test_get_plans_directory() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let project_root = temp_dir.path().to_path_buf();
        let plans_dir = PlanManager::get_plans_directory(project_root.clone()).unwrap();

        assert_eq!(plans_dir, project_root.join(".doge").join("plans"));
        assert!(plans_dir.starts_with(&project_root));
        assert_eq!(plans_dir.file_name().unwrap(), "plans");
        assert_eq!(plans_dir.parent().unwrap().file_name().unwrap(), ".doge");
    }

    #[test]
    fn test_plan_manager_new_creates_plans_directory() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let project_root = temp_dir.path().to_path_buf();

        // PlanManager::new を呼び出すと、.doge/plans ディレクトリが作成されるはず
        let manager = PlanManager::new(project_root.clone());
        assert!(manager.is_ok());

        let plans_dir = project_root.join(".doge").join("plans");
        assert!(plans_dir.exists());
        assert!(plans_dir.is_dir());
    }
}
