use crate::{
    jobs::{JobKind, JobRunOutcome, JobScope, JobSpec, WorkspaceAccess},
    tui::{commands::core::TuiExecutor, state::Status, view::TuiApp},
};

impl TuiExecutor {
    pub(crate) fn start_review_reject(&mut self, id: &str, ui: &mut TuiApp) {
        let Some(payload) = self.tools.review_payload(id) else {
            ui.push_log("[diff] Review expired; files left untouched.");
            return;
        };
        if let Some(reason) = payload.reject_reason {
            ui.push_log(format!("[diff] {reason}"));
            return;
        }
        let tools = self.tools.clone();
        let tx = self.ui_tx.clone();
        let jobs = self.jobs.clone();
        let id = id.to_string();
        match self.jobs.spawn(
            JobSpec::new(
                JobKind::DiffReject,
                JobScope::Foreground,
                WorkspaceAccess::Write,
                "Reject reviewed changes",
            ),
            move |ctx| async move {
                let report = tools
                    .reject_review_in_job(&id, &ctx.cancellation_token(), Some((&jobs, ctx.id)))
                    .await;
                let outcome = if ctx.cancellation.is_cancelled() {
                    JobRunOutcome::Cancelled
                } else if let Some(error) = &report.error {
                    JobRunOutcome::Failed {
                        message: error.clone(),
                    }
                } else {
                    JobRunOutcome::Completed
                };
                if let Some(tx) = tx
                    && let Ok(json) = serde_json::to_string(&report)
                {
                    let _ = tx.send(format!("::diff_rejected:{json}"));
                }
                outcome
            },
        ) {
            Ok(id) => {
                ui.status = Status::Running;
                if let Some(review) = &mut ui.diff_review {
                    review.rejectable = false;
                    review.rejecting = true;
                    review.reject_job_id = Some(id);
                }
            }
            Err(e) => ui.push_log(format!("[diff] Cannot reject: {e}; files left untouched.")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_reject_waiting_for_workspace_recovers_panel_without_writes() {
        use crate::tools::{
            FinalizeMutationOptions,
            mutation::{self, MutationTargetReceipt},
        };
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::config::AppConfig {
            project_root: dir.path().canonicalize().unwrap(),
            ..Default::default()
        };
        let mut executor = TuiExecutor::new(cfg).unwrap();
        let mut ui = TuiApp::new_for_test("reject", None, "default");
        executor.set_ui_tx(ui.sender());
        executor.tools = executor
            .tools
            .clone()
            .with_review_capture(crate::jobs::JobId(99));
        // macOS temp paths can start at /var while the configured canonical
        // root starts at /private/var. Build receipts in that same root.
        let path = executor.cfg.project_root.join("a");
        std::fs::write(&path, "baseline").unwrap();
        let before = mutation::read_text_snapshot(&path).unwrap();
        let after = mutation::commit_text_candidate_blocking(&path, &before, "agent").unwrap();
        executor
            .tools
            .finalize_mutation(
                mutation::build_receipt(
                    crate::provenance::ChangeKind::FileWrite,
                    path.clone(),
                    before,
                    after,
                    MutationTargetReceipt::File,
                ),
                FinalizeMutationOptions {
                    record_undo: true,
                    ..Default::default()
                },
            )
            .await;
        let payload = executor.tools.seal_review().unwrap();
        assert!(
            payload.reject_reason.is_none(),
            "capture must support rollback: {:?}",
            payload.reject_reason
        );
        assert!(
            !payload.files.is_empty(),
            "fixture mutation must be captured"
        );
        let review_id = payload.review_id.clone().unwrap();
        ui.diff_review = Some(crate::tui::diff_review::DiffReviewState::from_payload(
            payload,
        ));
        let (started, running) = tokio::sync::oneshot::channel();
        let (release, held) = tokio::sync::oneshot::channel();
        executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::Test,
                    JobScope::Background,
                    WorkspaceAccess::ReadOnly,
                    "gate holder",
                ),
                move |_| async move {
                    started.send(()).unwrap();
                    let _ = held.await;
                    JobRunOutcome::Completed
                },
            )
            .unwrap();
        running.await.unwrap();
        executor.start_review_reject(&review_id, &mut ui);
        let id = executor.jobs.foreground_id().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while executor.jobs.get_snapshot(id).unwrap().status
                != crate::jobs::JobStatus::WaitingForWorkspace
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        executor.jobs.cancel(id);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while executor.jobs.foreground_id().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        crate::tui::commands::core::CommandHandler::handle_job_completed(
            &mut executor,
            &id.to_string(),
            &mut ui,
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "agent");
        let review = ui.diff_review.as_ref().unwrap();
        assert!(!review.rejecting);
        assert!(review.rejectable);
        assert!(!ui.diff_rejected_pending);
        // A delayed completion from the cancelled predecessor cannot unlock
        // the new retry while it is waiting for this same workspace gate.
        executor.start_review_reject(&review_id, &mut ui);
        let retry = executor.jobs.foreground_id().unwrap();
        crate::tui::commands::core::CommandHandler::handle_job_completed(
            &mut executor,
            &id.to_string(),
            &mut ui,
        );
        assert!(ui.diff_review.as_ref().unwrap().rejecting);
        assert_eq!(ui.diff_review.as_ref().unwrap().reject_job_id, Some(retry));
        executor.jobs.cancel(retry);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while executor.jobs.foreground_id().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        crate::tui::commands::core::CommandHandler::handle_job_completed(
            &mut executor,
            &retry.to_string(),
            &mut ui,
        );
        assert!(ui.diff_review.as_ref().unwrap().rejectable);
        release.send(()).unwrap();
        executor
            .jobs
            .shutdown(std::time::Duration::from_secs(2))
            .await;
    }
}
