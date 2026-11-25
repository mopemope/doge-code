//! Projects and workspaces: registered directories and the checkouts agents
//! run in (the project directory itself, or a dgc-owned git worktree).
//!
//! Deleting a worktree is destructive, so removal is gated three ways: the
//! workspace must be `owned`, must be a worktree, and its path must resolve to
//! a location strictly inside the daemon's managed worktrees directory
//! ([`is_managed_path`]). Anything else is archived without touching disk, as
//! is a worktree whose project directory no longer exists.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use dgc_protocol::ids::{ProjectId, WorkspaceId};
use dgc_protocol::methods::{WorkspaceArchiveResult, WorkspaceCreateParams};
use dgc_protocol::model::{Isolation, Project, Workspace, WorkspaceState};
use dgc_protocol::rpc::{INTERNAL, INVALID_PARAMS, INVALID_STATE, NOT_FOUND, RpcError};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::paths::ensure_private_dir;
use crate::store::{Store, StoreError};
use crate::time::now_rfc3339;

/// Upper bound for one git invocation.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Largest stderr excerpt put into an error message, in bytes.
pub const STDERR_TAIL_BYTES: usize = 4096;

/// Longest accepted workspace directory name, in bytes.
const SLUG_MAX_BYTES: usize = 128;

/// Longest accepted branch name or base ref, in bytes.
const REF_MAX_BYTES: usize = 255;

/// Trailing hex characters of the project id's uuid used in the
/// per-repository directory name.
const REPO_KEY_ID_CHARS: usize = 12;

/// Failure of a project or workspace operation. Messages never contain file
/// contents.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    InvalidParams(String),
    #[error("{0}")]
    InvalidState(String),
    #[error("{0}")]
    Internal(String),
}

impl WorkspaceError {
    /// JSON-RPC error code for this failure.
    pub fn code(&self) -> i32 {
        match self {
            Self::NotFound(_) => NOT_FOUND,
            Self::InvalidParams(_) => INVALID_PARAMS,
            Self::InvalidState(_) => INVALID_STATE,
            Self::Internal(_) => INTERNAL,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::NotFound(m)
            | Self::InvalidParams(m)
            | Self::InvalidState(m)
            | Self::Internal(m) => m,
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidParams(message.into())
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }
}

impl From<WorkspaceError> for RpcError {
    fn from(err: WorkspaceError) -> Self {
        RpcError::new(err.code(), err.message())
    }
}

fn store_error(err: StoreError) -> WorkspaceError {
    tracing::error!(%err, "state store failure");
    WorkspaceError::internal("state store failure")
}

// --- path safety -----------------------------------------------------------

/// True when `path` lies strictly inside `dir` with at least two components
/// below it (`<repo-key>/<slug>`), and neither contains `.` or `..`. Purely
/// lexical.
fn lexically_inside(path: &Path, dir: &Path) -> bool {
    let plain = |p: &Path| {
        p.is_absolute()
            && p.components()
                .all(|c| !matches!(c, Component::ParentDir | Component::CurDir))
    };
    if !plain(path) || !plain(dir) {
        return false;
    }
    match path.strip_prefix(dir) {
        // `strip_prefix` compares whole components, so `worktrees-evil` does
        // not match `worktrees`.
        Ok(rest) => {
            rest.components().count() >= 2
                && rest.components().all(|c| matches!(c, Component::Normal(_)))
        }
        Err(_) => false,
    }
}

/// Canonicalize `path`; if its tail does not exist yet, canonicalize the
/// longest existing ancestor and re-append the missing names. `None` when the
/// path cannot be resolved safely (dangling symlink, I/O error).
fn resolve_with_missing_tail(path: &Path) -> Option<PathBuf> {
    let mut missing: Vec<&OsStr> = Vec::new();
    let mut current = path;
    loop {
        match fs::canonicalize(current) {
            Ok(mut resolved) => {
                resolved.extend(missing.iter().rev());
                return Some(resolved);
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                // A dangling symlink also reports NotFound; refuse it rather
                // than treating its (unknown) target as a plain missing name.
                if fs::symlink_metadata(current).is_ok() {
                    return None;
                }
                missing.push(current.file_name()?);
                current = current.parent()?;
            }
            Err(_) => return None,
        }
    }
}

/// Whether `path` is a location dgc may delete: strictly inside the managed
/// worktrees directory, both lexically (no `..`, component-wise prefix) and
/// after resolving symlinks.
pub fn is_managed_path(path: &Path, worktrees_dir: &Path) -> bool {
    if !lexically_inside(path, worktrees_dir) {
        return false;
    }
    match (
        resolve_with_missing_tail(path),
        resolve_with_missing_tail(worktrees_dir),
    ) {
        (Some(path), Some(dir)) => lexically_inside(&path, &dir),
        _ => false,
    }
}

// --- name rules ------------------------------------------------------------

/// Replace every character other than ASCII alphanumerics and `-_.` with `_`.
fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn validate_slug(slug: &str) -> Result<(), WorkspaceError> {
    let ok = !slug.is_empty()
        && slug.len() <= SLUG_MAX_BYTES
        && !slug.starts_with('.')
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(WorkspaceError::invalid(format!(
            "slug must be 1-{SLUG_MAX_BYTES} characters of letters, digits, '-', '_' or '.', not starting with '.'"
        )))
    }
}

/// Cheap checks that keep a user-supplied name from being read as a git
/// option; git's own `check-ref-format` does the rest.
fn validate_ref_text(what: &str, text: &str) -> Result<(), WorkspaceError> {
    let ok = !text.is_empty()
        && text.len() <= REF_MAX_BYTES
        && !text.starts_with('-')
        && !text.chars().any(char::is_control);
    if ok {
        Ok(())
    } else {
        Err(WorkspaceError::invalid(format!("invalid {what}")))
    }
}

fn validate_branch_text(branch: &str) -> Result<(), WorkspaceError> {
    validate_ref_text("branch name", branch)?;
    if branch == "HEAD" {
        return Err(WorkspaceError::invalid("invalid branch name"));
    }
    Ok(())
}

// --- git -------------------------------------------------------------------

struct GitOutput {
    success: bool,
    stdout: String,
    stderr: String,
}

/// Longest suffix of `text` that is at most `max` bytes, on a char boundary.
fn tail(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// Run `program` with a closed stdin and wait at most `limit`. On timeout the
/// child is killed (`kill_on_drop`) and an error returned.
async fn run_process(
    program: &str,
    cwd: &Path,
    args: &[&OsStr],
    limit: Duration,
) -> Result<GitOutput, WorkspaceError> {
    let child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        // Never block on a credential prompt, and ignore repository-selecting
        // variables inherited from whatever started the daemon.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| {
            tracing::error!(%err, program, "cannot start process");
            WorkspaceError::internal(format!("cannot start {program}: {}", err.kind()))
        })?;
    match timeout(limit, child.wait_with_output()).await {
        Ok(Ok(output)) => Ok(GitOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }),
        Ok(Err(err)) => Err(WorkspaceError::internal(format!(
            "{program} did not finish: {}",
            err.kind()
        ))),
        Err(_) => Err(WorkspaceError::internal(format!(
            "{program} timed out after {} s",
            limit.as_secs()
        ))),
    }
}

async fn git(cwd: &Path, args: &[&OsStr]) -> Result<GitOutput, WorkspaceError> {
    run_process("git", cwd, args, GIT_TIMEOUT).await
}

fn os(text: &str) -> &OsStr {
    OsStr::new(text)
}

/// Turn a failed git run into an INTERNAL error with a bounded stderr tail.
fn git_failure(action: &str, output: &GitOutput) -> WorkspaceError {
    let detail = tail(output.stderr.trim(), STDERR_TAIL_BYTES);
    WorkspaceError::internal(format!("{action} failed: {detail}"))
}

// --- manager ---------------------------------------------------------------

/// Registers projects and creates/archives their workspaces. Mutating
/// operations are serialized so that, for example, `remove_project` cannot
/// race `create_workspace`.
pub struct WorkspaceManager {
    store: Arc<Store>,
    worktrees_dir: PathBuf,
    write_lock: Mutex<()>,
}

impl WorkspaceManager {
    pub fn new(store: Arc<Store>, worktrees_dir: PathBuf) -> Self {
        Self {
            store,
            worktrees_dir,
            write_lock: Mutex::new(()),
        }
    }

    /// Register the directory at `path` (absolute; symlinks are resolved).
    /// Registering the same directory again returns the existing project.
    pub async fn add_project(&self, path: &Path) -> Result<Project, WorkspaceError> {
        if !path.is_absolute() {
            return Err(WorkspaceError::invalid("project path must be absolute"));
        }
        let canonical = fs::canonicalize(path).map_err(|err| {
            WorkspaceError::invalid(format!("cannot resolve project path: {}", err.kind()))
        })?;
        if !canonical.is_dir() {
            return Err(WorkspaceError::invalid("project path is not a directory"));
        }
        let _guard = self.write_lock.lock().await;
        let existing = self.store.list_projects().map_err(store_error)?;
        if let Some(found) = existing.into_iter().find(|p| p.path == canonical) {
            return Ok(found);
        }
        let name = canonical
            .file_name()
            .map_or_else(|| "root".to_owned(), |n| n.to_string_lossy().into_owned());
        let project = Project {
            id: ProjectId::generate(),
            path: canonical,
            name,
            created_at: now_rfc3339(),
        };
        self.store.put_project(&project).map_err(store_error)?;
        Ok(project)
    }

    pub async fn list_projects(&self) -> Result<Vec<Project>, WorkspaceError> {
        self.store.list_projects().map_err(store_error)
    }

    /// Unregister a project. Refused while it has active workspaces. The
    /// directory itself is never touched.
    pub async fn remove_project(&self, id: &ProjectId) -> Result<(), WorkspaceError> {
        let _guard = self.write_lock.lock().await;
        if self.store.get_project(id).map_err(store_error)?.is_none() {
            return Err(WorkspaceError::NotFound(format!("unknown project {id}")));
        }
        let active = self
            .store
            .list_workspaces()
            .map_err(store_error)?
            .into_iter()
            .filter(|w| &w.project_id == id && w.state == WorkspaceState::Active)
            .count();
        if active > 0 {
            return Err(WorkspaceError::InvalidState(format!(
                "project {id} has {active} active workspace(s); archive them first"
            )));
        }
        self.store.remove_project(id).map_err(store_error)?;
        Ok(())
    }

    /// Active workspaces, optionally only those of one project.
    pub async fn list_workspaces(
        &self,
        project: Option<&ProjectId>,
    ) -> Result<Vec<Workspace>, WorkspaceError> {
        Ok(self
            .store
            .list_workspaces()
            .map_err(store_error)?
            .into_iter()
            .filter(|w| w.state == WorkspaceState::Active)
            .filter(|w| project.is_none_or(|p| &w.project_id == p))
            .collect())
    }

    pub async fn create_workspace(
        &self,
        params: WorkspaceCreateParams,
    ) -> Result<Workspace, WorkspaceError> {
        let _guard = self.write_lock.lock().await;
        let project = self
            .store
            .get_project(&params.project_id)
            .map_err(store_error)?
            .ok_or_else(|| {
                WorkspaceError::NotFound(format!("unknown project {}", params.project_id))
            })?;
        let workspace = match params.isolation {
            Isolation::Local => {
                if params.branch.is_some() || params.base_ref.is_some() || params.slug.is_some() {
                    return Err(WorkspaceError::invalid(
                        "branch, base_ref and slug apply only to worktree workspaces",
                    ));
                }
                new_workspace(
                    &project,
                    Isolation::Local,
                    project.path.clone(),
                    None,
                    None,
                    false,
                )
            }
            Isolation::Worktree => self.create_worktree(&project, &params).await?,
        };
        if let Err(err) = self.store.put_workspace(&workspace) {
            if workspace.owned {
                // Do not leave an unrecorded worktree behind.
                if let Err(cleanup) = self.remove_owned_worktree(&project, &workspace).await {
                    tracing::warn!(error = %cleanup, "could not roll back worktree after store failure");
                }
            }
            return Err(store_error(err));
        }
        Ok(workspace)
    }

    async fn create_worktree(
        &self,
        project: &Project,
        params: &WorkspaceCreateParams,
    ) -> Result<Workspace, WorkspaceError> {
        let branch = params
            .branch
            .as_deref()
            .ok_or_else(|| WorkspaceError::invalid("a worktree workspace requires a branch"))?;
        validate_branch_text(branch)?;
        let slug = match params.slug.as_deref() {
            Some(slug) => slug.to_owned(),
            None => sanitize(branch),
        };
        validate_slug(&slug)?;
        let base = params.base_ref.as_deref().unwrap_or("HEAD");
        validate_ref_text("base ref", base)?;

        let root = fs::canonicalize(&project.path)
            .ok()
            .filter(|p| p.is_dir())
            .ok_or_else(|| WorkspaceError::InvalidState("project directory is gone".into()))?;
        let toplevel = git(&root, &[os("rev-parse"), os("--show-toplevel")]).await?;
        let is_root = toplevel.success
            && fs::canonicalize(toplevel.stdout.trim()).is_ok_and(|top| top == root);
        if !is_root {
            return Err(WorkspaceError::invalid(if toplevel.success {
                "not a git repository root (the project is a subdirectory of one)"
            } else {
                "not a git repository"
            }));
        }

        let full_ref = format!("refs/heads/{branch}");
        let format_check = git(&root, &[os("check-ref-format"), os(&full_ref)]).await?;
        if !format_check.success {
            return Err(WorkspaceError::invalid("invalid branch name"));
        }
        let exists = git(
            &root,
            &[os("show-ref"), os("--verify"), os("--quiet"), os(&full_ref)],
        )
        .await?;
        if exists.success {
            return Err(WorkspaceError::invalid(format!(
                "branch {branch} already exists"
            )));
        }
        let commit_ref = format!("{base}^{{commit}}");
        let resolved = git(
            &root,
            &[
                os("rev-parse"),
                os("--verify"),
                os("--quiet"),
                os(&commit_ref),
            ],
        )
        .await?;
        let commit = resolved.stdout.trim().to_owned();
        if !resolved.success || commit.is_empty() {
            return Err(WorkspaceError::invalid(format!(
                "base ref {base} does not resolve to a commit"
            )));
        }

        let key = repo_key(project);
        ensure_private_dir(&self.worktrees_dir).map_err(|err| {
            tracing::error!(%err, "cannot prepare the worktrees directory");
            WorkspaceError::internal("cannot prepare the worktrees directory")
        })?;
        let repo_dir = self.worktrees_dir.join(&key);
        ensure_private_dir(&repo_dir).map_err(|err| {
            tracing::error!(%err, "cannot prepare the repository worktree directory");
            WorkspaceError::internal("cannot prepare the repository worktree directory")
        })?;
        let path = repo_dir.join(&slug);
        if !is_managed_path(&path, &self.worktrees_dir) {
            // Cannot happen for validated names; never create outside.
            return Err(WorkspaceError::internal("worktree path is not managed"));
        }
        if fs::symlink_metadata(&path).is_ok() {
            return Err(WorkspaceError::invalid(format!(
                "workspace path for slug {slug} already exists"
            )));
        }

        let added = git(
            &root,
            &[
                os("worktree"),
                os("add"),
                os("-b"),
                os(branch),
                path.as_os_str(),
                os(&commit),
            ],
        )
        .await?;
        if !added.success {
            return Err(git_failure("git worktree add", &added));
        }
        Ok(new_workspace(
            project,
            Isolation::Worktree,
            path,
            Some(branch.to_owned()),
            Some(commit),
            true,
        ))
    }

    /// Check, without changing anything, that [`Self::archive_workspace`]
    /// with the same `force` would not refuse to remove the workspace's
    /// worktree. The handler calls this before archiving the workspace's
    /// agents so that a refusal leaves everything untouched.
    pub async fn check_archive(&self, id: &WorkspaceId, force: bool) -> Result<(), WorkspaceError> {
        let _guard = self.write_lock.lock().await;
        let workspace = self.get_workspace(id)?;
        if workspace.state == WorkspaceState::Archived {
            return Ok(());
        }
        self.plan_removal(&workspace, force).await.map(|_| ())
    }

    /// Mark the workspace archived. For an owned worktree inside the managed
    /// directory the checkout and its branch are removed first. Without
    /// `force` that is refused (`INVALID_STATE`, nothing changed) while the
    /// worktree has uncommitted or untracked changes or commits not
    /// reachable from the recorded base, and the branch is deleted only with
    /// git's safe `branch -d` (kept, and reported, if git refuses). If
    /// removing the checkout fails nothing is archived. Any other workspace
    /// keeps its directory. Archiving an archived workspace returns it
    /// unchanged.
    pub async fn archive_workspace(
        &self,
        id: &WorkspaceId,
        force: bool,
    ) -> Result<WorkspaceArchiveResult, WorkspaceError> {
        let _guard = self.write_lock.lock().await;
        let mut workspace = self.get_workspace(id)?;
        if workspace.state == WorkspaceState::Archived {
            return Ok(WorkspaceArchiveResult {
                workspace,
                worktree_removed: false,
                branch_kept: false,
            });
        }
        // The caller (`handlers::workspace_archive`) has archived the
        // workspace's agents already.
        let (worktree_removed, branch_kept) = match self.plan_removal(&workspace, force).await? {
            Removal::KeepOnDisk => (false, workspace.branch.is_some()),
            Removal::Remove(plan) => {
                let branch_kept = if force {
                    discard_worktree(&plan, &workspace).await?
                } else {
                    remove_clean_worktree(&plan, &workspace).await?
                };
                (true, branch_kept)
            }
        };
        workspace.state = WorkspaceState::Archived;
        self.store.put_workspace(&workspace).map_err(store_error)?;
        Ok(WorkspaceArchiveResult {
            workspace,
            worktree_removed,
            branch_kept,
        })
    }

    fn get_workspace(&self, id: &WorkspaceId) -> Result<Workspace, WorkspaceError> {
        self.store
            .get_workspace(id)
            .map_err(store_error)?
            .ok_or_else(|| WorkspaceError::NotFound(format!("unknown workspace {id}")))
    }

    /// Decide what archiving `workspace` does on disk. Only an owned worktree
    /// inside the managed directory is ever removed. Without `force`, an
    /// archive that would lose work is refused here.
    async fn plan_removal(
        &self,
        workspace: &Workspace,
        force: bool,
    ) -> Result<Removal, WorkspaceError> {
        if !workspace.owned || workspace.isolation != Isolation::Worktree {
            return Ok(Removal::KeepOnDisk);
        }
        if !is_managed_path(&workspace.path, &self.worktrees_dir) {
            tracing::warn!(workspace = %workspace.id, "owned workspace is outside the managed directory; keeping it on disk");
            return Ok(Removal::KeepOnDisk);
        }
        let project = self
            .store
            .get_project(&workspace.project_id)
            .map_err(store_error)?
            .ok_or_else(|| {
                WorkspaceError::InvalidState("the workspace's project is missing".into())
            })?;
        if !project.path.is_dir() {
            // The repository was deleted or moved: git cannot be run for it,
            // so archive the record only and leave the files for manual
            // cleanup.
            tracing::warn!(
                workspace = %workspace.id,
                project = %project.id,
                "project directory is gone; archiving the workspace without touching disk"
            );
            return Ok(Removal::KeepOnDisk);
        }
        let plan = inspect_worktree(&project, workspace).await?;
        if !force {
            ensure_nothing_lost(&plan, workspace).await?;
        }
        Ok(Removal::Remove(plan))
    }

    /// Remove a just-created worktree again (store failure in
    /// `create_workspace`). It holds no work, so the safe path suffices.
    async fn remove_owned_worktree(
        &self,
        project: &Project,
        workspace: &Workspace,
    ) -> Result<(), WorkspaceError> {
        let plan = inspect_worktree(project, workspace).await?;
        remove_clean_worktree(&plan, workspace).await.map(|_| ())
    }
}

/// What archiving does on disk.
enum Removal {
    /// Archive the record only.
    KeepOnDisk,
    /// Remove the owned worktree (and maybe its branch) first.
    Remove(RemovalPlan),
}

/// The git state of an owned, managed worktree that is about to be removed.
struct RemovalPlan {
    /// The project's repository (git's working directory for removal).
    repo: PathBuf,
    /// Whether the worktree directory exists.
    exists: bool,
    /// Whether git has a registration for exactly this path.
    registered: bool,
    /// The recorded branch, when git's registration for this very path
    /// still names it: the only branch dgc may delete.
    branch_to_delete: Option<String>,
}

impl RemovalPlan {
    /// Whether `git worktree remove` must run: the directory exists, or git
    /// has a stale entry for exactly this path. With neither there is
    /// nothing of ours left to remove.
    fn needs_git_remove(&self) -> bool {
        self.exists || self.registered
    }
}

/// Read git's registration for `workspace` and whether its directory exists.
/// Only this workspace's own git state is considered: never a
/// repository-wide prune, and the branch qualifies for deletion only when
/// git's registration for this very path still names the recorded branch.
async fn inspect_worktree(
    project: &Project,
    workspace: &Workspace,
) -> Result<RemovalPlan, WorkspaceError> {
    let repo = project.path.clone();
    let listed = git(
        &repo,
        &[os("worktree"), os("list"), os("--porcelain"), os("-z")],
    )
    .await?;
    if !listed.success {
        return Err(git_failure("git worktree list", &listed));
    }
    let entry = registered_branch(&listed.stdout, &workspace.path);
    let branch_to_delete = match (workspace.branch.as_deref(), &entry) {
        (Some(recorded), Some(Some(current)))
            if validate_branch_text(recorded).is_ok()
                && current.strip_prefix("refs/heads/") == Some(recorded) =>
        {
            Some(recorded.to_owned())
        }
        (Some(_), _) => {
            tracing::warn!(
                workspace = %workspace.id,
                "recorded branch is not checked out in this worktree; keeping the branch"
            );
            None
        }
        (None, _) => None,
    };
    let exists = match fs::symlink_metadata(&workspace.path) {
        Ok(_) => true,
        Err(err) if err.kind() == io::ErrorKind::NotFound => false,
        Err(err) => {
            return Err(WorkspaceError::internal(format!(
                "cannot inspect the worktree: {}",
                err.kind()
            )));
        }
    };
    Ok(RemovalPlan {
        repo,
        exists,
        registered: entry.is_some(),
        branch_to_delete,
    })
}

/// Refuse (`INVALID_STATE`) when removing the worktree and its branch would
/// lose work: uncommitted or untracked changes in the worktree, or commits
/// on its HEAD or on the branch to delete that are not reachable from the
/// recorded base commit.
async fn ensure_nothing_lost(
    plan: &RemovalPlan,
    workspace: &Workspace,
) -> Result<(), WorkspaceError> {
    let mut reasons: Vec<String> = Vec::new();
    // A worktree whose directory exists and is registered for this path has
    // its own `.git` file, so git inside it inspects exactly this checkout.
    let inspect_checkout = plan.exists && plan.registered;
    if inspect_checkout {
        let status = git(
            &workspace.path,
            &[os("status"), os("--porcelain"), os("-z")],
        )
        .await?;
        if !status.success {
            return Err(git_failure("git status", &status));
        }
        if !status.stdout.is_empty() {
            reasons.push("uncommitted changes".to_owned());
        }
    }

    let mut tips: Vec<String> = Vec::new();
    if inspect_checkout {
        tips.push("HEAD".to_owned());
    }
    if let Some(branch) = &plan.branch_to_delete {
        tips.push(format!("refs/heads/{branch}"));
    }
    if !tips.is_empty() {
        let base = workspace
            .base_ref
            .as_deref()
            .filter(|base| validate_ref_text("base ref", base).is_ok());
        match base {
            Some(base) => {
                let cwd = if inspect_checkout {
                    workspace.path.as_path()
                } else {
                    plan.repo.as_path()
                };
                let exclude = format!("^{base}");
                let mut args: Vec<&OsStr> = vec![os("rev-list"), os("--count")];
                args.extend(tips.iter().map(|tip| os(tip)));
                args.push(os(&exclude));
                args.push(os("--"));
                let counted = git(cwd, &args).await?;
                if !counted.success {
                    return Err(git_failure("git rev-list", &counted));
                }
                let ahead: u64 = counted.stdout.trim().parse().map_err(|_| {
                    WorkspaceError::internal("git rev-list printed no commit count")
                })?;
                if ahead > 0 {
                    let noun = if ahead == 1 { "commit" } else { "commits" };
                    reasons.push(format!("{ahead} {noun} not in base"));
                }
            }
            None => reasons.push("no recorded base commit to compare against".to_owned()),
        }
    }

    if reasons.is_empty() {
        return Ok(());
    }
    Err(WorkspaceError::InvalidState(format!(
        "workspace {} has {}; archiving would delete them (archive with force to discard them)",
        workspace.id,
        reasons.join(" and ")
    )))
}

/// The non-destructive removal: `git worktree remove` without `--force`
/// (git itself refuses a dirty checkout) and the safe `git branch -d`.
/// Returns whether the recorded branch still exists afterwards.
async fn remove_clean_worktree(
    plan: &RemovalPlan,
    workspace: &Workspace,
) -> Result<bool, WorkspaceError> {
    let repo = plan.repo.as_path();
    if plan.needs_git_remove() {
        let removed = git(
            repo,
            &[os("worktree"), os("remove"), workspace.path.as_os_str()],
        )
        .await?;
        if !removed.success {
            return Err(git_failure("git worktree remove", &removed));
        }
    }
    if let Some(branch) = &plan.branch_to_delete {
        match git(repo, &[os("branch"), os("-d"), os(branch)]).await {
            Ok(out) if out.success => {}
            Ok(out) => {
                tracing::warn!(workspace = %workspace.id, stderr_bytes = out.stderr.len(), "git refused the safe branch delete; keeping the branch")
            }
            Err(err) => {
                tracing::warn!(workspace = %workspace.id, %err, "could not delete the workspace branch")
            }
        }
    }
    recorded_branch_exists(repo, workspace).await
}

/// The destructive removal, used only when the caller passed `force`:
/// `git worktree remove --force` and `git branch -D`. Still limited to the
/// owned, managed worktree and to the branch git's registration for this
/// path names. Returns whether the recorded branch still exists afterwards.
async fn discard_worktree(
    plan: &RemovalPlan,
    workspace: &Workspace,
) -> Result<bool, WorkspaceError> {
    let repo = plan.repo.as_path();
    if plan.needs_git_remove() {
        let removed = git(
            repo,
            &[
                os("worktree"),
                os("remove"),
                os("--force"),
                workspace.path.as_os_str(),
            ],
        )
        .await?;
        if !removed.success {
            return Err(git_failure("git worktree remove", &removed));
        }
    }
    if let Some(branch) = &plan.branch_to_delete {
        match git(repo, &[os("branch"), os("-D"), os(branch)]).await {
            Ok(out) if out.success => {}
            Ok(out) => {
                tracing::warn!(workspace = %workspace.id, stderr_bytes = out.stderr.len(), "could not delete the workspace branch")
            }
            Err(err) => {
                tracing::warn!(workspace = %workspace.id, %err, "could not delete the workspace branch")
            }
        }
    }
    recorded_branch_exists(repo, workspace).await
}

/// Whether the workspace's recorded branch exists in `repo`. A failed check
/// counts as "exists" (the conservative answer for `branch_kept`).
async fn recorded_branch_exists(
    repo: &Path,
    workspace: &Workspace,
) -> Result<bool, WorkspaceError> {
    let Some(branch) = workspace.branch.as_deref() else {
        return Ok(false);
    };
    if validate_branch_text(branch).is_err() {
        return Ok(true);
    }
    let full_ref = format!("refs/heads/{branch}");
    let shown = git(
        repo,
        &[os("show-ref"), os("--verify"), os("--quiet"), os(&full_ref)],
    )
    .await;
    Ok(shown.map_or(true, |out| out.success))
}

/// In `git worktree list --porcelain -z` output, find the entry for `path`.
/// `None`: no such entry. `Some(None)`: registered with a detached HEAD (or no
/// branch). `Some(Some(ref))`: the full ref (`refs/heads/...`) it has checked out.
fn registered_branch(porcelain: &str, path: &Path) -> Option<Option<String>> {
    let wanted = [Some(path.to_owned()), resolve_with_missing_tail(path)];
    let matches = |entry: &Path| {
        wanted.iter().flatten().any(|w| w == entry)
            || resolve_with_missing_tail(entry)
                .is_some_and(|r| wanted.iter().flatten().any(|w| *w == r))
    };
    let mut found: Option<Option<String>> = None;
    let mut current_matches = false;
    for field in porcelain.split('\0') {
        if let Some(entry) = field.strip_prefix("worktree ") {
            current_matches = matches(Path::new(entry));
            if current_matches {
                found = Some(None);
            }
        } else if let Some(branch) = field.strip_prefix("branch ")
            && current_matches
        {
            found = Some(Some(branch.to_owned()));
        } else if field.is_empty() {
            current_matches = false;
        }
    }
    found
}

fn new_workspace(
    project: &Project,
    isolation: Isolation,
    path: PathBuf,
    branch: Option<String>,
    base_ref: Option<String>,
    owned: bool,
) -> Workspace {
    Workspace {
        id: WorkspaceId::generate(),
        project_id: project.id.clone(),
        isolation,
        path,
        branch,
        base_ref,
        state: WorkspaceState::Active,
        owned,
        created_at: now_rfc3339(),
    }
}

/// `<sanitized basename>-<last 12 hex chars of the project id's uuid>`. The
/// leading hex digits of a uuid v7 are its millisecond timestamp, so the
/// trailing (random) ones are used.
fn repo_key(project: &Project) -> String {
    let base = project
        .path
        .file_name()
        .map_or_else(|| "root".to_owned(), |n| sanitize(&n.to_string_lossy()));
    let id = project.id.as_str();
    let uuid = id.strip_prefix(ProjectId::PREFIX).unwrap_or(id);
    let skip = uuid.chars().count().saturating_sub(REPO_KEY_ID_CHARS);
    let short: String = uuid.chars().skip(skip).collect();
    format!("{base}-{short}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    fn managed(path: &str, dir: &str) -> bool {
        is_managed_path(Path::new(path), Path::new(dir))
    }

    #[test]
    fn accepts_repo_key_slug_below_the_directory() {
        assert!(managed("/data/worktrees/repo-1/slug", "/data/worktrees"));
        assert!(managed(
            "/data/worktrees/repo-1/slug/deeper",
            "/data/worktrees"
        ));
    }

    #[test]
    fn rejects_the_directory_itself_and_the_repo_key_level() {
        assert!(!managed("/data/worktrees", "/data/worktrees"));
        assert!(!managed("/data/worktrees/repo-1", "/data/worktrees"));
        assert!(!managed("/data", "/data/worktrees"));
    }

    #[test]
    fn rejects_a_sibling_with_a_common_string_prefix() {
        assert!(!managed(
            "/data/worktrees-evil/repo/slug",
            "/data/worktrees"
        ));
        assert!(!managed("/data/worktreesX/a/b", "/data/worktrees"));
    }

    #[test]
    fn rejects_parent_and_current_dir_components() {
        assert!(!managed("/data/worktrees/../x/y", "/data/worktrees"));
        assert!(!managed("/data/worktrees/a/../../x", "/data/worktrees"));
        assert!(!managed("/data/worktrees/a/b/..", "/data/worktrees"));
        assert!(!managed(
            "/data/worktrees/../worktrees/a/b",
            "/data/worktrees"
        ));
        assert!(!managed("/data/worktrees/a/b", "/data/worktrees/.."));
    }

    #[test]
    fn rejects_relative_paths() {
        assert!(!managed("worktrees/a/b", "/data/worktrees"));
        assert!(!managed("/data/worktrees/a/b", "worktrees"));
    }

    #[test]
    fn rejects_a_symlink_that_escapes_the_directory() {
        let tmp = tempdir().expect("tempdir");
        let dir = fs::canonicalize(tmp.path())
            .expect("canon")
            .join("worktrees");
        fs::create_dir_all(dir.join("repo")).expect("mkdir");
        let outside = fs::canonicalize(tmp.path()).expect("canon").join("outside");
        fs::create_dir(&outside).expect("mkdir");
        symlink(&outside, dir.join("repo/escape")).expect("symlink");
        symlink(tmp.path().join("missing-target"), dir.join("repo/dangling")).expect("symlink");
        fs::create_dir(dir.join("repo/real")).expect("mkdir");

        assert!(is_managed_path(&dir.join("repo/real"), &dir));
        assert!(!is_managed_path(&dir.join("repo/escape"), &dir));
        assert!(!is_managed_path(&dir.join("repo/escape/inner"), &dir));
        assert!(!is_managed_path(&dir.join("repo/dangling"), &dir));
    }

    #[test]
    fn accepts_missing_paths_under_an_existing_directory() {
        let tmp = tempdir().expect("tempdir");
        let dir = fs::canonicalize(tmp.path())
            .expect("canon")
            .join("worktrees");
        fs::create_dir_all(&dir).expect("mkdir");
        assert!(is_managed_path(&dir.join("repo/not-yet"), &dir));
    }

    #[test]
    fn works_through_a_symlinked_directory() {
        let tmp = tempdir().expect("tempdir");
        let real = fs::canonicalize(tmp.path()).expect("canon").join("real");
        fs::create_dir_all(real.join("repo/slug")).expect("mkdir");
        let link = tmp.path().join("link");
        symlink(&real, &link).expect("symlink");
        assert!(is_managed_path(&link.join("repo/slug"), &link));
    }

    #[test]
    fn destructive_git_flags_appear_only_in_the_force_path() {
        // Structural guard (like the legacy worktree manager's): forced
        // removal and forced branch deletion can discard work, so their
        // flags may appear only inside `discard_worktree`, which is called
        // only when the caller passed `force`. Tokens are built at runtime
        // so this test's own text cannot match.
        let source = include_str!("workspace.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        let flag = |text: &str| format!("os(\"{text}\")");
        let force_flag = flag(&["--", "force"].concat());
        let force_delete = flag(&["-", "D"].concat());
        let prune = format!("\"{}\"", ["pr", "une"].concat());
        assert!(!prod.contains(&prune), "no repository-wide prune");

        let def = ["async fn ", "discard_worktree", "("].concat();
        let start = prod.find(&def).expect("discard_worktree is defined");
        let end = prod[start + def.len()..]
            .find("\nasync fn ")
            .or_else(|| prod[start + def.len()..].find("\nfn "))
            .map_or(prod.len(), |offset| start + def.len() + offset);
        for token in [&force_flag, &force_delete] {
            let positions: Vec<usize> =
                prod.match_indices(token.as_str()).map(|(i, _)| i).collect();
            assert_eq!(positions.len(), 1, "{token} must appear exactly once");
            assert!(
                positions.iter().all(|&i| i > start && i < end),
                "{token} must appear only inside discard_worktree"
            );
        }
        let safe_delete = format!("{}, {}", flag("branch"), flag(&["-", "d"].concat()));
        assert!(
            prod.contains(&safe_delete),
            "the safe path uses `branch -d`"
        );

        let call = ["discard_worktree", "("].concat();
        let calls: Vec<usize> = prod
            .match_indices(&call)
            .map(|(i, _)| i)
            .filter(|&i| !prod[..i].ends_with("async fn "))
            .collect();
        assert_eq!(calls.len(), 1, "discard_worktree has exactly one caller");
        let guard = ["if ", "force", " {"].concat();
        let before = &prod[calls[0].saturating_sub(200)..calls[0]];
        assert!(
            before.contains(&guard),
            "discard_worktree is called only under `if force`"
        );
    }

    #[test]
    fn sanitize_replaces_unsafe_characters() {
        assert_eq!(sanitize("feature/login fix"), "feature_login_fix");
        assert_eq!(sanitize("a-b_c.d"), "a-b_c.d");
        assert_eq!(sanitize("日本"), "__");
    }

    #[test]
    fn slug_rules() {
        assert!(validate_slug("ok-1_2.3").is_ok());
        for bad in ["", ".", "..", ".x", "a/b", "a b", &"x".repeat(129)] {
            assert!(validate_slug(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn branch_text_rules() {
        assert!(validate_branch_text("feature/x").is_ok());
        for bad in ["", "-D", "--force", "HEAD", "a\nb", &"x".repeat(256)] {
            assert!(validate_branch_text(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn tail_keeps_the_end_on_a_char_boundary() {
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("abc", 10), "abc");
        // 3-byte chars: a cut at 4 bytes must move forward to a boundary.
        let cut = tail("ああああ", 4);
        assert_eq!(cut, "あ");
        let long = "x".repeat(10_000);
        assert_eq!(tail(&long, STDERR_TAIL_BYTES).len(), STDERR_TAIL_BYTES);
    }

    #[test]
    fn repo_key_uses_basename_and_uuid_prefix() {
        let project = Project {
            id: ProjectId::generate(),
            path: PathBuf::from("/work/my repo"),
            name: "my repo".into(),
            created_at: String::new(),
        };
        let key = repo_key(&project);
        let uuid = &project.id.as_str()[ProjectId::PREFIX.len()..];
        assert_eq!(key, format!("my_repo-{}", &uuid[uuid.len() - 12..]));
    }

    #[tokio::test]
    async fn a_slow_process_is_killed_at_the_timeout() {
        let tmp = tempdir().expect("tempdir");
        let pid_file = tmp.path().join("pid");
        let script = format!("echo $$ > '{}'; exec sleep 30", pid_file.display());
        let started = std::time::Instant::now();
        let result = run_process(
            "sh",
            tmp.path(),
            &[os("-c"), os(&script)],
            Duration::from_millis(300),
        )
        .await;
        let err = result.err().expect("must time out");
        assert_eq!(err.code(), INTERNAL);
        assert!(err.message().contains("timed out"), "{}", err.message());
        assert!(started.elapsed() < Duration::from_secs(10));

        let pid: i32 = fs::read_to_string(&pid_file)
            .expect("pid file")
            .trim()
            .parse()
            .expect("pid");
        let mut alive = true;
        for _ in 0..100 {
            // SAFETY: signal 0 only probes whether the process exists.
            alive = unsafe { libc::kill(pid, 0) } == 0;
            if !alive {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!alive, "the child must be killed on timeout");
    }

    #[tokio::test]
    async fn run_process_reports_exit_status_and_output() {
        let tmp = tempdir().expect("tempdir");
        let out = run_process(
            "sh",
            tmp.path(),
            &[os("-c"), os("echo out; echo err >&2; exit 3")],
            GIT_TIMEOUT,
        )
        .await
        .expect("runs");
        assert!(!out.success);
        assert_eq!(out.stdout.trim(), "out");
        assert_eq!(out.stderr.trim(), "err");
    }
}
