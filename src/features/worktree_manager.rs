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
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
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

/// Best-effort cleanup of this invocation's generated resources. Only the
/// generated path (inside this invocation's storage area) and the generated
/// `doge/` branch are ever candidates; anything else is left untouched.
/// Never prunes globally. Returns human-readable notes for leftovers or
/// cleanup failures; an empty vector means nothing remained.
///
/// Cleanup uses a fresh cancellation token: it must still run when the
/// original operation was cancelled, bounded by the same timeout.
async fn cleanup_generated_worktree(
    repo_root: &Path,
    generated_path: &Path,
    generated_branch: &str,
    timeout: Option<Duration>,
) -> Vec<String> {
    let mut notes = Vec::new();
    if !is_generated_branch(generated_branch) || !is_generated_path(generated_path, repo_root) {
        notes.push(format!(
            "cleanup refused: {} / {} is outside this invocation's generated identity",
            generated_path.display(),
            generated_branch
        ));
        return notes;
    }
    let fresh = CancellationToken::new();
    let expected_ref = format!("refs/heads/{generated_branch}");

    let records = match list_worktrees(repo_root, "cleanup-list", timeout, &fresh).await {
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
            timeout,
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
    let records = match list_worktrees(repo_root, "cleanup-list", timeout, &fresh).await {
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
        timeout,
        &fresh,
    )
    .await
    {
        Ok(_) => {}
        Err(_) => return notes,
    }
    if let Err(error) = run_git(
        repo_root,
        &["branch", "-d", generated_branch],
        "cleanup-branch",
        timeout,
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

/// Attach cleanup notes to the original failure without hiding it. An empty
/// note list returns the original error unchanged, and cancellation/timeout
/// are always returned unchanged so they stay distinguishable for lifecycle
/// mapping (leftover notes are already `tracing::warn!`-logged by the
/// caller and, for the TUI job, re-attached to the cancel message there).
fn with_cleanup_notes(original: WorktreeError, notes: Vec<String>) -> WorktreeError {
    if notes.is_empty() {
        return original;
    }
    if matches!(
        original,
        WorktreeError::Cancelled | WorktreeError::TimedOut { .. }
    ) {
        return original;
    }
    WorktreeError::CleanupFailed {
        original: original.to_string(),
        cleanup: notes.join("; "),
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

    let worktree_path = worktree_path.canonicalize().map_err(|error| {
        WorktreeError::VerificationFailed(format!(
            "cannot canonicalize verified worktree path: {error}"
        ))
    })?;

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
        // Cancellation/timeout stay distinguishable even when cleanup
        // reports leftovers; other failures gain the cleanup context.
        let cancelled = with_cleanup_notes(WorktreeError::Cancelled, vec!["leftover".to_string()]);
        assert!(matches!(cancelled, WorktreeError::Cancelled));
        let timed_out = with_cleanup_notes(
            WorktreeError::TimedOut { operation: "op" },
            vec!["leftover".to_string()],
        );
        assert!(matches!(timed_out, WorktreeError::TimedOut { .. }));
        let failed = with_cleanup_notes(
            WorktreeError::VerificationFailed("bad".to_string()),
            vec!["note".to_string()],
        );
        assert!(matches!(failed, WorktreeError::CleanupFailed { .. }));
        let clean = with_cleanup_notes(WorktreeError::Cancelled, Vec::new());
        assert!(matches!(clean, WorktreeError::Cancelled));
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
