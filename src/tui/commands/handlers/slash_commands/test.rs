use crate::features::testing;
use crate::jobs::{
    JobId, JobKind, JobRunOutcome, JobScope, JobSpec, JobStartError, WorkspaceAccess,
};
use crate::tui::channel::SenderExt;
use crate::tui::commands::core::TuiExecutor;
use crate::tui::commands::followup::{DeferredFollowup, FollowupKind, defer_message};
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
    executor.ensure_completion_hook();
    let project_root = executor.cfg.project_root.clone();
    let command_timeout_ms = executor.cfg.command_timeout_ms;

    let spec = JobSpec::new(
        JobKind::Test,
        JobScope::Foreground,
        WorkspaceAccess::ReadOnly,
        "Run project tests",
    );
    let spawn = executor.jobs.spawn(spec, move |ctx| async move {
        let producer = ctx.id;
        run_test_job(
            project_root,
            ui_tx,
            command_timeout_ms,
            ctx.cancellation_token(),
            producer,
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
    project_root: PathBuf,
    ui_tx: Sender<String>,
    command_timeout_ms: u64,
    cancellation: CancellationToken,
    producer: JobId,
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

    // If there are failed tests, send them to LLM for analysis.
    // Cancellation is never treated as a failure: a cancel racing the
    // failure build must not arm a follow-up, and the terminal outcome
    // must be Cancelled so the drain suppresses any deferred payload.
    if has_any_failures {
        if cancellation.is_cancelled() {
            ui_tx.send_logged("::shell_output:Test run cancelled.");
            ui_tx.send_logged("::status:cancelled");
            return JobRunOutcome::Cancelled;
        }
        let all_outputs =
            testing::budget_diagnostic_output(&all_command_outputs.join("\n\n---\n\n"));

        let mut prompt = String::from(
            "The following test(s) have failed. Please analyze the failures and provide fixes:\n\n",
        );
        prompt.push_str(&all_outputs);

        if !all_failed_tests.is_empty() {
            prompt.push_str("\n\n--- Parsed Failed Tests ---\n");
            for (i, test) in all_failed_tests.iter().enumerate() {
                prompt.push_str(&format!("\n{}. Test: {}\n", i + 1, test.name));
                if let Some(file) = &test.file_path {
                    prompt.push_str(&format!("   File: {}\n", file));
                }
                if let Some(line) = test.line_number {
                    prompt.push_str(&format!("   Line: {}\n", line));
                }
                prompt.push_str(&format!("   Message: {}\n", test.message));
                if let Some(expected) = &test.expected {
                    prompt.push_str(&format!("   Expected: {}\n", expected));
                }
                if let Some(actual) = &test.actual {
                    prompt.push_str(&format!("   Actual: {}\n", actual));
                }
            }
        }

        prompt.push_str("\n\nPlease analyze the test failures above. For each failure:\n1. Identify the root cause\n2. Read the relevant source files if needed\n3. Provide specific code fixes\n\nFocus on fixing the actual code bugs, not modifying the tests (unless the tests themselves are incorrect).");

        let prompt = testing::budget_diagnostic_output(&prompt);
        ui_tx
            .send_logged("::shell_output:\nSending test failures to LLM for analysis and fixes...");
        // Deferred handoff: the follow-up dispatches only after this job
        // reaches a terminal state and releases the foreground slot. Sending
        // the prompt for immediate dispatch here would race our own
        // reservation and be rejected as ForegroundBusy.
        ui_tx.send_logged(defer_message(&DeferredFollowup {
            producer: producer.0,
            kind: FollowupKind::Test,
            prompt,
            notice: "[test] Sending failures to LLM...".to_string(),
        }));
    } else {
        ui_tx.send_logged("::shell_output:\n✓ All tests passed!");
    }

    if cancellation.is_cancelled() {
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
    use crate::config::AppConfig;
    use crate::jobs::{JobManager, JobScope, JobSpec, WorkspaceAccess};
    use crate::tui::commands::core::{CommandHandler, TuiExecutor};
    use crate::tui::commands::followup::{
        DEFER_FOLLOWUP_PREFIX, completed_message, is_followup_message,
    };
    use crate::tui::state::LogEntry;
    use crate::tui::view::TuiApp;
    use std::time::Duration;

    #[tokio::test]
    async fn test_test_cancellation_stops_remaining_commands() {
        let dir = tempfile::tempdir().unwrap();
        // A project with no supported languages completes immediately.
        let (ui_tx, _rx) = std::sync::mpsc::channel::<String>();
        let outcome = run_test_job(
            dir.path().to_path_buf(),
            ui_tx,
            10_000,
            CancellationToken::new(),
            JobId(1),
        )
        .await;
        assert!(matches!(outcome, JobRunOutcome::Completed));
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

        // Job-level check: pre-cancelled token short-circuits without LLM analysis.
        let (ui_tx2, rx2) = std::sync::mpsc::channel::<String>();
        let cancelled_token = CancellationToken::new();
        cancelled_token.cancel();
        let outcome = run_test_job(
            dir.path().to_path_buf(),
            ui_tx2,
            10_000,
            cancelled_token,
            JobId(2),
        )
        .await;
        assert!(matches!(outcome, JobRunOutcome::Cancelled));
        let messages: Vec<String> = rx2.try_iter().collect();
        assert!(
            !messages
                .iter()
                .any(|m| m.starts_with(crate::tui::commands::followup::DEFER_FOLLOWUP_PREFIX))
        );
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

    fn log_texts(ui: &TuiApp) -> Vec<String> {
        ui.log
            .iter()
            .map(|entry| match entry {
                LogEntry::Plain(text) | LogEntry::Markdown(text) => text.clone(),
            })
            .collect()
    }

    async fn wait_for_terminal(executor: &TuiExecutor, id: JobId) -> crate::jobs::JobStatus {
        for _ in 0..400 {
            if let Some(snapshot) = executor.jobs.get_snapshot(id)
                && snapshot.status.is_terminal()
            {
                return snapshot.status;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("timed out waiting for {id} to finish");
    }

    #[tokio::test]
    async fn test_failing_test_handler_starts_one_followup_without_busy() {
        // Fixture-backed integration: a real `/test` producer (`handle_test`
        // -> `run_test_job`) on a temp TypeScript fixture whose `npm run test`
        // script fails deterministically via node (no network, no nested
        // cargo so no registry lock contention). Channel-to-executor routing
        // mirrors the event loop: only follow-up messages go through
        // `executor.handle`; status/shell output is drained without dispatch.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"per14-test-fixture","scripts":{"test":"node run-fail.js"}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("run-fail.js"),
            "console.log('\u{2715} my failing test');\nprocess.exit(1);\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("index.ts"), "console.log('hi');\n").unwrap();

        let cfg = AppConfig {
            project_root: dir.path().to_path_buf(),
            api_key: Some("test-key".to_string()),
            base_url: "http://127.0.0.1:1".to_string(),
            command_timeout_ms: 15_000,
            ..Default::default()
        };
        let mut executor = TuiExecutor::new(cfg).unwrap();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        let (ui_tx, rx) = std::sync::mpsc::channel::<String>();
        executor.set_ui_tx(Some(ui_tx));

        let before_history = executor
            .conversation_history
            .lock()
            .unwrap()
            .build_messages()
            .len();
        handle_test(&mut executor, &mut ui);
        let producer = executor
            .jobs
            .foreground_id()
            .expect("real /test producer must hold the foreground slot");
        assert_eq!(
            wait_for_terminal(&executor, producer).await,
            crate::jobs::JobStatus::Completed,
            "test exit 1 is a domain result, not infrastructure failure"
        );

        // Bounded drain for the post-terminal completion signal. Both sends
        // happen sequentially (defer inside the job, completion from the
        // manager hook after terminalization), so channel order is send order.
        let mut messages: Vec<String> = Vec::new();
        for _ in 0..600 {
            messages.extend(rx.try_iter());
            if messages.iter().any(|m| m == &completed_message(producer)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        messages.extend(rx.try_iter());
        let defers: Vec<&String> = messages
            .iter()
            .filter(|m| m.starts_with(DEFER_FOLLOWUP_PREFIX))
            .collect();
        assert_eq!(
            defers.len(),
            1,
            "failing /test must defer exactly one follow-up: {messages:?}"
        );
        assert!(
            messages.iter().any(|m| m == &completed_message(producer)),
            "hook must signal post-terminal completion: {messages:?}"
        );
        // Pump only follow-up messages through dispatch, in channel order.
        for msg in messages.iter().filter(|m| is_followup_message(m)) {
            executor.handle(msg, &mut ui);
        }

        let foreground = executor.jobs.foreground_id().expect("follow-up must start");
        assert_ne!(foreground, producer);
        assert_eq!(
            executor.jobs.get_snapshot(foreground).unwrap().kind,
            JobKind::AgentTurn
        );
        let logs = log_texts(&ui);
        assert!(
            !logs.iter().any(|line| line.contains("already running")),
            "follow-up must not race its own producer: {logs:?}"
        );
        assert!(
            logs.iter()
                .any(|line| line.contains("[test] Sending failures to LLM...")),
            "test follow-up notice missing: {logs:?}"
        );
        assert!(executor.pending_followups.is_empty());
        assert_eq!(
            executor
                .conversation_history
                .lock()
                .unwrap()
                .build_messages()
                .len(),
            before_history
        );
        executor.jobs.cancel(foreground);
    }
}
