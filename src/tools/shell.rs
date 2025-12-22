use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tracing::info;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
pub struct ExecuteShellResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub success: bool,
}

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "execute_shell".to_string(),
            description: "Executes a shell command in a persistent bash session. Unlike execute_bash, this maintains state (cwd, environment variables) between calls. Use this for sequences of commands that depend on each other (e.g., activating a venv then running a script, or cd into a directory then running make). Returns stdout, stderr, and exit code.".to_string(),
            strict: None,
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The command line to execute."}
                },
                "required": ["command"]
            }),
        },
    }
}

#[derive(Debug)]
pub struct ShellSession {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    // We capture stderr via stdout by redirecting 2>&1 in the sentinel wrapper,
    // or we could handle it separately. For simplicity and guaranteed ordering,
    // merging is often easier, but we lose distinction.
    // However, to keep distinct stdout/stderr, we need to read both.
    // Let's try to read both.
    stderr: Option<BufReader<tokio::process::ChildStderr>>,
    project_root: std::path::PathBuf,
}

// Global or shared state wrapper
#[derive(Clone, Debug)]
pub struct SharedShellSession(Arc<Mutex<ShellSession>>);

impl SharedShellSession {
    pub fn new(project_root: std::path::PathBuf) -> Self {
        Self(Arc::new(Mutex::new(ShellSession::new(project_root))))
    }

    pub async fn exec(&self, command: &str) -> Result<String> {
        let mut session = self.0.lock().await;
        // Ensure session is running
        if !session.is_running() {
            session.start()?;
        }
        let result = session.exec_command(command).await?;
        Ok(serde_json::to_string(&result)?)
    }
}

impl ShellSession {
    pub fn new(project_root: std::path::PathBuf) -> Self {
        Self {
            child: None,
            stdin: None,
            stdout: None,
            stderr: None,
            project_root,
        }
    }

    pub fn is_running(&mut self) -> bool {
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(Some(_)) => false, // Exited
                Ok(None) => true,     // Still running
                Err(_) => false,
            }
        } else {
            false
        }
    }

    pub fn start(&mut self) -> Result<()> {
        info!("Starting new persistent shell session");
        let mut child = Command::new("bash")
            .current_dir(&self.project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Don't inherit environment to avoid polluting, or do?
            // Usually inheriting is fine, users expect standard env.
            .spawn()
            .context("Failed to spawn bash session")?;

        let stdin = child.stdin.take().context("Failed to open stdin")?;
        let stdout = BufReader::new(child.stdout.take().context("Failed to open stdout")?);
        let stderr = BufReader::new(child.stderr.take().context("Failed to open stderr")?);

        self.child = Some(child);
        self.stdin = Some(stdin);
        self.stdout = Some(stdout);
        self.stderr = Some(stderr);

        Ok(())
    }

    pub async fn exec_command(&mut self, cmd: &str) -> Result<ExecuteShellResult> {
        let sentinel = Uuid::new_v4().to_string();
        // command; EC=$?; echo "SENTINEL:$EC:$ID"; echo "SENTINEL_ERR:$ID" >&2
        let sentinel_cmd = format!(
            "{}\nexport __DOGE_EC=$?; echo \"__DOGE_SENTINEL:$__DOGE_EC:{}\"; echo \"__DOGE_SENTINEL_ERR:{}\" >&2\n",
            cmd, sentinel, sentinel
        );

        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("Shell stdin not available"))?;
        stdin
            .write_all(sentinel_cmd.as_bytes())
            .await
            .context("Failed to write to shell stdin")?;
        stdin.flush().await.context("Failed to flush shell stdin")?;

        let stdout_reader = self
            .stdout
            .as_mut()
            .ok_or_else(|| anyhow!("Shell stdout not available"))?;
        let stderr_reader = self
            .stderr
            .as_mut()
            .ok_or_else(|| anyhow!("Shell stderr not available"))?;

        let stdout_prefix = "__DOGE_SENTINEL:".to_string();
        let stderr_prefix = "__DOGE_SENTINEL_ERR:".to_string();

        let (out_res, err_res) = tokio::join!(
            read_until_sentinel(stdout_reader, &stdout_prefix, &sentinel),
            read_until_sentinel(stderr_reader, &stderr_prefix, &sentinel)
        );

        let (stdout, exit_code) = out_res?;
        let (stderr, _) = err_res?;

        let success = exit_code.map(|c| c == 0).unwrap_or(false);

        Ok(ExecuteShellResult {
            stdout,
            stderr,
            exit_code,
            success,
        })
    }
}

async fn read_until_sentinel<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    prefix: &str,
    sentinel_id: &str,
) -> Result<(String, Option<i32>)> {
    let mut output = String::new();
    let mut lines = reader.lines();
    let mut exit_code = None;

    while let Some(line) = lines.next_line().await? {
        if line.contains(prefix) && line.contains(sentinel_id) {
            // Found sentinel
            // Parse exit code if present (only in stdout sentinel)
            if let Some(idx) = line.find(prefix) {
                let content = &line[idx + prefix.len()..];
                // Format: ECO:ID or just ID
                let parts: Vec<&str> = content.split(':').collect();
                if parts.len() >= 2 && parts[1] == sentinel_id {
                    // Format: CODE:ID
                    if let Ok(code) = parts[0].parse::<i32>() {
                        exit_code = Some(code);
                    }
                }
            }
            break;
        }
        output.push_str(&line);
        output.push('\n');
    }
    // Remove last newline if added
    if output.ends_with('\n') {
        output.pop();
    }
    Ok((output, exit_code))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_shell_session_basic() {
        let temp_dir = TempDir::new().unwrap();
        let mut session = ShellSession::new(temp_dir.path().to_path_buf());
        session.start().unwrap();

        let res = session.exec_command("echo hello").await.unwrap();
        assert_eq!(res.stdout.trim(), "hello");
        assert!(res.success);
    }

    #[tokio::test]
    async fn test_shell_session_state() {
        let temp_dir = TempDir::new().unwrap();
        let mut session = ShellSession::new(temp_dir.path().to_path_buf());
        session.start().unwrap();

        session.exec_command("export FOO=bar").await.unwrap();
        let res = session.exec_command("echo $FOO").await.unwrap();
        assert_eq!(res.stdout.trim(), "bar");
    }

    #[tokio::test]
    async fn test_shell_session_cwd() {
        let temp_dir = TempDir::new().unwrap();
        let mut session = ShellSession::new(temp_dir.path().to_path_buf());
        session.start().unwrap();

        let subdir = temp_dir.path().join("subdir");
        tokio::fs::create_dir(&subdir).await.unwrap();

        // Relative path due to initial cwd
        session.exec_command("cd subdir").await.unwrap();
        let res = session.exec_command("pwd").await.unwrap();

        // Resolve symlinks for accurate comparison if needed, but basic check:
        assert!(res.stdout.trim().ends_with("subdir"));
    }

    #[tokio::test]
    async fn test_shell_session_error() {
        let temp_dir = TempDir::new().unwrap();
        let mut session = ShellSession::new(temp_dir.path().to_path_buf());
        session.start().unwrap();

        let res = session.exec_command("nonexistent_command").await.unwrap();
        assert!(!res.success);
        assert!(res.exit_code.is_some());
        assert_ne!(res.exit_code.unwrap(), 0);
        // Stderr should contain something
        assert!(!res.stderr.is_empty());

        // Session should still work
        let res2 = session.exec_command("echo alive").await.unwrap();
        assert_eq!(res2.stdout.trim(), "alive");
    }
}
