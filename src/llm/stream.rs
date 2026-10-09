use anyhow::Result;
use futures::{Stream, StreamExt};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap};
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::llm::LlmErrorKind;
use crate::llm::client_core::OpenAIClient;
use crate::llm::retry::{
    RequestAttemptFailure, RetryDelayDecision, cancel_aware_sleep, classify_transport,
    compute_retry_delay, extract_provider_code, is_context_length_exceeded_code, kind_for_status,
    max_attempts, parse_retry_after, should_retry,
};
use crate::llm::telemetry::{self, PROTOCOL_CHAT_COMPLETIONS};
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
        messages: &[ChatMessage],
        cancel: Option<CancellationToken>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        let model = self.wire_model(model)?;
        if self.api_key_responses(model)? {
            let response = crate::features::openai_subscription::responses::infer_api_key(
                self,
                model,
                messages,
                &[],
                None,
                cancel.unwrap_or_default(),
            )
            .await?;
            anyhow::ensure!(
                response.message.refusal.is_none()
                    && response
                        .message
                        .content
                        .as_deref()
                        .is_some_and(|s| !s.trim().is_empty()),
                LlmErrorKind::Incomplete
            );
            return Ok(Box::pin(futures::stream::once(async move {
                Ok(response.message.content.unwrap_or_default())
            })));
        }
        if let Some(auth) = &self.subscription {
            let result = crate::features::openai_subscription::responses::infer(
                self,
                auth,
                model,
                messages,
                &[],
                None,
                cancel.unwrap_or_default(),
            )
            .await?;
            return Ok(Box::pin(futures::stream::once(async move {
                Ok(result.content.unwrap_or_default())
            })));
        }
        anyhow::ensure!(
            !messages.iter().any(|m| m.provider_state.is_some()),
            "Responses history requires its original provider"
        );
        #[derive(Serialize)]
        struct ChatRequestRef<'a> {
            model: &'a str,
            messages: &'a [ChatMessage],
            #[serde(skip_serializing_if = "Option::is_none")]
            temperature: Option<f32>,
            #[serde(skip_serializing_if = "Option::is_none")]
            stream: Option<bool>,
        }

        let url = self.endpoint();
        let req = ChatRequestRef {
            model,
            messages,
            temperature: None,
            stream: Some(true),
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            "HTTP-Refer",
            "https://github.com/mopemope/doge-code".parse().unwrap(),
        );
        headers.insert("X-Title", "Doge-Code".parse().unwrap());
        headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {}", self.api_key).parse().unwrap(),
        );

        if tracing::enabled!(tracing::Level::DEBUG) {
            let request_bytes = telemetry::measure_bytes(&req);
            let summary = telemetry::summarize_chat_request(messages, 0, request_bytes, None);
            debug!(
                protocol = PROTOCOL_CHAT_COMPLETIONS,
                message_count = summary.message_count,
                system_messages = summary.system_messages,
                developer_messages = summary.developer_messages,
                user_messages = summary.user_messages,
                assistant_messages = summary.assistant_messages,
                tool_messages = summary.tool_messages,
                request_bytes = summary.request_bytes,
                "sending chat.completions request (stream)"
            );
        }

        let cancel_token = cancel.unwrap_or_default();

        // Only retry establishing the stream, not mid-stream reads.
        // Mid-stream read failures never replay the request.
        let mut attempt = 1usize;
        let total_attempts = max_attempts(self.llm_cfg.max_retries);
        let resp = loop {
            let request = self
                .inner
                .post(url.clone())
                .headers(headers.clone())
                .json(&req);
            let request = self.request_headers(request);
            let fut = async {
                self.record_request_attempt();
                request.send().await
            };

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
                    let kind = if e.is_timeout() {
                        LlmErrorKind::Timeout
                    } else {
                        classify_transport(&e)
                    };
                    let transport = telemetry::summarize_transport_error(&e);
                    let failure = RequestAttemptFailure::new(
                        kind.clone(),
                        None,
                        None,
                        None,
                        anyhow::anyhow!(kind).context("send chat request (stream)"),
                    );
                    if !should_retry(&failure) || attempt >= total_attempts {
                        if attempt >= total_attempts && should_retry(&failure) {
                            return Err(failure.source.context(format!(
                                "LLM request failed after {total_attempts} attempts"
                            )));
                        }
                        return Err(failure.source);
                    }
                    let delay = match compute_retry_delay(&self.llm_cfg, attempt, None) {
                        RetryDelayDecision::Decline => {
                            return Err(failure
                                .source
                                .context(format!("LLM request failed after {attempt} attempts")));
                        }
                        RetryDelayDecision::Sleep(d) => d,
                    };
                    warn!(
                        attempt,
                        total_attempts,
                        transport_timeout = transport.timeout,
                        transport_connect = transport.connect,
                        transport_request = transport.request,
                        wait_ms = delay.as_millis() as u64,
                        "retrying stream establish after error"
                    );
                    if cancel_aware_sleep(delay, &cancel_token).await {
                        info!("chat_stream cancelled during retry sleep");
                        return Err(anyhow::anyhow!(LlmErrorKind::Cancelled));
                    }
                    attempt += 1;
                    continue;
                }
                Ok(resp) => {
                    if !resp.status().is_success() {
                        let status = resp.status();
                        let request_id = telemetry::extract_request_id(resp.headers());
                        let retry_after = parse_retry_after(resp.headers());
                        let text = tokio::select! {
                            biased;
                            _ = cancel_token.cancelled() => {
                                info!("chat_stream cancelled during error body read");
                                return Err(anyhow::anyhow!(LlmErrorKind::Cancelled));
                            }
                            res = resp.text() => res.unwrap_or_default(),
                        };
                        let trimmed = text.trim().to_owned();
                        let meta = telemetry::parse_provider_error_metadata(
                            &trimmed,
                            request_id.as_deref(),
                        );
                        let provider_code = extract_provider_code(&trimmed);
                        if status.as_u16() == 401 || status.as_u16() == 403 {
                            return Err(anyhow::anyhow!(LlmErrorKind::Authentication)
                                .context(format!("chat error: {status}")));
                        }
                        if status.as_u16() == 400
                            && let Some(code) = provider_code.as_deref()
                            && is_context_length_exceeded_code(code)
                        {
                            return Err(anyhow::anyhow!(LlmErrorKind::ContextLengthExceeded));
                        }
                        let kind = kind_for_status(status);
                        let message = telemetry::provider_error_message(status.as_u16(), &meta);
                        let failure = RequestAttemptFailure::new(
                            kind.clone(),
                            Some(status),
                            retry_after,
                            provider_code,
                            anyhow::anyhow!(kind).context(message),
                        );
                        if !should_retry(&failure) || attempt >= total_attempts {
                            if attempt >= total_attempts && should_retry(&failure) {
                                return Err(failure.source.context(format!(
                                    "LLM request failed after {total_attempts} attempts"
                                )));
                            }
                            return Err(failure.source);
                        }
                        let delay = match compute_retry_delay(
                            &self.llm_cfg,
                            attempt,
                            failure.retry_after,
                        ) {
                            RetryDelayDecision::Decline => {
                                return Err(failure.source.context(format!(
                                    "LLM request failed after {attempt} attempts"
                                )));
                            }
                            RetryDelayDecision::Sleep(d) => d,
                        };
                        info!(attempt, total_attempts, status=%status.as_u16(), wait_ms=%delay.as_millis() as u64, "retrying stream establish after HTTP error");
                        if cancel_aware_sleep(delay, &cancel_token).await {
                            info!("chat_stream cancelled during retry sleep");
                            return Err(anyhow::anyhow!(LlmErrorKind::Cancelled));
                        }
                        attempt += 1;
                        continue;
                    }
                    break resp;
                }
            }
        };

        let mut byte_stream = resp.bytes_stream();
        let mut buf = Vec::<u8>::new();
        let client = self.clone();
        let timeout_duration = Duration::from_millis(self.llm_cfg.timeout_ms);

        let stream = async_stream::try_stream! {
            loop {
                // Set timeout for reading each chunk from the byte stream
                let chunk_fut = tokio::time::timeout(timeout_duration, byte_stream.next());

                let chunk_res = tokio::select! {
                    biased;
                    _ = cancel_token.cancelled() => {
                        info!("chat_stream cancelled during byte stream read");
                        Err(anyhow::anyhow!(LlmErrorKind::Cancelled))
                    }
                    res = chunk_fut => {
                        match res {
                            Ok(Some(Ok(bytes))) => Ok(bytes),
                            Ok(Some(Err(e))) => {
                                let transport = telemetry::summarize_transport_error(&e);
                                let transport_timeout = transport.timeout;
                                let transport_connect = transport.connect;
                                let transport_request = transport.request;
                                let transport_has_body_error = transport.body;
                                debug!(
                                    transport_timeout = transport_timeout,
                                    transport_connect = transport_connect,
                                    transport_request = transport_request,
                                    transport_body = transport_has_body_error,
                                    "stream byte read transport error"
                                );
                                Err(anyhow::Error::new(e).context("byte stream read error"))
                            }
                            Ok(None) => break, // End of stream
                            Err(_) => Err(anyhow::anyhow!(LlmErrorKind::Timeout)),
                        }
                    }
                };

                let chunk = match chunk_res {
                    Ok(chunk) => chunk,
                    Err(e) => {
                        warn!("error reading chunk from byte stream");
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

                            match serde_json::from_str::<ChatStreamChunk>(payload) {
                                Ok(json) => {
                                    let mut content_delta_chars = 0usize;
                                    let mut tool_call_delta_count = 0usize;
                                    for choice in &json.choices {
                                        content_delta_chars = content_delta_chars.saturating_add(
                                            choice.delta.content.chars().count(),
                                        );
                                        tool_call_delta_count = tool_call_delta_count
                                            .saturating_add(choice.delta.tool_calls.len());
                                    }
                                    let summary = telemetry::summarize_stream_chunk(
                                        payload.len(),
                                        json.choices.len(),
                                        content_delta_chars,
                                        tool_call_delta_count,
                                        json.usage.is_some(),
                                        true,
                                    );
                                    debug!(
                                        protocol = PROTOCOL_CHAT_COMPLETIONS,
                                        chunk_bytes = summary.chunk_bytes,
                                        parsed = summary.parsed,
                                        choice_count = summary.choice_count,
                                        content_delta_chars = summary.content_delta_chars,
                                        tool_call_delta_count = summary.tool_call_delta_count,
                                        usage_present = summary.usage_present,
                                        "llm chat_stream response"
                                    );
                                    if let Some(usage) = &json.usage {
                                        client.record_usage(usage);
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
                                }
                                Err(_) => {
                                    let summary = telemetry::summarize_stream_chunk(
                                        payload.len(),
                                        0,
                                        0,
                                        0,
                                        false,
                                        false,
                                    );
                                    warn!(
                                        chunk_bytes = summary.chunk_bytes,
                                        parsed = summary.parsed,
                                        "failed to parse stream chunk"
                                    );
                                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Clone)]
    struct ScriptedStreamResponse {
        status: u16,
        body: String,
        content_type: String,
        retry_after: Option<String>,
    }

    async fn spawn_scripted_stream_server(
        script: Vec<ScriptedStreamResponse>,
        counter: Arc<AtomicUsize>,
    ) -> String {
        use axum::{
            Router,
            http::{HeaderMap, StatusCode},
            response::IntoResponse,
            routing::post,
        };
        let script = Arc::new(script);
        let app = Router::new().route(
            "/v1/chat/completions",
            post({
                let script = script.clone();
                move |_headers: HeaderMap, _body: axum::body::Bytes| async move {
                    let n = counter.fetch_add(1, Ordering::SeqCst);
                    let s = script
                        .get(n)
                        .or(script.last())
                        .expect("empty script")
                        .clone();
                    let status =
                        StatusCode::from_u16(s.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                    let mut headers = HeaderMap::new();
                    if let Some(ra) = s.retry_after {
                        headers.insert("retry-after", ra.parse().unwrap());
                    }
                    headers.insert("content-type", s.content_type.parse().unwrap());
                    (status, headers, s.body).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}/")
    }

    fn stream_test_client(base_url: String, max_retries: usize) -> OpenAIClient {
        use crate::config::LlmConfig;
        OpenAIClient::new(base_url, "x")
            .unwrap()
            .with_llm_config(LlmConfig {
                max_retries,
                retry_base_ms: 1,
                retry_jitter_ms: 0,
                respect_retry_after: true,
                timeout_ms: 5_000,
                connect_timeout_ms: 5_000,
                request_timeout_ms: 5_000,
                read_idle_timeout_ms: 5_000,
                ..LlmConfig::default()
            })
    }

    fn sse_done_body(text: &str) -> String {
        let chunk = serde_json::json!({
            "id": "test",
            "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": null}]
        });
        format!("data: {}\n\ndata: [DONE]\n\n", chunk)
    }

    fn stream_messages() -> Vec<ChatMessage> {
        vec![ChatMessage {
            reasoning: Default::default(),
            provider_state: None,
            role: "user".into(),
            content: Some("hi".into()),
            tool_calls: vec![],
            tool_call_id: None,
        }]
    }

    #[tokio::test]
    async fn stream_establishment_503_retries() {
        use futures::StreamExt;
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_scripted_stream_server(
            vec![
                ScriptedStreamResponse {
                    status: 503,
                    body: "busy".into(),
                    content_type: "application/json".into(),
                    retry_after: Some("0".into()),
                },
                ScriptedStreamResponse {
                    status: 200,
                    body: sse_done_body("hi"),
                    content_type: "text/event-stream".into(),
                    retry_after: None,
                },
            ],
            counter.clone(),
        )
        .await;
        let client = stream_test_client(url, 3);
        let mut stream = client
            .chat_stream("gpt", &stream_messages(), None)
            .await
            .expect("establishment retry");
        let mut collected = String::new();
        while let Some(item) = stream.next().await {
            collected.push_str(&item.expect("chunk"));
        }
        assert!(collected.contains("hi"));
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn stream_establishment_permanent_400_fails_fast() {
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_scripted_stream_server(
            vec![ScriptedStreamResponse {
                status: 400,
                body: r#"{"error":{"code":"invalid_request_error"}}"#.into(),
                content_type: "application/json".into(),
                retry_after: None,
            }],
            counter.clone(),
        )
        .await;
        let client = stream_test_client(url, 3);
        let err = match client.chat_stream("gpt", &stream_messages(), None).await {
            Ok(_) => panic!("expected error"),
            Err(e) => e,
        };
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert!(!format!("{err:?}").contains("100"));
    }

    #[tokio::test]
    async fn stream_no_replay_after_start() {
        use futures::StreamExt;
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_scripted_stream_server(
            vec![ScriptedStreamResponse {
                status: 200,
                body: sse_done_body("hello"),
                content_type: "text/event-stream".into(),
                retry_after: None,
            }],
            counter.clone(),
        )
        .await;
        let client = stream_test_client(url, 3);
        let mut stream = client
            .chat_stream("gpt", &stream_messages(), None)
            .await
            .expect("stream");
        while let Some(item) = stream.next().await {
            let _ = item.expect("chunk");
        }
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "stream start must not replay the request"
        );
    }

    #[tokio::test]
    async fn stream_cancel_during_establishment_backoff() {
        use tokio_util::sync::CancellationToken;
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_scripted_stream_server(
            vec![ScriptedStreamResponse {
                status: 503,
                body: "busy".into(),
                content_type: "application/json".into(),
                retry_after: None,
            }],
            counter.clone(),
        )
        .await;
        use crate::config::LlmConfig;
        let client = OpenAIClient::new(url, "x")
            .unwrap()
            .with_llm_config(LlmConfig {
                max_retries: 3,
                retry_base_ms: 30_000,
                retry_jitter_ms: 0,
                respect_retry_after: true,
                timeout_ms: 5_000,
                connect_timeout_ms: 5_000,
                request_timeout_ms: 5_000,
                read_idle_timeout_ms: 5_000,
                ..LlmConfig::default()
            });
        let token = CancellationToken::new();
        let canceller = token.clone();
        let watch = counter.clone();
        tokio::spawn(async move {
            for _ in 0..100 {
                if watch.load(Ordering::SeqCst) >= 1 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            canceller.cancel();
        });
        let res = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.chat_stream("gpt", &stream_messages(), Some(token)),
        )
        .await
        .expect("cancel must be fast");
        let err = match res {
            Ok(_) => panic!("expected Cancelled"),
            Err(e) => e,
        };
        assert!(
            matches!(
                err.downcast_ref::<LlmErrorKind>(),
                Some(LlmErrorKind::Cancelled)
            ),
            "must be Cancelled, got {err:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stream_error_body_secret_not_in_error() {
        let secret = "SECRET_PROVIDER_BODY_123";
        let counter = Arc::new(AtomicUsize::new(0));
        let body = serde_json::json!({"error": {"code": "invalid_request", "message": secret}});
        let url = spawn_scripted_stream_server(
            vec![ScriptedStreamResponse {
                status: 400,
                body: serde_json::to_string(&body).expect("body"),
                content_type: "application/json".into(),
                retry_after: None,
            }],
            counter.clone(),
        )
        .await;
        let client = stream_test_client(url, 0);
        let err = match client.chat_stream("gpt", &stream_messages(), None).await {
            Ok(_) => panic!("expected error"),
            Err(e) => e,
        };
        let rendered = format!("{err:?} {}", err);
        assert!(
            !rendered.contains(secret),
            "provider error body must not leak, got {rendered}"
        );
        assert!(rendered.contains("400"));
        assert!(rendered.contains("invalid_request"));
    }

    #[tokio::test]
    async fn stream_content_preserved_for_display_but_not_logged() {
        use futures::StreamExt;
        let secret = "SECRET_STREAM_OUTPUT_789";
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_scripted_stream_server(
            vec![ScriptedStreamResponse {
                status: 200,
                body: sse_done_body(secret),
                content_type: "text/event-stream".into(),
                retry_after: None,
            }],
            counter.clone(),
        )
        .await;
        let client = stream_test_client(url, 0);
        let mut stream = client
            .chat_stream("gpt", &stream_messages(), None)
            .await
            .expect("stream");
        let mut collected = String::new();
        while let Some(item) = stream.next().await {
            collected.push_str(&item.expect("chunk"));
        }
        // Display path preserves content.
        assert!(collected.contains(secret));
        // Diagnostic summary carries only counts.
        let summary = crate::llm::telemetry::summarize_stream_chunk(
            secret.len(),
            1,
            secret.chars().count(),
            0,
            false,
            true,
        );
        let rendered = format!("{summary:?}");
        assert!(!rendered.contains(secret));
        assert_eq!(summary.content_delta_chars, secret.chars().count());
    }

    #[tokio::test]
    async fn stream_malformed_chunk_secret_not_logged() {
        let secret = "SECRET_STREAM_OUTPUT_789";
        let malformed = format!("not-json {secret} {{{{");
        let summary =
            crate::llm::telemetry::summarize_stream_chunk(malformed.len(), 0, 0, 0, false, false);
        let rendered = format!("{summary:?}");
        assert!(!rendered.contains(secret));
        assert!(!summary.parsed);
    }
}
