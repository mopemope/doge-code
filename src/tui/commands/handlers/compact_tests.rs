use super::*;
use crate::config::AppConfig;
use crate::jobs::JobStatus;
use crate::llm::ChatHistory;
use crate::session::{SessionManager, SessionStore};
use std::sync::{Arc, Mutex};

fn fixture() -> (TuiExecutor, TuiApp, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp project");
    let cfg = AppConfig {
        project_root: dir.path().to_path_buf(),
        no_repomap: true,
        api_key: Some("mock".into()),
        base_url: "http://127.0.0.1:1".into(),
        ..Default::default()
    };
    let repomap = Arc::new(tokio::sync::RwLock::new(None));
    let tools = crate::tools::FsTools::new(repomap.clone(), Arc::new(cfg.clone()));
    let manager = Arc::new(Mutex::new(SessionManager::with_store(
        SessionStore::new(dir.path().join("sessions")).expect("store"),
    )));
    let executor = TuiExecutor::construct_with_session_manager(cfg, repomap, tools, manager)
        .expect("executor");
    let mut history = ChatHistory::new();
    for n in 0..6 {
        history.append_user(format!("older message {n}"));
    }
    let messages = history.snapshot();
    executor
        .session_manager
        .lock()
        .unwrap()
        .update_current_session_with_history(&messages)
        .unwrap();
    *executor.conversation_history.lock().unwrap() = history;
    let mut ui = TuiApp::new_for_test("compact", None, "default");
    ui.status = Status::Ready;
    (executor, ui, dir)
}

#[tokio::test]
async fn manual_compact_reserves_foreground_before_returning() {
    let (mut executor, mut ui, _dir) = fixture();
    executor.handle_compact_command(&mut ui);
    let id = executor
        .jobs
        .foreground_id()
        .expect("compact must own foreground");
    assert!(executor.jobs.get_snapshot(id).is_some());
    assert!(executor.ensure_session_idle().is_err());
    executor
        .jobs
        .shutdown(std::time::Duration::from_secs(1))
        .await;
    assert_eq!(
        executor.jobs.get_snapshot(id).unwrap().status,
        JobStatus::Cancelled
    );
}

#[derive(Clone)]
struct MockState {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    response: serde_json::Value,
    status: axum::http::StatusCode,
}
struct MockProvider {
    state: MockState,
    url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for MockProvider {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl MockProvider {
    async fn new(summary: &str, finish: &str, status: axum::http::StatusCode) -> Self {
        let state = MockState {
            started: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
            requests: Arc::new(Mutex::new(Vec::new())),
            status,
            response: serde_json::json!({"choices":[{"index":0,"finish_reason":finish,"message":{"role":"assistant","content":summary}}],
                "usage":{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15}}),
        };
        async fn reply(
            axum::extract::State(state): axum::extract::State<MockState>,
            axum::Json(body): axum::Json<serde_json::Value>,
        ) -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
            state.requests.lock().unwrap().push(body);
            state.started.notify_one();
            state.release.notified().await;
            (state.status, axum::Json(state.response))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock listener");
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = axum::Router::new()
            .route("/v1/chat/completions", axum::routing::post(reply))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock serve");
        });
        Self { state, url, task }
    }
    async fn started(&self) {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.state.started.notified(),
        )
        .await
        .expect("provider request started");
    }
    fn release(&self) {
        self.state.release.notify_one();
    }
    fn count(&self) -> usize {
        self.state.requests.lock().unwrap().len()
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
async fn terminal(executor: &TuiExecutor, id: crate::jobs::JobId) -> crate::jobs::JobSnapshot {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let snapshot = executor.jobs.get_snapshot(id).expect("job snapshot");
            if snapshot.status.is_terminal() {
                return snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("job terminates")
}
fn memory(executor: &TuiExecutor) -> serde_json::Value {
    serde_json::json!({"session": executor.session_manager.lock().unwrap().current_session,
        "runtime": executor.conversation_history.lock().unwrap().snapshot()})
}
fn disk_path(executor: &TuiExecutor, dir: &tempfile::TempDir) -> std::path::PathBuf {
    let id = executor
        .session_manager
        .lock()
        .unwrap()
        .current_session_id()
        .unwrap();
    dir.path().join("sessions").join(id).join("session.json")
}
fn notices(ui: &TuiApp) -> Vec<String> {
    ui.inbox_rx.as_ref().unwrap().try_iter().collect()
}

#[tokio::test]
async fn manual_compact_rejects_overlap_and_saves_before_adopting() {
    use crate::tui::commands::core::CommandHandler;
    let mock = MockProvider::new("summary", "stop", axum::http::StatusCode::OK).await;
    let (mut executor, mut ui, dir) = fixture();
    mock.configure(&mut executor);
    let before = memory(&executor);
    executor.handle_compact_command(&mut ui);
    let id = executor.jobs.foreground_id().unwrap();
    assert_eq!(
        executor.jobs.get_snapshot(id).unwrap().kind,
        crate::jobs::JobKind::Compact
    );
    mock.started().await;
    executor.handle_compact_command(&mut ui);
    executor.handle("/quick successor", &mut ui);
    assert_eq!(executor.jobs.foreground_id(), Some(id));
    assert_eq!(mock.count(), 1);
    assert_eq!(memory(&executor), before);
    assert!(
        executor
            .switch_to_session(
                &executor
                    .session_manager
                    .lock()
                    .unwrap()
                    .current_session_id()
                    .unwrap()
            )
            .is_err()
    );
    assert!(executor.clear_runtime_conversation().is_err());
    mock.release();
    assert_eq!(terminal(&executor, id).await.status, JobStatus::Completed);
    let path = disk_path(&executor, &dir);
    let saved: crate::session::SessionData =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(saved.conversation_messages().unwrap()).unwrap(),
        serde_json::to_value(executor.conversation_history.lock().unwrap().snapshot()).unwrap()
    );
    assert_eq!(
        saved.usage,
        executor
            .session_manager
            .lock()
            .unwrap()
            .current_session
            .as_ref()
            .unwrap()
            .usage
    );
    assert_eq!(
        serde_json::to_value(saved.usage).unwrap(),
        before["session"]["usage"]
    );
    assert_eq!(
        saved.requests,
        before["session"]["requests"].as_u64().unwrap()
    );
    let messages = notices(&ui);
    assert!(messages.iter().any(|m| m.contains("[SUCCESS]")));
    assert!(
        messages
            .iter()
            .any(|m| m == &format!("::job_completed:{id}"))
    );
    assert!(!messages.iter().any(|m| m == "::status:done"));
    // A delayed completion must leave a successor's status and timer intact.
    let successor = executor
        .jobs
        .spawn(
            JobSpec::new(
                JobKind::AgentTurn,
                JobScope::Foreground,
                WorkspaceAccess::None,
                "successor",
            ),
            |ctx| async move {
                ctx.cancellation.cancelled().await;
                JobRunOutcome::Cancelled
            },
        )
        .unwrap();
    ui.status = Status::Thinking;
    let started = std::time::Instant::now();
    ui.processing_start_time = Some(started);
    assert!(executor.handle_compact_completed(&id.to_string(), &mut ui));
    assert_eq!(ui.status, Status::Thinking);
    assert_eq!(ui.processing_start_time, Some(started));
    executor.jobs.cancel(successor);
    terminal(&executor, successor).await;
}

#[tokio::test]
async fn manual_compact_cancel_and_shutdown_drop_unresponsive_provider() {
    for shutdown in [false, true] {
        let mock = MockProvider::new("late summary", "stop", axum::http::StatusCode::OK).await;
        let (mut executor, mut ui, dir) = fixture();
        mock.configure(&mut executor);
        let before = memory(&executor);
        let path = disk_path(&executor, &dir);
        let disk = std::fs::read(path).unwrap();
        executor.handle_compact_command(&mut ui);
        let id = executor.jobs.foreground_id().unwrap();
        mock.started().await;
        if shutdown {
            executor
                .jobs
                .shutdown(std::time::Duration::from_millis(100))
                .await;
        } else {
            crate::tui::commands::handlers::slash_commands::cancel::handle_cancel(
                &mut executor,
                &mut ui,
            );
        }
        assert_eq!(terminal(&executor, id).await.status, JobStatus::Cancelled);
        mock.release();
        tokio::task::yield_now().await;
        assert_eq!(memory(&executor), before);
        assert_eq!(std::fs::read(disk_path(&executor, &dir)).unwrap(), disk);
        assert!(
            !notices(&ui).iter().any(|m| m.contains("SUCCESS")
                || m == "::status:done"
                || m == "::status:cancelled")
        );
        assert!(executor.handle_compact_completed(&id.to_string(), &mut ui));
        assert_eq!(ui.status, Status::Ready);
    }
}

#[tokio::test]
async fn manual_compact_discards_changed_session_and_runtime_snapshots() {
    for change in ["switch", "session", "runtime"] {
        let mock = MockProvider::new("obsolete summary", "stop", axum::http::StatusCode::OK).await;
        let (mut executor, mut ui, dir) = fixture();
        mock.configure(&mut executor);
        executor.handle_compact_command(&mut ui);
        let id = executor.jobs.foreground_id().unwrap();
        mock.started().await;
        match change {
            "switch" => executor
                .session_manager
                .lock()
                .unwrap()
                .create_session(Some("forced switch".into()))
                .unwrap(),
            "session" => {
                executor
                    .session_manager
                    .lock()
                    .unwrap()
                    .current_session
                    .as_mut()
                    .unwrap()
                    .requests += 1
            }
            _ => executor
                .conversation_history
                .lock()
                .unwrap()
                .append_user("forced edit"),
        }
        let changed = memory(&executor);
        let path = disk_path(&executor, &dir);
        let disk = std::fs::read(&path).unwrap();
        mock.release();
        assert_eq!(terminal(&executor, id).await.status, JobStatus::Failed);
        assert_eq!(memory(&executor), changed);
        assert_eq!(std::fs::read(path).unwrap(), disk);
        assert!(!notices(&ui).iter().any(|m| m.contains("SUCCESS")));
    }
}

#[tokio::test]
async fn manual_compact_invalid_provider_results_preserve_disk_and_memory() {
    for (summary, finish, status) in [
        ("", "stop", 200),
        ("  \n\t", "stop", 200),
        ("truncated", "length", 200),
        ("", "stop", 400),
    ] {
        let mock = MockProvider::new(
            summary,
            finish,
            axum::http::StatusCode::from_u16(status).unwrap(),
        )
        .await;
        let (mut executor, mut ui, dir) = fixture();
        mock.configure(&mut executor);
        let before = memory(&executor);
        let path = disk_path(&executor, &dir);
        let disk = std::fs::read(&path).unwrap();
        executor.handle_compact_command(&mut ui);
        let id = executor.jobs.foreground_id().unwrap();
        mock.started().await;
        mock.release();
        assert_eq!(terminal(&executor, id).await.status, JobStatus::Failed);
        assert_eq!(memory(&executor), before);
        assert_eq!(std::fs::read(path).unwrap(), disk);
        assert!(!notices(&ui).iter().any(|m| m.contains("SUCCESS")));
        executor.handle_compact_completed(&id.to_string(), &mut ui);
        assert_eq!(ui.status, Status::Error);
    }
}

#[tokio::test]
async fn manual_compact_save_failure_is_transactional() {
    let mock = MockProvider::new(
        "summary must not be adopted",
        "stop",
        axum::http::StatusCode::OK,
    )
    .await;
    let (mut executor, mut ui, dir) = fixture();
    mock.configure(&mut executor);
    let before = memory(&executor);
    let path = disk_path(&executor, &dir);
    let disk = std::fs::read(&path).unwrap();
    executor.handle_compact_command(&mut ui);
    let id = executor.jobs.foreground_id().unwrap();
    mock.started().await;
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions).unwrap();
    mock.release();
    assert_eq!(terminal(&executor, id).await.status, JobStatus::Failed);
    assert_eq!(memory(&executor), before);
    assert_eq!(std::fs::read(path).unwrap(), disk);
    assert!(!notices(&ui).iter().any(|m| m.contains("SUCCESS")));
}

#[tokio::test]
async fn manual_compact_commit_checks_cancel_and_capacity_without_adoption() {
    let (executor, _ui, dir) = fixture();
    let original = executor.conversation_history.lock().unwrap().snapshot();
    let session = executor
        .session_manager
        .lock()
        .unwrap()
        .current_session
        .clone()
        .unwrap();
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let before = memory(&executor);
    let path = disk_path(&executor, &dir);
    let disk = std::fs::read(&path).unwrap();
    assert!(
        commit_candidate(
            &executor.conversation_history,
            &executor.session_manager,
            &original,
            &session,
            (vec![], Default::default(), Default::default()),
            &token
        )
        .unwrap()
        .is_none()
    );
    let mut oversized = ChatHistory::new();
    oversized.append_user("x".repeat(17 * 1024 * 1024));
    assert!(
        commit_candidate(
            &executor.conversation_history,
            &executor.session_manager,
            &original,
            &session,
            (oversized.snapshot(), Default::default(), Default::default()),
            &tokio_util::sync::CancellationToken::new()
        )
        .is_err()
    );
    assert_eq!(memory(&executor), before);
    assert_eq!(std::fs::read(path).unwrap(), disk);
}

fn message(role: &str, content: &str) -> crate::llm::ChatMessage {
    crate::llm::ChatMessage {
        role: role.into(),
        content: Some(content.into()),
        provider_state: None,
        tool_calls: vec![],
        tool_call_id: None,
    }
}
fn call(ids: &[&str]) -> crate::llm::ChatMessage {
    crate::llm::ChatMessage {
        role: "assistant".into(),
        content: None,
        provider_state: None,
        tool_call_id: None,
        tool_calls: ids
            .iter()
            .map(|id| crate::llm::ToolCall {
                id: Some((*id).into()),
                r#type: "function".into(),
                function: crate::llm::ToolCallFunction {
                    name: "fs_read".into(),
                    arguments: "{}".into(),
                },
            })
            .collect(),
    }
}
fn tool(id: &str, content: &str) -> crate::llm::ChatMessage {
    let mut msg = message("tool", content);
    msg.tool_call_id = Some(id.into());
    msg
}
fn seed(
    executor: &TuiExecutor,
    messages: Vec<crate::llm::ChatMessage>,
    store: crate::llm::observation::ObservationStore,
    unseen: std::collections::BTreeSet<String>,
) {
    executor
        .session_manager
        .lock()
        .unwrap()
        .update_current_session_with_history_and_observations(&messages, Some(store), Some(unseen))
        .unwrap();
    executor
        .conversation_history
        .lock()
        .unwrap()
        .replace(messages);
}

#[tokio::test]
async fn manual_compact_keeps_unseen_batch_and_recoverable_observations() {
    let (mut executor, mut ui, dir) = fixture();
    let mut store = crate::llm::observation::ObservationStore::new();
    let handle = store
        .insert(
            "seen".into(),
            "fs_read".into(),
            "recoverable file content".into(),
            500,
        )
        .unwrap();
    let mock = MockProvider::new(
        &format!("summary referring to {handle}"),
        "stop",
        axum::http::StatusCode::OK,
    )
    .await;
    mock.configure(&mut executor);
    let mut messages: Vec<_> = (0..6)
        .map(|n| message("user", &format!("old {n} {}", "x".repeat(4000))))
        .collect();
    messages.push(call(&["seen"]));
    messages.push(tool(
        "seen",
        &crate::llm::observation::observation_stub("fs_read", 500, &handle),
    ));
    let protected = vec![
        call(&["unseen-a", "unseen-b"]),
        tool("unseen-a", "never read secret tool result A"),
        tool("unseen-b", "never read secret tool result B"),
        message("user", "latest"),
    ];
    messages.extend(protected.clone());
    // Runtime system notes must not become durable messages.
    messages.insert(0, message("system", "request-scoped runtime note"));
    let unseen = ["unseen-a".to_string(), "unseen-b".to_string()]
        .into_iter()
        .collect();
    seed(&executor, messages, store, unseen);
    executor.handle_compact_command(&mut ui);
    let id = executor.jobs.foreground_id().unwrap();
    mock.started().await;
    let sent = serde_json::to_string(&mock.state.requests.lock().unwrap()[0]).unwrap();
    assert!(!sent.contains("never read secret"));
    assert!(!sent.contains("request-scoped runtime note"));
    mock.release();
    assert_eq!(terminal(&executor, id).await.status, JobStatus::Completed);
    let runtime = executor.conversation_history.lock().unwrap().snapshot();
    assert_eq!(
        serde_json::to_value(&runtime[runtime.len() - protected.len()..]).unwrap(),
        serde_json::to_value(&protected).unwrap()
    );
    assert!(runtime.iter().all(|m| m.role != "system"));
    let saved: crate::session::SessionData =
        serde_json::from_slice(&std::fs::read(disk_path(&executor, &dir)).unwrap()).unwrap();
    assert_eq!(
        saved.unseen_tool_results,
        ["unseen-a".into(), "unseen-b".into()].into_iter().collect()
    );
    assert_eq!(
        saved.observations.get(&handle).unwrap().content,
        "recoverable file content"
    );
    assert_eq!(
        serde_json::to_value(saved.conversation_messages().unwrap()).unwrap(),
        serde_json::to_value(runtime).unwrap()
    );
    executor.resume_session(&saved.meta.id).unwrap();
    assert_eq!(
        serde_json::to_value(executor.conversation_history.lock().unwrap().snapshot()).unwrap(),
        serde_json::to_value(saved.conversation_messages().unwrap()).unwrap()
    );
}

#[tokio::test]
async fn manual_compact_without_safe_prefix_makes_no_request() {
    for orphan in [false, true] {
        let mock =
            MockProvider::new("should not be called", "stop", axum::http::StatusCode::OK).await;
        let (mut executor, mut ui, dir) = fixture();
        mock.configure(&mut executor);
        let mut messages = if orphan {
            vec![]
        } else {
            vec![call(&["unseen"])]
        };
        messages.push(tool("unseen", "unread result"));
        messages.push(message("user", "latest"));
        messages.push(message("assistant", "extra"));
        seed(
            &executor,
            messages,
            Default::default(),
            ["unseen".into()].into_iter().collect(),
        );
        let before = memory(&executor);
        let disk = std::fs::read(disk_path(&executor, &dir)).unwrap();
        executor.handle_compact_command(&mut ui);
        let id = executor.jobs.foreground_id().unwrap();
        assert_eq!(terminal(&executor, id).await.status, JobStatus::Completed);
        assert_eq!(mock.count(), 0);
        assert_eq!(memory(&executor), before);
        assert_eq!(std::fs::read(disk_path(&executor, &dir)).unwrap(), disk);
        assert!(!notices(&ui).iter().any(|m| m.contains("SUCCESS")));
    }
}

#[tokio::test]
async fn manual_compact_commit_success_cannot_be_undone_by_late_cancel() {
    let (executor, _ui, dir) = fixture();
    let original = executor.conversation_history.lock().unwrap().snapshot();
    let session = executor
        .session_manager
        .lock()
        .unwrap()
        .current_session
        .clone()
        .unwrap();
    let token = tokio_util::sync::CancellationToken::new();
    assert!(
        commit_candidate(
            &executor.conversation_history,
            &executor.session_manager,
            &original,
            &session,
            (
                vec![message("user", "saved summary")],
                Default::default(),
                Default::default()
            ),
            &token
        )
        .unwrap()
        .is_some()
    );
    token.cancel();
    assert_eq!(
        executor.conversation_history.lock().unwrap().snapshot()[0]
            .content
            .as_deref(),
        Some("saved summary")
    );
    let saved: crate::session::SessionData =
        serde_json::from_slice(&std::fs::read(disk_path(&executor, &dir)).unwrap()).unwrap();
    assert_eq!(
        saved.conversation_messages().unwrap()[0].content.as_deref(),
        Some("saved summary")
    );
}

#[tokio::test]
async fn manual_compact_post_replace_sync_failure_adopts_with_warning() {
    let mock = MockProvider::new(
        "applied but durability uncertain",
        "stop",
        axum::http::StatusCode::OK,
    )
    .await;
    let (mut executor, mut ui, dir) = fixture();
    mock.configure(&mut executor);
    let before = memory(&executor);
    let path = disk_path(&executor, &dir);
    let disk = std::fs::read(&path).unwrap();
    executor.handle_compact_command(&mut ui);
    let id = executor.jobs.foreground_id().unwrap();
    mock.started().await;
    executor
        .session_manager
        .lock()
        .unwrap()
        .store
        .fail_directory_sync = true;
    mock.release();
    assert_eq!(terminal(&executor, id).await.status, JobStatus::Completed);
    assert_ne!(std::fs::read(&path).unwrap(), disk);
    let saved: crate::session::SessionData =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&saved).unwrap(),
        memory(&executor)["session"]
    );
    assert_eq!(
        serde_json::to_value(saved.conversation_messages().unwrap()).unwrap(),
        memory(&executor)["runtime"]
    );
    assert_eq!(
        memory(&executor)["session"]["usage"],
        before["session"]["usage"]
    );
    let messages = notices(&ui);
    assert!(messages.iter().any(|m| m.contains("[WARN]")
        && m.contains("applied to disk and memory")
        && m.contains("durability is unconfirmed")));
    assert!(
        !messages
            .iter()
            .any(|m| m.contains("SUCCESS") || m.contains("unchanged"))
    );
    executor.handle_compact_completed(&id.to_string(), &mut ui);
    assert_eq!(ui.status, Status::Ready);
}

#[tokio::test]
async fn manual_compact_usage_is_excluded_from_next_agent_checkpoint_and_drop() {
    let mock = MockProvider::new("summary", "stop", axum::http::StatusCode::OK).await;
    let (mut executor, mut ui, _dir) = fixture();
    mock.configure(&mut executor);
    executor.handle_compact_command(&mut ui);
    let id = executor.jobs.foreground_id().unwrap();
    mock.started().await;
    mock.release();
    assert_eq!(terminal(&executor, id).await.status, JobStatus::Completed);
    let session = executor
        .session_manager
        .lock()
        .unwrap()
        .current_session
        .clone()
        .unwrap();
    let client = executor.client.clone().unwrap();
    assert_eq!(client.usage_snapshot().total_tokens, 15);
    let next = crate::llm::tool_execution::history::HistoryManager::with_observations(
        client.clone(),
        session.conversation_messages().unwrap(),
        None,
        executor.tools.clone(),
        executor.cfg.clone(),
        session.observations,
        session.unseen_tool_results,
    );
    client.record_request_attempt();
    client.record_usage(
        &serde_json::from_value(
            serde_json::json!({"prompt_tokens":100,"completion_tokens":50,"total_tokens":150}),
        )
        .unwrap(),
    );
    next.checkpoint().unwrap();
    next.checkpoint().unwrap();
    drop(next);
    let saved = executor
        .session_manager
        .lock()
        .unwrap()
        .current_session
        .clone()
        .unwrap();
    assert_eq!(saved.requests, 1);
    assert_eq!(saved.token_count, 150);
    assert_eq!(saved.usage.unwrap().total_tokens, 150);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manual_compact_shutdown_cannot_terminalize_an_in_progress_commit_as_cancelled() {
    let mock = MockProvider::new(
        "committed despite late shutdown",
        "stop",
        axum::http::StatusCode::OK,
    )
    .await;
    let (mut executor, mut ui, dir) = fixture();
    mock.configure(&mut executor);
    executor.handle_compact_command(&mut ui);
    let id = executor.jobs.foreground_id().unwrap();
    mock.started().await;
    let barrier = Arc::new(std::sync::Barrier::new(2));
    executor
        .session_manager
        .lock()
        .unwrap()
        .store
        .before_sync_barrier = Some(barrier.clone());
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let release = std::thread::spawn(move || {
        barrier.wait();
        entered.send(()).expect("commit entered");
        // Exceed shutdown's grace and first forced-abort wait while save is
        // synchronous. The registry must not publish a false cancellation.
        std::thread::sleep(std::time::Duration::from_millis(750));
        barrier.wait();
    });
    mock.release();
    tokio::time::timeout(std::time::Duration::from_secs(5), entered_rx)
        .await
        .unwrap()
        .unwrap();
    executor
        .jobs
        .shutdown(std::time::Duration::from_millis(1))
        .await;
    release.join().unwrap();
    assert_eq!(
        executor.jobs.get_snapshot(id).unwrap().status,
        JobStatus::Completed
    );
    assert_eq!(executor.jobs.active_count(), 0);
    let saved: crate::session::SessionData =
        serde_json::from_slice(&std::fs::read(disk_path(&executor, &dir)).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&saved).unwrap(),
        memory(&executor)["session"]
    );
    assert_eq!(
        serde_json::to_value(saved.conversation_messages().unwrap()).unwrap(),
        memory(&executor)["runtime"]
    );
    let messages = notices(&ui);
    assert!(messages.iter().any(|m| m.contains("SUCCESS")));
    assert!(
        !messages
            .iter()
            .any(|m| m.contains("cancelled") || m.contains("unchanged"))
    );
    executor.handle_compact_completed(&id.to_string(), &mut ui);
    assert_eq!(ui.status, Status::Ready);
}
