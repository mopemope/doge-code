use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub created_at: i64,
    pub title: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionData {
    pub meta: SessionMeta,
    pub history: Vec<String>,
}

impl SessionData {
    /// 新しいSessionDataを作成します。
    pub fn new(title: impl Into<String>) -> Self {
        let id = Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().timestamp();
        let meta = SessionMeta {
            id,
            created_at,
            title: title.into(),
        };
        Self {
            meta,
            history: Vec::new(),
        }
    }

    /// 会話履歴に新しいエントリを追加します。
    pub fn add_to_history(&mut self, entry: impl Into<String>) {
        self.history.push(entry.into());
    }

    /// 会話履歴をクリアします。
    pub fn clear_history(&mut self) {
        self.history.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new() {
        let title = "Test Session";
        let session_data = SessionData::new(title);
        assert!(
            !session_data.meta.id.is_empty(),
            "Session ID should not be empty"
        );
        assert!(session_data.meta.created_at > 0, "Created at should be set");
        assert_eq!(session_data.meta.title, title, "Title should match");
        assert!(
            session_data.history.is_empty(),
            "History should be empty initially"
        );
    }

    #[test]
    fn test_add_to_history() {
        let mut session_data = SessionData::new("Test Session");
        let entry = "Test entry";
        session_data.add_to_history(entry);
        assert_eq!(
            session_data.history.len(),
            1,
            "History should have one entry"
        );
        assert_eq!(session_data.history[0], entry, "History entry should match");
    }

    #[test]
    fn test_clear_history() {
        let mut session_data = SessionData::new("Test Session");
        session_data.add_to_history("Entry 1");
        session_data.add_to_history("Entry 2");
        assert_eq!(
            session_data.history.len(),
            2,
            "History should have two entries"
        );
        session_data.clear_history();
        assert!(
            session_data.history.is_empty(),
            "History should be empty after clearing"
        );
    }
}
