use crate::jobs::{CancelJobResult, JobId};
use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;

/// Cancel the current foreground job (`/cancel`) or a specific job
/// (`/cancel <id>`, accepting both `12` and `job-12`).
pub fn handle_cancel(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    handle_cancel_with_args(executor, ui, None);
}

pub fn handle_cancel_with_args(executor: &mut TuiExecutor, ui: &mut TuiApp, arg: Option<&str>) {
    if let Some(raw) = arg {
        let raw = raw.trim();
        if raw.is_empty() {
            cancel_foreground(executor, ui);
            return;
        }
        // Only the first token is an id; extra tokens are ignored.
        let id_token = raw.split_whitespace().next().unwrap_or(raw);
        let Some(id) = JobId::parse_arg(id_token) else {
            ui.push_log(format!(
                "Unknown job id: {id_token}. Use /jobs to list jobs."
            ));
            ui.dirty = true;
            return;
        };
        cancel_specific(executor, ui, id);
        return;
    }
    cancel_foreground(executor, ui);
}

fn cancel_foreground(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    let Some(foreground) = executor.jobs.foreground_id() else {
        ui.push_log("[no running task]");
        ui.dirty = true;
        return;
    };
    cancel_specific(executor, ui, foreground);
}

fn cancel_specific(executor: &mut TuiExecutor, ui: &mut TuiApp, id: JobId) {
    // Capture whether the target is the current foreground job before
    // cancelling so background-only cancels never flip foreground UI.
    let is_foreground = executor.jobs.foreground_id() == Some(id);
    match executor.jobs.cancel(id) {
        CancelJobResult::Cancelled { id } => {
            // Only the foreground cancellation drives the global status UI.
            if is_foreground && let Some(tx) = &executor.ui_tx {
                let _ = tx.send("::status:cancelled".into());
            }
            ui.push_log(format!("[Cancelled {id}]"));
        }
        CancelJobResult::AlreadyFinished { id } => {
            ui.push_log(format!("[{id} already finished]"));
        }
        CancelJobResult::NotFound { .. } => {
            ui.push_log("Unknown job id. Use /jobs to list jobs.");
        }
    }
    ui.dirty = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::jobs::{JobKind, JobRunOutcome, JobScope, JobSpec, WorkspaceAccess};
    use crate::tui::state::LogEntry;

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

    fn spawn_blocking(executor: &TuiExecutor) -> JobId {
        executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::AgentTurn,
                    JobScope::Foreground,
                    WorkspaceAccess::Write,
                    "blocking",
                ),
                |ctx| async move {
                    tokio::select! {
                        _ = ctx.cancellation.cancelled() => JobRunOutcome::Cancelled,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => JobRunOutcome::Completed,
                    }
                },
            )
            .unwrap()
    }

    #[test]
    fn test_cancel_foreground_cancels_current_job() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (mut executor, _dir) = test_executor();
            let mut app = test_app();
            executor.set_ui_tx(app.sender());
            let id = spawn_blocking(&executor);
            handle_cancel(&mut executor, &mut app);
            assert!(log_contains(&app, &id.to_string()));
            for _ in 0..200 {
                if let Some(snapshot) = executor.jobs.get_snapshot(id)
                    && snapshot.status.is_terminal()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            assert_eq!(
                executor.jobs.get_snapshot(id).map(|s| s.status),
                Some(crate::jobs::JobStatus::Cancelled)
            );
        });
    }

    #[test]
    fn test_cancel_with_id_forms() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (mut executor, _dir) = test_executor();
            let mut app = test_app();
            executor.set_ui_tx(app.sender());
            let id = spawn_blocking(&executor);
            handle_cancel_with_args(&mut executor, &mut app, Some(&id.to_string()));
            assert!(log_contains(&app, "Cancelled"));
            // Second cancel reports already-finished once the job drains.
            for _ in 0..200 {
                if let Some(snapshot) = executor.jobs.get_snapshot(id)
                    && snapshot.status.is_terminal()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            handle_cancel_with_args(&mut executor, &mut app, Some("job-99999"));
            assert!(log_contains(&app, "Unknown job id"));
        });
    }

    #[test]
    fn test_cancel_numeric_id_accepted() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (mut executor, _dir) = test_executor();
            let mut app = test_app();
            executor.set_ui_tx(app.sender());
            let id = spawn_blocking(&executor);
            handle_cancel_with_args(&mut executor, &mut app, Some(&id.0.to_string()));
            assert!(log_contains(&app, "Cancelled"));
        });
    }

    #[tokio::test]
    async fn test_cancel_unknown_id_reports_useful_error() {
        let (mut executor, _dir) = test_executor();
        let mut app = test_app();
        handle_cancel_with_args(&mut executor, &mut app, Some("job-12345"));
        assert!(log_contains(&app, "Unknown job id"));
        handle_cancel_with_args(&mut executor, &mut app, Some("nope"));
        assert!(log_contains(&app, "Unknown job id"));
    }
}
