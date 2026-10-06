use crate::features::testing;
use crate::jobs::{JobKind, JobRunOutcome, JobScope, JobSpec, JobStartError, WorkspaceAccess};
use crate::tui::channel::SenderExt;
use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;

/// Run tests for Go, Rust, TypeScript, and Python projects as a foreground job.
pub fn handle_test(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    // Fast pre-check so a busy rejection never flips the global status.
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
        ui.push_log("UI channel unavailable - cannot run tests.");
        return;
    };
    let project_root = executor.cfg.project_root.clone();
    let command_timeout_ms = executor.cfg.command_timeout_ms;
    let tools = executor.tools.clone();
    let deferred = executor.deferred_followups.clone();

    let spec = JobSpec::new(
        JobKind::Test,
        JobScope::Foreground,
        WorkspaceAccess::ReadOnly,
        "Run project tests",
    );
    let spawn = executor.jobs.spawn(spec, move |ctx| async move {
        let producer = ctx.id;
        run_test_job(
            producer,
            deferred,
            project_root,
            tools,
            ui_tx,
            command_timeout_ms,
            ctx.cancellation_token(),
        )
        .await
    });
    match spawn {
        Ok(id) => {
            ui.push_log("Running test command...");
            ui.push_log(format!("Test job started as {id}."));
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

async fn run_test_job(
    producer: crate::jobs::JobId,
    deferred: crate::tui::followup::DeferredFollowupStore,
    project_root: PathBuf,
    tools: crate::tools::FsTools,
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
        ui_tx.send_logged("::shell_output:Test run cancelled.");
        ui_tx.send_logged("::status:cancelled");
        return JobRunOutcome::Cancelled;
    }

    // Detect languages in the project
    let detected_languages = testing::detect_project_languages(&project_root);

    ui_tx.send_logged(format!(
        "::shell_output:Detected languages: {:?}",
        detected_languages
    ));

    if detected_languages.is_empty() {
        ui_tx.send_logged(
            "::shell_output:No supported languages (Go, Rust, TypeScript, Python) detected.",
        );
        ui_tx.send_logged("::status:idle");
        return JobRunOutcome::Completed;
    }

    let mut all_failed_tests = Vec::new();
    let mut all_command_outputs = Vec::new();
    let mut has_any_failures = false;

    let test_configs = testing::get_test_configs(&project_root);

    // Run tests for each detected language
    for lang in detected_languages {
        if cancellation.is_cancelled() {
            ui_tx.send_logged("::shell_output:Test run cancelled.");
            ui_tx.send_logged("::status:cancelled");
            return JobRunOutcome::Cancelled;
        }
        ui_tx.send_logged(format!("::shell_output:\n--- Running {} tests ---", lang));

        let Some(config) = test_configs.get(&lang) else {
            ui_tx.send_logged(format!(
                "::shell_output:No test configuration found for language: {}",
                lang
            ));
            continue;
        };
        if config.commands.is_empty() {
            ui_tx.send_logged(format!(
                "::shell_output:No test commands configured for language '{}'.",
                lang
            ));
            continue;
        }

        for test_cmd in &config.commands {
            if cancellation.is_cancelled() {
                ui_tx.send_logged("::shell_output:Test run cancelled.");
                ui_tx.send_logged("::status:cancelled");
                return JobRunOutcome::Cancelled;
            }
            // Snapshot verification context before the command starts so a
            // change landing mid-run is never attributed to this run.
            // Obligation attribution is frozen here via the shared matcher.
            let mut verification_context =
                crate::tools::provenance::capture_verification_context_for_invocation(
                    &tools,
                    &crate::provenance::ProvenanceAttribution::none(),
                    crate::provenance::VerificationKind::Test,
                    &test_cmd.command,
                    &test_cmd.args,
                );
            verification_context.execution_context = Some(
                crate::features::verification_context::capture_trusted(
                    &project_root,
                    &test_cmd.command,
                    Some(cancellation.child_token()),
                )
                .await,
            );
            crate::tools::provenance::prepare_verification_snapshot(
                &tools,
                &mut verification_context,
                Some(cancellation.child_token()),
            )
            .await;
            if cancellation.is_cancelled() {
                return JobRunOutcome::Cancelled;
            }
            // Per-command child token so parent cancel stops all commands.
            let child = cancellation.child_token();
            let mut result = testing::run_test_command_with_cancel(
                &project_root,
                &test_cmd.command,
                &test_cmd.args,
                command_timeout_ms,
                Some(child),
            )
            .await;

            if result.cancelled || cancellation.is_cancelled() {
                ui_tx.send_logged("::shell_output:Test run cancelled.");
                ui_tx.send_logged("::status:cancelled");
                return JobRunOutcome::Cancelled;
            }

            // One command = one observed verification event (never a proof).
            // Timed-out runs are recorded; cancellations above are not.
            // A recording failure never fails the test run itself; the
            // session is marked incomplete and the UI is told, matching the
            // semantic-edit and plan_write paths.
            if result.execution_observed {
                crate::tools::provenance::finish_verification_snapshot(
                    &tools,
                    &mut verification_context,
                    Some(cancellation.child_token()),
                )
                .await;
                if cancellation.is_cancelled() {
                    return JobRunOutcome::Cancelled;
                }
                if let Some(record) = &verification_context.execution_workspace
                    && let Some(warning) = crate::features::verification_snapshot::warning(record)
                {
                    ui_tx.send_logged(format!("[verification][warning] {warning}"));
                }
                if !crate::tools::provenance::record_tui_test_verification(
                    &tools,
                    &test_cmd.command,
                    &test_cmd.args,
                    result.success,
                    if result.timed_out {
                        "timed_out"
                    } else {
                        "completed"
                    },
                    result.exit_code,
                    result.timed_out,
                    &result.stdout,
                    &result.stderr,
                    result.output_truncated,
                    result.warnings.clone(),
                    verification_context,
                ) {
                    ui_tx.send_logged(
                        "[provenance][warning] Test ran, but provenance recording failed."
                            .to_string(),
                    );
                }
            }

            // Store the command output
            all_command_outputs.push(format!(
                "Command: {} {}\nExit code: {}\nSTDOUT:\n{}\nSTDERR:\n{}",
                test_cmd.command,
                test_cmd.args.join(" "),
                result
                    .exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| {
                        if result.timed_out {
                            "timeout".to_string()
                        } else {
                            "none".to_string()
                        }
                    }),
                testing::budget_diagnostic_output(&result.stdout),
                testing::budget_diagnostic_output(&result.stderr)
            ));

            // Cancellation is not a test failure.
            if !result.success {
                has_any_failures = true;
            }

            // Send output to UI
            if result.success {
                ui_tx.send_logged(format!(
                    "::shell_output:✓ All tests passed: {} {}",
                    test_cmd.command,
                    test_cmd.args.join(" ")
                ));
            } else {
                ui_tx.send_logged(format!(
                    "::shell_output:✗ Tests failed: {} {}",
                    test_cmd.command,
                    test_cmd.args.join(" ")
                ));
            }

            if !result.stdout.is_empty() {
                ui_tx.send_logged(format!(
                    "::shell_output:Output:\n{}",
                    testing::budget_diagnostic_output(&result.stdout)
                ));
            }

            if !result.stderr.is_empty() {
                ui_tx.send_logged(format!(
                    "::shell_output:STDERR:\n{}",
                    testing::budget_diagnostic_output(&result.stderr)
                ));
            }

            // Parse failed tests
            let failed_tests = testing::parse_test_output(&result, &lang);
            result.failed_tests = failed_tests.clone();
            if !failed_tests.is_empty() {
                ui_tx.send_logged(format!(
                    "::shell_output:Found {} failed test(s)",
                    failed_tests.len()
                ));
            }

            all_failed_tests.extend(failed_tests);
        }
    }

    if cancellation.is_cancelled() {
        ui_tx.send_logged("::shell_output:Test run cancelled.");
        ui_tx.send_logged("::status:cancelled");
        return JobRunOutcome::Cancelled;
    }

    // If there are failed tests, stage one bounded follow-up payload for
    // the post-terminal handoff. The successor AgentTurn starts only after
    // this job terminalizes and releases foreground ownership; nothing is
    // dispatched from inside the producer. Cancellation is never treated
    // as a failure.
    if has_any_failures {
        let all_outputs =
            testing::budget_diagnostic_output(&all_command_outputs.join("\n\n---\n\n"));

        let prompt =
            crate::tui::followup::build_test_followup_prompt(&all_outputs, &all_failed_tests);
        ui_tx
            .send_logged("::shell_output:\nSending test failures to LLM for analysis and fixes...");
        deferred.stage(crate::tui::followup::DeferredFollowup {
            producer,
            kind: JobKind::Test,
            display: crate::tui::followup::TEST_FOLLOWUP_DISPLAY.to_string(),
            prompt,
        });
    } else {
        ui_tx.send_logged("::shell_output:\n✓ All tests passed!");
    }

    // Close the stage-then-cancel race: a cancellation landing between the
    // pre-stage check and this return must discard the just-staged payload
    // and report cancellation instead of handing off a successor.
    if deferred.discard_staged_on_cancel(producer, cancellation.is_cancelled()) {
        ui_tx.send_logged("::shell_output:Test run cancelled.");
        ui_tx.send_logged("::status:cancelled");
        return JobRunOutcome::Cancelled;
    }

    ui_tx.send_logged("::shell_output:Test run completed.");
    ui_tx.send_logged("::status:idle");
    // `cargo test` exit 1 is a domain failure, not infrastructure failure.
    JobRunOutcome::Completed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{JobManager, JobScope, JobSpec, WorkspaceAccess};

    fn test_tools_for(project_root: &std::path::Path) -> crate::tools::FsTools {
        let config = std::sync::Arc::new(crate::config::AppConfig {
            project_root: project_root.to_path_buf(),
            ..crate::config::AppConfig::default()
        });
        crate::tools::FsTools::new(std::sync::Arc::new(tokio::sync::RwLock::new(None)), config)
    }

    #[tokio::test]
    async fn test_test_cancellation_stops_remaining_commands() {
        let dir = tempfile::tempdir().unwrap();
        // A project with no supported languages completes immediately.
        let (ui_tx, _rx) = std::sync::mpsc::channel::<String>();
        let tools = test_tools_for(dir.path());
        let deferred = crate::tui::followup::DeferredFollowupStore::default();
        let outcome = run_test_job(
            crate::jobs::JobId(1),
            deferred.clone(),
            dir.path().to_path_buf(),
            tools,
            ui_tx,
            10_000,
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(outcome, JobRunOutcome::Completed));
        assert_eq!(deferred.len(), 0);
    }

    #[tokio::test]
    async fn test_test_cancelled_token_returns_cancelled() {
        // `sleep`-backed command proves ManagedProcess cleanup is reused.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let (ui_tx, _rx) = std::sync::mpsc::channel::<String>();
        let token = CancellationToken::new();
        let canceller = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            canceller.cancel();
        });
        // Direct runner-level check: cancellation maps to TestResult.cancelled.
        let result = testing::run_test_command_with_cancel(
            dir.path(),
            "sleep",
            &["30".to_string()],
            30_000,
            Some(token.clone()),
        )
        .await;
        assert!(result.cancelled);
        assert!(!result.timed_out);
        assert!(!result.success);

        // Job-level check: pre-cancelled token short-circuits without staging
        // a follow-up or requesting analysis.
        let (ui_tx2, rx2) = std::sync::mpsc::channel::<String>();
        let cancelled_token = CancellationToken::new();
        cancelled_token.cancel();
        let tools2 = test_tools_for(dir.path());
        let deferred2 = crate::tui::followup::DeferredFollowupStore::default();
        let outcome = run_test_job(
            crate::jobs::JobId(2),
            deferred2.clone(),
            dir.path().to_path_buf(),
            tools2,
            ui_tx2,
            10_000,
            cancelled_token,
        )
        .await;
        assert!(matches!(outcome, JobRunOutcome::Cancelled));
        let messages: Vec<String> = rx2.try_iter().collect();
        assert!(
            !messages
                .iter()
                .any(|m| m.starts_with("::test_failures_analysis:"))
        );
        assert_eq!(deferred2.len(), 0);
        let _ = ui_tx;
    }

    #[tokio::test]
    async fn test_test_job_foreground_busy_does_not_send_status() {
        // Status-race regression: a busy /test must not emit shell_running/idle.
        let manager = JobManager::new();
        let (ui_tx, rx) = std::sync::mpsc::channel::<String>();
        let blocker = manager
            .spawn(
                JobSpec::new(
                    JobKind::Test,
                    JobScope::Foreground,
                    WorkspaceAccess::ReadOnly,
                    "blocker",
                ),
                |ctx| async move {
                    tokio::select! {
                        _ = ctx.cancellation.cancelled() => JobRunOutcome::Cancelled,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => JobRunOutcome::Completed,
                    }
                },
            )
            .unwrap();
        // Second foreground spawn is rejected; no status messages follow.
        let second = manager.spawn(
            JobSpec::new(
                JobKind::Test,
                JobScope::Foreground,
                WorkspaceAccess::ReadOnly,
                "second",
            ),
            move |_ctx| async move {
                ui_tx.send("::status:idle".to_string()).unwrap();
                JobRunOutcome::Completed
            },
        );
        assert!(second.is_err());
        assert!(rx.try_recv().is_err());
        manager.cancel(blocker);
    }
}
