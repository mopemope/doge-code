use crate::config::AppConfig;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

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
        }
    }
}

/// Apply the combined output budget to stdout/stderr, preserving the head and
/// tail of each stream. Returns the budgeted streams plus a truncation flag.
pub fn budget_command_output(stdout: &str, stderr: &str) -> (String, String, bool, Vec<String>) {
    use crate::tools::budget::{DEFAULT_TOOL_BUDGET_CHARS, head_tail_truncate};

    let budget = DEFAULT_TOOL_BUDGET_CHARS;
    let total = stdout.chars().count() + stderr.chars().count();
    if total <= budget {
        return (stdout.to_string(), stderr.to_string(), false, Vec::new());
    }

    let (stdout_budget, stderr_budget) = if stderr.is_empty() {
        (budget, 0)
    } else {
        let out = budget * 7 / 10;
        (out, budget - out)
    };

    let out = head_tail_truncate(stdout, stdout_budget.max(200));
    let err = head_tail_truncate(stderr, stderr_budget.max(200));
    let mut warnings = vec![format!(
        "command output trimmed to ~{} chars (head and tail preserved); refine the command (e.g. pipe to `head`, `tail`, or `grep`) to see more",
        budget
    )];
    if out.truncated {
        warnings.push("stdout was truncated".to_string());
    }
    if err.truncated {
        warnings.push("stderr was truncated".to_string());
    }
    (out.text, err.text, true, warnings)
}

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "execute_bash".to_string(),
            description: "Executes a STATELESS bash command in project root. No persistent env/cwd. Use for build, test, ls. For stateful sequences, use `execute_shell`.".to_string(),
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

pub async fn execute_bash(command: &str, config: &AppConfig) -> Result<ExecuteBashResult> {
    // Change to the project root directory before executing the command
    let project_root = &config.project_root;
    let timeout_ms = config.command_timeout_ms;

    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(command)
        .current_dir(project_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = match run_command_with_timeout(cmd, timeout_ms).await {
        Ok(Some(output)) => output,
        Ok(None) => {
            return Ok(ExecuteBashResult::simple(
                String::new(),
                format!("Command timed out after {} ms", timeout_ms),
                None,
                false,
            ));
        }
        Err(e) => {
            return Err(anyhow::anyhow!(
                "Failed to execute command: {command} in directory: {} ({})",
                project_root.display(),
                e
            ));
        }
    };

    let stdout_raw = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr_raw = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code();
    let success = output.status.success();
    let (stdout, stderr, output_truncated, warnings) =
        budget_command_output(&stdout_raw, &stderr_raw);

    Ok(ExecuteBashResult {
        stdout,
        stderr,
        exit_code,
        success,
        output_truncated,
        warnings,
    })
}

async fn run_command_with_timeout(
    mut cmd: Command,
    timeout_ms: u64,
) -> Result<Option<std::process::Output>> {
    cmd.kill_on_drop(true);
    let child = cmd.spawn().with_context(|| "Failed to spawn command")?;

    if timeout_ms == 0 {
        let output = child.wait_with_output().await?;
        return Ok(Some(output));
    }

    match timeout(Duration::from_millis(timeout_ms), child.wait_with_output()).await {
        Ok(output) => Ok(Some(output?)),
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
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
        // Generate output well over the 6,000-char budget.
        let result = execute_bash("seq 1 20000", &config).await.unwrap();
        assert!(result.output_truncated);
        assert!(!result.warnings.is_empty());
        let combined = result.stdout.chars().count() + result.stderr.chars().count();
        assert!(combined <= 7_000, "combined output too large: {combined}");
        // Head and tail preserved.
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
