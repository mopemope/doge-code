use chrono::{TimeZone, Utc};
use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

fn serialize_rfc3339<S>(val: &str, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(val)
}

fn deserialize_rfc3339<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    struct RfcVisitor;

    impl<'de> Visitor<'de> for RfcVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(
                formatter,
                "an RFC3339 timestamp string or integer seconds since epoch"
            )
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            let dt = Utc
                .timestamp_opt(value, 0)
                .single()
                .ok_or_else(|| E::custom("invalid timestamp"))?;
            Ok(dt.to_rfc3339())
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            let secs = value as i64;
            let dt = Utc
                .timestamp_opt(secs, 0)
                .single()
                .ok_or_else(|| E::custom("invalid timestamp"))?;
            Ok(dt.to_rfc3339())
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            // Accept RFC3339 strings as-is.
            Ok(value.to_string())
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            self.visit_str(&value)
        }
    }

    deserializer.deserialize_any(RfcVisitor)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionMeta {
    pub id: String,
    #[serde(
        serialize_with = "serialize_rfc3339",
        deserialize_with = "deserialize_rfc3339"
    )]
    pub created_at: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub title_is_default: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionData {
    pub meta: SessionMeta,
    /// Last updated timestamp (RFC3339 string)
    #[serde(
        serialize_with = "serialize_rfc3339",
        deserialize_with = "deserialize_rfc3339"
    )]
    pub timestamp: String,
    /// Conversation payload data sent to LLM
    pub conversation: Vec<HashMap<String, serde_json::Value>>,
    /// Number of tokens consumed
    pub token_count: u64,
    /// Number of requests sent to LLM
    pub requests: u64,
    /// Number of tool calls made
    pub tool_calls: u64,
    /// Number of lines edited
    pub lines_edited: u64,
    /// Tool call success counts by tool name
    pub tool_call_successes: HashMap<String, u64>,
    /// Tool call failure counts by tool name
    pub tool_call_failures: HashMap<String, u64>,
    /// Changed files during the session for repomap update
    pub changed_files: Vec<String>,
}

impl SessionData {
    /// Create a new SessionData.
    pub fn new() -> Self {
        let id = Uuid::now_v7().to_string(); // Use UUIDv7
        let now = Utc::now().to_rfc3339();
        let meta = SessionMeta {
            id,
            created_at: now.clone(),
            title: "New Session".to_string(), // Initialize title to a sensible default
            title_is_default: true,
        };
        Self {
            meta,
            timestamp: now,
            conversation: Vec::new(),
            token_count: 0,
            requests: 0,
            tool_calls: 0,
            lines_edited: 0,
            tool_call_successes: HashMap::new(),
            tool_call_failures: HashMap::new(),
            changed_files: Vec::new(),
        }
    }

    /// Add a new entry to the conversation.
    pub fn add_conversation_entry(&mut self, entry: HashMap<String, serde_json::Value>) {
        self.conversation.push(entry);
        self.timestamp = Utc::now().to_rfc3339(); // Update timestamp
    }

    /// Clear the conversation.
    pub fn clear_conversation(&mut self) {
        self.conversation.clear();
        self.timestamp = Utc::now().to_rfc3339(); // Update timestamp
    }

    /// Increment token count.
    pub fn increment_token_count(&mut self, count: u64) {
        self.token_count += count;
        self.timestamp = Utc::now().to_rfc3339(); // Update timestamp
    }

    /// Increment requests count.
    pub fn increment_requests(&mut self) {
        self.requests += 1;
        self.timestamp = Utc::now().to_rfc3339(); // Update timestamp
    }

    /// Increment tool calls count.
    pub fn increment_tool_calls(&mut self) {
        self.tool_calls += 1;
        self.timestamp = Utc::now().to_rfc3339(); // Update timestamp
    }

    /// Increment lines edited count.
    pub fn increment_lines_edited(&mut self, count: u64) {
        self.lines_edited += count;
        self.timestamp = Utc::now().to_rfc3339(); // Update timestamp
    }

    /// Record a successful tool call.
    pub fn record_tool_call_success(&mut self, tool_name: &str) {
        let count = self
            .tool_call_successes
            .entry(tool_name.to_string())
            .or_insert(0);
        *count += 1;
        self.timestamp = Utc::now().to_rfc3339(); // Update timestamp
    }

    /// Record a failed tool call.
    pub fn record_tool_call_failure(&mut self, tool_name: &str) {
        let count = self
            .tool_call_failures
            .entry(tool_name.to_string())
            .or_insert(0);
        *count += 1;
        self.timestamp = Utc::now().to_rfc3339(); // Update timestamp
    }

    /// Set the initial prompt for the session and update the title.
    pub fn set_initial_prompt(&mut self, prompt: &str) {
        // Take the first 30 characters of the prompt as the title
        self.meta.title = prompt.chars().take(30).collect();
        self.timestamp = Utc::now().to_rfc3339(); // Update timestamp
    }

    /// Add a changed file to the session.
    pub fn add_changed_file(&mut self, path: std::path::PathBuf) {
        let path_str = path.to_string_lossy().to_string();
        if !self.changed_files.contains(&path_str) {
            self.changed_files.push(path_str);
        }
        self.timestamp = Utc::now().to_rfc3339(); // Update timestamp
    }

    /// Check if there are any changed files in the session.
    pub fn has_changed_files(&self) -> bool {
        !self.changed_files.is_empty()
    }

    /// Clear the changed files list.
    pub fn clear_changed_files(&mut self) {
        self.changed_files.clear();
        self.timestamp = Utc::now().to_rfc3339(); // Update timestamp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn test_new() {
        let session_data = SessionData::new();
        assert!(
            !session_data.meta.id.is_empty(),
            "Session ID should not be empty"
        );
        assert!(
            !session_data.meta.created_at.is_empty(),
            "Created at should be set"
        );
        // Ensure created_at is RFC3339 parseable
        DateTime::parse_from_rfc3339(&session_data.meta.created_at)
            .expect("Created at should be RFC3339 formatted");
        assert!(
            session_data.conversation.is_empty(),
            "Conversation should be empty initially"
        );
        assert!(
            session_data.changed_files.is_empty(),
            "Changed files should be empty initially"
        );
        assert!(
            !session_data.timestamp.is_empty(),
            "Timestamp should be set"
        );
        DateTime::parse_from_rfc3339(&session_data.timestamp)
            .expect("Timestamp should be RFC3339 formatted");
        assert_eq!(session_data.token_count, 0, "Token count should be 0");
        assert_eq!(session_data.requests, 0, "Requests count should be 0");
        assert_eq!(session_data.tool_calls, 0, "Tool calls count should be 0");
        assert!(
            session_data.tool_call_successes.is_empty(),
            "Tool call successes should be empty"
        );
        assert!(
            session_data.tool_call_failures.is_empty(),
            "Tool call failures should be empty"
        );
    }

    #[test]
    fn test_add_changed_file() {
        let mut session_data = SessionData::new();
        let path = PathBuf::from("/path/to/file.rs");
        session_data.add_changed_file(path.clone());

        assert_eq!(session_data.changed_files.len(), 1);
        assert_eq!(session_data.changed_files[0], "/path/to/file.rs");

        // Add the same path again to test deduplication
        session_data.add_changed_file(path.clone());
        assert_eq!(session_data.changed_files.len(), 1);
    }

    #[test]
    fn test_has_changed_files() {
        let mut session_data = SessionData::new();
        assert!(!session_data.has_changed_files());

        let path = PathBuf::from("/path/to/file.rs");
        session_data.add_changed_file(path);
        assert!(session_data.has_changed_files());
    }

    #[test]
    fn test_clear_changed_files() {
        let mut session_data = SessionData::new();
        let path1 = PathBuf::from("/path/to/file1.rs");
        let path2 = PathBuf::from("/path/to/file2.rs");

        session_data.add_changed_file(path1);
        session_data.add_changed_file(path2);
        assert_eq!(session_data.changed_files.len(), 2);
        assert!(session_data.has_changed_files());

        session_data.clear_changed_files();
        assert_eq!(session_data.changed_files.len(), 0);
        assert!(!session_data.has_changed_files());
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

    #[test]
    fn test_record_tool_call_success() {
        let mut session_data = SessionData::new();
        session_data.record_tool_call_success("fs_read");
        session_data.record_tool_call_success("fs_read");
        session_data.record_tool_call_success("fs_write");

        assert_eq!(*session_data.tool_call_successes.get("fs_read").unwrap(), 2);
        assert_eq!(
            *session_data.tool_call_successes.get("fs_write").unwrap(),
            1
        );
        assert!(session_data.tool_call_failures.is_empty());
    }

    #[test]
    fn test_record_tool_call_failure() {
        let mut session_data = SessionData::new();
        session_data.record_tool_call_failure("fs_read");
        session_data.record_tool_call_failure("fs_write");
        session_data.record_tool_call_failure("fs_write");

        assert_eq!(*session_data.tool_call_failures.get("fs_read").unwrap(), 1);
        assert_eq!(*session_data.tool_call_failures.get("fs_write").unwrap(), 2);
        assert!(session_data.tool_call_successes.is_empty());
    }
}
