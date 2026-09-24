use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::json;
use std::path::Path;
use tokio_util::sync::CancellationToken;
use tracing::info;

#[derive(Debug, Deserialize)]
pub struct Workflow {
    pub name: String,
    pub steps: Vec<WorkflowStep>,
}

#[derive(Debug, Deserialize)]
pub struct WorkflowStep {
    pub name: String,
    /// Legacy shell step (`run: ...`). Subject to `execution.allow_shell`.
    #[serde(default)]
    pub run: Option<String>,
    /// Structured step (`program: ...`). Subject to the process policy.
    #[serde(default)]
    pub program: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<std::path::PathBuf>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

pub async fn run_workflow(
    workflow_name: &str,
    project_root: &Path,
    fs_tools: &crate::tools::FsTools,
) -> Result<String> {
    run_workflow_with_cancel(workflow_name, project_root, fs_tools, None).await
}

/// Cancellation-aware workflow runner. Cancellation is passed into every
/// finite process step; a cancelled step aborts the workflow immediately.
pub async fn run_workflow_with_cancel(
    workflow_name: &str,
    project_root: &Path,
    fs_tools: &crate::tools::FsTools,
    cancel: Option<CancellationToken>,
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
        if cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
            return Err(anyhow::anyhow!(crate::llm::LlmErrorKind::Cancelled));
        }
        output.push_str(&format!("Step: {}\n", step.name));
        match (&step.run, &step.program) {
            (Some(_), Some(_)) => {
                anyhow::bail!(
                    "Workflow step '{}' specifies both `run` and `program`; use exactly one",
                    step.name
                );
            }
            (None, None) => {
                anyhow::bail!(
                    "Workflow step '{}' specifies neither `run` nor `program`",
                    step.name
                );
            }
            (Some(run), None) => {
                info!("Running workflow step: {} ({})", step.name, run);
                let result = fs_tools
                    .execute_bash_with_cancel(run, cancel.clone())
                    .await?;
                let exec_result: crate::tools::execute::ExecuteBashResult =
                    serde_json::from_str(&result)?;
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
            (None, Some(program)) => {
                info!(
                    "Running workflow step: {} ({} {:?})",
                    step.name, program, step.args
                );
                let params = crate::execution::ExecuteProcessParams {
                    program: program.clone(),
                    args: step.args.clone(),
                    cwd: step.cwd.clone(),
                    env: step.env.clone(),
                    timeout_ms: step.timeout_ms,
                };
                let result = fs_tools.execute_process(params, cancel.clone()).await?;
                let exec_result: crate::execution::ProcessResult = serde_json::from_str(&result)?;
                if !exec_result.success {
                    let err = exec_result.error.unwrap_or_default();
                    let detail = if exec_result.stderr.is_empty() {
                        err.clone()
                    } else {
                        exec_result.stderr.clone()
                    };
                    output.push_str(&format!("  Status: FAILED\n  Error: {detail}\n"));
                    anyhow::bail!("Workflow step '{}' failed: {err}", step.name);
                } else {
                    output.push_str(&format!(
                        "  Status: SUCCESS\n  Output: {}\n",
                        exec_result.stdout.trim()
                    ));
                }
            }
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

    #[tokio::test]
    async fn test_run_workflow_structured_success() -> Result<()> {
        let temp_dir = tempdir()?;
        let project_root = temp_dir.path().to_path_buf();
        let workflows_dir = project_root.join(".doge/workflows");
        tokio::fs::create_dir_all(&workflows_dir).await?;

        let workflow_content = r#"
name: Structured Workflow
steps:
  - name: Echo
    program: echo
    args: ["hello"]
"#;
        tokio::fs::write(workflows_dir.join("structured.yml"), workflow_content).await?;

        let cfg = AppConfig {
            project_root: project_root.clone(),
            ..Default::default()
        };
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        let result = run_workflow("structured", &project_root, &fs_tools).await?;
        assert!(result.contains("Status: SUCCESS"));
        assert!(result.contains("hello"));
        Ok(())
    }

    #[tokio::test]
    async fn test_run_workflow_structured_failure() -> Result<()> {
        let temp_dir = tempdir()?;
        let project_root = temp_dir.path().to_path_buf();
        let workflows_dir = project_root.join(".doge/workflows");
        tokio::fs::create_dir_all(&workflows_dir).await?;

        let workflow_content = r#"
name: Structured Fail
steps:
  - name: Fail
    program: bash
    args: ["-c", "exit 2"]
"#;
        tokio::fs::write(workflows_dir.join("sfail.yml"), workflow_content).await?;

        let cfg = AppConfig {
            project_root: project_root.clone(),
            ..Default::default()
        };
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        let result = run_workflow("sfail", &project_root, &fs_tools).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Workflow step 'Fail' failed")
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_run_workflow_mixed_step_is_error() -> Result<()> {
        let temp_dir = tempdir()?;
        let project_root = temp_dir.path().to_path_buf();
        let workflows_dir = project_root.join(".doge/workflows");
        tokio::fs::create_dir_all(&workflows_dir).await?;

        let workflow_content = r#"
name: Mixed
steps:
  - name: Bad
    run: echo hi
    program: echo
"#;
        tokio::fs::write(workflows_dir.join("mixed.yml"), workflow_content).await?;

        let cfg = AppConfig {
            project_root: project_root.clone(),
            ..Default::default()
        };
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        let result = run_workflow("mixed", &project_root, &fs_tools).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("both `run` and `program`")
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_run_workflow_shell_disabled_but_structured_allowed() -> Result<()> {
        use crate::config::{ExecutionConfig, ExecutionMode};
        let temp_dir = tempdir()?;
        let project_root = temp_dir.path().to_path_buf();
        let workflows_dir = project_root.join(".doge/workflows");
        tokio::fs::create_dir_all(&workflows_dir).await?;

        let shell_wf = r#"
name: Shell
steps:
  - name: S
    run: echo hi
"#;
        tokio::fs::write(workflows_dir.join("sh.yml"), shell_wf).await?;
        let proc_wf = r#"
name: Proc
steps:
  - name: P
    program: echo
    args: ["hi"]
"#;
        tokio::fs::write(workflows_dir.join("pr.yml"), proc_wf).await?;

        let exec = ExecutionConfig {
            mode: ExecutionMode::Allowlist,
            allowed_programs: vec!["echo".to_string()],
            allow_shell: false,
            ..ExecutionConfig::default()
        };
        let cfg = AppConfig {
            project_root: project_root.clone(),
            execution: exec,
            execution_configured: true,
            ..Default::default()
        };
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        // Legacy shell step is denied under the policy.
        let shell_out = run_workflow("sh", &project_root, &fs_tools).await;
        assert!(shell_out.is_err(), "shell step should be denied");
        // Structured step runs fine under the same policy.
        let proc_out = run_workflow("pr", &project_root, &fs_tools).await?;
        assert!(proc_out.contains("Status: SUCCESS"));
        Ok(())
    }

    #[tokio::test]
    async fn test_run_workflow_structured_cancellation_stops_following_steps() -> Result<()> {
        let temp_dir = tempdir()?;
        let project_root = temp_dir.path().to_path_buf();
        let workflows_dir = project_root.join(".doge/workflows");
        tokio::fs::create_dir_all(&workflows_dir).await?;
        let marker = project_root.join("structured-ran");
        let workflow = format!(
            "name: Cancel Structured\nsteps:\n  - name: Wait\n    program: sleep\n    args: [\"30\"]\n  - name: MustNotRun\n    program: touch\n    args: [\"{}\"]\n",
            marker.display()
        );
        tokio::fs::write(workflows_dir.join("cancel.yml"), workflow).await?;
        let cfg = AppConfig {
            project_root: project_root.clone(),
            ..Default::default()
        };
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));
        let token = CancellationToken::new();
        let timer_token = token.clone();
        let timer = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            timer_token.cancel();
        });
        let error = run_workflow_with_cancel("cancel", &project_root, &fs_tools, Some(token))
            .await
            .expect_err("cancellation should abort workflow");
        timer.await?;
        assert!(error.downcast_ref::<crate::llm::LlmErrorKind>().is_some());
        assert!(!marker.exists());
        Ok(())
    }

    #[tokio::test]
    async fn test_run_workflow_shell_cancellation_stops_following_steps() -> Result<()> {
        let temp_dir = tempdir()?;
        let project_root = temp_dir.path().to_path_buf();
        let workflows_dir = project_root.join(".doge/workflows");
        tokio::fs::create_dir_all(&workflows_dir).await?;
        let marker = project_root.join("shell-ran");
        let workflow = format!(
            "name: Cancel Shell\nsteps:\n  - name: Wait\n    run: sleep 30\n  - name: MustNotRun\n    run: touch '{}'\n",
            marker.display()
        );
        tokio::fs::write(workflows_dir.join("cancel-shell.yml"), workflow).await?;
        let cfg = AppConfig {
            project_root: project_root.clone(),
            ..Default::default()
        };
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));
        let token = CancellationToken::new();
        let timer_token = token.clone();
        let timer = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            timer_token.cancel();
        });
        let error = run_workflow_with_cancel("cancel-shell", &project_root, &fs_tools, Some(token))
            .await
            .expect_err("cancellation should abort workflow");
        timer.await?;
        assert!(error.downcast_ref::<crate::llm::LlmErrorKind>().is_some());
        assert!(!marker.exists());
        Ok(())
    }
}
