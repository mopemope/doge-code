use crate::session::{SessionData, SessionStore};
use anyhow::Result;

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
    pub fn create_session(&mut self, title: &str) -> Result<()> {
        let session = self.store.create(title)?;
        self.current_session = Some(session);
        Ok(())
    }

    /// Load a session by ID
    pub fn load_session(&mut self, id: &str) -> Result<()> {
        let session = self.store.load(id)?;
        self.current_session = Some(session);
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

    /// Clear the current session's history
    pub fn clear_current_session_history(&mut self) -> Result<()> {
        if let Some(ref mut session) = self.current_session {
            session.clear_history();
            self.store.save(session)?;
        }
        Ok(())
    }

    /// Update the current session with conversation history
    pub fn update_current_session_with_history(
        &mut self,
        history: &[crate::llm::types::ChatMessage],
    ) -> Result<()> {
        if let Some(ref mut session) = self.current_session {
            // Clear existing history
            session.clear_history();

            // Add each message to the session history
            // For simplicity, we'll serialize the ChatMessage to JSON
            for msg in history {
                if let Ok(serialized) = serde_json::to_string(msg) {
                    session.add_to_history(serialized);
                }
            }

            self.store.save(session)?;
        }
        Ok(())
    }

    /// Get current session info
    pub fn current_session_info(&self) -> Option<String> {
        self.current_session.as_ref().map(|session| {
            format!(
                "Current Session:\n  ID: {}\n  Title: {}\n  Created: {}\n  History entries: {}",
                session.meta.id,
                session.meta.title,
                session.meta.created_at,
                session.history.len()
            )
        })
    }
}
