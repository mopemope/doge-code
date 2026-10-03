use crate::session::data::{SessionData, SessionMeta, SessionSummary};
use crate::session::error::SessionError;
use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use tracing::error;

/// Maximum number of sessions to keep
const MAX_SESSIONS: usize = 100;
/// Shared save/read limit, including recoverable observations.
const MAX_SESSION_BYTES: u64 = 16 * 1024 * 1024;

fn validate_id(id: &str) -> Result<(), SessionError> {
    if id.is_empty() || id == "." || id == ".." || id.contains(['/', '\\', '\0']) {
        return Err(SessionError::InvalidId(id.to_owned()));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct SessionStore {
    pub(crate) root: PathBuf,
    read_only: bool,
}

impl SessionStore {
    /// Create a new SessionStore. Session data is stored in .doge/sessions in the project directory.
    pub fn new_default() -> Result<Self, SessionError> {
        let base = default_store_dir()?;
        fs::create_dir_all(&base).map_err(|e| {
            error!(?e, "Failed to create session store directory: {:?}", base);
            SessionError::CreateDirError(e)
        })?;
        Ok(Self {
            root: base,
            read_only: false,
        })
    }

    /// Create a SessionStore with the specified path as the root directory.
    #[allow(dead_code)]
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, SessionError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|e| {
            error!(?e, "Failed to create session store directory: {:?}", root);
            SessionError::CreateDirError(e)
        })?;
        Ok(Self {
            root,
            read_only: false,
        })
    }

    /// Open an existing store without creating directories or cleaning sessions.
    pub fn open_existing(root: impl Into<PathBuf>) -> Result<Self, SessionError> {
        let root = root.into();
        if !root.is_dir() {
            return Err(SessionError::NotFound("session store".to_string()));
        }
        for (index, entry) in fs::read_dir(&root)
            .map_err(SessionError::ReadError)?
            .enumerate()
        {
            if index >= 10_000 {
                return Err(SessionError::ReadError(std::io::Error::other(
                    "session inventory exceeds read limit",
                )));
            }
            let entry = entry.map_err(SessionError::ReadError)?;
            let kind = entry.file_type().map_err(SessionError::ReadError)?;
            if kind.is_symlink() {
                return Err(SessionError::InvalidId("symlink in session store".into()));
            }
            if kind.is_dir() {
                let metadata = fs::symlink_metadata(entry.path().join("session.json"));
                if metadata.is_ok_and(|m| !m.is_file() || m.file_type().is_symlink()) {
                    return Err(SessionError::InvalidId("unsafe session metadata".into()));
                }
            }
        }
        Ok(Self {
            root,
            read_only: true,
        })
    }

    fn read_session_file(&self, path: &std::path::Path) -> Result<String, SessionError> {
        use std::io::Read;
        // The configured store root may use a normal alias (e.g. /var on
        // macOS). Below this explicit canonical anchor, follow no symlinks.
        let anchor = self.root.canonicalize().map_err(SessionError::ReadError)?;
        let relative = path
            .strip_prefix(&self.root)
            .ok()
            .and_then(|p| p.to_str())
            .ok_or_else(|| SessionError::InvalidId("unsafe session path".into()))?;
        let file = crate::features::verification_snapshot::open_relative(&anchor, relative)
            .map_err(SessionError::ReadError)?;
        let meta = file.metadata().map_err(SessionError::ReadError)?;
        if !meta.is_file() || meta.len() > MAX_SESSION_BYTES {
            return Err(SessionError::ReadError(std::io::Error::other(
                "unsafe or oversized session metadata",
            )));
        }
        let mut bytes = Vec::new();
        file.take(MAX_SESSION_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(SessionError::ReadError)?;
        if bytes.len() as u64 > MAX_SESSION_BYTES {
            return Err(SessionError::ReadError(std::io::Error::other(
                "session metadata exceeds read limit",
            )));
        }
        String::from_utf8(bytes).map_err(|e| {
            SessionError::ReadError(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        })
    }

    fn ensure_writable(&self) -> Result<(), SessionError> {
        if self.read_only {
            return Err(SessionError::WriteError(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "read-only session store",
            )));
        }
        Ok(())
    }

    /// Get metadata for all sessions, sorted by last update in descending
    /// order (most recently active first). Convenience wrapper over
    /// [`SessionStore::list_with_stats`].
    pub fn list(&self) -> Result<Vec<SessionMeta>, SessionError> {
        self.list_with_stats()
            .map(|summaries| summaries.into_iter().map(|s| s.meta).collect())
    }

    /// Get lightweight summaries for all sessions, sorted by last update in
    /// descending order (most recently active first).
    pub fn list_with_stats(&self) -> Result<Vec<SessionSummary>, SessionError> {
        let mut out = Vec::new();
        let mut scanned_bytes = 0usize;
        let mut scanned_entries = 0usize;
        if !self.root.exists() {
            return Ok(out);
        }
        for entry in fs::read_dir(&self.root).map_err(SessionError::ReadError)? {
            let entry = match entry {
                Ok(e) => e,
                Err(error) if self.read_only => return Err(SessionError::ReadError(error)),
                Err(_) => continue,
            };
            scanned_entries += 1;
            if scanned_entries > 10_000 {
                return Err(SessionError::ReadError(std::io::Error::other(
                    "session inventory exceeds read limit",
                )));
            }
            let p = entry.path();
            if !p.is_dir() {
                continue;
            }
            let session_p = p.join("session.json");
            if p.is_symlink() || session_p.is_symlink() {
                return Err(SessionError::InvalidId(
                    "symlink in session inventory".into(),
                ));
            }
            // Incomplete directories without metadata are not sessions. A
            // present corrupt/unsafe file is an explicit inventory error.
            if !session_p.exists() {
                continue;
            }
            let text = self.read_session_file(&session_p)?;
            scanned_bytes += text.len();
            if scanned_bytes > 128 * 1024 * 1024 {
                return Err(SessionError::ReadError(std::io::Error::other(
                    "session inventory bytes exceed read limit",
                )));
            }
            let session_data: SessionData = serde_json::from_str(&text)?;
            let directory_id = entry
                .file_name()
                .into_string()
                .map_err(|_| SessionError::InvalidId("non-UTF8 session directory".into()))?;
            validate_id(&directory_id)?;
            if session_data.meta.id != directory_id {
                return Err(SessionError::InvalidId(
                    "session metadata ID differs from its directory".into(),
                ));
            }
            out.push(session_data.summary());
        }
        // Sort by updated_at in descending order (most recently active first).
        // updated_at is stored as an RFC3339 string; try to parse it for
        // accurate ordering. If parsing fails for either entry, fall back to
        // string comparison.
        out.sort_by(|a, b| {
            let a_dt = chrono::DateTime::parse_from_rfc3339(&a.updated_at).ok();
            let b_dt = chrono::DateTime::parse_from_rfc3339(&b.updated_at).ok();
            match (a_dt, b_dt) {
                (Some(a_dt), Some(b_dt)) => b_dt.cmp(&a_dt),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => b.updated_at.cmp(&a.updated_at),
            }
        });
        Ok(out)
    }

    /// Create a new session and return the session data.
    /// Automatically cleans up old sessions if the limit is exceeded.
    pub fn create(&self) -> Result<SessionData, SessionError> {
        // Create a new session using SessionData::new
        let data = SessionData::new();

        self.save(&data)?; // Save the session using the save method

        Ok(data)
    }

    /// Load session data by specifying the session ID.
    pub fn load(&self, id: &str) -> Result<SessionData, SessionError> {
        validate_id(id)?;
        let dir = self.root.join(id);
        if dir.is_symlink() {
            return Err(SessionError::InvalidId("symlink session directory".into()));
        }
        if !dir.exists() {
            return Err(SessionError::NotFound(id.to_owned()));
        }
        // Load the entire session data from a single JSON file
        let session_file = dir.join("session.json");
        let session_s = self.read_session_file(&session_file)?;
        let session_data: SessionData =
            serde_json::from_str(&session_s).map_err(SessionError::ParseError)?;

        if session_data.meta.id != id {
            return Err(SessionError::InvalidId(
                "session metadata ID differs from its directory".into(),
            ));
        }
        Ok(session_data)
    }

    /// Save the session data.
    /// Automatically cleans up old sessions if the limit is exceeded.
    pub fn save(&self, data: &SessionData) -> Result<(), SessionError> {
        self.ensure_writable()?;
        validate_id(&data.meta.id)?;
        let json_data = serde_json::to_string_pretty(data)?;
        if json_data.len() as u64 > MAX_SESSION_BYTES {
            return Err(SessionError::WriteError(std::io::Error::other(
                "session metadata exceeds 16 MiB; previous checkpoint retained",
            )));
        }
        let anchor = self.root.canonicalize().map_err(SessionError::WriteError)?;
        let dir = anchor.join(&data.meta.id);
        if dir.is_symlink() {
            return Err(SessionError::InvalidId("symlink session directory".into()));
        }
        fs::create_dir_all(&dir).map_err(SessionError::CreateDirError)?;
        let session_file = dir.join("session.json");
        let permissions = match fs::symlink_metadata(&session_file) {
            Ok(meta)
                if meta.is_file()
                    && !meta.file_type().is_symlink()
                    && !meta.permissions().readonly() =>
            {
                Some(meta.permissions())
            }
            Ok(_) => {
                return Err(SessionError::WriteError(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "unsafe or read-only session metadata",
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(SessionError::WriteError(e)),
        };
        let mut candidate =
            tempfile::NamedTempFile::new_in(&dir).map_err(SessionError::WriteError)?;
        candidate
            .write_all(json_data.as_bytes())
            .map_err(SessionError::WriteError)?;
        if let Some(permissions) = permissions {
            candidate
                .as_file()
                .set_permissions(permissions)
                .map_err(SessionError::WriteError)?;
        }
        candidate
            .as_file()
            .sync_all()
            .map_err(SessionError::WriteError)?;
        candidate
            .persist(&session_file)
            .map_err(|e| SessionError::WriteError(e.error))?;
        #[cfg(unix)]
        fs::File::open(&dir)
            .and_then(|file| file.sync_all())
            .map_err(|e| {
                SessionError::WriteError(std::io::Error::other(format!(
                    "session replaced, but directory sync failed: {e}"
                )))
            })?;
        if let Err(error) = cleanup_old_sessions(self, Some(&data.meta.id)) {
            tracing::warn!(%error, "session checkpoint saved; retention cleanup skipped");
        }

        Ok(())
    }

    /// Delete session data by specifying the session ID.
    pub fn delete(&self, id: &str) -> Result<(), SessionError> {
        self.ensure_writable()?;
        validate_id(id)?;
        let dir = self.root.join(id);
        if dir.is_symlink() {
            return Err(SessionError::InvalidId("symlink session directory".into()));
        }
        match fs::remove_dir_all(&dir) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(SessionError::DeleteError(e));
            }
            _ => {}
        }
        Ok(())
    }

    /// Get the latest session data.
    ///
    /// "Latest" means the most recently updated session (not merely the most
    /// recently created one), which matches the expectation when resuming work.
    pub fn get_latest(&self) -> Result<Option<SessionData>, SessionError> {
        let sessions = self.list_with_stats()?;
        if let Some(latest_meta) = sessions.first() {
            let session = self.load(&latest_meta.meta.id)?;
            Ok(Some(session))
        } else {
            Ok(None)
        }
    }

    /// Resolve a session ID from a (possibly partial) prefix.
    ///
    /// Returns the full ID when exactly one session matches. Full IDs always
    /// match themselves first, so passing a complete UUID is never ambiguous.
    pub fn resolve_id_prefix(&self, prefix: &str) -> Result<String, SessionError> {
        if prefix.is_empty() {
            return Err(SessionError::InvalidId(prefix.to_string()));
        }
        let sessions = self.list_with_stats()?;
        if let Some(exact) = sessions.iter().find(|s| s.meta.id == prefix) {
            return Ok(exact.meta.id.clone());
        }
        let matches: Vec<&SessionSummary> = sessions
            .iter()
            .filter(|s| s.meta.id.starts_with(prefix))
            .collect();
        match matches.len() {
            0 => Err(SessionError::NotFound(prefix.to_string())),
            1 => Ok(matches[0].meta.id.clone()),
            _ => Err(SessionError::AmbiguousId(
                prefix.to_string(),
                matches.iter().map(|s| s.meta.id.clone()).collect(),
            )),
        }
    }
    /// Directory holding one session (`session.json` plus `provenance/`).
    ///
    /// Single source of truth for session storage layout; callers must not
    /// hand-assemble `.doge/sessions/...` paths.
    pub fn session_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }
}

fn default_store_dir() -> Result<PathBuf, SessionError> {
    // Check for environment variable override (useful for testing)
    if let Ok(dir) = env::var("DOGE_SESSIONS_DIR") {
        return Ok(PathBuf::from(dir));
    }
    // Use .doge/sessions in the project directory or fallback to temp
    let project_dir = env::current_dir()
        .map_err(SessionError::ReadError)
        .unwrap_or_else(|_| std::env::temp_dir());
    let base = project_dir.join(".doge/sessions");
    Ok(base)
}

/// Clean up old sessions if we exceed the maximum limit
fn cleanup_old_sessions(store: &SessionStore, protected: Option<&str>) -> Result<(), SessionError> {
    // Most checkpoints need no retention work. Count directory entries without
    // reading every historical conversation, and never delete unknown metadata.
    let mut directories = 0usize;
    for (index, entry) in fs::read_dir(&store.root)
        .map_err(SessionError::ReadError)?
        .enumerate()
    {
        if index >= 10_000 {
            return Err(SessionError::ReadError(std::io::Error::other(
                "session inventory exceeds read limit",
            )));
        }
        let entry = entry.map_err(SessionError::ReadError)?;
        if entry.file_type().map_err(SessionError::ReadError)?.is_dir() {
            directories += 1;
        }
    }
    if directories <= MAX_SESSIONS {
        return Ok(());
    }
    let mut sessions = store.list()?;
    // Cleanup is based on creation date (oldest sessions are removed first),
    // independent of the update-ordered listing.
    sessions.sort_by(|a, b| {
        let a_dt = chrono::DateTime::parse_from_rfc3339(&a.created_at).ok();
        let b_dt = chrono::DateTime::parse_from_rfc3339(&b.created_at).ok();
        match (a_dt, b_dt) {
            (Some(a_dt), Some(b_dt)) => b_dt.cmp(&a_dt),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => b.created_at.cmp(&a.created_at),
        }
    });
    if sessions.len() > MAX_SESSIONS {
        // Calculate how many sessions to delete
        let excess_count = sessions.len() - MAX_SESSIONS;

        // The sessions are sorted by creation date in descending order (newest first)
        // So we need to delete from the end of the vector (oldest sessions)
        for session_meta in sessions
            .iter()
            .rev()
            .filter(|s| Some(s.id.as_str()) != protected)
            .take(excess_count)
        {
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
    use std::collections::HashMap;
    use tempfile::tempdir;

    #[test]
    fn test_new_default() {
        let dir = tempdir().expect("Failed to create temp directory");
        // We use an environment variable to redirect the default store directory for this test
        // This avoids creating .doge/sessions in the actual project directory and prevents race conditions
        unsafe {
            std::env::set_var("DOGE_SESSIONS_DIR", dir.path());
        }

        // Ensure cleanup happens even if test panics (Rust doesn't have try-finally block for var,
        // relying on test isolation or hoping for the best. Since other tests use explicit paths, it's low risk.)
        let result = std::panic::catch_unwind(|| {
            let store =
                SessionStore::new_default().expect("Failed to create default session store");
            assert!(
                store.root.exists(),
                "Session store root directory should exist"
            );
            assert_eq!(store.root, dir.path());
        });

        unsafe {
            std::env::remove_var("DOGE_SESSIONS_DIR");
        }

        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
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
    fn test_list_with_stats_sorted_by_updated_at() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");

        let session1 = store.create().expect("Failed to create session 1");
        std::thread::sleep(std::time::Duration::from_millis(20));
        let _session2 = store.create().expect("Failed to create session 2");

        // Initially session2 is the most recently active (created last).
        let summaries = store
            .list_with_stats()
            .expect("Failed to list sessions with stats");
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].meta.id, _session2.meta.id);
        assert_eq!(summaries[0].messages, 0);
        assert_eq!(summaries[0].token_count, 0);

        // Update session1 so it becomes the most recently active.
        let mut updated = session1.clone();
        updated.add_conversation_entry(HashMap::new());
        updated.increment_token_count(42);
        store.save(&updated).expect("Failed to save session 1");

        let summaries = store
            .list_with_stats()
            .expect("Failed to list sessions with stats");
        assert_eq!(summaries[0].meta.id, session1.meta.id);
        assert_eq!(summaries[0].messages, 1);
        assert_eq!(summaries[0].token_count, 42);
    }

    #[test]
    fn test_get_latest_prefers_most_recently_updated() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");

        let session1 = store.create().expect("Failed to create session 1");
        std::thread::sleep(std::time::Duration::from_millis(20));
        let _session2 = store.create().expect("Failed to create session 2");

        // Touch session1 so its update timestamp is the newest.
        let mut updated = session1.clone();
        updated.add_conversation_entry(HashMap::new());
        store.save(&updated).expect("Failed to save session 1");

        let latest = store.get_latest().expect("Failed to get latest session");
        assert_eq!(
            latest.expect("Should have a latest session").meta.id,
            session1.meta.id,
            "Latest should be the most recently updated session"
        );
    }

    #[test]
    fn test_resolve_id_prefix() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");

        let session = store.create().expect("Failed to create session");

        // Full ID resolves to itself.
        let resolved = store
            .resolve_id_prefix(&session.meta.id)
            .expect("Failed to resolve full ID");
        assert_eq!(resolved, session.meta.id);

        // A unique prefix resolves to the full ID.
        let prefix: String = session.meta.id.chars().take(8).collect();
        let resolved = store
            .resolve_id_prefix(&prefix)
            .expect("Failed to resolve ID prefix");
        assert_eq!(resolved, session.meta.id);

        // Unknown prefix is not found.
        let err = store.resolve_id_prefix("deadbeef-dead-beef-dead-beefdeadbeef");
        assert!(err.is_err(), "Unknown ID should fail to resolve");

        // Empty prefix is invalid.
        assert!(store.resolve_id_prefix("").is_err());
    }

    #[test]
    fn test_resolve_id_prefix_ambiguous() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");

        // Two sessions with a shared 1-char prefix ("0" for UUIDv7) should be
        // ambiguous for that prefix. UUIDv7 starts with a time-based byte, so
        // both IDs very likely share the first hex char; force it by testing
        // with the longest common prefix of the two IDs.
        let s1 = store.create().expect("Failed to create session 1");
        let s2 = store.create().expect("Failed to create session 2");

        let mut common = String::new();
        for (a, b) in s1.meta.id.chars().zip(s2.meta.id.chars()) {
            if a == b {
                common.push(a);
            } else {
                break;
            }
        }
        if common.is_empty() {
            // Extremely unlikely: IDs share no prefix. Skip this test.
            return;
        }
        let err = store.resolve_id_prefix(&common);
        match err {
            Err(SessionError::AmbiguousId(prefix, matches)) => {
                assert_eq!(prefix, common);
                assert_eq!(matches.len(), 2);
            }
            other => panic!("Expected AmbiguousId, got: {:?}", other.map(|_| ())),
        }
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

    #[test]
    fn writable_store_rejects_unsafe_ids_and_mismatched_metadata() {
        let root = tempdir().expect("fixture");
        let store = SessionStore::new(root.path()).expect("store");
        let session = store.create().expect("session");
        for id in ["../outside", "/absolute", "..", ".", "bad\\id", "bad\0id"] {
            assert!(store.load(id).is_err());
            assert!(store.delete(id).is_err());
            let mut bad = session.clone();
            bad.meta.id = id.into();
            assert!(store.save(&bad).is_err());
        }
        let mut wrong = session.clone();
        wrong.meta.id = "../outside".into();
        fs::write(
            store.session_dir(&session.meta.id).join("session.json"),
            serde_json::to_vec(&wrong).expect("encode"),
        )
        .expect("fixture");
        assert!(store.load(&session.meta.id).is_err());
        assert!(store.list().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn writable_store_refuses_links_and_readonly_replacement() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempdir().expect("fixture");
        let outside = tempdir().expect("outside fixture");
        let store = SessionStore::new(root.path()).expect("store");
        let mut session = store.create().expect("session");
        let path = store.session_dir(&session.meta.id).join("session.json");
        let old = fs::read(&path).expect("bytes");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).expect("permissions");
        session.meta.title = "updated".into();
        assert!(store.save(&session).is_err());
        assert_eq!(fs::read(&path).expect("bytes"), old);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("permissions");
        fs::remove_file(&path).expect("fixture");
        let target = outside.path().join("target.json");
        fs::write(&target, &old).expect("fixture");
        symlink(&target, &path).expect("link");
        assert!(store.save(&session).is_err());
        assert!(store.load(&session.meta.id).is_err());
        assert_eq!(fs::read(&target).expect("outside"), old);
        symlink(outside.path(), root.path().join("linked")).expect("dir link");
        assert!(store.load("linked").is_err());
    }

    #[test]
    fn capacity_failure_preserves_previous_checkpoint_and_limits_reads() {
        let root = tempdir().expect("fixture");
        let store = SessionStore::new(root.path()).expect("store");
        let mut session = store.create().expect("session");
        let path = store.session_dir(&session.meta.id).join("session.json");
        let old = fs::read(&path).expect("bytes");
        session.meta.title = "x".repeat(MAX_SESSION_BYTES as usize);
        assert!(store.save(&session).is_err());
        assert_eq!(fs::read(&path).expect("bytes"), old);
        assert_eq!(
            fs::read_dir(path.parent().expect("dir"))
                .expect("dir")
                .count(),
            1
        );
        let file = fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("fixture");
        file.set_len(MAX_SESSION_BYTES + 1)
            .expect("sparse oversized fixture");
        assert!(store.load(&session.meta.id).is_err());
    }

    #[test]
    fn retention_keeps_resumed_session_and_preserves_unknown_inventory() {
        let root = tempdir().expect("fixture");
        let store = SessionStore::new(root.path()).expect("store");
        let mut old = store.create().expect("old");
        old.meta.created_at = "2000-01-01T00:00:00Z".into();
        store.save(&old).expect("old");
        for _ in 0..100 {
            let session = SessionData::new();
            let dir = store.session_dir(&session.meta.id);
            fs::create_dir(&dir).expect("fixture");
            fs::write(
                dir.join("session.json"),
                serde_json::to_vec(&session).expect("encode"),
            )
            .expect("fixture");
        }
        old.meta.title = "resumed".into();
        store.save(&old).expect("protected save");
        assert_eq!(
            store.load(&old.meta.id).expect("old retained").meta.title,
            "resumed"
        );
        assert_eq!(store.list().expect("list").len(), MAX_SESSIONS);
        let corrupt = root.path().join("corrupt");
        fs::create_dir(&corrupt).expect("fixture");
        fs::write(corrupt.join("session.json"), "broken").expect("fixture");
        store
            .save(&old)
            .expect("valid checkpoint succeeds despite retention failure");
        assert_eq!(
            fs::read_to_string(corrupt.join("session.json")).expect("preserved"),
            "broken"
        );
        assert!(store.list().is_err());
    }

    #[test]
    fn atomic_replacement_never_exposes_partial_json_to_reader() {
        use std::sync::{
            Arc, Barrier,
            atomic::{AtomicBool, Ordering},
        };
        let root = tempdir().expect("fixture");
        let store = SessionStore::new(root.path()).expect("store");
        let mut session = store.create().expect("session");
        let path = store.session_dir(&session.meta.id).join("session.json");
        let done = Arc::new(AtomicBool::new(false));
        let barrier = Arc::new(Barrier::new(2));
        let reader_done = done.clone();
        let reader_barrier = barrier.clone();
        let reader = std::thread::spawn(move || {
            reader_barrier.wait();
            let mut reads = 0;
            while !reader_done.load(Ordering::Relaxed) {
                let _: SessionData =
                    serde_json::from_slice(&fs::read(&path).expect("read")).expect("complete JSON");
                reads += 1;
            }
            reads
        });
        barrier.wait();
        for i in 0..10 {
            session.meta.title = format!("{i}:{}", "x".repeat(16_000));
            store.save(&session).expect("save");
        }
        done.store(true, Ordering::Relaxed);
        assert!(reader.join().expect("reader") > 0);
    }
}
