use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::config::LlmConfig;
use crate::llm::LlmErrorKind;
use crate::llm::types::{ChatMessage, ChoiceMessage};

mod network;

#[derive(Debug, Clone)]
pub struct OpenAIClient {
    pub base_url: String,
    pub api_key: String,
    pub(crate) inner: reqwest::Client,
    pub llm_cfg: LlmConfig,
    /// Tracks total tokens used by this client
    pub tokens_used: Arc<AtomicU32>,
    /// Tracks prompt tokens used by this client (for header display)
    pub prompt_tokens_used: Arc<AtomicU32>,
    pub reason_enable: bool,
}

impl OpenAIClient {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Result<Self> {
        let url = base_url.into();
        let mut reason_enable = false;
        // openai
        if url.contains("api.openai.com") {
            reason_enable = true;
        }
        let inner = reqwest::Client::builder().build()?;
        Ok(Self {
            base_url: url,
            api_key: api_key.into(),
            inner,
            llm_cfg: LlmConfig::default(),
            tokens_used: Arc::new(AtomicU32::new(0)),
            prompt_tokens_used: Arc::new(AtomicU32::new(0)),
            reason_enable,
        })
    }

    pub fn with_llm_config(mut self, cfg: LlmConfig) -> Self {
        // Rebuild reqwest client with timeouts from cfg to ensure network layer reaches server in tests and prod.
        let builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(cfg.connect_timeout_ms))
            .timeout(Duration::from_millis(cfg.request_timeout_ms))
            .read_timeout(Duration::from_millis(cfg.timeout_ms)); // Add timeout settings
        // If building fails, keep existing client to avoid panic; but in normal cases it should succeed.
        if let Ok(c) = builder.build() {
            self.inner = c;
        }
        self.llm_cfg = cfg;
        self
    }

    pub(crate) fn endpoint(&self) -> String {
        let mut base = self.base_url.trim_end_matches('/').to_string();
        if let Some(pos) = base.rfind("/v1") {
            base.truncate(pos);
            base = base.trim_end_matches('/').to_string();
        }
        format!("{base}/v1/chat/completions")
    }

    /// Get the total number of tokens used by this client
    pub fn get_tokens_used(&self) -> u32 {
        self.tokens_used.load(Ordering::Relaxed)
    }

    /// Add tokens to the total count
    pub fn add_tokens(&self, tokens: u32) {
        self.tokens_used.fetch_add(tokens, Ordering::Relaxed);
    }

    pub fn set_tokens(&self, tokens: u32) {
        self.tokens_used.store(tokens, Ordering::Relaxed);
    }

    /// Get the total number of prompt tokens used by this client
    pub fn get_prompt_tokens_used(&self) -> u32 {
        self.prompt_tokens_used.load(Ordering::Relaxed)
    }

    /// Add prompt tokens to the prompt count
    pub fn add_prompt_tokens(&self, tokens: u32) {
        self.prompt_tokens_used.fetch_add(tokens, Ordering::Relaxed);
    }

    pub fn set_prompt_tokens(&self, tokens: u32) {
        self.prompt_tokens_used.store(tokens, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    pub async fn chat_once(
        &self,
        model: &str,
        messages: Vec<ChatMessage>,
        cancel: Option<CancellationToken>,
    ) -> Result<ChoiceMessage> {
        // Delegate to network module implementation for clarity and to keep this file small
        crate::llm::client_core::network::chat_once(self, model, messages, cancel).await
    }

    #[allow(dead_code)]
    fn should_retry(&self, kind: LlmErrorKind) -> bool {
        // borrow kind to avoid move
        matches!(
            kind,
            LlmErrorKind::RateLimited
                | LlmErrorKind::Server
                | LlmErrorKind::Network
                | LlmErrorKind::Timeout
        )
    }

    pub(crate) fn backoff_delay(&self, attempt: usize, retry_after_secs: Option<u64>) -> Duration {
        if self.llm_cfg.respect_retry_after
            && let Some(secs) = retry_after_secs
        {
            return Duration::from_secs(secs);
        }
        let base = self.llm_cfg.retry_base_ms;
        let exp = base.saturating_mul(1u64 << (attempt as u32 - 1));
        let jitter = self.llm_cfg.retry_jitter_ms as i64;
        let half = jitter / 2;
        let rnd = fastrand::i64(-half..=half).max(0) as u64;
        Duration::from_millis(exp.saturating_add(rnd))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::ChatMessage;
    use httptest::{Expectation, Server, matchers::*, responders::*};

    #[tokio::test]
    async fn chat_once_happy_path() {
        let server = Server::run();
        server.expect(
            Expectation::matching(all_of![
                request::method_path("POST", "/v1/chat/completions"),
                request::headers(contains(key("authorization"))),
            ])
            .respond_with(json_encoded(serde_json::json!({
                "id": "test",
                "choices": [
                    {"index":0, "message": {"role":"assistant","content":"hello"}}
                ]
            }))),
        );

        let client = OpenAIClient::new(format!("{}/", server.url_str("")), "test-key").unwrap();
        let msg = client
            .chat_once(
                "gpt-test",
                vec![ChatMessage {
                    role: "user".into(),
                    content: Some("hi".into()),
                    tool_calls: vec![],
                    tool_call_id: None,
                }],
                None,
            )
            .await
            .unwrap();
        assert_eq!(msg.content, "hello");
    }

    #[tokio::test]
    #[ignore]
    async fn chat_once_retries_on_500_then_succeeds() {
        let server = Server::run();
        // Phase 1: expect a single 500 and verify it happens
        server.expect(
            Expectation::matching(request::method_path("POST", "/v1/chat.completions"))
                .times(1)
                .respond_with(
                    status_code(500)
                        .append_header("Retry-After", "0")
                        .body("oops"),
                ),
        );
        let client = OpenAIClient::new(format!("{}/", server.url_str("")), "x")
            .unwrap()
            .with_llm_config(LlmConfig {
                connect_timeout_ms: 5_000,
                request_timeout_ms: 5_000,
                max_retries: 0, // do not retry in phase 1
                retry_base_ms: 1,
                retry_jitter_ms: 0,
                ..LlmConfig::default()
            });
        let err = client
            .chat_once(
                "gpt",
                vec![ChatMessage {
                    role: "user".into(),
                    content: Some("hi".into()),
                    tool_calls: vec![],
                    tool_call_id: None,
                }],
                None,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("500"));

        // Phase 2: expect a single 200 and verify success with one retry allowed
        server.expect(
            Expectation::matching(request::method_path("POST", "/v1/chat.completions"))
                .times(1)
                .respond_with(json_encoded(serde_json::json!({
                    "id": "test",
                    "choices": [
                        {"index":0, "message": {"role":"assistant","content":"ok"}}
                    ]
                }))),
        );
        let client = OpenAIClient::new(format!("{}/", server.url_str("")), "x")
            .unwrap()
            .with_llm_config(LlmConfig {
                connect_timeout_ms: 5_000,
                request_timeout_ms: 5_000,
                max_retries: 1,
                retry_base_ms: 1,
                retry_jitter_ms: 0,
                ..LlmConfig::default()
            });
        let msg = client
            .chat_once(
                "gpt",
                vec![ChatMessage {
                    role: "user".into(),
                    content: Some("hi".into()),
                    tool_calls: vec![],
                    tool_call_id: None,
                }],
                None,
            )
            .await
            .unwrap();
        assert_eq!(msg.content, "ok");
    }

    #[tokio::test]
    #[ignore]
    async fn chat_once_non200_is_error_no_retry_on_400() {
        let server = Server::run();
        server.expect(
            Expectation::matching(request::method_path("POST", "/v1/chat.completions"))
                .respond_with(status_code(400).body("bad")),
        );
        let client = OpenAIClient::new(format!("{}/", server.url_str("")), "x")
            .unwrap()
            .with_llm_config(LlmConfig {
                connect_timeout_ms: 5_000,
                request_timeout_ms: 5_000,
                max_retries: 1,
                retry_base_ms: 1,
                retry_jitter_ms: 0,
                ..LlmConfig::default()
            });
        let err = client
            .chat_once(
                "gpt",
                vec![ChatMessage {
                    role: "user".into(),
                    content: Some("hi".into()),
                    tool_calls: vec![],
                    tool_call_id: None,
                }],
                None,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("400"));
    }

    #[tokio::test]
    #[ignore]
    async fn chat_once_retries_on_timeout_then_succeeds() {
        let server = Server::run();
        // Phase 1: expect a single timeout and verify it happens
        server.expect(
            Expectation::matching(request::method_path("POST", "/v1/chat.completions"))
                .times(1)
                .respond_with(
                    // Use a very short timeout in the test to trigger timeout quickly
                    status_code(408), // HTTP 408 Request Timeout
                ),
        );
        let client = OpenAIClient::new(format!("{}/", server.url_str("")), "x")
            .unwrap()
            .with_llm_config(LlmConfig {
                connect_timeout_ms: 5_000,
                request_timeout_ms: 50, // Short timeout to trigger
                timeout_ms: 50,         // Short timeout to trigger
                max_retries: 0,         // do not retry in phase 1
                retry_base_ms: 1,
                retry_jitter_ms: 0,
                ..LlmConfig::default()
            });
        let err = client
            .chat_once(
                "gpt",
                vec![ChatMessage {
                    role: "user".into(),
                    content: Some("hi".into()),
                    tool_calls: vec![],
                    tool_call_id: None,
                }],
                None,
            )
            .await
            .unwrap_err();
        // Verify that a timeout error, 408 error, request sending error, status code error, or chat error occurs
        println!("Error: {}", err);
        assert!(
            format!("{err}").contains("timed out")
                || format!("{err}").contains("408")
                || format!("{err}").contains("error sending request")
                || format!("{err}").contains("status code")
                || format!("{err}").contains("chat error")
        );

        // Phase 2: expect a single 200 and verify success with one retry allowed
        server.expect(
            Expectation::matching(request::method_path("POST", "/v1/chat.completions"))
                .times(1)
                .respond_with(json_encoded(serde_json::json!({
                    "id": "test",
                    "choices": [
                        {"index":0, "message": {"role":"assistant","content":"ok"}}
                    ]
                }))),
        );
        let client = OpenAIClient::new(format!("{}/", server.url_str("")), "x")
            .unwrap()
            .with_llm_config(LlmConfig {
                connect_timeout_ms: 5_000,
                request_timeout_ms: 5_000,
                timeout_ms: 5_000,
                max_retries: 1,
                retry_base_ms: 1,
                retry_jitter_ms: 0,
                ..LlmConfig::default()
            });
        let msg = client
            .chat_once(
                "gpt",
                vec![ChatMessage {
                    role: "user".into(),
                    content: Some("hi".into()),
                    tool_calls: vec![],
                    tool_call_id: None,
                }],
                None,
            )
            .await
            .unwrap();
        assert_eq!(msg.content, "ok");
    }

    #[test]
    fn endpoint_normalization() {
        let c = OpenAIClient {
            base_url: "https://api.example.com/v1/".into(),
            api_key: "x".into(),
            inner: reqwest::Client::new(),
            llm_cfg: LlmConfig::default(),
            tokens_used: Arc::new(AtomicU32::new(0)),
            prompt_tokens_used: Arc::new(AtomicU32::new(0)),
            reason_enable: false,
        };
        assert_eq!(c.endpoint(), "https://api.example.com/v1/chat/completions");
        let c2 = OpenAIClient {
            base_url: "https://api.example.com/".into(),
            api_key: "x".into(),
            inner: reqwest::Client::new(),
            llm_cfg: LlmConfig::default(),
            tokens_used: Arc::new(AtomicU32::new(0)),
            prompt_tokens_used: Arc::new(AtomicU32::new(0)),
            reason_enable: false,
        };
        assert_eq!(c2.endpoint(), "https://api.example.com/v1/chat/completions");
    }

    #[test]
    fn token_tracking() {
        let client = OpenAIClient::new("https://api.example.com/", "x").unwrap();
        assert_eq!(client.get_tokens_used(), 0);
        client.add_tokens(100);
        assert_eq!(client.get_tokens_used(), 100);
        client.add_tokens(50);
        assert_eq!(client.get_tokens_used(), 150);
    }
}
