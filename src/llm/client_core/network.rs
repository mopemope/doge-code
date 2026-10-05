use anyhow::Result;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap};
use serde::Serialize;
use serde_json;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use super::OpenAIClient;
use crate::llm::LlmErrorKind;
use crate::llm::retry::{
    RequestAttemptFailure, RetryDelayDecision, cancel_aware_sleep, classify_transport,
    compute_retry_delay, extract_provider_code, is_context_length_exceeded_code, kind_for_status,
    max_attempts, parse_retry_after, should_retry,
};
use crate::llm::telemetry::{self, PROTOCOL_CHAT_COMPLETIONS};
use crate::llm::types::{ChatMessage, ChatRequest, ChatResponse, ChoiceMessage};

pub async fn chat_once(
    client: &OpenAIClient,
    model: &str,
    messages: Vec<ChatMessage>,
    cancel: Option<CancellationToken>,
) -> Result<ChoiceMessage> {
    let req = ChatRequest {
        model: model.to_string(),
        messages,
        temperature: None,
        stream: None,
    };

    chat_once_request(client, &req, cancel).await
}

pub(crate) async fn chat_once_request<T: Serialize + ?Sized>(
    client: &OpenAIClient,
    req: &T,
    cancel: Option<CancellationToken>,
) -> Result<ChoiceMessage> {
    let value = serde_json::to_value(req)?;
    anyhow::ensure!(
        !value
            .get("messages")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|messages| messages.iter().any(|m| m.get("provider_state").is_some())),
        "Responses history requires its original provider; start a new session"
    );
    let url = client.endpoint();

    let mut headers = HeaderMap::new();
    headers.insert(
        "HTTP-Refer",
        "https://github.com/mopemope/doge-code".parse().unwrap(),
    );
    headers.insert("X-Title", "Doge-Code".parse().unwrap());
    headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());
    headers.insert(
        AUTHORIZATION,
        format!("Bearer {}", client.api_key)
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid API key: {}", e))?,
    );

    if tracing::enabled!(tracing::Level::DEBUG) {
        let request_bytes = telemetry::measure_bytes(&value);
        let summary = telemetry::summarize_request_value(
            &value,
            PROTOCOL_CHAT_COMPLETIONS,
            request_bytes,
            None,
        );
        debug!(
            protocol = summary.protocol,
            message_count = summary.message_count,
            system_messages = summary.system_messages,
            developer_messages = summary.developer_messages,
            user_messages = summary.user_messages,
            assistant_messages = summary.assistant_messages,
            tool_messages = summary.tool_messages,
            tool_schema_count = summary.tool_schema_count,
            request_bytes = summary.request_bytes,
            "sending chat.completions request"
        );
    }

    let total_attempts = max_attempts(client.llm_cfg.max_retries);
    let cancel_token = cancel.unwrap_or_default();
    let mut last_failure: Option<RequestAttemptFailure> = None;

    for attempt in 1..=total_attempts {
        let req_builder = client.inner.post(&url).headers(headers.clone()).json(req);

        let resp_res = tokio::select! {
            biased;
            _ = cancel_token.cancelled() => {
                info!("chat_once cancelled before send");
                return Err(anyhow::anyhow!(LlmErrorKind::Cancelled));
            }
            res = async { client.record_request_attempt(); req_builder.send().await } => res,
        };

        // Transport failure: no HTTP status.
        let resp = match resp_res {
            Err(e) => {
                let kind = if e.is_timeout() {
                    LlmErrorKind::Timeout
                } else {
                    classify_transport(&e)
                };
                let transport = telemetry::summarize_transport_error(&e);
                let transport_timeout = transport.timeout;
                let transport_connect = transport.connect;
                let transport_request = transport.request;
                let transport_has_body_error = transport.body;
                error!(
                    attempt,
                    total_attempts,
                    transport_timeout = transport_timeout,
                    transport_connect = transport_connect,
                    transport_request = transport_request,
                    transport_body = transport_has_body_error,
                    "llm chat_once send error"
                );
                let failure = RequestAttemptFailure::new(
                    kind.clone(),
                    None,
                    None,
                    None,
                    anyhow::anyhow!(kind).context("send chat request"),
                );
                if !should_retry(&failure) || attempt >= total_attempts {
                    if attempt >= total_attempts && should_retry(&failure) {
                        return Err(failure.source.context(format!(
                            "LLM request failed after {total_attempts} attempts"
                        )));
                    }
                    return Err(failure.source);
                }
                let delay = match compute_retry_delay(&client.llm_cfg, attempt, None) {
                    RetryDelayDecision::Decline => {
                        return Err(failure
                            .source
                            .context(format!("LLM request failed after {attempt} attempts")));
                    }
                    RetryDelayDecision::Sleep(d) => d,
                };
                info!(
                    attempt,
                    total_attempts,
                    kind = ?failure.kind,
                    wait_ms = delay.as_millis() as u64,
                    "retrying chat_once"
                );
                last_failure = Some(failure);
                if cancel_aware_sleep(delay, &cancel_token).await {
                    info!("chat_once cancelled during retry sleep");
                    return Err(anyhow::anyhow!(LlmErrorKind::Cancelled));
                }
                continue;
            }
            Ok(resp) => resp,
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let request_id = telemetry::extract_request_id(resp.headers());
            let retry_after = parse_retry_after(resp.headers());
            let text = tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    info!("chat_once cancelled during error body read");
                    return Err(anyhow::anyhow!(LlmErrorKind::Cancelled));
                }
                res = resp.text() => res.unwrap_or_default(),
            };
            let trimmed = text.trim().to_owned();
            let meta = telemetry::parse_provider_error_metadata(&trimmed, request_id.as_deref());
            let provider_code = extract_provider_code(&trimmed);
            error!(
                attempt,
                total_attempts,
                status = status.as_u16(),
                provider_code = meta.code.as_deref().unwrap_or(""),
                provider_type = meta.error_type.as_deref().unwrap_or(""),
                body_bytes = meta.body_bytes,
                body_is_json = meta.body_is_json,
                request_id = request_id.as_deref().unwrap_or(""),
                "llm chat_once non-success status"
            );

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
            let delay = match compute_retry_delay(&client.llm_cfg, attempt, failure.retry_after) {
                RetryDelayDecision::Decline => {
                    return Err(failure
                        .source
                        .context(format!("LLM request failed after {attempt} attempts")));
                }
                RetryDelayDecision::Sleep(d) => d,
            };
            info!(
                attempt,
                total_attempts,
                kind = ?failure.kind,
                status = status.as_u16(),
                wait_ms = delay.as_millis() as u64,
                retry_after_present = failure.retry_after.is_some(),
                "retrying chat_once"
            );
            last_failure = Some(failure);
            if cancel_aware_sleep(delay, &cancel_token).await {
                info!("chat_once cancelled during retry sleep");
                return Err(anyhow::anyhow!(LlmErrorKind::Cancelled));
            }
            continue;
        }

        let response_text = match tokio::select! {
            biased;
            _ = cancel_token.cancelled() => {
                info!("chat_once cancelled during body read");
                return Err(anyhow::anyhow!(LlmErrorKind::Cancelled));
            }
            res = resp.text() => res
        } {
            Ok(text) => text,
            Err(e) => {
                let kind = classify_transport(&e);
                let transport = telemetry::summarize_transport_error(&e);
                let transport_timeout = transport.timeout;
                let transport_connect = transport.connect;
                let transport_request = transport.request;
                let transport_has_body_error = transport.body;
                error!(
                    attempt,
                    total_attempts,
                    transport_timeout = transport_timeout,
                    transport_connect = transport_connect,
                    transport_request = transport_request,
                    transport_body = transport_has_body_error,
                    "llm chat_once read body error"
                );
                let failure = RequestAttemptFailure::new(
                    kind.clone(),
                    None,
                    None,
                    None,
                    anyhow::anyhow!(kind).context("read chat response body"),
                );
                if !should_retry(&failure) || attempt >= total_attempts {
                    if attempt >= total_attempts && should_retry(&failure) {
                        return Err(failure.source.context(format!(
                            "LLM request failed after {total_attempts} attempts"
                        )));
                    }
                    return Err(failure.source);
                }
                let delay = match compute_retry_delay(&client.llm_cfg, attempt, None) {
                    RetryDelayDecision::Decline => {
                        return Err(failure
                            .source
                            .context(format!("LLM request failed after {attempt} attempts")));
                    }
                    RetryDelayDecision::Sleep(d) => d,
                };
                warn!(attempt, total_attempts, kind=?failure.kind, "retrying after body read error");
                last_failure = Some(failure);
                if cancel_aware_sleep(delay, &cancel_token).await {
                    info!("chat_once cancelled during retry sleep");
                    return Err(anyhow::anyhow!(LlmErrorKind::Cancelled));
                }
                continue;
            }
        };

        let response_bytes = response_text.len();
        let response_summary_placeholder = response_bytes;

        let mut parsed_response_text = response_text.as_str();
        let body: Result<ChatResponse, _> = serde_json::from_str(parsed_response_text);
        let body = match body {
            Ok(b) => Ok(b),
            Err(e) => {
                // Try to extract JSON from text if direct parsing failed
                if let Some(extracted) = extract_json_from_text(&response_text) {
                    debug!("Extracted JSON from response text");
                    parsed_response_text = extracted;
                    serde_json::from_str::<ChatResponse>(extracted).map_err(|e2| {
                        anyhow::anyhow!(
                            "Failed to parse extracted JSON: {} (original error: {})",
                            e2,
                            e
                        )
                    })
                } else {
                    Err(anyhow::Error::new(e))
                }
            }
        };

        match body {
            Ok(body) => {
                // Track token usage if available
                if let Some(usage) = &body.usage {
                    client.record_usage(usage);
                }
                let usage_present = body.usage.is_some();
                let choice_count = body.choices.len();
                let first = body.choices.into_iter().next();
                if let Some(choice) = first {
                    // Refusal is detected from the raw JSON pointer (the typed
                    // `ChoiceMessage` carries no refusal field). Compute it
                    // before summarizing so telemetry reflects the refusal.
                    let refusal_present =
                        serde_json::from_str::<serde_json::Value>(parsed_response_text)
                            .ok()
                            .and_then(|v| v.pointer("/choices/0/message/refusal").cloned())
                            .is_some_and(|value| !value.is_null());
                    let summary = telemetry::summarize_response_message(
                        Some(choice.message.content.as_str()),
                        0,
                        refusal_present.then_some("refusal"),
                        usage_present,
                        choice.finish_reason.as_deref(),
                        response_summary_placeholder,
                        choice_count,
                    );
                    debug!(
                        protocol = PROTOCOL_CHAT_COMPLETIONS,
                        response_bytes = summary.response_bytes,
                        choice_count = summary.choice_count,
                        assistant_content_chars = summary.assistant_content_chars,
                        refusal_present = summary.refusal_present,
                        usage_present = summary.usage_present,
                        finish_reason = summary.finish_reason.as_deref().unwrap_or(""),
                        "llm chat_once response"
                    );
                    crate::llm::types::validate_completion(choice.finish_reason.as_deref(), false)?;
                    anyhow::ensure!(choice.message.role == "assistant", LlmErrorKind::Client);
                    // Refusal is optional on compatible endpoints; explicit
                    // refusal must never become replacement file content.
                    if serde_json::from_str::<serde_json::Value>(parsed_response_text)
                        .ok()
                        .and_then(|v| v.pointer("/choices/0/message/refusal").cloned())
                        .is_some_and(|value| !value.is_null())
                    {
                        return Err(anyhow::anyhow!(LlmErrorKind::Incomplete)
                            .context("provider refused the response"));
                    }
                    return Ok(choice.message);
                }
                return Err(anyhow::anyhow!(LlmErrorKind::Client).context("no choices returned"));
            }
            Err(e) => {
                error!(attempt, total_attempts, err=%e, "llm chat_once deserialize error");
                // Deserialization never retries (fail fast).
                return Err(anyhow::anyhow!(LlmErrorKind::Deserialize)
                    .context(format!("parse chat response: {e}")));
            }
        }
    }

    Err(last_failure
        .map(|f| {
            f.source.context(format!(
                "LLM request failed after {total_attempts} attempts"
            ))
        })
        .unwrap_or_else(|| anyhow::anyhow!("unknown error")))
}

/// Attempts to extract a valid JSON object from a string that might contain other text.
/// It prioritizes extracting from markdown code blocks (```json ... ```).
/// If no code block is found, it falls back to finding the first `{` and the last `}`.
fn extract_json_from_text(text: &str) -> Option<&str> {
    // 1. Try to find a markdown code block with "json" language specifier
    if let Some(start_tag) = text.find("```json") {
        let rest = &text[start_tag + 7..];
        if let Some(end_pos) = rest.find("```") {
            return Some(rest[..end_pos].trim());
        }
    }

    // 2. Fallback: Find the first `{` and last `}`.
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if start <= end {
        Some(&text[start..=end])
    } else {
        None
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
    struct ScriptedChatResponse {
        status: u16,
        body: serde_json::Value,
        retry_after: Option<String>,
    }

    async fn spawn_scripted_chat_server(
        script: Vec<ScriptedChatResponse>,
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
                    headers.insert("content-type", "application/json".parse().unwrap());
                    let body = serde_json::to_string(&s.body).unwrap_or_default();
                    (status, headers, body).into_response()
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

    fn chat_test_client(base_url: String, max_retries: usize) -> OpenAIClient {
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

    fn chat_messages() -> Vec<ChatMessage> {
        vec![ChatMessage {
            provider_state: None,
            role: "user".into(),
            content: Some("hi".into()),
            tool_calls: vec![],
            tool_call_id: None,
        }]
    }

    fn chat_ok_body() -> serde_json::Value {
        serde_json::json!({
            "id": "test",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}}]
        })
    }

    #[tokio::test]
    async fn chat_once_max_retries_zero_sends_once() {
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_scripted_chat_server(
            vec![ScriptedChatResponse {
                status: 500,
                body: serde_json::json!({"error": "oops"}),
                retry_after: None,
            }],
            counter.clone(),
        )
        .await;
        let client = chat_test_client(url, 0);
        let err = client
            .chat_once("gpt", chat_messages(), None)
            .await
            .unwrap_err();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert!(format!("{err:?}").contains("500"));
    }

    #[tokio::test]
    async fn chat_once_500_retries_then_succeeds() {
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_scripted_chat_server(
            vec![
                ScriptedChatResponse {
                    status: 500,
                    body: serde_json::json!({"error": "oops"}),
                    retry_after: Some("0".into()),
                },
                ScriptedChatResponse {
                    status: 200,
                    body: chat_ok_body(),
                    retry_after: None,
                },
            ],
            counter.clone(),
        )
        .await;
        let client = chat_test_client(url, 1);
        let msg = client
            .chat_once("gpt", chat_messages(), None)
            .await
            .expect("500 then success");
        assert_eq!(msg.content, "ok");
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn chat_once_400_does_not_retry() {
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_scripted_chat_server(
            vec![ScriptedChatResponse {
                status: 400,
                body: serde_json::json!({"error": {"code": "invalid_request_error"}}),
                retry_after: None,
            }],
            counter.clone(),
        )
        .await;
        let client = chat_test_client(url, 3);
        let err = client
            .chat_once("gpt", chat_messages(), None)
            .await
            .unwrap_err();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert!(format!("{err:?}").contains("400"));
    }

    #[tokio::test]
    async fn chat_once_error_body_secret_not_in_error() {
        let secret = "SECRET_PROVIDER_BODY_123";
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_scripted_chat_server(
            vec![ScriptedChatResponse {
                status: 400,
                body: serde_json::json!({"error": {"code": "invalid_request", "message": secret}}),
                retry_after: None,
            }],
            counter.clone(),
        )
        .await;
        let client = chat_test_client(url, 0);
        let err = client
            .chat_once("gpt", chat_messages(), None)
            .await
            .unwrap_err();
        let rendered = format!("{err:?} {}", err);
        assert!(
            !rendered.contains(secret),
            "provider error body must not leak, got {rendered}"
        );
        assert!(
            rendered.contains("400"),
            "status must remain, got {rendered}"
        );
        assert!(
            rendered.contains("invalid_request"),
            "safe code must remain, got {rendered}"
        );
    }

    #[tokio::test]
    async fn chat_once_plain_text_error_hides_body() {
        let secret = "SECRET_PROVIDER_BODY_456";
        // Plain-text bodies need a raw-string server (JSON helper would quote it).
        async fn spawn_raw(status: u16, raw: String, counter: Arc<AtomicUsize>) -> String {
            use axum::{
                Router,
                http::{HeaderMap, StatusCode},
                response::IntoResponse,
                routing::post,
            };
            let app = Router::new().route(
                "/v1/chat/completions",
                post(
                    move |_headers: HeaderMap, _body: axum::body::Bytes| async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        let status = StatusCode::from_u16(status)
                            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                        let mut headers = HeaderMap::new();
                        headers.insert("content-type", "text/plain".parse().unwrap());
                        (status, headers, raw.clone()).into_response()
                    },
                ),
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
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_raw(500, secret.to_owned(), counter.clone()).await;
        let client = chat_test_client(url, 0);
        let err = client
            .chat_once("gpt", chat_messages(), None)
            .await
            .unwrap_err();
        let rendered = format!("{err:?} {}", err);
        assert!(
            !rendered.contains(secret),
            "plain-text body must not leak, got {rendered}"
        );
        assert!(
            rendered.contains("500"),
            "status must remain, got {rendered}"
        );
    }

    #[test]
    fn test_extract_json_from_text() {
        let cases = vec![
            (r#"{"key": "value"}"#, Some(r#"{"key": "value"}"#)),
            (
                r#"```json
{"key": "value"}
```"#,
                Some(r#"{"key": "value"}"#),
            ),
            (
                r#"Here is the JSON: {"key": "value"}"#,
                Some(r#"{"key": "value"}"#),
            ),
            (
                r#"{"key": "value"} and some trailing text"#,
                Some(r#"{"key": "value"}"#),
            ),
            (r#"No JSON here"#, None),
            (r#"Invalid { range }"#, Some(r#"{ range }"#)),
            (
                r#"Some code:
```rust
fn main() {
    println!("hello");
}
```
And the JSON:
{"tool": "edit"}
"#,
                Some(
                    r#"{
    println!("hello");
}
```
And the JSON:
{"tool": "edit"}"#,
                ), // optimizing for the naive behavior failure demonstration
            ),
            (
                r#"Some code:
fn main() {
    println!("hello");
}
```
And the JSON:
```json
{"tool": "edit"}
```
"#,
                Some(r#"{"tool": "edit"}"#),
            ),
            // Still naive fallback behavior for non-marked blocks, but checking it doesn't crash
            (
                r#"Some code:
fn main() { ... }
And the JSON:
{"tool": "edit"}
"#,
                Some(
                    r#"{ ... }
And the JSON:
{"tool": "edit"}"#,
                ),
            ),
        ];

        for (input, expected) in cases {
            assert_eq!(extract_json_from_text(input), expected, "Input: {}", input);
        }
    }
}
