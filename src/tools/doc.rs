use crate::analysis::RepoMap;
use crate::features::doc_skill::generator::DocGenerator;
use crate::llm::OpenAIClient;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::Result;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "doc_generate".to_string(),
            strict: None,
            description: "Generates documentation for a specific symbol or file using LLM and RepoMap context.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Absolute path to the file"},
                    "symbol": {"type": "string", "description": "Name of the symbol to document (optional, if omitted documents the whole file)"}
                },
                "required": ["path"]
            }),
        },
    }
}

/// Generate documentation using the agent's shared client.
///
/// Never constructs a new client internally: the caller passes the shared
/// `OpenAIClient` (same ledger as the main loop) so nested usage is
/// attributed to the run budget and session checkpoints.
pub async fn doc_generate(
    path: &str,
    symbol: Option<&str>,
    client: OpenAIClient,
    model: &str,
    project_root: &Path,
    repomap: Arc<RwLock<Option<RepoMap>>>,
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> Result<String> {
    let client_arc = Arc::new(client);

    let generator = DocGenerator::new(
        repomap,
        client_arc,
        model.to_string(),
        project_root.to_path_buf(),
    );
    let path_obj = std::path::Path::new(path);

    if let Some(sym) = symbol {
        generator
            .generate_doc_for_symbol_with_cancel(path_obj, sym, cancel)
            .await
    } else {
        generator
            .generate_doc_for_file_with_cancel(path_obj, cancel)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LlmConfig;
    use httptest::{Expectation, matchers::*, responders::*};

    #[tokio::test]
    async fn doc_generate_uses_shared_client_not_config_base_url() {
        // AppConfig base URL is intentionally invalid; the passed shared
        // client points at the mock server. Success proves no internal
        // `OpenAIClient::from_config()` regeneration.
        let server = crate::test_support::HTTP_SERVER_POOL.get_server();
        server.expect(
            Expectation::matching(request::method_path("POST", "/v1/chat/completions"))
                .times(1)
                .respond_with(json_encoded(serde_json::json!({
                    "id": "doc-1",
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": "//! Shared docs"}}],
                    "usage": {"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120}
                }))),
        );
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("lib.rs");
        tokio::fs::write(&file_path, "pub fn foo() {}\n")
            .await
            .unwrap();
        let repomap = Arc::new(RwLock::new(None));
        let client = OpenAIClient::new(format!("{}/", server.url_str("")), "test-key")
            .unwrap()
            .with_llm_config(LlmConfig::default());
        let before = client.usage_snapshot();
        let doc = doc_generate(
            file_path.to_str().unwrap(),
            None,
            client.clone(),
            "test-model",
            temp_dir.path(),
            repomap,
            None,
        )
        .await
        .expect("shared-client doc_generate");
        assert_eq!(doc, "//! Shared docs");
        let delta = client.usage_snapshot().difference(&before);
        assert_eq!(delta.attempts, 1);
        assert_eq!(delta.usage_records, 1);
        assert_eq!(delta.total_tokens, 120);
    }

    #[tokio::test]
    async fn doc_generate_cancellation_propagates_as_cancelled() {
        use axum::routing::post;
        use std::sync::Arc as StdArc;
        use tokio::sync::Notify;
        let release = StdArc::new(Notify::new());
        let release_clone = release.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            post(move |axum::Json(_body): axum::Json<serde_json::Value>| async move {
                release_clone.notified().await;
                (
                    axum::http::StatusCode::OK,
                    axum::Json(serde_json::json!({
                        "id": "blocked",
                        "choices": [{"index": 0, "message": {"role": "assistant", "content": "late"}}]
                    })),
                )
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("lib.rs");
        tokio::fs::write(&file_path, "pub fn foo() {}\n")
            .await
            .unwrap();
        let client = OpenAIClient::new(&url, "test-key")
            .unwrap()
            .with_llm_config(LlmConfig::default());
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            cancel_clone.cancel();
        });
        let err = doc_generate(
            file_path.to_str().unwrap(),
            None,
            client,
            "test-model",
            temp_dir.path(),
            Arc::new(RwLock::new(None)),
            Some(cancel),
        )
        .await
        .expect_err("blocked doc_generate must cancel");
        assert!(
            err.downcast_ref::<crate::llm::LlmErrorKind>()
                .is_some_and(|k| *k == crate::llm::LlmErrorKind::Cancelled)
        );
        release.notify_one();
        task.abort();
    }
}
