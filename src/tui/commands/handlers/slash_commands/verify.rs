//! Explicit selection of snapshot-bound verification; no agent follow-up.
use crate::{
    features::{
        testing,
        verification_queue::{self, Outcome, VerificationQueue},
        verification_snapshot,
    },
    jobs::{JobKind, JobRunOutcome, JobScope, JobSpec, WorkspaceAccess},
    tools::FsTools,
    tui::{commands::core::TuiExecutor, state::Status, view::TuiApp},
};
use std::sync::{Arc, Mutex, mpsc::Sender};
use tokio_util::sync::CancellationToken;

#[derive(Debug, PartialEq, Eq)]
enum Action {
    List,
    Refresh(Option<Vec<String>>),
    Run(String),
}
fn parse(args: &str) -> anyhow::Result<Action> {
    let words: Vec<_> = args.split_whitespace().collect();
    match words.as_slice() {
        [] => Ok(Action::List),
        ["refresh", paths @ ..] => Ok(Action::Refresh(
            (!paths.is_empty()).then(|| paths.iter().map(|p| (*p).to_owned()).collect()),
        )),
        ["run", id] if id.starts_with('q') && id.len() > 32 => Ok(Action::Run((*id).to_owned())),
        _ => {
            anyhow::bail!("Usage: /verify | /verify refresh [relative paths...] | /verify run <id>")
        }
    }
}
pub fn handle_verify(executor: &mut TuiExecutor, ui: &mut TuiApp, args: &str) {
    let action = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            ui.push_log(format!("[verify] {e}"));
            return;
        }
    };
    if executor.ensure_session_idle().is_err() {
        ui.push_log("[verify] Wait for the active job or use /cancel.");
        return;
    }
    let Some(tx) = executor.ui_tx.clone() else {
        ui.push_log("[verify] UI channel unavailable.");
        return;
    };
    let tools = executor.tools.clone();
    let queue = executor.verification_queue.clone();
    let timeout = executor.cfg.command_timeout_ms;
    let jobs = executor.jobs.clone();
    match executor.jobs.spawn(
        JobSpec::new(
            JobKind::Verify,
            JobScope::Foreground,
            WorkspaceAccess::ReadOnly,
            "Inspect or run selected verification",
        ),
        move |ctx| async move {
            match run_action(
                tools,
                queue,
                action,
                timeout,
                tx,
                ctx.cancellation_token(),
                Some((jobs, ctx.id)),
            )
            .await
            {
                Ok(outcome) => outcome,
                Err(e) => JobRunOutcome::Failed {
                    message: e.to_string(),
                },
            }
        },
    ) {
        Ok(id) => {
            ui.latest_agent_job_id = Some(id);
            ui.status = Status::Running;
            ui.push_log(format!(
                "[verify] {id}; /jobs shows progress, /cancel cancels. No automatic fixes."
            ));
        }
        Err(_) => ui.push_log(
            "[verify] Cannot start while another job is active or shutdown is in progress.",
        ),
    }
}
fn lock(
    queue: &Arc<Mutex<VerificationQueue>>,
) -> anyhow::Result<std::sync::MutexGuard<'_, VerificationQueue>> {
    queue
        .lock()
        .map_err(|_| anyhow::anyhow!("Verification queue unavailable."))
}
// Also terminalizes session-local state when JobManager aborts the future.
struct RunningGuard {
    queue: Arc<Mutex<VerificationQueue>>,
    index: usize,
}
impl Drop for RunningGuard {
    fn drop(&mut self) {
        if let Ok(mut queue) = self.queue.lock()
            && let Some(entry) = queue.entries.get_mut(self.index)
            && entry.outcome == Outcome::Running
        {
            entry.outcome = Outcome::Cancelled;
        }
    }
}
// Send synchronously so the completion hook cannot overtake final log messages.
struct VerifyUi {
    tx: Sender<String>,
    job_id: Option<crate::jobs::JobId>,
}
impl VerifyUi {
    fn send(&self, message: String) -> Result<(), std::sync::mpsc::SendError<String>> {
        self.tx.send(match self.job_id {
            Some(id) => format!("::job_message:{id}:{message}"),
            None => message,
        })
    }
}
async fn run_action(
    tools: FsTools,
    queue: Arc<Mutex<VerificationQueue>>,
    action: Action,
    timeout: u64,
    tx: Sender<String>,
    token: CancellationToken,
    completion: Option<(crate::jobs::JobManager, crate::jobs::JobId)>,
) -> anyhow::Result<JobRunOutcome> {
    let tx = VerifyUi {
        tx,
        job_id: completion.as_ref().map(|(_, id)| *id),
    };
    if token.is_cancelled() {
        return Ok(JobRunOutcome::Cancelled);
    }
    let root = &tools.config.project_root;
    let session_id = tools.get_current_session().map(|s| s.meta.id);
    let reset = {
        let q = lock(&queue)?;
        q.planned.is_none() || q.session_id != session_id
    };
    if matches!(action, Action::Refresh(_)) || (matches!(action, Action::List) && reset) {
        let paths = match &action {
            Action::Refresh(Some(paths)) => Some(paths.clone()),
            _ if !reset => lock(&queue)?.paths.clone(),
            _ => None,
        };
        let paths = paths
            .map(|paths| {
                paths
                    .iter()
                    .map(|p| {
                        crate::analysis::normalize_relative_path(root, std::path::Path::new(p))
                    })
                    .collect::<anyhow::Result<Vec<_>>>()
            })
            .transpose()?;
        let before = verification_snapshot::capture(
            root,
            paths
                .clone()
                .unwrap_or_else(|| {
                    tools
                        .get_current_session()
                        .map(|s| s.changed_files)
                        .unwrap_or_default()
                })
                .into_iter()
                .collect(),
            Some(token.child_token()),
        )
        .await;
        let history = verification_queue::history::load(&tools);
        let impact_history = match &history {
            Ok(loaded) => Ok(loaded.clone()),
            Err(_) => Err(anyhow::anyhow!("bounded history unavailable")),
        };
        let impact = crate::tools::impact::impact_analyze_with_history(
            &tools,
            crate::tools::impact::ImpactAnalyzeArgs {
                paths: paths.clone(),
                response_budget_chars: Some(8000),
                ..Default::default()
            },
            Some(token.child_token()),
            Some(impact_history),
        )
        .await?;
        let references = before
            .files
            .iter()
            .map(|f| f.path.clone())
            .chain(impact.changed_files.iter().cloned())
            .collect();
        let after =
            verification_snapshot::capture(root, references, Some(token.child_token())).await;
        if token.is_cancelled() {
            return Ok(JobRunOutcome::Cancelled);
        }
        let after = verification_queue::bind_planning_snapshot(&before, after)?;
        if after.status != verification_snapshot::CaptureStatus::Complete {
            let _ = tx.send("[verify] Snapshot correspondence unknown; suggestions can be inspected but cannot be selected until a complete snapshot is available.".into());
        }
        for warning in impact.warnings {
            let _ = tx.send(format!("[verify] {warning}"));
        }
        lock(&queue)?.refresh(session_id.clone(), after, impact.verification, paths);
        match history {
            Ok(Some(history)) => lock(&queue)?.restore_history(&history, root),
            Ok(None) => {}
            Err(_) => {
                for entry in &mut lock(&queue)?.entries {
                    entry.history = None;
                }
                lock(&queue)?.history_warning =
                    Some("Recorded history unavailable; no historical results restored.".into())
            }
        }
    }
    let references = lock(&queue)?
        .planned
        .as_ref()
        .map(|s| s.files.iter().map(|f| f.path.clone()).collect())
        .unwrap_or_default();
    let current = verification_snapshot::capture(root, references, Some(token.child_token())).await;
    if token.is_cancelled() {
        return Ok(JobRunOutcome::Cancelled);
    }
    let mut pending = None;
    let mut _running_guard = None;
    if let Action::Run(id) = action {
        let (index, entry) = lock(&queue)?.selection(&id, &session_id, &current)?;
        let r = entry.recommendation;
        anyhow::ensure!(
            r.kind == "test",
            "Unsupported verification kind; nothing executed."
        );
        let cwd = if let Some(relative) = &r.cwd {
            let path = std::path::Path::new(relative);
            anyhow::ensure!(
                !path.is_absolute()
                    && path
                        .components()
                        .all(|p| matches!(p, std::path::Component::Normal(_))),
                "Invalid candidate working directory."
            );
            let cwd = root.join(path).canonicalize()?;
            anyhow::ensure!(
                cwd.starts_with(root.canonicalize()?),
                "Candidate working directory escapes project."
            );
            cwd
        } else {
            root.clone()
        };
        let mut context = crate::tools::provenance::capture_verification_context_for_invocation(
            &tools,
            &crate::provenance::ProvenanceAttribution::none(),
            crate::provenance::VerificationKind::Test,
            &r.program,
            &r.args,
        );
        context.execution_context = Some(
            crate::features::verification_context::capture_trusted(
                &cwd,
                &r.program,
                Some(token.child_token()),
            )
            .await,
        );
        context.execution_identity =
            crate::features::verification_context::identity::capture(root, &cwd, &r.program);
        let references = current.files.iter().map(|f| f.path.clone()).collect();
        let workspace =
            verification_snapshot::begin(root, references, Some(token.child_token())).await;
        lock(&queue)?.selection(&id, &session_id, &workspace.start)?;
        if token.is_cancelled() {
            return Ok(JobRunOutcome::Cancelled);
        }
        context.execution_workspace = Some(workspace.clone());
        {
            let mut q = lock(&queue)?;
            q.entries[index].history = None;
            q.entries[index].outcome = Outcome::Running;
            q.entries[index].workspace = Some(workspace);
            q.entries[index].test_counts = None;
            q.entries[index].exit_code = None;
        }
        _running_guard = Some(RunningGuard {
            queue: queue.clone(),
            index,
        });
        let _ = tx.send(format!(
            "[verify] Running selected candidate {}: {} {:?}; scope={}",
            index + 1,
            r.program,
            r.args,
            r.coverage_scope
        ));
        let result = testing::run_test_command_with_cancel(
            &cwd,
            &r.program,
            &r.args,
            timeout,
            Some(token.child_token()),
        )
        .await;
        if token.is_cancelled() || result.cancelled {
            lock(&queue)?.entries[index].outcome = Outcome::Cancelled;
            let _ = tx.send("[verify] Cancelled; no successful verification recorded.".into());
            return Ok(JobRunOutcome::Cancelled);
        }
        crate::tools::provenance::finish_verification_snapshot(
            &tools,
            &mut context,
            Some(token.child_token()),
        )
        .await;
        if token.is_cancelled() {
            return Ok(JobRunOutcome::Cancelled);
        }
        if context.execution_identity
            != crate::features::verification_context::identity::capture(root, &cwd, &r.program)
        {
            context.execution_identity = None;
        }
        pending = Some((index, r, result, context));
    }
    let references = lock(&queue)?
        .planned
        .as_ref()
        .map(|s| s.files.iter().map(|f| f.path.clone()).collect())
        .unwrap_or_default();
    let current = verification_snapshot::capture(root, references, Some(token.child_token())).await;
    if token.is_cancelled() {
        return Ok(JobRunOutcome::Cancelled);
    }
    Ok(commit_completion(completion, &token, || {
        let publish = || -> anyhow::Result<JobRunOutcome> {
            if let Some((index, r, result, context)) = pending {
                {
                    let mut q = lock(&queue)?;
                    let entry = &mut q.entries[index];
                    entry.outcome = verification_queue::classify(&result);
                    entry.workspace = context.execution_workspace.clone();
                    entry.exit_code = result.exit_code;
                    entry.test_counts =
                        result.structured_test_result.as_ref().and_then(|s| {
                            match s.observation {
                        crate::features::structured_test_results::ResultObservation::Complete {
                            test_events,
                            ..
                        } => Some(test_events),
                        crate::features::structured_test_results::ResultObservation::Pytest { test_events, .. } => test_events,
                        _ => None,
                    }
                        });
                }
                if result.execution_observed {
                    let mut warnings = result.warnings.clone();
                    warnings.push(format!(
                "Selected via /verify; declared coverage scope={} (not observed runtime coverage).",
                r.coverage_scope
            ));
                    if let Some(warning) = context
                        .execution_workspace
                        .as_ref()
                        .and_then(verification_snapshot::warning)
                    {
                        warnings.push(warning.into());
                    }
                    let persisted = crate::tools::provenance::record_selected_test_verification(
                        &tools,
                        crate::provenance::VerificationRecordInput {
                            kind: crate::provenance::VerificationKind::Test,
                            source: crate::provenance::VerificationSource::TuiTest,
                            program: &r.program,
                            args: &r.args,
                            cwd_relative: r.cwd.clone(),
                            success: result.success,
                            status: if result.timed_out {
                                "timed_out"
                            } else {
                                "completed"
                            },
                            exit_code: result.exit_code,
                            timed_out: result.timed_out,
                            stdout: &result.stdout,
                            stderr: &result.stderr,
                            capture_truncated: result.output_truncated,
                            context,
                            extra_warnings: warnings,
                            structured_test_result: result.structured_test_result.clone(),
                        },
                    );
                    if !persisted {
                        let _ = tx.send(
                            "[verify] Evidence persistence incomplete; inspect diagnostics.".into(),
                        );
                    }
                }
                for output in [&result.stdout, &result.stderr] {
                    if !output.is_empty() {
                        let _ = tx.send(testing::budget_diagnostic_output(output));
                    }
                }
            }
            for line in lock(&queue)?.lines(&current) {
                let _ = tx.send(line);
            }
            Ok(JobRunOutcome::Completed)
        };
        match publish() {
            Ok(outcome) => outcome,
            Err(e) => JobRunOutcome::Failed {
                message: e.to_string(),
            },
        }
    }))
}
fn commit_completion(
    completion: Option<(crate::jobs::JobManager, crate::jobs::JobId)>,
    token: &CancellationToken,
    publish: impl FnOnce() -> JobRunOutcome,
) -> JobRunOutcome {
    if let Some((jobs, id)) = completion {
        jobs.finish_synchronous_commit(id, publish)
    } else if token.is_cancelled() {
        JobRunOutcome::Cancelled
    } else {
        publish()
    }
}

impl TuiExecutor {
    pub(crate) fn handle_verify_completed(&self, producer: &str, ui: &mut TuiApp) -> bool {
        let Some(job) =
            crate::jobs::JobId::parse_arg(producer).and_then(|id| self.jobs.get_snapshot(id))
        else {
            return false;
        };
        if job.kind != JobKind::Verify || !job.status.is_terminal() {
            return false;
        }
        if let Some(error) = job.error {
            ui.push_log(format!("[verify] {error}"));
        } else if job.status == crate::jobs::JobStatus::Cancelled {
            ui.push_log("[verify] Cancelled; no verification success claimed.");
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
mod tests;
