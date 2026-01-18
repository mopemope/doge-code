use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::info;

use crate::config::AppConfig;
use crate::exec::Executor;

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkflowMetadata {
    pub description: Option<String>,
}

#[derive(Debug)]
pub struct Workflow {
    pub name: String,
    pub metadata: WorkflowMetadata,
    pub steps: Vec<String>,
    pub file_path: PathBuf,
}

impl Workflow {
    pub async fn load(name: &str, project_root: &Path) -> Result<Self> {
        let workflows_dir = project_root.join(".doge").join("workflows");
        let file_path = workflows_dir.join(format!("{}.md", name));

        if !file_path.exists() {
            return Err(anyhow::anyhow!(
                "Workflow file not found: {}. Expected at: {}",
                name,
                file_path.display()
            ));
        }

        let content = fs::read_to_string(&file_path)
            .await
            .with_context(|| format!("Failed to read workflow file: {}", file_path.display()))?;

        Self::parse(name, &content, file_path)
    }

    fn parse(name: &str, content: &str, file_path: PathBuf) -> Result<Self> {
        let parts: Vec<&str> = content.splitn(3, "---").collect();

        let (metadata, body) = if parts.len() >= 3 && parts[0].trim().is_empty() {
            // Frontmatter exists
            let yaml_str = parts[1];
            let body = parts[2];
            let metadata: WorkflowMetadata = serde_yaml::from_str(yaml_str)
                .map_err(|e| anyhow::anyhow!("Failed to parse workflow frontmatter: {}", e))?;
            (metadata, body)
        } else {
            // No frontmatter
            (WorkflowMetadata { description: None }, content)
        };

        // Parse steps from markdown list items or non-empty lines
        // For simplicity, we treat every non-empty line that looks like an instruction as a step.
        // We'll filter out comments and headers.
        let steps = body
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty() && !line.starts_with('#')) // Ignore headers
            .filter_map(|line: &str| {
                // Strip list markers if present
                let clean_line = if let Some(stripped) = line.strip_prefix("- ") {
                    stripped
                } else if let Some(idx) = line.find(". ") {
                    // numeric list "1. "
                    if line[..idx].chars().all(char::is_numeric) {
                        &line[idx + 2..]
                    } else {
                        line
                    }
                } else {
                    line
                };

                if clean_line.is_empty() {
                    None
                } else {
                    Some(clean_line.to_string())
                }
            })
            .collect();

        Ok(Self {
            name: name.to_string(),
            metadata,
            steps,
            file_path,
        })
    }
}

pub struct WorkflowExecutor {
    cfg: AppConfig,
}

impl WorkflowExecutor {
    pub fn new(cfg: AppConfig) -> Self {
        Self { cfg }
    }

    pub async fn execute(&self, workflow: &Workflow) -> Result<()> {
        info!(
            "Starting workflow: {} ({})",
            workflow.name,
            workflow
                .metadata
                .description
                .as_deref()
                .unwrap_or("No description")
        );

        let mut executor = Executor::new(self.cfg.clone())?;

        for (i, step) in workflow.steps.iter().enumerate() {
            let step_num = i + 1;
            info!(
                "Running step {}/{}: {}",
                step_num,
                workflow.steps.len(),
                step
            );
            println!("🚀 Step {}/{}: {}", step_num, workflow.steps.len(), step);

            // Execute the step using the robust Executor (which has the self-healing loop in fix mode,
            // but for general exec we use run())
            // Ideally, we want the workflow step to be robust.
            // We can prefix the instruction with specific guidance.
            let instruction = format!(
                "Workflow Step {}/{}: {}\n\nExecute this step.",
                step_num,
                workflow.steps.len(),
                step
            );

            // We use the standard executor run.
            // If the user wants specific "fix" behavior, they might need to specify it,
            // BUT our recent change to agent_loop.rs makes it self-healing by default on verification failure!
            // So standard `executor.run` is already robust!
            executor.run(&instruction, false).await?;

            println!("✅ Step {} completed.", step_num);
        }

        println!("🎉 Workflow '{}' completed successfully!", workflow.name);
        Ok(())
    }
}

pub async fn run_workflow(cfg: AppConfig, workflow_name: &str) -> Result<()> {
    let workflow = Workflow::load(workflow_name, &cfg.project_root).await?;
    let executor = WorkflowExecutor::new(cfg);
    executor.execute(&workflow).await
}
