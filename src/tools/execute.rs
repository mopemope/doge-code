use crate::config::AppConfig;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

#[derive(Debug, Serialize, Deserialize)]
pub struct ExecuteBashResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub success: bool,
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

pub async fn execute_bash(command: &str, config: &AppConfig) -> Result<String> {
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
            let result = ExecuteBashResult {
                stdout: String::new(),
                stderr: format!("Command timed out after {} ms", timeout_ms),
                exit_code: None,
                success: false,
            };
            return Ok(serde_json::to_string(&result)?);
        }
        Err(e) => {
            return Err(anyhow::anyhow!(
                "Failed to execute command: {command} in directory: {} ({})",
                project_root.display(),
                e
            ));
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code();
    let success = output.status.success();

    let result = ExecuteBashResult {
        stdout,
        stderr,
        exit_code,
        success,
    };

    Ok(serde_json::to_string(&result)?)
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
        let result_str = execute_bash("echo 'hello'", &config).await.unwrap();
        let result: ExecuteBashResult = serde_json::from_str(&result_str).unwrap();
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
        let result = execute_bash("invalid_command", &config).await;
        assert!(result.is_ok()); // The function should return Ok with a JSON string even for command failures
        let result: ExecuteBashResult = serde_json::from_str(&result.unwrap()).unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn test_execute_bash_with_stderr() {
        let temp_dir = TempDir::new().unwrap();
        let config = AppConfig {
            project_root: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let result_str = execute_bash("echo 'test error' >&2; exit 1", &config)
            .await
            .unwrap();
        let result: ExecuteBashResult = serde_json::from_str(&result_str).unwrap();
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
        let result_str = execute_bash("exit 0", &config).await.unwrap();
        let result: ExecuteBashResult = serde_json::from_str(&result_str).unwrap();
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
        let result_str = execute_bash("exit 42", &config).await.unwrap();
        let result: ExecuteBashResult = serde_json::from_str(&result_str).unwrap();
        assert_eq!(result.stdout, "");
        assert_eq!(result.stderr, "");
        assert_eq!(result.exit_code, Some(42));
        assert!(!result.success);
    }
}
