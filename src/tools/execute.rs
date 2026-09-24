use crate::config::AppConfig;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Character budget for combined command output (stdout + stderr). Keeps the
/// serialized JSON payload safely under the 8,000-char global truncation cap.
pub const BASH_OUTPUT_BUDGET_CHARS: usize = 6_000;

#[derive(Debug, Serialize, Deserialize)]
pub struct ExecuteBashResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub success: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub output_truncated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timed_out: bool,
}

impl ExecuteBashResult {
    pub fn simple(stdout: String, stderr: String, exit_code: Option<i32>, success: bool) -> Self {
        Self {
            stdout,
            stderr,
            exit_code,
            success,
            output_truncated: false,
            warnings: Vec::new(),
            timed_out: false,
        }
    }
}

/// Apply the combined output budget to stdout/stderr, preserving the head and
/// tail of each stream. The low-level runner uses only bounded capture; this
/// LLM-facing adapter applies the ~6,000-character tool budget.
pub fn budget_command_output(stdout: &str, stderr: &str) -> (String, String, bool, Vec<String>) {
    crate::execution::output::budget_command_output(stdout, stderr)
}

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "execute_bash".to_string(),
            description: "Executes a stateless shell command (legacy/shell escape hatch). Prefer `execute_process` for normal builds, tests, git, and other single-program commands. Use this only when shell syntax such as pipes, redirects, or shell builtins is genuinely required. For stateful sequences, use `execute_shell`.".to_string(),
            strict: None,
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
        },
    }
}

/// Run a trusted-after-policy shell command through the common finite-process
/// runner. Policy is applied by `FsTools` before this function is called.
pub async fn execute_bash(command: &str, config: &AppConfig) -> Result<ExecuteBashResult> {
    execute_bash_with_cancel(command, config, None).await
}

/// Cancellation-aware variant used by the agent loop and workflow tool.
pub async fn execute_bash_with_cancel(
    command: &str,
    config: &AppConfig,
    cancel: Option<CancellationToken>,
) -> Result<ExecuteBashResult> {
    let spec = crate::execution::ManagedProcessSpec {
        program: "bash".to_string(),
        args: vec!["-c".to_string(), command.to_string()],
        cwd: config.project_root.clone(),
        env: Default::default(),
    };
    let timeout =
        (config.command_timeout_ms != 0).then(|| Duration::from_millis(config.command_timeout_ms));
    let options = crate::execution::ManagedRunOptions {
        timeout,
        cancellation: cancel,
    };

    let managed = crate::execution::run_managed_process(spec, options).await?;
    if managed.termination == crate::execution::ManagedProcessTermination::Cancelled {
        return Err(anyhow::anyhow!(crate::llm::LlmErrorKind::Cancelled));
    }

    let (stdout, stderr, budgeted, mut warnings) =
        budget_command_output(&managed.stdout, &managed.stderr);
    if managed.capture_truncated {
        warnings
            .push("command output exceeded capture limits; head and tail preserved".to_string());
    }
    let timed_out = managed.termination == crate::execution::ManagedProcessTermination::TimedOut;
    Ok(ExecuteBashResult {
        stdout,
        stderr,
        exit_code: managed.exit_code,
        success: managed.success(),
        output_truncated: budgeted || managed.capture_truncated || timed_out,
        warnings,
        timed_out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_execute_bash_success() {
        let temp_dir = TempDir::new().unwrap();
        let config = AppConfig {
            project_root: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let result = execute_bash("echo 'hello'", &config).await.unwrap();
        assert_eq!(result.stdout.trim(), "hello");
        assert_eq!(result.stderr, "");
        assert_eq!(result.exit_code, Some(0));
        assert!(result.success);
    }

    #[tokio::test]
    async fn test_execute_bash_failure() {
        let temp_dir = TempDir::new().unwrap();
        let config = AppConfig {
            project_root: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let result = execute_bash("invalid_command", &config).await.unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn test_execute_bash_with_stderr() {
        let temp_dir = TempDir::new().unwrap();
        let config = AppConfig {
            project_root: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let result = execute_bash("echo 'test error' >&2; exit 1", &config)
            .await
            .unwrap();
        assert_eq!(result.stdout, "");
        assert!(result.stderr.contains("test error"));
        assert_eq!(result.exit_code, Some(1));
        assert!(!result.success);
    }

    #[tokio::test]
    async fn test_execute_bash_with_exit_code_zero() {
        let temp_dir = TempDir::new().unwrap();
        let config = AppConfig {
            project_root: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let result = execute_bash("exit 0", &config).await.unwrap();
        assert_eq!(result.stdout, "");
        assert_eq!(result.stderr, "");
        assert_eq!(result.exit_code, Some(0));
        assert!(result.success);
    }

    #[tokio::test]
    async fn test_execute_bash_with_non_zero_exit_code() {
        let temp_dir = TempDir::new().unwrap();
        let config = AppConfig {
            project_root: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let result = execute_bash("exit 42", &config).await.unwrap();
        assert_eq!(result.stdout, "");
        assert_eq!(result.stderr, "");
        assert_eq!(result.exit_code, Some(42));
        assert!(!result.success);
    }

    #[tokio::test]
    async fn test_execute_bash_output_budgeted() {
        let temp_dir = TempDir::new().unwrap();
        let config = AppConfig {
            project_root: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let result = execute_bash("seq 1 20000", &config).await.unwrap();
        assert!(result.output_truncated);
        assert!(!result.warnings.is_empty());
        let combined = result.stdout.chars().count() + result.stderr.chars().count();
        assert!(combined <= 7_000, "combined output too large: {combined}");
        assert!(result.stdout.starts_with('1'));
        assert!(result.stdout.contains("20000"));
    }

    #[tokio::test]
    async fn test_execute_bash_small_output_not_truncated() {
        let temp_dir = TempDir::new().unwrap();
        let config = AppConfig {
            project_root: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let result = execute_bash("echo hello", &config).await.unwrap();
        assert!(!result.output_truncated);
        assert!(result.warnings.is_empty());
    }

    #[tokio::test]
    async fn test_execute_bash_timeout() {
        let temp_dir = TempDir::new().unwrap();
        let config = AppConfig {
            project_root: temp_dir.path().to_path_buf(),
            command_timeout_ms: 50,
            ..Default::default()
        };
        let result = execute_bash("sleep 30", &config).await.unwrap();
        assert!(!result.success);
        assert!(result.timed_out);
        assert!(result.output_truncated);
        assert_eq!(result.exit_code, None);
    }

    #[tokio::test]
    async fn test_execute_bash_cancel() {
        let temp_dir = TempDir::new().unwrap();
        let config = AppConfig {
            project_root: temp_dir.path().to_path_buf(),
            command_timeout_ms: 30_000,
            ..Default::default()
        };
        let token = CancellationToken::new();
        let timer_token = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            timer_token.cancel();
        });
        let error = execute_bash_with_cancel("sleep 30", &config, Some(token))
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<crate::llm::LlmErrorKind>().is_some());
    }

    #[test]
    fn test_budget_command_output_head_tail() {
        let stdout = "a".repeat(10_000);
        let stderr = "b".repeat(2_000);
        let (out, err, truncated, warnings) = budget_command_output(&stdout, &stderr);
        assert!(truncated);
        assert!(!warnings.is_empty());
        assert!(out.starts_with("aaaa"));
        assert!(out.ends_with("aaaa"));
        assert!(err.starts_with("bbbb"));
        assert!(err.ends_with("bbbb"));
        let total = out.chars().count() + err.chars().count();
        assert!(total <= 7_000, "combined too large: {total}");
    }

    #[test]
    fn test_budget_command_output_under_budget_untouched() {
        let (out, err, truncated, warnings) = budget_command_output("hello", "");
        assert_eq!(out, "hello");
        assert_eq!(err, "");
        assert!(!truncated);
        assert!(warnings.is_empty());
    }
}
