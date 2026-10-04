//! Post-terminal Test/Lint follow-up handoff.
//!
//! Test/Lint producers may compute and stage at most one optional follow-up
//! prompt while they run, but they must never start (or request) a successor
//! foreground `AgentTurn` themselves: foreground ownership is released only
//! by `JobManager` terminalization, which fires the completion hook after the
//! foreground slot is free. The TUI consumes the staged payload exactly once
//! when it observes the post-terminal `::job_completed:<id>` signal and only
//! then spawns one synthetic (internal, no-new-directive) `AgentTurn`.
//!
//! UI `::status:idle` messages are presentation only and never authorize a
//! successor; only the post-terminal hook signal does.

use crate::features::testing::FailedTest;
use crate::jobs::{JobId, JobKind};
use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Budget for the final coalesced follow-up prompt handed to the LLM.
pub const FOLLOWUP_PROMPT_BUDGET_CHARS: usize = 32_000;

/// Short log label recorded alongside the staged prompt.
pub const TEST_FOLLOWUP_DISPLAY: &str = "[test] failure analysis";
/// Short log label recorded alongside the staged prompt.
pub const LINT_FOLLOWUP_DISPLAY: &str = "[lint] issue analysis";

/// One optional successor payload staged by a Test/Lint producer run.
///
/// Keyed by the producing `JobId`, so duplicate or stale completion events
/// are idempotently ignored and one producer run can never create more than
/// one successor `AgentTurn`.
#[derive(Debug, Clone)]
pub struct DeferredFollowup {
    pub producer: JobId,
    pub kind: JobKind,
    pub display: String,
    pub prompt: String,
}

/// TUI-owned consume-once store for staged follow-ups.
///
/// The generic `JobManager` never sees prompts: it only emits the
/// post-terminal identity signal, and this store resolves it to payload.
#[derive(Debug, Clone, Default)]
pub struct DeferredFollowupStore {
    inner: Arc<Mutex<HashMap<JobId, DeferredFollowup>>>,
}

impl DeferredFollowupStore {
    /// Stage (or replace) the single follow-up for `producer`.
    pub fn stage(&self, followup: DeferredFollowup) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.insert(followup.producer, followup);
        }
    }

    /// Consume the staged follow-up exactly once. Duplicates and stale ids
    /// resolve to `None`.
    pub fn take(&self, producer: JobId) -> Option<DeferredFollowup> {
        self.inner.lock().ok()?.remove(&producer)
    }

    /// Producer-tail gate against the stage-then-cancel race: a cancellation
    /// landing between staging and the terminal return must discard the
    /// just-staged payload so no successor is handed off. Returns `true`
    /// when the caller must report `Cancelled`; otherwise the staged payload
    /// stands and the caller reports `Completed`.
    pub fn discard_staged_on_cancel(&self, producer: JobId, cancelled: bool) -> bool {
        if cancelled {
            self.take(producer);
            true
        } else {
            false
        }
    }

    /// Peek without consuming. Only used to enforce pre-terminal
    /// suppression without losing the staged payload.
    #[cfg(test)]
    pub fn contains(&self, producer: JobId) -> bool {
        self.inner
            .lock()
            .map(|guard| guard.contains_key(&producer))
            .unwrap_or(false)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().map(|guard| guard.len()).unwrap_or(0)
    }
}

/// Build the single bounded Test follow-up prompt. Callers stage it only
/// when the run observed domain failures.
pub fn build_test_followup_prompt(all_outputs: &str, failed_tests: &[FailedTest]) -> String {
    let mut prompt = String::from(
        "The following test(s) have failed. Please analyze the failures and provide fixes:\n\n",
    );
    prompt.push_str(all_outputs);

    if !failed_tests.is_empty() {
        prompt.push_str("\n\n--- Parsed Failed Tests ---\n");
        for (i, test) in failed_tests.iter().enumerate() {
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

    crate::features::testing::budget_diagnostic_output(&prompt)
}

/// Build the single coalesced Lint follow-up prompt from parsed issues and
/// bounded raw command diagnostics. Returns `None` when there is nothing
/// actionable, so producers stage nothing.
///
/// `issues_section` must already be budget-bounded by the caller
/// (`budget_lint_issues`); `raw_outputs` must already be bounded by the
/// caller (`budget_diagnostic_output`). The coalesced result is bounded
/// again so the combination can never exceed the prompt budget.
pub fn build_lint_followup_prompt(
    issues: &[crate::tui::commands::handlers::slash_commands::lint::LintIssue],
    issues_truncated: bool,
    raw_outputs: &str,
    include_raw_outputs: bool,
) -> Option<String> {
    if issues.is_empty() && !include_raw_outputs {
        return None;
    }

    let mut prompt =
        String::from("Please analyze and fix the following lint issues in the codebase:\n\n");

    for (i, issue) in issues.iter().enumerate() {
        prompt.push_str(&format!("Issue {}: {}\n", i + 1, issue.message));
        if !issue.file_path.is_empty() {
            prompt.push_str(&format!("File: {}\n", issue.file_path));
        }
        if let Some(line) = issue.line_number {
            prompt.push_str(&format!("Line: {}\n", line));
        }
        prompt.push_str(&format!("Severity: {}\n", issue.severity));
        if let Some(code) = &issue.code {
            prompt.push_str(&format!("Code: {}\n", code));
        }
        prompt.push('\n');
    }
    if issues_truncated {
        prompt.push_str(
            "Additional lint issues were omitted to stay within the diagnostic budget.\n\n",
        );
    }

    if include_raw_outputs {
        prompt.push_str("--- Raw lint command output ---\n");
        prompt.push_str(raw_outputs);
        prompt.push_str("\n\nPlease analyze the outputs above. Identify any warnings, errors, or issues in the codebase. For each issue detected, provide specific fixes with clear explanations. If you need to see the current content of any file, use the appropriate tool to read it first, then provide the corrected code.");
    } else {
        prompt.push_str("Please provide specific code fixes for each issue.");
    }

    Some(crate::tools::budget::head_tail_truncate(&prompt, FOLLOWUP_PROMPT_BUDGET_CHARS).text)
}

impl TuiExecutor {
    /// Consume the post-terminal completion signal for `producer_arg` and
    /// start at most one synthetic follow-up `AgentTurn`.
    ///
    /// Runs on the TUI thread. Suppression (no staged payload, producer not
    /// terminally `Completed`, stale/duplicate signal, busy successor) never
    /// pollutes conversation history or directive provenance: only a
    /// successfully spawned synthetic turn touches the agent path, and that
    /// path records no `DirectiveObserved`.
    pub fn handle_deferred_followup(&mut self, producer_arg: &str, ui: &mut TuiApp) {
        let Some(producer) = JobId::parse_arg(producer_arg.trim()) else {
            return;
        };
        // Pre-terminal gate: while the producer still owns the foreground
        // slot, no successor may start. The staged payload stays parked
        // until the post-terminal signal arrives.
        let Some(snapshot) = self.jobs.get_snapshot(producer) else {
            return;
        };
        if !snapshot.status.is_terminal() {
            return;
        }
        if snapshot.status != crate::jobs::JobStatus::Completed {
            // Terminal cancellation/failure creates no follow-up. Discard a
            // parked payload (if any) so a stage-then-cancel race cannot leak
            // entries in the consume-once store.
            self.deferred_followups.take(producer);
            return;
        }
        if self.jobs.foreground_id() == Some(producer) {
            return;
        }
        let Some(followup) = self.deferred_followups.take(producer) else {
            return;
        };
        if snapshot.kind != followup.kind {
            return;
        }
        // Busy/missing-key rejection is owned inside the spawn path: it logs
        // without touching history, provenance, or ownership. Only echo the
        // staged prompt after a successful reservation so a rejected handoff
        // never leaves a phantom `> prompt` in the UI.
        let display = followup.display.clone();
        let prompt = followup.prompt.clone();
        let kind = followup.kind;
        if crate::tui::commands::agent_job::spawn_synthetic_followup(
            self,
            ui,
            &display,
            prompt.clone(),
        )
        .is_ok()
        {
            match kind {
                JobKind::Test => ui.push_log("[test] Sending failures to LLM..."),
                JobKind::Lint => ui.push_log("[lint] Sending output to LLM..."),
                _ => ui.push_log("[job] Sending follow-up to LLM..."),
            }
            ui.push_log(format!("> {prompt}"));
            ui.dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed_test(name: &str) -> FailedTest {
        FailedTest {
            name: name.to_string(),
            file_path: Some("src/lib.rs".to_string()),
            line_number: Some(42),
            message: "assertion failed".to_string(),
            expected: Some("1".to_string()),
            actual: Some("2".to_string()),
            stack_trace: None,
            related_files: Vec::new(),
        }
    }

    fn lint_issue() -> crate::tui::commands::handlers::slash_commands::lint::LintIssue {
        crate::tui::commands::handlers::slash_commands::lint::LintIssue {
            file_path: "src/main.rs".to_string(),
            line_number: Some(3),
            severity: "warning".to_string(),
            message: "unused variable `x`".to_string(),
            code: None,
        }
    }

    #[test]
    fn test_test_followup_prompt_contains_failures_and_is_bounded() {
        let outputs = "x".repeat(100_000);
        let prompt = build_test_followup_prompt(&outputs, &[failed_test("my_test")]);
        assert!(prompt.contains("my_test"));
        assert!(prompt.contains("src/lib.rs"));
        assert!(
            prompt.chars().count()
                <= crate::features::testing::DIAGNOSTIC_OUTPUT_BUDGET_CHARS + 8_000
        );
    }

    #[test]
    fn test_lint_followup_coalesces_issues_and_raw_output() {
        let prompt = build_lint_followup_prompt(&[lint_issue()], false, "raw warning output", true)
            .expect("must coalesce into one payload");
        assert!(prompt.contains("unused variable"));
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("raw warning output"));
        assert!(prompt.chars().count() <= FOLLOWUP_PROMPT_BUDGET_CHARS);
    }

    #[test]
    fn test_lint_followup_issues_only_without_raw_output() {
        let prompt = build_lint_followup_prompt(&[lint_issue()], true, "", false)
            .expect("issues alone must produce a payload");
        assert!(prompt.contains("unused variable"));
        assert!(prompt.contains("omitted to stay within the diagnostic budget"));
    }

    #[test]
    fn test_lint_followup_none_without_actionable_content() {
        assert!(
            build_lint_followup_prompt(&[], false, "", false).is_none(),
            "no issues and no warnings must stage nothing"
        );
    }

    #[test]
    fn test_lint_followup_raw_only_without_issues() {
        let prompt = build_lint_followup_prompt(&[], false, "warning: something", true)
            .expect("raw warnings alone must produce a payload");
        assert!(prompt.contains("warning: something"));
    }

    #[test]
    fn test_store_take_is_consume_once() {
        let store = DeferredFollowupStore::default();
        let followup = DeferredFollowup {
            producer: JobId(7),
            kind: JobKind::Test,
            display: TEST_FOLLOWUP_DISPLAY.to_string(),
            prompt: "prompt".to_string(),
        };
        store.stage(followup);
        assert!(store.contains(JobId(7)));
        assert!(store.take(JobId(7)).is_some());
        assert!(store.take(JobId(7)).is_none());
        assert!(store.take(JobId(999)).is_none());
        assert!(!store.contains(JobId(7)));
    }

    #[test]
    fn test_store_stage_replaces_single_entry_per_producer() {
        let store = DeferredFollowupStore::default();
        for prompt in ["first", "second"] {
            store.stage(DeferredFollowup {
                producer: JobId(3),
                kind: JobKind::Lint,
                display: LINT_FOLLOWUP_DISPLAY.to_string(),
                prompt: prompt.to_string(),
            });
        }
        assert_eq!(store.len(), 1);
        assert_eq!(store.take(JobId(3)).unwrap().prompt, "second");
    }

    #[test]
    fn test_post_stage_gate_discards_on_cancel() {
        // A cancellation landing between staging and the terminal return
        // must drop the parked payload so no successor is handed off.
        let store = DeferredFollowupStore::default();
        store.stage(DeferredFollowup {
            producer: JobId(11),
            kind: JobKind::Test,
            display: TEST_FOLLOWUP_DISPLAY.to_string(),
            prompt: "prompt".to_string(),
        });
        assert!(store.discard_staged_on_cancel(JobId(11), true));
        assert!(!store.contains(JobId(11)));
        assert_eq!(store.len(), 0);
        // Idempotent: no entry, no panic.
        assert!(store.discard_staged_on_cancel(JobId(11), true));
    }

    #[test]
    fn test_post_stage_gate_retains_when_live() {
        let store = DeferredFollowupStore::default();
        store.stage(DeferredFollowup {
            producer: JobId(12),
            kind: JobKind::Lint,
            display: LINT_FOLLOWUP_DISPLAY.to_string(),
            prompt: "prompt".to_string(),
        });
        assert!(!store.discard_staged_on_cancel(JobId(12), false));
        assert!(store.contains(JobId(12)));
    }

    // --- Post-terminal handoff lifecycle tests ---------------------------

    fn test_executor_with_client() -> (TuiExecutor, tempfile::TempDir) {
        test_executor_with_base_url("http://127.0.0.1:1")
    }

    fn test_executor_with_base_url(base_url: &str) -> (TuiExecutor, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::config::AppConfig {
            project_root: dir.path().to_path_buf(),
            api_key: Some("test-key".to_string()),
            base_url: base_url.to_string(),
            // Fail fast without network access: no LLM retries.
            llm: crate::config::LlmConfig {
                max_retries: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        let executor = TuiExecutor::new(cfg).unwrap();
        (executor, dir)
    }

    /// Local stub LLM endpoint that answers every chat request with `401`.
    /// The client treats authentication errors as non-retrying, so an
    /// AgentTurn against this endpoint reaches terminal `Failed` fast and
    /// deterministically, proving the job body ran past the
    /// directive-recording point.
    async fn stub_auth_error_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = vec![0u8; 16384];
                    let _ = socket.read(&mut buf).await;
                    let body = r#"{"error":{"message":"bad key","type":"invalid_request_error","code":"invalid_api_key"}}"#;
                    let resp = format!(
                        "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                });
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    async fn wait_for_terminal(
        manager: &crate::jobs::JobManager,
        id: JobId,
    ) -> crate::jobs::JobStatus {
        for _ in 0..1200 {
            if let Some(snapshot) = manager.get_snapshot(id)
                && snapshot.status.is_terminal()
            {
                return snapshot.status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("timed out waiting for {id} to finish");
    }

    fn history_len(executor: &TuiExecutor) -> usize {
        executor
            .conversation_history
            .lock()
            .unwrap()
            .build_messages()
            .len()
    }

    fn directive_count(executor: &TuiExecutor) -> usize {
        crate::tools::provenance::load_current_events(&executor.tools)
            .unwrap()
            .map(|l| l.events)
            .unwrap_or_default()
            .iter()
            .filter(|e| {
                matches!(
                    e.event,
                    crate::provenance::ProvenanceEvent::DirectiveObserved(_)
                )
            })
            .count()
    }

    fn stage_test_followup(executor: &TuiExecutor, producer: JobId) {
        executor.deferred_followups.stage(DeferredFollowup {
            producer,
            kind: JobKind::Test,
            display: TEST_FOLLOWUP_DISPLAY.to_string(),
            prompt: "analyze this failure".to_string(),
        });
    }

    /// Blocking Test-kind foreground producer gated by Notify primitives.
    async fn spawn_blocking_test_producer(
        executor: &TuiExecutor,
    ) -> (JobId, Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        use crate::jobs::{JobRunOutcome, JobScope, JobSpec, WorkspaceAccess};
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let started_clone = started.clone();
        let release_clone = release.clone();
        let id = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::Test,
                    JobScope::Foreground,
                    WorkspaceAccess::ReadOnly,
                    "blocking test producer",
                ),
                move |ctx| async move {
                    started_clone.notify_one();
                    tokio::select! {
                        _ = ctx.cancellation.cancelled() => JobRunOutcome::Cancelled,
                        _ = release_clone.notified() => JobRunOutcome::Completed,
                    }
                },
            )
            .unwrap();
        started.notified().await;
        (id, started, release)
    }

    #[tokio::test]
    async fn test_followup_starts_only_after_producer_terminalizes() {
        use crate::jobs::JobStatus;
        let (mut executor, _dir) = test_executor_with_client();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());

        let (producer, _started, release) = spawn_blocking_test_producer(&executor).await;
        assert_eq!(executor.jobs.foreground_id(), Some(producer));
        stage_test_followup(&executor, producer);

        // While the producer still owns the foreground slot, the staged
        // follow-up must not start and must stay parked.
        executor.handle_deferred_followup(&producer.to_string(), &mut ui);
        assert_eq!(executor.jobs.foreground_id(), Some(producer));
        assert_eq!(executor.jobs.active_count(), 1);
        assert!(executor.deferred_followups.contains(producer));

        // Terminalize: foreground release is authoritative.
        release.notify_one();
        assert_eq!(
            wait_for_terminal(&executor.jobs, producer).await,
            JobStatus::Completed
        );
        assert_eq!(executor.jobs.foreground_id(), None);

        // The post-terminal hook signal must arrive on the UI channel.
        let rx = ui.inbox_rx.as_ref().unwrap();
        let expected = format!("::job_completed:{producer}");
        let mut saw_completion = false;
        for _ in 0..50 {
            match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(msg) if msg == expected => {
                    saw_completion = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => continue,
            }
        }
        assert!(saw_completion, "post-terminal hook signal must arrive");

        // Now exactly one synthetic successor starts.
        executor.handle_deferred_followup(&producer.to_string(), &mut ui);
        let successor = executor.jobs.foreground_id().unwrap();
        assert_ne!(successor, producer);
        assert_eq!(executor.jobs.active_count(), 1);
        let snapshot = executor.jobs.get_snapshot(successor).unwrap();
        assert_eq!(snapshot.kind, crate::jobs::JobKind::AgentTurn);
        assert!(!executor.deferred_followups.contains(producer));

        // Duplicate completion signal: no duplicate follow-up.
        executor.handle_deferred_followup(&producer.to_string(), &mut ui);
        assert_eq!(executor.jobs.foreground_id(), Some(successor));
        assert_eq!(executor.jobs.active_count(), 1);

        // Stale producer id: no follow-up.
        executor.handle_deferred_followup("job-999999", &mut ui);
        assert_eq!(executor.jobs.foreground_id(), Some(successor));
        assert_eq!(executor.jobs.active_count(), 1);

        executor.jobs.cancel(successor);
    }

    #[tokio::test]
    async fn test_cancelled_producer_creates_no_followup() {
        use crate::jobs::JobStatus;
        let (mut executor, _dir) = test_executor_with_client();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());

        let (producer, _started, _release) = spawn_blocking_test_producer(&executor).await;
        stage_test_followup(&executor, producer);
        executor.jobs.cancel(producer);
        assert_eq!(
            wait_for_terminal(&executor.jobs, producer).await,
            JobStatus::Cancelled
        );

        // Terminal completion releases the queue, but cancellation never starts a follow-up.
        let stray: Vec<String> = ui.inbox_rx.as_ref().unwrap().try_iter().collect();
        assert!(
            stray
                .iter()
                .any(|m| m == &format!("::job_completed:{producer}")),
            "cancelled foreground must emit terminal completion"
        );

        // Even a direct handoff attempt is suppressed on Cancelled.
        executor.handle_deferred_followup(&producer.to_string(), &mut ui);
        assert_eq!(executor.jobs.foreground_id(), None);
        assert_eq!(executor.jobs.active_count(), 0);
    }

    #[tokio::test]
    async fn test_shutdown_producer_creates_no_followup() {
        use crate::jobs::JobStatus;
        let (mut executor, _dir) = test_executor_with_client();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());

        let (producer, _started, _release) = spawn_blocking_test_producer(&executor).await;
        stage_test_followup(&executor, producer);
        executor
            .jobs
            .shutdown(std::time::Duration::from_secs(5))
            .await;
        assert_eq!(
            wait_for_terminal(&executor.jobs, producer).await,
            JobStatus::Cancelled
        );

        let stray: Vec<String> = ui.inbox_rx.as_ref().unwrap().try_iter().collect();
        assert!(
            stray
                .iter()
                .any(|m| m == &format!("::job_completed:{producer}")),
            "shutdown foreground must emit terminal completion"
        );
        executor.handle_deferred_followup(&producer.to_string(), &mut ui);
        assert_eq!(executor.jobs.foreground_id(), None);
        assert_eq!(executor.jobs.active_count(), 0);
    }

    #[tokio::test]
    async fn test_busy_successor_corrupts_nothing() {
        use crate::jobs::{JobKind, JobRunOutcome, JobScope, JobSpec, JobStatus, WorkspaceAccess};
        let (mut executor, _dir) = test_executor_with_client();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());

        // Completed producer with a staged follow-up.
        let producer = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::Test,
                    JobScope::Foreground,
                    WorkspaceAccess::ReadOnly,
                    "quick producer",
                ),
                |_ctx| async { JobRunOutcome::Completed },
            )
            .unwrap();
        assert_eq!(
            wait_for_terminal(&executor.jobs, producer).await,
            JobStatus::Completed
        );
        stage_test_followup(&executor, producer);

        // Occupy the foreground slot before the handoff runs.
        let started = Arc::new(tokio::sync::Notify::new());
        let started_clone = started.clone();
        let blocker = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::AgentTurn,
                    JobScope::Foreground,
                    WorkspaceAccess::Write,
                    "blocker",
                ),
                move |ctx| async move {
                    started_clone.notify_one();
                    tokio::select! {
                        _ = ctx.cancellation.cancelled() => JobRunOutcome::Cancelled,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => JobRunOutcome::Completed,
                    }
                },
            )
            .unwrap();
        started.notified().await;

        let history_before = history_len(&executor);
        let directives_before = directive_count(&executor);
        executor.handle_deferred_followup(&producer.to_string(), &mut ui);

        // Busy rejection: ownership stays with the blocker, history and
        // directive provenance are untouched.
        assert_eq!(executor.jobs.foreground_id(), Some(blocker));
        assert_eq!(history_len(&executor), history_before);
        assert_eq!(directive_count(&executor), directives_before);
        executor.jobs.cancel(blocker);
    }

    fn log_contains(ui: &TuiApp, needle: &str) -> bool {
        ui.log.iter().any(|entry| match entry {
            crate::tui::state::LogEntry::Plain(text)
            | crate::tui::state::LogEntry::Markdown(text) => text.contains(needle),
        })
    }

    #[tokio::test]
    async fn test_synthetic_followup_records_no_directive() {
        use crate::jobs::JobStatus;
        let base_url = stub_auth_error_server().await;
        let (mut executor, _dir) = test_executor_with_base_url(&base_url);
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());

        let directives_before = directive_count(&executor);
        assert!(executor.last_user_prompt.is_none());
        let id = crate::tui::commands::agent_job::spawn_synthetic_followup(
            &mut executor,
            &mut ui,
            "synthetic",
            "synthetic follow-up content".to_string(),
        )
        .unwrap();
        // Stub answers 401 (non-retrying): fast deterministic terminal.
        assert_eq!(
            wait_for_terminal(&executor.jobs, id).await,
            JobStatus::Failed
        );
        let snapshot = executor.jobs.get_snapshot(id).unwrap();
        assert_eq!(snapshot.kind, crate::jobs::JobKind::AgentTurn);
        // No fresh user directive, no requirement-writing authority basis,
        // and no overwrite of the real user input tracking.
        assert_eq!(directive_count(&executor), directives_before);
        assert!(executor.last_user_prompt.is_none());
        assert!(ui.last_user_input.is_none());
    }

    #[tokio::test]
    async fn test_synthetic_followup_busy_leaves_history_untouched() {
        use crate::jobs::{JobKind, JobRunOutcome, JobScope, JobSpec, WorkspaceAccess};
        let (mut executor, _dir) = test_executor_with_client();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());

        let started = Arc::new(tokio::sync::Notify::new());
        let started_clone = started.clone();
        let blocker = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::AgentTurn,
                    JobScope::Foreground,
                    WorkspaceAccess::Write,
                    "blocker",
                ),
                move |ctx| async move {
                    started_clone.notify_one();
                    tokio::select! {
                        _ = ctx.cancellation.cancelled() => JobRunOutcome::Cancelled,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => JobRunOutcome::Completed,
                    }
                },
            )
            .unwrap();
        started.notified().await;

        let history_before = history_len(&executor);
        let directives_before = directive_count(&executor);
        let result = crate::tui::commands::agent_job::spawn_synthetic_followup(
            &mut executor,
            &mut ui,
            "synthetic",
            "synthetic follow-up content".to_string(),
        );
        assert!(matches!(
            result,
            Err(crate::jobs::JobStartError::ForegroundBusy { .. })
        ));
        assert_eq!(history_len(&executor), history_before);
        assert_eq!(directive_count(&executor), directives_before);
        executor.jobs.cancel(blocker);
    }

    #[tokio::test]
    async fn test_cancelled_terminal_discards_staged_payload() {
        use crate::jobs::JobStatus;
        let (mut executor, _dir) = test_executor_with_client();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());
        let (producer, _started, _release) = spawn_blocking_test_producer(&executor).await;
        stage_test_followup(&executor, producer);
        assert!(executor.deferred_followups.contains(producer));
        executor.jobs.cancel(producer);
        assert_eq!(
            wait_for_terminal(&executor.jobs, producer).await,
            JobStatus::Cancelled
        );
        executor.handle_deferred_followup(&producer.to_string(), &mut ui);
        assert!(!executor.deferred_followups.contains(producer));
        assert_eq!(executor.jobs.foreground_id(), None);
        assert_eq!(executor.jobs.active_count(), 0);
    }

    #[tokio::test]
    async fn test_busy_handoff_echoes_no_phantom_prompt() {
        use crate::jobs::{JobKind, JobRunOutcome, JobScope, JobSpec, JobStatus, WorkspaceAccess};
        let (mut executor, _dir) = test_executor_with_client();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.set_ui_tx(ui.sender());
        let producer = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::Test,
                    JobScope::Foreground,
                    WorkspaceAccess::ReadOnly,
                    "quick producer",
                ),
                |_ctx| async { JobRunOutcome::Completed },
            )
            .unwrap();
        assert_eq!(
            wait_for_terminal(&executor.jobs, producer).await,
            JobStatus::Completed
        );
        executor.deferred_followups.stage(DeferredFollowup {
            producer,
            kind: JobKind::Test,
            display: TEST_FOLLOWUP_DISPLAY.to_string(),
            prompt: "phantom-prompt-marker-xyz-123".to_string(),
        });
        let started = Arc::new(tokio::sync::Notify::new());
        let started_clone = started.clone();
        let blocker = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::AgentTurn,
                    JobScope::Foreground,
                    WorkspaceAccess::Write,
                    "blocker",
                ),
                move |ctx| async move {
                    started_clone.notify_one();
                    tokio::select! {
                        _ = ctx.cancellation.cancelled() => JobRunOutcome::Cancelled,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => JobRunOutcome::Completed,
                    }
                },
            )
            .unwrap();
        started.notified().await;
        executor.handle_deferred_followup(&producer.to_string(), &mut ui);
        assert_eq!(executor.jobs.foreground_id(), Some(blocker));
        assert!(!log_contains(&ui, "phantom-prompt-marker-xyz-123"));
        executor.jobs.cancel(blocker);
    }
}
