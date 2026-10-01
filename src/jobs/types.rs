use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;

/// Process-local job identifier. Displayed as `job-<n>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct JobId(pub u64);

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "job-{}", self.0)
    }
}

impl JobId {
    /// Parse `12` or `job-12` (surrounding whitespace tolerated).
    pub fn parse_arg(arg: &str) -> Option<JobId> {
        let trimmed = arg.trim();
        let digits = trimmed.strip_prefix("job-").unwrap_or(trimmed);
        digits.parse::<u64>().ok().map(JobId)
    }
}

/// Initial job classification. Only foreground kinds are used today;
/// background variants are reserved for follow-up integrations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum JobKind {
    AgentTurn,
    Test,
    Lint,
    SemanticEdit,
}

impl fmt::Display for JobKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobKind::AgentTurn => write!(f, "agent_turn"),
            JobKind::Test => write!(f, "test"),
            JobKind::Lint => write!(f, "lint"),
            JobKind::SemanticEdit => write!(f, "semantic_edit"),
        }
    }
}

/// Foreground jobs are mutually exclusive; background jobs may overlap
/// subject to the workspace gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum JobScope {
    Foreground,
    Background,
}

impl fmt::Display for JobScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobScope::Foreground => write!(f, "foreground"),
            JobScope::Background => write!(f, "background"),
        }
    }
}

/// Workspace access classification used by the shared RwLock gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WorkspaceAccess {
    None,
    ReadOnly,
    Write,
}

impl fmt::Display for WorkspaceAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorkspaceAccess::None => write!(f, "none"),
            WorkspaceAccess::ReadOnly => write!(f, "read_only"),
            WorkspaceAccess::Write => write!(f, "write"),
        }
    }
}

/// Lifecycle status. `Completed`/`Failed`/`Cancelled` are terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum JobStatus {
    Starting,
    WaitingForWorkspace,
    Running,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled
        )
    }

    /// Whether `self -> next` is a legal visible transition.
    ///
    /// Terminal states never leave, and `Cancelling` never regresses into
    /// execution/waiting states (`Starting`/`WaitingForWorkspace`/`Running`).
    /// Forward edges (`Starting -> WaitingForWorkspace -> Running`,
    /// anything `-> Cancelling`, anything `-> terminal`) stay legal so valid
    /// startup behavior is preserved.
    pub fn allows_transition(self, next: JobStatus) -> bool {
        if self == next {
            return true;
        }
        if self.is_terminal() {
            return false;
        }
        if self == JobStatus::Cancelling
            && matches!(
                next,
                JobStatus::Starting | JobStatus::WaitingForWorkspace | JobStatus::Running
            )
        {
            return false;
        }
        true
    }
}

impl fmt::Display for JobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobStatus::Starting => write!(f, "starting"),
            JobStatus::WaitingForWorkspace => write!(f, "waiting_for_workspace"),
            JobStatus::Running => write!(f, "running"),
            JobStatus::Cancelling => write!(f, "cancelling"),
            JobStatus::Completed => write!(f, "completed"),
            JobStatus::Failed => write!(f, "failed"),
            JobStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Job body result classification. The body itself decides whether the
/// outcome was a user cancellation, an infrastructure failure, or a
/// successful completion (domain failures such as `cargo test` exit 1
/// are `Completed`, not `Failed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobRunOutcome {
    Completed,
    Cancelled,
    Failed { message: String },
}

/// Post-terminal completion notice delivered after `finish_job` has
/// released foreground ownership and recorded terminal history.
///
/// Protocol-agnostic: carries only job identity, classification, and the
/// body outcome. Payload semantics (prompts, diagnostics) stay with the
/// owner of the deferred work, never in the generic job manager.
#[derive(Debug, Clone)]
pub struct JobCompletion {
    pub id: JobId,
    pub kind: JobKind,
    pub scope: JobScope,
    pub outcome: JobRunOutcome,
}

/// Spawn-time specification (UI label is display-only).
#[derive(Debug, Clone)]
pub struct JobSpec {
    pub kind: JobKind,
    pub scope: JobScope,
    pub workspace_access: WorkspaceAccess,
    pub label: String,
}

impl JobSpec {
    pub fn new(
        kind: JobKind,
        scope: JobScope,
        workspace_access: WorkspaceAccess,
        label: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            scope,
            workspace_access,
            label: bound_label(&label.into()),
        }
    }
}

/// Point-in-time snapshot safe to expose to the UI. Never contains the
/// internal cancellation token or abort handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSnapshot {
    pub id: JobId,
    pub kind: JobKind,
    pub scope: JobScope,
    pub workspace_access: WorkspaceAccess,
    pub status: JobStatus,
    pub label: String,
    pub elapsed_ms: u128,
    pub error: Option<String>,
}

impl JobSnapshot {
    /// One-line `/jobs` rendering:
    /// `job-12  running     agent_turn  00:00:08  Fix login handler`
    pub fn display_line(&self) -> String {
        format!(
            "{}  {:<11}  {:<10}  {}  {}",
            self.id,
            self.status.to_string(),
            self.kind.to_string(),
            format_elapsed(self.elapsed_ms),
            self.label
        )
    }
}

/// Handle handed to the job closure.
#[derive(Debug, Clone)]
pub struct JobContext {
    pub id: JobId,
    pub cancellation: tokio_util::sync::CancellationToken,
}

impl JobContext {
    pub fn cancellation_token(&self) -> tokio_util::sync::CancellationToken {
        self.cancellation.clone()
    }
}

/// Spawn failure.
#[derive(Debug, Clone)]
pub enum JobStartError {
    ForegroundBusy { active: JobSnapshot },
    ShuttingDown,
}

impl fmt::Display for JobStartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobStartError::ForegroundBusy { active } => write!(
                f,
                "Foreground job {} is already running. Use /jobs to inspect it or /cancel to stop it.",
                active.id
            ),
            JobStartError::ShuttingDown => write!(f, "Job manager is shutting down."),
        }
    }
}

impl std::error::Error for JobStartError {}

/// Cancel result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelJobResult {
    Cancelled { id: JobId },
    AlreadyFinished { id: JobId },
    NotFound { id: JobId },
}

/// Maximum label chars stored/displayed.
pub const MAX_LABEL_CHARS: usize = 120;
/// Maximum error chars retained in snapshots/history.
pub const MAX_ERROR_CHARS: usize = 512;
/// Bounded terminal history length.
pub const MAX_RECENT_JOBS: usize = 50;

pub fn bound_label(label: &str) -> String {
    bound_chars(label, MAX_LABEL_CHARS)
}

pub fn bound_error(message: &str) -> String {
    bound_chars(message, MAX_ERROR_CHARS)
}

fn bound_chars(input: &str, max_chars: usize) -> String {
    let truncated: String = input.chars().take(max_chars).collect();
    if input.chars().count() > max_chars {
        format!("{}…", truncated)
    } else {
        truncated
    }
}

pub fn format_elapsed(elapsed_ms: u128) -> String {
    let duration = Duration::from_millis(elapsed_ms.min(u64::MAX as u128) as u64);
    let secs = duration.as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}
