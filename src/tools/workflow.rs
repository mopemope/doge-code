use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::json;
use std::path::Path;
use tracing::info;

#[derive(Debug, Deserialize)]
pub struct Workflow {
    pub name: String,
    pub steps: Vec<WorkflowStep>,
}

#[derive(Debug, Deserialize)]
pub struct WorkflowStep {
    pub name: String,
    pub run: String,
}

pub async fn run_workflow(
    workflow_name: &str,
    project_root: &Path,
    fs_tools: &crate::tools::FsTools,
) -> Result<String> {
    if !workflow_name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        anyhow::bail!(
            "Invalid workflow name '{}'. Only alphanumeric characters, underscores, and dashes are allowed.",
            workflow_name
        );
    }

    let mut workflow_path = project_root
        .join(".doge/workflows")
        .join(format!("{}.yml", workflow_name));

    if !workflow_path.exists() {
        let workflow_path_yaml = project_root
            .join(".doge/workflows")
            .join(format!("{}.yaml", workflow_name));
        if !workflow_path_yaml.exists() {
            anyhow::bail!(
                "Workflow file not found: {} (checked .yml and .yaml)",
                workflow_name
            );
        }
        workflow_path = workflow_path_yaml;
    }

    // We need to read the file content.
    // Since we are inside a tool implementation, we can use std::fs or tokio::fs directly if we are careful,
    // or use fs_tools. But fs_tools might check permissions for arbitrary paths?
    // Reading .doge/workflows is usually safe.

    let content = tokio::fs::read_to_string(&workflow_path)
        .await
        .context(format!("Failed to read workflow file: {:?}", workflow_path))?;

    let workflow: Workflow =
        serde_yaml::from_str(&content).context("Failed to parse workflow YAML")?;

    let mut output = String::new();
    output.push_str(&format!("Running workflow: {}\n", workflow.name));

    for step in workflow.steps {
        output.push_str(&format!("Step: {}\n", step.name));
        info!("Running workflow step: {} ({})", step.name, step.run);

        let result = fs_tools.execute_bash(&step.run).await?;

        // execute_bash returns a JSON string Result
        // We need to parse it to check success.
        let exec_result: crate::tools::execute::ExecuteBashResult = serde_json::from_str(&result)?;

        if !exec_result.success {
            output.push_str(&format!(
                "  Status: FAILED\n  Error: {}\n",
                exec_result.stderr
            ));
            anyhow::bail!(
                "Workflow step '{}' failed: {}",
                step.name,
                exec_result.stderr
            );
        } else {
            output.push_str(&format!(
                "  Status: SUCCESS\n  Output: {}\n",
                exec_result.stdout.trim()
            ));
        }
    }

    output.push_str("Workflow completed successfully.\n");
    Ok(output)
}

pub fn run_workflow_tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "run_workflow".to_string(),
            description: "Run a predefined workflow from .doge/workflows/".to_string(),
            strict: Some(true),
            parameters: json!( {
                "type": "object",
                "properties": {
                    "workflow_name": {
                        "type": "string",
                        "description": "Name of the workflow file (without extension)",
                    }
                },
                "additionalProperties": false,
                "required": ["workflow_name"],
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::tools::FsTools;
    use std::sync::Arc;
    use tempfile::tempdir;
    use tokio::sync::RwLock;

    #[tokio::test]
    async fn test_run_workflow_success() -> Result<()> {
        let temp_dir = tempdir()?;
        let project_root = temp_dir.path().to_path_buf();
        let workflows_dir = project_root.join(".doge/workflows");
        tokio::fs::create_dir_all(&workflows_dir).await?;

        let workflow_content = r#"
name: Test Workflow
steps:
  - name: Step 1
    run: echo "Hello"
  - name: Step 2
    run: echo "World"
"#;
        tokio::fs::write(workflows_dir.join("test.yml"), workflow_content).await?;

        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec![], // Allow all
            ..Default::default()
        };
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        let result = run_workflow("test", &project_root, &fs_tools).await?;
        assert!(result.contains("Running workflow: Test Workflow"));
        assert!(result.contains("Step: Step 1"));
        assert!(result.contains("Step: Step 2"));
        assert!(result.contains("Status: SUCCESS"));

        Ok(())
    }

    #[tokio::test]
    async fn test_run_workflow_missing_file() -> Result<()> {
        let temp_dir = tempdir()?;
        let project_root = temp_dir.path().to_path_buf();
        // No workflows dir

        let cfg = AppConfig {
            project_root: project_root.clone(),
            ..Default::default()
        };
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        let result = run_workflow("missing", &project_root, &fs_tools).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));

        Ok(())
    }

    #[tokio::test]
    async fn test_run_workflow_failure() -> Result<()> {
        let temp_dir = tempdir()?;
        let project_root = temp_dir.path().to_path_buf();
        let workflows_dir = project_root.join(".doge/workflows");
        tokio::fs::create_dir_all(&workflows_dir).await?;

        let workflow_content = r#"
name: Fail Workflow
steps:
  - name: Fail Step
    run: exit 1
"#;
        tokio::fs::write(workflows_dir.join("fail.yml"), workflow_content).await?;

        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec![],
            ..Default::default()
        };
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        let result = run_workflow("fail", &project_root, &fs_tools).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Workflow step 'Fail Step' failed")
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_run_workflow_invalid_name() -> Result<()> {
        let temp_dir = tempdir()?;
        let project_root = temp_dir.path().to_path_buf();
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec![],
            ..Default::default()
        };
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        let result = run_workflow("../evil", &project_root, &fs_tools).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid workflow name")
        );

        Ok(())
    }
}
