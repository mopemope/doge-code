use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use serde_json::json;
use tokio::process::Command;

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "execute_bash".to_string(),
            description: "Executes an arbitrary bash command within the project root directory. It captures and returns both standard output (stdout) and standard error (stderr). Use this for tasks that require shell interaction, such as running build commands (`cargo build`), tests (`cargo test`), or external utilities (`git status`). Be cautious with commands that modify the file system (e.g., `rm`, `mv`) and consider their impact beforehand. Interactive commands are not supported.".to_string(),
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

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        anyhow::bail!("Command failed with status {}: {}", output.status, stderr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_execute_bash_success() {
        let result = execute_bash("echo 'hello'").await.unwrap();
        assert_eq!(result.trim(), "hello");
    }

    #[tokio::test]
    async fn test_execute_bash_failure() {
        let result = execute_bash("invalid_command").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_bash_with_stderr() {
        let result = execute_bash("echo 'test error' >&2; exit 1").await;
        assert!(result.is_err());
        let error_message = result.err().unwrap().to_string();
        assert!(error_message.contains("test error"));
    }
}
