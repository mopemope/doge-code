use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionData {
    pub meta: SessionMeta,
    /// Last updated timestamp
    pub timestamp: i64,
    /// Conversation payload data sent to LLM
    pub conversation: Vec<HashMap<String, serde_json::Value>>,
    /// Number of tokens consumed
    pub token_count: u64,
    /// Number of requests sent to LLM
    pub requests: u64,
    /// Number of tool calls made
    pub tool_calls: u64,
}

impl SessionData {
    /// Create a new SessionData.
    pub fn new() -> Self {
        let id = Uuid::now_v7().to_string(); // Use UUIDv7
        let created_at = Utc::now().timestamp();
        let meta = SessionMeta { id, created_at };
        Self {
            meta,
            timestamp: created_at,
            conversation: Vec::new(),
            token_count: 0,
            requests: 0,
            tool_calls: 0,
        }
    }

    /// Add a new entry to the conversation.
    pub fn add_conversation_entry(&mut self, entry: HashMap<String, serde_json::Value>) {
        self.conversation.push(entry);
        self.timestamp = Utc::now().timestamp(); // Update timestamp
    }

    /// Clear the conversation.
    pub fn clear_conversation(&mut self) {
        self.conversation.clear();
        self.timestamp = Utc::now().timestamp(); // Update timestamp
    }

    /// Increment token count.
    pub fn increment_token_count(&mut self, count: u64) {
        self.token_count += count;
        self.timestamp = Utc::now().timestamp(); // Update timestamp
    }

    /// Increment requests count.
    pub fn increment_requests(&mut self) {
        self.requests += 1;
        self.timestamp = Utc::now().timestamp(); // Update timestamp
    }

    /// Increment tool calls count.
    pub fn increment_tool_calls(&mut self) {
        self.tool_calls += 1;
        self.timestamp = Utc::now().timestamp(); // Update timestamp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_new() {
        let session_data = SessionData::new();
        assert!(
            !session_data.meta.id.is_empty(),
            "Session ID should not be empty"
        );
        assert!(session_data.meta.created_at > 0, "Created at should be set");
        assert!(
            session_data.conversation.is_empty(),
            "Conversation should be empty initially"
        );
        assert!(session_data.timestamp > 0, "Timestamp should be set");
        assert_eq!(session_data.token_count, 0, "Token count should be 0");
        assert_eq!(session_data.requests, 0, "Requests count should be 0");
        assert_eq!(session_data.tool_calls, 0, "Tool calls count should be 0");
    }

    #[test]
    fn test_add_conversation_entry() {
        let mut session_data = SessionData::new();
        let mut entry = HashMap::new();
        entry.insert(
            "role".to_string(),
            serde_json::Value::String("user".to_string()),
        );
        entry.insert(
            "content".to_string(),
            serde_json::Value::String("Test message".to_string()),
        );
        session_data.add_conversation_entry(entry.clone());
        assert_eq!(
            session_data.conversation.len(),
            1,
            "Conversation should have one entry"
        );
        assert_eq!(
            session_data.conversation[0], entry,
            "Conversation entry should match"
        );
    }

    #[test]
    fn test_clear_conversation() {
        let mut session_data = SessionData::new();
        let mut entry1 = HashMap::new();
        entry1.insert(
            "role".to_string(),
            serde_json::Value::String("user".to_string()),
        );
        entry1.insert(
            "content".to_string(),
            serde_json::Value::String("Message 1".to_string()),
        );
        let mut entry2 = HashMap::new();
        entry2.insert(
            "role".to_string(),
            serde_json::Value::String("assistant".to_string()),
        );
        entry2.insert(
            "content".to_string(),
            serde_json::Value::String("Message 2".to_string()),
        );
        session_data.add_conversation_entry(entry1);
        session_data.add_conversation_entry(entry2);
        assert_eq!(
            session_data.conversation.len(),
            2,
            "Conversation should have two entries"
        );
        session_data.clear_conversation();
        assert!(
            session_data.conversation.is_empty(),
            "Conversation should be empty after clearing"
        );
    }

    #[test]
    fn test_increment_token_count() {
        let mut session_data = SessionData::new();
        session_data.increment_token_count(10);
        assert_eq!(session_data.token_count, 10, "Token count should be 10");
    }

    #[test]
    fn test_increment_requests() {
        let mut session_data = SessionData::new();
        session_data.increment_requests();
        assert_eq!(session_data.requests, 1, "Requests count should be 1");
    }

    #[test]
    fn test_increment_tool_calls() {
        let mut session_data = SessionData::new();
        session_data.increment_tool_calls();
        assert_eq!(session_data.tool_calls, 1, "Tool calls count should be 1");
    }
}
