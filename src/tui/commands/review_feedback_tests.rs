use crate::{
    config::AppConfig,
    features::review_feedback::{Anchor, Comment, FeedbackBatch, identity},
    jobs::{JobId, JobKind, JobRunOutcome, JobScope, JobSpec, JobStatus, WorkspaceAccess},
    tools::{
        FinalizeMutationOptions, FsTools,
        mutation::{self, MutationTargetReceipt},
    },
    tui::{
        commands::{CommandHandler, TuiExecutor},
        diff_review::DiffReviewState,
        state::TuiApp,
    },
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

async fn fixture() -> (TuiExecutor, TuiApp, tempfile::TempDir, FeedbackBatch) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cfg = AppConfig {
        project_root: root.clone(),
        no_repomap: true,
        tool_routing: crate::config::ToolRoutingConfig::default()
            .with_mode(crate::config::ToolRoutingMode::Eager),
        api_key: Some("mock".into()),
        base_url: "http://127.0.0.1:1".into(),
        ..Default::default()
    };
    let map = Arc::new(tokio::sync::RwLock::new(None));
    let tools = FsTools::new(map.clone(), Arc::new(cfg.clone()));
    let manager = Arc::new(Mutex::new(crate::session::SessionManager::with_store(
        crate::session::SessionStore::new(root.join("sessions")).unwrap(),
    )));
    let mut executor =
        TuiExecutor::construct_with_session_manager(cfg, map, tools, manager).unwrap();
    executor.tools = executor.tools.clone().with_review_capture(JobId(900));
    executor
        .conversation_history
        .lock()
        .unwrap()
        .append_user("Original request retained");
    executor
        .session_manager
        .lock()
        .unwrap()
        .update_current_session_with_history(
            &executor.conversation_history.lock().unwrap().snapshot(),
        )
        .unwrap();
    let directive = crate::tools::provenance::record_directive_observed(
        &executor.tools,
        crate::provenance::DirectiveOrigin::TuiPrompt,
        "Original request retained",
        "Original request retained",
    )
    .unwrap()
    .event_id;
    for name in ["a.txt", "b.txt"] {
        let path = root.join(name);
        let old = (1..=25).map(|n| format!("line {n}\n")).collect::<String>();
        std::fs::write(&path, &old).unwrap();
        let new = if name == "a.txt" {
            old.replace("line 2\n", "first changed\n")
                .replace("line 20\n", "second changed\n")
        } else {
            old.replace("line 4\n", "third changed\n")
        };
        change(&executor.tools, &path, &new, Some(directive.clone())).await;
    }
    executor
        .tools
        .plan_write(
            vec![crate::tools::plan::PlanItem {
                id: "existing-step".into(),
                parent_id: None,
                content: "Existing plan retained".into(),
                status: "in_progress".into(),
                requirement_ids: vec![],
                verification_obligations: vec![],
            }],
            crate::tools::plan::PlanWriteMode::Replace,
        )
        .unwrap();
    let source = executor.tools.seal_review().unwrap();
    let state = DiffReviewState::from_payload(source.clone());
    assert_eq!(state.files.len(), 2);
    assert_eq!(state.files.iter().map(|f| f.hunks.len()).sum::<usize>(), 3);
    let comments = state
        .files
        .iter()
        .flat_map(|f| {
            f.hunks.iter().map(|h| Comment {
                anchor: Anchor {
                    selection: None,
                    version: 1,
                    session_id: source.session_id.clone().unwrap(),
                    review_id: source.review_id.clone().unwrap(),
                    review_identity: identity(&source.diff),
                    path: f.path.clone(),
                    hunk: h.clone(),
                },
                text: format!("{} の {} 行を日本語で改善してください", f.path, h.old.start),
            })
        })
        .collect();
    let batch = FeedbackBatch {
        id: uuid::Uuid::now_v7().to_string(),
        revision: 1,
        source,
        comments,
        original_directive_ids: vec![],
    };
    batch.validate_structure().unwrap();
    let mut ui = TuiApp::new_for_test("feedback", None, "default");
    ui.diff_review = Some(state.clone());
    ui.last_user_input = Some("Original request retained".into());
    executor.last_user_prompt = Some("Original request retained".into());
    ui.review_feedback = Some(crate::tui::review_feedback::FeedbackDraft {
        batch: batch.clone(),
        source: state,
        latest_review: None,
        job_id: None,
        submitted_revision: None,
        repair_review_id: None,
        outcome: None,
        stale: false,
    });
    executor.set_ui_tx(ui.sender());
    (executor, ui, dir, batch)
}
async fn change(fs: &FsTools, path: &std::path::Path, text: &str, directive: Option<String>) {
    let before = mutation::read_text_snapshot(path).unwrap();
    let after = mutation::commit_text_candidate(path, &before, text)
        .await
        .unwrap();
    fs.finalize_mutation(
        mutation::build_receipt(
            crate::provenance::ChangeKind::FileWrite,
            path.to_owned(),
            before,
            after,
            MutationTargetReceipt::File,
        ),
        FinalizeMutationOptions {
            record_undo: true,
            attribution: directive
                .map(crate::provenance::ProvenanceAttribution::with_directive)
                .unwrap_or_else(crate::provenance::ProvenanceAttribution::none),
            ..Default::default()
        },
    )
    .await;
}
struct Provider {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Provider {
    async fn new(responses: Vec<(u16, Value)>, delay_second: bool) -> Self {
        #[derive(Clone)]
        struct State {
            responses: Arc<Vec<(u16, Value)>>,
            requests: Arc<Mutex<Vec<Value>>>,
            delay: bool,
        }
        async fn reply(
            axum::extract::State(state): axum::extract::State<State>,
            axum::Json(body): axum::Json<Value>,
        ) -> (axum::http::StatusCode, axum::Json<Value>) {
            let index = {
                let mut requests = state.requests.lock().unwrap();
                let index = requests.len();
                requests.push(body);
                index
            };
            if state.delay && index == 1 {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
            let (status, response) = state
                .responses
                .get(index)
                .cloned()
                .unwrap_or((500, json!({"error":"unexpected extra request"})));
            (
                axum::http::StatusCode::from_u16(status).unwrap(),
                axum::Json(response),
            )
        }
        let requests = Arc::new(Mutex::new(vec![]));
        let state = State {
            responses: Arc::new(responses),
            requests: requests.clone(),
            delay: delay_second,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = axum::Router::new()
            .route("/v1/chat/completions", axum::routing::post(reply))
            .with_state(state);
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            url,
            requests,
            task,
        }
    }
    fn configure(&self, executor: &mut TuiExecutor) {
        let mut llm = executor.cfg.llm.clone();
        llm.max_retries = 0;
        executor.client = Some(
            crate::llm::OpenAIClient::new(&self.url, "mock")
                .unwrap()
                .with_llm_config(llm),
        );
    }
}
fn done() -> Value {
    json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"repair complete"}}],"usage":{"prompt_tokens":12,"completion_tokens":4,"total_tokens":16}})
}
fn write(path: &std::path::Path) -> Value {
    json!({"choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","tool_calls":[{"id":"repair-write","type":"function","function":{"name":"fs_write","arguments":json!({"path":path,"content":"repair-only change"}).to_string()}}]}}],"usage":{"prompt_tokens":12,"completion_tokens":4,"total_tokens":16}})
}
async fn terminal(executor: &TuiExecutor, id: JobId) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !executor.jobs.get_snapshot(id).unwrap().status.is_terminal() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn notices(ui: &TuiApp) -> Vec<String> {
    ui.inbox_rx.as_ref().unwrap().try_iter().collect()
}
#[tokio::test]
async fn feedback_batch_real_loop_preserves_conversation_plan_raw_provenance_and_repair_only_rollback()
 {
    let (mut executor, mut ui, _dir, batch) = fixture().await;
    let path = executor.cfg.project_root.join("a.txt");
    let original_after = std::fs::read_to_string(&path).unwrap();
    let provider = Provider::new(vec![(200, write(&path)), (200, done())], false).await;
    provider.configure(&mut executor);
    let id = executor
        .submit_review_feedback(batch.clone(), &mut ui)
        .unwrap();
    ui.review_feedback.as_mut().unwrap().job_id = Some(id);
    assert!(
        executor
            .submit_review_feedback(batch.clone(), &mut ui)
            .is_err()
    );
    terminal(&executor, id).await;
    executor.handle_job_completed(&id.to_string(), &mut ui);
    assert_eq!(
        executor.jobs.get_snapshot(id).unwrap().status,
        JobStatus::Completed,
        "{:?}",
        executor.jobs.get_snapshot(id)
    );
    assert_eq!(provider.requests.lock().unwrap().len(), 2);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "repair-only change"
    );
    assert_eq!(
        executor.last_user_prompt.as_deref(),
        Some("Original request retained")
    );
    assert_eq!(
        ui.last_user_input.as_deref(),
        Some("Original request retained")
    );
    assert_eq!(
        ui.review_feedback.as_ref().unwrap().batch.comments,
        batch.comments
    );
    assert!(
        executor
            .submit_review_feedback(batch.clone(), &mut ui)
            .unwrap_err()
            .to_string()
            .contains("already submitted")
    );
    let request = provider.requests.lock().unwrap()[0].clone();
    let messages = request["messages"].as_array().unwrap();
    assert!(messages.iter().any(|m| {
        m["content"]
            .as_str()
            .is_some_and(|s| s.contains("Original request retained"))
    }));
    assert!(messages.iter().any(|m| {
        m["role"] == "user"
            && m["content"].as_str().is_some_and(|s| {
                s.contains("saved_execution_plan") && s.contains("Existing plan retained")
            })
    }));
    let evidence = messages
        .iter()
        .find(|m| {
            m["role"] == "assistant"
                && m["content"]
                    .as_str()
                    .is_some_and(|s| s.contains("Generated review feedback evidence"))
        })
        .unwrap();
    assert!(
        evidence["content"]
            .as_str()
            .unwrap()
            .contains("original_directive_ids")
    );
    let events = crate::tools::provenance::load_current_events(&executor.tools)
        .unwrap()
        .unwrap()
        .events;
    let directives = events
        .iter()
        .filter_map(|e| match &e.event {
            crate::provenance::ProvenanceEvent::DirectiveObserved(d) => Some(d),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(directives.len(), 4);
    for c in &batch.comments {
        assert!(
            directives
                .iter()
                .any(|d| d.raw_input == c.text && d.effective_instruction == c.text)
        );
    }
    let repair = ui.diff_review.as_ref().unwrap().review_id.clone().unwrap();
    assert_ne!(Some(&repair), batch.source.review_id.as_ref());
    assert!(
        ui.review_feedback
            .as_ref()
            .unwrap()
            .repair_review_id
            .is_some()
    );
    let report = executor
        .tools
        .reject_review(&repair, &tokio_util::sync::CancellationToken::new())
        .await;
    assert!(report.error.is_none(), "{report:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original_after);
}
#[tokio::test]
async fn feedback_stale_external_changes_new_capture_session_and_missing_client_block_without_model()
 {
    let (mut executor, mut ui, _dir, batch) = fixture().await;
    let provider = Provider::new(vec![(200, done())], false).await;
    provider.configure(&mut executor);
    let path = executor.cfg.project_root.join("b.txt");
    let saved = std::fs::read(&path).unwrap();
    std::fs::write(&path, "external").unwrap();
    assert!(
        executor
            .submit_review_feedback(batch.clone(), &mut ui)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"external");
    std::fs::write(&path, saved).unwrap();
    executor.client = None;
    assert!(
        executor
            .submit_review_feedback(batch.clone(), &mut ui)
            .unwrap_err()
            .to_string()
            .contains("No model")
    );
    provider.configure(&mut executor);
    executor
        .session_manager
        .lock()
        .unwrap()
        .create_session(None)
        .unwrap();
    assert!(
        executor
            .submit_review_feedback(batch.clone(), &mut ui)
            .is_err()
    );
    executor.tools = executor.tools.clone().with_review_capture(JobId(901));
    assert!(executor.submit_review_feedback(batch, &mut ui).is_err());
    assert!(provider.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn feedback_revalidates_after_workspace_gate_before_directive_or_model() {
    let (mut executor, mut ui, _dir, batch) = fixture().await;
    let provider = Provider::new(vec![(200, done())], false).await;
    provider.configure(&mut executor);
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, held) = tokio::sync::oneshot::channel();
    executor
        .jobs
        .spawn(
            JobSpec::new(
                JobKind::Test,
                JobScope::Background,
                WorkspaceAccess::ReadOnly,
                "hold gate",
            ),
            move |_| async move {
                started.send(()).unwrap();
                let _ = held.await;
                JobRunOutcome::Completed
            },
        )
        .unwrap();
    ready.await.unwrap();
    let id = executor
        .submit_review_feedback(batch.clone(), &mut ui)
        .unwrap();
    let path = executor.cfg.project_root.join("a.txt");
    std::fs::write(&path, "external while waiting").unwrap();
    release.send(()).unwrap();
    terminal(&executor, id).await;
    assert_eq!(
        executor.jobs.get_snapshot(id).unwrap().status,
        JobStatus::Failed
    );
    assert!(provider.requests.lock().unwrap().is_empty());
    let events = crate::tools::provenance::load_current_events(&executor.tools)
        .unwrap()
        .unwrap()
        .events;
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                e.event,
                crate::provenance::ProvenanceEvent::DirectiveObserved(_)
            ))
            .count(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "external while waiting"
    );
    assert_eq!(
        ui.review_feedback.as_ref().unwrap().batch.comments,
        batch.comments
    );
}
#[tokio::test]
async fn feedback_cancellation_and_partial_budget_expose_repair_receipts_without_resolving_comments()
 {
    for cancel in [true, false] {
        let (mut executor, mut ui, _dir, batch) = fixture().await;
        let path = executor.cfg.project_root.join("a.txt");
        let original_after = std::fs::read_to_string(&path).unwrap();
        if !cancel {
            executor.cfg.agent_budget.max_iterations = 1;
        }
        let provider = Provider::new(vec![(200, write(&path)), (200, done())], cancel).await;
        provider.configure(&mut executor);
        let id = executor
            .submit_review_feedback(batch.clone(), &mut ui)
            .unwrap();
        ui.review_feedback.as_mut().unwrap().job_id = Some(id);
        if cancel {
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                while provider.requests.lock().unwrap().len() < 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            executor.jobs.cancel(id);
        }
        terminal(&executor, id).await;
        executor.handle_job_completed(&id.to_string(), &mut ui);
        let messages = notices(&ui);
        assert!(
            messages.iter().any(|m| m.contains("::feedback_outcome:")
                && m.contains(if cancel { "Cancelled" } else { "Partial" })),
            "{messages:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "repair-only change"
        );
        assert_eq!(
            ui.review_feedback.as_ref().unwrap().batch.comments,
            batch.comments
        );
        let repair = ui.diff_review.as_ref().unwrap().review_id.clone().unwrap();
        assert_ne!(Some(&repair), batch.source.review_id.as_ref());
        let report = executor
            .tools
            .reject_review(&repair, &tokio_util::sync::CancellationToken::new())
            .await;
        assert!(report.error.is_none(), "{report:?}");
        assert_eq!(std::fs::read_to_string(path).unwrap(), original_after);
    }
}
#[tokio::test]
async fn feedback_shutdown_spawn_rejection_and_cancel_waiting_gate_keep_comments() {
    let (mut executor, mut ui, _dir, batch) = fixture().await;
    let provider = Provider::new(vec![(200, done())], false).await;
    provider.configure(&mut executor);
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, held) = tokio::sync::oneshot::channel();
    executor
        .jobs
        .spawn(
            JobSpec::new(
                JobKind::Test,
                JobScope::Background,
                WorkspaceAccess::ReadOnly,
                "hold gate",
            ),
            move |_| async move {
                started.send(()).unwrap();
                let _ = held.await;
                JobRunOutcome::Completed
            },
        )
        .unwrap();
    ready.await.unwrap();
    let id = executor
        .submit_review_feedback(batch.clone(), &mut ui)
        .unwrap();
    ui.review_feedback.as_mut().unwrap().job_id = Some(id);
    executor.jobs.cancel(id);
    terminal(&executor, id).await;
    executor.handle_job_completed(&id.to_string(), &mut ui);
    assert!(provider.requests.lock().unwrap().is_empty());
    assert_eq!(
        ui.review_feedback.as_ref().unwrap().batch.comments,
        batch.comments
    );
    release.send(()).unwrap();
    executor
        .jobs
        .shutdown(std::time::Duration::from_secs(1))
        .await;
    let mut retry = batch;
    retry.revision += 1;
    assert!(executor.submit_review_feedback(retry, &mut ui).is_err());
    assert!(provider.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn feedback_terminal_persistence_failure_overrides_premature_completion_notice() {
    let (mut executor, mut ui, _dir, _batch) = fixture().await;
    let id = executor
        .jobs
        .spawn(
            JobSpec::new(
                JobKind::AgentTurn,
                JobScope::Foreground,
                WorkspaceAccess::Write,
                "persistence failure",
            ),
            |_| async {
                JobRunOutcome::Failed {
                    message: "Could not save session checkpoint: fixture".into(),
                }
            },
        )
        .unwrap();
    ui.latest_agent_job_id = Some(id);
    let draft = ui.review_feedback.as_mut().unwrap();
    draft.job_id = Some(id);
    draft.outcome = Some("Completed (inspect changes)".into());
    terminal(&executor, id).await;
    executor.handle_job_completed(&id.to_string(), &mut ui);
    assert!(
        ui.review_feedback
            .as_ref()
            .unwrap()
            .outcome
            .as_ref()
            .unwrap()
            .starts_with("Failed: Could not save session checkpoint")
    );
}

#[tokio::test]
async fn feedback_history_two_managed_repairs_preserve_sources_and_second_only_rollback() {
    let (mut executor, mut ui, _dir, mut original) = fixture().await;
    original.comments[0].anchor.selection = Some(
        crate::features::review_feedback::selection::select(
            &original.comments[0].anchor.hunk,
            0,
            0,
        )
        .unwrap(),
    );
    ui.review_feedback.as_mut().unwrap().batch = original.clone();
    let path = executor.cfg.project_root.join("a.txt");
    let mut second = write(&path);
    second["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"] =
        json!({"path":path,"content":"second repair 日本語"})
            .to_string()
            .into();
    let provider = Provider::new(
        vec![
            (200, write(&path)),
            (200, done()),
            (200, second),
            (200, done()),
        ],
        false,
    )
    .await;
    provider.configure(&mut executor);
    ui.handler = Some(Box::new(executor));
    ui.confirm_feedback();
    ui.submit_feedback();
    let first = ui.review_feedback.as_ref().unwrap().job_id.unwrap();
    terminal(
        ui.handler
            .as_ref()
            .unwrap()
            .as_any()
            .downcast_ref::<TuiExecutor>()
            .unwrap(),
        first,
    )
    .await;
    let mut handler = ui.handler.take().unwrap();
    handler.handle_job_completed(&first.to_string(), &mut ui);
    ui.handler = Some(handler);
    let first_repair = ui.diff_review.as_ref().unwrap().source.clone();
    // An external edit blocks a new batch without discarding the submitted text.
    ui.open_comment_list();
    ui.handle_feedback_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('e'),
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(ui.comment_editor.is_none()); // original capture expired after first repair
    ui.handle_feedback_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    ));
    std::fs::write(&path, "external edit").unwrap();
    ui.start_fresh_feedback();
    assert!(ui.feedback_error.is_some());
    assert_eq!(ui.feedback_history.len(), 0);
    assert_eq!(
        ui.review_feedback.as_ref().unwrap().batch.comments,
        original.comments
    );
    std::fs::write(&path, "repair-only change").unwrap();
    ui.start_fresh_feedback();
    assert!(ui.feedback_error.is_none(), "{:?}", ui.feedback_error);
    assert_eq!(ui.feedback_history.len(), 1);
    let fresh = ui.review_feedback.as_ref().unwrap().batch.id.clone();
    assert_ne!(fresh, original.id);
    assert_eq!(
        ui.review_feedback.as_ref().unwrap().batch.source.review_id,
        first_repair.review_id
    );
    assert!(
        ui.review_feedback
            .as_ref()
            .unwrap()
            .batch
            .comments
            .is_empty()
    );
    assert_eq!(provider.requests.lock().unwrap().len(), 2);
    ui.start_line_selection();
    ui.handle_feedback_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(ui.comment_editor.is_some());
    ui.handle_paste("二回目だけを改善してください");
    ui.handle_feedback_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ));
    ui.open_comment_list();
    ui.handle_feedback_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('d'),
        crossterm::event::KeyModifiers::NONE,
    ));
    std::fs::write(&path, "external during delete confirmation").unwrap();
    ui.handle_feedback_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(ui.review_feedback.as_ref().unwrap().batch.comments.len(), 1);
    assert!(ui.comment_list.as_ref().unwrap().error.is_some());
    std::fs::write(&path, "repair-only change").unwrap();
    ui.handle_feedback_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    ));
    ui.handle_feedback_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('e'),
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(
        ui.comment_editor
            .as_ref()
            .unwrap()
            .anchor
            .selection
            .is_some()
    );
    ui.handle_feedback_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(ui.comment_list.is_some());
    assert!(ui.comment_list.as_ref().unwrap().error.is_none());
    assert_eq!(provider.requests.lock().unwrap().len(), 2);
    ui.handle_feedback_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('s'),
        crossterm::event::KeyModifiers::NONE,
    ));
    ui.submit_feedback();
    let second = ui.review_feedback.as_ref().unwrap().job_id.unwrap();
    assert_ne!(first, second);
    terminal(
        ui.handler
            .as_ref()
            .unwrap()
            .as_any()
            .downcast_ref::<TuiExecutor>()
            .unwrap(),
        second,
    )
    .await;
    let mut handler = ui.handler.take().unwrap();
    handler.handle_job_completed(&second.to_string(), &mut ui);
    let second_review = ui.diff_review.as_ref().unwrap().source.clone();
    handler.handle_job_completed(&first.to_string(), &mut ui);
    assert_eq!(
        ui.diff_review.as_ref().unwrap().source.review_id,
        second_review.review_id
    );
    assert_eq!(ui.review_feedback.as_ref().unwrap().batch.id, fresh);
    assert_eq!(
        ui.feedback_history.get(0).unwrap().batch.comments,
        original.comments
    );
    assert_eq!(
        ui.feedback_history.get(0).unwrap().batch.source.diff,
        original.source.diff
    );
    assert_eq!(provider.requests.lock().unwrap().len(), 4);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "second repair 日本語"
    );
    let executor = handler.as_any().downcast_ref::<TuiExecutor>().unwrap();
    let report = executor
        .tools
        .reject_review(
            second_review.review_id.as_deref().unwrap(),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert!(report.error.is_none(), "{report:?}");
    assert_eq!(std::fs::read_to_string(path).unwrap(), "repair-only change");
}
