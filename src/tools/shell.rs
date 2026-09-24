use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::info;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
pub struct ExecuteShellResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub success: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub output_truncated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timed_out: bool,
}

impl ExecuteShellResult {
    pub fn simple(stdout: String, stderr: String, exit_code: Option<i32>, success: bool) -> Self {
        Self {
            stdout,
            stderr,
            exit_code,
            success,
            output_truncated: false,
            warnings: Vec::new(),
            timed_out: false,
        }
    }
}

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "execute_shell".to_string(),
            description: "Executes a command in a stateful persistent shell session (shell escape hatch). Prefer `execute_process` for normal builds, tests, git, and other single-program commands. Use this only when persistent cwd/env, shell variables, builtins, or shell-specific workflows are needed.".to_string(),
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
    stderr: Option<BufReader<ChildStderr>>,
    /// PGID captured immediately after shell spawn. It remains available even
    /// after the shell leader exits so timeout/cancel can clean descendants.
    process_group: Option<crate::execution::ProcessGroupHandle>,
    project_root: std::path::PathBuf,
    command_timeout_ms: u64,
}

/// Best-effort cleanup for a persistent-shell invocation whose future is
/// dropped before its normal completion path can run.
///
/// `ShellSession` is stored behind a shared mutex, so dropping the invocation
/// future only drops the mutex guard; it does not drop the session itself.
/// Owning this guard while the command is in flight ensures the current shell
/// tree is still terminated in that case.
struct ShellInvocationGuard<'a> {
    session: &'a mut ShellSession,
    started: bool,
}

impl<'a> ShellInvocationGuard<'a> {
    fn new(session: &'a mut ShellSession) -> Self {
        Self {
            session,
            started: false,
        }
    }

    fn disarm(&mut self) {
        self.started = false;
    }

    async fn run(
        &mut self,
        command: &str,
        cancel: Option<CancellationToken>,
    ) -> Result<ExecuteShellResult> {
        self.session
            .exec_command_with_cancel_inner(command, cancel, &mut self.started)
            .await
    }
}

impl Drop for ShellInvocationGuard<'_> {
    fn drop(&mut self) {
        if self.started {
            self.session.abort_in_flight();
        }
    }
}

/// Global or shared state wrapper.
#[derive(Clone, Debug)]
pub struct SharedShellSession(Arc<Mutex<ShellSession>>);

impl SharedShellSession {
    pub fn new(project_root: std::path::PathBuf, command_timeout_ms: u64) -> Self {
        Self(Arc::new(Mutex::new(ShellSession::new(
            project_root,
            command_timeout_ms,
        ))))
    }

    pub async fn exec(&self, command: &str) -> Result<String> {
        self.exec_with_cancel(command, None).await
    }

    pub async fn exec_with_cancel(
        &self,
        command: &str,
        cancel: Option<CancellationToken>,
    ) -> Result<String> {
        if cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
            return Err(anyhow!(crate::llm::LlmErrorKind::Cancelled));
        }
        let mut session = if let Some(token) = cancel.as_ref() {
            tokio::select! {
                biased;
                _ = token.cancelled() => return Err(anyhow!(crate::llm::LlmErrorKind::Cancelled)),
                session = self.0.lock() => session,
            }
        } else {
            self.0.lock().await
        };
        // `is_running` uses try_wait and may observe a dead leader. Reset it
        // before starting a replacement so residual descendants are cleaned.
        if !session.is_running() {
            session.reset_session().await;
            session.start()?;
            if cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
                session.reset_session().await;
                return Err(anyhow!(crate::llm::LlmErrorKind::Cancelled));
            }
        }
        let result = session.exec_command_with_cancel(command, cancel).await?;
        Ok(serde_json::to_string(&result)?)
    }
}

impl ShellSession {
    pub fn new(project_root: std::path::PathBuf, command_timeout_ms: u64) -> Self {
        Self {
            child: None,
            stdin: None,
            stdout: None,
            stderr: None,
            process_group: None,
            project_root,
            command_timeout_ms,
        }
    }

    pub fn is_running(&mut self) -> bool {
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(Some(_)) => false,
                Ok(None) => true,
                Err(_) => false,
            }
        } else {
            false
        }
    }

    pub fn start(&mut self) -> Result<()> {
        info!("Starting new persistent shell session");
        let mut command = Command::new("bash");
        command
            .current_dir(&self.project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        crate::execution::configure_process_group(&mut command);
        let mut child = command.spawn().context("Failed to spawn bash session")?;
        let process_group = crate::execution::ProcessGroupHandle::from_child(&child);
        let stdin = child.stdin.take().context("Failed to open stdin")?;
        let stdout = BufReader::new(child.stdout.take().context("Failed to open stdout")?);
        let stderr = BufReader::new(child.stderr.take().context("Failed to open stderr")?);

        self.child = Some(child);
        self.stdin = Some(stdin);
        self.stdout = Some(stdout);
        self.stderr = Some(stderr);
        self.process_group = Some(process_group);
        Ok(())
    }

    /// Synchronously abort an in-flight invocation when its future is dropped.
    /// Async termination/reaping is handled by the normal completion path; a
    /// `Drop` implementation can only provide the immediate kill safety net.
    fn abort_in_flight(&mut self) {
        self.stdin = None;
        self.stdout = None;
        self.stderr = None;

        // Drop the group guard before the direct child so descendants are
        // signalled even if the child exits between the two drops.
        let process_group = self.process_group.take();
        let child = self.child.take();
        drop(process_group);
        drop(child);
    }

    pub async fn exec_command(&mut self, command: &str) -> Result<ExecuteShellResult> {
        self.exec_command_with_cancel(command, None).await
    }

    pub async fn exec_command_with_cancel(
        &mut self,
        command: &str,
        cancel: Option<CancellationToken>,
    ) -> Result<ExecuteShellResult> {
        let mut invocation = ShellInvocationGuard::new(self);
        let result = invocation.run(command, cancel).await;
        invocation.disarm();
        result
    }

    async fn exec_command_with_cancel_inner(
        &mut self,
        command: &str,
        cancel: Option<CancellationToken>,
        started: &mut bool,
    ) -> Result<ExecuteShellResult> {
        let cancellation = cancel.unwrap_or_default();
        if cancellation.is_cancelled() {
            return Err(anyhow!(crate::llm::LlmErrorKind::Cancelled));
        }

        let sentinel = Uuid::new_v4().to_string();
        // The command is followed by a newline even when the user's command
        // did not print one, so the sentinel is always a complete line.
        let sentinel_command = format!(
            "{}\nexport __DOGE_EC=$?; echo \"__DOGE_SENTINEL:$__DOGE_EC:{}\"; echo \"__DOGE_SENTINEL_ERR:{}\" >&2\n",
            command, sentinel, sentinel
        );

        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("Shell stdin not available"))?;
        *started = true;
        stdin
            .write_all(sentinel_command.as_bytes())
            .await
            .context("Failed to write to shell stdin")?;
        stdin.flush().await.context("Failed to flush shell stdin")?;

        let stdout_prefix = "__DOGE_SENTINEL:";
        let stderr_prefix = "__DOGE_SENTINEL_ERR:";
        let timeout =
            (self.command_timeout_ms != 0).then(|| Duration::from_millis(self.command_timeout_ms));

        // Keep the borrowed readers inside this block. On timeout/cancel the
        // read future is dropped before reset_session() reclaims the shell.
        let read_outcome = {
            let stdout_reader = self
                .stdout
                .as_mut()
                .ok_or_else(|| anyhow!("Shell stdout not available"))?;
            let stderr_reader = self
                .stderr
                .as_mut()
                .ok_or_else(|| anyhow!("Shell stderr not available"))?;
            let read_task = async {
                tokio::join!(
                    read_until_sentinel(stdout_reader, stdout_prefix, &sentinel),
                    read_until_sentinel(stderr_reader, stderr_prefix, &sentinel)
                )
            };
            tokio::pin!(read_task);

            if let Some(duration) = timeout {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => ReadOutcome::Cancelled,
                    result = tokio::time::timeout(duration, &mut read_task) => {
                        match result {
                            Ok(result) => ReadOutcome::Completed(result),
                            Err(_) => ReadOutcome::TimedOut,
                        }
                    }
                }
            } else {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => ReadOutcome::Cancelled,
                    result = &mut read_task => ReadOutcome::Completed(result),
                }
            }
        };

        match read_outcome {
            ReadOutcome::Cancelled => {
                self.reset_session().await;
                Err(anyhow!(crate::llm::LlmErrorKind::Cancelled))
            }
            ReadOutcome::TimedOut => {
                self.reset_session().await;
                Ok(ExecuteShellResult {
                    stdout: String::new(),
                    stderr: format!("Command timed out after {} ms", self.command_timeout_ms),
                    exit_code: None,
                    success: false,
                    output_truncated: true,
                    warnings: vec!["shell session terminated after timeout".to_string()],
                    timed_out: true,
                })
            }
            ReadOutcome::Completed((stdout_capture, stderr_capture)) => {
                let (stdout_raw, stdout_truncated) = stdout_capture.capture.finish();
                let (stderr_raw, stderr_truncated) = stderr_capture.capture.finish();
                let (stdout, stderr, budgeted, mut warnings) =
                    crate::execution::output::budget_command_output(&stdout_raw, &stderr_raw);
                if stdout_truncated || stderr_truncated {
                    warnings.push(
                        "shell output exceeded capture limits; head and tail preserved".to_string(),
                    );
                }
                warnings.extend(stdout_capture.warnings);
                warnings.extend(stderr_capture.warnings);
                let success = stdout_capture.exit_code == Some(0);
                Ok(ExecuteShellResult {
                    stdout,
                    stderr,
                    exit_code: stdout_capture.exit_code,
                    success,
                    output_truncated: budgeted || stdout_truncated || stderr_truncated,
                    warnings,
                    timed_out: false,
                })
            }
        }
    }

    async fn reset_session(&mut self) {
        // Drop pipe ends before waiting so a killed descendant cannot keep the
        // reader tasks alive through inherited descriptors.
        self.stdin = None;
        self.stdout = None;
        self.stderr = None;
        let mut child = self.child.take();
        let mut process_group = self.process_group.take();
        if let Some(mut child) = child.take() {
            if let Some(mut group) = process_group.take() {
                if let Err(error) =
                    crate::execution::terminate_process_tree_with_group(&mut child, &mut group)
                        .await
                {
                    tracing::warn!(error = %error, "failed to clean persistent shell tree");
                }
            } else {
                crate::execution::terminate_process_tree(&mut child).await;
            }
        } else if let Some(mut group) = process_group.take() {
            // The direct child was already gone, but a residual descendant may
            // still own the PGID. Use the same explicit group cleanup path.
            if let Err(error) = crate::execution::cleanup_process_group_after_exit(&mut group).await
            {
                tracing::warn!(error = %error, "failed to clean residual shell group");
            }
        }
    }
}

enum ReadOutcome {
    Completed((SentinelRead, SentinelRead)),
    TimedOut,
    Cancelled,
}

struct SentinelRead {
    capture: crate::execution::BoundedCapture,
    exit_code: Option<i32>,
    warnings: Vec<String>,
}

/// Read one shell response without allowing a command to grow an unbounded
/// line buffer. The scanner keeps only a small rolling line window; the actual
/// output is fed into the same head/tail bounded capture used by finite tools.
async fn read_until_sentinel<R>(reader: &mut R, prefix: &str, sentinel_id: &str) -> SentinelRead
where
    R: AsyncRead + Unpin,
{
    const SCANNER_BUFFER_LIMIT: usize = 8 * 1024;
    const SCANNER_TAIL_MARGIN: usize = 256;

    let mut capture = crate::execution::BoundedCapture::new();
    let mut warnings = Vec::new();
    let mut line_buffer = Vec::new();
    let mut chunk = vec![0_u8; 8192];

    loop {
        let count = match reader.read(&mut chunk).await {
            Ok(count) => count,
            Err(error) => {
                warnings.push(format!("shell output reader failed: {error}"));
                break;
            }
        };
        if count == 0 {
            break;
        }
        let mut start = 0;
        for (index, byte) in chunk[..count].iter().enumerate() {
            if *byte != b'\n' {
                continue;
            }
            line_buffer.extend_from_slice(&chunk[start..=index]);
            let line = std::mem::take(&mut line_buffer);
            if let Some((marker_offset, exit_code)) = find_sentinel_line(&line, prefix, sentinel_id)
            {
                capture.push(&line[..marker_offset]);
                return SentinelRead {
                    capture,
                    exit_code,
                    warnings,
                };
            }
            capture.push(&line);
            capture.push(b"\n");
            start = index + 1;
        }
        if start < count {
            line_buffer.extend_from_slice(&chunk[start..count]);
        }

        // A pathological command with no newline must not make the scanner
        // itself unbounded. Retain enough tail for a marker split across reads.
        if line_buffer.len() > SCANNER_BUFFER_LIMIT {
            let keep =
                (prefix.len() + sentinel_id.len() + SCANNER_TAIL_MARGIN).min(SCANNER_BUFFER_LIMIT);
            let flush = line_buffer.len() - keep;
            capture.push(&line_buffer[..flush]);
            line_buffer.drain(..flush);
        }
    }

    if !line_buffer.is_empty() {
        let line = std::mem::take(&mut line_buffer);
        if let Some((marker_offset, exit_code)) = find_sentinel_line(&line, prefix, sentinel_id) {
            capture.push(&line[..marker_offset]);
            return SentinelRead {
                capture,
                exit_code,
                warnings,
            };
        }
        capture.push(&line);
    }
    SentinelRead {
        capture,
        exit_code: None,
        warnings,
    }
}

/// Find a sentinel in one complete line. `Some(None)` means a valid stderr
/// sentinel (which carries no exit code); `None` means ordinary output. The
/// byte offset lets the caller retain output that shared a line with the
/// sentinel (for example, `printf`-style commands without a trailing newline).
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn find_sentinel_line(
    line: &[u8],
    prefix: &str,
    sentinel_id: &str,
) -> Option<(usize, Option<i32>)> {
    // Keep the offset in the original byte buffer. `String::from_utf8_lossy`
    // expands invalid bytes, so an offset into that string cannot safely be
    // used to slice `line`.
    let mut line_end = line.len();
    while line_end > 0 && matches!(line[line_end - 1], b'\r' | b'\n') {
        line_end -= 1;
    }
    let line = &line[..line_end];
    let prefix = prefix.as_bytes();
    let sentinel_id = sentinel_id.as_bytes();
    let position = find_bytes(line, prefix)?;
    let suffix = &line[position + prefix.len()..];
    if prefix.ends_with(b"_ERR:") {
        return (suffix == sentinel_id).then_some((position, None));
    }
    let separator = suffix.iter().position(|byte| *byte == b':')?;
    let code = std::str::from_utf8(&suffix[..separator]).ok()?.parse().ok();
    if &suffix[separator + 1..] != sentinel_id {
        return None;
    }
    Some((position, code))
}

#[cfg(test)]
fn parse_sentinel_line(line: &[u8], prefix: &str, sentinel_id: &str) -> Option<Option<i32>> {
    find_sentinel_line(line, prefix, sentinel_id).map(|(_, exit_code)| exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_shell_session_basic() {
        let temp_dir = TempDir::new().unwrap();
        let mut session = ShellSession::new(temp_dir.path().to_path_buf(), 0);
        session.start().unwrap();

        let result = session.exec_command("echo hello").await.unwrap();
        assert_eq!(result.stdout.trim(), "hello");
        assert!(result.success);
        session.reset_session().await;
    }

    #[tokio::test]
    async fn test_shell_session_state() {
        let temp_dir = TempDir::new().unwrap();
        let mut session = ShellSession::new(temp_dir.path().to_path_buf(), 0);
        session.start().unwrap();

        session.exec_command("export FOO=bar").await.unwrap();
        let result = session.exec_command("echo $FOO").await.unwrap();
        assert_eq!(result.stdout.trim(), "bar");
        session.reset_session().await;
    }

    #[tokio::test]
    async fn test_shell_session_cwd() {
        let temp_dir = TempDir::new().unwrap();
        let mut session = ShellSession::new(temp_dir.path().to_path_buf(), 0);
        session.start().unwrap();

        let subdir = temp_dir.path().join("subdir");
        tokio::fs::create_dir(&subdir).await.unwrap();
        session.exec_command("cd subdir").await.unwrap();
        let result = session.exec_command("pwd").await.unwrap();
        assert!(result.stdout.trim().ends_with("subdir"));
        session.reset_session().await;
    }

    #[tokio::test]
    async fn test_shell_session_error() {
        let temp_dir = TempDir::new().unwrap();
        let mut session = ShellSession::new(temp_dir.path().to_path_buf(), 0);
        session.start().unwrap();

        let result = session.exec_command("nonexistent_command").await.unwrap();
        assert!(!result.success);
        assert!(result.exit_code.is_some());
        assert_ne!(result.exit_code.unwrap(), 0);
        assert!(!result.stderr.is_empty());
        let result = session.exec_command("echo alive").await.unwrap();
        assert_eq!(result.stdout.trim(), "alive");
        session.reset_session().await;
    }

    #[tokio::test]
    async fn test_shell_restarts_after_shell_exit() {
        let temp_dir = TempDir::new().unwrap();
        let mut session = ShellSession::new(temp_dir.path().to_path_buf(), 0);
        session.start().unwrap();
        let result = session.exec_command("exit 0").await.unwrap();
        assert!(!result.success || result.exit_code.is_some());
        session.start().unwrap();
        let result = session.exec_command("echo recovered").await.unwrap();
        assert_eq!(result.stdout.trim(), "recovered");
        session.reset_session().await;
    }

    #[tokio::test]
    async fn test_shell_timeout_resets_session() {
        let temp_dir = TempDir::new().unwrap();
        let mut session = ShellSession::new(temp_dir.path().to_path_buf(), 50);
        session.start().unwrap();
        let result = session.exec_command("sleep 30").await.unwrap();
        assert!(!result.success);
        assert!(result.timed_out);
        assert!(!session.is_running());
        session.start().unwrap();
        let result = session.exec_command("echo recovered").await.unwrap();
        assert_eq!(result.stdout.trim(), "recovered");
        session.reset_session().await;
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_timeout_kills_descendant() {
        use crate::execution::lifecycle::is_process_alive;
        let temp_dir = TempDir::new().unwrap();
        let pid_file = temp_dir.path().join("shell-child.pid");
        let mut session = ShellSession::new(temp_dir.path().to_path_buf(), 75);
        session.start().unwrap();
        let result = session
            .exec_command(&format!(
                "sleep 30 & echo $! > '{}'; wait",
                pid_file.display()
            ))
            .await
            .unwrap();
        assert!(result.timed_out);
        tokio::time::sleep(Duration::from_millis(200)).await;
        if let Some(pid) = std::fs::read_to_string(pid_file)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
        {
            assert!(!is_process_alive(pid));
        }
        assert!(!session.is_running());
    }

    #[tokio::test]
    async fn test_shell_cancel_resets_session() {
        let temp_dir = TempDir::new().unwrap();
        let mut session = ShellSession::new(temp_dir.path().to_path_buf(), 30_000);
        session.start().unwrap();
        let token = CancellationToken::new();
        let timer_token = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            timer_token.cancel();
        });
        let error = session
            .exec_command_with_cancel("sleep 30", Some(token))
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<crate::llm::LlmErrorKind>().is_some());
        session.start().unwrap();
        let result = session.exec_command("echo recovered").await.unwrap();
        assert_eq!(result.stdout.trim(), "recovered");
        session.reset_session().await;
    }

    #[tokio::test]
    async fn test_shell_large_output_bounded() {
        let temp_dir = TempDir::new().unwrap();
        let mut session = ShellSession::new(temp_dir.path().to_path_buf(), 10_000);
        session.start().unwrap();
        let result = session
            .exec_command("head -c 200000 /dev/zero | tr '\\0' x")
            .await
            .unwrap();
        assert!(result.output_truncated);
        assert!(result.stdout.len() < 100_000);
        session.reset_session().await;
    }

    #[tokio::test]
    async fn test_sentinel_reader_handles_chunk_boundary_and_prefix_output() {
        let (mut writer, mut reader) = tokio::io::duplex(128);
        writer.write_all(b"partial__DOGE_SEN").await.unwrap();
        writer.write_all(b"TINEL:0:abc\n").await.unwrap();
        drop(writer);
        let result = read_until_sentinel(&mut reader, "__DOGE_SENTINEL:", "abc").await;
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.capture.finish().0, "partial");
    }

    #[test]
    fn test_sentinel_parser_handles_complete_line() {
        assert_eq!(
            parse_sentinel_line(b"__DOGE_SENTINEL:7:abc", "__DOGE_SENTINEL:", "abc"),
            Some(Some(7))
        );
        assert_eq!(
            parse_sentinel_line(b"__DOGE_SENTINEL_ERR:abc", "__DOGE_SENTINEL_ERR:", "abc"),
            Some(None)
        );
    }

    #[tokio::test]
    async fn test_sentinel_reader_handles_invalid_utf8_before_marker() {
        let mut line = vec![0xff; 1_000];
        line.extend_from_slice(b"__DOGE_SENTINEL:0:abc\n");
        let (mut writer, mut reader) = tokio::io::duplex(2_048);
        writer.write_all(&line).await.unwrap();
        drop(writer);

        let result = read_until_sentinel(&mut reader, "__DOGE_SENTINEL:", "abc").await;
        assert_eq!(result.exit_code, Some(0));
        let (output, _) = result.capture.finish();
        assert_eq!(output.chars().count(), 1_000);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_cancellation_interrupts_mutex_wait() {
        let temp_dir = TempDir::new().unwrap();
        let session = SharedShellSession::new(temp_dir.path().to_path_buf(), 0);
        let first_session = session.clone();
        let first = tokio::spawn(async move { first_session.exec("sleep 30").await });
        tokio::time::sleep(Duration::from_millis(100)).await;

        let token = CancellationToken::new();
        token.cancel();
        let result = tokio::time::timeout(
            Duration::from_millis(500),
            session.exec_with_cancel("echo queued", Some(token)),
        )
        .await
        .expect("cancellation should interrupt the mutex wait")
        .expect_err("cancelled shell call should fail");
        assert!(result.downcast_ref::<crate::llm::LlmErrorKind>().is_some());

        first.abort();
        let _ = first.await;
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_future_drop_aborts_in_flight_command() {
        let temp_dir = TempDir::new().unwrap();
        let session = SharedShellSession::new(temp_dir.path().to_path_buf(), 0);
        let command = "echo $$ > shell.pid; sleep 30".to_string();
        let task_session = session.clone();
        let task = tokio::spawn(async move { task_session.exec(&command).await });

        for _ in 0..40 {
            if temp_dir.path().join("shell.pid").exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(temp_dir.path().join("shell.pid").exists());

        task.abort();
        let _ = task.await;

        let recovered =
            tokio::time::timeout(Duration::from_secs(2), session.exec("echo recovered"))
                .await
                .expect("shell session should be reusable after future drop")
                .unwrap();
        assert!(recovered.contains("recovered"));
    }
}
