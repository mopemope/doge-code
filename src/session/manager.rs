use crate::session::{SessionData, SessionStore};
use anyhow::Result;
use tracing::{debug, error as tracing_error};

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

    /// Create a new session with an optional initial prompt.
    /// If `initial_prompt` is provided the session title will be set and persisted.
    pub fn create_session(&mut self, initial_prompt: Option<String>) -> Result<()> {
        let mut session = self.store.create()?;

        if let Some(prompt) = initial_prompt {
            // Use owned String; pass &str to session API
            session.set_initial_prompt(&prompt);
            // Mark that the title is user-provided
            session.meta.title_is_default = false;
            if let Err(e) = self.store.save(&session) {
                tracing_error!(
                    ?e,
                    "Failed to save session data after setting initial prompt"
                );
                return Err(e.into());
            }
        }

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
                && let Some(prompt) = first_user_prompt
            {
                session.set_initial_prompt(&prompt);
                // Mark that the title is now user-provided
                session.meta.title_is_default = false;
                debug!(
                    "Overrode default session title with first user prompt (truncated to 30 chars)"
                );
            }

            if let Err(e) = self.store.save(session) {
                tracing_error!(?e, "Failed to save session data");
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
                tracing_error!(?e, "Failed to save session data");
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
                tracing_error!(?e, "Failed to save session data");
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
                tracing_error!(?e, "Failed to save session data");
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
                tracing_error!(?e, "Failed to save session data");
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
                tracing_error!(?e, "Failed to save session data");
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
                tracing_error!(?e, "Failed to save session data");
                return Err(e.into());
            }
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
            if let Err(e) = self.store.save(session) {
                tracing_error!(?e, "Failed to save session data after adding changed file");
                return Err(e.into());
            }
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
            if let Err(e) = self.store.save(session) {
                tracing_error!(
                    ?e,
                    "Failed to save session data after clearing changed files"
                );
                return Err(e.into());
            }
        }
        Ok(())
    }

    /// Get current session info
    pub fn current_session_info(&self) -> Option<String> {
        self.current_session.as_ref().map(|session| {
            format!(
                "Current Session:\n  ID: {}\n  Title: {}\n  Created: {}\n  Updated: {}\n  Conversation entries: {}\n  Token count: {}\n  Requests: {}\n  Tool calls: {}\n  Changed files count: {}",
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
            let mut stats = format!(
                "\n=== Session Statistics ===\n\
                 ID: {}\n\
                 Title: {}\n\
                 Created: {}\n\
                 Updated: {}\n\
                 Requests: {}\n\
                 Token count: {}\n\
                 Tool calls: {}\n\
                 Lines edited: {}\n\
                 Changed files: {}",
                session.meta.id,
                session.meta.title,
                session.meta.created_at,
                session.timestamp,
                session.requests,
                session.token_count,
                session.tool_calls,
                session.lines_edited,
                session.changed_files.len()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

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
    fn test_current_session_has_changed_files() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");
        let mut session_manager = SessionManager {
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
    fn test_clear_changed_files_from_current_session() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");
        let mut session_manager = SessionManager {
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
}
