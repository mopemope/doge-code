use anyhow::Result;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, info};

use crate::llm::client::{ChatMessage, OpenAIClient};

// Stream types
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StreamChoiceDelta {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub role: Option<String>,
    // OpenAI-compatible tool_calls (streamed as incremental deltas)
    #[serde(default)]
    pub tool_calls: Vec<ToolCallDelta>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolCallDelta {
    pub index: Option<usize>,
    #[serde(rename = "type")]
    pub kind: Option<String>, // "function"
    #[serde(default)]
    pub function: Option<ToolCallFunctionDelta>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolCallFunctionDelta {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub arguments: String, // streamed as partial JSON string
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChoice {
    pub index: usize,
    pub delta: StreamChoiceDelta,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatStreamChunk {
    pub id: Option<String>,
    pub choices: Vec<StreamChoice>,
}

impl OpenAIClient {
    #[allow(dead_code)]
    pub async fn chat_stream(
        &self,
        model: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<impl futures::Stream<Item = Result<String>> + '_> {
        use crate::llm::client::ChatRequest;
        use futures::StreamExt;

        let url = self.endpoint();
        let req = ChatRequest {
            model: model.to_string(),
            messages,
            temperature: None,
            stream: Some(true),
        };

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {}", self.api_key).parse().unwrap(),
        );

        if let Ok(payload) = serde_json::to_string(&req) {
            debug!(target: "llm", payload=%payload, endpoint=%url, "sending chat.completions payload (stream)");
        }

        // Only retry establishing the stream, not mid-stream reads
        let mut attempt = 1usize;
        let max_attempts = self.llm_cfg.max_retries.saturating_add(1);
        let resp = loop {
            let fut = self
                .inner
                .post(url.clone())
                .headers(headers.clone())
                .json(&req)
                .send();
            let timeout = Duration::from_millis(self.llm_cfg.request_timeout_ms);
            match tokio::time::timeout(timeout, fut).await {
                Err(_) => {
                    if attempt < max_attempts {
                        let wait = self.backoff_delay(attempt, None);
                        info!(attempt, wait_ms=%wait.as_millis(), "retrying stream establish after timeout");
                        tokio::time::sleep(wait).await;
                        attempt += 1;
                        continue;
                    } else {
                        anyhow::bail!("stream establish timeout");
                    }
                }
                Ok(Err(e)) => {
                    if attempt < max_attempts {
                        let wait = self.backoff_delay(attempt, None);
                        info!(attempt, err=%e, wait_ms=%wait.as_millis(), "retrying stream establish after error");
                        tokio::time::sleep(wait).await;
                        attempt += 1;
                        continue;
                    } else {
                        return Err(anyhow::Error::new(e).context("send chat request (stream)"));
                    }
                }
                Ok(Ok(resp)) => {
                    if !resp.status().is_success() {
                        let status = resp.status();
                        let text = resp.text().await.unwrap_or_default();
                        if attempt < max_attempts
                            && (status.is_server_error() || status.as_u16() == 429)
                        {
                            let wait = self.backoff_delay(attempt, None);
                            info!(attempt, status=%status.as_u16(), wait_ms=%wait.as_millis(), "retrying stream establish after HTTP error");
                            tokio::time::sleep(wait).await;
                            attempt += 1;
                            continue;
                        }
                        anyhow::bail!("chat error: {} - {}", status, text);
                    }
                    break resp;
                }
            }
        };

        let stream = resp.bytes_stream();
        let mut buf = Vec::<u8>::new();
        let _idle = Duration::from_millis(self.llm_cfg.read_idle_timeout_ms);
        let s = stream
            .map(move |chunk_res| match chunk_res {
                Ok(chunk) => {
                    buf.extend_from_slice(&chunk);
                    let mut out: Vec<Result<String>> = Vec::new();
                    let mut start = 0usize;
                    for i in 0..buf.len() {
                        if buf[i] == b'\n' {
                            let line = &buf[start..i];
                            start = i + 1;
                            if let Ok(s) = std::str::from_utf8(line) {
                                let s = s.trim();
                                if s.is_empty() {
                                    continue;
                                }
                                let payload = if let Some(rest) = s.strip_prefix("data:") {
                                    rest.trim()
                                } else {
                                    s
                                };
                                if payload == "[DONE]" {
                                    out.push(Ok(String::new()));
                                    continue;
                                }
                                if let Ok(json) = serde_json::from_str::<ChatStreamChunk>(payload) {
                                    for ch in json.choices {
                                        if let Some(reason) = ch.finish_reason {
                                            if reason == "stop" {
                                                continue;
                                            }
                                        }
                                        let delta = ch.delta.content;
                                        if !delta.is_empty() {
                                            out.push(Ok(delta));
                                        }
                                        if !ch.delta.tool_calls.is_empty() {
                                            if let Ok(marker) =
                                                serde_json::to_string(&ch.delta.tool_calls)
                                            {
                                                out.push(Ok(format!(
                                                    "__TOOL_CALLS_DELTA__:{marker}"
                                                )));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if start > 0 {
                        buf.drain(0..start);
                    }
                    futures::stream::iter(out)
                }
                Err(e) => futures::stream::iter(vec![Err(anyhow::anyhow!(e))]),
            })
            .map(move |item| {
                // Implement idle timeout by wrapping each chunk with a timeout if needed in future.
                item
            })
            .flatten();
        Ok(s)
    }
}
