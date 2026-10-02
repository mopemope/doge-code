//! Git worktree creation service.
//!
//! This module owns reliable, cancellable worktree creation through the
//! shared managed process runner. All finite Git invocations use structured
//! `program + argv` requests with an explicit repository `cwd`; the
//! process-global current directory is never read or mutated, and no shell
//! is involved.
//!
//! Creation contract (v1):
//!
//! ```text
//! git worktree add -b <generated-branch> <generated-path> HEAD
//! ```
//!
//! The branch and the checkout are created in a single Git operation. A
//! successful Git exit alone is not enough: the resulting registration is
//! verified through `git worktree list --porcelain -z` before success is
//! reported. Failure cleanup only touches resources this invocation
//! generated (derived from its unique id).
//!
//! Creating a worktree never switches the current Doge project/session
//! context; that remains an explicit, separate concern.

use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::execution::{
    ManagedProcessError, ManagedProcessOutput, ManagedProcessSpec, ManagedProcessTermination,
    ManagedRunOptions, run_managed_process,
};

/// Chars of Git stderr retained in typed errors surfaced toward the UI.
const GIT_STDERR_BUDGET_CHARS: usize = 2_000;

/// Fallback per-command timeout for failure cleanup when the normal command
/// timeout is unlimited (`command_timeout_ms == 0`).
const CLEANUP_DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
/// Upper bound for any single cleanup Git invocation, even when the normal
/// command timeout is larger or unlimited.
const CLEANUP_MAX_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// Overall budget for the whole cleanup sequence so cancellation stays
/// responsive even if several cleanup steps stall.
const CLEANUP_OVERALL_BUDGET: Duration = Duration::from_secs(30);

/// Namespace for Doge-generated worktree branches (`doge/<uuid>`).
const GENERATED_BRANCH_PREFIX: &str = "doge/";

/// Request to create one isolated linked worktree.
#[derive(Debug, Clone)]
pub struct CreateWorktreeRequest {
    /// Starting point for repository discovery. The effective repository is
    /// resolved via `git rev-parse --show-toplevel` with this directory as
    /// the process cwd, so subdirectories of a repository work as input.
    pub project_root: PathBuf,
    /// Base ref for the new worktree. v1 fixes this to `HEAD`.
    pub base_ref: String,
    /// Per-command timeout in milliseconds. `0` means unlimited.
    pub command_timeout_ms: u64,
}

impl CreateWorktreeRequest {
    pub fn new(project_root: PathBuf, command_timeout_ms: u64) -> Self {
        Self {
            project_root,
            base_ref: "HEAD".to_string(),
            command_timeout_ms,
        }
    }
}

/// Successfully created and verified worktree.
#[derive(Debug, Clone)]
pub struct CreatedWorktree {
    /// Canonicalized repository root the worktree belongs to.
    pub repository_root: PathBuf,
    /// Canonicalized path of the new worktree checkout.
    pub worktree_path: PathBuf,
    /// Generated branch checked out in the new worktree (`doge/<id>`).
    pub branch: String,
    /// Base ref the worktree was created from (`HEAD` in v1).
    pub base_ref: String,
    /// Commit OID resolved for the base ref at creation time.
    pub head_oid: String,
}

/// Typed worktree failures. Control flow matches on variants, never on
/// error-string substrings.
#[derive(Error, Debug)]
pub enum WorktreeError {
    #[error("The directory is not inside a Git repository: {0}")]
    NotGitRepository(String),
    #[error("Git '{operation}' failed (exit code: {exit_code:?}): {stderr}")]
    GitCommandFailed {
        operation: &'static str,
        exit_code: Option<i32>,
        stderr: String,
    },
    #[error("Git '{operation}' could not be run: {message}")]
    ProcessError {
        operation: &'static str,
        message: String,
    },
    #[error("Git '{operation}' timed out")]
    TimedOut { operation: &'static str },
    #[error("Worktree creation was cancelled")]
    Cancelled,
    #[error("Invalid repository root: {0}")]
    InvalidRepositoryRoot(String),
    #[error("Invalid Git output from '{operation}': {reason}")]
    InvalidGitOutput {
        operation: &'static str,
        reason: String,
    },
    #[error("Worktree verification failed: {0}")]
    VerificationFailed(String),
    #[error("Worktree creation failed: {original}; cleanup reported: {cleanup}")]
    CleanupFailed { original: String, cleanup: String },
    #[error("Worktree creation was cancelled; cleanup reported: {cleanup}")]
    CancelledWithCleanup { cleanup: String },
    #[error("Git '{operation}' timed out; cleanup reported: {cleanup}")]
    TimedOutWithCleanup {
        operation: &'static str,
        cleanup: String,
    },
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl WorktreeError {
    /// Primary lifecycle classification: cancellation survives cleanup
    /// attachment. Covers both plain `Cancelled` and `CancelledWithCleanup`.
    pub fn is_cancelled(&self) -> bool {
        matches!(
            self,
            WorktreeError::Cancelled | WorktreeError::CancelledWithCleanup { .. }
        )
    }

    /// Primary lifecycle classification: timeout survives cleanup attachment.
    pub fn is_timed_out(&self) -> bool {
        matches!(
            self,
            WorktreeError::TimedOut { .. } | WorktreeError::TimedOutWithCleanup { .. }
        )
    }

    /// Cleanup diagnostics carried alongside the primary outcome, if any.
    /// Used by the TUI to surface leftovers without reclassifying the
    /// primary lifecycle outcome.
    pub fn cleanup_diagnostic(&self) -> Option<&str> {
        match self {
            WorktreeError::CleanupFailed { cleanup, .. }
            | WorktreeError::CancelledWithCleanup { cleanup }
            | WorktreeError::TimedOutWithCleanup { cleanup, .. } => Some(cleanup),
            _ => None,
        }
    }
}

/// Primary lifecycle classification for a worktree error, independent of any
/// attached cleanup diagnostics. Control flow matches on this, never on
/// error-string substrings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreePrimary {
    Cancelled,
    TimedOut,
    Failed,
}

/// Classify the primary outcome of a worktree error.
pub fn classify_primary(error: &WorktreeError) -> WorktreePrimary {
    if error.is_cancelled() {
        WorktreePrimary::Cancelled
    } else if error.is_timed_out() {
        WorktreePrimary::TimedOut
    } else {
        WorktreePrimary::Failed
    }
}

/// One `git worktree list --porcelain -z` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorktreeRecord {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub locked: Option<String>,
    pub prunable: Option<String>,
}

/// Resolve the per-command timeout. `0` preserves the historical
/// unlimited semantics; no hidden fixed timeout is introduced.
pub fn resolve_timeout(command_timeout_ms: u64) -> Option<Duration> {
    if command_timeout_ms == 0 {
        None
    } else {
        Some(Duration::from_millis(command_timeout_ms))
    }
}

/// Resolve the finite per-command timeout used for failure cleanup. Always
/// returns a finite budget, even when the normal command timeout is
/// unlimited (`command_timeout_ms == 0`). A configured timeout larger than
/// the cleanup cap is clamped down so cleanup stays responsive.
pub fn resolve_cleanup_timeout(command_timeout_ms: u64) -> Duration {
    bound_cleanup_timeout(resolve_timeout(command_timeout_ms))
}

/// Clamp an already-resolved normal timeout to the finite cleanup budget.
/// `None` (unlimited normal execution) maps to the cleanup default; any
/// configured timeout above the cap is clamped to the cap. A zero duration
/// can never usefully bound a subprocess (the managed runner would time out
/// immediately), so it also maps to the cleanup default.
pub fn bound_cleanup_timeout(timeout: Option<Duration>) -> Duration {
    match timeout {
        None => CLEANUP_DEFAULT_COMMAND_TIMEOUT,
        Some(duration) if duration.is_zero() => CLEANUP_DEFAULT_COMMAND_TIMEOUT,
        Some(duration) => duration.min(CLEANUP_MAX_COMMAND_TIMEOUT),
    }
}

/// Stable, collision-resistant directory key for a canonical repository
/// root: sanitized basename plus a short BLAKE3 hash of the full path, so
/// two repositories sharing a basename never share a worktree directory.
pub fn repo_key(canonical_root: &Path) -> String {
    let basename = canonical_root
        .file_name()
        .and_then(|name| name.to_str())
        .map(sanitize_path_component)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "repo".to_string());
    let hash = blake3::hash(canonical_root.to_string_lossy().as_bytes());
    let hex = hash.to_hex();
    format!("{basename}-{}", &hex[..12])
}

fn sanitize_path_component(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Portable worktree storage root for a repository:
/// `<temp>/doge-code/worktrees/<repo-key>`.
pub fn worktree_base_dir(canonical_root: &Path) -> PathBuf {
    std::env::temp_dir()
        .join("doge-code")
        .join("worktrees")
        .join(repo_key(canonical_root))
}

/// Whether a branch name belongs to this feature's generated namespace.
/// Cleanup only ever deletes branches inside this namespace.
pub fn is_generated_branch(branch: &str) -> bool {
    branch.starts_with(GENERATED_BRANCH_PREFIX)
        && branch.len() > GENERATED_BRANCH_PREFIX.len()
        && !branch.contains(' ')
        && !branch.contains('\0')
}

/// Whether a path is contained in the generated storage area of the given
/// canonical repository root. Cleanup only ever removes paths that pass
/// this check.
pub fn is_generated_path(path: &Path, canonical_root: &Path) -> bool {
    path.starts_with(worktree_base_dir(canonical_root))
}

/// Parse `git worktree list --porcelain -z` output.
///
/// With `-z`, every line is NUL-terminated (an empty segment marks the
/// record boundary). Unknown future fields are ignored so the parser keeps
/// working when Git adds new attributes; the verification step fails closed
/// on missing required fields instead.
pub fn parse_worktree_porcelain_z(output: &str) -> Vec<GitWorktreeRecord> {
    let mut records = Vec::new();
    let mut current: Option<GitWorktreeRecord> = None;

    // Split on NUL first (the `-z` record/line separator), then on newlines
    // defensively so non-`-z` output still parses.
    let lines = output.split('\0').flat_map(|chunk| chunk.split('\n'));
    for raw_line in lines {
        // `worktree <path>` lines may carry trailing slashes or CR artifacts;
        // only strip `\r`, never touch the path itself (`trim` would corrupt
        // paths with significant leading/trailing spaces on the field).
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() {
            if let Some(record) = current.take() {
                records.push(record);
            }
            continue;
        }
        if let Some(path_text) = line.strip_prefix("worktree ") {
            if let Some(record) = current.take() {
                records.push(record);
            }
            current = Some(GitWorktreeRecord {
                path: PathBuf::from(path_text),
                head: None,
                branch: None,
                detached: false,
                locked: None,
                prunable: None,
            });
            continue;
        }
        let Some(record) = current.as_mut() else {
            // Lines before the first `worktree` header carry no record.
            continue;
        };
        if let Some(oid) = line.strip_prefix("HEAD ") {
            record.head = Some(oid.to_string());
        } else if let Some(branch) = line.strip_prefix("branch ") {
            record.branch = Some(branch.to_string());
        } else if line == "detached" {
            record.detached = true;
        } else if line == "bare" {
            // Repository-level attribute; irrelevant for verification.
        } else if line == "locked" {
            record.locked = Some(String::new());
        } else if let Some(reason) = line.strip_prefix("locked ") {
            record.locked = Some(reason.trim().to_string());
        } else if line == "prunable" {
            record.prunable = Some(String::new());
        } else if let Some(reason) = line.strip_prefix("prunable ") {
            record.prunable = Some(reason.trim().to_string());
        }
        // Any other unknown field is ignored deliberately.
    }
    if let Some(record) = current.take() {
        records.push(record);
    }
    records
}

/// Map a finished managed Git process to success or a typed error.
/// Cancellation and timeout stay distinct from Git non-zero exits.
fn map_git_output(
    operation: &'static str,
    output: ManagedProcessOutput,
) -> Result<ManagedProcessOutput, WorktreeError> {
    match output.termination {
        ManagedProcessTermination::Cancelled => Err(WorktreeError::Cancelled),
        ManagedProcessTermination::TimedOut => Err(WorktreeError::TimedOut { operation }),
        ManagedProcessTermination::Exited => {
            if output.exit_code == Some(0) {
                Ok(output)
            } else {
                Err(WorktreeError::GitCommandFailed {
                    operation,
                    exit_code: output.exit_code,
                    stderr: crate::tools::budget::head_tail_truncate(
                        output.stderr.trim(),
                        GIT_STDERR_BUDGET_CHARS,
                    )
                    .text,
                })
            }
        }
    }
}

/// Central helper for every Git invocation of this feature. Structured
/// `program = git` plus argv; never a shell string. Timeout, cancellation,
/// non-zero-exit, and bounded-output handling live here exactly once.
async fn run_git(
    repo_cwd: &Path,
    args: &[&str],
    operation: &'static str,
    timeout: Option<Duration>,
    cancellation: &CancellationToken,
) -> Result<ManagedProcessOutput, WorktreeError> {
    if cancellation.is_cancelled() {
        return Err(WorktreeError::Cancelled);
    }
    let spec = ManagedProcessSpec::new(
        "git",
        args.iter().map(|arg| (*arg).to_string()).collect(),
        repo_cwd.to_path_buf(),
    );
    let options = ManagedRunOptions::new(timeout).with_cancellation(Some(cancellation.clone()));
    let output = run_managed_process(spec, options)
        .await
        .map_err(|error| match error {
            ManagedProcessError::Spawn(source) => WorktreeError::ProcessError {
                operation,
                message: format!("failed to spawn git: {source}"),
            },
            ManagedProcessError::Wait(source) => WorktreeError::ProcessError {
                operation,
                message: format!("failed to wait for git: {source}"),
            },
            ManagedProcessError::Cleanup(source) => WorktreeError::ProcessError {
                operation,
                message: format!("failed to clean up git process tree: {source}"),
            },
        })?;
    map_git_output(operation, output)
}

/// Resolve the canonical repository root for a project directory via
/// `git rev-parse --show-toplevel` with an explicit cwd. The process-global
/// cwd is never consulted.
async fn discover_repository_root(
    project_root: &Path,
    timeout: Option<Duration>,
    cancellation: &CancellationToken,
) -> Result<PathBuf, WorktreeError> {
    if !project_root.is_dir() {
        return Err(WorktreeError::InvalidRepositoryRoot(format!(
            "project root is not a directory: {}",
            project_root.display()
        )));
    }
    let output = run_git(
        project_root,
        &["rev-parse", "--show-toplevel"],
        "discover-repository",
        timeout,
        cancellation,
    )
    .await
    .map_err(|error| match error {
        WorktreeError::GitCommandFailed { stderr, .. } => WorktreeError::NotGitRepository(format!(
            "{} ({})",
            project_root.display(),
            stderr.lines().next().unwrap_or("not a git repository")
        )),
        other => other,
    })?;
    let toplevel = output.stdout.trim();
    if toplevel.is_empty() {
        return Err(WorktreeError::InvalidGitOutput {
            operation: "discover-repository",
            reason: "empty `git rev-parse --show-toplevel` output".to_string(),
        });
    }
    let root = PathBuf::from(toplevel);
    if !root.is_dir() {
        return Err(WorktreeError::InvalidGitOutput {
            operation: "discover-repository",
            reason: format!("toplevel is not a directory: {toplevel}"),
        });
    }
    root.canonicalize().map_err(|error| {
        WorktreeError::InvalidRepositoryRoot(format!(
            "cannot canonicalize repository root {toplevel}: {error}"
        ))
    })
}

/// Resolve the base commit OID before creation. A repository with an unborn
/// `HEAD` (no commits) fails here with the Git error instead of silently
/// creating an orphan branch.
async fn resolve_base_oid(
    repo_root: &Path,
    base_ref: &str,
    timeout: Option<Duration>,
    cancellation: &CancellationToken,
) -> Result<String, WorktreeError> {
    if base_ref.is_empty() {
        return Err(WorktreeError::InvalidGitOutput {
            operation: "resolve-base",
            reason: "base ref must not be empty".to_string(),
        });
    }
    let verify_spec = format!("{base_ref}^{{commit}}");
    let output = run_git(
        repo_root,
        &["rev-parse", "--verify", &verify_spec],
        "resolve-base",
        timeout,
        cancellation,
    )
    .await?;
    let oid = output.stdout.trim().to_string();
    if oid.is_empty() || !oid.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(WorktreeError::InvalidGitOutput {
            operation: "resolve-base",
            reason: "could not resolve the base commit OID".to_string(),
        });
    }
    Ok(oid)
}

/// Compare a porcelain record path against the expected generated path.
/// Canonicalizes both sides when possible (handles `/tmp` symlink spelling
/// differences across platforms); falls back to the raw argv spelling Git
/// registered when either side no longer exists.
fn same_worktree_path(record: &Path, expected: &Path) -> bool {
    if record == expected {
        return true;
    }
    match (record.canonicalize(), expected.canonicalize()) {
        (Ok(actual), Ok(want)) => actual == want,
        _ => false,
    }
}

/// Confirm the new worktree is registered exactly as created: canonicalized
/// path match, expected `HEAD` OID, and expected `refs/heads/<branch>`.
/// Any mismatch fails closed.
fn verify_registration(
    records: &[GitWorktreeRecord],
    expected_path: &Path,
    expected_branch: &str,
    expected_oid: &str,
) -> Result<(), WorktreeError> {
    let expected_canonical = expected_path.canonicalize().map_err(|error| {
        WorktreeError::VerificationFailed(format!(
            "cannot canonicalize new worktree path {}: {error}",
            expected_path.display()
        ))
    })?;
    let expected_ref = format!("refs/heads/{expected_branch}");
    for record in records {
        let Ok(actual_canonical) = record.path.canonicalize() else {
            continue;
        };
        if actual_canonical != expected_canonical {
            continue;
        }
        match &record.head {
            Some(head) if head == expected_oid => {}
            other => {
                return Err(WorktreeError::VerificationFailed(format!(
                    "worktree HEAD mismatch: expected {expected_oid}, found {}",
                    other.as_deref().unwrap_or("<missing>")
                )));
            }
        }
        match &record.branch {
            Some(branch) if branch == &expected_ref => {}
            other => {
                return Err(WorktreeError::VerificationFailed(format!(
                    "worktree branch mismatch: expected {expected_ref}, found {}",
                    other.as_deref().unwrap_or("<missing>")
                )));
            }
        }
        return Ok(());
    }
    Err(WorktreeError::VerificationFailed(format!(
        "new worktree {} is not registered in `git worktree list`",
        expected_path.display()
    )))
}

async fn list_worktrees(
    repo_root: &Path,
    operation: &'static str,
    timeout: Option<Duration>,
    cancellation: &CancellationToken,
) -> Result<Vec<GitWorktreeRecord>, WorktreeError> {
    let output = run_git(
        repo_root,
        &["worktree", "list", "--porcelain", "-z"],
        operation,
        timeout,
        cancellation,
    )
    .await?;
    Ok(parse_worktree_porcelain_z(&output.stdout))
}

/// Outcome of probing for the invocation-generated branch. A Git non-zero
/// exit whose stderr positively indicates a missing ref proves the ref is
/// absent; any spawn/wait/timeout/signal (no exit status)/output failure —
/// or a non-zero exit without missing-ref evidence — is an observer failure
/// and must fail closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeneratedBranchProbe {
    /// Git exited non-zero reporting a missing revision: the ref is
    /// definitely absent.
    Absent,
    /// The probe itself could not run reliably; carries a cleanup note.
    Unknown(String),
}

/// Whether `git rev-parse --verify` stderr positively indicates a missing
/// ref, as opposed to a repository/infrastructure failure. Matching is
/// case-insensitive and limited to Git's missing-revision diagnostics;
/// anything else (including empty stderr) returns `false` so the caller
/// fails closed with an observation failure instead of assuming absence.
fn branch_probe_stderr_indicates_absence(stderr: &str) -> bool {
    let lowered = stderr.to_lowercase();
    lowered.contains("unknown revision") || lowered.contains("needed a single revision")
}

/// Classify a branch-existence probe error fail-closed. Only a
/// `GitCommandFailed` with a concrete non-zero exit status *and* stderr
/// positively indicating a missing ref is authoritative absence; every
/// other error — spawn/wait/cleanup, cancellation, timeout, signal death
/// without an exit status, invalid output, or a Git failure that does not
/// identify the ref as missing — is an observer failure that must be
/// reported, never treated as absence.
pub fn classify_branch_probe_error(
    generated_branch: &str,
    error: &WorktreeError,
) -> GeneratedBranchProbe {
    match error {
        WorktreeError::GitCommandFailed {
            exit_code: Some(code),
            stderr,
            ..
        } if *code != 0 && branch_probe_stderr_indicates_absence(stderr) => {
            GeneratedBranchProbe::Absent
        }
        other => GeneratedBranchProbe::Unknown(format!(
            "cleanup: could not verify branch {generated_branch}: {other}; left in place"
        )),
    }
}

/// Best-effort cleanup of this invocation's generated resources. Only the
/// generated path (inside this invocation's storage area) and the generated
/// `doge/` branch are ever candidates; anything else is left untouched.
/// Never prunes globally. Returns human-readable notes for leftovers or
/// cleanup failures; an empty vector means nothing remained.
///
/// Cleanup uses a fresh cancellation token: it must still run when the
/// original operation was cancelled. Every cleanup Git invocation uses the
/// finite cleanup budget (never the unlimited normal timeout), and the
/// whole sequence is bounded by an overall cleanup budget.
async fn cleanup_generated_worktree(
    repo_root: &Path,
    generated_path: &Path,
    generated_branch: &str,
    timeout: Option<Duration>,
) -> Vec<String> {
    if !is_generated_branch(generated_branch) || !is_generated_path(generated_path, repo_root) {
        return vec![format!(
            "cleanup refused: {} / {} is outside this invocation's generated identity",
            generated_path.display(),
            generated_branch
        )];
    }
    match tokio::time::timeout(
        CLEANUP_OVERALL_BUDGET,
        cleanup_generated_worktree_inner(repo_root, generated_path, generated_branch, timeout),
    )
    .await
    {
        Ok(notes) => notes,
        Err(_) => vec![format!(
            "cleanup: overall cleanup budget of {} ms exceeded for {}; leftovers may remain",
            CLEANUP_OVERALL_BUDGET.as_millis(),
            generated_path.display()
        )],
    }
}

async fn cleanup_generated_worktree_inner(
    repo_root: &Path,
    generated_path: &Path,
    generated_branch: &str,
    timeout: Option<Duration>,
) -> Vec<String> {
    let mut notes = Vec::new();
    // `debug_assert!` documents the wrapper contract; the checks below stay
    // fail-closed in release builds as well.
    debug_assert!(is_generated_branch(generated_branch));
    debug_assert!(is_generated_path(generated_path, repo_root));
    if !is_generated_branch(generated_branch) || !is_generated_path(generated_path, repo_root) {
        notes.push(format!(
            "cleanup refused: {} / {} is outside this invocation's generated identity",
            generated_path.display(),
            generated_branch
        ));
        return notes;
    }
    // Finite per-command budget even when normal execution is unlimited.
    let cleanup_timeout = Some(bound_cleanup_timeout(timeout));
    let fresh = CancellationToken::new();
    let expected_ref = format!("refs/heads/{generated_branch}");

    let records = match list_worktrees(repo_root, "cleanup-list", cleanup_timeout, &fresh).await {
        Ok(records) => records,
        Err(error) => {
            return vec![format!(
                "cleanup: could not list worktrees for {}: {error}",
                generated_path.display()
            )];
        }
    };
    let registered = records.iter().any(|record| {
        same_worktree_path(&record.path, generated_path)
            && record.branch.as_deref() == Some(expected_ref.as_str())
    });
    if registered {
        if let Err(error) = run_git(
            repo_root,
            &[
                "worktree",
                "remove",
                "--force",
                &generated_path.to_string_lossy(),
            ],
            "cleanup-remove",
            cleanup_timeout,
            &fresh,
        )
        .await
        {
            notes.push(format!(
                "cleanup: could not remove worktree {}: {error}",
                generated_path.display()
            ));
        }
    } else if generated_path.exists() {
        // `git worktree add` failed before registering the path but left the
        // directory behind. It is inside this invocation's storage area and
        // carries this invocation's unique id, so removing it is safe.
        if let Err(error) = tokio::fs::remove_dir_all(generated_path).await {
            notes.push(format!(
                "cleanup: could not remove stray directory {}: {error}",
                generated_path.display()
            ));
        }
    }

    // Remove the generated branch only when no worktree has it checked out
    // anymore. `branch -d` (never `-D`) refuses to delete work that gained
    // commits in the meantime; that refusal is reported, not forced.
    let records = match list_worktrees(repo_root, "cleanup-list", cleanup_timeout, &fresh).await {
        Ok(records) => records,
        Err(error) => {
            notes.push(format!("cleanup: could not re-list worktrees: {error}"));
            return notes;
        }
    };
    if records
        .iter()
        .any(|record| record.branch.as_deref() == Some(expected_ref.as_str()))
    {
        notes.push(format!(
            "cleanup: branch {generated_branch} is still checked out; left in place"
        ));
        return notes;
    }
    match run_git(
        repo_root,
        &["rev-parse", "--verify", &expected_ref],
        "cleanup-branch-exists",
        cleanup_timeout,
        &fresh,
    )
    .await
    {
        Ok(_) => {}
        Err(error) => match classify_branch_probe_error(generated_branch, &error) {
            GeneratedBranchProbe::Absent => return notes,
            GeneratedBranchProbe::Unknown(note) => {
                notes.push(note);
                return notes;
            }
        },
    }
    if let Err(error) = run_git(
        repo_root,
        &["branch", "-d", generated_branch],
        "cleanup-branch",
        cleanup_timeout,
        &fresh,
    )
    .await
    {
        notes.push(format!(
            "cleanup: branch {generated_branch} remains; delete it manually (`git branch -D {generated_branch}`): {error}"
        ));
    }
    notes
}

/// Attach cleanup notes to the original failure while preserving the primary
/// lifecycle classification. An empty note list returns the original error
/// unchanged. Cancellation stays cancellation (`CancelledWithCleanup`),
/// timeout stays timeout (`TimedOutWithCleanup`), and other failures gain
/// the cleanup context (`CleanupFailed`); cleanup diagnostics are always
/// retrievable via [`WorktreeError::cleanup_diagnostic`] without parsing
/// strings.
fn with_cleanup_notes(original: WorktreeError, notes: Vec<String>) -> WorktreeError {
    if notes.is_empty() {
        return original;
    }
    let cleanup = notes.join("; ");
    match original {
        WorktreeError::Cancelled => WorktreeError::CancelledWithCleanup { cleanup },
        WorktreeError::TimedOut { operation } => {
            WorktreeError::TimedOutWithCleanup { operation, cleanup }
        }
        // Already carry diagnostics: append without reclassifying the
        // primary lifecycle outcome. Cancellation must never become a
        // generic failure merely because a second cleanup note arrived.
        WorktreeError::CancelledWithCleanup { cleanup: existing } => {
            WorktreeError::CancelledWithCleanup {
                cleanup: if existing.is_empty() {
                    cleanup
                } else {
                    format!("{existing}; {cleanup}")
                },
            }
        }
        WorktreeError::TimedOutWithCleanup {
            operation,
            cleanup: existing,
        } => WorktreeError::TimedOutWithCleanup {
            operation,
            cleanup: if existing.is_empty() {
                cleanup
            } else {
                format!("{existing}; {cleanup}")
            },
        },
        WorktreeError::CleanupFailed {
            original,
            cleanup: existing,
        } => WorktreeError::CleanupFailed {
            original,
            cleanup: if existing.is_empty() {
                cleanup
            } else {
                format!("{existing}; {cleanup}")
            },
        },
        other => WorktreeError::CleanupFailed {
            original: other.to_string(),
            cleanup,
        },
    }
}

/// Canonicalize the verified worktree path. A failure here still owns the
/// invocation-generated worktree/branch, so failure cleanup runs before the
/// typed failure is returned (with any diagnostics attached instead of
/// orphaning the resources silently).
async fn canonicalize_verified_path(
    repo_root: &Path,
    worktree_path: &Path,
    generated_branch: &str,
    timeout: Option<Duration>,
) -> Result<PathBuf, WorktreeError> {
    match worktree_path.canonicalize() {
        Ok(canonical) => Ok(canonical),
        Err(error) => {
            let notes =
                cleanup_generated_worktree(repo_root, worktree_path, generated_branch, timeout)
                    .await;
            for note in &notes {
                tracing::warn!(note = %note, "worktree path cleanup reported");
            }
            Err(with_cleanup_notes(
                WorktreeError::VerificationFailed(format!(
                    "cannot canonicalize verified worktree path: {error}"
                )),
                notes,
            ))
        }
    }
}
/// Create one isolated linked worktree and its generated branch in a single
/// Git operation (`git worktree add -b <branch> <path> HEAD`), then verify
/// the registration through porcelain output. Only `CreatedWorktree` after
/// successful verification counts as success.
///
/// Never mutates the process-global cwd, never touches the current Doge
/// project/session context, and never uses force semantics (`-B`, `-f`,
/// `--force`) for creation.
pub async fn create_worktree(
    request: CreateWorktreeRequest,
    cancellation: CancellationToken,
) -> Result<CreatedWorktree, WorktreeError> {
    let started = std::time::Instant::now();
    if cancellation.is_cancelled() {
        return Err(WorktreeError::Cancelled);
    }
    let timeout = resolve_timeout(request.command_timeout_ms);
    let base_ref = request.base_ref.clone();

    let repo_root = discover_repository_root(&request.project_root, timeout, &cancellation).await?;
    let base_oid = resolve_base_oid(&repo_root, &base_ref, timeout, &cancellation).await?;

    let id = uuid::Uuid::now_v7().to_string();
    let branch = format!("{GENERATED_BRANCH_PREFIX}{id}");
    let worktree_path = worktree_base_dir(&repo_root).join(&id);
    if let Some(parent) = worktree_path.parent()
        && let Err(error) = tokio::fs::create_dir_all(parent).await
    {
        return Err(WorktreeError::Io(error));
    }

    tracing::info!(
        repository_root = %repo_root.display(),
        worktree_path = %worktree_path.display(),
        branch = %branch,
        base_ref = %base_ref,
        "creating git worktree"
    );

    let path_arg = worktree_path.to_string_lossy().to_string();
    if let Err(error) = run_git(
        &repo_root,
        &["worktree", "add", "-b", &branch, &path_arg, &base_ref],
        "create-worktree",
        timeout,
        &cancellation,
    )
    .await
    {
        let notes = cleanup_generated_worktree(&repo_root, &worktree_path, &branch, timeout).await;
        for note in &notes {
            tracing::warn!(note = %note, "worktree failure cleanup reported");
        }
        return Err(with_cleanup_notes(error, notes));
    }

    match list_worktrees(&repo_root, "verify-worktree", timeout, &cancellation).await {
        Ok(records) => {
            if let Err(error) = verify_registration(&records, &worktree_path, &branch, &base_oid) {
                let notes =
                    cleanup_generated_worktree(&repo_root, &worktree_path, &branch, timeout).await;
                for note in &notes {
                    tracing::warn!(note = %note, "worktree verification cleanup reported");
                }
                return Err(with_cleanup_notes(error, notes));
            }
        }
        Err(error) => {
            let notes =
                cleanup_generated_worktree(&repo_root, &worktree_path, &branch, timeout).await;
            for note in &notes {
                tracing::warn!(note = %note, "worktree verification cleanup reported");
            }
            return Err(with_cleanup_notes(error, notes));
        }
    }

    let worktree_path =
        canonicalize_verified_path(&repo_root, &worktree_path, &branch, timeout).await?;

    tracing::info!(
        repository_root = %repo_root.display(),
        worktree_path = %worktree_path.display(),
        branch = %branch,
        base_oid = %base_oid,
        elapsed_ms = started.elapsed().as_millis(),
        "git worktree created"
    );

    Ok(CreatedWorktree {
        repository_root: repo_root,
        worktree_path,
        branch,
        base_ref,
        head_oid: base_oid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Initialize a Git repository with one commit. Every Git command uses
    /// an explicit `current_dir`; the process-global cwd is never touched.
    fn init_git_repo(root: &Path) {
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .expect("git must run");
            assert!(
                status.success(),
                "git {args:?} failed in {}",
                root.display()
            );
        };
        run(&["init"]);
        run(&["config", "user.name", "Test User"]);
        run(&["config", "user.email", "test@example.com"]);
        std::fs::write(root.join("README.md"), "test").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "Initial commit"]);
    }

    fn head_oid(root: &Path) -> String {
        let output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn branch_exists(root: &Path, branch: &str) -> bool {
        std::process::Command::new("git")
            .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
            .current_dir(root)
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    fn request_for(root: &Path) -> CreateWorktreeRequest {
        CreateWorktreeRequest::new(root.to_path_buf(), 30_000)
    }

    #[test]
    fn test_resolve_timeout_preserves_unlimited() {
        assert_eq!(resolve_timeout(0), None);
        assert_eq!(resolve_timeout(1500), Some(Duration::from_millis(1500)));
    }

    #[test]
    fn test_repo_key_differs_for_same_basename() {
        // Two different repositories sharing a basename must not collide.
        let outer = tempfile::tempdir().unwrap();
        let first = outer.path().join("parent-a").join("same-name");
        let second = outer.path().join("parent-b").join("same-name");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let first_key = repo_key(&first);
        let second_key = repo_key(&second);
        assert_ne!(first_key, second_key);
        assert!(first_key.starts_with("same-name-"));
        assert_ne!(worktree_base_dir(&first), worktree_base_dir(&second));
        assert!(worktree_base_dir(&first).starts_with(std::env::temp_dir()));
    }

    #[test]
    fn test_repo_key_sanitizes_components() {
        let root = PathBuf::from("/tmp/odd name!here");
        let key = repo_key(&root);
        assert!(key.starts_with("odd_name_here-"));
    }

    #[test]
    fn test_generated_identity_guards() {
        assert!(is_generated_branch(
            "doge/550e8400-e29b-41d4-a716-446655440000"
        ));
        assert!(!is_generated_branch("doge/"));
        assert!(!is_generated_branch("main"));
        assert!(!is_generated_branch("feature/doge/x"));
        assert!(!is_generated_branch("doge/has space"));
    }

    #[test]
    fn test_parse_porcelain_normal_branch_worktree() {
        let fixture = "worktree /repo/main\0HEAD abc123\0branch refs/heads/main\0\0worktree /repo/wt-1\0HEAD def456\0branch refs/heads/doge/wt-1\0\0";
        let records = parse_worktree_porcelain_z(fixture);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].path, PathBuf::from("/repo/main"));
        assert_eq!(records[0].head.as_deref(), Some("abc123"));
        assert_eq!(records[0].branch.as_deref(), Some("refs/heads/main"));
        assert!(!records[0].detached);
        assert_eq!(records[1].branch.as_deref(), Some("refs/heads/doge/wt-1"));
    }

    #[test]
    fn test_parse_porcelain_detached_locked_prunable() {
        let fixture = "worktree /repo/det\0HEAD abc123\0detached\0\0worktree /repo/locked\0HEAD abc123\0branch refs/heads/x\0locked user reason\0\0worktree /repo/gone\0HEAD abc123\0branch refs/heads/y\0prunable\0\0";
        let records = parse_worktree_porcelain_z(fixture);
        assert_eq!(records.len(), 3);
        assert!(records[0].detached);
        assert_eq!(records[0].branch, None);
        assert_eq!(records[1].locked.as_deref(), Some("user reason"));
        assert_eq!(records[2].prunable.as_deref(), Some(""));
    }

    #[test]
    fn test_parse_porcelain_paths_with_spaces_and_unknown_fields() {
        let fixture = "worktree /tmp/my repo/wt 1\0HEAD abc123\0branch refs/heads/doge/1\0bare\0some-future-field value\0\0";
        let records = parse_worktree_porcelain_z(fixture);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].path, PathBuf::from("/tmp/my repo/wt 1"));
        assert_eq!(records[0].branch.as_deref(), Some("refs/heads/doge/1"));
    }

    #[test]
    fn test_parse_porcelain_newline_separated_and_trailing() {
        let fixture = "worktree /a\nHEAD 111\nbranch refs/heads/main\n\nworktree /b\nHEAD 222\nbranch refs/heads/x\n";
        let records = parse_worktree_porcelain_z(fixture);
        assert_eq!(records.len(), 2);
        assert_eq!(parse_worktree_porcelain_z(""), Vec::new());
    }

    #[test]
    fn test_parse_porcelain_bare_locked_prunable_and_future_fields() {
        let fixture = "worktree /repo/l\0HEAD abc123\0branch refs/heads/x\0locked\0\0worktree /repo/p\0HEAD abc123\0branch refs/heads/y\0prunable\0\0worktree /repo/f\0HEAD abc123\0branch refs/heads/z\0lockedby eve\0\0";
        let records = parse_worktree_porcelain_z(fixture);
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].locked.as_deref(), Some(""));
        assert_eq!(records[1].prunable.as_deref(), Some(""));
        // A hypothetical future `lockedby` field must not be mistaken for
        // the `locked` attribute.
        assert_eq!(records[2].locked, None);
    }

    #[test]
    fn test_map_git_output_keeps_termination_distinct() {
        let exited_ok = ManagedProcessOutput {
            termination: ManagedProcessTermination::Exited,
            exit_code: Some(0),
            stdout: "ok".to_string(),
            stderr: String::new(),
            capture_truncated: false,
            warnings: Vec::new(),
        };
        assert!(map_git_output("op", exited_ok).is_ok());

        let exited_fail = ManagedProcessOutput {
            termination: ManagedProcessTermination::Exited,
            exit_code: Some(128),
            stdout: String::new(),
            stderr: "fatal: boom".to_string(),
            capture_truncated: false,
            warnings: Vec::new(),
        };
        assert!(matches!(
            map_git_output("op", exited_fail),
            Err(WorktreeError::GitCommandFailed { .. })
        ));

        for termination in [
            ManagedProcessTermination::TimedOut,
            ManagedProcessTermination::Cancelled,
        ] {
            let output = ManagedProcessOutput {
                termination,
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                capture_truncated: false,
                warnings: Vec::new(),
            };
            match (termination, map_git_output("op", output)) {
                (ManagedProcessTermination::TimedOut, Err(WorktreeError::TimedOut { .. })) => {}
                (ManagedProcessTermination::Cancelled, Err(WorktreeError::Cancelled)) => {}
                other => panic!("unexpected mapping: {other:?}"),
            }
        }
    }

    #[test]
    fn test_cleanup_notes_preserve_cancel_and_timeout() {
        // Cancellation stays cancellation while carrying cleanup diagnostics;
        // timeout stays timeout; other failures gain the cleanup context.
        let cancelled = with_cleanup_notes(WorktreeError::Cancelled, vec!["leftover".to_string()]);
        assert!(matches!(
            cancelled,
            WorktreeError::CancelledWithCleanup { .. }
        ));
        assert!(cancelled.is_cancelled());
        assert!(!cancelled.is_timed_out());
        assert_eq!(cancelled.cleanup_diagnostic(), Some("leftover"));
        assert_eq!(classify_primary(&cancelled), WorktreePrimary::Cancelled);

        let timed_out = with_cleanup_notes(
            WorktreeError::TimedOut { operation: "op" },
            vec!["leftover".to_string()],
        );
        assert!(matches!(
            timed_out,
            WorktreeError::TimedOutWithCleanup { .. }
        ));
        assert!(timed_out.is_timed_out());
        assert!(!timed_out.is_cancelled());
        assert_eq!(timed_out.cleanup_diagnostic(), Some("leftover"));
        assert_eq!(classify_primary(&timed_out), WorktreePrimary::TimedOut);

        let failed = with_cleanup_notes(
            WorktreeError::VerificationFailed("bad".to_string()),
            vec!["note".to_string()],
        );
        assert!(matches!(failed, WorktreeError::CleanupFailed { .. }));
        assert!(!failed.is_cancelled());
        assert!(!failed.is_timed_out());
        assert_eq!(failed.cleanup_diagnostic(), Some("note"));
        assert_eq!(classify_primary(&failed), WorktreePrimary::Failed);

        let clean = with_cleanup_notes(WorktreeError::Cancelled, Vec::new());
        assert!(matches!(clean, WorktreeError::Cancelled));
        assert!(clean.cleanup_diagnostic().is_none());

        let clean_timeout =
            with_cleanup_notes(WorktreeError::TimedOut { operation: "op" }, Vec::new());
        assert!(matches!(clean_timeout, WorktreeError::TimedOut { .. }));
        assert!(clean_timeout.cleanup_diagnostic().is_none());
    }

    #[test]
    fn test_cleanup_budget_is_finite_when_normal_timeout_unlimited() {
        // Normal execution keeps unlimited semantics, but cleanup never does.
        assert_eq!(resolve_timeout(0), None);
        let budget = resolve_cleanup_timeout(0);
        assert!(budget > Duration::ZERO);
        assert!(budget <= CLEANUP_MAX_COMMAND_TIMEOUT);
        assert_eq!(bound_cleanup_timeout(None), CLEANUP_DEFAULT_COMMAND_TIMEOUT);
        // A large configured timeout is clamped to the cleanup cap.
        assert_eq!(
            bound_cleanup_timeout(Some(Duration::from_secs(600))),
            CLEANUP_MAX_COMMAND_TIMEOUT
        );
        assert_eq!(
            resolve_cleanup_timeout(600_000),
            CLEANUP_MAX_COMMAND_TIMEOUT
        );
        // A small configured timeout stays bounded and finite.
        assert_eq!(
            bound_cleanup_timeout(Some(Duration::from_millis(1500))),
            Duration::from_millis(1500)
        );
        // A zero duration can never bound a subprocess; it maps to the
        // finite default instead of an immediate timeout.
        assert_eq!(
            bound_cleanup_timeout(Some(Duration::ZERO)),
            CLEANUP_DEFAULT_COMMAND_TIMEOUT
        );
    }

    #[test]
    fn test_cleanup_notes_append_without_reclassifying() {
        // Already-wrapped errors keep their primary lifecycle classification
        // when further notes arrive; cancellation never becomes failure.
        let again = with_cleanup_notes(
            WorktreeError::CancelledWithCleanup {
                cleanup: "first".to_string(),
            },
            vec!["second".to_string()],
        );
        assert!(matches!(again, WorktreeError::CancelledWithCleanup { .. }));
        assert!(again.is_cancelled());
        assert_eq!(again.cleanup_diagnostic(), Some("first; second"));
        assert_eq!(classify_primary(&again), WorktreePrimary::Cancelled);

        let again = with_cleanup_notes(
            WorktreeError::TimedOutWithCleanup {
                operation: "op",
                cleanup: "first".to_string(),
            },
            vec!["second".to_string()],
        );
        assert!(matches!(again, WorktreeError::TimedOutWithCleanup { .. }));
        assert!(again.is_timed_out());
        assert_eq!(again.cleanup_diagnostic(), Some("first; second"));
        assert_eq!(classify_primary(&again), WorktreePrimary::TimedOut);

        let again = with_cleanup_notes(
            WorktreeError::CleanupFailed {
                original: "boom".to_string(),
                cleanup: "first".to_string(),
            },
            vec!["second".to_string()],
        );
        assert!(matches!(again, WorktreeError::CleanupFailed { .. }));
        assert_eq!(again.cleanup_diagnostic(), Some("first; second"));
        assert_eq!(classify_primary(&again), WorktreePrimary::Failed);
    }

    #[test]
    fn test_branch_probe_absent_only_on_git_nonzero_exit() {
        let absent = classify_branch_probe_error(
            "doge/id",
            &WorktreeError::GitCommandFailed {
                operation: "cleanup-branch-exists",
                exit_code: Some(128),
                stderr: "unknown revision".to_string(),
            },
        );
        assert_eq!(absent, GeneratedBranchProbe::Absent);

        // A Git failure without an exit status (signal death, unknown exit)
        // is an observer failure, not proof of absence: fail closed.
        match classify_branch_probe_error(
            "doge/id",
            &WorktreeError::GitCommandFailed {
                operation: "cleanup-branch-exists",
                exit_code: None,
                stderr: String::new(),
            },
        ) {
            GeneratedBranchProbe::Unknown(note) => {
                assert!(note.contains("could not verify branch doge/id"), "{note}");
                assert!(note.contains("left in place"), "{note}");
            }
            other => panic!("missing exit status must fail closed, got {other:?}"),
        }

        // Spawn/wait/timeout/cancellation/observer failures fail closed.
        for error in [
            WorktreeError::ProcessError {
                operation: "cleanup-branch-exists",
                message: "failed to spawn git: boom".to_string(),
            },
            WorktreeError::TimedOut {
                operation: "cleanup-branch-exists",
            },
            WorktreeError::Cancelled,
            WorktreeError::InvalidGitOutput {
                operation: "cleanup-branch-exists",
                reason: "bad".to_string(),
            },
        ] {
            match classify_branch_probe_error("doge/id", &error) {
                GeneratedBranchProbe::Unknown(note) => {
                    assert!(note.contains("could not verify branch doge/id"), "{note}");
                    assert!(note.contains("left in place"), "{note}");
                }
                other => panic!("observer failure must fail closed, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_branch_probe_requires_absence_evidence_in_stderr() {
        // A non-zero Git exit without missing-ref evidence (wrong
        // repository, corrupted state, unexpected failure) must fail closed
        // instead of being treated as authoritative absence.
        for stderr in [
            "",
            "fatal: not a git repository (or any of the parent directories)",
            "fatal: unable to read log 'refs/heads/doge/id': Input/output error",
        ] {
            match classify_branch_probe_error(
                "doge/id",
                &WorktreeError::GitCommandFailed {
                    operation: "cleanup-branch-exists",
                    exit_code: Some(128),
                    stderr: stderr.to_string(),
                },
            ) {
                GeneratedBranchProbe::Unknown(note) => {
                    assert!(note.contains("could not verify branch doge/id"), "{note}");
                    assert!(note.contains("left in place"), "{note}");
                }
                other => {
                    panic!("exit 128 without absence evidence must fail closed, got {other:?}")
                }
            }
        }

        // A contradictory success exit code is never absence either.
        match classify_branch_probe_error(
            "doge/id",
            &WorktreeError::GitCommandFailed {
                operation: "cleanup-branch-exists",
                exit_code: Some(0),
                stderr: "fatal: Needed a single revision".to_string(),
            },
        ) {
            GeneratedBranchProbe::Unknown(_) => {}
            other => panic!("success exit code must fail closed, got {other:?}"),
        }

        // Both of Git's missing-revision diagnostics count as absence
        // (case-insensitively, so locale-cased output still matches).
        for stderr in [
            "fatal: Needed a single revision",
            "FATAL: NEEDED A SINGLE REVISION",
            "error: unknown revision refs/heads/doge/id",
        ] {
            assert_eq!(
                classify_branch_probe_error(
                    "doge/id",
                    &WorktreeError::GitCommandFailed {
                        operation: "cleanup-branch-exists",
                        exit_code: Some(128),
                        stderr: stderr.to_string(),
                    },
                ),
                GeneratedBranchProbe::Absent,
                "stderr: {stderr}"
            );
        }
    }

    #[test]
    fn test_primary_classification_never_parses_strings() {
        // Typed predicates stay distinct across attached diagnostics.
        let cancelled = WorktreeError::CancelledWithCleanup {
            cleanup: "x".to_string(),
        };
        assert!(cancelled.is_cancelled());
        assert_eq!(classify_primary(&cancelled), WorktreePrimary::Cancelled);
        let timed_out = WorktreeError::TimedOutWithCleanup {
            operation: "create-worktree",
            cleanup: "y".to_string(),
        };
        assert!(timed_out.is_timed_out());
        assert_eq!(classify_primary(&timed_out), WorktreePrimary::TimedOut);
        // Display carries diagnostics but classification does not depend on it.
        assert!(cancelled.to_string().contains("cancelled"));
        assert!(timed_out.to_string().contains("timed out"));
    }

    #[test]
    fn test_cleanup_never_uses_global_prune_or_force_delete() {
        // Structural guard: the manager must never introduce a global prune
        // or a forced branch deletion that could discard commits. Forbidden
        // tokens are built at runtime so this test's own literals cannot
        // self-match the source under test.
        let source = include_str!("worktree_manager.rs");
        // Only scan production code: the test module itself names the
        // forbidden tokens, so split before `#[cfg(test)]`.
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        let prune_cmd = ["worktree", "prune"].join(" ");
        assert!(!prod.contains(&prune_cmd), "no global prune allowed");
        let prune_arg = format!("\"{}\"", ["pr", "une"].concat());
        assert!(!prod.contains(&prune_arg), "no global prune allowed");
        let force_flag = format!("\"{}\"", ["-", "D"].concat());
        assert!(
            !prod.contains(&force_flag),
            "no forced branch deletion allowed"
        );
        let safe_delete = format!("\"{}\", \"{}\"", "branch", ["-", "d"].concat());
        assert!(
            prod.contains(&safe_delete),
            "branch removal must use safe `-d`"
        );
    }

    #[tokio::test]
    async fn test_cleanup_refuses_outside_generated_identity() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_git_repo(&repo);
        let canonical = repo.canonicalize().unwrap();

        // Outside path and outside branch are both refused without touching
        // anything: no new branches appear.
        let outside_path = canonical.join("elsewhere");
        let notes = cleanup_generated_worktree(
            &canonical,
            &outside_path,
            "doge/some-id",
            Some(Duration::from_secs(5)),
        )
        .await;
        assert!(
            notes.iter().any(|note| note.contains("outside")),
            "{notes:?}"
        );

        let inside_path = worktree_base_dir(&canonical).join("some-id");
        let notes = cleanup_generated_worktree(
            &canonical,
            &inside_path,
            "main",
            Some(Duration::from_secs(5)),
        )
        .await;
        assert!(
            notes.iter().any(|note| note.contains("outside")),
            "{notes:?}"
        );

        let branches = std::process::Command::new("git")
            .args(["branch", "--list", "doge/*"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            String::from_utf8(branches.stdout)
                .unwrap()
                .trim()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_cleanup_with_unlimited_timeout_stays_bounded() {
        // Cleanup with `None` (unlimited normal timeout) still resolves to a
        // finite per-command budget; the call below completes promptly with
        // a diagnostic instead of hanging.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_git_repo(&repo);
        let canonical = repo.canonicalize().unwrap();
        let missing = worktree_base_dir(&canonical).join("missing-id");
        let notes = tokio::time::timeout(
            Duration::from_secs(20),
            cleanup_generated_worktree(&canonical, &missing, "doge/missing-id", None),
        )
        .await
        .expect("cleanup with unlimited normal timeout must stay bounded");
        // Missing path/branch yields no leftovers; the key assertion is that
        // the future above completed within the test timeout.
        assert!(notes.is_empty(), "{notes:?}");
    }

    /// Run one Git command synchronously in tests. Every invocation uses an
    /// explicit `current_dir`; the process-global cwd is never touched.
    fn git_ok(root: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .expect("git must run");
        assert!(
            status.success(),
            "git {args:?} failed in {}",
            root.display()
        );
    }

    #[tokio::test]
    async fn test_cleanup_removes_registered_worktree_and_branch() {
        // Positive path: a registered invocation-owned worktree and its
        // generated branch are both removed without diagnostics.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_git_repo(&repo);
        let canonical = repo.canonicalize().unwrap();
        let id = uuid::Uuid::now_v7().to_string();
        let branch = format!("{GENERATED_BRANCH_PREFIX}{id}");
        let path = worktree_base_dir(&canonical).join(&id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        git_ok(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                &path.to_string_lossy(),
                "HEAD",
            ],
        );
        assert!(path.is_dir());
        assert!(branch_exists(&repo, &branch));

        let notes =
            cleanup_generated_worktree(&canonical, &path, &branch, Some(Duration::from_secs(10)))
                .await;
        assert!(notes.is_empty(), "{notes:?}");
        assert!(!path.exists(), "registered worktree must be removed");
        assert!(
            !branch_exists(&repo, &branch),
            "generated branch must be removed"
        );
    }

    #[tokio::test]
    async fn test_cleanup_removes_stray_directory_without_branch() {
        // `git worktree add` can fail after creating the invocation-owned
        // directory but before registering it; the stray directory is
        // removed even though no branch exists.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_git_repo(&repo);
        let canonical = repo.canonicalize().unwrap();
        let id = uuid::Uuid::now_v7().to_string();
        let branch = format!("{GENERATED_BRANCH_PREFIX}{id}");
        let path = worktree_base_dir(&canonical).join(&id);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("partial.txt"), "partial").unwrap();

        let notes =
            cleanup_generated_worktree(&canonical, &path, &branch, Some(Duration::from_secs(10)))
                .await;
        assert!(notes.is_empty(), "{notes:?}");
        assert!(!path.exists(), "stray directory must be removed");
        assert!(!branch_exists(&repo, &branch));
    }

    #[tokio::test]
    async fn test_cleanup_leaves_branch_checked_out_elsewhere() {
        // The generated branch checked out by a *different* worktree path
        // must never be deleted: cleanup reports and leaves it in place.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_git_repo(&repo);
        let canonical = repo.canonicalize().unwrap();
        let id = uuid::Uuid::now_v7().to_string();
        let branch = format!("{GENERATED_BRANCH_PREFIX}{id}");
        let path = worktree_base_dir(&canonical).join(&id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        git_ok(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                &path.to_string_lossy(),
                "HEAD",
            ],
        );
        // A different invocation-owned path that was never registered.
        let other = worktree_base_dir(&canonical).join(uuid::Uuid::now_v7().to_string());
        assert!(!other.exists());

        let notes =
            cleanup_generated_worktree(&canonical, &other, &branch, Some(Duration::from_secs(10)))
                .await;
        assert!(
            notes.iter().any(|note| note.contains("still checked out")),
            "{notes:?}"
        );
        assert!(
            branch_exists(&repo, &branch),
            "branch in use elsewhere must be preserved"
        );
        assert!(path.is_dir(), "other worktree must be untouched");

        git_ok(
            &repo,
            &["worktree", "remove", "--force", &path.to_string_lossy()],
        );
        git_ok(&repo, &["branch", "-D", &branch]);
    }

    #[tokio::test]
    async fn test_canonicalize_failure_still_cleans_up_generated_branch() {
        // Regression: a post-verification canonicalize failure owns the
        // invocation-generated branch and must surface its leftover instead
        // of orphaning it silently.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_git_repo(&repo);
        let canonical = repo.canonicalize().unwrap();

        // Standalone generated branch holding an unmerged commit, so safe
        // `branch -d` refuses and the diagnostic is observable. Created and
        // abandoned without a checkout, then HEAD is restored.
        let id = uuid::Uuid::now_v7().to_string();
        let branch = format!("{GENERATED_BRANCH_PREFIX}{id}");
        git_ok(&repo, &["checkout", "-b", &branch]);
        std::fs::write(repo.join("unmerged.txt"), "unmerged").unwrap();
        git_ok(&repo, &["add", "."]);
        git_ok(&repo, &["commit", "-m", "unmerged"]);
        git_ok(&repo, &["checkout", "-"]);

        // The verified path vanished before canonicalization. This simulates
        // the TOCTOU race deterministically: no sleeps, no timing.
        let missing = worktree_base_dir(&canonical).join(&id);
        assert!(!missing.exists());

        match canonicalize_verified_path(
            &canonical,
            &missing,
            &branch,
            Some(Duration::from_secs(10)),
        )
        .await
        {
            Err(error) => {
                let diagnostic = error
                    .cleanup_diagnostic()
                    .expect("cleanup diagnostic must be attached");
                assert!(diagnostic.contains("remains"), "{diagnostic}");
                assert_eq!(classify_primary(&error), WorktreePrimary::Failed);
                // Safety: the unmerged branch was reported, never
                // force-deleted.
                assert!(branch_exists(&repo, &branch));
            }
            Ok(_) => panic!("missing path must fail canonicalization"),
        }
    }

    #[tokio::test]
    async fn test_non_repository_fails_typed() {
        let dir = tempfile::tempdir().unwrap();
        let result = create_worktree(request_for(dir.path()), CancellationToken::new()).await;
        assert!(matches!(result, Err(WorktreeError::NotGitRepository(_))));
    }

    #[tokio::test]
    async fn test_create_worktree_generates_branch_itself() {
        // Core regression: the caller never pre-creates the branch.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_git_repo(&repo);
        let expected_head = head_oid(&repo);

        let created = create_worktree(request_for(&repo), CancellationToken::new())
            .await
            .expect("worktree creation must succeed without a pre-existing branch");

        assert!(created.branch.starts_with("doge/"));
        assert_eq!(created.head_oid, expected_head);
        assert_eq!(created.base_ref, "HEAD");
        assert!(created.worktree_path.is_dir());
        assert!(branch_exists(&repo, &created.branch));

        // Registered with matching branch and HEAD.
        let output = std::process::Command::new("git")
            .args(["worktree", "list", "--porcelain", "-z"])
            .current_dir(&repo)
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let records = parse_worktree_porcelain_z(&stdout);
        let found = records.iter().any(|record| {
            record.branch.as_deref() == Some(format!("refs/heads/{}", created.branch).as_str())
                && record.head.as_deref() == Some(expected_head.as_str())
        });
        assert!(found, "generated branch must be registered");
    }

    #[tokio::test]
    async fn test_create_worktree_matches_base_head() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_git_repo(&repo);
        // Advance HEAD so the base is not the initial commit.
        std::fs::write(repo.join("second.txt"), "two").unwrap();
        let status = std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(status.success());
        let status = std::process::Command::new("git")
            .args(["commit", "-m", "second"])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(status.success());
        let expected_head = head_oid(&repo);

        let created = create_worktree(request_for(&repo), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(created.head_oid, expected_head);
    }

    #[tokio::test]
    async fn test_repository_subdirectory_input_discovers_root() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let subdir = repo.join("src").join("sub");
        std::fs::create_dir_all(&subdir).unwrap();
        init_git_repo(&repo);

        let created = create_worktree(request_for(&subdir), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(created.repository_root, repo.canonicalize().unwrap());
        assert!(created.worktree_path.is_dir());
    }

    #[tokio::test]
    async fn test_unborn_repository_fails_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("empty");
        std::fs::create_dir(&repo).unwrap();
        let status = std::process::Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(status.success());

        let mut before: Vec<String> = std::fs::read_dir(&repo)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        before.sort();
        let result = create_worktree(request_for(&repo), CancellationToken::new()).await;
        assert!(
            matches!(result, Err(WorktreeError::GitCommandFailed { .. })),
            "unborn HEAD must fail, got {result:?}"
        );
        // Nothing invented, nothing removed.
        let mut after: Vec<String> = std::fs::read_dir(&repo)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        after.sort();
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn test_path_with_spaces_needs_no_shell_quoting() {
        let outer = tempfile::tempdir().unwrap();
        let spaced = outer.path().join("dir with spaces");
        let repo = spaced.join("repo here");
        std::fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo);

        let created = create_worktree(request_for(&repo), CancellationToken::new())
            .await
            .unwrap();
        assert!(created.worktree_path.is_dir());
        assert!(created.worktree_path.exists());
    }

    #[tokio::test]
    async fn test_precancelled_token_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_git_repo(&repo);

        let token = CancellationToken::new();
        token.cancel();
        let result = create_worktree(request_for(&repo), token).await;
        assert!(matches!(result, Err(WorktreeError::Cancelled)));

        // No worktree registered and no doge/ branch left behind.
        let output = std::process::Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&repo)
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let non_empty = stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count();
        assert_eq!(non_empty, 3, "only the main worktree may exist");
        let branches = std::process::Command::new("git")
            .args(["branch", "--list", "doge/*"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            String::from_utf8(branches.stdout)
                .unwrap()
                .trim()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_run_git_precancelled_maps_to_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let token = CancellationToken::new();
        token.cancel();
        let result = run_git(
            dir.path(),
            &["rev-parse", "--show-toplevel"],
            "test-op",
            Some(Duration::from_secs(5)),
            &token,
        )
        .await;
        assert!(matches!(result, Err(WorktreeError::Cancelled)));
    }

    #[tokio::test]
    async fn test_run_git_timeout_maps_to_timed_out() {
        // `git` itself has no sleep subcommand; exercise the timeout path
        // through a rev-parse against a real repo with an absurdly small
        // budget is racy, so assert the mapping contract on the timeout
        // option plumbing instead: a 1ns budget against any spawn must not
        // report success as a clean exit.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_git_repo(&repo);
        let token = CancellationToken::new();
        // Direct runner check with an impossible budget proves the managed
        // runner reports timeout; the worktree layer maps it via
        // `map_git_output` (covered above without races).
        let output = run_managed_process(
            ManagedProcessSpec::new("sleep", vec!["30".to_string()], repo.clone()),
            ManagedRunOptions::new(Some(Duration::from_nanos(1))).with_cancellation(Some(token)),
        )
        .await
        .unwrap();
        assert_eq!(output.termination, ManagedProcessTermination::TimedOut);
        assert!(matches!(
            map_git_output("test-op", output),
            Err(WorktreeError::TimedOut { .. })
        ));
    }

    #[test]
    fn test_same_worktree_path_raw_and_canonical() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("wt");
        std::fs::create_dir(&real).unwrap();
        // Exact spelling matches without touching the filesystem.
        assert!(same_worktree_path(&real, &real));
        assert!(!same_worktree_path(&real, &dir.path().join("other")));
        // Canonical equality covers symlink spelling differences.
        #[cfg(unix)]
        {
            let link = dir.path().join("wt-link");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert!(same_worktree_path(&link, &real));
        }
    }

    #[test]
    fn test_is_generated_path_containment() {
        let outer = tempfile::tempdir().unwrap();
        let repo = outer.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let canonical = repo.canonicalize().unwrap();
        let inside = worktree_base_dir(&canonical).join("some-id");
        assert!(is_generated_path(&inside, &canonical));
        assert!(!is_generated_path(&canonical.join("elsewhere"), &canonical));
        assert!(!is_generated_path(
            Path::new("/tmp/doge-code/worktrees/other/id"),
            &canonical
        ));
    }
}
