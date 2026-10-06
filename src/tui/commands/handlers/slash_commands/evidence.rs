//! Explicit, cancellable read job; never exports to a file or invokes an LLM.
use crate::{
    jobs::{JobKind, JobRunOutcome, JobScope, JobSpec, WorkspaceAccess},
    tui::{commands::core::TuiExecutor, state::Status, view::TuiApp},
};
use std::{path::PathBuf, sync::mpsc::Sender};
use tokio_util::sync::CancellationToken;

pub fn handle_evidence(executor: &mut TuiExecutor, ui: &mut TuiApp, args: &str) {
    if args.split_whitespace().count() > 1 {
        ui.push_log("Usage: /evidence [session-id]");
        return;
    }
    if executor.ensure_session_idle().is_err() {
        ui.push_log(
            "[evidence] Wait for the active job or cancel it before inspecting saved evidence.",
        );
        return;
    }
    let id = if args.is_empty() {
        match crate::utils::safe_std_lock(&executor.session_manager, "session_manager") {
            Ok(manager) => manager.current_session_id(),
            Err(_) => None,
        }
    } else {
        Some(args.to_owned())
    };
    let Some(id) = id else {
        ui.push_log("[evidence] No current session. Select a saved session with /evidence <id>.");
        return;
    };
    let Some(tx) = executor.ui_tx.clone() else {
        ui.push_log("[evidence] UI channel unavailable.");
        return;
    };
    let root = executor.cfg.project_root.clone();
    match executor.jobs.spawn(
        JobSpec::new(
            JobKind::Evidence,
            JobScope::Foreground,
            WorkspaceAccess::ReadOnly,
            "Inspect saved session evidence",
        ),
        move |ctx| async move {
            let (tx, _forwarder) = crate::tui::job_messages::scoped_sender(Some(tx), ctx.id);
            let Some(tx) = tx else {
                return JobRunOutcome::Failed {
                    message: "Evidence UI channel unavailable.".into(),
                };
            };
            run_evidence_job(root, id, tx, ctx.cancellation_token()).await
        },
    ) {
        Ok(id) => {
            ui.latest_agent_job_id = Some(id);
            ui.push_log(format!("[evidence] Comparing saved evidence with current selected files ({id}); /cancel cancels. Results appear in the log beside Diff Review."));
            ui.status = Status::Running;
        }
        Err(_) => ui.push_log(
            "[evidence] Cannot start while another job is active or shutdown is in progress.",
        ),
    }
}
async fn run_evidence_job(
    root: PathBuf,
    id: String,
    tx: Sender<String>,
    token: CancellationToken,
) -> JobRunOutcome {
    publish_evidence_result(
        tx,
        token,
        crate::features::evidence_report::review_summary(&root, &id),
    )
    .await
}
async fn publish_evidence_result(
    tx: Sender<String>,
    token: CancellationToken,
    read: impl std::future::Future<
        Output = Result<
            crate::features::evidence_report::ReviewSummary,
            crate::features::evidence_report::ReportError,
        >,
    >,
) -> JobRunOutcome {
    let result = tokio::select! {
        biased;
        _ = token.cancelled() => return JobRunOutcome::Cancelled,
        result = read => result,
    };
    if token.is_cancelled() {
        return JobRunOutcome::Cancelled;
    }
    match result {
        Ok(summary) => { for line in summary.lines() { let _ = tx.send(line); } JobRunOutcome::Completed }
        Err(_) => JobRunOutcome::Failed { message: "Saved evidence unavailable; no report exported. Check the explicit session ID with dgc session list and inspect diagnostics with dgc session evidence <id>.".into() },
    }
}
impl TuiExecutor {
    pub(crate) fn handle_evidence_completed(&self, producer: &str, ui: &mut TuiApp) -> bool {
        let Some(id) = crate::jobs::JobId::parse_arg(producer) else {
            return false;
        };
        let Some(job) = self.jobs.get_snapshot(id) else {
            return false;
        };
        if job.kind != JobKind::Evidence || !job.status.is_terminal() {
            return false;
        }
        if let Some(error) = job.error {
            ui.push_log(format!("[evidence] {error}"));
        } else if job.status == crate::jobs::JobStatus::Cancelled {
            ui.push_log("[evidence] Cancelled; no report exported.");
        }
        if self.jobs.foreground_id().is_none() {
            ui.status = Status::Ready;
            ui.detailed_status = None;
        }
        ui.dirty = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::JobStatus;
    async fn terminal(jobs: &crate::jobs::JobManager, id: crate::jobs::JobId) -> JobStatus {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if let Some(job) = jobs.get_snapshot(id)
                    && job.status.is_terminal()
                {
                    return job.status;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("job terminalizes")
    }
    #[tokio::test]
    async fn precancelled_evidence_does_not_read_or_publish_success() {
        let dir = tempfile::tempdir().expect("root");
        let token = CancellationToken::new();
        token.cancel();
        let (tx, rx) = std::sync::mpsc::channel();
        assert_eq!(
            run_evidence_job(dir.path().into(), "unknown".into(), tx, token).await,
            JobRunOutcome::Cancelled
        );
        assert_eq!(rx.try_iter().count(), 0);
        assert!(!dir.path().join(".doge").exists());
    }
    #[tokio::test]
    async fn cancellation_after_collection_starts_suppresses_summary() {
        let token = CancellationToken::new();
        let cancel = token.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let (entered, started) = tokio::sync::oneshot::channel();
        let (_release, wait) = tokio::sync::oneshot::channel::<()>();
        let job = tokio::spawn(async move {
            publish_evidence_result(tx, token, async move {
                let _ = entered.send(());
                let _ = wait.await;
                Err(crate::features::evidence_report::ReportError::UnsafeInput)
            })
            .await
        });
        started.await.expect("collector started");
        cancel.cancel();
        assert_eq!(job.await.expect("join"), JobRunOutcome::Cancelled);
        assert_eq!(rx.try_iter().count(), 0);
    }
    #[tokio::test]
    async fn successful_evidence_job_registers_message_ownership_and_publishes_summary() {
        let dir = tempfile::tempdir().expect("root");
        let cfg = crate::config::AppConfig {
            project_root: dir.path().into(),
            no_repomap: true,
            ..Default::default()
        };
        let repomap = std::sync::Arc::new(tokio::sync::RwLock::new(None));
        let tools = crate::tools::FsTools::new(repomap.clone(), std::sync::Arc::new(cfg.clone()));
        let store =
            crate::session::SessionStore::new(dir.path().join(".doge/sessions")).expect("store");
        let manager = std::sync::Arc::new(std::sync::Mutex::new(
            crate::session::SessionManager::with_store(store),
        ));
        let mut executor =
            TuiExecutor::construct_with_session_manager(cfg, repomap, tools, manager)
                .expect("executor");
        let (tx, rx) = std::sync::mpsc::channel();
        executor.set_ui_tx(Some(tx));
        let mut ui = TuiApp::new_for_test("summary", None, "default");
        handle_evidence(&mut executor, &mut ui, "");
        let id = executor.jobs.foreground_id().expect("job");
        assert!(ui.accept_job_message(id));
        assert_eq!(terminal(&executor.jobs, id).await, JobStatus::Completed);
        let messages = rx.try_iter().collect::<Vec<_>>();
        let prefix = format!("::job_message:{id}:");
        let summaries = messages
            .iter()
            .filter_map(|m| m.strip_prefix(&prefix))
            .collect::<Vec<_>>();
        assert!(
            summaries
                .iter()
                .any(|m| m.contains("Fresh comparison of saved evidence"))
        );
        assert!(
            summaries
                .iter()
                .any(|m| m.contains("incomplete/unavailable"))
        );
        assert!(
            summaries
                .iter()
                .any(|m| m.contains("Inspect/export to stdout"))
        );
        for line in summaries {
            assert!(ui.accept_job_message(id));
            ui.push_log(line);
        }
        assert!(ui.log.len() > 3);
    }
    #[tokio::test]
    async fn unavailable_evidence_fails_without_raw_error_content() {
        let dir = tempfile::tempdir().expect("root");
        let (tx, rx) = std::sync::mpsc::channel();
        let outcome = run_evidence_job(
            dir.path().into(),
            "PRIVATE-MARKER".into(),
            tx,
            CancellationToken::new(),
        )
        .await;
        let JobRunOutcome::Failed { message } = outcome else {
            panic!("expected unavailable");
        };
        assert!(!message.contains("PRIVATE-MARKER"));
        assert!(!message.contains(&dir.path().display().to_string()));
        assert_eq!(rx.try_iter().count(), 0);
    }
    #[tokio::test]
    async fn evidence_waiting_for_workspace_blocks_session_switch_and_cancel_releases() {
        let dir = tempfile::tempdir().expect("root");
        let cfg = crate::config::AppConfig {
            project_root: dir.path().into(),
            no_repomap: true,
            ..Default::default()
        };
        let repomap = std::sync::Arc::new(tokio::sync::RwLock::new(None));
        let tools = crate::tools::FsTools::new(repomap.clone(), std::sync::Arc::new(cfg.clone()));
        let store =
            crate::session::SessionStore::new(dir.path().join(".doge/sessions")).expect("store");
        let manager = std::sync::Arc::new(std::sync::Mutex::new(
            crate::session::SessionManager::with_store(store),
        ));
        let mut executor =
            TuiExecutor::construct_with_session_manager(cfg, repomap, tools, manager)
                .expect("executor");
        let mut ui = TuiApp::new_for_test("evidence", None, "default");
        executor.set_ui_tx(ui.sender());
        executor.start_new_session(&mut ui, None).expect("session");
        let session = executor
            .session_manager
            .lock()
            .expect("manager")
            .current_session_id()
            .expect("ID");
        let (entered, wait) = tokio::sync::oneshot::channel();
        let (release, end) = tokio::sync::oneshot::channel();
        let writer = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::Test,
                    JobScope::Background,
                    WorkspaceAccess::Write,
                    "hold workspace",
                ),
                move |_| async move {
                    let _ = entered.send(());
                    let _ = end.await;
                    JobRunOutcome::Completed
                },
            )
            .expect("writer");
        wait.await.expect("workspace acquired");
        handle_evidence(&mut executor, &mut ui, &session);
        let id = executor.jobs.foreground_id().expect("evidence job");
        assert!(ui.accept_job_message(id));
        assert_eq!(
            executor.jobs.get_snapshot(id).expect("job").kind,
            JobKind::Evidence
        );
        assert!(executor.start_new_session(&mut ui, None).is_err());
        handle_evidence(&mut executor, &mut ui, &session);
        assert_eq!(executor.jobs.foreground_id(), Some(id));
        executor.jobs.cancel(id);
        assert_eq!(terminal(&executor.jobs, id).await, JobStatus::Cancelled);
        assert!(executor.jobs.foreground_id().is_none());
        assert_eq!(
            executor
                .session_manager
                .lock()
                .expect("manager")
                .current_session_id(),
            Some(session)
        );
        assert!(executor.handle_evidence_completed(&id.to_string(), &mut ui));
        let _ = release.send(());
        assert_eq!(terminal(&executor.jobs, writer).await, JobStatus::Completed);
        executor
            .start_new_session(&mut ui, None)
            .expect("switch now allowed");
    }
}
