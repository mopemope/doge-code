#[cfg(test)]
mod tests {
    use crate::config::AppConfig;
    use crate::tui::commands::TuiExecutor;
    use crate::tui::commands_sessions::SessionManager;
    use crate::session::data::SessionData;
    use crate::session::store::SessionStore;
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;
    use std::collections::HashMap;
    use std::path::PathBuf;

    // Helper function to create a minimal AppConfig for testing
    fn create_test_config(project_root: PathBuf, resume: bool) -> AppConfig {
        AppConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            api_key: None,
            project_root,
            git_root: None,
            llm: crate::config::LlmConfig::default(),
            enable_stream_tools: false,
            theme: "dark".to_string(),
            project_instructions_file: None,
            no_repomap: false,
            resume,
            auto_compact_prompt_token_threshold: 250_000,
        }
    }

    #[test]
    fn test_resume_latest_session() {
        // 1. Create a temporary directory for the test
        let temp_dir = tempdir().expect("Failed to create temp directory");
        let temp_path = temp_dir.path().to_path_buf();

        // 2. Create a mock AppConfig with resume=true
        let cfg = create_test_config(temp_path.clone(), true);

        // 3. Create a SessionStore using the temp directory
        let store = SessionStore::new(temp_path.join(".doge/sessions")).expect("Failed to create session store");

        // 4. Create a session and add some conversation data
        let mut session_data = SessionData::new();
        let mut entry = HashMap::new();
        entry.insert("role".to_string(), serde_json::Value::String("user".to_string()));
        entry.insert("content".to_string(), serde_json::Value::String("Test message for resume test".to_string()));
        session_data.add_conversation_entry(entry);

        // 5. Save the session
        store.save(&session_data).expect("Failed to save session");

        // 6. Create a SessionManager with the store
        let mut session_manager = SessionManager {
            store,
            current_session: None,
        };

        // 7. Simulate the resume logic from main.rs/run_tui
        if cfg.resume {
            if let Err(e) = session_manager.load_latest_session() {
                eprintln!("Failed to load latest session: {}", e);
                // In actual code, this might panic or handle the error differently
                // For test, we assert it doesn't error
                panic!("load_latest_session failed: {}", e);
            }
        }

        // 8. Verify that the session was loaded
        assert!(session_manager.current_session.is_some(), "Current session should be loaded");
        let loaded_session = session_manager.current_session.unwrap();
        assert_eq!(loaded_session.meta.id, session_data.meta.id, "Session IDs should match");
        assert_eq!(loaded_session.conversation.len(), 1, "Loaded session should have one conversation entry");
        assert_eq!(
            loaded_session.conversation[0].get("content"),
            Some(&serde_json::Value::String("Test message for resume test".to_string())),
            "Loaded session content should match"
        );
    }
}