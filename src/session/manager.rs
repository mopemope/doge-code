use crate::session::{SessionData, SessionStore};
use anyhow::Result;
use tracing::debug;

/// Session management for TUI
#[derive(Debug)]
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

            // Find the first user prompt in the history to set session title if not set
            let first_user_prompt = history
                .iter()
                .find(|m| {
                    m.role == "user" && m.content.as_ref().map(|s| !s.is_empty()).unwrap_or(false)
                })
                .and_then(|m| m.content.clone());

            // Add each message to the session conversation
            for msg in history {
                // Convert serde_json::Value to HashMap<String, serde_json::Value>
                if let Ok(serde_json::Value::Object(map)) = serde_json::to_value(msg) {
                    session.add_conversation_entry(map.into_iter().collect());
                }
            }

            // If the session title is default (auto-generated) and we have a first user prompt, override it
            if session.meta.title_is_default
                && let Some(prompt) = first_user_prompt {
                    session.set_initial_prompt(&prompt);
                    // Mark that the title is now user-provided
                    session.meta.title_is_default = false;
                    tracing::debug!(
                        "Overrode default session title with first user prompt (truncated to 30 chars)"
                    );
                }

            if let Err(e) = self.store.save(session) {
                tracing::error!(?e, "Failed to save session data");
                return Err(e.into());
            }
        }
        Ok(())
    }

    /// Update the current session with token count
    pub fn update_current_session_with_token_count(&mut self, token_count: u64) -> Result<()> {
        if let Some(ref mut session) = self.current_session {
            session.increment_token_count(token_count);
            if let Err(e) = self.store.save(session) {
                tracing::error!(?e, "Failed to save session data");
                return Err(e.into());
            }
        }
        Ok(())
    }

    /// Update the current session with request count
    pub fn update_current_session_with_request_count(&mut self) -> Result<()> {
        if let Some(ref mut session) = self.current_session {
            session.increment_requests();
            if let Err(e) = self.store.save(session) {
                tracing::error!(?e, "Failed to save session data");
                return Err(e.into());
            }
        }
        Ok(())
    }

    /// Update the current session with tool call count
    pub fn update_current_session_with_tool_call_count(&mut self) -> Result<()> {
        if let Some(ref mut session) = self.current_session {
            session.increment_tool_calls();
            if let Err(e) = self.store.save(session) {
                tracing::error!(?e, "Failed to save session data");
                return Err(e.into());
            }
        }
        Ok(())
    }

    /// Record a successful tool call in the current session
    pub fn record_tool_call_success(&mut self, tool_name: &str) -> Result<()> {
        if let Some(ref mut session) = self.current_session {
            session.record_tool_call_success(tool_name);
            if let Err(e) = self.store.save(session) {
                tracing::error!(?e, "Failed to save session data");
                return Err(e.into());
            }
        }
        Ok(())
    }

    /// Record a failed tool call in the current session
    pub fn record_tool_call_failure(&mut self, tool_name: &str) -> Result<()> {
        if let Some(ref mut session) = self.current_session {
            session.record_tool_call_failure(tool_name);
            if let Err(e) = self.store.save(session) {
                tracing::error!(?e, "Failed to save session data");
                return Err(e.into());
            }
        }
        Ok(())
    }

    /// Set the initial prompt for the current session
    pub fn set_initial_prompt_for_current_session(&mut self, prompt: &str) -> Result<()> {
        if let Some(ref mut session) = self.current_session {
            session.set_initial_prompt(prompt);
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
                "Current Session:\n  ID: {}\n  Title: {}\n  Created: {}\n  Updated: {}\n  Conversation entries: {}\n  Token count: {}\n  Requests: {}\n  Tool calls: {}",
                session.meta.id,
                session.meta.title,
                session.meta.created_at,
                session.timestamp,
                session.conversation.len(),
                session.token_count,
                session.requests,
                session.tool_calls
            )
        })
    }

    /// Get detailed session statistics including tool call success/failure counts
    pub fn get_session_statistics(&self) -> Option<String> {
        self.current_session.as_ref().map(|session| {
            let mut stats = format!(
                "\n=== Session Statistics ===\n\
                 ID: {}\n\
                 Title: {}\n\
                 Created: {}\n\
                 Updated: {}\n\
                 Requests: {}\n\
                 Token count: {}\n\
                 Tool calls: {}\n\
                 Lines edited: {}",
                session.meta.id,
                session.meta.title,
                session.meta.created_at,
                session.timestamp,
                session.requests,
                session.token_count,
                session.tool_calls,
                session.lines_edited
            );

            // Add tool call success statistics
            if !session.tool_call_successes.is_empty() {
                stats.push_str("\n\nTool Call Successes:");
                for (tool_name, count) in &session.tool_call_successes {
                    stats.push_str(&format!("\n  {}: {}", tool_name, count));
                }
            }

            // Add tool call failure statistics
            if !session.tool_call_failures.is_empty() {
                stats.push_str("\n\nTool Call Failures:");
                for (tool_name, count) in &session.tool_call_failures {
                    stats.push_str(&format!("\n  {}: {}", tool_name, count));
                }
            }

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
}
