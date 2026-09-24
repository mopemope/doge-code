//! Structured, policy-aware process execution adapter.
//!
//! This module is intentionally an adapter around the policy-free
//! [`crate::execution::runner`]. It applies LLM-facing execution policy,
//! resolves the request timeout, and maps the managed result into the stable
//! `ProcessResult` JSON contract. It never implements a second process
//! lifecycle.

use crate::config::AppConfig;
use crate::execution::output::budget_command_output;
use crate::execution::policy::{ExecutionPolicy, PolicyDenial, ProcessRequest};
use crate::execution::runner::{
    ManagedProcessSpec, ManagedProcessTermination, ManagedRunOptions, run_managed_process,
};
use crate::llm::LlmErrorKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

/// Terminal status of a structured process run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStatus {
    Completed,
    TimedOut,
    PolicyDenied,
    SpawnFailed,
}

/// Structured result returned to the LLM (JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessResult {
    pub success: bool,
    pub status: ProcessStatus,
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub output_truncated: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ProcessResult {
    pub fn policy_denied(denial: &PolicyDenial) -> Self {
        Self {
            success: false,
            status: ProcessStatus::PolicyDenied,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            output_truncated: false,
            warnings: Vec::new(),
            error: Some(denial.message()),
        }
    }

    pub fn spawn_failed(msg: impl Into<String>) -> Self {
        Self {
            success: false,
            status: ProcessStatus::SpawnFailed,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            output_truncated: false,
            warnings: Vec::new(),
            error: Some(msg.into()),
        }
    }

    /// Serialize for the tool layer with `ok` mirroring `success`
    /// (`ok == success` invariant).
    pub fn to_json(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(object) = value.as_object_mut() {
            object.insert("ok".to_string(), serde_json::Value::Bool(self.success));
        }
        value
    }
}

/// Parameters for one structured run (tool schema shape).
#[derive(Debug, Clone, Deserialize)]
pub struct ExecuteProcessParams {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

impl ExecuteProcessParams {
    pub fn into_request(self) -> ProcessRequest {
        ProcessRequest {
            program: self.program,
            args: self.args,
            cwd: self.cwd,
            env: self.env,
            timeout_ms: self.timeout_ms,
        }
    }
}

/// Run a structured process to completion.
///
/// - Policy denial → `Ok(policy_denied result)`.
/// - Timeout → `Ok(timed_out result)` (tree killed, child reaped).
/// - Agent cancellation → `Err(Cancelled)` (tree killed, child reaped,
///   propagates to the agent loop; NOT a normal LLM-visible failure).
/// - Spawn/wait/cleanup failure → `Ok(spawn_failed result)` for compatibility.
pub async fn run_process(
    req: ProcessRequest,
    config: &AppConfig,
    cancel: Option<CancellationToken>,
) -> anyhow::Result<ProcessResult> {
    if cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
        return Err(anyhow::anyhow!(LlmErrorKind::Cancelled));
    }
    let config_arc = Arc::new(config.clone());
    let policy = ExecutionPolicy::new(config_arc);
    if let Err(denial) = policy.check_process(&req) {
        tracing::warn!(program = %req.program, error = %denial.message(), "process denied by policy");
        return Ok(ProcessResult::policy_denied(&denial));
    }

    let started = Instant::now();
    let cwd = policy.resolve_cwd(&req.cwd);
    let timeout = policy.effective_timeout(req.timeout_ms);
    let program = req.program.clone();
    let spec = ManagedProcessSpec {
        program: req.program,
        args: req.args,
        cwd,
        env: req.env,
    };
    let options = ManagedRunOptions {
        timeout,
        cancellation: cancel,
    };

    let managed = match run_managed_process(spec, options).await {
        Ok(output) => output,
        Err(crate::execution::ManagedProcessError::Spawn(error)) => {
            return Ok(ProcessResult::spawn_failed(format!(
                "Failed to spawn '{program}': {error}"
            )));
        }
        Err(crate::execution::ManagedProcessError::Wait(error)) => {
            return Ok(ProcessResult::spawn_failed(format!(
                "Failed to wait for process: {error}"
            )));
        }
        Err(crate::execution::ManagedProcessError::Cleanup(error)) => {
            return Ok(ProcessResult::spawn_failed(format!(
                "Failed to clean up process tree: {error}"
            )));
        }
    };

    match managed.termination {
        ManagedProcessTermination::Cancelled => {
            return Err(anyhow::anyhow!(LlmErrorKind::Cancelled));
        }
        ManagedProcessTermination::TimedOut => {
            let (stdout, stderr, _budgeted, mut warnings) =
                budget_command_output(&managed.stdout, &managed.stderr);
            if managed.capture_truncated {
                warnings.push(
                    "process output exceeded capture limits; head and tail preserved".to_string(),
                );
            }
            let timeout_ms = timeout.map(|duration| duration.as_millis()).unwrap_or(0);
            tracing::warn!(
                program = %program,
                timeout_ms,
                elapsed_ms = started.elapsed().as_millis(),
                "process timed out"
            );
            return Ok(ProcessResult {
                success: false,
                status: ProcessStatus::TimedOut,
                // A timeout has no meaningful child exit status. Do not invent
                // a signal-derived code such as 137.
                exit_code: None,
                stdout,
                stderr,
                output_truncated: true,
                warnings,
                error: Some(format!("Command timed out after {timeout_ms} ms")),
            });
        }
        ManagedProcessTermination::Exited => {}
    }

    let (stdout, stderr, budgeted, mut warnings) =
        budget_command_output(&managed.stdout, &managed.stderr);
    if managed.capture_truncated {
        warnings
            .push("process output exceeded capture limits; head and tail preserved".to_string());
    }
    let success = managed.success();
    let exit_code = managed.exit_code;
    tracing::info!(
        exit_code = ?exit_code,
        elapsed_ms = started.elapsed().as_millis(),
        "process completed"
    );
    // Environment values are never logged.

    Ok(ProcessResult {
        success,
        status: ProcessStatus::Completed,
        exit_code,
        stdout,
        stderr,
        output_truncated: budgeted || managed.capture_truncated,
        warnings,
        error: if success {
            None
        } else {
            Some(format!("Process exited with code {exit_code:?}"))
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ExecutionConfig;
    use std::time::Duration;
    use tempfile::TempDir;

    fn test_config() -> AppConfig {
        let dir = TempDir::new().unwrap();
        let root = dir.keep();
        AppConfig {
            project_root: root,
            execution: ExecutionConfig::default(),
            ..Default::default()
        }
    }

    fn req_echo(msg: &str) -> ProcessRequest {
        ProcessRequest {
            program: "echo".to_string(),
            args: vec![msg.to_string()],
            cwd: None,
            env: BTreeMap::new(),
            timeout_ms: Some(10_000),
        }
    }

    #[tokio::test]
    async fn test_run_echo_success() {
        let cfg = test_config();
        let mut req = req_echo("hello");
        req.cwd = Some(cfg.project_root.clone());
        let result = run_process(req, &cfg, None).await.unwrap();
        assert!(result.success);
        assert_eq!(result.status, ProcessStatus::Completed);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.stdout.contains("hello"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_injection_not_executed() {
        // args must pass through as a single argv element, never via a shell.
        let dir = TempDir::new().unwrap();
        let marker = dir.path().join("OWNED");
        assert!(!marker.exists());
        let cfg = AppConfig {
            project_root: dir.path().to_path_buf(),
            ..Default::default()
        };
        let req = ProcessRequest {
            program: "echo".to_string(),
            args: vec![format!("hello; touch {}", marker.display())],
            cwd: Some(dir.path().to_path_buf()),
            env: BTreeMap::new(),
            timeout_ms: Some(10_000),
        };
        let result = run_process(req, &cfg, None).await.unwrap();
        assert!(result.success);
        assert!(
            !marker.exists(),
            "shell injection must not create the marker file"
        );
        assert!(result.stdout.contains("hello; touch"));
    }

    #[tokio::test]
    async fn test_exit_nonzero_is_completed_failure() {
        let cfg = test_config();
        let req = ProcessRequest {
            program: "bash".to_string(),
            args: vec!["-c".to_string(), "exit 3".to_string()],
            cwd: Some(cfg.project_root.clone()),
            env: BTreeMap::new(),
            timeout_ms: Some(10_000),
        };
        let result = run_process(req, &cfg, None).await.unwrap();
        assert!(!result.success);
        assert_eq!(result.status, ProcessStatus::Completed);
        assert_eq!(result.exit_code, Some(3));
    }

    #[tokio::test]
    async fn test_spawn_failure() {
        let cfg = test_config();
        let req = ProcessRequest {
            program: "definitely-not-a-real-binary-xyz".to_string(),
            args: vec![],
            cwd: Some(cfg.project_root.clone()),
            env: BTreeMap::new(),
            timeout_ms: Some(10_000),
        };
        let result = run_process(req, &cfg, None).await.unwrap();
        assert!(!result.success);
        assert_eq!(result.status, ProcessStatus::SpawnFailed);
    }

    #[tokio::test]
    async fn test_timeout_kills_and_reports() {
        let cfg = test_config();
        let req = ProcessRequest {
            program: "sleep".to_string(),
            args: vec!["30".to_string()],
            cwd: Some(cfg.project_root.clone()),
            env: BTreeMap::new(),
            timeout_ms: Some(500),
        };
        let start = Instant::now();
        let result = run_process(req, &cfg, None).await.unwrap();
        assert!(!result.success);
        assert_eq!(result.status, ProcessStatus::TimedOut);
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "must not wait for the full sleep"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_timeout_kills_descendants() {
        use crate::execution::lifecycle::is_process_alive;
        let dir = TempDir::new().unwrap();
        let pid_file = dir.path().join("child.pid");
        let cfg = AppConfig {
            project_root: dir.path().to_path_buf(),
            ..Default::default()
        };
        let req = ProcessRequest {
            program: "bash".to_string(),
            args: vec![
                "-c".to_string(),
                format!("sleep 30 & echo $! > '{}'; wait", pid_file.display()),
            ],
            cwd: Some(dir.path().to_path_buf()),
            env: BTreeMap::new(),
            timeout_ms: Some(1_000),
        };
        let result = run_process(req, &cfg, None).await.unwrap();
        assert_eq!(result.status, ProcessStatus::TimedOut);
        tokio::time::sleep(Duration::from_millis(300)).await;
        if let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
        {
            assert!(!is_process_alive(pid), "grandchild sleep must be killed");
        }
    }

    #[tokio::test]
    async fn test_cancellation_propagates() {
        let cfg = test_config();
        let req = ProcessRequest {
            program: "sleep".to_string(),
            args: vec!["30".to_string()],
            cwd: Some(cfg.project_root.clone()),
            env: BTreeMap::new(),
            timeout_ms: Some(30_000),
        };
        let token = CancellationToken::new();
        let token_for_timer = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            token_for_timer.cancel();
        });
        let error = run_process(req, &cfg, Some(token)).await.unwrap_err();
        assert!(error.downcast_ref::<LlmErrorKind>().is_some());
    }

    #[tokio::test]
    async fn test_large_output_bounded() {
        let cfg = test_config();
        let req = ProcessRequest {
            program: "seq".to_string(),
            args: vec!["1".to_string(), "20000".to_string()],
            cwd: Some(cfg.project_root.clone()),
            env: BTreeMap::new(),
            timeout_ms: Some(15_000),
        };
        let result = run_process(req, &cfg, None).await.unwrap();
        assert!(result.success);
        assert!(result.output_truncated);
        assert!(!result.warnings.is_empty());
        let combined = result.stdout.chars().count() + result.stderr.chars().count();
        assert!(combined <= 7_000, "combined too large: {combined}");
    }

    #[tokio::test]
    async fn test_zero_config_timeout_is_unlimited_for_fast_command() {
        let dir = TempDir::new().unwrap();
        let cfg = AppConfig {
            project_root: dir.path().to_path_buf(),
            command_timeout_ms: 0,
            ..Default::default()
        };
        let req = ProcessRequest {
            program: "printf".to_string(),
            args: vec!["ok".to_string()],
            cwd: Some(dir.path().to_path_buf()),
            env: BTreeMap::new(),
            timeout_ms: None,
        };
        let result = run_process(req, &cfg, None).await.unwrap();
        assert!(result.success);
        assert_eq!(result.status, ProcessStatus::Completed);
    }
}
