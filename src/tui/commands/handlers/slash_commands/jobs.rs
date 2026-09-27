use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;

/// List running and recent jobs from the shared `JobManager` snapshot.
/// The UI keeps no duplicate job list; this reads the manager directly.
pub fn handle_jobs(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    let snapshots = executor.jobs.snapshots();
    if snapshots.is_empty() {
        ui.push_log("No jobs.");
        return;
    }
    ui.push_log("Jobs:");
    for snapshot in snapshots.iter().take(50) {
        let mut line = snapshot.display_line();
        if let Some(error) = &snapshot.error {
            line.push_str(&format!("  error: {error}"));
        }
        ui.push_log(format!("  {line}"));
    }
    ui.dirty = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::jobs::{JobKind, JobRunOutcome, JobScope, JobSpec, WorkspaceAccess};
    use crate::tui::state::LogEntry;

    fn log_text(app: &TuiApp) -> String {
        app.log
            .iter()
            .map(|entry| match entry {
                LogEntry::Plain(text) | LogEntry::Markdown(text) => text.as_str(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn log_contains(app: &TuiApp, needle: &str) -> bool {
        app.log.iter().any(|entry| match entry {
            LogEntry::Plain(text) | LogEntry::Markdown(text) => text.contains(needle),
        })
    }

    fn test_executor() -> (TuiExecutor, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = AppConfig {
            project_root: dir.path().to_path_buf(),
            ..Default::default()
        };
        let executor = TuiExecutor::new(cfg).unwrap();
        // Return the tempdir alongside to keep project_root valid.
        (executor, dir)
    }

    fn test_app() -> TuiApp {
        TuiApp::new("test", None, "dark").unwrap()
    }

    #[tokio::test]
    async fn test_jobs_empty_message() {
        let (mut executor, _dir) = test_executor();
        let mut app = test_app();
        handle_jobs(&mut executor, &mut app);
        assert!(log_contains(&app, "No jobs"));
    }

    #[tokio::test]
    async fn test_jobs_lists_running_and_terminal() {
        let (mut executor, _dir) = test_executor();
        let mut app = test_app();
        let running = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::AgentTurn,
                    JobScope::Foreground,
                    WorkspaceAccess::None,
                    "Fix login handler",
                ),
                |ctx| async move {
                    tokio::select! {
                        _ = ctx.cancellation.cancelled() => JobRunOutcome::Cancelled,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => JobRunOutcome::Completed,
                    }
                },
            )
            .unwrap();
        let done = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::Test,
                    JobScope::Background,
                    WorkspaceAccess::None,
                    "Run project tests",
                ),
                |_ctx| async { JobRunOutcome::Completed },
            )
            .unwrap();
        for _ in 0..200 {
            if let Some(snapshot) = executor.jobs.get_snapshot(done)
                && snapshot.status.is_terminal()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        handle_jobs(&mut executor, &mut app);
        let log = log_text(&app);
        assert!(log.contains(&running.to_string()));
        assert!(log.contains("agent_turn"));
        assert!(log.contains("running"));
        assert!(log.contains("Fix login handler"));
        assert!(log.contains(&done.to_string()));
        assert!(log.contains("completed"));
        executor.jobs.cancel(running);
    }
}
