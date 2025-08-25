use anyhow::{Context, Result};
use futures::{Stream, StreamExt};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap};
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::llm::LlmErrorKind;
use crate::llm::client_core::OpenAIClient;
use crate::llm::types::{ChatMessage, Usage};

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
    pub usage: Option<Usage>,
}

impl OpenAIClient {
    pub async fn chat_stream(
        &self,
        model: &str,
        messages: Vec<ChatMessage>,
        cancel: Option<CancellationToken>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        use crate::llm::types::ChatRequest;

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

        if let Ok(payload) = serde_json::to_string_pretty(&req) {
            debug!(payload=%payload, endpoint=%url, "sending chat.completions payload (stream)");
        }

        let cancel_token = cancel.unwrap_or_default();

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

            let resp_res = tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    info!("chat_stream cancelled before send");
                    return Err(anyhow::anyhow!(LlmErrorKind::Cancelled));
                }
                res = fut => res,
            };

            match resp_res {
                Err(e) => {
                    if attempt < max_attempts {
                        let wait = self.backoff_delay(attempt, None);
                        warn!(attempt, err=%e, wait_ms=%wait.as_millis(), "retrying stream establish after error");
                        tokio::select! {
                            biased;
                            _ = cancel_token.cancelled() => {
                                info!("chat_stream cancelled during retry sleep");
                                return Err(anyhow::anyhow!(LlmErrorKind::Cancelled));
                            }
                            _ = tokio::time::sleep(wait) => {}
                        }
                        attempt += 1;
                        continue;
                    } else {
                        return Err(anyhow::Error::new(e).context("send chat request (stream)"));
                    }
                }
                Ok(resp) => {
                    if !resp.status().is_success() {
                        let status = resp.status();
                        let text = resp.text().await.unwrap_or_default();
                        if attempt < max_attempts
                            && (status.is_server_error() || status.as_u16() == 429)
                        {
                            let wait = self.backoff_delay(attempt, None);
                            info!(attempt, status=%status.as_u16(), wait_ms=%wait.as_millis(), "retrying stream establish after HTTP error");
                            tokio::select! {
                                biased;
                                _ = cancel_token.cancelled() => {
                                    info!("chat_stream cancelled during retry sleep");
                                    return Err(anyhow::anyhow!(LlmErrorKind::Cancelled));
                                }
                                _ = tokio::time::sleep(wait) => {}
                            }
                            attempt += 1;
                            continue;
                        }
                        anyhow::bail!("chat error: {} - {}", status, text);
                    }
                    break resp;
                }
            }
        };

        let mut byte_stream = resp.bytes_stream();
        let mut buf = Vec::<u8>::new();
        let client = self.clone();

        let stream = async_stream::try_stream! {
            loop {
                let chunk_res = tokio::select! {
                    biased;
                    _ = cancel_token.cancelled() => {
                        info!("chat_stream cancelled during byte stream read");
                        Err(anyhow::anyhow!(LlmErrorKind::Cancelled))
                    }
                    chunk = byte_stream.next() => match chunk {
                        Some(Ok(bytes)) => Ok(bytes),
                        Some(Err(e)) => Err(anyhow::Error::new(e).context("byte stream read error")),
                        None => break, // End of stream
                    }
                };

                let chunk = match chunk_res {
                    Ok(chunk) => chunk,
                    Err(e) => {
                        warn!(err=%e, "error reading chunk from byte stream");
                        Err(e)?;
                        break;
                    }
                };

                buf.extend_from_slice(&chunk);
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
                                continue;
                            }

                            debug!(response_chunk=%payload, "llm chat_stream response");

                            if let Ok(json) = serde_json::from_str::<ChatStreamChunk>(payload) {
                                if let Some(usage) = &json.usage {
                                    client.add_tokens(usage.total_tokens);
                                }

                                for ch in json.choices {
                                    if let Some(reason) = ch.finish_reason
                                        && reason == "stop"
                                    {
                                        continue;
                                    }
                                    let delta = ch.delta.content;
                                    if !delta.is_empty() {
                                        yield delta;
                                    }
                                    if !ch.delta.tool_calls.is_empty()
                                        && let Ok(marker) =
                                            serde_json::to_string(&ch.delta.tool_calls)
                                    {
                                        yield format!("__TOOL_CALLS_DELTA__:{}", marker);
                                    }
                                }
                            } else {
                                warn!(payload, "failed to parse stream chunk");
                            }
                        }
                    }
                }
                if start > 0 {
                    buf.drain(0..start);
                }
            }
        };

        Ok(Box::pin(stream))
    }
}
