use crate::session::{SessionData, SessionStore};
use anyhow::Result;
use tracing::debug;

/// Session management for TUI
pub struct SessionManager {
    pub store: SessionStore,
    pub current_session: Option<SessionData>,
}

impl SessionManager {
    /// Create a new SessionManager with the default store location
    pub fn new() -> Result<Self> {
        let store = SessionStore::new_default()?;
        Ok(Self {
            store,
            current_session: None,
        })
    }

    /// List all sessions
    pub fn list_sessions(&self) -> Result<Vec<crate::session::SessionMeta>> {
        self.store.list().map_err(|e| anyhow::anyhow!(e))
    }

    /// Create a new session
    pub fn create_session(&mut self) -> Result<()> {
        let session = self.store.create()?;
        self.current_session = Some(session);
        Ok(())
    }

    /// Load a session by ID
    pub fn load_session(&mut self, id: &str) -> Result<()> {
        let session = self.store.load(id)?;
        self.current_session = Some(session);
        Ok(())
    }

    /// Load the latest session
    pub fn load_latest_session(&mut self) -> Result<()> {
        if let Some(session) = self.store.get_latest()? {
            self.current_session = Some(session);
        }
        Ok(())
    }

    /// Delete a session by ID
    pub fn delete_session(&mut self, id: &str) -> Result<()> {
        self.store.delete(id)?;
        // If the current session is the one being deleted, clear it
        if let Some(current) = &self.current_session
            && current.meta.id == id
        {
            self.current_session = None;
        }
        Ok(())
    }

    /// Clear the current session's conversation
    pub fn clear_current_session_conversation(&mut self) -> Result<()> {
        if let Some(ref mut session) = self.current_session {
            session.clear_conversation();
            self.store.save(session)?;
        }
        Ok(())
    }

    /// Update the current session with conversation history
    pub fn update_current_session_with_history(
        &mut self,
        history: &[crate::llm::types::ChatMessage],
    ) -> Result<()> {
        debug!(
            "Updating session with history: {:?} session: {:?}",
            history, &self.current_session
        );
        if let Some(ref mut session) = self.current_session {
            // Clear existing conversation
            session.clear_conversation();

            // Add each message to the session conversation
            for msg in history {
                // Convert serde_json::Value to HashMap<String, serde_json::Value>
                if let Ok(serde_json::Value::Object(map)) = serde_json::to_value(msg) {
                    session.add_conversation_entry(map.into_iter().collect());
                }
            }

            if let Err(e) = self.store.save(session) {
                tracing::error!(?e, "Failed to save session data");
                return Err(e.into());
            }
        }
        Ok(())
    }

    /// Get current session info
    pub fn current_session_info(&self) -> Option<String> {
        self.current_session.as_ref().map(|session| {
            format!(
                "Current Session:\n  ID: {}\n  Created: {}\n  Updated: {}\n  Conversation entries: {}\n  Token count: {}\n  Requests: {}\n  Tool calls: {}",
                session.meta.id,
                session.meta.created_at,
                session.timestamp,
                session.conversation.len(),
                session.token_count,
                session.requests,
                session.tool_calls
            )
        })
    }
}
