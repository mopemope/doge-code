//! Child process management for provider CLIs (Unix only).
//!
//! Each child leads its own process group so that stopping an agent kills the
//! whole tree (the CLI and anything it spawned), not just the direct child.
//! Stdout is delivered line by line over a bounded channel; the last 4 KiB of
//! stderr is kept for diagnostics. Neither stream's content is ever logged.

#[cfg(not(unix))]
compile_error!("dgc-daemon process management supports Unix (Linux and macOS) only");

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;
use tokio::time::timeout;

/// Longest stdout line that is delivered; longer lines are discarded.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
/// How many trailing stderr bytes are retained.
pub const STDERR_TAIL_BYTES: usize = 4096;
/// Default grace period between SIGTERM and SIGKILL.
pub const DEFAULT_TERMINATE_GRACE: Duration = Duration::from_secs(3);

const LINE_CHANNEL_CAPACITY: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("cannot spawn {}: {source}", program.display())]
    Spawn { program: PathBuf, source: io::Error },
    #[error("child {0} pipe was not available")]
    MissingPipe(&'static str),
    #[error("child process I/O error: {0}")]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, ProcessError>;

#[derive(Debug, Clone)]
pub struct ChildSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

/// One item from the child's stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineEvent {
    /// A line without its trailing `\n` (a trailing `\r` is kept).
    Line(String),
    /// A line longer than [`MAX_LINE_BYTES`] was discarded; `bytes` is its
    /// total length excluding the newline.
    Oversized { bytes: usize },
    /// Stdout reached end of file. Sent once, last.
    Eof,
}

/// The last [`STDERR_TAIL_BYTES`] bytes written to stderr.
#[derive(Debug, Default)]
pub struct TailBuffer {
    bytes: VecDeque<u8>,
}

impl TailBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, data: &[u8]) {
        let data = &data[data.len().saturating_sub(STDERR_TAIL_BYTES)..];
        let overflow = (self.bytes.len() + data.len()).saturating_sub(STDERR_TAIL_BYTES);
        self.bytes.drain(..overflow);
        self.bytes.extend(data);
    }

    /// The retained bytes as lossy UTF-8 (may start mid-character).
    pub fn contents(&self) -> String {
        let (a, b) = self.bytes.as_slices();
        let mut all = Vec::with_capacity(self.bytes.len());
        all.extend_from_slice(a);
        all.extend_from_slice(b);
        String::from_utf8_lossy(&all).into_owned()
    }
}

pub struct ChildIo {
    pub stdin: ChildStdin,
    pub lines: mpsc::Receiver<LineEvent>,
    pub stderr_tail: Arc<Mutex<TailBuffer>>,
}

/// A spawned child that leads its own process group.
#[derive(Debug)]
pub struct ChildProcess {
    child: Child,
    pgid: libc::pid_t,
    /// Whether `Drop` may still signal the group.
    armed: bool,
    status: Option<ExitStatus>,
}

impl ChildProcess {
    /// Spawn `spec` with piped stdio. Must be called inside a tokio runtime
    /// (the stdout/stderr reader tasks are spawned here).
    pub fn spawn(spec: ChildSpec) -> Result<(Self, ChildIo)> {
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(false);
        let mut child = command.spawn().map_err(|source| ProcessError::Spawn {
            program: spec.program.clone(),
            source,
        })?;

        // From here on `process` owns the group, so an early return kills it.
        let pgid = child.id().and_then(|pid| libc::pid_t::try_from(pid).ok());
        let pipes = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let process = Self {
            child,
            pgid: pgid.unwrap_or(0),
            armed: pgid.is_some_and(|pgid| pgid > 0),
            status: None,
        };
        let (Some(stdin), Some(stdout), Some(stderr)) = pipes else {
            return Err(ProcessError::MissingPipe("stdio"));
        };

        let (tx, lines) = mpsc::channel(LINE_CHANNEL_CAPACITY);
        tokio::spawn(read_lines(stdout, tx));
        let stderr_tail = Arc::new(Mutex::new(TailBuffer::new()));
        tokio::spawn(read_stderr(stderr, Arc::clone(&stderr_tail)));
        tracing::debug!(pgid = process.pgid, "spawned child process group");
        Ok((
            process,
            ChildIo {
                stdin,
                lines,
                stderr_tail,
            },
        ))
    }

    /// The child's pid (also its process-group id), if it has not been reaped.
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Wait for the leader to exit. Any group members left behind are killed,
    /// so the group id cannot be recycled while we still hold it.
    pub async fn wait(&mut self) -> Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        let status = self.child.wait().await?;
        self.finish(status);
        Ok(status)
    }

    /// Stop the whole process group: SIGTERM, wait up to `grace` for the
    /// leader, then SIGKILL. Members that outlive the leader are SIGKILLed too.
    /// Returns the leader's exit status; already-exited children just return it.
    pub async fn terminate(&mut self, grace: Duration) -> Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        if let Some(status) = self.child.try_wait()? {
            self.finish(status);
            return Ok(status);
        }
        self.signal_group(libc::SIGTERM);
        let status = match timeout(grace, self.child.wait()).await {
            Ok(status) => status?,
            Err(_) => {
                self.signal_group(libc::SIGKILL);
                self.child.wait().await?
            }
        };
        self.finish(status);
        Ok(status)
    }

    /// The leader has been reaped: kill leftovers and stop owning the group.
    fn finish(&mut self, status: ExitStatus) {
        self.status = Some(status);
        self.signal_group(libc::SIGKILL);
        self.armed = false;
    }

    fn signal_group(&self, signal: libc::c_int) {
        if !self.armed {
            return;
        }
        // SAFETY: killpg only sends a signal; the PGID was captured at spawn
        // and is held (armed) until the leader has been reaped.
        if unsafe { libc::killpg(self.pgid, signal) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                tracing::debug!(pgid = self.pgid, signal, %error, "process-group signal failed");
            }
        }
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        self.signal_group(libc::SIGKILL);
    }
}

async fn read_stderr<R: AsyncRead + Unpin>(mut stderr: R, tail: Arc<Mutex<TailBuffer>>) {
    let mut chunk = [0u8; 4096];
    loop {
        match stderr.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                let mut tail = tail.lock().unwrap_or_else(|e| e.into_inner());
                tail.push(&chunk[..n]);
            }
            Err(error) => {
                tracing::debug!(%error, "child stderr read failed");
                break;
            }
        }
    }
}

/// Split `stdout` into lines, buffering at most [`MAX_LINE_BYTES`] per line.
/// Ends with exactly one `Eof` unless the receiver was dropped.
async fn read_lines<R: AsyncRead + Unpin>(stdout: R, tx: mpsc::Sender<LineEvent>) {
    let mut reader = BufReader::new(stdout);
    let mut line: Vec<u8> = Vec::new();
    // Total length of the current line once it has exceeded the limit.
    let mut discarded: Option<usize> = None;

    loop {
        let available = match reader.fill_buf().await {
            Ok(available) => available,
            Err(error) => {
                tracing::debug!(%error, "child stdout read failed");
                break;
            }
        };
        if available.is_empty() {
            // EOF: flush an unterminated final line.
            if let Some(event) = take_line(&mut line, &mut discarded, true)
                && tx.send(event).await.is_err()
            {
                return;
            }
            break;
        }

        let (chunk, ends_line) = match available.iter().position(|&b| b == b'\n') {
            Some(newline) => (&available[..newline], true),
            None => (available, false),
        };
        let consumed = chunk.len() + usize::from(ends_line);
        match &mut discarded {
            Some(total) => *total += chunk.len(),
            None if line.len() + chunk.len() > MAX_LINE_BYTES => {
                discarded = Some(line.len() + chunk.len());
                line = Vec::new();
            }
            None => line.extend_from_slice(chunk),
        }
        reader.consume(consumed);

        if ends_line
            && let Some(event) = take_line(&mut line, &mut discarded, false)
            && tx.send(event).await.is_err()
        {
            return;
        }
    }
    let _ = tx.send(LineEvent::Eof).await;
}

/// Turn the accumulated line into an event. `at_eof` lines that are empty
/// (no bytes after the last newline) produce nothing.
fn take_line(line: &mut Vec<u8>, discarded: &mut Option<usize>, at_eof: bool) -> Option<LineEvent> {
    if let Some(bytes) = discarded.take() {
        return Some(LineEvent::Oversized { bytes });
    }
    if at_eof && line.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(line).into_owned();
    line.clear();
    Some(LineEvent::Line(text))
}
