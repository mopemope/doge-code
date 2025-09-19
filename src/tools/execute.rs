use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::process::Command;

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
            description: "Executes an arbitrary bash command within the project root directory. It captures and returns both standard output (stdout) and standard error (stderr), as well as the exit code. Use this for tasks that require shell interaction, such as running build commands (`cargo build`), tests (`cargo test`), or external utilities (`git status`). Be cautious with commands that modify the file system (e.g., `rm`, `mv`) and consider their impact beforehand. Interactive commands are not supported.".to_string(),
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

pub async fn execute_bash(command: &str) -> Result<String> {
    let output = Command::new("bash")
        .arg("-c")
        .arg(command)
        .output()
        .await
        .with_context(|| format!("Failed to execute command: {command}"))?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_execute_bash_success() {
        let result_str = execute_bash("echo 'hello'").await.unwrap();
        let result: ExecuteBashResult = serde_json::from_str(&result_str).unwrap();
        assert_eq!(result.stdout.trim(), "hello");
        assert_eq!(result.stderr, "");
        assert_eq!(result.exit_code, Some(0));
        assert!(result.success);
    }

    #[tokio::test]
    async fn test_execute_bash_failure() {
        let result = execute_bash("invalid_command").await;
        assert!(result.is_ok()); // The function should return Ok with a JSON string even for command failures
        let result: ExecuteBashResult = serde_json::from_str(&result.unwrap()).unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn test_execute_bash_with_stderr() {
        let result_str = execute_bash("echo 'test error' >&2; exit 1").await.unwrap();
        let result: ExecuteBashResult = serde_json::from_str(&result_str).unwrap();
        assert_eq!(result.stdout, "");
        assert!(result.stderr.contains("test error"));
        assert_eq!(result.exit_code, Some(1));
        assert!(!result.success);
    }

    #[tokio::test]
    async fn test_execute_bash_with_exit_code_zero() {
        let result_str = execute_bash("exit 0").await.unwrap();
        let result: ExecuteBashResult = serde_json::from_str(&result_str).unwrap();
        assert_eq!(result.stdout, "");
        assert_eq!(result.stderr, "");
        assert_eq!(result.exit_code, Some(0));
        assert!(result.success);
    }

    #[tokio::test]
    async fn test_execute_bash_with_non_zero_exit_code() {
        let result_str = execute_bash("exit 42").await.unwrap();
        let result: ExecuteBashResult = serde_json::from_str(&result_str).unwrap();
        assert_eq!(result.stdout, "");
        assert_eq!(result.stderr, "");
        assert_eq!(result.exit_code, Some(42));
        assert!(!result.success);
    }
}
