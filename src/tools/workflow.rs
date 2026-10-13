use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
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

enum StepCommand<'a> {
    Shell(&'a str),
    Process(&'a str),
}

impl WorkflowStep {
    /// Shared by whole-file preflight and execution; never interprets argv or policy.
    fn command(&self) -> Result<StepCommand<'_>> {
        let name = crate::tools::budget::safe_take_chars(&self.name, 128);
        match (&self.run, &self.program) {
            (Some(_), Some(_)) => anyhow::bail!(
                "Workflow step '{}' specifies both `run` and `program`; use exactly one",
                name
            ),
            (None, None) => anyhow::bail!(
                "Workflow step '{}' specifies neither `run` nor `program`",
                name
            ),
            (Some(run), None) => Ok(StepCommand::Shell(run)),
            (None, Some(program)) => Ok(StepCommand::Process(program)),
        }
    }
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
        serde_saphyr::from_str(&content).context("Failed to parse workflow YAML")?;

    // A malformed later step must not be discovered after earlier side effects.
    // Runtime policy, cwd/env validation and process failures still belong to
    // each execution; this preflight checks only the static command shape.
    for (index, step) in workflow.steps.iter().enumerate() {
        if cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
            return Err(anyhow::anyhow!(crate::llm::LlmErrorKind::Cancelled));
        }
        step.command().map_err(|error| {
            let message = format!(
                "Workflow configuration rejected at step {} before any command ran: {error}",
                index + 1
            );
            error.context(message)
        })?;
    }

    let mut output = format!("Running workflow: {}\n", workflow.name);
    for (index, step) in workflow.steps.into_iter().enumerate() {
        if cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
            return Err(anyhow::anyhow!(crate::llm::LlmErrorKind::Cancelled));
        }
        let (success, stdout, stderr, status, exit_code, truncated, warnings, error) = match step
            .command()?
        {
            StepCommand::Shell(run) => {
                info!("Running workflow step: {} ({})", step.name, run);
                let result = fs_tools
                    .execute_bash_with_cancel(run, cancel.clone())
                    .await?;
                let r: crate::tools::execute::ExecuteBashResult = serde_json::from_str(&result)?;
                // The legacy adapter has no policy/spawn status. Do not infer one.
                let status = if r.timed_out { "timed_out" } else { "failed" };
                (
                    r.success,
                    r.stdout,
                    r.stderr,
                    status.to_string(),
                    r.exit_code,
                    r.output_truncated,
                    r.warnings,
                    None,
                )
            }
            StepCommand::Process(program) => {
                info!(
                    "Running workflow step: {} ({} {:?})",
                    step.name, program, step.args
                );
                let params = crate::execution::ExecuteProcessParams {
                    program: program.to_owned(),
                    args: step.args.clone(),
                    cwd: step.cwd.clone(),
                    env: step.env.clone(),
                    timeout_ms: step.timeout_ms,
                };
                let result = fs_tools.execute_process(params, cancel.clone()).await?;
                let r: crate::execution::ProcessResult = serde_json::from_str(&result)?;
                let status = match r.status {
                    crate::execution::ProcessStatus::Completed => "completed",
                    crate::execution::ProcessStatus::TimedOut => "timed_out",
                    crate::execution::ProcessStatus::PolicyDenied => "policy_denied",
                    crate::execution::ProcessStatus::SpawnFailed => "spawn_failed",
                };
                (
                    r.success,
                    r.stdout,
                    r.stderr,
                    status.to_string(),
                    r.exit_code,
                    r.output_truncated,
                    r.warnings,
                    r.error,
                )
            }
        };
        if !success {
            return Err(WorkflowFailure {
                workflow: workflow.name,
                step: step.name,
                step_index: index + 1,
                completed_steps: index,
                status,
                exit_code,
                stdout,
                stderr,
                error,
                output_truncated: truncated,
                warnings,
            }
            .bounded()?
            .into());
        }
        output.push_str(&format!(
            "Step: {}\n  Status: SUCCESS\n  Output: {}\n",
            step.name,
            stdout.trim()
        ));
        if !stderr.is_empty() {
            output.push_str(&format!("  Stderr: {stderr}\n"));
        }
        if truncated || !warnings.is_empty() {
            output.push_str("  Warning: command diagnostics may be incomplete.\n");
        }
        // Bound accumulation across any number of successful steps.
        output = budget_success_output(&output)?;
    }
    output.push_str("Workflow completed successfully.\n");
    budget_success_output(&output)
}

/// Full model-visible workflow envelope stays below the default 8,000-char fallback.
const WORKFLOW_OUTPUT_MAX_CHARS: usize = 6_000;

/// A terminal unsuccessful step, not an invitation to replay earlier effects.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowFailure {
    pub workflow: String,
    pub step: String,
    pub step_index: usize,
    pub completed_steps: usize,
    pub status: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub error: Option<String>,
    pub output_truncated: bool,
    pub warnings: Vec<String>,
}
impl std::fmt::Display for WorkflowFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Workflow step '{}' failed: {}",
            self.step,
            self.to_json()
        )
    }
}
impl std::error::Error for WorkflowFailure {}
impl WorkflowFailure {
    pub fn to_json(&self) -> serde_json::Value {
        json!({"ok": false, "result": self})
    }
    fn bounded(self) -> Result<Self> {
        let mut limit = WORKFLOW_OUTPUT_MAX_CHARS;
        loop {
            let mut result = self.clone();
            let mut shortened = self.warnings.len() > 4;
            for (text, cap) in [
                (&mut result.workflow, 128),
                (&mut result.step, 128),
                (&mut result.status, 32),
                (&mut result.stdout, limit),
                (&mut result.stderr, limit),
            ] {
                let bounded = crate::tools::budget::head_tail_truncate(text, limit.min(cap));
                shortened |= bounded.truncated;
                *text = bounded.text;
            }
            if let Some(error) = &mut result.error {
                let bounded = crate::tools::budget::head_tail_truncate(error, limit.min(512));
                shortened |= bounded.truncated;
                *error = bounded.text;
            }
            result.warnings.truncate(4);
            for warning in &mut result.warnings {
                let bounded = crate::tools::budget::head_tail_truncate(warning, limit.min(128));
                shortened |= bounded.truncated;
                *warning = bounded.text;
            }
            if shortened {
                result.output_truncated = true;
                result.warnings.push("Workflow diagnostics shortened; earlier completed steps were not replayed. Inspect the workspace before retrying.".into());
            }
            result.warnings.push("Workflow stopped at this step. Earlier completed steps may have changed files; inspect the workspace before retrying.".into());
            if serde_json::to_string(&result.to_json())?.chars().count()
                <= WORKFLOW_OUTPUT_MAX_CHARS
            {
                return Ok(result);
            }
            limit = (limit / 2).max(1);
        }
    }
}

fn budget_success_output(text: &str) -> Result<String> {
    let mut limit = WORKFLOW_OUTPUT_MAX_CHARS;
    loop {
        let bounded = crate::tools::budget::head_tail_truncate(text, limit);
        if serde_json::to_string(&json!({"result": bounded.text}))?
            .chars()
            .count()
            <= WORKFLOW_OUTPUT_MAX_CHARS
        {
            return Ok(bounded.text);
        }
        limit = (limit / 2).max(1);
    }
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
    async fn failed_step_preserves_diagnostics_and_never_runs_following_step() -> Result<()> {
        for structured in [false, true] {
            let root = tempdir()?;
            let dir = root.path().join(".doge/workflows");
            std::fs::create_dir_all(&dir)?;
            let command =
                "printf 'FAILED assertion: 日本語\\n'; printf 'compiler detail\\n' >&2; exit 7";
            let step = if structured {
                format!(
                    "program: bash\n    args: ['-c', {}]",
                    serde_json::to_string(command)?
                )
            } else {
                format!("run: {}", serde_json::to_string(command)?)
            };
            std::fs::write(
                dir.join("failure.yml"),
                format!(
                    "name: Failure\nsteps:\n  - name: Prepare\n    program: echo\n    args: ['prepared']\n  - name: Check\n    {step}\n  - name: Never\n    program: touch\n    args: ['must-not-run']\n"
                ),
            )?;
            let cfg = AppConfig {
                project_root: root.path().into(),
                ..Default::default()
            };
            let tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));
            let error = run_workflow("failure", root.path(), &tools)
                .await
                .expect_err("failure");
            assert!(
                error.to_string().contains("FAILED assertion: 日本語"),
                "{error}"
            );
            assert!(error.to_string().contains("compiler detail"), "{error}");
            assert!(!root.path().join("must-not-run").exists());
            let report = error
                .downcast_ref::<WorkflowFailure>()
                .expect("typed failure");
            assert_eq!(report.step_index, 2);
            assert_eq!(report.completed_steps, 1);
            assert_eq!(report.exit_code, Some(7));
            assert_eq!(report.to_json()["ok"], false);
        }
        Ok(())
    }

    #[test]
    fn escaped_failure_budget_preserves_terminal_metadata_and_both_stream_ends() {
        let content = format!("HEAD日本語{}TAILassertion", "\n\"\\\u{0001}".repeat(20_000));
        let report = WorkflowFailure {
            workflow: "\n\"\\".repeat(10_000),
            step: "\n\"\\".repeat(10_000),
            step_index: 101,
            completed_steps: 100,
            status: "timed_out".into(),
            exit_code: None,
            stdout: content.clone(),
            stderr: content,
            error: Some("\n\"\\".repeat(10_000)),
            output_truncated: false,
            warnings: vec!["\n\"\\".repeat(10_000); 100],
        }
        .bounded()
        .expect("bounded");
        let json = serde_json::to_string(&report.to_json()).unwrap();
        assert!(json.chars().count() <= WORKFLOW_OUTPUT_MAX_CHARS);
        assert_eq!(
            crate::llm::truncate_tool_output(json.clone(), "run_workflow"),
            json
        );
        assert_eq!(report.status, "timed_out");
        assert_eq!(report.step_index, 101);
        assert_eq!(report.completed_steps, 100);
        assert!(report.output_truncated);
        for stream in [&report.stdout, &report.stderr] {
            assert!(stream.starts_with("HEAD日本語") && stream.ends_with("TAILassertion"));
        }
        assert!(report.warnings.iter().any(|s| s.contains("shortened")));
    }

    #[test]
    fn success_accumulation_budget_accounts_for_json_escaping() {
        let original = format!(
            "Running workflow: long\n{}Workflow completed successfully.\n",
            "\n\"\\\u{0001}日本語".repeat(10_000)
        );
        let result = budget_success_output(&original).unwrap();
        assert!(result.starts_with("Running workflow:"));
        assert!(result.contains("output truncated"));
        assert!(result.ends_with("Workflow completed successfully.\n"));
        assert!(
            serde_json::to_string(&json!({"result": result}))
                .unwrap()
                .chars()
                .count()
                <= WORKFLOW_OUTPUT_MAX_CHARS
        );
    }

    #[tokio::test]
    async fn structured_terminal_status_is_not_lost_or_followed_by_more_steps() -> Result<()> {
        use crate::config::{ExecutionConfig, ExecutionMode};
        for (program, args, expected, denied) in [
            ("sleep", vec!["30"], "timed_out", false),
            (
                "dgc-intentionally-missing-program",
                vec![],
                "spawn_failed",
                false,
            ),
            ("touch", vec!["denied-marker"], "policy_denied", true),
        ] {
            let root = tempdir()?;
            let dir = root.path().join(".doge/workflows");
            std::fs::create_dir_all(&dir)?;
            std::fs::write(
                dir.join("status.yml"),
                format!(
                    "name: Status\nsteps:\n- name: Check\n  program: {program}\n  args: {}\n  timeout_ms: 30\n- name: Never\n  program: touch\n  args: ['following-marker']\n",
                    serde_json::to_string(&args)?
                ),
            )?;
            let cfg = AppConfig {
                project_root: root.path().into(),
                execution_configured: denied,
                execution: if denied {
                    ExecutionConfig {
                        mode: ExecutionMode::Allowlist,
                        allowed_programs: vec!["echo".into()],
                        ..Default::default()
                    }
                } else {
                    ExecutionConfig::default()
                },
                ..Default::default()
            };
            let tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));
            let error = run_workflow("status", root.path(), &tools)
                .await
                .expect_err("failure");
            assert_eq!(
                error.downcast_ref::<WorkflowFailure>().unwrap().status,
                expected
            );
            assert!(!root.path().join("following-marker").exists());
            assert!(!root.path().join("denied-marker").exists());
        }
        Ok(())
    }

    #[test]
    fn legacy_yaml_workflow_preserves_quoted_args_and_environment() {
        let workflow: Workflow = serde_saphyr::from_str("name: legacy\nsteps:\n- name: inspect\n  program: printf\n  args: ['on', '001', 'null', '日本語']\n  env: {MODE: 'on'}\n  timeout_ms: 1500\n").expect("legacy workflow");
        assert_eq!(workflow.name, "legacy");
        assert_eq!(workflow.steps[0].args, ["on", "001", "null", "日本語"]);
        assert_eq!(workflow.steps[0].env["MODE"], "on");
        assert_eq!(workflow.steps[0].timeout_ms, Some(1500));
    }

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

#[cfg(test)]
mod preflight_tests;
