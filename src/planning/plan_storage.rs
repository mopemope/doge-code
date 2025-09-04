use crate::planning::plan_execution::PlanExecution;
use anyhow::Result;
use std::path::PathBuf;
use tracing::info;

#[derive(Debug)]
pub struct PlanStorage {
    /// Directory to save plans
    plans_dir: PathBuf,
}

impl PlanStorage {
    pub fn new(plans_dir: PathBuf) -> Self {
        Self { plans_dir }
    }

    /// 計画をディスクに保存
    pub fn save_plan_to_disk(&self, execution: &PlanExecution) -> Result<()> {
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
}
