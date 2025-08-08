use anyhow::{Context, Result};
use tokio::process::Command;
use std::path::PathBuf;

pub async fn execute_bash(root: &PathBuf, command: &str) -> Result<String> {
    let output = Command::new("bash")
        .arg("-c")
        .arg(command)
        .current_dir(root)
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
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_execute_bash_success() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();

        let result = execute_bash(&root, "echo 'hello'").await.unwrap();
        assert_eq!(result.trim(), "hello");
    }

    #[tokio::test]
    async fn test_execute_bash_failure() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();

        let result = execute_bash(&root, "invalid_command").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_bash_with_stderr() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();

        let result = execute_bash(&root, "echo 'test error' >&2; exit 1").await;
        assert!(result.is_err());
        let error_message = result.err().unwrap().to_string();
        assert!(error_message.contains("test error"));
    }
}
