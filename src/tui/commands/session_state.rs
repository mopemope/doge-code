use crate::llm::types::ChatMessage;
use crate::session::SessionData;
use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;
use anyhow::Result;

/// Outcome of a startup `--resume` attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeOutcome {
    Resumed { session_id: String },
    NoPreviousSession,
}

impl TuiExecutor {
    pub(crate) fn ensure_session_idle(&self) -> Result<()> {
        anyhow::ensure!(
            self.jobs.foreground_id().is_none(),
            "a foreground job still owns this session; wait for /jobs to show completion before switching or clearing"
        );
        Ok(())
    }

    /// Replace the runtime conversation with canonical messages.
    pub fn replace_conversation_from_messages(&self, messages: Vec<ChatMessage>) {
        if let Ok(mut history) = self.conversation_history.lock() {
            history.replace(messages);
        }
    }

    /// Clear the persisted conversation context (messages, Observation Store,
    /// unseen tool results) of the current session and the in-memory runtime
    /// conversation together. The session id and metrics are preserved; the
    /// next turn cannot resurrect the old conversation.
    pub(crate) fn clear_runtime_conversation(
        &self,
    ) -> Result<crate::session::store::SessionSaveOutcome> {
        self.ensure_session_idle()?;
        // Acquire both fallible locks before committing the durable clear.
        let mut history =
            crate::utils::safe_std_lock(&self.conversation_history, "conversation_history")?;
        let outcome = {
            let mut sm = crate::utils::safe_std_lock(&self.session_manager, "session_manager")?;
            sm.clear_current_session_conversation()?
        };
        history.clear();
        Ok(outcome)
    }

    /// Delete without holding the manager lock while publishing UI state.
    pub(crate) fn delete_runtime_session(&mut self, ui: &mut TuiApp, id: &str) -> Result<String> {
        self.ensure_session_idle()?;
        let mut history =
            crate::utils::safe_std_lock(&self.conversation_history, "conversation_history")?;
        let (resolved, active) = {
            let mut sm = crate::utils::safe_std_lock(&self.session_manager, "session_manager")?;
            let resolved = sm.store.resolve_id_prefix(id)?;
            let active = sm.current_session_id().as_deref() == Some(resolved.as_str());
            sm.delete_session(&resolved)?;
            (resolved, active)
        };
        if active {
            history.clear();
            self.last_user_prompt = None;
            ui.last_user_input = None;
            ui.last_observed_directive_id = None;
            ui.last_observed_raw_input = None;
            ui.last_observed_effective_input = None;
            self.send_plan_items_to_ui(&[]);
        }
        Ok(resolved)
    }

    /// Create a fresh session and isolate it from the previous conversation:
    /// the runtime buffer is emptied and turn metadata that would replay the
    /// old instruction (`last_user_prompt`, retry directive tracking) is
    /// reset. UI theme and unrelated state are untouched.
    pub fn start_new_session(
        &mut self,
        ui: &mut TuiApp,
        initial_prompt: Option<String>,
    ) -> Result<String> {
        self.ensure_session_idle()?;
        let new_id = {
            let mut sm = crate::utils::safe_std_lock(&self.session_manager, "session_manager")?;
            sm.create_session(initial_prompt)?;
            sm.get_current_session_id()?
        };
        if let Ok(mut history) = self.conversation_history.lock() {
            history.clear();
        }
        self.last_user_prompt = None;
        ui.last_user_input = None;
        ui.last_observed_directive_id = None;
        ui.last_observed_raw_input = None;
        ui.last_observed_effective_input = None;
        // NOTE: `last_observed_seq` is intentionally NOT reset. It is a
        // monotonic pairing counter for async `::directive_observed` delivery
        // (see `TuiApp::note_directive_observed`); resetting it would let a
        // stale in-flight message from the previous session numerically
        // collide with the new session's first turn and attach the wrong
        // directive id. Clearing the raw/effective/id inputs above is
        // sufficient to drop stale state.
        Ok(new_id)
    }

    /// Switch to the target session, replacing the runtime conversation with
    /// the target's canonical messages. The target is decoded and validated
    /// before it becomes current, so a malformed target fails without leaving
    /// `current_session` and the runtime conversation diverged. Repeated
    /// switches always replace, never merge.
    pub fn switch_to_session(&self, id: &str) -> Result<SessionData> {
        self.ensure_session_idle()?;
        // Validate-then-commit under the session lock only; the runtime
        // conversation lock is taken afterwards, never together with it.
        let (session, messages) = {
            let mut sm = crate::utils::safe_std_lock(&self.session_manager, "session_manager")?;
            sm.switch_to_validated_session(id)?
        };
        self.replace_conversation_from_messages(messages);
        Ok(session)
    }

    /// Resolve startup resume without creating or deleting a placeholder.
    /// Unknown and malformed targets leave existing sessions untouched.
    pub fn resume_session(&self, resume_id: &str) -> Result<ResumeOutcome> {
        self.ensure_session_idle()?;
        let prepared = {
            let mut sm = crate::utils::safe_std_lock(&self.session_manager, "session_manager")?;
            let loaded = match resume_id {
                "latest" => sm.load_latest_validated_excluding(None)?,
                id => Some(sm.switch_to_validated_session(id)?),
            };
            if loaded.is_none() && sm.current_session.is_none() {
                sm.create_session(None)?;
            }
            loaded
        };
        match prepared {
            Some((session, messages)) => {
                self.replace_conversation_from_messages(messages);
                Ok(ResumeOutcome::Resumed {
                    session_id: session.meta.id,
                })
            }
            None => Ok(ResumeOutcome::NoPreviousSession),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionManager, SessionStore};

    fn user_msg(content: &str) -> ChatMessage {
        ChatMessage {
            provider_state: None,
            role: "user".into(),
            content: Some(content.to_string()),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    fn assistant_msg(content: &str) -> ChatMessage {
        ChatMessage {
            provider_state: None,
            role: "assistant".into(),
            content: Some(content.to_string()),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    fn test_executor() -> (TuiExecutor, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = crate::config::AppConfig {
            project_root: dir.path().to_path_buf(),
            api_key: Some("test-key".to_string()),
            base_url: "http://127.0.0.1:1".to_string(),
            no_repomap: true,
            ..Default::default()
        };
        let repomap = std::sync::Arc::new(tokio::sync::RwLock::new(None));
        let tools = crate::tools::FsTools::new(repomap.clone(), std::sync::Arc::new(cfg.clone()));
        let store = SessionStore::new(dir.path().join(".doge/sessions")).expect("session store");
        let manager = SessionManager::with_store(store);
        let executor = TuiExecutor::construct_with_session_manager(
            cfg,
            repomap,
            tools,
            std::sync::Arc::new(std::sync::Mutex::new(manager)),
        )
        .expect("executor");
        (executor, dir)
    }

    fn runtime_messages(executor: &TuiExecutor) -> Vec<ChatMessage> {
        executor
            .conversation_history
            .lock()
            .expect("history lock")
            .snapshot()
    }

    fn persist_runtime(executor: &TuiExecutor, messages: &[ChatMessage]) {
        let mut sm = executor.session_manager.lock().expect("session lock");
        sm.update_current_session_with_history(messages)
            .expect("persist");
    }

    fn current_session_id(executor: &TuiExecutor) -> String {
        executor
            .session_manager
            .lock()
            .expect("session lock")
            .current_session_id()
            .expect("current session")
    }

    #[test]
    fn startup_resume_preserves_retention_target() {
        for resume_latest in [false, true] {
            let (executor, _dir) = test_executor();
            let store_path = executor.cfg.project_root.join(".doge/sessions");
            let target = {
                let sm = executor.session_manager.lock().unwrap();
                let mut target = sm.current_session.clone().unwrap();
                target.meta.created_at = "2020-01-01T00:00:00Z".into();
                target
                    .replace_conversation_messages(&[user_msg("oldest but updated latest")])
                    .unwrap();
                target.timestamp = "2030-01-01T00:00:00Z".into();
                sm.store.save(&target).unwrap();
                for _ in 0..99 {
                    sm.store.create().unwrap();
                }
                target
            };
            let resume = if resume_latest {
                "latest".to_string()
            } else {
                target.meta.id.clone()
            };
            let mut cfg = executor.cfg.clone();
            cfg.resume = Some(resume.clone());
            let repomap = std::sync::Arc::new(tokio::sync::RwLock::new(None));
            let tools =
                crate::tools::FsTools::new(repomap.clone(), std::sync::Arc::new(cfg.clone()));
            let fresh = TuiExecutor::construct_with_session_manager(
                cfg,
                repomap,
                tools,
                std::sync::Arc::new(std::sync::Mutex::new(SessionManager::with_store(
                    SessionStore::new(store_path).unwrap(),
                ))),
            )
            .unwrap();
            assert_eq!(
                fresh.resume_session(&resume).unwrap(),
                ResumeOutcome::Resumed {
                    session_id: target.meta.id.clone()
                }
            );
            let sm = fresh.session_manager.lock().unwrap();
            assert_eq!(sm.store.list_with_stats().unwrap().len(), 100);
            assert!(sm.store.load(&target.meta.id).is_ok());
            assert_eq!(
                runtime_messages(&fresh)[0].content.as_deref(),
                Some("oldest but updated latest")
            );
        }
    }

    #[test]
    fn failed_clear_preserves_live_and_runtime_context() {
        let (executor, _dir) = test_executor();
        let messages = vec![user_msg("preserve on failure")];
        persist_runtime(&executor, &messages);
        executor.replace_conversation_from_messages(messages);
        let (path, before) = {
            let mut sm = executor.session_manager.lock().unwrap();
            let session = sm.current_session.as_mut().unwrap();
            session.unseen_tool_results.insert("pending".into());
            session
                .observations
                .insert("pending".into(), "test".into(), "recoverable".into(), 32)
                .unwrap();
            let session = sm.current_session.clone().unwrap();
            sm.store.save(&session).unwrap();
            (
                sm.store.root.join(&session.meta.id).join("session.json"),
                serde_json::to_value(session).unwrap(),
            )
        };
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).unwrap();
        assert!(executor.clear_runtime_conversation().is_err());
        let sm = executor.session_manager.lock().unwrap();
        assert_eq!(
            serde_json::to_value(sm.current_session.as_ref().unwrap()).unwrap(),
            before
        );
        assert_eq!(
            serde_json::to_value(sm.store.load(&current_id(&before)).unwrap()).unwrap(),
            before
        );
        assert_eq!(
            runtime_messages(&executor)[0].content.as_deref(),
            Some("preserve on failure")
        );
    }

    fn current_id(value: &serde_json::Value) -> String {
        value["meta"]["id"].as_str().unwrap().into()
    }

    #[test]
    fn clear_after_rename_adopts_disk_and_warns() {
        let (mut executor, _dir) = test_executor();
        persist_runtime(&executor, &[user_msg("clear committed")]);
        executor.replace_conversation_from_messages(vec![user_msg("clear committed")]);
        let id = current_session_id(&executor);
        {
            let mut sm = executor.session_manager.lock().unwrap();
            let session = sm.current_session.as_mut().unwrap();
            session.unseen_tool_results.insert("pending".into());
            session
                .observations
                .insert("pending".into(), "test".into(), "recoverable".into(), 32)
                .unwrap();
            sm.store.fail_directory_sync = true;
        }
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        executor.handle_session_command("clear", &mut ui).unwrap();
        assert!(runtime_messages(&executor).is_empty());
        let sm = executor.session_manager.lock().unwrap();
        let live = sm.current_session.as_ref().unwrap();
        assert!(live.conversation.is_empty());
        assert!(live.observations.is_empty());
        assert!(live.unseen_tool_results.is_empty());
        assert_eq!(
            serde_json::to_value(live).unwrap(),
            serde_json::to_value(sm.store.load(&id).unwrap()).unwrap()
        );
        assert!(ui.log.iter().any(|entry| matches!(entry, crate::tui::state::LogEntry::Plain(text) if text.contains("Warning:") && text.contains("directory sync failed"))));
    }

    #[test]
    fn inactive_and_failed_delete_preserve_current_context() {
        let (mut executor, _dir) = test_executor();
        persist_runtime(&executor, &[user_msg("active conversation")]);
        executor.replace_conversation_from_messages(vec![user_msg("active conversation")]);
        executor.last_user_prompt = Some("active retry".into());
        let active_id = current_session_id(&executor);
        let inactive = executor
            .session_manager
            .lock()
            .unwrap()
            .store
            .create()
            .unwrap();
        let mut ui = TuiApp::new("test", None, "dark").unwrap();
        ui.last_user_input = Some("active retry".into());
        executor
            .delete_runtime_session(&mut ui, &inactive.meta.id)
            .unwrap();
        assert_eq!(current_session_id(&executor), active_id);
        assert_eq!(
            runtime_messages(&executor)[0].content.as_deref(),
            Some("active conversation")
        );
        assert_eq!(executor.last_user_prompt.as_deref(), Some("active retry"));
        assert_eq!(ui.last_user_input.as_deref(), Some("active retry"));
        {
            let mut sm = executor.session_manager.lock().unwrap();
            sm.store = SessionStore::open_existing(sm.store.root.clone()).unwrap();
        }
        assert!(
            executor
                .delete_runtime_session(&mut ui, &active_id)
                .is_err()
        );
        assert!(
            executor
                .delete_runtime_session(&mut ui, "missing-id")
                .is_err()
        );
        assert_eq!(current_session_id(&executor), active_id);
        assert_eq!(
            runtime_messages(&executor)[0].content.as_deref(),
            Some("active conversation")
        );
        assert_eq!(executor.last_user_prompt.as_deref(), Some("active retry"));
        assert_eq!(ui.last_user_input.as_deref(), Some("active retry"));
        assert!(
            executor
                .session_manager
                .lock()
                .unwrap()
                .store
                .load(&active_id)
                .is_ok()
        );
    }

    #[test]
    fn empty_and_invalid_startup_resume_do_not_delete_sessions() {
        let (executor, _dir) = test_executor();
        let id = current_session_id(&executor);
        let path = executor.cfg.project_root.join(".doge/sessions");
        let construct = || {
            let mut cfg = executor.cfg.clone();
            cfg.resume = Some("latest".into());
            let repomap = std::sync::Arc::new(tokio::sync::RwLock::new(None));
            let tools =
                crate::tools::FsTools::new(repomap.clone(), std::sync::Arc::new(cfg.clone()));
            TuiExecutor::construct_with_session_manager(
                cfg,
                repomap,
                tools,
                std::sync::Arc::new(std::sync::Mutex::new(SessionManager::with_store(
                    SessionStore::new(&path).unwrap(),
                ))),
            )
            .unwrap()
        };
        let fresh = construct();
        assert!(fresh.resume_session("missing-id").is_err());
        assert!(
            fresh
                .session_manager
                .lock()
                .unwrap()
                .current_session
                .is_none()
        );
        {
            let sm = executor.session_manager.lock().unwrap();
            let mut corrupt = sm.current_session.clone().unwrap();
            corrupt.conversation.push(std::collections::HashMap::from([(
                "role".into(),
                serde_json::json!(123),
            )]));
            sm.store.save(&corrupt).unwrap();
        }
        assert!(fresh.resume_session(&id).is_err());
        assert!(fresh.resume_session("latest").is_err());
        assert!(
            fresh
                .session_manager
                .lock()
                .unwrap()
                .current_session
                .is_none()
        );
        assert_eq!(
            fresh
                .session_manager
                .lock()
                .unwrap()
                .store
                .list_with_stats()
                .unwrap()
                .len(),
            1
        );
        executor
            .session_manager
            .lock()
            .unwrap()
            .store
            .delete(&id)
            .unwrap();
        let empty = construct();
        assert_eq!(
            empty.resume_session("latest").unwrap(),
            ResumeOutcome::NoPreviousSession
        );
        assert!(runtime_messages(&empty).is_empty());
        assert_eq!(
            empty
                .session_manager
                .lock()
                .unwrap()
                .store
                .list_with_stats()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn active_delete_returns_and_isolates_next_session() {
        let (mut executor, dir) = test_executor();
        persist_runtime(&executor, &[user_msg("deleted conversation")]);
        executor.replace_conversation_from_messages(vec![user_msg("deleted conversation")]);
        executor.last_user_prompt = Some("old retry".into());
        let id = current_session_id(&executor);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _dir = dir;
            let mut ui = TuiApp::new("test", None, "dark").unwrap();
            let late_history = crate::llm::tool_execution::history::HistoryManager::new(
                executor.client.as_ref().unwrap().clone(),
                vec![user_msg("stale late checkpoint")],
                None,
                executor.tools.clone(),
                executor.cfg.clone(),
            );
            ui.last_user_input = Some("old retry".into());
            executor
                .handle_session_command(&format!("delete {id}"), &mut ui)
                .unwrap();
            assert!(runtime_messages(&executor).is_empty());
            assert!(executor.last_user_prompt.is_none());
            assert!(ui.last_user_input.is_none());
            assert!(
                executor
                    .session_manager
                    .lock()
                    .unwrap()
                    .current_session
                    .is_none()
            );
            assert!(
                !executor
                    .cfg
                    .project_root
                    .join(".doge/sessions")
                    .join(&id)
                    .exists()
            );
            assert!(late_history.checkpoint().is_err());
            executor.start_new_session(&mut ui, None).unwrap();
            persist_runtime(&executor, &[user_msg("new conversation")]);
            assert!(late_history.checkpoint().is_err());
            drop(late_history);
            let sm = executor.session_manager.lock().unwrap();
            assert_ne!(sm.current_session_id().unwrap(), id);
            assert_eq!(
                sm.current_session
                    .as_ref()
                    .unwrap()
                    .conversation_messages()
                    .unwrap()
                    .len(),
                1
            );
            tx.send(()).unwrap();
        });
        rx.recv_timeout(std::time::Duration::from_secs(3))
            .expect("delete must not deadlock");
    }

    #[test]
    fn startup_resume_restores_conversation() {
        let (executor, _dir) = test_executor();
        let saved = vec![
            user_msg("first"),
            assistant_msg("answer"),
            user_msg("second"),
        ];
        persist_runtime(&executor, &saved);
        let resumed_id = current_session_id(&executor);

        // Fresh executor over the same store, like process startup.
        let store_path = executor.cfg.project_root.join(".doge/sessions");
        let repomap = std::sync::Arc::new(tokio::sync::RwLock::new(None));
        let tools =
            crate::tools::FsTools::new(repomap.clone(), std::sync::Arc::new(executor.cfg.clone()));
        let store = SessionStore::new(store_path).expect("reopen store");
        let fresh = TuiExecutor::construct_with_session_manager(
            {
                let mut cfg = executor.cfg.clone();
                cfg.resume = Some("latest".into());
                cfg
            },
            repomap,
            tools,
            std::sync::Arc::new(std::sync::Mutex::new(SessionManager::with_store(store))),
        )
        .expect("fresh executor");
        assert!(runtime_messages(&fresh).is_empty());

        let outcome = fresh.resume_session("latest").expect("resume");
        assert_eq!(
            outcome,
            ResumeOutcome::Resumed {
                session_id: resumed_id
            }
        );
        let restored = runtime_messages(&fresh);
        assert_eq!(restored.len(), 3);
        assert_eq!(restored[0].content.as_deref(), Some("first"));
        assert_eq!(restored[1].content.as_deref(), Some("answer"));
        assert_eq!(restored[2].content.as_deref(), Some("second"));
    }

    #[test]
    fn session_switch_replaces_conversation() {
        let (executor, _dir) = test_executor();
        persist_runtime(&executor, &[user_msg("in A")]);
        let id_a = current_session_id(&executor);
        {
            let mut sm = executor.session_manager.lock().expect("session lock");
            sm.create_session(None).expect("create B");
        }
        let id_b = current_session_id(&executor);
        assert_ne!(id_a, id_b);
        persist_runtime(&executor, &[user_msg("in B")]);

        // Switch back to A: history must be exactly A, not A+B.
        executor.switch_to_session(&id_a).expect("switch to A");
        let restored = runtime_messages(&executor);
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].content.as_deref(), Some("in A"));

        // Switch to B repeatedly: still exactly B, never merged.
        executor.switch_to_session(&id_b).expect("switch to B");
        executor
            .switch_to_session(&id_b)
            .expect("switch to B again");
        let restored = runtime_messages(&executor);
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].content.as_deref(), Some("in B"));
    }

    #[test]
    fn session_new_starts_with_empty_conversation() {
        let (mut executor, _dir) = test_executor();
        persist_runtime(&executor, &[user_msg("old history")]);
        executor.replace_conversation_from_messages(vec![user_msg("old history")]);
        executor.last_user_prompt = Some("old history".to_string());
        let mut ui = TuiApp::new("test", None, "dark").expect("ui");
        ui.last_user_input = Some("old history".to_string());
        ui.last_observed_raw_input = Some("old history".to_string());
        ui.last_observed_effective_input = Some("old history".to_string());
        ui.last_observed_seq = 3;
        ui.last_observed_directive_id = Some("dir-old".to_string());

        executor
            .start_new_session(&mut ui, None)
            .expect("new session");

        assert!(runtime_messages(&executor).is_empty());
        assert!(executor.last_user_prompt.is_none());
        assert!(ui.last_user_input.is_none());
        assert!(ui.last_observed_raw_input.is_none());
        assert!(ui.last_observed_effective_input.is_none());
        assert!(ui.last_observed_directive_id.is_none());
        // The pairing sequence stays monotonic across sessions so a stale
        // in-flight `::directive_observed` from the old session can never
        // numerically collide with the new session's turns.
        assert_eq!(ui.last_observed_seq, 3);
    }

    #[test]
    fn session_clear_clears_runtime_and_durable_context() {
        let (executor, _dir) = test_executor();
        let saved = vec![user_msg("keep me not"), assistant_msg("answer")];
        persist_runtime(&executor, &saved);
        executor.replace_conversation_from_messages(saved);
        {
            let mut sm = executor.session_manager.lock().expect("session lock");
            if let Some(session) = sm.current_session.as_mut() {
                session.unseen_tool_results.insert("call-1".into());
            }
            let snapshot = sm.current_session.clone().expect("session");
            sm.store.save(&snapshot).expect("save with unseen");
        }
        let id_before = current_session_id(&executor);

        executor.clear_runtime_conversation().expect("clear");

        assert!(runtime_messages(&executor).is_empty());
        let sm = executor.session_manager.lock().expect("session lock");
        let session = sm.current_session.as_ref().expect("session");
        assert_eq!(session.meta.id, id_before);
        assert!(session.conversation.is_empty());
        assert!(session.observations.is_empty());
        assert!(session.unseen_tool_results.is_empty());
    }

    #[test]
    fn clear_does_not_reappear_on_next_save() {
        let (executor, _dir) = test_executor();
        persist_runtime(&executor, &[user_msg("old message")]);
        executor.replace_conversation_from_messages(vec![user_msg("old message")]);
        executor.clear_runtime_conversation().expect("clear");

        // Next turn saves only the new conversation.
        let next = vec![user_msg("brand new")];
        executor.replace_conversation_from_messages(next.clone());
        persist_runtime(&executor, &next);

        let sm = executor.session_manager.lock().expect("session lock");
        let session = sm.current_session.as_ref().expect("session");
        let messages = session.conversation_messages().expect("decode");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content.as_deref(), Some("brand new"));
    }

    #[test]
    fn malformed_target_fails_without_split_brain() {
        let (executor, _dir) = test_executor();
        persist_runtime(&executor, &[user_msg("healthy")]);
        executor.replace_conversation_from_messages(vec![user_msg("healthy")]);
        let healthy_id = current_session_id(&executor);
        {
            let mut sm = executor.session_manager.lock().expect("session lock");
            sm.create_session(None).expect("create target");
        }
        let target_id = current_session_id(&executor);
        {
            // Corrupt one entry of the target session on disk shape.
            let mut sm = executor.session_manager.lock().expect("session lock");
            if let Some(session) = sm.current_session.as_mut() {
                let mut bad = std::collections::HashMap::new();
                bad.insert("role".to_string(), serde_json::json!(123));
                session.add_conversation_entry(bad);
            }
            let snapshot = sm.current_session.clone().expect("session");
            sm.store.save(&snapshot).expect("save corrupt target");
        }
        // Return to the healthy session first so the corrupt one is a switch
        // target rather than the current session.
        executor
            .switch_to_session(&healthy_id)
            .expect("back to healthy");

        let err = executor
            .switch_to_session(&target_id)
            .expect_err("must fail");
        assert!(err.to_string().contains("index 0"), "{err}");
        // No split brain: current session and runtime still agree on healthy.
        assert_eq!(current_session_id(&executor), healthy_id);
        let restored = runtime_messages(&executor);
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].content.as_deref(), Some("healthy"));
    }

    #[tokio::test]
    async fn session_mutations_wait_for_foreground_release_without_clearing_ui() {
        use crate::jobs::{JobKind, JobRunOutcome, JobScope, JobSpec, WorkspaceAccess};
        let (mut executor, _dir) = test_executor();
        persist_runtime(&executor, &[user_msg("keep history")]);
        executor.replace_conversation_from_messages(vec![user_msg("keep history")]);
        let id = current_session_id(&executor);
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        let blocker = executor
            .jobs
            .spawn(
                JobSpec::new(
                    JobKind::AgentTurn,
                    JobScope::Foreground,
                    WorkspaceAccess::Write,
                    "finishing fixture",
                ),
                |_| async move {
                    let _ = wait.await;
                    JobRunOutcome::Completed
                },
            )
            .expect("blocker");
        let mut ui = TuiApp::new("test", None, "dark").expect("ui");
        ui.push_log("keep log");
        ui.tokens_prompt_used = 321;
        assert!(executor.start_new_session(&mut ui, None).is_err());
        assert!(executor.switch_to_session(&id).is_err());
        assert!(executor.resume_session(&id).is_err());
        assert!(executor.clear_runtime_conversation().is_err());
        for command in ["new", "clear", "delete", "switch"] {
            assert!(executor.handle_session_command(command, &mut ui).is_err());
        }
        crate::tui::commands::handlers::slash_commands::clear::handle_clear(&mut executor, &mut ui);
        assert_eq!(ui.tokens_prompt_used, 321);
        assert!(ui.log.iter().any(
            |entry| matches!(entry, crate::tui::state::LogEntry::Plain(text) if text == "keep log")
        ));
        assert_eq!(current_session_id(&executor), id);
        assert_eq!(
            runtime_messages(&executor)[0].content.as_deref(),
            Some("keep history")
        );
        release.send(()).expect("release");
        for _ in 0..100 {
            if executor.jobs.foreground_id().is_none() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            executor.jobs.foreground_id().is_none(),
            "job {blocker} released"
        );
        executor
            .start_new_session(&mut ui, None)
            .expect("new after release");
        assert_ne!(current_session_id(&executor), id);
    }
}
