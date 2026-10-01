//! `/git-worktree`: create an isolated linked worktree and branch as a
//! JobManager-managed foreground job.
//!
//! The worktree is created through the managed Git process path with an
//! explicit repository context derived from `AppConfig.project_root`. The
//! current Doge session (project root, RepoMap, provenance, tools) is never
//! switched into the new worktree; the success message states this
//! explicitly. No synthetic agent follow-up is spawned.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use tokio_util::sync::CancellationToken;

use crate::features::worktree_manager::{CreateWorktreeRequest, WorktreeError, create_worktree};
use crate::jobs::{JobKind, JobRunOutcome, JobScope, JobSpec, JobStartError, WorkspaceAccess};
use crate::tui::channel::SenderExt;
use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;

/// Create a new git worktree without leaving the current session behind.
pub fn handle_git_worktree(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    // Fast pre-check so a busy rejection never flips the global status and
    // never creates branches, directories, or jobs.
    if let Some(active_id) = executor.jobs.foreground_id()
        && let Some(active) = executor.jobs.get_snapshot(active_id)
    {
        ui.push_log(format!(
            "[Job] {} is already running. Use /jobs or /cancel {}.",
            active.id, active.id
        ));
        return;
    }

    let Some(ui_tx) = executor.ui_tx.clone() else {
        ui.push_log("UI channel unavailable - cannot create worktree.");
        return;
    };
    let project_root = executor.cfg.project_root.clone();
    let command_timeout_ms = executor.cfg.command_timeout_ms;

    let spec = JobSpec::new(
        JobKind::Worktree,
        JobScope::Foreground,
        WorkspaceAccess::Write,
        "Create git worktree",
    );
    let spawn = executor.jobs.spawn(spec, move |ctx| async move {
        run_worktree_job(
            project_root,
            ui_tx,
            command_timeout_ms,
            ctx.cancellation_token(),
        )
        .await
    });
    match spawn {
        Ok(id) => {
            ui.push_log("Creating git worktree...");
            ui.push_log(format!("Worktree job started as {id}."));
        }
        Err(JobStartError::ForegroundBusy { active }) => {
            ui.push_log(format!(
                "[Job] {} is already running. Use /jobs or /cancel {}.",
                active.id, active.id
            ));
        }
        Err(JobStartError::ShuttingDown) => {
            ui.push_log("Job manager is shutting down.");
        }
    }
}

async fn run_worktree_job(
    project_root: PathBuf,
    ui_tx: Sender<String>,
    command_timeout_ms: u64,
    cancellation: CancellationToken,
) -> JobRunOutcome {
    ui_tx.send_logged("::status:shell_running".to_string());
    ui_tx.send_logged(format!(
        "::shell_output:Project root: {}",
        project_root.display()
    ));

    if cancellation.is_cancelled() {
        ui_tx.send_logged("::shell_output:Worktree creation cancelled.".to_string());
        ui_tx.send_logged("::status:cancelled".to_string());
        return JobRunOutcome::Cancelled;
    }

    let original_root = project_root.clone();
    let outcome = create_worktree(
        CreateWorktreeRequest::new(project_root, command_timeout_ms),
        cancellation.clone(),
    )
    .await;

    // A cancellation landing anywhere in the run reports `Cancelled`, even
    // when the error already carries cleanup leftovers (those stay visible
    // in the message). Timeout is an infrastructure failure, not a cancel.
    if cancellation.is_cancelled() {
        match outcome {
            Err(WorktreeError::CleanupFailed { cleanup, .. }) => {
                ui_tx.send_logged("::shell_output:Worktree creation cancelled.".to_string());
                ui_tx.send_logged(format!("::shell_output:Leftovers: {cleanup}"));
            }
            _ => {
                ui_tx.send_logged("::shell_output:Worktree creation cancelled.".to_string());
            }
        }
        ui_tx.send_logged("::status:cancelled".to_string());
        return JobRunOutcome::Cancelled;
    }

    match outcome {
        Ok(created) => {
            ui_tx.send_logged(format!(
                "::shell_output:Worktree created.\nPath: {}\nBranch: {}\nBase: {} ({})",
                created.worktree_path.display(),
                created.branch,
                created.base_ref,
                created.head_oid,
            ));
            ui_tx.send_logged(format!(
                "::shell_output:Current Doge session remains on: {}",
                original_root.display()
            ));
            ui_tx.send_logged("::status:idle".to_string());
            JobRunOutcome::Completed
        }
        Err(WorktreeError::Cancelled) => {
            ui_tx.send_logged("::shell_output:Worktree creation cancelled.".to_string());
            ui_tx.send_logged("::status:cancelled".to_string());
            JobRunOutcome::Cancelled
        }
        Err(error) => {
            ui_tx.send_logged(format!("::shell_output:Worktree creation failed: {error}"));
            ui_tx.send_logged("::status:idle".to_string());
            JobRunOutcome::Failed {
                message: crate::jobs::bound_error(&error.to_string()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{JobManager, JobScope, JobSpec, WorkspaceAccess};
    use crate::tui::view::TuiApp;

    fn init_git_repo(root: &std::path::Path) {
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .expect("git must run");
            assert!(status.success(), "git {args:?} failed");
        };
        run(&["init"]);
        run(&["config", "user.name", "Test User"]);
        run(&["config", "user.email", "test@example.com"]);
        std::fs::write(root.join("README.md"), "test").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "Initial commit"]);
    }

    fn test_executor_for(project_root: &std::path::Path) -> (TuiExecutor, tempfile::TempDir) {
        let hold = tempfile::tempdir().unwrap();
        let cfg = crate::config::AppConfig {
            project_root: project_root.to_path_buf(),
            no_repomap: true,
            ..Default::default()
        };
        let executor = TuiExecutor::new(cfg).unwrap();
        (executor, hold)
    }

    fn log_text(ui: &TuiApp) -> String {
        ui.log
            .iter()
            .map(|entry| match entry {
                crate::tui::state::LogEntry::Plain(text) => text.clone(),
                other => format!("{other:?}"),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn test_worktree_job_success_keeps_original_session_root() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_git_repo(&repo);
        let (ui_tx, rx) = std::sync::mpsc::channel::<String>();
        let outcome = run_worktree_job(repo.clone(), ui_tx, 30_000, CancellationToken::new()).await;
        assert!(matches!(outcome, JobRunOutcome::Completed));
        let messages: Vec<String> = rx.try_iter().collect();
        let joined = messages.join("\n");
        assert!(joined.contains("Worktree created."), "{joined}");
        assert!(
            joined.contains("Current Doge session remains on:"),
            "{joined}"
        );
        assert!(joined.contains(&repo.display().to_string()), "{joined}");
        assert!(messages.iter().any(|m| m == "::status:idle"));
    }

    #[tokio::test]
    async fn test_worktree_job_precancelled_reports_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let (ui_tx, rx) = std::sync::mpsc::channel::<String>();
        let token = CancellationToken::new();
        token.cancel();
        let outcome = run_worktree_job(dir.path().to_path_buf(), ui_tx, 30_000, token).await;
        assert!(matches!(outcome, JobRunOutcome::Cancelled));
        let messages: Vec<String> = rx.try_iter().collect();
        assert!(messages.iter().any(|m| m == "::status:cancelled"));
        assert!(!messages.iter().any(|m| m.contains("Worktree created.")));
    }

    #[tokio::test]
    async fn test_worktree_job_non_repository_reports_failed() {
        let dir = tempfile::tempdir().unwrap();
        let (ui_tx, rx) = std::sync::mpsc::channel::<String>();
        let outcome = run_worktree_job(
            dir.path().to_path_buf(),
            ui_tx,
            30_000,
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(outcome, JobRunOutcome::Failed { .. }));
        let messages: Vec<String> = rx.try_iter().collect();
        let joined = messages.join("\n");
        assert!(joined.contains("Worktree creation failed:"), "{joined}");
        assert!(messages.iter().any(|m| m == "::status:idle"));
    }

    #[tokio::test]
    async fn test_worktree_handler_busy_rejects_without_side_effects() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_git_repo(&repo);
        let (mut executor, _hold) = test_executor_for(&repo);
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());

        // Occupy the foreground slot with a blocking producer.
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        let release_clone = release.clone();
        let blocker = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::AgentTurn,
                    JobScope::Foreground,
                    WorkspaceAccess::Write,
                    "blocker",
                ),
                |ctx| async move {
                    tokio::select! {
                        _ = ctx.cancellation.cancelled() => JobRunOutcome::Cancelled,
                        _ = release_clone.notified() => JobRunOutcome::Completed,
                    }
                },
            )
            .unwrap();

        let jobs_before = executor.jobs.active_count();
        handle_git_worktree(&mut executor, &mut ui);
        release.notify_one();

        // Rejected: the busy message is logged, no worktree job exists, and
        // no branch or directory was created as a side effect.
        let logs = log_text(&ui);
        assert!(logs.contains("is already running"), "{logs}");
        assert_eq!(executor.jobs.active_count(), jobs_before);
        assert_eq!(executor.jobs.foreground_id(), Some(blocker));
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

        executor.jobs.cancel(blocker);
    }

    #[tokio::test]
    async fn test_worktree_job_visible_in_jobs_as_worktree_kind() {
        use crate::jobs::JobStatus;

        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_git_repo(&repo);
        let manager = JobManager::new();
        let root = repo.clone();
        let id = manager
            .spawn(
                JobSpec::new(
                    JobKind::Worktree,
                    JobScope::Foreground,
                    WorkspaceAccess::Write,
                    "Create git worktree",
                ),
                |ctx| async move {
                    let (ui_tx, _rx) = std::sync::mpsc::channel::<String>();
                    run_worktree_job(root, ui_tx, 30_000, ctx.cancellation_token()).await
                },
            )
            .unwrap();
        let snapshot = manager.get_snapshot(id).unwrap();
        assert_eq!(snapshot.kind, JobKind::Worktree);
        assert_eq!(snapshot.kind.to_string(), "worktree");
        for _ in 0..1200 {
            if let Some(done) = manager.get_snapshot(id)
                && done.status.is_terminal()
            {
                assert_eq!(done.status, JobStatus::Completed);
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("worktree job did not terminalize");
    }
}
