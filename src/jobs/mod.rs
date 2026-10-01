pub mod manager;
pub mod types;

pub use manager::{JobCompletionHook, JobManager};
pub use types::{
    CancelJobResult, JobCompletion, JobContext, JobId, JobKind, JobRunOutcome, JobScope,
    JobSnapshot, JobSpec, JobStartError, JobStatus, MAX_ERROR_CHARS, MAX_LABEL_CHARS,
    MAX_RECENT_JOBS, WorkspaceAccess, bound_error,
};

/// Grace period for TUI shutdown: cancel, close the tracker, and wait.
pub const JOB_SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

#[cfg(test)]
mod tests;
