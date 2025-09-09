use crate::session::data::{SessionData, SessionMeta};
use crate::session::error::SessionError;
use std::env;
use std::fs;
use std::path::PathBuf;
use tracing::error;

/// Maximum number of sessions to keep
const MAX_SESSIONS: usize = 100;

#[derive(Debug, Clone)]
pub struct SessionStore {
    pub(crate) root: PathBuf,
}

impl SessionStore {
    /// Create a new SessionStore. Session data is stored in .doge/sessions in the project directory.
    pub fn new_default() -> Result<Self, SessionError> {
        let base = default_store_dir()?;
        fs::create_dir_all(&base).map_err(|e| {
            error!(?e, "Failed to create session store directory: {:?}", base);
            SessionError::CreateDirError(e)
        })?;
        Ok(Self { root: base })
    }

    /// Create a SessionStore with the specified path as the root directory.
    #[allow(dead_code)]
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, SessionError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|e| {
            error!(?e, "Failed to create session store directory: {:?}", root);
            SessionError::CreateDirError(e)
        })?;
        Ok(Self { root })
    }

    /// Get metadata for all sessions and return them sorted by creation date in descending order.
    pub fn list(&self) -> Result<Vec<SessionMeta>, SessionError> {
        let mut out = Vec::new();
        if !self.root.exists() {
            return Ok(out);
        }
        for entry in fs::read_dir(&self.root).map_err(SessionError::ReadError)? {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let p = entry.path();
            if !p.is_dir() {
                continue;
            }
            let session_p = p.join("session.json");
            if let Ok(s) = fs::read_to_string(&session_p).map_err(SessionError::ReadError)
                && let Ok(session_data) =
                    serde_json::from_str::<SessionData>(&s).map_err(SessionError::ParseError)
            {
                out.push(session_data.meta);
            }
        }
        // Sort by created_at in descending order (newest first)
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    /// Create a new session and return the session data.
    /// Automatically cleans up old sessions if the limit is exceeded.
    pub fn create(&self) -> Result<SessionData, SessionError> {
        // Create a new session using SessionData::new
        let data = SessionData::new();

        self.save(&data)?; // Save the session using the save method

        // Clean up old sessions if we exceed the limit
        cleanup_old_sessions(self)?;

        Ok(data)
    }

    /// Load session data by specifying the session ID.
    pub fn load(&self, id: &str) -> Result<SessionData, SessionError> {
        // Validate ID format if necessary
        if id.is_empty() {
            return Err(SessionError::InvalidId(id.to_string()));
        }
        let dir = self.root.join(id);
        if !dir.exists() {
            return Err(SessionError::NotFound(id.to_string()));
        }

        // Load the entire session data from a single JSON file
        let session_file = dir.join("session.json");
        let session_s = fs::read_to_string(session_file).map_err(SessionError::ReadError)?;
        let session_data: SessionData =
            serde_json::from_str(&session_s).map_err(SessionError::ParseError)?;

        Ok(session_data)
    }

    /// Save the session data.
    /// Automatically cleans up old sessions if the limit is exceeded.
    pub fn save(&self, data: &SessionData) -> Result<(), SessionError> {
        let dir = self.root.join(&data.meta.id);
        fs::create_dir_all(&dir).map_err(SessionError::CreateDirError)?;

        // Save the entire session data as a single JSON file
        let session_file = dir.join("session.json");
        let json_data = serde_json::to_string_pretty(data)?;
        fs::write(&session_file, &json_data).map_err(|e| {
            error!(
                ?e,
                "Failed to write session data to file: {:?}", session_file
            );
            SessionError::WriteError(e)
        })?;

        // Clean up old sessions if we exceed the limit
        cleanup_old_sessions(self)?;

        Ok(())
    }

    /// Delete session data by specifying the session ID.
    pub fn delete(&self, id: &str) -> Result<(), SessionError> {
        if id.is_empty() {
            return Err(SessionError::InvalidId(id.to_string()));
        }
        let dir = self.root.join(id);
        if dir.exists() {
            fs::remove_dir_all(dir).map_err(SessionError::DeleteError)?;
        }
        Ok(())
    }

    /// Get the latest session data.
    pub fn get_latest(&self) -> Result<Option<SessionData>, SessionError> {
        let sessions = self.list()?;
        if let Some(latest_meta) = sessions.first() {
            let session = self.load(&latest_meta.id)?;
            Ok(Some(session))
        } else {
            Ok(None)
        }
    }
}

fn default_store_dir() -> Result<PathBuf, SessionError> {
    // Use .doge/sessions in the project directory
    let project_dir = env::current_dir().map_err(SessionError::ReadError)?;
    let base = project_dir.join(".doge/sessions");
    Ok(base)
}

/// Clean up old sessions if we exceed the maximum limit
fn cleanup_old_sessions(store: &SessionStore) -> Result<(), SessionError> {
    let sessions = store.list()?;
    if sessions.len() > MAX_SESSIONS {
        // Calculate how many sessions to delete
        let excess_count = sessions.len() - MAX_SESSIONS;

        // The sessions are sorted by creation date in descending order (newest first)
        // So we need to delete from the end of the vector (oldest sessions)
        for session_meta in sessions.iter().skip(MAX_SESSIONS) {
            store.delete(&session_meta.id)?;
        }

        tracing::info!(
            "Cleaned up {} old sessions to maintain limit of {}",
            excess_count,
            MAX_SESSIONS
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn test_list_empty() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");
        let sessions = store.list().expect("Failed to list sessions");
        assert!(sessions.is_empty(), "Sessions list should be empty");
    }

    #[test]
    fn test_create_and_list() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");

        let session1 = store.create().expect("Failed to create session1");
        let session2 = store.create().expect("Failed to create session 2");
        let sessions = store.list().expect("Failed to list sessions");
        assert_eq!(sessions.len(), 2, "Should have 2 sessions");
        // Just check that the sessions are listed, not the order
        let session_ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert!(
            session_ids.contains(&session1.meta.id.as_str()),
            "Session1 should be in the list"
        );
        assert!(
            session_ids.contains(&session2.meta.id.as_str()),
            "Session2 should be in the list"
        );
    }

    #[test]
    fn test_create_and_load() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");

        let created_session = store.create().expect("Failed to create session");
        let loaded_session = store
            .load(&created_session.meta.id)
            .expect("Failed to load session");
        assert_eq!(
            loaded_session.meta.id, created_session.meta.id,
            "Session IDs should match"
        );
        assert_eq!(
            loaded_session.conversation, created_session.conversation,
            "Session conversations should match"
        );
        assert_eq!(
            loaded_session.timestamp, created_session.timestamp,
            "Session timestamp should match"
        );
        assert_eq!(
            loaded_session.token_count, created_session.token_count,
            "Session token_count should match"
        );
        assert_eq!(
            loaded_session.requests, created_session.requests,
            "Session requests should match"
        );
        assert_eq!(
            loaded_session.tool_calls, created_session.tool_calls,
            "Session tool_calls should match"
        );
    }

    #[test]
    fn test_save() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");

        let mut session = store.create().expect("Failed to create session");
        let mut entry = std::collections::HashMap::new();
        entry.insert(
            "test".to_string(),
            serde_json::Value::String("entry".to_string()),
        );
        session.add_conversation_entry(entry);
        session.increment_token_count(10);
        session.increment_requests();
        session.increment_tool_calls();
        store.save(&session).expect("Failed to save session");
        let loaded_session = store
            .load(&session.meta.id)
            .expect("Failed to load session");
        assert_eq!(
            loaded_session.conversation.len(),
            1,
            "Conversation should have one entry"
        );
        assert_eq!(loaded_session.token_count, 10, "Token count should be 10");
        assert_eq!(loaded_session.requests, 1, "Requests count should be 1");
        assert_eq!(loaded_session.tool_calls, 1, "Tool calls count should be 1");
    }

    #[test]
    fn test_delete() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");

        let session = store.create().expect("Failed to create session");
        let session_id = session.meta.id;
        store.delete(&session_id).expect("Failed to delete session");

        let sessions = store.list().expect("Failed to list sessions");
        assert!(
            sessions.is_empty(),
            "Sessions list should be empty after deletion"
        );
        let load_result = store.load(&session_id);
        assert!(load_result.is_err(), "Loading deleted session should fail");
    }

    #[test]
    fn test_load_not_found() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");

        let result = store.load("non-existent-id");
        assert!(result.is_err(), "Loading non-existent session should fail");
    }

    #[test]
    fn test_delete_not_found() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");
        let result = store.delete("non-existent-id");
        assert!(
            result.is_ok(),
            "Deleting non-existent session should not fail"
        );
    }

    #[test]
    fn test_invalid_id() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");

        let load_result = store.load("");
        assert!(load_result.is_err(), "Loading with empty ID should fail");
        let delete_result = store.delete("");
        assert!(delete_result.is_err(), "Deleting with empty ID should fail");
    }

    #[test]
    fn test_get_latest() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");

        // If no session exists
        let latest = store.get_latest().expect("Failed to get latest session");
        assert!(
            latest.is_none(),
            "Should return None when no sessions exist"
        );

        // Create a session
        let _session1 = store.create().expect("Failed to create session 1");
        let session2 = store.create().expect("Failed to create session 2");

        // Get the latest session
        let latest = store.get_latest().expect("Failed to get latest session");
        assert!(latest.is_some(), "Should return Some when sessions exist");
        assert_eq!(
            latest.unwrap().meta.id,
            session2.meta.id,
            "Should return the most recently created session"
        );
    }

    #[test]
    fn test_session_limit_and_cleanup() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");

        // Create more sessions than the limit
        for _ in 0..105 {
            store.create().expect("Failed to create session");
        }

        // Check that we only have the maximum allowed sessions
        let sessions = store.list().expect("Failed to list sessions");
        assert_eq!(
            sessions.len(),
            MAX_SESSIONS,
            "Should limit sessions to MAX_SESSIONS"
        );

        // Create one more session
        store.create().expect("Failed to create session");

        // Check that we still have the maximum allowed sessions
        let sessions = store.list().expect("Failed to list sessions");
        assert_eq!(
            sessions.len(),
            MAX_SESSIONS,
            "Should still limit sessions to MAX_SESSIONS"
        );
    }
}
