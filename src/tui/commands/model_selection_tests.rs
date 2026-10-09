use super::core::{CommandHandler, TuiExecutor};
use crate::{
    config::AppConfig,
    features::{openai_subscription::ProviderKind, opencode},
    session::{SessionManager, SessionStore},
    tui::state::TuiApp,
};

fn fixture(key: bool) -> (TuiExecutor, TuiApp, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = AppConfig {
        project_root: dir.path().into(),
        provider: ProviderKind::OpencodeGo,
        base_url: opencode::GO_BASE.into(),
        model: "gpt-6-luna".into(),
        api_key: key.then(|| "SYNTHETIC_SECRET".into()),
        no_repomap: true,
        mcp_servers: vec![],
        ..Default::default()
    };
    let map = std::sync::Arc::new(tokio::sync::RwLock::new(None));
    let tools = crate::tools::FsTools::new(map.clone(), std::sync::Arc::new(cfg.clone()));
    let manager = std::sync::Arc::new(std::sync::Mutex::new(SessionManager::with_store(
        SessionStore::new(dir.path().join("sessions")).unwrap(),
    )));
    let executor = TuiExecutor::construct_with_session_manager(cfg, map, tools, manager).unwrap();
    let mut ui = TuiApp::new_for_test("models", None, "default");
    executor.sync_selection_ui(&mut ui);
    (executor, ui, dir)
}
fn id(executor: &TuiExecutor) -> String {
    executor
        .session_manager
        .lock()
        .unwrap()
        .current_session_id()
        .unwrap()
}
fn history(executor: &TuiExecutor) -> Vec<crate::llm::ChatMessage> {
    executor.conversation_history.lock().unwrap().snapshot()
}
fn preserve_history(executor: &TuiExecutor) {
    let messages = vec![crate::llm::ChatMessage {
        role: "assistant".into(),
        content: Some("OLD TOOL HISTORY".into()),
        tool_calls: vec![],
        tool_call_id: None,
        reasoning: Default::default(),
        provider_state: Some(crate::features::openai_subscription::ProviderState {
            version: 1,
            account: "old-account".into(),
            model: "gpt-6-luna".into(),
            output: vec![serde_json::json!({"type":"reasoning","encrypted_content":"OLD_OPAQUE"})],
            additional_tool_names: vec![],
        }),
    }];
    executor.replace_conversation_from_messages(messages.clone());
    let mut sm = executor.session_manager.lock().unwrap();
    sm.update_current_session_with_history(&messages).unwrap();
    let current = sm.current_session.as_mut().unwrap();
    current.activated_tools.insert("fs_read".into());
    current.unseen_tool_results.insert("old-tool-id".into());
    let snapshot = current.clone();
    sm.store
        .save_with_lease(&snapshot, sm.current_lease.as_ref().unwrap())
        .unwrap();
}
#[test]
fn model_selection_new_session_isolates_history_and_restores_selection() {
    let (mut executor, mut ui, _dir) = fixture(true);
    preserve_history(&executor);
    let old_id = id(&executor);
    let old_bytes = {
        let sm = executor.session_manager.lock().unwrap();
        std::fs::read(sm.store.session_dir(&old_id).join("session.json")).unwrap()
    };
    executor.last_user_prompt = Some("old prompt".into());
    ui.last_user_input = Some("old prompt".into());
    let new_id = executor
        .start_model_session("opencode-go/glm-5.3", &mut ui)
        .unwrap();
    assert_ne!(old_id, new_id);
    assert!(history(&executor).is_empty());
    assert_eq!(executor.cfg.model, "glm-5.3");
    assert_eq!(executor.tools.config.model, "glm-5.3");
    assert_eq!(ui.cfg.as_ref().unwrap().model, "glm-5.3");
    assert_eq!(executor.client.as_ref().unwrap().opencode_session, new_id);
    assert!(executor.last_user_prompt.is_none() && ui.last_user_input.is_none());
    let store = executor.session_manager.lock().unwrap().store.clone();
    let new_session = store.load(&new_id).unwrap();
    assert_eq!(
        new_session.model_selection.as_ref().unwrap().model,
        "glm-5.3"
    );
    assert!(
        new_session.conversation.is_empty()
            && new_session.activated_tools.is_empty()
            && new_session.unseen_tool_results.is_empty()
            && new_session.observations.is_empty()
    );
    assert_eq!(
        std::fs::read(store.session_dir(&old_id).join("session.json")).unwrap(),
        old_bytes
    );
    assert!(
        !String::from_utf8(std::fs::read(store.session_dir(&new_id).join("session.json")).unwrap())
            .unwrap()
            .contains("SYNTHETIC_SECRET")
    );
    executor
        .handle_session_command(&format!("switch {old_id}"), &mut ui)
        .unwrap();
    assert_eq!(executor.cfg.model, "gpt-6-luna");
    assert_eq!(ui.model.as_deref(), Some("gpt-6-luna"));
    assert_eq!(
        history(&executor)[0]
            .provider_state
            .as_ref()
            .unwrap()
            .output[0]["encrypted_content"],
        "OLD_OPAQUE"
    );
    executor
        .handle_session_command(&format!("switch {new_id}"), &mut ui)
        .unwrap();
    assert_eq!(executor.cfg.model, "glm-5.3");
    assert!(history(&executor).is_empty());
    let mut cfg = executor.cfg.clone();
    cfg.model = "gpt-6-luna".into();
    cfg.resume = Some(new_id.clone());
    drop(executor);
    let map = std::sync::Arc::new(tokio::sync::RwLock::new(None));
    let tools = crate::tools::FsTools::new(map.clone(), std::sync::Arc::new(cfg.clone()));
    let manager = std::sync::Arc::new(std::sync::Mutex::new(SessionManager::with_store(
        store.clone(),
    )));
    let mut resumed =
        TuiExecutor::construct_with_session_manager(cfg, map, tools, manager).unwrap();
    resumed.resume_session(&new_id).unwrap();
    resumed.sync_selection_ui(&mut ui);
    assert_eq!(resumed.cfg.model, "glm-5.3");
    assert_eq!(ui.cfg.as_ref().unwrap().model, "glm-5.3");
    assert!(ui.inference_label.as_ref().unwrap().contains("glm-5.3"));
    assert_eq!(store.list().unwrap().len(), 2);
}
#[test]
fn model_selection_validation_and_prepare_failures_keep_current_state() {
    let (mut executor, mut ui, _dir) = fixture(true);
    let old = id(&executor);
    for model in ["unknown", "opencode/glm-5.3", "qwen3.8-max"] {
        assert!(executor.start_model_session(model, &mut ui).is_err());
        assert_eq!(id(&executor), old);
        assert_eq!(ui.model.as_deref(), Some("gpt-6-luna"));
    }
    executor.cfg.base_url = opencode::ZEN_BASE.into();
    assert!(executor.start_model_session("glm-5.3", &mut ui).is_err());
    assert_eq!(id(&executor), old);
    assert_eq!(executor.cfg.model, "gpt-6-luna");
    assert_eq!(
        executor
            .session_manager
            .lock()
            .unwrap()
            .store
            .list()
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn model_selection_save_failure_keeps_runtime_client_ui_and_old_checkpoint() {
    let (mut executor, mut ui, dir) = fixture(true);
    preserve_history(&executor);
    let old = id(&executor);
    let root = dir.path().join("sessions");
    let backup = dir.path().join("preserved");
    let before = std::fs::read(root.join(&old).join("session.json")).unwrap();
    std::fs::rename(&root, &backup).unwrap();
    std::fs::write(&root, "not a directory").unwrap();
    assert!(executor.start_model_session("glm-5.3", &mut ui).is_err());
    assert_eq!(id(&executor), old);
    assert_eq!(executor.cfg.model, "gpt-6-luna");
    assert_eq!(ui.cfg.as_ref().unwrap().model, "gpt-6-luna");
    assert_eq!(
        history(&executor)[0].content.as_deref(),
        Some("OLD TOOL HISTORY")
    );
    assert_eq!(
        std::fs::read(backup.join(&old).join("session.json")).unwrap(),
        before
    );
    std::fs::remove_file(&root).unwrap();
    std::fs::rename(backup, root).unwrap();
}
#[test]
fn model_selection_keyless_resume_rejects_provider_and_binding_mismatch() {
    let (mut executor, mut ui, _dir) = fixture(false);
    let old = id(&executor);
    let target = executor.start_model_session("glm-5.3", &mut ui).unwrap();
    executor.switch_to_session(&old).unwrap();
    let sm = executor.session_manager.lock().unwrap();
    let mut bad = sm.store.load(&target).unwrap();
    bad.model_selection.as_mut().unwrap().provider = ProviderKind::OpencodeZen;
    sm.store.save(&bad).unwrap();
    drop(sm);
    assert!(executor.switch_to_session(&target).is_err());
    assert_eq!(id(&executor), old);
    assert_eq!(executor.cfg.model, "gpt-6-luna");
    let sm = executor.session_manager.lock().unwrap();
    bad.model_selection.as_mut().unwrap().provider = ProviderKind::OpencodeGo;
    bad.inference_binding = Some("another-model".into());
    sm.store.save(&bad).unwrap();
    drop(sm);
    assert!(executor.resume_session(&target).is_err());
    assert_eq!(id(&executor), old);
}
#[test]
fn model_selection_never_prunes_existing_sessions_even_after_exit_flush() {
    let (mut executor, mut ui, _dir) = fixture(false);
    let store = executor.session_manager.lock().unwrap().store.clone();
    let mut existing = Vec::new();
    for _ in 0..105 {
        let mut data = crate::session::SessionData::new();
        data.model_selection = Some(crate::session::data::SessionModelSelection {
            provider: ProviderKind::OpencodeGo,
            model: "gpt-6-luna".into(),
        });
        store.save(&data).unwrap();
        existing.push(data.meta.id);
    }
    // Reproduce the previously unsafe legacy dirty pre-transition flush.
    let original = id(&executor);
    {
        let mut sm = executor.session_manager.lock().unwrap();
        sm.current_session.as_mut().unwrap().model_selection = None;
        let path = sm
            .store
            .session_dir(&sm.current_session_id().unwrap())
            .join("session.json");
        let permissions = std::fs::metadata(&path).unwrap().permissions();
        let mut readonly = permissions.clone();
        readonly.set_readonly(true);
        std::fs::set_permissions(&path, readonly).unwrap();
        assert!(sm.flush_current_session().is_err());
        assert!(sm.has_unsaved_current_session());
        std::fs::set_permissions(&path, permissions).unwrap();
    }
    executor.start_new_session(&mut ui, None).unwrap();
    {
        let mut sm = executor.session_manager.lock().unwrap();
        sm.current_session.as_mut().unwrap().model_selection = None;
        let path = sm
            .store
            .session_dir(&sm.current_session_id().unwrap())
            .join("session.json");
        let permissions = std::fs::metadata(&path).unwrap().permissions();
        let mut readonly = permissions.clone();
        readonly.set_readonly(true);
        std::fs::set_permissions(&path, readonly).unwrap();
        assert!(sm.flush_current_session().is_err());
        assert!(sm.has_unsaved_current_session());
        std::fs::set_permissions(&path, permissions).unwrap();
    }
    executor.switch_to_session(&original).unwrap();
    executor.start_model_session("glm-5.3", &mut ui).unwrap();
    executor.flush_session().unwrap();
    assert_eq!(store.list().unwrap().len(), 108);
    for id in existing {
        assert!(store.load(&id).is_ok());
    }
}
#[tokio::test]
async fn model_selection_rejects_background_jobs_and_queued_input() {
    use crate::jobs::{JobKind, JobRunOutcome, JobScope, JobSpec, WorkspaceAccess};
    let (mut executor, mut ui, _dir) = fixture(false);
    let old = id(&executor);
    ui.pending_instructions.push_back("previous prompt".into());
    assert!(executor.start_model_session("glm-5.3", &mut ui).is_err());
    ui.pending_instructions.clear();
    let job = executor
        .jobs
        .spawn(
            JobSpec::new(
                JobKind::AgentTurn,
                JobScope::Background,
                WorkspaceAccess::None,
                "background",
            ),
            |ctx| async move {
                ctx.cancellation.cancelled().await;
                JobRunOutcome::Cancelled
            },
        )
        .unwrap();
    assert!(executor.start_model_session("glm-5.3", &mut ui).is_err());
    assert!(executor.switch_to_session(&old).is_err());
    assert!(executor.resume_session(&old).is_err());
    assert_eq!(id(&executor), old);
    executor.jobs.cancel(job);
    executor
        .jobs
        .shutdown(std::time::Duration::from_secs(1))
        .await;
}
#[test]
fn model_selection_picker_search_unsupported_cancel_paste_and_reopen() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let (mut executor, mut ui, _dir) = fixture(false);
    let old = id(&executor);
    ui.textarea.insert_str("keep draft");
    for _ in 0..3 {
        executor.handle("/models", &mut ui);
        ui.handle_paste("qwen3.8-max");
        assert_eq!(ui.model_picker.as_ref().unwrap().query, "qwen3.8-max");
        crate::tui::event_handlers::handle_model_picker_key(
            &mut ui,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .unwrap();
        assert!(
            ui.model_picker
                .as_ref()
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("unsupported")
        );
        assert_eq!(ui.textarea.lines(), ["keep draft"]);
        crate::tui::event_handlers::handle_model_picker_key(
            &mut ui,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        )
        .unwrap();
        assert!(ui.model_picker.is_none());
    }
    executor.handle("/models NO_MATCH", &mut ui);
    assert!(ui.model_picker.as_ref().unwrap().results().is_empty());
    crate::tui::event_handlers::handle_model_picker_key(
        &mut ui,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();
    assert!(ui.model_picker.as_ref().unwrap().error.is_some());
    crate::tui::event_handlers::handle_model_picker_key(
        &mut ui,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    )
    .unwrap();
    assert!(ui.model_picker.is_none());
    assert_eq!(id(&executor), old);
    assert_eq!(ui.textarea.lines(), ["keep draft"]);
}

#[test]
fn model_selection_picker_renders_supported_unsupported_and_empty_at_terminal_widths() {
    use ratatui::{Terminal, backend::TestBackend};
    for width in [40, 80, 110] {
        let (_executor, mut ui, _dir) = fixture(false);
        let mut terminal = Terminal::new(TestBackend::new(width, 32)).unwrap();
        for (query, marker) in [
            ("glm-5.3", "Chat Completions"),
            ("qwen3.8-max", "unsupported API adapter"),
            ("NO_MATCH", "No matching models"),
        ] {
            ui.model_picker = Some(
                crate::features::model_selection::ModelPicker::new(ProviderKind::OpencodeGo, query)
                    .unwrap(),
            );
            terminal.draw(|frame| ui.view(frame, None)).unwrap();
            let screen = terminal.backend().to_string();
            assert!(screen.contains("Models: opencode-go"), "{screen}");
            assert!(screen.contains(marker), "{screen}");
            assert!(screen.contains("Enter: NEW session"), "{screen}");
        }
    }
}
#[test]
fn model_selection_enter_commits_once_and_repeats_do_not_create_sessions() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    let (executor, ui, _dir) = fixture(false);
    let manager = executor.session_manager.clone();
    let mut ui = ui.with_handler(Box::new(executor));
    ui.dispatch("/models glm-5.3");
    let mut repeat = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    repeat.kind = KeyEventKind::Repeat;
    crate::tui::event_handlers::handle_model_picker_key(&mut ui, repeat).unwrap();
    assert_eq!(manager.lock().unwrap().store.list().unwrap().len(), 1);
    crate::tui::event_handlers::handle_model_picker_key(
        &mut ui,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();
    assert!(ui.model_picker.is_none());
    assert_eq!(manager.lock().unwrap().store.list().unwrap().len(), 2);
    assert_eq!(ui.model.as_deref(), Some("glm-5.3"));
    crate::tui::event_handlers::handle_model_picker_key(
        &mut ui,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(manager.lock().unwrap().store.list().unwrap().len(), 2);
}

#[test]
fn model_selection_postrename_sync_warning_keeps_disk_and_runtime_consistent() {
    let (mut executor, mut ui, _dir) = fixture(false);
    executor
        .session_manager
        .lock()
        .unwrap()
        .store
        .fail_directory_sync = true;
    let selected = executor.start_model_session("glm-5.3", &mut ui).unwrap();
    assert_eq!(id(&executor), selected);
    assert_eq!(ui.model.as_deref(), Some("glm-5.3"));
    let sm = executor.session_manager.lock().unwrap();
    assert_eq!(
        sm.store
            .load(&selected)
            .unwrap()
            .model_selection
            .unwrap()
            .model,
        executor.cfg.model
    );
    assert!(sm.checkpoint_warning().is_some());
    assert!(ui.log.iter().any(|entry| matches!(entry, crate::tui::state::LogEntry::Plain(text) if text.contains("Warning:"))));
}

#[test]
fn model_selection_allows_idle_persistent_shell_pty() {
    let (mut executor, mut ui, _dir) = fixture(false);
    let (tx, _rx) = std::sync::mpsc::channel();
    ui.shell_session = Some(crate::tui::shell::ShellSession::new(tx).unwrap());
    executor.handle("/models glm-5.3", &mut ui);
    assert!(ui.model_picker.is_some());
    executor.start_model_session("glm-5.3", &mut ui).unwrap();
    assert_eq!(executor.cfg.model, "glm-5.3");
}

#[test]
fn session_operation_list_prefixes_resolve_switch_resume_and_render() {
    use ratatui::{Terminal, backend::TestBackend};
    let (mut executor, mut ui, _dir) = fixture(false);
    let original = id(&executor);
    let selected = executor.start_model_session("glm-5.3", &mut ui).unwrap();
    let (summaries, legacy) = {
        let manager = executor.session_manager.lock().unwrap();
        let (legacy, _lease, _outcome) = manager
            .store
            .create_with_lease(Some("旧形式 日本語".into()))
            .unwrap();
        (manager.store.list_with_stats().unwrap(), legacy.meta.id)
    };
    let listing = crate::session::format::format_summary_list(&summaries, Some(&selected));
    let prefixes: Vec<_> = listing
        .lines()
        .skip(1)
        .filter(|line| !line.starts_with("  Model:"))
        .map(|line| {
            line.split_whitespace()
                .next()
                .unwrap()
                .trim_start_matches('*')
                .to_string()
        })
        .collect();
    assert_eq!(prefixes.len(), 3);
    let resolved: Vec<_> = {
        let manager = executor.session_manager.lock().unwrap();
        prefixes
            .iter()
            .map(|prefix| manager.store.resolve_id_prefix(prefix).unwrap())
            .collect()
    };
    for target in [&original, &selected, &legacy] {
        assert!(resolved.contains(target));
    }
    let selected_prefix = &prefixes[resolved.iter().position(|id| id == &selected).unwrap()];
    let original_prefix = &prefixes[resolved.iter().position(|id| id == &original).unwrap()];
    executor.switch_to_session(original_prefix).unwrap();
    assert_eq!(executor.cfg.model, "gpt-6-luna");
    executor.resume_session(selected_prefix).unwrap();
    assert_eq!(executor.cfg.model, "glm-5.3");
    for width in [40, 80, 110] {
        ui.clear_log();
        ui.push_log(&listing);
        let mut terminal = Terminal::new(TestBackend::new(width, 32)).unwrap();
        terminal.draw(|frame| ui.view(frame, None)).unwrap();
        let screen = terminal.backend().to_string();
        for prefix in &prefixes {
            assert!(screen.contains(prefix), "{width}: {screen}");
        }
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .flat_map(|cell| cell.symbol().chars())
            .filter(|ch| !ch.is_whitespace())
            .collect();
        for marker in ["opencode-go", "glm-5.3", "gpt-6-luna", "legacy", "日本語"] {
            assert!(rendered.contains(marker), "{width}: {marker}: {screen}");
        }
    }
}
