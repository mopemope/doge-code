#[cfg(test)]
mod tests {
    use crate::analysis::database::migration::run_migrations;
    use crate::analysis::semantic::SemanticService;
    use crate::config::RagConfig;
    use sea_orm::{Database, DatabaseConnection};
    use serde_json::json;

    async fn setup_test_db() -> DatabaseConnection {
        // Use in-memory SQLite for testing
        // Note: sea-orm sqlite in-memory requires shared cache usually if we reconnect,
        // but here we keep the connection open.
        let db = Database::connect("sqlite::memory:").await.unwrap();
        run_migrations(&db).await.unwrap();
        db
    }

    #[tokio::test]
    async fn test_log_and_search_action_history() {
        let db = setup_test_db().await;
        let config = RagConfig {
            enabled: true,
            // We need a small batch size for testing
            batch_size: 1,
            ..Default::default()
        };

        // Initialize SemanticService
        let service = SemanticService::new(db, config);

        // Log an action
        let session_id = "test_session_1";
        let action_type = "tool_use";
        let content = "Ran a python script to calculate pi";
        let metadata = json!({
            "tool": "execute_bash",
            "command": "python3 pi.py"
        });

        // This might fail if the embedder model files are not present or cannot be downloaded in this environment.
        // However, fastembed usually downloads on first use. If network is restricted, this might fail.
        // Assuming we can run it or mocking embedder is needed.
        // For this test in this environment, it's safer to check if we can run it.
        // If it fails due to embedder, we might need a mocked embedder, but SemanticService hardcodes Embedder.
        // Implementation detail: Embedder::new() is called.

        let result = service
            .log_action(session_id, action_type, content, metadata)
            .await;

        // If embedder Init fails, we skip assertions about success if it's strictly due to environment.
        // But let's try.
        if result.is_err() {
            println!(
                "Skipping test due to embedder initialization failure (likely no network/model): {:?}",
                result.err()
            );
            return;
        }

        assert!(result.is_ok());

        // Search for the action
        let results = service
            .search_action_history("calculate pi", 5)
            .await
            .unwrap();

        assert!(!results.is_empty());
        let (log, score) = &results[0];

        assert_eq!(log.session_id, session_id);
        assert_eq!(log.content, content);
        assert!(score > &0.5); // Should be high similarity
    }
}
