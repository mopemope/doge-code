use crate::session::data::{SessionData, SessionMeta};
use crate::session::error::SessionError;
use std::env;
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

pub struct SessionStore {
    pub(crate) root: PathBuf,
}

impl SessionStore {
    /// 新しいSessionStoreを作成します。プロジェクトディレクトリ内の .doge/sessions にセッションデータを保存します。
    pub fn new_default() -> Result<Self, SessionError> {
        let base = default_store_dir()?;
        fs::create_dir_all(&base).map_err(SessionError::CreateDirError)?;
        Ok(Self { root: base })
    }

    /// 指定されたパスをルートディレクトリとするSessionStoreを作成します。
    #[allow(dead_code)]
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, SessionError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(SessionError::CreateDirError)?;
        Ok(Self { root })
    }

    /// すべてのセッションのメタデータを取得し、作成日時で降順にソートして返します。
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
            let meta_p = p.join("meta.json");
            if let Ok(s) = fs::read_to_string(&meta_p).map_err(SessionError::ReadError)
                && let Ok(meta) =
                    serde_json::from_str::<SessionMeta>(&s).map_err(SessionError::ParseError)
            {
                out.push(meta);
            }
        }
        out.sort_by_key(|m| m.created_at);
        out.reverse();
        Ok(out)
    }

    /// 新しいセッションを作成し、セッションデータを返します。
    pub fn create(&self, title: impl Into<String>) -> Result<SessionData, SessionError> {
        let id = Uuid::new_v4().to_string();
        let dir = self.root.join(&id);
        fs::create_dir_all(&dir).map_err(SessionError::CreateDirError)?;
        let meta = SessionMeta {
            id: id.clone(),
            created_at: chrono::Utc::now().timestamp(),
            title: title.into(),
        };
        let data = SessionData {
            meta: meta.clone(),
            history: Vec::new(),
        };
        fs::write(dir.join("meta.json"), serde_json::to_string_pretty(&meta)?)
            .map_err(SessionError::WriteError)?;
        fs::write(
            dir.join("history.json"),
            serde_json::to_string_pretty(&data.history)?,
        )
        .map_err(SessionError::WriteError)?;
        Ok(data)
    }

    /// セッションIDを指定して、セッションデータを読み込みます。
    pub fn load(&self, id: &str) -> Result<SessionData, SessionError> {
        // Validate ID format if necessary
        if id.is_empty() {
            return Err(SessionError::InvalidId(id.to_string()));
        }
        let dir = self.root.join(id);
        if !dir.exists() {
            return Err(SessionError::NotFound(id.to_string()));
        }
        let meta_s = fs::read_to_string(dir.join("meta.json")).map_err(SessionError::ReadError)?;
        let meta: SessionMeta = serde_json::from_str(&meta_s).map_err(SessionError::ParseError)?;
        let hist_s = fs::read_to_string(dir.join("history.json")).unwrap_or_else(|_| "[]".into());
        let history: Vec<String> = serde_json::from_str(&hist_s).unwrap_or_default();
        Ok(SessionData { meta, history })
    }

    /// セッションデータを保存します。
    pub fn save(&self, data: &SessionData) -> Result<(), SessionError> {
        let dir = self.root.join(&data.meta.id);
        fs::create_dir_all(&dir).map_err(SessionError::CreateDirError)?;
        fs::write(
            dir.join("meta.json"),
            serde_json::to_string_pretty(&data.meta)?,
        )
        .map_err(SessionError::WriteError)?;
        fs::write(
            dir.join("history.json"),
            serde_json::to_string_pretty(&data.history)?,
        )
        .map_err(SessionError::WriteError)?;
        Ok(())
    }

    /// セッションIDを指定して、セッションデータを削除します。
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
}

fn default_store_dir() -> Result<PathBuf, SessionError> {
    // プロジェクトディレクトリ内の .doge/sessions を使用する
    let project_dir = env::current_dir().map_err(SessionError::ReadError)?;
    let base = project_dir.join(".doge/sessions");
    Ok(base)
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

        let session1 = store.create("Session1").expect("Failed to create session1");
        let session2 = store
            .create("Session 2")
            .expect("Failed to create session 2");
        let sessions = store.list().expect("Failed to list sessions");
        assert_eq!(sessions.len(), 2, "Should have 2 sessions");
        // セッションがリストされていることを確認するだけで、順序は確認しない
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

        let title = "Test Session";
        let created_session = store.create(title).expect("Failed to create session");
        let loaded_session = store
            .load(&created_session.meta.id)
            .expect("Failed to load session");
        assert_eq!(
            loaded_session.meta.id, created_session.meta.id,
            "Session IDs should match"
        );
        assert_eq!(
            loaded_session.meta.title, title,
            "Session titles should match"
        );
        assert_eq!(
            loaded_session.history, created_session.history,
            "Session histories should match"
        );
    }

    #[test]
    fn test_save() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");

        let mut session = store
            .create("Test Session")
            .expect("Failed to create session");
        let entry = "Test entry";
        session.add_to_history(entry);
        store.save(&session).expect("Failed to save session");
        let loaded_session = store
            .load(&session.meta.id)
            .expect("Failed to load session");
        assert_eq!(
            loaded_session.history.len(),
            1,
            "History should have one entry"
        );
        assert_eq!(
            loaded_session.history[0], entry,
            "History entry should match"
        );
    }

    #[test]
    fn test_delete() {
        let dir = tempdir().expect("Failed to create temp directory");
        let store = SessionStore::new(dir.path()).expect("Failed to create session store");

        let session = store
            .create("Test Session")
            .expect("Failed to create session");
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
}
