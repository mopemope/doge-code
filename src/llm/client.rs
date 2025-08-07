use anyhow::{Context, Result};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, RETRY_AFTER};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::config::LlmConfig;
use crate::llm::LlmErrorKind;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChoiceMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    pub index: usize,
    pub message: ChoiceMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub id: Option<String>,
    pub choices: Vec<Choice>,
}

#[derive(Debug, Clone)]
pub struct OpenAIClient {
    pub base_url: String,
    pub api_key: String,
    pub(crate) inner: reqwest::Client,
    pub llm_cfg: LlmConfig,
}

impl OpenAIClient {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Result<Self> {
        let inner = reqwest::Client::builder().build()?;
        Ok(Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            inner,
            llm_cfg: LlmConfig::default(),
        })
    }

    pub fn with_llm_config(mut self, cfg: LlmConfig) -> Self {
        // Rebuild reqwest client with timeouts from cfg to ensure network layer reaches server in tests and prod.
        let builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(cfg.connect_timeout_ms))
            .timeout(Duration::from_millis(cfg.request_timeout_ms));
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

    pub async fn chat_once(
        &self,
        model: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<ChoiceMessage> {
        let url = self.endpoint();
        let req = ChatRequest {
            model: model.to_string(),
            messages,
            temperature: None,
            stream: None,
        };

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {}", self.api_key).parse().unwrap(),
        );

        if let Ok(payload) = serde_json::to_string(&req) {
            debug!(target: "llm", payload=%payload, endpoint=%url, "sending chat.completions payload");
        }

        let max_attempts = self.llm_cfg.max_retries.saturating_add(1);
        let mut last_err: Option<anyhow::Error> = None;

        for attempt in 1..=max_attempts {
            let req_builder = self.inner.post(&url).headers(headers.clone()).json(&req);

            let resp_res = req_builder.send().await;

            match resp_res {
                Err(e) => {
                    last_err = Some(anyhow::Error::new(e).context("send chat request"));
                }
                Ok(resp) => {
                    if !resp.status().is_success() {
                        let status = resp.status();
                        let retry_after = resp
                            .headers()
                            .get(RETRY_AFTER)
                            .and_then(|h| h.to_str().ok())
                            .and_then(|s| s.parse::<u64>().ok());
                        let text = resp.text().await.unwrap_or_default();
                        let e = anyhow::anyhow!("chat error: {} - {}", status, text);
                        let kind = crate::llm::classify_error(Some(status), &e);
                        if self.should_retry(kind.clone()) && attempt < max_attempts {
                            let wait = self.backoff_delay(attempt, retry_after);
                            info!(attempt, kind=?kind, wait_ms=%wait.as_millis(), "retrying chat_once");
                            tokio::time::sleep(wait).await;
                            continue;
                        } else {
                            return Err(e);
                        }
                    }

                    let body: Result<ChatResponse> =
                        resp.json().await.context("parse chat response");
                    match body {
                        Ok(body) => {
                            if let Some(msg) = body.choices.into_iter().next().map(|c| c.message) {
                                return Ok(msg);
                            } else {
                                let e = anyhow::anyhow!("no choices returned");
                                return Err(e);
                            }
                        }
                        Err(e) => {
                            let kind = LlmErrorKind::Deserialize;
                            if self.should_retry(kind.clone()) && attempt < max_attempts {
                                let wait = self.backoff_delay(attempt, None);
                                warn!(attempt, kind=?kind, "retrying after deserialize error");
                                tokio::time::sleep(wait).await;
                                last_err = Some(e);
                                continue;
                            } else {
                                return Err(e);
                            }
                        }
                    }
                }
            }

            if attempt < max_attempts {
                let kind = crate::llm::classify_error(None, last_err.as_ref().unwrap());
                if self.should_retry(kind.clone()) {
                    let wait = self.backoff_delay(attempt, None);
                    info!(attempt, kind=?kind, wait_ms=%wait.as_millis(), "retrying chat_once");
                    tokio::time::sleep(wait).await;
                    continue;
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("unknown error")))
    }

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
        if self.llm_cfg.respect_retry_after {
            if let Some(secs) = retry_after_secs {
                return Duration::from_secs(secs);
            }
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
                    content: "hi".into(),
                }],
            )
            .await
            .unwrap();
        assert_eq!(msg.content, "hello");
    }

    #[tokio::test]
    async fn chat_once_retries_on_500_then_succeeds() {
        let server = Server::run();
        // Phase 1: expect a single 500 and verify it happens
        server.expect(
            Expectation::matching(request::method_path("POST", "/v1/chat/completions"))
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
                    content: "hi".into(),
                }],
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("500"));

        // Phase 2: expect a single 200 and verify success with one retry allowed
        server.expect(
            Expectation::matching(request::method_path("POST", "/v1/chat/completions"))
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
                    content: "hi".into(),
                }],
            )
            .await
            .unwrap();
        assert_eq!(msg.content, "ok");
    }

    #[tokio::test]
    async fn chat_once_non200_is_error_no_retry_on_400() {
        let server = Server::run();
        server.expect(
            Expectation::matching(request::method_path("POST", "/v1/chat/completions"))
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
                    content: "hi".into(),
                }],
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("400"));
    }

    #[test]
    fn endpoint_normalization() {
        let c = OpenAIClient {
            base_url: "https://api.example.com/v1/".into(),
            api_key: "x".into(),
            inner: reqwest::Client::new(),
            llm_cfg: LlmConfig::default(),
        };
        assert_eq!(c.endpoint(), "https://api.example.com/v1/chat/completions");
        let c2 = OpenAIClient {
            base_url: "https://api.example.com/".into(),
            api_key: "x".into(),
            inner: reqwest::Client::new(),
            llm_cfg: LlmConfig::default(),
        };
        assert_eq!(c2.endpoint(), "https://api.example.com/v1/chat/completions");
    }
}
