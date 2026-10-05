//! Content-free structured telemetry for LLM/agent diagnostics.
//!
//! Diagnostic logs must record `SHAPE + COUNTS + SAFE IDENTIFIERS`, never
//! `CONTENT`. This module is the single place where LLM transport metadata
//! is summarized so no caller needs to serialize full payloads or embed raw
//! provider bodies into logs or errors.
//!
//! Never store prompt bodies, source code, tool arguments/results,
//! assistant output, stream deltas, raw request/response bytes, provider
//! error message bodies, refusal bodies, encrypted Responses state, or
//! credentials in any struct here.

use serde::Serialize;

/// Protocol label for OpenAI-compatible Chat Completions transport.
pub const PROTOCOL_CHAT_COMPLETIONS: &str = "chat_completions";

/// Serde's error message can contain provider values or field names. Retain
/// only a closed category and numeric position, never the original error.
pub(crate) struct JsonErrorSummary {
    pub category: &'static str,
    pub line: usize,
    pub column: usize,
}

impl JsonErrorSummary {
    pub fn from_error(error: &serde_json::Error) -> Self {
        use serde_json::error::Category;
        Self {
            category: match error.classify() {
                Category::Io => "io",
                Category::Syntax => "syntax",
                Category::Data => "data",
                Category::Eof => "eof",
            },
            line: error.line(),
            column: error.column(),
        }
    }
}

impl std::fmt::Display for JsonErrorSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "JSON {} error at line {} column {}",
            self.category, self.line, self.column
        )
    }
}

/// Content-free summary of an outgoing LLM request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmRequestSummary {
    pub protocol: &'static str,
    pub message_count: usize,
    pub system_messages: usize,
    pub developer_messages: usize,
    pub user_messages: usize,
    pub assistant_messages: usize,
    pub tool_messages: usize,
    pub tool_schema_count: usize,
    pub request_bytes: u64,
    pub reasoning_effort: Option<&'static str>,
}

/// Content-free summary of a received LLM response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmResponseSummary {
    pub response_bytes: usize,
    pub choice_count: usize,
    pub assistant_content_chars: usize,
    pub tool_call_count: usize,
    pub refusal_present: bool,
    pub usage_present: bool,
    pub finish_reason: Option<String>,
}

/// Content-free summary of one SSE stream chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamChunkSummary {
    pub chunk_bytes: usize,
    pub parsed: bool,
    pub choice_count: usize,
    pub content_delta_chars: usize,
    pub tool_call_delta_count: usize,
    pub usage_present: bool,
}

/// Safe subset of a provider error body. Raw `message`/`detail`/`body`
/// text is never retained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderErrorMetadata {
    pub code: Option<String>,
    pub error_type: Option<String>,
    pub param: Option<String>,
    pub request_id: Option<String>,
    pub body_bytes: usize,
    pub body_is_json: bool,
}

/// Content-free classification of a `reqwest` transport failure.
/// The underlying error (which may contain a URL) is never logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportErrorSummary {
    pub timeout: bool,
    pub connect: bool,
    pub request: bool,
    pub body: bool,
    pub decode: bool,
    pub status: Option<u16>,
}

/// Content-free size of a text value (no content retained).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextSize {
    pub chars: usize,
    pub bytes: usize,
}

/// Keep only identifier-safe characters and bound the length.
///
/// Canonical implementation lives in [`crate::logging::safe_identifier`];
/// this re-export keeps LLM call sites on the telemetry path without
/// forming a `features <-> llm` module cycle.
pub fn safe_identifier(value: &str) -> String {
    crate::logging::safe_identifier(value)
}

/// Map a provider `finish_reason` to a small allowlist.
///
/// Known terminal reasons pass through; unknown provider-controlled
/// strings become `"unknown"` instead of being logged verbatim.
/// `None` stays `None`.
pub fn sanitize_finish_reason(value: Option<&str>) -> Option<String> {
    match value {
        None => None,
        Some(reason) => {
            let allowed = matches!(
                reason,
                "stop" | "tool_calls" | "length" | "content_filter" | "function_call"
            );
            if allowed {
                Some(reason.to_owned())
            } else {
                Some("unknown".to_owned())
            }
        }
    }
}

/// Measure serialized JSON bytes without allocating the payload string.
pub fn measure_bytes<T: Serialize + ?Sized>(value: &T) -> u64 {
    crate::llm::context_budget::serialized_size(value).unwrap_or(0)
}

/// Build a request summary from a serialized request value without
/// retaining any content. Counts `messages[*].role` and `tools` length only.
pub fn summarize_request_value(
    value: &serde_json::Value,
    protocol: &'static str,
    request_bytes: u64,
    reasoning_effort: Option<&'static str>,
) -> LlmRequestSummary {
    let mut system_messages = 0usize;
    let mut developer_messages = 0usize;
    let mut user_messages = 0usize;
    let mut assistant_messages = 0usize;
    let mut tool_messages = 0usize;
    let mut message_count = 0usize;
    if let Some(messages) = value.get("messages").and_then(|v| v.as_array()) {
        message_count = messages.len();
        for message in messages {
            match message.get("role").and_then(|v| v.as_str()) {
                Some("system") => system_messages += 1,
                Some("developer") => developer_messages += 1,
                Some("user") => user_messages += 1,
                Some("assistant") => assistant_messages += 1,
                Some("tool") => tool_messages += 1,
                _ => {}
            }
        }
    }
    let tool_schema_count = value
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    LlmRequestSummary {
        protocol,
        message_count,
        system_messages,
        developer_messages,
        user_messages,
        assistant_messages,
        tool_messages,
        tool_schema_count,
        request_bytes,
        reasoning_effort,
    }
}

/// Count roles in a Chat Completions message slice (no content inspected).
pub fn count_message_roles(messages: &[crate::llm::types::ChatMessage]) -> LlmRequestSummary {
    let mut system_messages = 0usize;
    let mut developer_messages = 0usize;
    let mut user_messages = 0usize;
    let mut assistant_messages = 0usize;
    let mut tool_messages = 0usize;
    for message in messages {
        match message.role.as_str() {
            "system" => system_messages += 1,
            "developer" => developer_messages += 1,
            "user" => user_messages += 1,
            "assistant" => assistant_messages += 1,
            "tool" => tool_messages += 1,
            _ => {}
        }
    }
    LlmRequestSummary {
        protocol: PROTOCOL_CHAT_COMPLETIONS,
        message_count: messages.len(),
        system_messages,
        developer_messages,
        user_messages,
        assistant_messages,
        tool_messages,
        tool_schema_count: 0,
        request_bytes: 0,
        reasoning_effort: None,
    }
}

/// Build a request summary without serializing content into the log.
pub fn summarize_chat_request(
    messages: &[crate::llm::types::ChatMessage],
    tool_schema_count: usize,
    request_bytes: u64,
    reasoning_effort: Option<&'static str>,
) -> LlmRequestSummary {
    let mut summary = count_message_roles(messages);
    summary.tool_schema_count = tool_schema_count;
    summary.request_bytes = request_bytes;
    summary.reasoning_effort = reasoning_effort;
    summary
}

/// Summarize a tool-call response message without retaining content.
pub fn summarize_response_message(
    content: Option<&str>,
    tool_call_count: usize,
    refusal: Option<&str>,
    usage_present: bool,
    finish_reason: Option<&str>,
    response_bytes: usize,
    choice_count: usize,
) -> LlmResponseSummary {
    LlmResponseSummary {
        response_bytes,
        choice_count,
        assistant_content_chars: content.map(|s| s.chars().count()).unwrap_or(0),
        tool_call_count,
        refusal_present: refusal.is_some_and(|s| !s.is_empty()),
        usage_present,
        finish_reason: sanitize_finish_reason(finish_reason),
    }
}

/// Summarize a parsed stream chunk without retaining delta text.
pub fn summarize_stream_chunk(
    chunk_bytes: usize,
    choice_count: usize,
    content_delta_chars: usize,
    tool_call_delta_count: usize,
    usage_present: bool,
    parsed: bool,
) -> StreamChunkSummary {
    StreamChunkSummary {
        chunk_bytes,
        parsed,
        choice_count,
        content_delta_chars,
        tool_call_delta_count,
        usage_present,
    }
}

/// Content-free size of a text value.
pub fn text_size(value: &str) -> TextSize {
    TextSize {
        chars: value.chars().count(),
        bytes: value.len(),
    }
}

/// Parse a provider error body into safe metadata.
///
/// Only structured `code` / `type` / `param` identifiers survive
/// (sanitized). The human-readable `message` is always discarded because
/// providers may echo submitted prompts or secrets there.
pub fn parse_provider_error_metadata(
    body: &str,
    request_id: Option<&str>,
) -> ProviderErrorMetadata {
    let body_bytes = body.len();
    let parsed: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let body_is_json = parsed.is_some();
    let mut code = None;
    let mut error_type = None;
    let mut param = None;
    if let Some(value) = parsed.as_ref()
        && let Some(error) = value.get("error")
        && let Some(object) = error.as_object()
    {
        code = object
            .get("code")
            .and_then(|v| v.as_str())
            .map(safe_identifier)
            .filter(|s| !s.is_empty());
        error_type = object
            .get("type")
            .and_then(|v| v.as_str())
            .map(safe_identifier)
            .filter(|s| !s.is_empty());
        param = object
            .get("param")
            .and_then(|v| v.as_str())
            .map(safe_identifier)
            .filter(|s| !s.is_empty());
    }
    ProviderErrorMetadata {
        code,
        error_type,
        param,
        request_id: request_id.map(safe_identifier).filter(|s| !s.is_empty()),
        body_bytes,
        body_is_json,
    }
}

/// Render a user-facing provider error without any raw body text.
pub fn provider_error_message(status: u16, meta: &ProviderErrorMetadata) -> String {
    let mut out = format!("chat error: HTTP {status}");
    if let Some(code) = meta.code.as_deref() {
        out.push_str(&format!(" (code={code}"));
    } else {
        out.push_str(" (code=unknown");
    }
    if let Some(error_type) = meta.error_type.as_deref() {
        out.push_str(&format!(", type={error_type}"));
    }
    if let Some(param) = meta.param.as_deref() {
        out.push_str(&format!(", param={param}"));
    }
    if let Some(request_id) = meta.request_id.as_deref() {
        out.push_str(&format!(", request={request_id}"));
    }
    out.push(')');
    out
}

/// Extract a sanitized `x-request-id` header value, if present.
pub fn extract_request_id(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(safe_identifier)
        .filter(|s| !s.is_empty())
}

/// Classify a `reqwest` transport error without retaining URL/body text.
pub fn summarize_transport_error(error: &reqwest::Error) -> TransportErrorSummary {
    TransportErrorSummary {
        timeout: error.is_timeout(),
        connect: error.is_connect(),
        request: error.is_request(),
        body: error.is_body(),
        decode: error.is_decode(),
        status: error.status().map(|s| s.as_u16()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET_SYSTEM: &str = "SECRET_SYSTEM_PROMPT_8fb3";
    const SECRET_USER: &str = "SECRET_USER_PROMPT_019a";
    const SECRET_SOURCE: &str = "SECRET_SOURCE_CODE_c1aa";
    const SECRET_ARG: &str = "SECRET_TOOL_ARGUMENT_76ef";
    const SECRET_RESULT: &str = "SECRET_TOOL_RESULT_ab12";
    const SECRET_MODEL: &str = "SECRET_MODEL_OUTPUT_cd34";
    const SECRET_ENCRYPTED: &str = "SECRET_ENCRYPTED_CONTENT_ef56";

    fn secret_message(role: &str, content: &str) -> crate::llm::types::ChatMessage {
        crate::llm::types::ChatMessage {
            provider_state: None,
            role: role.into(),
            content: Some(content.into()),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    fn assert_no_secret(rendered: &str) {
        for secret in [
            SECRET_SYSTEM,
            SECRET_USER,
            SECRET_SOURCE,
            SECRET_ARG,
            SECRET_RESULT,
            SECRET_MODEL,
            SECRET_ENCRYPTED,
        ] {
            assert!(
                !rendered.contains(secret),
                "telemetry must not contain secret {secret}"
            );
        }
    }

    #[test]
    fn request_summary_counts_without_content() {
        let messages = vec![
            secret_message("system", SECRET_SYSTEM),
            secret_message("user", SECRET_USER),
            secret_message("user", SECRET_SOURCE),
            secret_message("assistant", SECRET_MODEL),
            secret_message("tool", SECRET_RESULT),
        ];
        let bytes = measure_bytes(&messages);
        assert!(bytes > 0);
        let summary = summarize_chat_request(&messages, 7, bytes, Some("medium"));
        assert_eq!(summary.message_count, 5);
        assert_eq!(summary.system_messages, 1);
        assert_eq!(summary.user_messages, 2);
        assert_eq!(summary.assistant_messages, 1);
        assert_eq!(summary.tool_messages, 1);
        assert_eq!(summary.tool_schema_count, 7);
        let rendered = format!("{summary:?}");
        assert_no_secret(&rendered);
        assert!(rendered.contains("message_count"));
    }

    #[test]
    fn response_summary_hides_tool_arguments_and_content() {
        let tool_call = crate::llm::types::ToolCall {
            id: Some("call-1".into()),
            r#type: "function".into(),
            function: crate::llm::types::ToolCallFunction {
                name: "edit".into(),
                arguments: format!(r#"{{"content":"{SECRET_ARG}"}}"#),
            },
        };
        let rendered_args = format!("{tool_call:?}");
        assert!(rendered_args.contains(SECRET_ARG));
        let summary = summarize_response_message(
            Some(SECRET_MODEL),
            1,
            None,
            true,
            Some("tool_calls"),
            512,
            1,
        );
        let rendered = format!("{summary:?}");
        assert_no_secret(&rendered);
        assert_eq!(
            summary.assistant_content_chars,
            SECRET_MODEL.chars().count()
        );
        assert_eq!(summary.tool_call_count, 1);
        assert_eq!(summary.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn finish_reason_rejects_arbitrary_provider_strings() {
        assert_eq!(sanitize_finish_reason(None), None);
        assert_eq!(
            sanitize_finish_reason(Some("stop")).as_deref(),
            Some("stop")
        );
        assert_eq!(
            sanitize_finish_reason(Some("SECRET_MODEL_OUTPUT_cd34")).as_deref(),
            Some("unknown")
        );
        assert_eq!(
            sanitize_finish_reason(Some("stop\ninjected")).as_deref(),
            Some("unknown")
        );
    }

    #[test]
    fn safe_identifier_strips_controls_and_bounds_length() {
        assert_eq!(safe_identifier("req_abc-123.X:y_z"), "req_abc-123.X:y_z");
        assert_eq!(safe_identifier("a\nb\rc d\te"), "abcde");
        assert_eq!(safe_identifier(""), "");
        let long = "a".repeat(500);
        assert_eq!(safe_identifier(&long).len(), 128);
        assert_eq!(safe_identifier(SECRET_MODEL), SECRET_MODEL);
    }

    #[test]
    fn provider_error_metadata_discards_message_body() {
        let body = serde_json::json!({
            "error": {
                "message": "SECRET_PROVIDER_BODY_123",
                "type": "invalid_request_error",
                "code": "context_length_exceeded",
                "param": "messages"
            }
        });
        let raw = serde_json::to_string(&body).expect("body");
        assert!(raw.contains("SECRET_PROVIDER_BODY_123"));
        let meta = parse_provider_error_metadata(&raw, Some("req_abc"));
        assert_eq!(meta.code.as_deref(), Some("context_length_exceeded"));
        assert_eq!(meta.error_type.as_deref(), Some("invalid_request_error"));
        assert_eq!(meta.param.as_deref(), Some("messages"));
        assert_eq!(meta.request_id.as_deref(), Some("req_abc"));
        assert!(meta.body_is_json);
        let rendered = format!("{meta:?} {}", provider_error_message(400, &meta));
        assert!(!rendered.contains("SECRET_PROVIDER_BODY_123"));
        assert!(rendered.contains("context_length_exceeded"));
    }

    #[test]
    fn provider_error_metadata_plain_text_hides_body() {
        let body = "SECRET_PROVIDER_BODY_456";
        let meta = parse_provider_error_metadata(body, None);
        assert_eq!(meta.code, None);
        assert!(!meta.body_is_json);
        assert_eq!(meta.body_bytes, body.len());
        let rendered = format!("{meta:?} {}", provider_error_message(500, &meta));
        assert!(!rendered.contains("SECRET_PROVIDER_BODY_456"));
        assert!(rendered.contains("code=unknown"));
    }

    #[test]
    fn provider_state_debug_hides_encrypted_content() {
        let state = crate::features::openai_subscription::ProviderState {
            version: 1,
            account: "test".into(),
            model: "model".into(),
            output: vec![serde_json::json!({
                "type": "compaction",
                "encrypted_content": SECRET_ENCRYPTED
            })],
            additional_tool_names: Vec::new(),
        };
        let rendered = format!("{state:?}");
        assert!(!rendered.contains(SECRET_ENCRYPTED));
        assert!(rendered.contains("output_items"));
    }
}
