use anyhow::{Context, Result};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap};
use serde::{Deserialize, Serialize};

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

// For stream delta based on OpenAI-compatible APIs (delta.content chunks)
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

#[derive(Debug, Clone)]
pub struct OpenAIClient {
    pub base_url: String,
    pub api_key: String,
    inner: reqwest::Client,
}

impl OpenAIClient {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Result<Self> {
        let inner = reqwest::Client::builder().build()?;
        Ok(Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            inner,
        })
    }

    fn endpoint(&self) -> String {
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

        let resp = self
            .inner
            .post(url)
            .headers(headers)
            .json(&req)
            .send()
            .await
            .context("send chat request")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("chat error: {} - {}", status, text);
        }
        let body: ChatResponse = resp.json().await.context("parse chat response")?;
        body.choices
            .into_iter()
            .next()
            .map(|c| c.message)
            .context("no choices returned")
    }

    #[allow(dead_code)]
    pub async fn chat_stream(
        &self,
        model: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<impl futures::Stream<Item = Result<String>> + '_> {
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
        // Convert bytes to lines, parse each SSE/JSONL-like "data: {json}" line
        let mut buf = Vec::<u8>::new();
        let s = stream
            .map(move |chunk_res| {
                match chunk_res {
                    Ok(chunk) => {
                        buf.extend_from_slice(&chunk);
                        let mut out: Vec<Result<String>> = Vec::new();
                        // split by newlines
                        let mut start = 0usize;
                        for i in 0..buf.len() {
                            if buf[i] == b'\n' {
                                // one line
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
                                    if let Ok(json) =
                                        serde_json::from_str::<ChatStreamChunk>(payload)
                                    {
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
                        // retain tail
                        if start > 0 {
                            buf.drain(0..start);
                        }
                        futures::stream::iter(out)
                    }
                    Err(e) => futures::stream::iter(vec![Err(anyhow::anyhow!(e))]),
                }
            })
            .flatten();
        Ok(s)
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

        // Provide base with a trailing slash to verify normalization
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
    async fn chat_once_non200_is_error() {
        let server = Server::run();
        server.expect(
            Expectation::matching(request::method_path("POST", "/v1/chat/completions"))
                .respond_with(status_code(500).body("oops")),
        );
        let client = OpenAIClient::new(server.url_str(""), "test-key").unwrap();
        let err = client
            .chat_once(
                "gpt-test",
                vec![ChatMessage {
                    role: "user".into(),
                    content: "hi".into(),
                }],
            )
            .await
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("500"));
    }

    #[test]
    fn endpoint_normalization() {
        let c = OpenAIClient {
            base_url: "https://api.example.com/v1/".into(),
            api_key: "x".into(),
            inner: reqwest::Client::new(),
        };
        assert_eq!(c.endpoint(), "https://api.example.com/v1/chat/completions");
        let c2 = OpenAIClient {
            base_url: "https://api.example.com/".into(),
            api_key: "x".into(),
            inner: reqwest::Client::new(),
        };
        assert_eq!(c2.endpoint(), "https://api.example.com/v1/chat/completions");
    }
}
