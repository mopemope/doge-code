//! Structured process execution without a shell.
//!
//! The runner spawns `Command::new(program).args(args)` directly — never
//! `bash -c`. Stdout/stderr are drained by background tasks into bounded
//! captures so RAM stays flat even for verbose builds. Timeout and agent
//! cancellation terminate the whole process tree (see `lifecycle`) and reap
//! the direct child.

use crate::config::AppConfig;
use crate::execution::lifecycle::{configure_process_group, terminate_process_tree};
use crate::execution::output::{BoundedCapture, budget_command_output};
use crate::execution::policy::{ExecutionPolicy, PolicyDenial, ProcessRequest};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::AsyncReadExt;
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
        let mut v = serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(obj) = v.as_object_mut() {
            obj.insert("ok".to_string(), serde_json::Value::Bool(self.success));
        }
        v
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
/// - Spawn failure → `Ok(spawn_failed result)`.
pub async fn run_process(
    req: ProcessRequest,
    config: &AppConfig,
    cancel: Option<CancellationToken>,
) -> anyhow::Result<ProcessResult> {
    let config_arc = Arc::new(config.clone());
    let policy = ExecutionPolicy::new(config_arc.clone());
    if let Err(denial) = policy.check_process(&req) {
        tracing::warn!(program = %req.program, error = %denial.message(), "process denied by policy");
        return Ok(ProcessResult::policy_denied(&denial));
    }

    let started = Instant::now();
    let cwd = policy.resolve_cwd(&req.cwd);
    let timeout = policy.effective_timeout(req.timeout_ms);

    tracing::info!(
        program = %req.program,
        arg_count = req.args.len(),
        cwd = %cwd.display(),
        timeout_ms = ?timeout.map(|d| d.as_millis()),
        "spawning structured process"
    );

    let mut cmd = tokio::process::Command::new(&req.program);
    cmd.args(&req.args)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Only the allowlisted keys are overridden; the rest of the environment
    // is inherited (PATH etc. cannot be replaced unless explicitly allowed).
    for (k, v) in &req.env {
        cmd.env(k, v);
    }
    configure_process_group(&mut cmd);
    // Ensure descendants die if we are dropped unexpectedly.
    cmd.kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            tracing::warn!(program = %req.program, error = %e, "process spawn failed");
            return Ok(ProcessResult::spawn_failed(format!(
                "Failed to spawn '{}': {e}",
                req.program
            )));
        }
    };

    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();

    let stdout_task = tokio::spawn(async move {
        let mut cap = BoundedCapture::new();
        if let Some(out) = stdout.as_mut() {
            let mut buf = vec![0u8; 8192];
            loop {
                match out.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => cap.push(&buf[..n]),
                    Err(_) => break,
                }
            }
        }
        cap
    });
    let stderr_task = tokio::spawn(async move {
        let mut cap = BoundedCapture::new();
        if let Some(err) = stderr.as_mut() {
            let mut buf = vec![0u8; 8192];
            loop {
                match out_read(err, &mut buf).await {
                    Ok(0) => break,
                    Ok(n) => cap.push(&buf[..n]),
                    Err(_) => break,
                }
            }
        }
        cap
    });

    let cancel_token = cancel.unwrap_or_default();
    // If no timeout is configured, wait indefinitely (still cancellable).
    let wait_fut = async { child.wait().await };
    let status_opt: Option<std::process::ExitStatus> = if let Some(dur) = timeout {
        tokio::select! {
            biased;
            _ = cancel_token.cancelled() => {
                tracing::warn!(program = %req.program, "process cancelled; terminating tree");
                terminate_process_tree(&mut child).await;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                return Err(anyhow::anyhow!(crate::llm::LlmErrorKind::Cancelled));
            }
            res = tokio::time::timeout(dur, wait_fut) => {
                match res {
                    Ok(Ok(status)) => Some(status),
                    Ok(Err(e)) => {
                        tracing::warn!(program = %req.program, error = %e, "wait failed");
                        let _ = stdout_task.await;
                        let _ = stderr_task.await;
                        return Ok(ProcessResult::spawn_failed(format!("Failed to wait: {e}")));
                    }
                    Err(_) => {
                        tracing::warn!(program = %req.program, timeout_ms = dur.as_millis(), "process timed out; terminating tree");
                        terminate_process_tree(&mut child).await;
                        let stdout_cap = stdout_task.await.unwrap_or_default();
                        let stderr_cap = stderr_task.await.unwrap_or_default();
                        let (raw_out, out_trunc) = stdout_cap.finish();
                        let (raw_err, err_trunc) = stderr_cap.finish();
                        let (stdout_s, stderr_s, budgeted, mut warnings) =
                            budget_command_output(&raw_out, &raw_err);
                        if out_trunc || err_trunc {
                            warnings.push("process output exceeded capture limits; head and tail preserved".to_string());
                        }
                        // Timeout means the output is necessarily partial: the
                        // tree was killed mid-run, so always flag truncation
                        // even when the captured bytes fit the budget.
                        let _ = budgeted;
                        return Ok(ProcessResult {
                            success: false,
                            status: ProcessStatus::TimedOut,
                            exit_code: None,
                            stdout: stdout_s,
                            stderr: stderr_s,
                            output_truncated: true,
                            warnings,
                            error: Some(format!("Command timed out after {} ms", dur.as_millis())),
                        });
                    }
                }
            }
        }
    } else {
        tokio::select! {
            biased;
            _ = cancel_token.cancelled() => {
                tracing::warn!(program = %req.program, "process cancelled; terminating tree");
                terminate_process_tree(&mut child).await;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                return Err(anyhow::anyhow!(crate::llm::LlmErrorKind::Cancelled));
            }
            res = wait_fut => {
                match res {
                    Ok(status) => Some(status),
                    Err(e) => {
                        let _ = stdout_task.await;
                        let _ = stderr_task.await;
                        return Ok(ProcessResult::spawn_failed(format!("Failed to wait: {e}")));
                    }
                }
            }
        }
    };

    // Normal completion: drain tasks have seen EOF; join them.
    let stdout_cap = stdout_task.await.unwrap_or_default();
    let stderr_cap = stderr_task.await.unwrap_or_default();
    let (raw_out, out_trunc) = stdout_cap.finish();
    let (raw_err, err_trunc) = stderr_cap.finish();
    let (stdout_s, stderr_s, budgeted, mut warnings) = budget_command_output(&raw_out, &raw_err);
    if out_trunc || err_trunc {
        warnings
            .push("process output exceeded capture limits; head and tail preserved".to_string());
    }

    let (exit_code, success) = match status_opt {
        Some(st) => (st.code(), st.success()),
        None => (None, false),
    };
    tracing::info!(
        program = %req.program,
        exit_code = ?exit_code,
        elapsed_ms = started.elapsed().as_millis(),
        "process completed"
    );
    // NOTE: env values are never logged.

    Ok(ProcessResult {
        success,
        status: ProcessStatus::Completed,
        exit_code,
        stdout: stdout_s,
        stderr: stderr_s,
        output_truncated: budgeted || out_trunc || err_trunc,
        warnings,
        error: if success {
            None
        } else {
            Some(format!("Process exited with code {exit_code:?}"))
        },
    })
}

async fn out_read(r: &mut tokio::process::ChildStderr, buf: &mut [u8]) -> std::io::Result<usize> {
    r.read(buf).await
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
        let res = run_process(req, &cfg, None).await.unwrap();
        assert!(res.success);
        assert_eq!(res.status, ProcessStatus::Completed);
        assert_eq!(res.exit_code, Some(0));
        assert!(res.stdout.contains("hello"));
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
        let res = run_process(req, &cfg, None).await.unwrap();
        assert!(res.success);
        assert!(
            !marker.exists(),
            "shell injection must not create the marker file"
        );
        assert!(res.stdout.contains("hello; touch"));
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
        // NOTE: this specific test spawns bash directly (not via shell
        // string) to check exit-code semantics.
        let res = run_process(req, &cfg, None).await.unwrap();
        assert!(!res.success);
        assert_eq!(res.status, ProcessStatus::Completed);
        assert_eq!(res.exit_code, Some(3));
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
        let res = run_process(req, &cfg, None).await.unwrap();
        assert!(!res.success);
        assert_eq!(res.status, ProcessStatus::SpawnFailed);
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
        let res = run_process(req, &cfg, None).await.unwrap();
        assert!(!res.success);
        assert_eq!(res.status, ProcessStatus::TimedOut);
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
        // bash spawns a background sleep (grandchild) and waits.
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
        let res = run_process(req, &cfg, None).await.unwrap();
        assert_eq!(res.status, ProcessStatus::TimedOut);
        // Give the kernel a moment, then check the grandchild is gone.
        tokio::time::sleep(Duration::from_millis(300)).await;
        if let Ok(pid_str) = std::fs::read_to_string(&pid_file)
            && let Ok(pid) = pid_str.trim().parse::<u32>()
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
        let t2 = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            t2.cancel();
        });
        let err = run_process(req, &cfg, Some(token)).await.unwrap_err();
        assert!(err.downcast_ref::<crate::llm::LlmErrorKind>().is_some());
    }

    #[tokio::test]
    async fn test_large_output_bounded() {
        let cfg = test_config();
        // `seq` prints 20000 lines; internal capture + final payload must stay bounded.
        let req = ProcessRequest {
            program: "seq".to_string(),
            args: vec!["1".to_string(), "20000".to_string()],
            cwd: Some(cfg.project_root.clone()),
            env: BTreeMap::new(),
            timeout_ms: Some(15_000),
        };
        let res = run_process(req, &cfg, None).await.unwrap();
        assert!(res.success);
        assert!(res.output_truncated);
        assert!(!res.warnings.is_empty());
        let combined = res.stdout.chars().count() + res.stderr.chars().count();
        assert!(combined <= 7_000, "combined too large: {combined}");
    }
}
