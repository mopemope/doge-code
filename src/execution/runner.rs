//! Policy-free managed process runner.
//!
//! This module owns finite process mechanics only: spawn, bounded streaming
//! capture, timeout/cancellation, process-group cleanup, and direct-child
//! reaping. It deliberately does not know about `ExecutionPolicy`, allowed
//! programs, shell escape rules, or LLM metadata. Callers apply those
//! concerns before constructing a [`ManagedProcessSpec`].

use crate::execution::lifecycle::{
    ProcessGroupHandle, cleanup_process_group_after_exit, configure_process_group,
    terminate_process_tree_with_group,
};
use crate::execution::output::BoundedCapture;
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// A finite process-tree request owned by one managed runner invocation.
///
/// The runner treats `cwd` and `env` as mechanics, not policy. Callers that
/// accept untrusted input must validate them before calling this API.
#[derive(Debug, Clone)]
pub struct ManagedProcessSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
}

impl ManagedProcessSpec {
    pub fn new(program: impl Into<String>, args: Vec<String>, cwd: PathBuf) -> Self {
        Self {
            program: program.into(),
            args,
            cwd,
            env: BTreeMap::new(),
        }
    }

    pub fn with_env(mut self, env: BTreeMap<String, String>) -> Self {
        self.env = env;
        self
    }
}

/// Runtime controls for one finite process-tree request.
///
/// `timeout: None` means unlimited. A cancellation token only controls this
/// request; it is not a policy or a process-tree identity.
#[derive(Debug, Clone, Default)]
pub struct ManagedRunOptions {
    pub timeout: Option<Duration>,
    pub cancellation: Option<CancellationToken>,
}

impl ManagedRunOptions {
    pub fn new(timeout: Option<Duration>) -> Self {
        Self {
            timeout,
            cancellation: None,
        }
    }

    pub fn with_cancellation(mut self, cancellation: Option<CancellationToken>) -> Self {
        self.cancellation = cancellation;
        self
    }
}

/// Why a managed finite process stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedProcessTermination {
    Exited,
    TimedOut,
    Cancelled,
}

/// Result of a managed process run after its process tree has been cleaned up
/// and its direct child has been reaped (on normal/error paths).
#[derive(Debug, Clone)]
pub struct ManagedProcessOutput {
    pub termination: ManagedProcessTermination,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub capture_truncated: bool,
    pub warnings: Vec<String>,
}

impl ManagedProcessOutput {
    pub fn success(&self) -> bool {
        self.termination == ManagedProcessTermination::Exited && self.exit_code == Some(0)
    }
}

/// Typed failures for runner operations which do not have a normal process
/// result. Timeout and cancellation are represented in
/// [`ManagedProcessOutput`] instead of being collapsed into this error type.
#[derive(Debug, thiserror::Error)]
pub enum ManagedProcessError {
    #[error("failed to spawn process: {0}")]
    Spawn(#[source] io::Error),
    #[error("failed to wait for process: {0}")]
    Wait(#[source] io::Error),
    #[error("failed to clean up process tree: {0}")]
    Cleanup(#[source] io::Error),
}

/// Run one finite process tree through the shared managed lifecycle.
///
/// The function does not apply execution policy. It always captures output in
/// bounded memory, terminates the owned process group on timeout/cancellation,
/// and retains the group handle until cleanup has completed. Dropping this
/// future is also safe on Unix: the retained [`ProcessGroupHandle`] sends a
/// best-effort SIGKILL to the group.
pub async fn run_managed_process(
    spec: ManagedProcessSpec,
    options: ManagedRunOptions,
) -> Result<ManagedProcessOutput, ManagedProcessError> {
    let cancellation = options.cancellation.unwrap_or_default();
    if cancellation.is_cancelled() {
        return Ok(empty_output(ManagedProcessTermination::Cancelled));
    }

    let started = Instant::now();
    tracing::info!(
        program = %spec.program,
        arg_count = spec.args.len(),
        cwd = %spec.cwd.display(),
        timeout_ms = ?options.timeout.map(|duration| duration.as_millis()),
        "spawning managed process"
    );

    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in &spec.env {
        command.env(key, value);
    }
    configure_process_group(&mut command);
    // Direct-child safety net. It is not a substitute for ProcessGroupHandle.
    command.kill_on_drop(true);

    let mut child = command.spawn().map_err(ManagedProcessError::Spawn)?;
    // Capture the PGID immediately, before any wait/termination path can make
    // Child::id() unavailable.
    let mut group = ProcessGroupHandle::from_child(&child);

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_task = spawn_capture(stdout);
    let stderr_task = spawn_capture(stderr);

    let wait_decision = wait_for_process(&mut child, options.timeout, &cancellation).await;

    let (termination, cleanup_error) = match wait_decision {
        WaitDecision::Exited(status) => {
            // A finite command owns its complete process group even after its
            // direct child exits. Do not leave background workers behind.
            let cleanup_error = cleanup_process_group_after_exit(&mut group).await.err();
            let stdout = collect_capture(stdout_task).await;
            let stderr = collect_capture(stderr_task).await;
            let (stdout_text, stdout_truncated) = stdout.capture.finish();
            let (stderr_text, stderr_truncated) = stderr.capture.finish();
            let mut warnings = stdout.warnings;
            warnings.extend(stderr.warnings);
            if stdout_truncated || stderr_truncated {
                warnings.push(
                    "process output exceeded capture limits; head and tail preserved".to_string(),
                );
            }
            if let Some(error) = cleanup_error {
                tracing::warn!(error = %error, "residual process-group cleanup failed");
                return Err(ManagedProcessError::Cleanup(error));
            }
            let exit_code = status.code();
            tracing::info!(
                program = %spec.program,
                termination = "exited",
                exit_code = ?exit_code,
                elapsed_ms = started.elapsed().as_millis(),
                "managed process completed"
            );
            return Ok(ManagedProcessOutput {
                termination: ManagedProcessTermination::Exited,
                exit_code,
                stdout: stdout_text,
                stderr: stderr_text,
                capture_truncated: stdout_truncated || stderr_truncated,
                warnings,
            });
        }
        WaitDecision::TimedOut => {
            let cleanup_error = terminate_and_reap(&mut child, &mut group, "timeout")
                .await
                .err();
            (ManagedProcessTermination::TimedOut, cleanup_error)
        }
        WaitDecision::Cancelled => {
            let cleanup_error = terminate_and_reap(&mut child, &mut group, "cancellation")
                .await
                .err();
            (ManagedProcessTermination::Cancelled, cleanup_error)
        }
        WaitDecision::WaitError(error) => {
            if let Err(cleanup_error) =
                terminate_and_reap(&mut child, &mut group, "wait-error").await
            {
                tracing::warn!(error = %cleanup_error, "managed process cleanup failed");
            }
            // Reap/close the reader tasks even though the typed wait error is
            // returned to the caller; dropping JoinHandles would detach them.
            let _ = collect_capture(stdout_task).await;
            let _ = collect_capture(stderr_task).await;
            return Err(ManagedProcessError::Wait(error));
        }
    };

    let stdout = collect_capture(stdout_task).await;
    let stderr = collect_capture(stderr_task).await;
    let (stdout_text, stdout_truncated) = stdout.capture.finish();
    let (stderr_text, stderr_truncated) = stderr.capture.finish();
    let mut warnings = stdout.warnings;
    warnings.extend(stderr.warnings);
    if stdout_truncated || stderr_truncated {
        warnings
            .push("process output exceeded capture limits; head and tail preserved".to_string());
    }
    warnings.push(match termination {
        ManagedProcessTermination::TimedOut => format!(
            "managed process timed out after {} ms",
            options
                .timeout
                .map(|duration| duration.as_millis())
                .unwrap_or(0)
        ),
        ManagedProcessTermination::Cancelled => "managed process cancelled".to_string(),
        ManagedProcessTermination::Exited => String::new(),
    });
    if let Some(error) = cleanup_error {
        warnings.push(format!("managed process cleanup failed: {error}"));
    }
    warnings.retain(|warning| !warning.is_empty());

    tracing::info!(
        program = %spec.program,
        termination = ?termination,
        exit_code = ?Option::<i32>::None,
        elapsed_ms = started.elapsed().as_millis(),
        "managed process terminated"
    );
    Ok(ManagedProcessOutput {
        termination,
        exit_code: None,
        stdout: stdout_text,
        stderr: stderr_text,
        capture_truncated: stdout_truncated || stderr_truncated,
        warnings,
    })
}

enum WaitDecision {
    Exited(std::process::ExitStatus),
    TimedOut,
    Cancelled,
    WaitError(io::Error),
}

async fn wait_for_process(
    child: &mut Child,
    timeout: Option<Duration>,
    cancellation: &CancellationToken,
) -> WaitDecision {
    if let Some(duration) = timeout {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => WaitDecision::Cancelled,
            result = tokio::time::timeout(duration, child.wait()) => {
                classify_wait_result(result.map_err(|_| ()))
            }
        }
    } else {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => WaitDecision::Cancelled,
            result = child.wait() => classify_wait_result(Ok(result)),
        }
    }
}

/// Keep the three outcomes of a wait/timeout operation distinct. In
/// particular, `Ok(Err(wait_error))` must never be interpreted as a clean
/// process exit; the inner error is preserved for the typed runner error.
fn classify_wait_result(result: Result<io::Result<std::process::ExitStatus>, ()>) -> WaitDecision {
    match result {
        Ok(Ok(status)) => WaitDecision::Exited(status),
        Ok(Err(error)) => WaitDecision::WaitError(error),
        Err(()) => WaitDecision::TimedOut,
    }
}

async fn terminate_and_reap(
    child: &mut Child,
    group: &mut ProcessGroupHandle,
    reason: &str,
) -> io::Result<()> {
    match terminate_process_tree_with_group(child, group).await {
        Ok(()) => Ok(()),
        Err(error) => {
            tracing::warn!(reason, error = %error, "managed process cleanup failed");
            Err(error)
        }
    }
}

struct CaptureTaskResult {
    capture: BoundedCapture,
    warnings: Vec<String>,
}

fn spawn_capture<R>(reader: Option<R>) -> JoinHandle<CaptureTaskResult>
where
    R: AsyncRead + Send + Unpin + 'static,
{
    tokio::spawn(async move {
        let mut capture = BoundedCapture::new();
        let mut warnings = Vec::new();
        if let Some(mut reader) = reader {
            let mut buffer = vec![0_u8; 8192];
            loop {
                match reader.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(count) => capture.push(&buffer[..count]),
                    Err(error) => {
                        warnings.push(format!("process output reader failed: {error}"));
                        break;
                    }
                }
            }
        }
        CaptureTaskResult { capture, warnings }
    })
}

async fn collect_capture(mut task: JoinHandle<CaptureTaskResult>) -> CaptureTaskResult {
    // A correctly terminated process closes both pipes. Keep a bounded safety
    // valve for unusual descriptors inherited by a descendant.
    match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => CaptureTaskResult {
            capture: BoundedCapture::new(),
            warnings: vec![format!("process output reader task failed: {error}")],
        },
        Err(_) => {
            task.abort();
            let _ = task.await;
            CaptureTaskResult {
                capture: BoundedCapture::new(),
                warnings: vec!["process output reader did not close after cleanup".to_string()],
            }
        }
    }
}

fn empty_output(termination: ManagedProcessTermination) -> ManagedProcessOutput {
    ManagedProcessOutput {
        termination,
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        capture_truncated: false,
        warnings: vec!["managed process cancelled before spawn".to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn spec(program: &str, args: &[&str], root: &Path) -> ManagedProcessSpec {
        ManagedProcessSpec::new(
            program,
            args.iter().map(|arg| (*arg).to_string()).collect(),
            root.to_path_buf(),
        )
    }

    #[test]
    fn test_wait_error_not_treated_as_clean_exit() {
        let error = io::Error::other("wait failed");
        assert!(matches!(
            classify_wait_result(Ok(Err(error))),
            WaitDecision::WaitError(_)
        ));
        assert!(matches!(
            classify_wait_result(Err(())),
            WaitDecision::TimedOut
        ));
    }

    #[tokio::test]
    async fn test_managed_process_success() {
        let dir = TempDir::new().unwrap();
        let output = run_managed_process(
            spec("printf", &["hello"], dir.path()),
            ManagedRunOptions::new(Some(Duration::from_secs(5))),
        )
        .await
        .unwrap();
        assert_eq!(output.termination, ManagedProcessTermination::Exited);
        assert_eq!(output.exit_code, Some(0));
        assert!(output.success());
        assert_eq!(output.stdout, "hello");
    }

    #[tokio::test]
    async fn test_managed_process_nonzero_exit() {
        let dir = TempDir::new().unwrap();
        let output = run_managed_process(
            spec("sh", &["-c", "exit 7"], dir.path()),
            ManagedRunOptions::new(Some(Duration::from_secs(5))),
        )
        .await
        .unwrap();
        assert_eq!(output.termination, ManagedProcessTermination::Exited);
        assert_eq!(output.exit_code, Some(7));
        assert!(!output.success());
    }

    #[tokio::test]
    async fn test_managed_process_spawn_failure() {
        let dir = TempDir::new().unwrap();
        let error = run_managed_process(
            spec("doge-command-that-does-not-exist", &[], dir.path()),
            ManagedRunOptions::new(Some(Duration::from_secs(1))),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ManagedProcessError::Spawn(_)));
    }

    #[tokio::test]
    async fn test_managed_process_timeout() {
        let dir = TempDir::new().unwrap();
        let output = run_managed_process(
            spec("sleep", &["30"], dir.path()),
            ManagedRunOptions::new(Some(Duration::from_millis(50))),
        )
        .await
        .unwrap();
        assert_eq!(output.termination, ManagedProcessTermination::TimedOut);
        assert_eq!(output.exit_code, None);
        assert!(!output.success());
    }

    #[tokio::test]
    async fn test_managed_process_cancel() {
        let dir = TempDir::new().unwrap();
        let token = CancellationToken::new();
        let child_token = token.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            child_token.cancel();
        });
        let output = run_managed_process(
            spec("sleep", &["30"], dir.path()),
            ManagedRunOptions::new(Some(Duration::from_secs(30))).with_cancellation(Some(token)),
        )
        .await
        .unwrap();
        task.await.unwrap();
        assert_eq!(output.termination, ManagedProcessTermination::Cancelled);
        assert_eq!(output.exit_code, None);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_managed_process_cancel_kills_descendant() {
        let dir = TempDir::new().unwrap();
        let pid_file = dir.path().join("descendant.pid");
        let script = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());
        let token = CancellationToken::new();
        let timer_token = token.clone();
        let timer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            timer_token.cancel();
        });
        let output = run_managed_process(
            spec("bash", &["-c", &script], dir.path()),
            ManagedRunOptions::new(Some(Duration::from_secs(30))).with_cancellation(Some(token)),
        )
        .await
        .unwrap();
        timer.await.unwrap();
        assert_eq!(output.termination, ManagedProcessTermination::Cancelled);
        tokio::time::sleep(Duration::from_millis(200)).await;
        if let Some(pid) = fs::read_to_string(pid_file)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
        {
            assert!(!crate::execution::lifecycle::is_process_alive(pid));
        }
    }

    #[tokio::test]
    async fn test_managed_process_large_output_bounded() {
        let dir = TempDir::new().unwrap();
        let output = run_managed_process(
            spec(
                "sh",
                &["-c", "head -c 200000 /dev/zero | tr '\\0' x"],
                dir.path(),
            ),
            ManagedRunOptions::new(Some(Duration::from_secs(10))),
        )
        .await
        .unwrap();
        assert!(output.capture_truncated);
        assert!(output.stdout.len() <= 70 * 1024);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_managed_process_timeout_kills_descendant() {
        let dir = TempDir::new().unwrap();
        let pid_file = dir.path().join("descendant.pid");
        let script = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());
        let output = run_managed_process(
            spec("bash", &["-c", &script], dir.path()),
            ManagedRunOptions::new(Some(Duration::from_millis(100))),
        )
        .await
        .unwrap();
        assert_eq!(output.termination, ManagedProcessTermination::TimedOut);
        tokio::time::sleep(Duration::from_millis(200)).await;
        if let Some(pid) = fs::read_to_string(pid_file)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
        {
            assert!(!crate::execution::lifecycle::is_process_alive(pid));
        }
    }

    #[tokio::test]
    async fn test_managed_process_large_stderr_bounded() {
        let dir = TempDir::new().unwrap();
        let output = run_managed_process(
            spec(
                "sh",
                &["-c", "head -c 200000 /dev/zero | tr '\\0' x >&2"],
                dir.path(),
            ),
            ManagedRunOptions::new(Some(Duration::from_secs(10))),
        )
        .await
        .unwrap();
        assert!(output.capture_truncated);
        assert!(output.stderr.len() <= 70 * 1024);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_managed_process_normal_exit_cleans_descendant() {
        let dir = TempDir::new().unwrap();
        let pid_file = dir.path().join("descendant.pid");
        let script = format!("sleep 30 & echo $! > '{}'; exit 0", pid_file.display());
        let output = run_managed_process(
            spec("bash", &["-c", &script], dir.path()),
            ManagedRunOptions::new(Some(Duration::from_secs(10))),
        )
        .await
        .unwrap();
        assert_eq!(output.termination, ManagedProcessTermination::Exited);
        assert_eq!(output.exit_code, Some(0));
        tokio::time::sleep(Duration::from_millis(200)).await;
        if let Some(pid) = fs::read_to_string(pid_file)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
        {
            assert!(!crate::execution::lifecycle::is_process_alive(pid));
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_process_group_guard_kills_tree_when_future_dropped() {
        let dir = TempDir::new().unwrap();
        let pid_file = dir.path().join("descendant.pid");
        let script = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());
        let task = tokio::spawn(run_managed_process(
            spec("bash", &["-c", &script], dir.path()),
            ManagedRunOptions::new(None),
        ));
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(pid_file.exists(), "fixture should have started descendant");
        task.abort();
        let _ = task.await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        if let Some(pid) = fs::read_to_string(pid_file)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
        {
            assert!(!crate::execution::lifecycle::is_process_alive(pid));
        }
    }
}
