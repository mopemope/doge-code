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

/// Synchronous structured-stream completion. Cancellation remains an async caller concern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamStop {
    Eof,
    Limit,
}

pub(crate) struct ManagedStreamOutput<T> {
    pub value: T,
    pub stop: StreamStop,
    pub status: std::process::ExitStatus,
    pub stderr: String,
    pub kill_requested: bool,
}

struct StreamChild {
    child: std::process::Child,
    group: Option<ProcessGroupHandle>,
    stderr: Option<std::thread::JoinHandle<io::Result<String>>>,
}
impl StreamChild {
    fn finish(
        &mut self,
        stop: StreamStop,
    ) -> anyhow::Result<(std::process::ExitStatus, String, bool)> {
        use anyhow::Context;
        let polled = (stop == StreamStop::Limit).then(|| self.child.try_wait());
        let kill_requested = matches!(polled, Some(Ok(None)));
        if stop == StreamStop::Limit {
            // Group drop terminates descendants before the direct-child reap.
            drop(self.group.take());
            let _ = self.child.kill();
        }
        let status = self.child.wait();
        drop(self.group.take());
        let diagnostic = self
            .stderr
            .take()
            .map(|task| {
                task.join()
                    .map_err(|_| anyhow::anyhow!("stderr capture thread panicked"))?
                    .context("failed to read process stderr")
            })
            .transpose();
        if let Some(polled) = polled {
            let _ = polled.context("failed to poll streaming process")?;
        }
        Ok((
            status.context("failed to wait for streaming process")?,
            diagnostic?.unwrap_or_default(),
            kill_requested,
        ))
    }
}
impl Drop for StreamChild {
    fn drop(&mut self) {
        let _ = self.finish(StreamStop::Limit);
    }
}

/// Managed synchronous record streaming: keeps parser framing intact instead
/// of applying the async runner's head/tail text capture to structured JSON.
/// The consumer must bound its own stdout records. All exits terminate/reap
/// the owned process tree and concurrently drain bounded stderr diagnostics.
/// This adapter deliberately does not introduce async cancellation semantics.
pub(crate) fn run_managed_stream<T>(
    mut command: std::process::Command,
    consume: impl FnOnce(&mut dyn std::io::Read) -> anyhow::Result<(T, StreamStop)>,
) -> anyhow::Result<ManagedStreamOutput<T>> {
    use anyhow::Context;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command
        .spawn()
        .context("failed to spawn streaming process")?;
    let group = ProcessGroupHandle::from_std_child(&child);
    let mut owned = StreamChild {
        child,
        group: Some(group),
        stderr: None,
    };
    let mut stdout = owned
        .child
        .stdout
        .take()
        .context("failed to capture process stdout")?;
    let mut stderr = owned
        .child
        .stderr
        .take()
        .context("failed to capture process stderr")?;
    owned.stderr = Some(
        std::thread::Builder::new()
            .name("process-stderr".into())
            .spawn(move || {
                use std::io::Read;
                let mut prefix = Vec::new();
                let mut bytes = [0u8; 4096];
                loop {
                    let count = match stderr.read(&mut bytes) {
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        value => value?,
                    };
                    if count == 0 {
                        break;
                    }
                    let retain = count.min(4096usize.saturating_sub(prefix.len()));
                    prefix.extend_from_slice(&bytes[..retain]);
                }
                Ok(String::from_utf8_lossy(&prefix).into_owned())
            })
            .context("failed to start stderr capture")?,
    );
    let parsed = consume(&mut stdout);
    drop(stdout);
    let stop = parsed.as_ref().map_or(StreamStop::Limit, |(_, stop)| *stop);
    let finished = owned.finish(stop);
    let (value, stop) = parsed?;
    let (status, stderr, kill_requested) = finished?;
    Ok(ManagedStreamOutput {
        value,
        stop,
        status,
        stderr,
        kill_requested,
    })
}

/// Structured-stream async owner. Normal cancellation joins the parser and
/// reaps the process; future drop signals a detached cleanup owner.
pub(crate) async fn run_managed_stream_async<T, F>(
    command: std::process::Command,
    parent: CancellationToken,
    consume: F,
) -> anyhow::Result<Option<ManagedStreamOutput<T>>>
where
    T: Send + 'static,
    F: FnOnce(&mut dyn io::Read) -> anyhow::Result<(T, StreamStop)> + Send + 'static,
{
    struct StopOnDrop(CancellationToken);
    impl Drop for StopOnDrop {
        fn drop(&mut self) {
            self.0.cancel();
        }
    }
    let guard = StopOnDrop(parent.child_token());
    let token = guard.0.clone();
    // Ownership remains in this task if a caller drops its join future.
    let owner: JoinHandle<anyhow::Result<Option<ManagedStreamOutput<T>>>> = tokio::spawn(
        async move {
            use anyhow::Context;
            use std::sync::{Arc, OnceLock};
            static STREAMS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
            let permit = tokio::select! {
                biased;
                _ = token.cancelled() => return Ok(None),
                permit = STREAMS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(4))).clone().acquire_owned() => permit?,
            };
            if token.is_cancelled() {
                return Ok(None);
            }
            let mut command = Command::from(command);
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            configure_process_group(&mut command);
            let mut child = command
                .spawn()
                .context("failed to spawn streaming process")?;
            let mut group = ProcessGroupHandle::from_child(&child);
            let stdout = child
                .stdout
                .take()
                .context("failed to capture process stdout")?;
            let mut stderr = child
                .stderr
                .take()
                .context("failed to capture process stderr")?;
            let mut diagnostic = tokio::spawn(async move {
                let mut prefix = Vec::new();
                let mut bytes = [0u8; 4096];
                loop {
                    let count = stderr.read(&mut bytes).await?;
                    if count == 0 {
                        break;
                    }
                    let retain = count.min(4096usize.saturating_sub(prefix.len()));
                    prefix.extend_from_slice(&bytes[..retain]);
                }
                Ok::<_, io::Error>(String::from_utf8_lossy(&prefix).into_owned())
            });
            struct Pipe {
                stdout: tokio::process::ChildStdout,
                handle: tokio::runtime::Handle,
                token: CancellationToken,
            }
            impl io::Read for Pipe {
                fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                    self.handle.block_on(async {
                        tokio::select! {
                            biased;
                            _ = self.token.cancelled() => Err(io::Error::other("stream cancelled")),
                            result = self.stdout.read(bytes) => result,
                        }
                    })
                }
            }
            let mut pipe = Pipe {
                stdout,
                handle: tokio::runtime::Handle::current(),
                token: token.clone(),
            };
            let _permit = permit; // Keep the slot until cleanup and joins finish.
            let mut parser = tokio::task::spawn_blocking(move || consume(&mut pipe));
            let mut canceled = false;
            let parsed = tokio::select! {
                biased;
                _ = token.cancelled() => { canceled = true; None },
                parsed = &mut parser => Some(parsed),
            };
            let stop = parsed
                .as_ref()
                .and_then(|value| value.as_ref().ok())
                .and_then(|value| value.as_ref().ok())
                .map_or(StreamStop::Limit, |(_, stop)| *stop);
            let kill_requested = stop == StreamStop::Limit && matches!(child.try_wait(), Ok(None));
            let status: anyhow::Result<_> = if canceled || stop == StreamStop::Limit {
                // Use the same immediate SIGKILL intentional-stop contract as the
                // synchronous stream adapter; cleanup still explicitly reaps.
                drop(group);
                let kill = child.start_kill();
                let wait = child.wait().await;
                wait.context("failed to reap streaming process")
                    .and_then(|status| {
                        // Already-exited processes can reject start_kill harmlessly.
                        if kill_requested && status.success() {
                            kill.context("failed to stop streaming process")?;
                        }
                        Ok(status)
                    })
            } else {
                let waited = tokio::select! {
                    biased;
                    _ = token.cancelled() => { canceled = true; None },
                    status = child.wait() => Some(status),
                };
                if let Some(waited) = waited {
                    let cleanup = cleanup_process_group_after_exit(&mut group).await;
                    waited
                        .context("failed to wait for streaming process")
                        .and_then(|status| {
                            cleanup?;
                            Ok(status)
                        })
                } else {
                    let cleanup = terminate_process_tree_with_group(&mut child, &mut group).await;
                    child
                        .wait()
                        .await
                        .context("failed to reap streaming process")
                        .and_then(|status| {
                            cleanup?;
                            Ok(status)
                        })
                }
            };
            // Join on cancellation/errors too; never discard cleanup-owned work.
            let parsed = match parsed {
                Some(parsed) => parsed,
                None => parser.await,
            };
            let diagnostic_result =
                match tokio::time::timeout(Duration::from_secs(1), &mut diagnostic).await {
                    Ok(result) => result
                        .context("stderr task failed")?
                        .context("failed to read process stderr"),
                    Err(_) => {
                        diagnostic.abort();
                        let _ = diagnostic.await;
                        Err(anyhow::anyhow!(
                            "stderr did not close after process cleanup"
                        ))
                    }
                };
            status
                .as_ref()
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            if canceled || token.is_cancelled() {
                return Ok(None);
            }
            let (value, stop) = parsed.context("stream parser task failed")??;
            Ok(Some(ManagedStreamOutput {
                value,
                stop,
                status: status?,
                stderr: diagnostic_result?,
                kill_requested,
            }))
        },
    );
    let result = owner.await??;
    if guard.0.is_cancelled() {
        Ok(None)
    } else {
        Ok(result)
    }
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

#[cfg(all(test, unix))]
mod stream_cancellation_tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(flavor = "current_thread")]
    async fn cancellable_stream_slot_is_held_until_child_is_reaped() {
        let dir = tempfile::tempdir().unwrap();
        let mut runs = Vec::new();
        let mut tokens = Vec::new();
        for index in 0..5 {
            let token = CancellationToken::new();
            let ready = dir.path().join(format!("ready-{index}"));
            let mut cmd = std::process::Command::new("/bin/sh");
            // Close stdout before sleeping: the parser finishes while the
            // live child still owns its concurrency slot.
            cmd.arg("-c").arg(format!(
                "exec 1>&-; printf '%s' $$ > '{}'; sleep 5",
                ready.display()
            ));
            runs.push(tokio::spawn(run_managed_stream_async(
                cmd,
                token.clone(),
                |stdout| {
                    std::io::copy(stdout, &mut std::io::sink())?;
                    Ok(((), StreamStop::Eof))
                },
            )));
            tokens.push(token);
            if index < 4 {
                tokio::time::timeout(Duration::from_secs(3), async {
                    while !ready.exists() {
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                })
                .await
                .unwrap();
            }
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(
            !dir.path().join("ready-4").exists(),
            "fifth child bypassed live-process bound"
        );
        let first_pid: u32 = std::fs::read_to_string(dir.path().join("ready-0"))
            .unwrap()
            .parse()
            .unwrap();
        tokens[0].cancel();
        assert!(runs.remove(0).await.unwrap().unwrap().is_none());
        assert!(!crate::execution::is_process_alive(first_pid));
        tokio::time::timeout(Duration::from_secs(2), async {
            while !dir.path().join("ready-4").exists() {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        for token in tokens {
            token.cancel();
        }
        for run in runs {
            assert!(run.await.unwrap().unwrap().is_none());
        }
    }
}
