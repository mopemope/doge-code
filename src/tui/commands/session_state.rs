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
    pub fn clear_runtime_conversation(&self) -> Result<()> {
        {
            let mut sm = crate::utils::safe_std_lock(&self.session_manager, "session_manager")?;
            sm.clear_current_session_conversation()?;
        }
        if let Ok(mut history) = self.conversation_history.lock() {
            history.clear();
        }
        Ok(())
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
        // Validate-then-commit under the session lock only; the runtime
        // conversation lock is taken afterwards, never together with it.
        let (session, messages) = {
            let mut sm = crate::utils::safe_std_lock(&self.session_manager, "session_manager")?;
            sm.switch_to_validated_session(id)?
        };
        self.replace_conversation_from_messages(messages);
        Ok(session)
    }

    /// Startup `--resume`: point both the `SessionManager` and the runtime
    /// conversation at the same saved session. `"latest"` resumes the most
    /// recently updated pre-existing session (skipping the eagerly-created
    /// empty session, which is then deleted); an explicit id (prefix allowed)
    /// resumes that session. Unknown ids and malformed targets are errors,
    /// never silent partial restores.
    pub fn resume_session(&self, resume_id: &str) -> Result<ResumeOutcome> {
        let prepared = {
            let mut sm = crate::utils::safe_std_lock(&self.session_manager, "session_manager")?;
            let fresh_id = sm.current_session_id();
            let loaded: Option<(SessionData, Vec<ChatMessage>)> = match resume_id {
                "latest" => sm.load_latest_validated_excluding(fresh_id.as_deref())?,
                id => Some(sm.switch_to_validated_session(id)?),
            };
            match loaded {
                Some((session, messages)) => {
                    let resumed_id = session.meta.id.clone();
                    if fresh_id.as_deref() != Some(resumed_id.as_str())
                        && let Some(fid) = fresh_id
                    {
                        let _ = sm.delete_session(&fid);
                    }
                    Some((resumed_id, messages))
                }
                None => None,
            }
        };
        match prepared {
            Some((resumed_id, messages)) => {
                self.replace_conversation_from_messages(messages);
                Ok(ResumeOutcome::Resumed {
                    session_id: resumed_id,
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
            executor.cfg.clone(),
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
}
