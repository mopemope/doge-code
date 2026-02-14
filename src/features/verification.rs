use crate::config::{AppConfig, VerificationConfig};
use crate::llm::types::ToolCall;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, warn};

#[derive(Clone)]
pub struct AutoVerifier {
    config: VerificationConfig,
    project_root: PathBuf,
}

impl Default for AutoVerifier {
    fn default() -> Self {
        Self::new(&AppConfig::default())
    }
}

impl AutoVerifier {
    pub fn new(config: &AppConfig) -> Self {
        Self {
            config: config.verification.clone(),
            project_root: config.project_root.clone(),
        }
    }

    /// Checks if the tool call warrants verification and runs it.
    /// Returns Some(warning_message) if verification fails.
    /// If enforce is true, it returns an error message indicating that the change should be rolled back.
    pub async fn verify(&self, tool_call: &ToolCall, success: bool) -> Option<VerificationResult> {
        if !success {
            return None;
        }

        if !self.config.enabled {
            return None;
        }

        let function_name = tool_call.function.name.as_str();
        if !matches!(function_name, "fs_write" | "edit" | "apply_patch") {
            return None;
        }

        let args: serde_json::Value = serde_json::from_str(&tool_call.function.arguments).ok()?;

        // Extract file path from arguments
        let path_str = match function_name {
            "fs_write" => args.get("path").and_then(|v| v.as_str()),
            "edit" => args.get("file_path").and_then(|v| v.as_str()),
            "apply_patch" => args.get("file_path").and_then(|v| v.as_str()),
            _ => None,
        }?;

        let path = Path::new(path_str);
        self.verify_path(path).await
    }

    pub async fn verify_path(&self, path: &Path) -> Option<VerificationResult> {
        let extension = path.extension().and_then(|e| e.to_str())?;

        match extension {
            "rs" => {
                self.verify_command("Rust", &self.config.commands.rust, path)
                    .await
            }
            "py" => {
                self.verify_command("Python", &self.config.commands.python, path)
                    .await
            }
            "js" | "jsx" => {
                self.verify_command("Node.js", &self.config.commands.node, path)
                    .await
            }
            "ts" | "tsx" => {
                self.verify_command("TypeScript", &self.config.commands.typescript, path)
                    .await
            }
            "go" => {
                self.verify_command("Go", &self.config.commands.go, path)
                    .await
            }
            _ => None,
        }
    }

    async fn verify_command(
        &self,
        label: &str,
        command: &[String],
        path: &Path,
    ) -> Option<VerificationResult> {
        if command.is_empty() {
            return None;
        }

        let rendered = self.render_command(command, path);
        let (program, args) = rendered.split_first()?;

        debug!("Running verification command for {}: {:?}", label, rendered);

        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(&self.project_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = match self.run_with_timeout(cmd).await {
            Ok(output) => output,
            Err(message) => {
                return Some(VerificationResult {
                    success: false,
                    stdout: String::new(),
                    stderr: message.clone(),
                    exit_code: None,
                    message: format!(
                        "<verification_error>\n{} Check Failed:\n{}\n</verification_error>",
                        label, message
                    ),
                    should_revert: self.config.enforce,
                });
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let msg = format!(
                "<verification_error>\n{} Check Failed:\n{}{}\n</verification_error>",
                label, stdout, stderr
            );
            warn!("Verification failed: {}", msg);
            return Some(VerificationResult {
                success: false,
                stdout,
                stderr,
                exit_code: output.status.code(),
                message: msg,
                should_revert: self.config.enforce,
            });
        }
        None
    }

    async fn run_with_timeout(&self, mut cmd: Command) -> Result<std::process::Output, String> {
        let timeout_ms = self.config.timeout_ms;
        cmd.kill_on_drop(true);
        let child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn verification command: {}", e))?;

        if timeout_ms == 0 {
            return child
                .wait_with_output()
                .await
                .map_err(|e| format!("Failed to wait for verification command: {}", e));
        }

        match timeout(Duration::from_millis(timeout_ms), child.wait_with_output()).await {
            Ok(result) => {
                result.map_err(|e| format!("Failed to wait for verification command: {}", e))
            }
            Err(_) => Err(format!(
                "Verification command timed out after {} ms",
                timeout_ms
            )),
        }
    }

    fn render_command(&self, command: &[String], path: &Path) -> Vec<String> {
        let path_str = path.to_string_lossy();
        let root_str = self.project_root.to_string_lossy();
        command
            .iter()
            .map(|part| {
                part.replace("{path}", &path_str)
                    .replace("{project_root}", &root_str)
            })
            .collect()
    }
}

pub struct VerificationResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub message: String,
    pub should_revert: bool,
}
