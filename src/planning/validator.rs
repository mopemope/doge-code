use crate::planning::task_types::TaskStep;
use crate::tools::FsTools;
use anyhow::{Result, anyhow};
use tracing::debug;

/// Check step validation criteria
pub async fn validate_step_criteria(
    step: &TaskStep,
    result: &str,
    fs_tools: &FsTools,
) -> Result<()> {
    for criteria in &step.validation_criteria {
        match criteria.as_str() {
            "Compilation Success" | "No Syntax Errors" => {
                // Run cargo check
                if let Err(e) = fs_tools.execute_bash("cargo check").await {
                    return Err(anyhow!("Compilation failed: {}", e));
                }
            }
            "Tests Pass" => {
                // Run cargo test
                if let Err(e) = fs_tools.execute_bash("cargo test").await {
                    return Err(anyhow!("Tests failed: {}", e));
                }
            }
            "File Exists" => {
                // Extract file paths from results and check existence
                // Simple implementation: Improve later
                debug!("File existence check: {}", result);
            }
            _ => {
                // Check if other criteria are contained in the result text
                if !result.to_lowercase().contains(&criteria.to_lowercase()) {
                    tracing::warn!("Validation criteria '{}' not met in result", criteria);
                }
            }
        }
    }
    Ok(())
}
