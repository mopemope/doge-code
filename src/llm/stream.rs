use anyhow::{Context, Result};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap};
use serde::{Deserialize, Serialize};

use crate::llm::client::{ChatMessage, OpenAIClient};

// Stream types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChoiceDelta {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub role: Option<String>,
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

        let resp = self
            .inner
            .post(url)
            .headers(headers)
            .json(&req)
            .send()
            .await
            .context("send chat request (stream)")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("chat error: {} - {}", status, text);
        }

        let stream = resp.bytes_stream();
        let mut buf = Vec::<u8>::new();
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
            .flatten();
        Ok(s)
    }
}
