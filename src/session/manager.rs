use crate::session::{SessionData, SessionStore};
use anyhow::Result;
use tracing::debug;

#[derive(Debug)]
pub struct SessionManager {
    pub store: SessionStore,
    pub current_session: Option<SessionData>,
    pub(crate) current_lease: Option<super::lease::SessionLease>,
    pub(crate) save_state: SessionSaveState,
}

/// Process-local state only; never added to the session wire format.
#[derive(Debug, Default)]
pub(crate) enum SessionSaveState {
    #[default]
    Durable,
    Unsaved {
        session_id: String,
    },
    DurabilityUnconfirmed {
        session_id: String,
        message: String,
    },
}

/// Canonical conversation checkpoint payload.
///
/// Groups history, observations, unseen results, usage, and deferred
/// activation names so a single durable write keeps them atomic: the
/// tool result, provider activation marker, and activation sidecar are
/// never persisted separately.
pub struct ConversationCheckpointState {
    pub history: Vec<crate::llm::types::ChatMessage>,
    pub observations: Option<crate::llm::observation::ObservationStore>,
    pub unseen: Option<std::collections::BTreeSet<String>>,
    pub usage_delta: Option<crate::llm::usage_ledger::UsageLedger>,
    pub activated_tools: Option<std::collections::BTreeSet<String>>,
}

impl SessionManager {
    /// Create a new SessionManager with the default store location
    pub fn new() -> Result<Self> {
        let store = SessionStore::new_default()?;
        Ok(Self {
            store,
            current_session: None,
            save_state: SessionSaveState::Durable,
            current_lease: None,
        })
    }

    /// Create a new SessionManager with a specific store
    pub fn with_store(store: SessionStore) -> Self {
        Self {
            store,
            current_session: None,
            save_state: SessionSaveState::Durable,
            current_lease: None,
        }
    }

    pub fn has_unsaved_current_session(&self) -> bool {
        matches!(&self.save_state, SessionSaveState::Unsaved { session_id }
            if self.current_session_id().as_deref() == Some(session_id.as_str()))
    }

    pub(crate) fn checkpoint_warning(&self) -> Option<&str> {
        match &self.save_state {
            SessionSaveState::DurabilityUnconfirmed {
                session_id,
                message,
            } if self.current_session_id().as_deref() == Some(session_id.as_str()) => Some(message),
            _ => None,
        }
    }

    /// Re-save the complete payload already in memory, without applying usage again.
    pub(crate) fn flush_current_session(
        &mut self,
    ) -> Result<crate::session::store::SessionSaveOutcome> {
        self.flush_current_session_policy(false)
    }

    fn flush_current_session_policy(
        &mut self,
        preserve_sessions: bool,
    ) -> Result<crate::session::store::SessionSaveOutcome> {
        use crate::session::store::SessionSaveOutcome;
        let Some(session) = self.current_session.as_ref() else {
            self.save_state = SessionSaveState::Durable;
            return Ok(SessionSaveOutcome::Durable);
        };
        let id = session.meta.id.clone();
        self.save_state = SessionSaveState::Unsaved {
            session_id: id.clone(),
        };
        anyhow::ensure!(
            self.store
                .session_dir(&id)
                .join("session.json")
                .try_exists()?,
            "session checkpoint was deleted; refusing to recreate it during flush"
        );
        let lease = self
            .current_lease
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("current session has no ownership lease"))?;
        let result = if preserve_sessions {
            self.store.save_preserving_sessions(session, lease)
        } else {
            self.store.save_with_lease(session, lease)
        };
        let outcome = match result {
            Err(error @ super::error::SessionError::CapacityExceeded { .. }) => return Err(anyhow::Error::new(error).context("Normal checkpoint remains unsaved; use /session export to recover the in-memory session")),
            result => result?,
        };
        self.adopt_save_outcome(&id, &outcome);
        Ok(outcome)
    }

    fn adopt_save_outcome(
        &mut self,
        id: &str,
        outcome: &crate::session::store::SessionSaveOutcome,
    ) {
        self.save_state = match outcome {
            crate::session::store::SessionSaveOutcome::Durable => SessionSaveState::Durable,
            crate::session::store::SessionSaveOutcome::DurabilityUnconfirmed { message } => {
                tracing::warn!(%message, "session checkpoint replaced; durability unconfirmed");
                SessionSaveState::DurabilityUnconfirmed {
                    session_id: id.into(),
                    message: message.clone(),
                }
            }
        };
    }

    pub(crate) fn export_current_session(&self) -> Result<super::recovery::RecoveryExport> {
        let session = self
            .current_session
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No session loaded; nothing to export"))?;
        let lease = self
            .current_lease
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("current session has no ownership lease"))?;
        Ok(self.store.export_recovery(session, lease)?)
    }

    /// Called only for irreversible error exits, after all checkpoint owners stop.
    /// A successful recovery never turns the original save/run failure into success.
    pub(crate) fn recover_capacity_exit(&self, error: anyhow::Error) -> anyhow::Error {
        if !error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<super::error::SessionError>(),
                Some(super::error::SessionError::CapacityExceeded { .. })
            )
        }) {
            return error;
        }
        match self.export_current_session() {
            Ok(export) => {
                eprintln!(
                    "Normal checkpoint remains unsaved; recovery file: {} ({} bytes). This is a recovery artifact, not a resumable session.",
                    export.path.display(),
                    export.bytes
                );
                if let Some(warning) = export.durability_warning {
                    eprintln!("Warning: {warning}");
                }
                error
            }
            Err(recovery_error) => {
                error.context(format!("recovery export also failed: {recovery_error:#}"))
            }
        }
    }

    fn save_current_session(&mut self) -> Result<()> {
        self.flush_current_session().map(|_| ())
    }

    pub(crate) fn flush_before_transition(
        &mut self,
    ) -> Result<crate::session::store::SessionSaveOutcome> {
        if self.has_unsaved_current_session() {
            self.flush_current_session()
        } else {
            Ok(crate::session::store::SessionSaveOutcome::Durable)
        }
    }

    pub(crate) fn flush_before_model_transition(
        &mut self,
    ) -> Result<crate::session::store::SessionSaveOutcome> {
        if self.has_unsaved_current_session() {
            self.flush_current_session_policy(true)
        } else {
            Ok(crate::session::store::SessionSaveOutcome::Durable)
        }
    }

    /// Create a new session with an optional initial prompt.
    /// If `initial_prompt` is provided the session title will be set and persisted.
    pub fn create_session(&mut self, initial_prompt: Option<String>) -> Result<()> {
        self.create_session_with_outcome(initial_prompt).map(|_| ())
    }

    pub(crate) fn create_session_with_outcome(
        &mut self,
        initial_prompt: Option<String>,
    ) -> Result<(String, crate::session::store::SessionSaveOutcome)> {
        self.flush_before_transition()?;
        let (session, lease, outcome) = self.store.create_with_lease(initial_prompt)?;
        let id = session.meta.id.clone();
        self.current_session = Some(session);
        self.current_lease = Some(lease);
        self.adopt_save_outcome(&id, &outcome);
        Ok((id, outcome))
    }

    /// Save the complete new selection before publishing it as current.
    /// This explicit user transition never prunes other sessions.
    pub(crate) fn create_session_with_model(
        &mut self,
        initial_prompt: Option<String>,
        selection: super::data::SessionModelSelection,
    ) -> Result<(String, crate::session::store::SessionSaveOutcome)> {
        if self.has_unsaved_current_session() {
            self.flush_current_session_policy(true)?;
        }
        let mut candidate = SessionData::new();
        if let Some(prompt) = initial_prompt {
            candidate.set_initial_prompt(&prompt);
            candidate.meta.title_is_default = false;
        }
        candidate.model_selection = Some(selection);
        let lease = self.store.try_lease(&candidate.meta.id)?;
        let outcome = self.store.save_preserving_sessions(&candidate, &lease)?;
        let id = candidate.meta.id.clone();
        self.current_session = Some(candidate);
        self.current_lease = Some(lease);
        self.adopt_save_outcome(&id, &outcome);
        Ok((id, outcome))
    }

    /// Load a session by ID
    pub fn load_session(&mut self, id: &str) -> Result<()> {
        self.flush_before_transition()?;
        let lease = self.acquire_transition_lease(id)?;
        let session = self.store.load(id)?;
        self.current_session = Some(session);
        if let Some(lease) = lease {
            self.current_lease = Some(lease);
        }
        self.save_state = SessionSaveState::Durable;
        Ok(())
    }

    fn acquire_transition_lease(&self, id: &str) -> Result<Option<super::lease::SessionLease>> {
        if self.current_session_id().as_deref() == Some(id) {
            let lease = self
                .current_lease
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("current session has no ownership lease"))?;
            lease.validate(&self.store.root, id)?;
            Ok(None)
        } else {
            Ok(Some(self.store.try_lease(id)?))
        }
    }

    /// Load a session by (possibly partial) ID without making it current.
    /// Used to decode/validate a resume/switch target before committing, so
    /// a malformed target can never leave `current_session` and the runtime
    /// conversation pointing at different sessions.
    pub fn peek_session(&self, id: &str) -> Result<SessionData> {
        let full_id = self.store.resolve_id_prefix(id)?;
        Ok(self.store.load(&full_id)?)
    }

    /// Validate a switch target's conversation, then commit it as current.
    /// On decode failure the active session is left untouched (fail closed,
    /// never a partial conversation). Returns the committed session plus its
    /// decoded canonical messages.
    pub fn switch_to_validated_session(
        &mut self,
        id: &str,
    ) -> Result<(SessionData, Vec<crate::llm::types::ChatMessage>)> {
        let (session, messages, ()) = self.switch_to_prepared_session(id, |_| Ok(()))?;
        Ok((session, messages))
    }

    pub(crate) fn switch_to_prepared_session<T>(
        &mut self,
        id: &str,
        prepare: impl FnOnce(&SessionData) -> Result<T>,
    ) -> Result<(SessionData, Vec<crate::llm::types::ChatMessage>, T)> {
        self.flush_before_model_transition()?;
        let full_id = self.store.resolve_id_prefix(id)?;
        let lease = self.acquire_transition_lease(&full_id)?;
        let session = self.store.load(&full_id)?;
        let messages = session.conversation_messages()?;
        let prepared = prepare(&session)?;
        self.current_session = Some(session.clone());
        if let Some(lease) = lease {
            self.current_lease = Some(lease);
        }
        self.save_state = SessionSaveState::Durable;
        Ok((session, messages, prepared))
    }

    /// Validate the latest session (optionally skipping one ID, e.g. an
    /// eagerly-created empty session) and commit it as current. Returns
    /// `None` when no session exists. Decode failure leaves the active
    /// session untouched.
    pub fn load_latest_validated_excluding(
        &mut self,
        exclude_id: Option<&str>,
    ) -> Result<Option<(SessionData, Vec<crate::llm::types::ChatMessage>)>> {
        self.flush_before_transition()?;
        let summaries = self.store.list_with_stats()?;
        let target = summaries
            .iter()
            .map(|s| &s.meta.id)
            .find(|id| Some(id.as_str()) != exclude_id)
            .cloned();
        match target {
            Some(id) => {
                let result = self.switch_to_validated_session(&id)?;
                let (session, messages) = result;
                Ok(Some((session, messages)))
            }
            None => Ok(None),
        }
    }

    /// Get the current session ID, if any.
    pub fn current_session_id(&self) -> Option<String> {
        self.current_session.as_ref().map(|s| s.meta.id.clone())
    }

    /// Load the latest session
    pub fn load_latest_session(&mut self) -> Result<()> {
        self.flush_before_transition()?;
        self.load_latest_session_excluding(None)?;
        Ok(())
    }

    /// Load the most recently updated session, optionally skipping one session
    /// ID (used to skip an eagerly-created empty session when resuming).
    ///
    /// Returns whether a session was loaded.
    pub fn load_latest_session_excluding(&mut self, exclude_id: Option<&str>) -> Result<bool> {
        self.flush_before_transition()?;
        let summaries = self.store.list_with_stats()?;
        let target = summaries
            .iter()
            .map(|s| &s.meta.id)
            .find(|id| Some(id.as_str()) != exclude_id)
            .cloned();
        match target {
            Some(id) => {
                self.load_session(&id)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Delete a session by ID
    pub fn delete_session(&mut self, id: &str) -> Result<()> {
        if self.current_session_id().as_deref() == Some(id) {
            self.store.delete_with_lease(
                id,
                self.current_lease
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("current session has no ownership lease"))?,
            )?;
        } else {
            self.store.delete(id)?;
        }
        // If the current session is the one being deleted, clear it
        if let Some(current) = &self.current_session
            && current.meta.id == id
        {
            self.current_session = None;
            self.current_lease = None;
            self.save_state = SessionSaveState::Durable;
        }
        Ok(())
    }

    /// Clear the current session's conversation plus the conversation-owned
    /// Observation Store and unseen tool results. Session identity, metrics,
    /// and provenance flags are preserved.
    pub(crate) fn clear_current_session_conversation(
        &mut self,
    ) -> Result<crate::session::store::SessionSaveOutcome> {
        let Some(current) = self.current_session.as_ref() else {
            return Ok(crate::session::store::SessionSaveOutcome::Durable);
        };
        let mut candidate = current.clone();
        candidate.clear_conversation_context();
        let outcome = self.store.save_with_lease(
            &candidate,
            self.current_lease
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("current session has no ownership lease"))?,
        )?;
        let id = candidate.meta.id.clone();
        self.current_session = Some(candidate);
        self.adopt_save_outcome(&id, &outcome);
        Ok(outcome)
    }

    pub fn bind_inference(&mut self, binding: String) -> Result<()> {
        if let Some(session) = &mut self.current_session {
            if let Some(existing) = &session.inference_binding {
                anyhow::ensure!(
                    existing == &binding,
                    "Session belongs to another inference account/provider/model; start a new session or restore its original selection"
                );
            } else {
                session.inference_binding = Some(binding);
                self.save_current_session()?;
            }
        }
        Ok(())
    }

    /// Update the current session with conversation history
    pub fn update_current_session_with_history(
        &mut self,
        history: &[crate::llm::types::ChatMessage],
    ) -> Result<()> {
        self.update_current_session_with_history_and_observations(history, None, None)
    }

    /// Update history plus the conversation-owned Observation Store snapshot.
    /// `None` leaves the stored observations/unseen state untouched (used by
    /// callers that only persist messages).
    pub fn update_current_session_with_history_and_observations(
        &mut self,
        history: &[crate::llm::types::ChatMessage],
        observations: Option<crate::llm::observation::ObservationStore>,
        unseen: Option<std::collections::BTreeSet<String>>,
    ) -> Result<()> {
        self.update_current_session_with_history_observations_and_usage(
            history,
            observations,
            unseen,
            None,
            || {},
        )
    }

    pub fn update_current_session_with_history_observations_and_usage(
        &mut self,
        history: &[crate::llm::types::ChatMessage],
        observations: Option<crate::llm::observation::ObservationStore>,
        unseen: Option<std::collections::BTreeSet<String>>,
        usage_delta: Option<crate::llm::usage_ledger::UsageLedger>,
        usage_applied: impl FnOnce(),
    ) -> Result<()> {
        // Legacy entry point preserves the activation sidecar: pass `None`
        // so existing callers that only persist messages never clear it.
        let state = ConversationCheckpointState {
            history: history.to_vec(),
            observations,
            unseen,
            usage_delta,
            activated_tools: None,
        };
        self.update_current_session_with_checkpoint_state(state, usage_applied)
    }

    /// Atomic checkpoint including the `activated_tools` sidecar.
    ///
    /// `activated_tools == None` leaves the stored sidecar untouched.
    /// `Some(set)` replaces it (empty clears, e.g. after `/clear` via a
    /// fresh `HistoryManager`).
    pub fn update_current_session_with_checkpoint_state(
        &mut self,
        state: ConversationCheckpointState,
        usage_applied: impl FnOnce(),
    ) -> Result<()> {
        let ConversationCheckpointState {
            history,
            observations,
            unseen,
            usage_delta,
            activated_tools,
        } = state;
        debug!(
            history_len = history.len(),
            has_current_session = self.current_session.is_some(),
            "Updating session with history"
        );
        if let Some(ref mut session) = self.current_session {
            // Durable projection first: request-scoped system messages never
            // persist; provider state, tool calls, and tool results survive.
            let durable = crate::llm::durable_conversation_messages(history.iter().cloned());

            // Find the first user prompt in the durable history to set
            // session title if not set
            let first_user_prompt = durable
                .iter()
                .find(|m| {
                    m.role == "user" && m.content.as_ref().map(|s| !s.is_empty()).unwrap_or(false)
                })
                .and_then(|m| m.content.clone());

            // Atomically replace the stored conversation: every message is
            // encoded before the stored payload is swapped, so a failure
            // never leaves a partially rewritten conversation.
            session.replace_conversation_messages(&durable)?;

            // If the session title is default (auto-generated) and we have a first user prompt, override it
            if session.meta.title_is_default
                && let Some(prompt) = first_user_prompt
            {
                session.set_initial_prompt(&prompt);
                // Mark that the title is now user-provided
                session.meta.title_is_default = false;
                debug!(
                    "Overrode default session title with first user prompt (truncated to 30 chars)"
                );
            }
            if let Some(obs) = observations {
                session.observations = obs;
            }
            if let Some(unseen_set) = unseen {
                session.unseen_tool_results = unseen_set;
            }
            if let Some(activated) = activated_tools {
                session.activated_tools = activated;
            }

            if let Some(delta) = usage_delta {
                let usage = session
                    .usage
                    .get_or_insert(crate::llm::usage_ledger::UsageLedger {
                        historical_usage_unknown: true,
                        ..Default::default()
                    });
                usage.add(&delta);
                session.token_count = session.token_count.saturating_add(delta.total_tokens);
                session.requests = session.requests.saturating_add(delta.attempts);
            }
            // Called only after history encoding and usage application succeed.
            // Disk failure leaves this complete payload in memory for retry.
            usage_applied();
            self.save_current_session()?;
        }
        Ok(())
    }

    pub(crate) fn apply_usage_delta(
        &mut self,
        expected_session_id: &str,
        delta: &crate::llm::usage_ledger::UsageLedger,
    ) -> Result<crate::session::store::SessionSaveOutcome> {
        use crate::session::store::SessionSaveOutcome;
        // Empty provider activity must not touch timestamps or disk.
        if !delta.has_activity() {
            return Ok(SessionSaveOutcome::Durable);
        }
        let Some(session) = self.current_session.as_mut() else {
            anyhow::bail!("no current session for usage attribution");
        };
        // Never attribute to a session other than the one active when the
        // operation started. Fail closed without mutating either session.
        anyhow::ensure!(
            session.meta.id == expected_session_id,
            "active session changed during LLM operation; usage attribution refused"
        );
        let usage = session
            .usage
            .get_or_insert(crate::llm::usage_ledger::UsageLedger {
                historical_usage_unknown: true,
                ..Default::default()
            });
        usage.add(delta);
        session.token_count = session.token_count.saturating_add(delta.total_tokens);
        session.requests = session.requests.saturating_add(delta.attempts);
        session.timestamp = chrono::Utc::now().to_rfc3339();
        // In-memory payload is complete here. A save failure leaves it in
        // memory with Unsaved state; a later flush re-saves the same payload
        // without reapplying the delta (exact-once).
        self.flush_current_session()
    }

    /// Manual compaction is disk-first. Unlike agent checkpoints, failed saves
    /// must leave the original in-memory payload available without a summary.
    /// The caller holds the conversation lock and rechecks its runtime snapshot.
    pub(crate) fn commit_compacted_history(
        &mut self,
        expected: &SessionData,
        history: &[crate::llm::ChatMessage],
        observations: crate::llm::observation::ObservationStore,
        unseen: std::collections::BTreeSet<String>,
    ) -> Result<crate::session::store::SessionSaveOutcome> {
        let current = self
            .current_session
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no current session for compaction"))?;
        anyhow::ensure!(
            current.meta.id == expected.meta.id
                && serde_json::to_value(current)? == serde_json::to_value(expected)?,
            "session changed during compaction; summary discarded"
        );
        let mut candidate = current.clone();
        let durable = crate::llm::durable_conversation_messages(history.iter().cloned());
        candidate.replace_conversation_messages(&durable)?;
        candidate.observations = observations;
        candidate.unseen_tool_results = unseen;
        candidate.timestamp = chrono::Utc::now().to_rfc3339();
        let outcome = self.store.save_with_lease(
            &candidate,
            self.current_lease
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("current session has no ownership lease"))?,
        )?;
        let id = candidate.meta.id.clone();
        self.current_session = Some(candidate);
        self.adopt_save_outcome(&id, &outcome);
        Ok(outcome)
    }

    /// Persist only the Observation Store snapshot (history untouched).
    pub fn update_current_session_with_observations(
        &mut self,
        observations: crate::llm::observation::ObservationStore,
        unseen: std::collections::BTreeSet<String>,
    ) -> Result<()> {
        if let Some(ref mut session) = self.current_session {
            session.observations = observations;
            session.unseen_tool_results = unseen;
            self.save_current_session()?;
        }
        Ok(())
    }

    /// Load the persisted Observation Store snapshot, if any.
    pub fn load_current_observations(
        &self,
    ) -> Option<(
        crate::llm::observation::ObservationStore,
        std::collections::BTreeSet<String>,
    )> {
        self.current_session
            .as_ref()
            .map(|s| (s.observations.clone(), s.unseen_tool_results.clone()))
    }

    /// Update the current session with token count
    pub fn update_current_session_with_token_count(&mut self, token_count: u64) -> Result<()> {
        if let Some(ref mut session) = self.current_session {
            session.increment_token_count(token_count);
            self.save_current_session()?;
        }
        Ok(())
    }

    /// Update the current session with request count
    pub fn update_current_session_with_request_count(&mut self) -> Result<()> {
        if let Some(ref mut session) = self.current_session {
            session.increment_requests();
            self.save_current_session()?;
        }
        Ok(())
    }

    /// Update the current session with tool call count
    pub fn update_current_session_with_lines_edited(&mut self, lines_edited: u64) -> Result<()> {
        if let Some(session) = &mut self.current_session {
            session.increment_lines_edited(lines_edited);
            self.save_current_session()?;
        }
        Ok(())
    }

    pub fn update_current_session_with_tool_call_count(&mut self) -> Result<()> {
        if let Some(ref mut session) = self.current_session {
            session.increment_tool_calls();
            self.save_current_session()?;
        }
        Ok(())
    }

    /// Record a successful tool call in the current session
    pub fn record_tool_call_success(&mut self, tool_name: &str) -> Result<()> {
        if let Some(ref mut session) = self.current_session {
            session.record_tool_call_success(tool_name);
            self.save_current_session()?;
        }
        Ok(())
    }

    /// Record a failed tool call in the current session
    pub fn record_tool_call_failure(&mut self, tool_name: &str) -> Result<()> {
        if let Some(ref mut session) = self.current_session {
            session.record_tool_call_failure(tool_name);
            self.save_current_session()?;
        }
        Ok(())
    }

    /// Set the initial prompt for the current session
    pub fn set_initial_prompt_for_current_session(&mut self, prompt: &str) -> Result<()> {
        if let Some(ref mut session) = self.current_session {
            session.set_initial_prompt(prompt);
            self.save_current_session()?;
        }
        Ok(())
    }

    /// Update the current session with a changed file path
    pub fn update_current_session_with_changed_file(
        &mut self,
        path: std::path::PathBuf,
    ) -> Result<()> {
        if let Some(ref mut session) = self.current_session {
            session.add_changed_file(path);
            self.save_current_session()?;
        }
        Ok(())
    }

    /// Check if the current session has any changed files
    pub fn current_session_has_changed_files(&self) -> bool {
        if let Some(ref session) = self.current_session {
            session.has_changed_files()
        } else {
            false
        }
    }

    /// Get changed files from the current session
    pub fn get_changed_files_from_current_session(&self) -> Vec<std::path::PathBuf> {
        if let Some(ref session) = self.current_session {
            session
                .changed_files
                .iter()
                .map(std::path::PathBuf::from)
                .collect()
        } else {
            vec![]
        }
    }

    /// Clear changed files from the current session
    pub fn clear_changed_files_from_current_session(&mut self) -> Result<()> {
        if let Some(ref mut session) = self.current_session {
            session.clear_changed_files();
            self.save_current_session()?;
        }
        Ok(())
    }

    /// Get current session info
    pub fn current_session_info(&self) -> Option<String> {
        self.current_session.as_ref().map(|session| {
            let checkpoint = if self.has_unsaved_current_session() { "unsaved; retry /session save".to_string() }
                else if let SessionSaveState::DurabilityUnconfirmed { session_id, message } = &self.save_state {
                    if session_id == &session.meta.id { format!("replaced; durability unconfirmed: {message}") } else { "saved".into() }
                } else { "saved".into() };
            format!(
                "Current Session:\n  Checkpoint: {}\n  ID: {}\n  Title: {}\n  Created: {}\n  Updated: {}\n  Conversation entries: {}\n  Token count: {}\n  Requests: {}\n  Tool calls: {}\n  Changed files count: {}",
                checkpoint,
                session.meta.id,
                session.meta.title,
                session.meta.created_at,
                session.timestamp,
                session.conversation.len(),
                session.token_count,
                session.requests,
                session.tool_calls,
                session.changed_files.len()
            )
        })
    }

    /// Get detailed session statistics including tool call success/failure counts
    pub fn get_session_statistics(&self) -> Option<String> {
        self.current_session.as_ref().map(|session| {
            // Calculate duration
            let created = chrono::DateTime::parse_from_rfc3339(&session.meta.created_at)
                .unwrap_or_else(|_| chrono::Utc::now().into());
            let updated = chrono::DateTime::parse_from_rfc3339(&session.timestamp)
                .unwrap_or_else(|_| chrono::Utc::now().into());
            let duration = updated.signed_duration_since(created);

            let duration_str = format!(
                "{}m {}s",
                duration.num_minutes(),
                duration.num_seconds() % 60
            );

            // Truncate title manually
            let title_display = if session.meta.title.chars().count() <= 36 {
                session.meta.title.clone()
            } else {
                format!(
                    "{}...",
                    session.meta.title.chars().take(33).collect::<String>()
                )
            };

            let mut stats = String::new();

            // Header
            stats.push_str("\n╭──────────────────────────────────────────────────────╮\n");
            stats.push_str("│                 SESSION STATISTICS                   │\n");
            stats.push_str("├──────────────────────────────────────────────────────┤\n");

            // Core Info
            stats.push_str(&format!("│ 🆔 Session ID : {:<36} │\n", session.meta.id));
            stats.push_str(&format!("│ 📑 Title      : {:<36} │\n", title_display));
            stats.push_str(&format!("│ ⏱️  Duration   : {:<36} │\n", duration_str));
            stats.push_str("├──────────────────────────────────────────────────────┤\n");

            // Metrics
            stats.push_str(&format!("│ 📈 Requests   : {:<36} │\n", session.requests));
            stats.push_str(&format!(
                "│ 🏷️  Tokens     : {:<36} │\n",
                session.token_count
            ));
            stats.push_str(&format!(
                "│ ✏️  Edits      : {:<36} │\n",
                format!(
                    "{} lines ({} files)",
                    session.lines_edited,
                    session.changed_files.len()
                )
            ));
            stats.push_str("├──────────────────────────────────────────────────────┤\n");

            // Tools
            stats.push_str(&format!(
                "│ 🛠️  Tool Calls : {:<36} │\n",
                session.tool_calls
            ));

            // Success/Failure breakdown
            for (name, count) in &session.tool_call_successes {
                stats.push_str(&format!(
                    "│    • {:<9}: {:<25} │\n",
                    name,
                    format!("{} (OK)", count)
                ));
            }

            for (name, count) in &session.tool_call_failures {
                stats.push_str(&format!(
                    "│    • {:<9}: {:<25} │\n",
                    name,
                    format!("{} (Fail)", count)
                ));
            }

            // If no tool details but count > 0
            if session.tool_calls > 0
                && session.tool_call_successes.is_empty()
                && session.tool_call_failures.is_empty()
            {
                stats.push_str("│    (No detailed tool stats available)                │\n");
            }

            stats.push_str("╰──────────────────────────────────────────────────────╯");

            stats
        })
    }

    /// Get the current session ID
    pub fn get_current_session_id(&self) -> Result<String> {
        self.current_session
            .as_ref()
            .map(|session| session.meta.id.clone())
            .ok_or_else(|| anyhow::anyhow!("No current session"))
    }

    /// Storage context for provenance and other per-session durable state.
    ///
    /// Returns the session id plus its on-disk directory without exposing
    /// the whole store layout.
    pub fn current_session_storage_context(&self) -> Option<SessionStorageContext> {
        let session = self.current_session.as_ref()?;
        Some(SessionStorageContext {
            session_id: session.meta.id.clone(),
            session_dir: self.store.session_dir(&session.meta.id),
        })
    }

    /// Mark a provenance recording failure on the current session.
    ///
    /// Never rolls back the committed source change; it only flips the
    /// incomplete flag and bumps the counter so coverage can warn.
    pub fn mark_current_session_provenance_failure(&mut self) -> Result<()> {
        if let Some(ref mut session) = self.current_session {
            session.mark_provenance_failure();
            self.save_current_session()?;
        }
        Ok(())
    }
}

/// Minimal session storage context for provenance and similar per-session
/// durable state. Built only by [`SessionManager`].
#[derive(Debug, Clone)]
pub struct SessionStorageContext {
    pub session_id: String,
    pub session_dir: std::path::PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn capacity_exit_recovery_only_handles_typed_capacity_and_keeps_both_errors() {
        let dir = tempdir().unwrap();
        let mut sm = SessionManager::with_store(SessionStore::new(dir.path()).unwrap());
        assert!(sm.export_current_session().is_err());
        sm.create_session(None).unwrap();
        let snapshot = serde_json::to_value(sm.current_session.as_ref().unwrap()).unwrap();
        let generic = sm.recover_capacity_exit(anyhow::anyhow!("disk full"));
        assert_eq!(generic.to_string(), "disk full");
        assert!(!dir.path().join(".recovery").exists());
        let capacity = || {
            anyhow::Error::new(crate::session::error::SessionError::CapacityExceeded {
                limit: 16 * 1024 * 1024,
                detected_at_least: 17 * 1024 * 1024,
            })
        };
        let error = sm.recover_capacity_exit(capacity());
        assert!(
            error
                .downcast_ref::<crate::session::error::SessionError>()
                .is_some()
        );
        assert_eq!(
            std::fs::read_dir(dir.path().join(".recovery"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            serde_json::to_value(sm.current_session.as_ref().unwrap()).unwrap(),
            snapshot
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.path().join(".recovery"),
                std::fs::Permissions::from_mode(0o500),
            )
            .unwrap();
            let error = sm.recover_capacity_exit(capacity());
            assert!(error.to_string().contains("recovery export also failed"));
            assert!(error.chain().any(|e| matches!(
                e.downcast_ref::<crate::session::error::SessionError>(),
                Some(crate::session::error::SessionError::CapacityExceeded { .. })
            )));
            std::fs::set_permissions(
                dir.path().join(".recovery"),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
    }

    #[test]
    fn lease_busy_transitions_preserve_current_and_release_on_errors() {
        let dir = tempdir().unwrap();
        let store = SessionStore::new(dir.path()).unwrap();
        let mut first = SessionManager::with_store(store.clone());
        first.create_session(Some("owned first".into())).unwrap();
        let a = first.current_session_id().unwrap();
        let mut second = SessionManager::with_store(store.clone());
        second.create_session(Some("owned second".into())).unwrap();
        let b = second.current_session_id().unwrap();
        assert!(
            second
                .load_session(&a)
                .unwrap_err()
                .to_string()
                .contains("already in use")
        );
        assert_eq!(second.current_session_id().as_deref(), Some(b.as_str()));
        let before = serde_json::to_value(first.current_session.as_ref().unwrap()).unwrap();
        assert!(
            first
                .switch_to_validated_session(&b)
                .unwrap_err()
                .to_string()
                .contains("already in use")
        );
        assert_eq!(
            serde_json::to_value(first.current_session.as_ref().unwrap()).unwrap(),
            before
        );
        first.switch_to_validated_session(&a).unwrap(); // Reuses its own lease.
        assert!(matches!(
            store.save(first.current_session.as_ref().unwrap()),
            Err(crate::session::error::SessionError::Busy(_))
        ));
        assert!(matches!(
            store.delete(&a),
            Err(crate::session::error::SessionError::Busy(_))
        ));
        let read = SessionStore::open_existing(dir.path()).unwrap();
        assert_eq!(read.list().unwrap().len(), 2);
        assert!(read.load(&a).is_ok());
        // Latest is owned by second; don't silently select an older session.
        let mut third = SessionManager::with_store(store.clone());
        assert!(
            third
                .load_latest_validated_excluding(None)
                .unwrap_err()
                .to_string()
                .contains("already in use")
        );
        assert!(third.current_session.is_none());
        let absent = "missing-target";
        assert!(third.load_session(absent).is_err());
        assert!(store.try_lease(absent).is_ok()); // Failed load released temporary guard.
        drop(second);
        first.switch_to_validated_session(&b).unwrap();
        assert!(store.try_lease(&a).is_ok()); // Old ownership released after adoption.
        first.delete_session(&b).unwrap();
        assert!(first.current_lease.is_none());
        first.flush_current_session().unwrap();
        assert!(store.load(&b).is_err()); // No resurrection after active delete.
    }

    #[test]
    fn test_new_default() {
        let store = SessionStore::new_default().expect("Failed to create default session store");
        assert!(
            store.root.exists(),
            "Session store root directory should exist"
        );
    }

    #[test]
    fn test_new() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");
        assert_eq!(
            store.root,
            dir.path(),
            "Session store root should match the provided path"
        );
    }

    #[test]
    fn test_load_latest_session_excluding() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");
        let mut session_manager = SessionManager {
            save_state: Default::default(),
            current_lease: None,
            store,
            current_session: None,
        };

        // Simulate the eagerly-created fresh session.
        session_manager
            .create_session(None)
            .expect("Failed to create fresh session");
        let fresh_id = session_manager
            .current_session_id()
            .expect("Fresh session should exist");
        std::thread::sleep(std::time::Duration::from_millis(20));

        // Create a pre-existing session with history.
        session_manager
            .create_session(Some("older real session".to_string()))
            .expect("Failed to create previous session");
        let previous_id = session_manager
            .current_session_id()
            .expect("Previous session should exist");
        // Fresh session is still the most recently created; touch the
        // previous one so it is a realistic resume target.
        let previous = session_manager
            .store
            .load(&previous_id)
            .expect("Failed to load previous session");

        // Resuming latest excluding the fresh session must pick the previous one.
        session_manager.current_session = Some(previous);
        let loaded = session_manager
            .load_latest_session_excluding(Some(&fresh_id))
            .expect("Failed to resume");
        assert!(loaded, "a session should be loaded");
        assert_eq!(
            session_manager.current_session_id().as_deref(),
            Some(previous_id.as_str())
        );

        // Excluding everything yields no load.
        let loaded = session_manager
            .load_latest_session_excluding(None)
            .expect("Failed to resume");
        assert!(loaded, "no exclusion should load the latest session");
        let loaded = session_manager
            .load_latest_session_excluding(Some("zzzz-no-match"))
            .expect("Failed to resume");
        assert!(loaded, "non-matching exclusion should not prevent loading");
        let _ = previous_id;
    }

    #[test]
    fn test_current_session_has_changed_files() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");
        let mut session_manager = SessionManager {
            save_state: Default::default(),
            current_lease: None,
            store,
            current_session: None,
        };

        // No current session
        assert!(!session_manager.current_session_has_changed_files());

        // Create a session with no changed files
        session_manager
            .create_session(None)
            .expect("Failed to create session");
        assert!(!session_manager.current_session_has_changed_files());

        // Add a changed file
        let path = PathBuf::from("/path/to/file.rs");
        session_manager
            .update_current_session_with_changed_file(path)
            .expect("Failed to update session with changed file");
        assert!(session_manager.current_session_has_changed_files());
    }

    #[test]
    fn test_get_changed_files_from_current_session() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");
        let mut session_manager = SessionManager {
            save_state: Default::default(),
            current_lease: None,
            store,
            current_session: None,
        };

        // No current session
        assert_eq!(
            session_manager
                .get_changed_files_from_current_session()
                .len(),
            0
        );

        // Create a session and add changed files
        session_manager
            .create_session(None)
            .expect("Failed to create session");
        let path1 = PathBuf::from("/path/to/file1.rs");
        let path2 = PathBuf::from("/path/to/file2.rs");
        session_manager
            .update_current_session_with_changed_file(path1)
            .expect("Failed to update session with changed file");
        session_manager
            .update_current_session_with_changed_file(path2)
            .expect("Failed to update session with changed file");

        let changed_files = session_manager.get_changed_files_from_current_session();
        assert_eq!(changed_files.len(), 2);
        assert!(changed_files.contains(&PathBuf::from("/path/to/file1.rs")));
        assert!(changed_files.contains(&PathBuf::from("/path/to/file2.rs")));
    }

    #[test]
    fn test_get_session_statistics() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");
        let mut session_manager = SessionManager {
            save_state: Default::default(),
            current_lease: None,
            store,
            current_session: None,
        };

        // Create a session and add some data
        session_manager
            .create_session(Some("Test prompt".to_string()))
            .expect("Failed to create session");
        session_manager
            .update_current_session_with_token_count(1500)
            .expect("Failed to update token count");
        session_manager
            .update_current_session_with_request_count()
            .expect("Failed to update request count");
        session_manager
            .record_tool_call_success("test_tool")
            .expect("Failed to record tool success");
        session_manager
            .record_tool_call_failure("another_tool")
            .expect("Failed to record tool failure");
        session_manager
            .update_current_session_with_changed_file(PathBuf::from("test.rs"))
            .expect("Failed to update changed file");

        let stats = session_manager
            .get_session_statistics()
            .expect("Should have session statistics");

        assert!(stats.contains("SESSION STATISTICS"));
        assert!(stats.contains("Test prompt"));
        assert!(stats.contains("1500"));
        assert!(stats.contains("test_tool"));
        assert!(stats.contains("another_tool"));
        // Changed files now shown as count "1 files" not individual filenames
        assert!(stats.contains("1 files"));
        // Box format uses different emojis
        assert!(stats.contains("📈")); // Requests
        assert!(stats.contains("🛠️")); // Tool Calls
    }

    #[test]
    fn test_clear_changed_files_from_current_session() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");
        let mut session_manager = SessionManager {
            save_state: Default::default(),
            current_lease: None,
            store,
            current_session: None,
        };

        // Create a session and add changed files
        session_manager
            .create_session(None)
            .expect("Failed to create session");
        let path1 = PathBuf::from("/path/to/file1.rs");
        let path2 = PathBuf::from("/path/to/file2.rs");
        session_manager
            .update_current_session_with_changed_file(path1)
            .expect("Failed to update session with changed file");
        session_manager
            .update_current_session_with_changed_file(path2)
            .expect("Failed to update session with changed file");
        assert_eq!(
            session_manager
                .get_changed_files_from_current_session()
                .len(),
            2
        );

        // Clear changed files
        session_manager
            .clear_changed_files_from_current_session()
            .expect("Failed to clear changed files from session");
        assert_eq!(
            session_manager
                .get_changed_files_from_current_session()
                .len(),
            0
        );
    }
    #[test]
    fn recovery_all_memory_first_mutations_track_failures_and_flush_without_reapplying_usage() {
        type Mutation = Box<dyn Fn(&mut SessionManager) -> Result<()>>;
        let mutations: Vec<Mutation> = vec![
            Box::new(|sm| sm.bind_inference("fixture-binding".into())),
            Box::new(|sm| {
                sm.update_current_session_with_history(&[crate::llm::ChatMessage {
                    role: "user".into(),
                    content: Some("pending".into()),
                    reasoning: Default::default(),
                    provider_state: None,
                    tool_calls: vec![],
                    tool_call_id: None,
                }])
            }),
            Box::new(|sm| {
                sm.update_current_session_with_observations(
                    crate::llm::observation::ObservationStore::default(),
                    std::collections::BTreeSet::from(["pending".into()]),
                )
            }),
            Box::new(|sm| sm.update_current_session_with_token_count(12)),
            Box::new(|sm| sm.update_current_session_with_request_count()),
            Box::new(|sm| sm.update_current_session_with_tool_call_count()),
            Box::new(|sm| sm.record_tool_call_success("fixture")),
            Box::new(|sm| sm.record_tool_call_failure("fixture")),
            Box::new(|sm| sm.set_initial_prompt_for_current_session("new title")),
            Box::new(|sm| sm.update_current_session_with_changed_file("fixture.rs".into())),
            Box::new(|sm| sm.clear_changed_files_from_current_session()),
            Box::new(|sm| sm.mark_current_session_provenance_failure()),
        ];
        for mutation in mutations {
            let dir = tempfile::tempdir().unwrap();
            let mut sm =
                SessionManager::with_store(SessionStore::new(dir.path().join("sessions")).unwrap());
            sm.create_session(None).unwrap();
            let id = sm.current_session_id().unwrap();
            let path = sm.store.session_dir(&id).join("session.json");
            let original = std::fs::metadata(&path).unwrap().permissions();
            let mut readonly = original.clone();
            readonly.set_readonly(true);
            std::fs::set_permissions(&path, readonly).unwrap();
            assert!(mutation(&mut sm).is_err());
            assert!(sm.has_unsaved_current_session());
            let expected = serde_json::to_value(sm.current_session.as_ref().unwrap()).unwrap();
            std::fs::set_permissions(&path, original).unwrap();
            sm.flush_current_session().unwrap();
            sm.flush_current_session().unwrap();
            assert!(!sm.has_unsaved_current_session());
            assert_eq!(
                serde_json::to_value(sm.store.load(&id).unwrap()).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn recovery_capacity_and_post_rename_warning_and_deleted_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut sm =
            SessionManager::with_store(SessionStore::new(dir.path().join("sessions")).unwrap());
        assert!(matches!(
            sm.flush_current_session().unwrap(),
            crate::session::store::SessionSaveOutcome::Durable
        ));
        sm.create_session(None).unwrap();
        let id = sm.current_session_id().unwrap();
        sm.current_session.as_mut().unwrap().meta.title = "x".repeat(17 * 1024 * 1024);
        assert!(sm.flush_current_session().is_err());
        assert!(sm.has_unsaved_current_session());
        sm.current_session.as_mut().unwrap().meta.title = "restored".into();
        sm.store.fail_directory_sync = true;
        assert!(matches!(
            sm.flush_current_session().unwrap(),
            crate::session::store::SessionSaveOutcome::DurabilityUnconfirmed { .. }
        ));
        assert!(!sm.has_unsaved_current_session());
        assert!(
            sm.current_session_info()
                .unwrap()
                .contains("durability unconfirmed")
        );
        assert_eq!(sm.store.load(&id).unwrap().meta.title, "restored");
        sm.delete_session(&id).unwrap();
        sm.flush_current_session().unwrap();
        assert!(!sm.store.session_dir(&id).exists());
        assert!(!sm.has_unsaved_current_session());
    }

    fn delta_for_test(
        attempts: u64,
        records: u64,
        total: u64,
    ) -> crate::llm::usage_ledger::UsageLedger {
        crate::llm::usage_ledger::UsageLedger {
            attempts,
            usage_records: records,
            prompt_tokens: total.saturating_sub(20),
            completion_tokens: 20.min(total),
            total_tokens: total,
            ..Default::default()
        }
    }

    #[test]
    fn apply_usage_delta_updates_usage_tokens_and_requests() {
        let dir = tempfile::tempdir().unwrap();
        let mut sm =
            SessionManager::with_store(SessionStore::new(dir.path().join("sessions")).unwrap());
        sm.create_session(None).unwrap();
        let id = sm.current_session_id().unwrap();
        let delta = delta_for_test(1, 1, 150);
        sm.apply_usage_delta(&id, &delta).unwrap();
        let session = sm.current_session.as_ref().unwrap();
        let usage = session.usage.as_ref().unwrap();
        assert_eq!(usage.attempts, 1);
        assert_eq!(usage.usage_records, 1);
        assert_eq!(usage.total_tokens, 150);
        assert_eq!(session.token_count, 150);
        assert_eq!(session.requests, 1);
        assert!(!usage.historical_usage_unknown);
    }

    #[test]
    fn apply_usage_delta_preserves_optional_usage() {
        let dir = tempfile::tempdir().unwrap();
        let mut sm =
            SessionManager::with_store(SessionStore::new(dir.path().join("sessions")).unwrap());
        sm.create_session(None).unwrap();
        let id = sm.current_session_id().unwrap();
        let usage_json = serde_json::json!({"prompt_tokens":100,"completion_tokens":20,"total_tokens":120,"prompt_tokens_details":{"cached_tokens":60,"cache_write_tokens":10},"completion_tokens_details":{"reasoning_tokens":10}});
        let reported: crate::llm::types::Usage = serde_json::from_value(usage_json).unwrap();
        let mut ledger = crate::llm::usage_ledger::UsageLedger {
            attempts: 1,
            ..Default::default()
        };
        ledger.record(&reported);
        sm.apply_usage_delta(&id, &ledger).unwrap();
        let usage = sm.current_session.as_ref().unwrap().usage.as_ref().unwrap();
        assert_eq!(usage.cached_tokens, Some(60));
        assert_eq!(usage.cache_write_tokens, Some(10));
        assert_eq!(usage.reasoning_tokens, Some(10));
        assert_eq!(usage.cached_usage_records, 1);
    }

    #[test]
    fn legacy_session_remains_historical_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let mut sm =
            SessionManager::with_store(SessionStore::new(dir.path().join("sessions")).unwrap());
        sm.create_session(None).unwrap();
        let id = sm.current_session_id().unwrap();
        sm.current_session.as_mut().unwrap().usage = None;
        let delta = delta_for_test(1, 1, 50);
        sm.apply_usage_delta(&id, &delta).unwrap();
        let usage = sm.current_session.as_ref().unwrap().usage.as_ref().unwrap();
        assert!(usage.historical_usage_unknown);
        assert_eq!(usage.total_tokens, 50);
    }

    #[test]
    fn wrong_session_rejects_without_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let mut sm =
            SessionManager::with_store(SessionStore::new(dir.path().join("sessions")).unwrap());
        sm.create_session(None).unwrap();
        let before = serde_json::to_value(sm.current_session.as_ref().unwrap()).unwrap();
        let delta = delta_for_test(1, 1, 150);
        assert!(sm.apply_usage_delta("other-session", &delta).is_err());
        assert_eq!(
            serde_json::to_value(sm.current_session.as_ref().unwrap()).unwrap(),
            before
        );
    }

    #[test]
    fn empty_delta_does_not_save() {
        let dir = tempfile::tempdir().unwrap();
        let mut sm =
            SessionManager::with_store(SessionStore::new(dir.path().join("sessions")).unwrap());
        sm.create_session(None).unwrap();
        let id = sm.current_session_id().unwrap();
        let path = sm.store.session_dir(&id).join("session.json");
        let disk = std::fs::read(&path).unwrap();
        let before = serde_json::to_value(sm.current_session.as_ref().unwrap()).unwrap();
        sm.apply_usage_delta(&id, &crate::llm::usage_ledger::UsageLedger::default())
            .unwrap();
        assert_eq!(
            serde_json::to_value(sm.current_session.as_ref().unwrap()).unwrap(),
            before
        );
        assert_eq!(std::fs::read(&path).unwrap(), disk);
    }

    #[test]
    fn save_failure_exact_once_then_flush() {
        let dir = tempfile::tempdir().unwrap();
        let mut sm =
            SessionManager::with_store(SessionStore::new(dir.path().join("sessions")).unwrap());
        sm.create_session(None).unwrap();
        let id = sm.current_session_id().unwrap();
        let path = sm.store.session_dir(&id).join("session.json");
        let disk = std::fs::read(&path).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).unwrap();
        let delta = delta_for_test(1, 1, 150);
        assert!(sm.apply_usage_delta(&id, &delta).is_err());
        // In-memory delta retained as Unsaved; disk unchanged.
        let session = sm.current_session.as_ref().unwrap();
        assert_eq!(session.usage.as_ref().unwrap().total_tokens, 150);
        assert_eq!(session.token_count, 150);
        assert_eq!(session.requests, 1);
        assert!(sm.has_unsaved_current_session());
        assert_eq!(std::fs::read(&path).unwrap(), disk);
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(0o600);
        }
        #[cfg(not(unix))]
        {
            permissions.set_readonly(false);
        }
        std::fs::set_permissions(&path, permissions).unwrap();
        sm.flush_current_session().unwrap();
        assert!(!sm.has_unsaved_current_session());
        let session = sm.current_session.as_ref().unwrap();
        assert_eq!(session.usage.as_ref().unwrap().total_tokens, 150);
        assert_eq!(session.token_count, 150);
        // Retry never reapplies: still exactly once, not 300.
        sm.flush_current_session().unwrap();
        let reloaded = sm.store.load(&id).unwrap();
        assert_eq!(reloaded.usage.as_ref().unwrap().total_tokens, 150);
        assert_eq!(reloaded.token_count, 150);
        assert_eq!(reloaded.requests, 1);
    }
}
