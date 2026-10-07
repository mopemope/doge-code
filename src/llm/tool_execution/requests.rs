use crate::config::{ReasoningEffort, ReasoningMode};
use crate::llm::LlmErrorKind;
use crate::llm::chat_with_tools::{ChatResponseWithTools, ChoiceMessageWithTools};
use crate::llm::client_core::OpenAIClient;
use crate::llm::message_utils::clean_json_text;
use crate::llm::reasoning::resolve_reasoning_hint;
use crate::llm::retry::{
    self, RequestAttemptFailure, RetryDelayDecision, classify_transport, compute_retry_delay,
    extract_provider_code, is_context_length_exceeded_code, kind_for_status, max_attempts,
    parse_retry_after, should_retry,
};
use crate::llm::telemetry::{self, PROTOCOL_CHAT_COMPLETIONS};
use crate::llm::types::{ChatMessage, ToolDef};
use anyhow::{Result, anyhow};
use serde::Serialize;
use std::sync::mpsc::Sender;
use std::time::Duration;
use tracing::{debug, error, warn};

#[derive(Debug, Serialize)]
struct ChatRequestWithToolsRef<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [ToolDef]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>, // {"type":"auto"}
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'static str>,
}

// GPT-6 Luna permits Chat Completions function calling only when reasoning
// is explicitly disabled. Omitting the field leaves the provider's medium
// default enabled, so `[reasoning] mode = "off"` alone is insufficient.
fn chat_tools_request<'a>(
    base_url: &str,
    model: &'a str,
    messages: &'a [ChatMessage],
    tools: &'a [ToolDef],
    effort: Option<ReasoningEffort>,
    mode: ReasoningMode,
) -> ChatRequestWithToolsRef<'a> {
    let luna_tools = !tools.is_empty()
        && model == "gpt-6-luna"
        && reqwest::Url::parse(base_url)
            .ok()
            .is_some_and(|url| url.host_str() == Some("api.openai.com"));
    let reasoning_effort = if luna_tools {
        Some("none")
    } else {
        resolve_reasoning_hint(base_url, model, &mode, effort).map(|e| e.as_api_str())
    };
    ChatRequestWithToolsRef {
        model,
        messages,
        temperature: None,
        tools: (!tools.is_empty()).then_some(tools),
        tool_choice: None,
        reasoning_effort,
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn chat_tools_once(
    client: &OpenAIClient,
    model: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    reasoning_effort: Option<ReasoningEffort>,
    reasoning_mode: ReasoningMode,
    cancel: Option<tokio_util::sync::CancellationToken>,
    ui_tx: Option<Sender<String>>,
) -> Result<ChoiceMessageWithTools> {
    chat_tools_once_with_activation(
        client,
        model,
        messages,
        tools,
        tools,
        reasoning_effort,
        reasoning_mode,
        cancel,
        ui_tx,
    )
    .await
}

/// Append-only Responses variant: `base_tools` is the stable top-level
/// namespace, `active_tools` is the live catalog snapshot for
/// `additional_tools` wiring and response validation. OpenAI-compatible
/// providers ignore `base_tools` and use `active_tools` as before.
#[allow(clippy::too_many_arguments)]
pub async fn chat_tools_once_with_activation(
    client: &OpenAIClient,
    model: &str,
    messages: &[ChatMessage],
    base_tools: &[ToolDef],
    active_tools: &[ToolDef],
    reasoning_effort: Option<ReasoningEffort>,
    reasoning_mode: ReasoningMode,
    cancel: Option<tokio_util::sync::CancellationToken>,
    ui_tx: Option<Sender<String>>,
) -> Result<ChoiceMessageWithTools> {
    crate::llm::history::validate_tool_blocks(messages, false)?;
    if let Some(auth) = &client.subscription {
        let effort = crate::llm::reasoning::resolve_reasoning_hint(
            &client.base_url,
            model,
            &reasoning_mode,
            reasoning_effort,
        );
        let message = crate::features::openai_subscription::responses::infer_with_activation(
            client,
            auth,
            model,
            messages,
            base_tools,
            active_tools,
            effort,
            cancel.unwrap_or_default(),
        )
        .await
        .map_err(|error| {
            if error
                .downcast_ref::<crate::features::openai_subscription::ProviderError>()
                .is_some_and(|provider| is_context_length_exceeded_code(&provider.code))
            {
                error.context(LlmErrorKind::ContextLengthExceeded)
            } else {
                error
            }
        })?;
        validate_tool_message_with_activation(&message, base_tools, active_tools)?;
        return Ok(message);
    }
    anyhow::ensure!(
        !messages.iter().any(|m| m.provider_state.is_some()),
        "Responses history cannot be sent through another provider; start a new session"
    );
    // Single source of truth: `[llm] max_retries` = additional retries after
    // the first attempt. No separate timeout budget.
    let total_attempts = max_attempts(client.llm_cfg.max_retries);
    let cancel_token = cancel.unwrap_or_default();
    let mut last_failure: Option<RequestAttemptFailure> = None;

    for attempt in 1..=total_attempts {
        match chat_tools_once_attempt(
            client,
            model,
            messages,
            active_tools,
            reasoning_effort,
            reasoning_mode,
            Some(cancel_token.clone()),
        )
        .await
        {
            Ok(result) => return Ok(result),
            Err(failure) => {
                let is_last = attempt >= total_attempts;
                if !should_retry(&failure) || is_last {
                    if matches!(failure.kind, LlmErrorKind::Cancelled) {
                        return Err(failure.source);
                    }
                    error!(
                        attempt,
                        total_attempts,
                        kind = ?failure.kind,
                        status = failure.status.map(|s| s.as_u16()),
                        "llm chat_tools_once not retrying"
                    );
                    if is_last && should_retry(&failure) {
                        return Err(failure.source.context(format!(
                            "LLM request failed after {total_attempts} attempts"
                        )));
                    }
                    return Err(failure.source);
                }

                let decision = compute_retry_delay(&client.llm_cfg, attempt, failure.retry_after);
                let delay = match decision {
                    RetryDelayDecision::Decline => {
                        error!(
                            attempt,
                            total_attempts,
                            kind = ?failure.kind,
                            status = failure.status.map(|s| s.as_u16()),
                            "llm chat_tools_once declining retry (Retry-After too large)"
                        );
                        return Err(failure
                            .source
                            .context(format!("LLM request failed after {attempt} attempts")));
                    }
                    RetryDelayDecision::Sleep(d) => d,
                };

                warn!(
                    attempt,
                    total_attempts,
                    kind = ?failure.kind,
                    status = failure.status.map(|s| s.as_u16()),
                    provider_code = failure.provider_code.as_deref().unwrap_or(""),
                    wait_ms = delay.as_millis() as u64,
                    retry_after_present = failure.retry_after.is_some(),
                    "Retrying chat_tools_once after error"
                );
                if let Some(ref tx) = ui_tx {
                    let _ = tx.send(format!(
                        "::status:waiting:Retrying request (attempt {}/{})...",
                        attempt.saturating_add(1),
                        total_attempts
                    ));
                }
                last_failure = Some(failure);
                if retry::cancel_aware_sleep(delay, &cancel_token).await {
                    warn!("chat_tools_once cancelled during retry sleep");
                    return Err(anyhow!(LlmErrorKind::Cancelled));
                }
            }
        }
    }

    Err(last_failure
        .map(|f| {
            f.source.context(format!(
                "LLM request failed after {total_attempts} attempts"
            ))
        })
        .unwrap_or_else(|| anyhow!("LLM request failed")))
}

/// Exactly one HTTP attempt. No hidden retry inside: the caller
/// ([`chat_tools_once`]) owns retry orchestration, counting and backoff.
#[allow(clippy::too_many_arguments)]
async fn chat_tools_once_attempt(
    client: &OpenAIClient,
    model: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    reasoning_effort: Option<ReasoningEffort>,
    reasoning_mode: ReasoningMode,
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> Result<ChoiceMessageWithTools, RequestAttemptFailure> {
    use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap};

    let url = client.endpoint();
    let req = chat_tools_request(
        &client.base_url,
        model,
        messages,
        tools,
        reasoning_effort,
        reasoning_mode,
    );
    let reasoning_effort_str = req.reasoning_effort;
    let hint_sent = reasoning_effort_str.is_some();
    if tracing::enabled!(tracing::Level::DEBUG) {
        let summary = telemetry::summarize_chat_request(
            messages,
            tools.len(),
            telemetry::measure_bytes(&req),
            reasoning_effort_str,
        );
        debug!(
            protocol = PROTOCOL_CHAT_COMPLETIONS,
            message_count = summary.message_count,
            system_messages = summary.system_messages,
            developer_messages = summary.developer_messages,
            user_messages = summary.user_messages,
            assistant_messages = summary.assistant_messages,
            tool_messages = summary.tool_messages,
            tool_schema_count = summary.tool_schema_count,
            request_bytes = summary.request_bytes,
            reasoning_mode = reasoning_mode.as_str(),
            reasoning_hint_sent = hint_sent,
            "reasoning decision for chat_tools_once",
        );
    }
    // Keep the original requested effort available for future telemetry
    // without logging content; the resolved hint above drives the wire.

    let mut headers = HeaderMap::new();
    headers.insert(
        "HTTP-Referer",
        "https://github.com/mopemope/doge-code".parse().unwrap(),
    );
    headers.insert("X-Title", "Doge-Code".parse().unwrap());
    headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());
    let auth_value = match format!("Bearer {}", client.api_key).parse() {
        Ok(v) => v,
        Err(e) => {
            return Err(RequestAttemptFailure::new(
                LlmErrorKind::Client,
                None,
                None,
                None,
                anyhow!(LlmErrorKind::Client).context(format!("Invalid API key: {e}")),
            ));
        }
    };
    headers.insert(AUTHORIZATION, auth_value);

    let cancel_token = cancel.unwrap_or_default();
    let req_builder = client.inner.post(&url).headers(headers).json(&req);

    let timeout_duration = Duration::from_millis(client.llm_cfg.timeout_ms);
    let resp_fut = tokio::time::timeout(timeout_duration, async {
        client.begin_request_attempt()?;
        Ok::<_, anyhow::Error>(req_builder.send().await)
    });

    let resp = tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            warn!("chat_tools_once cancelled before send");
            return Err(RequestAttemptFailure::new(
                LlmErrorKind::Cancelled,
                None,
                None,
                None,
                anyhow!(LlmErrorKind::Cancelled),
            ));
        }
        res = resp_fut => {
            match res {
                Ok(Ok(Ok(resp))) => resp,
                Ok(Err(error)) => {
                    return Err(RequestAttemptFailure::new(LlmErrorKind::Client,
                        None, None, None, error));
                }
                Ok(Ok(Err(e))) => {
                    let kind = if e.is_timeout() {
                        LlmErrorKind::Timeout
                    } else {
                        classify_transport(&e)
                    };
                    return Err(RequestAttemptFailure::new(
                        kind.clone(),
                        None,
                        None,
                        None,
                        anyhow!(kind).context("send chat request (tools)"),
                    ));
                }
                Err(_) => {
                    return Err(RequestAttemptFailure::new(
                        LlmErrorKind::Timeout,
                        None,
                        None,
                        None,
                        anyhow!(LlmErrorKind::Timeout)
                            .context("send chat request (tools) timed out"),
                    ));
                }
            }
        }
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let request_id = telemetry::extract_request_id(resp.headers());
        let retry_after = parse_retry_after(resp.headers());
        let text = tokio::select! {
            biased;
            _ = cancel_token.cancelled() => {
                warn!("chat_tools_once cancelled during error body read");
                return Err(RequestAttemptFailure::new(
                    LlmErrorKind::Cancelled,
                    Some(status),
                    retry_after,
                    None,
                    anyhow!(LlmErrorKind::Cancelled),
                ));
            }
            res = resp.text() => res.unwrap_or_default(),
        };
        let trimmed = text.trim().to_owned();
        let meta = telemetry::parse_provider_error_metadata(&trimmed, request_id.as_deref());
        let provider_code = extract_provider_code(&trimmed);
        // Never log full bodies or prompts; status + structured code only.
        error!(
            status = status.as_u16(),
            provider_code = meta.code.as_deref().unwrap_or(""),
            provider_type = meta.error_type.as_deref().unwrap_or(""),
            body_bytes = meta.body_bytes,
            body_is_json = meta.body_is_json,
            request_id = request_id.as_deref().unwrap_or(""),
            "llm chat_tools_once non-success status"
        );

        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(RequestAttemptFailure::new(
                LlmErrorKind::Authentication,
                Some(status),
                retry_after,
                provider_code,
                anyhow!(LlmErrorKind::Authentication)
                    .context(format!("chat (tools) auth error: {status}")),
            ));
        }

        if status.as_u16() == 400
            && let Some(code) = provider_code.as_deref()
            && is_context_length_exceeded_code(code)
        {
            // Must reach the agent loop untouched for reactive compaction.
            return Err(RequestAttemptFailure::new(
                LlmErrorKind::ContextLengthExceeded,
                Some(status),
                None,
                provider_code,
                anyhow!(LlmErrorKind::ContextLengthExceeded),
            ));
        }

        let kind = kind_for_status(status);
        let message = telemetry::provider_error_message(status.as_u16(), &meta);
        return Err(RequestAttemptFailure::new(
            kind.clone(),
            Some(status),
            retry_after,
            provider_code,
            anyhow!(kind).context(message),
        ));
    }

    let response_text_fut = tokio::time::timeout(timeout_duration, resp.text());

    let response_text: String = tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            warn!("chat_tools_once cancelled during body read");
            return Err(RequestAttemptFailure::new(
                LlmErrorKind::Cancelled,
                None,
                None,
                None,
                anyhow!(LlmErrorKind::Cancelled),
            ));
        }
        res = response_text_fut => {
            match res {
                Ok(Ok(text)) => text.trim().to_owned(),
                Ok(Err(e)) => {
                    let kind = classify_transport(&e);
                    return Err(RequestAttemptFailure::new(
                        kind.clone(),
                        None,
                        None,
                        None,
                        anyhow!(kind).context("read chat response body (tools)"),
                    ));
                }
                Err(_) => {
                    return Err(RequestAttemptFailure::new(
                        LlmErrorKind::Timeout,
                        None,
                        None,
                        None,
                        anyhow!(LlmErrorKind::Timeout)
                            .context("read chat response body (tools) timed out"),
                    ));
                }
            }
        }
    };

    let response_bytes = response_text.len();

    let cleaned_text = clean_json_text(&response_text);
    let body: ChatResponseWithTools = match serde_json::from_str(&cleaned_text) {
        Ok(b) => b,
        Err(e) => {
            let summary = telemetry::JsonErrorSummary::from_error(&e);
            error!(
                category = summary.category,
                line = summary.line,
                column = summary.column,
                response_bytes,
                "llm chat_tools_once deserialize error"
            );
            return Err(RequestAttemptFailure::new(
                LlmErrorKind::Deserialize,
                None,
                None,
                None,
                anyhow!(LlmErrorKind::Deserialize)
                    .context(format!("parse chat response: {summary}")),
            ));
        }
    };

    // Success-only usage accounting: exactly one record per successful
    // response, never for failed attempts.
    if let Some(usage) = &body.usage {
        client.record_usage(usage);
    }
    let usage_present = body.usage.is_some();
    let choice_count = body.choices.len();

    let msg = match body.choices.into_iter().next() {
        Some(m) => m,
        None => {
            return Err(RequestAttemptFailure::new(
                LlmErrorKind::Client,
                None,
                None,
                None,
                anyhow!(LlmErrorKind::Client).context("no choices"),
            ));
        }
    };

    let validation = (|| -> Result<()> {
        crate::llm::types::validate_completion(
            msg.finish_reason.as_deref(),
            !msg.message.tool_calls.is_empty(),
        )?;
        validate_tool_message(&msg.message, tools)
    })();
    if let Err(error) = validation {
        let kind = error
            .downcast_ref::<LlmErrorKind>()
            .cloned()
            .unwrap_or(LlmErrorKind::Client);
        return Err(RequestAttemptFailure::new(kind, None, None, None, error));
    }
    {
        let summary = telemetry::summarize_response_message(
            msg.message.content.as_deref(),
            msg.message.tool_calls.len(),
            msg.message.refusal.as_deref(),
            usage_present,
            msg.finish_reason.as_deref(),
            response_bytes,
            choice_count,
        );
        debug!(
            protocol = PROTOCOL_CHAT_COMPLETIONS,
            response_bytes = summary.response_bytes,
            choice_count = summary.choice_count,
            assistant_content_chars = summary.assistant_content_chars,
            tool_call_count = summary.tool_call_count,
            refusal_present = summary.refusal_present,
            usage_present = summary.usage_present,
            finish_reason = summary.finish_reason.as_deref().unwrap_or(""),
            "llm chat_tools_once response"
        );
    }
    Ok(msg.message)
}

/// One preflight for both provider protocols, before any batch sibling executes.
fn validate_tool_message(message: &ChoiceMessageWithTools, tools: &[ToolDef]) -> Result<()> {
    validate_tool_message_with_activation(message, tools, tools)
}

/// Responses-aware preflight: for subscription responses the provider-side
/// namespace check already ran in `completed_with_activation`; here we
/// only ensure the batch is well-formed and within the union of base and
/// active catalogs. Local dispatch still re-checks `is_tool_active`.
fn validate_tool_message_with_activation(
    message: &ChoiceMessageWithTools,
    base_tools: &[ToolDef],
    active_tools: &[ToolDef],
) -> Result<()> {
    if message.refusal.is_some() {
        return Err(anyhow!(LlmErrorKind::Incomplete).context("provider refused the response"));
    }
    let validation = (|| -> Result<()> {
        anyhow::ensure!(
            message.role == "assistant",
            "response must have assistant role"
        );
        let mut ids = std::collections::HashSet::new();
        for call in &message.tool_calls {
            anyhow::ensure!(
                call.r#type == "function"
                    && call
                        .id
                        .as_deref()
                        .is_some_and(|id| !id.is_empty() && ids.insert(id)),
                "invalid tool-call batch"
            );
            anyhow::ensure!(
                base_tools
                    .iter()
                    .any(|tool| tool.function.name == call.function.name)
                    || active_tools
                        .iter()
                        .any(|tool| tool.function.name == call.function.name),
                "tool-call batch requested a tool outside the active catalog"
            );
            let arguments: serde_json::Value = serde_json::from_str(&call.function.arguments)?;
            super::arguments::validate_builtin_arguments(&call.function.name, &arguments)?;
        }
        Ok(())
    })();
    validation.map_err(|error| {
        if let Some(parse_error) = error.downcast_ref::<serde_json::Error>() {
            let summary = telemetry::JsonErrorSummary::from_error(parse_error);
            error!(
                category = summary.category,
                line = summary.line,
                column = summary.column,
                "llm tool-call arguments deserialize error"
            );
            anyhow!(LlmErrorKind::Client).context(format!("invalid tool-call arguments: {summary}"))
        } else {
            error.context(LlmErrorKind::Client)
        }
    })
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
async fn chat_tools_once_inner(
    client: &OpenAIClient,
    model: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    reasoning_effort: Option<ReasoningEffort>,
    reasoning_mode: ReasoningMode,
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> Result<ChoiceMessageWithTools> {
    chat_tools_once_attempt(
        client,
        model,
        messages,
        tools,
        reasoning_effort,
        reasoning_mode,
        cancel,
    )
    .await
    .map_err(|failure| failure.source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ToolRoutingConfig, ToolRoutingMode};
    use crate::llm::tool_catalog::{
        ToolCatalog, ToolCatalogEntry, ToolSource, build_searchable_text,
    };
    use crate::llm::tool_def::default_tools_def;
    use crate::llm::types::{ToolDef, ToolFunctionDef};
    use httptest::{Expectation, matchers::*, responders::*};

    fn remote_fixture_entry(
        alias: &str,
        server: &str,
        remote: &str,
        desc: &str,
    ) -> ToolCatalogEntry {
        let def = ToolDef {
            kind: "function".into(),
            function: ToolFunctionDef {
                name: alias.to_string(),
                description: desc.to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "description": "resource id"}
                    }
                }),
                strict: None,
            },
        };
        let source = ToolSource::RemoteMcp {
            server_name: server.to_string(),
            remote_name: remote.to_string(),
        };
        let searchable_text = build_searchable_text(&def, &source);
        ToolCatalogEntry {
            definition: def,
            source,
            searchable_text,
        }
    }

    /// Shared deferred fixture: builtins plus GitHub/Slack/Linear remotes.
    fn deferred_fixture_catalog() -> ToolCatalog {
        let builtin_entries: Vec<ToolCatalogEntry> = default_tools_def()
            .into_iter()
            .map(|def| {
                let text = build_searchable_text(&def, &ToolSource::Builtin);
                ToolCatalogEntry {
                    definition: def,
                    source: ToolSource::Builtin,
                    searchable_text: text,
                }
            })
            .collect();
        let mut entries = builtin_entries;
        entries.push(remote_fixture_entry(
            "mcp_github_get_pull_request",
            "github",
            "get_pull_request",
            "Get a GitHub pull request by number",
        ));
        entries.push(remote_fixture_entry(
            "mcp_github_create_issue",
            "github",
            "create_issue",
            "Create a GitHub issue in a repository",
        ));
        entries.push(remote_fixture_entry(
            "mcp_slack_post_message",
            "slack",
            "post_message",
            "Send a message to a Slack channel",
        ));
        entries.push(remote_fixture_entry(
            "mcp_linear_create_issue",
            "linear",
            "create_issue",
            "Create a Linear task issue for the team",
        ));
        ToolCatalog::from_entries(
            entries,
            &ToolRoutingConfig {
                mode: ToolRoutingMode::Deferred,
                search_result_limit: 5,
            },
        )
    }

    fn assistant_done_response() -> serde_json::Value {
        serde_json::json!({
            "id": "test",
            "choices": [
                {"index": 0, "message": {"role": "assistant", "content": "done"}}
            ]
        })
    }

    fn assistant_done_response_with_usage(reasoning_tokens: Option<u32>) -> serde_json::Value {
        let mut usage = serde_json::json!({
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150
        });
        if let Some(rt) = reasoning_tokens {
            usage["completion_tokens_details"] = serde_json::json!({"reasoning_tokens": rt});
        }
        serde_json::json!({
            "id": "test",
            "choices": [
                {"index": 0, "message": {"role": "assistant", "content": "done"}}
            ],
            "usage": usage
        })
    }

    fn user_message() -> Vec<ChatMessage> {
        vec![ChatMessage {
            provider_state: None,
            role: "user".into(),
            content: Some("do the thing".into()),
            tool_calls: vec![],
            tool_call_id: None,
        }]
    }

    fn test_client_for(server: &httptest::Server) -> OpenAIClient {
        OpenAIClient::new(format!("{}/", server.url_str("")), "test-key").unwrap()
    }

    #[tokio::test]
    async fn test_first_payload_defers_remote_and_builtin_schemas() {
        let server = crate::test_support::HTTP_SERVER_POOL.get_server();
        let catalog = deferred_fixture_catalog();
        // Sanity: remotes and non-core builtins really are deferred here.
        assert!(!catalog.is_active("mcp_github_get_pull_request").await);
        assert!(catalog.is_active("edit").await);
        assert!(!catalog.is_active("task").await);
        assert!(catalog.is_active("tool_search").await);

        // The final HTTP payload carries core + tool_search schemas only.
        server.expect(
            Expectation::matching(all_of![
                request::method_path("POST", "/v1/chat/completions"),
                request::body(matches("tool_search")),
                request::body(not(matches("mcp_github_get_pull_request"))),
                request::body(not(matches("mcp_slack_post_message"))),
                request::body(not(matches("mcp_linear_create_issue"))),
            ])
            .times(1)
            .respond_with(json_encoded(assistant_done_response())),
        );
        let client = test_client_for(&server);
        let active = catalog.active_tool_defs().await;
        // Structural check on the exact snapshot being sent: only the core
        // set plus `tool_search` (a substring matcher for short names like
        // "edit" would be brittle against future description edits).
        let mut names: Vec<String> = active.iter().map(|d| d.function.name.clone()).collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "edit",
                "execute_process",
                "fs_read",
                "observation_read",
                "search_repomap",
                "search_text",
                "tool_search",
            ]
        );
        let msg = chat_tools_once_inner(
            &client,
            "gpt-test",
            &user_message(),
            &active,
            None,
            ReasoningMode::Off,
            None,
        )
        .await
        .expect("first deferred request");
        assert_eq!(msg.content.as_deref(), Some("done"));
    }

    #[tokio::test]
    async fn test_post_activation_payload_advertises_remote_schema() {
        let server = crate::test_support::HTTP_SERVER_POOL.get_server();
        let catalog = deferred_fixture_catalog();
        let newly = catalog
            .activate(&["mcp_github_get_pull_request".to_string()])
            .await;
        assert_eq!(newly, vec!["mcp_github_get_pull_request".to_string()]);

        server.expect(
            Expectation::matching(all_of![
                request::method_path("POST", "/v1/chat/completions"),
                request::body(matches("mcp_github_get_pull_request")),
            ])
            .times(1)
            .respond_with(json_encoded(assistant_done_response())),
        );
        let client = test_client_for(&server);
        let active = catalog.active_tool_defs().await;
        assert!(
            active
                .iter()
                .any(|d| d.function.name == "mcp_github_get_pull_request")
        );
        let msg = chat_tools_once_inner(
            &client,
            "gpt-test",
            &user_message(),
            &active,
            None,
            ReasoningMode::Off,
            None,
        )
        .await
        .expect("post-activation request");
        assert_eq!(msg.content.as_deref(), Some("done"));
    }

    #[tokio::test]
    async fn test_initial_payload_size_is_independent_of_remote_count() {
        // Deferred routing keeps the initial schema payload flat as MCP
        // servers are added: 5 vs 50 remote tools, same initial bytes.
        async fn initial_active_json(remote_count: usize) -> String {
            let mut entries: Vec<ToolCatalogEntry> = default_tools_def()
                .into_iter()
                .map(|def| {
                    let text = build_searchable_text(&def, &ToolSource::Builtin);
                    ToolCatalogEntry {
                        definition: def,
                        source: ToolSource::Builtin,
                        searchable_text: text,
                    }
                })
                .collect();
            for i in 0..remote_count {
                entries.push(remote_fixture_entry(
                    &format!("mcp_srv{i}_tool"),
                    &format!("srv{i}"),
                    "tool",
                    "A remote MCP helper tool for testing",
                ));
            }
            let catalog = ToolCatalog::from_entries(
                entries,
                &ToolRoutingConfig {
                    mode: ToolRoutingMode::Deferred,
                    search_result_limit: 5,
                },
            );
            let active = catalog.active_tool_defs().await;
            let all = catalog.all_tool_defs();
            let active_json = serde_json::to_string(&active).expect("serialize active");
            let all_json = serde_json::to_string(&all).expect("serialize all");
            assert!(
                active_json.len() < all_json.len(),
                "deferred payload must be smaller than the full catalog"
            );
            assert!(
                !active_json.contains("mcp_srv"),
                "no remote schema may leak into the initial payload"
            );
            active_json
        }

        let five = initial_active_json(5).await;
        let fifty = initial_active_json(50).await;
        assert_eq!(
            five, fifty,
            "initial active schema must not grow with remote tool count"
        );
    }

    #[test]
    fn luna_chat_tools_require_explicit_none_for_every_reasoning_mode() {
        let tools = default_tools_def();
        assert!(!tools.is_empty());
        for mode in [
            ReasoningMode::Auto,
            ReasoningMode::Fixed,
            ReasoningMode::Off,
        ] {
            let req = chat_tools_request(
                "https://api.openai.com/v1",
                "gpt-6-luna",
                &[],
                &tools,
                Some(ReasoningEffort::Medium),
                mode,
            );
            let body = serde_json::to_value(req).unwrap();
            assert_eq!(body["reasoning_effort"], "none");
            assert!(!body["tools"].as_array().unwrap().is_empty());
            assert!(body.get("temperature").is_none());
        }
    }

    #[test]
    fn luna_chat_tools_preserve_other_endpoints_models_and_toolless_requests() {
        let tools = default_tools_def();
        for (base, model, defs) in [
            ("https://api.openai.com/v1", "gpt-6-luna", &[][..]),
            ("https://api.openai.com/v1", "gpt-5-mini", tools.as_slice()),
            (
                "https://openrouter.ai/api/v1",
                "gpt-6-luna",
                tools.as_slice(),
            ),
            (
                "https://api.openai.com.example/v1",
                "gpt-6-luna",
                tools.as_slice(),
            ),
        ] {
            let req = chat_tools_request(
                base,
                model,
                &[],
                defs,
                Some(ReasoningEffort::High),
                ReasoningMode::Fixed,
            );
            assert_eq!(req.reasoning_effort, Some("high"));
        }
    }

    #[tokio::test]
    async fn luna_chat_tools_http_wire_uses_none_without_retry() {
        let server = crate::test_support::HTTP_SERVER_POOL.get_server();
        server.expect(
            Expectation::matching(all_of![
                request::method_path("POST", "/v1/chat/completions"),
                request::body(matches("\"reasoning_effort\":\"none\"")),
                request::body(matches("\"tools\":")),
            ])
            .times(1)
            .respond_with(json_encoded(assistant_done_response())),
        );
        let client = OpenAIClient::new(server.url_str(""), "test-key").unwrap();
        let messages = user_message();
        let tools = default_tools_def();
        let req = chat_tools_request(
            "https://api.openai.com/v1",
            "gpt-6-luna",
            &messages,
            &tools,
            Some(ReasoningEffort::Medium),
            ReasoningMode::Auto,
        );
        server.expect(
            Expectation::matching(all_of![
                request::method_path("POST", "/v1/chat/completions"),
                request::body(matches("\"reasoning_effort\":\"medium\"")),
            ])
            .times(1)
            .respond_with(
                httptest::responders::status_code(400).body(
                    r#"{"error":{"type":"invalid_request_error","code":"unsupported_parameter","param":"reasoning_effort","message":"synthetic incompatible reasoning fixture"}}"#,
                ),
            ),
        );
        let mut incompatible = chat_tools_request(
            "https://api.openai.com/v1",
            "gpt-6-luna",
            &messages,
            &tools,
            Some(ReasoningEffort::Medium),
            ReasoningMode::Auto,
        );
        incompatible.reasoning_effort = Some("medium");
        let error = client
            .chat_once_request(&incompatible, None)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("HTTP 400"));
        let result = client.chat_once_request(&req, None).await.unwrap();
        assert_eq!(result.content, "done");
    }

    // --- Reasoning payload tests (v1) ---

    fn reasoning_body_serializes(
        base_url: &str,
        model: &str,
        effort: Option<ReasoningEffort>,
        mode: ReasoningMode,
    ) -> serde_json::Value {
        let resolved =
            crate::llm::reasoning::resolve_reasoning_hint(base_url, model, &mode, effort);
        let req = ChatRequestWithToolsRef {
            model,
            messages: &[],
            temperature: None,
            tools: None,
            tool_choice: None,
            reasoning_effort: resolved.map(|e| e.as_api_str()),
        };
        serde_json::to_value(&req).expect("serialize reasoning request")
    }

    #[tokio::test]
    async fn test_reasoning_initial_medium_for_capable_provider() {
        // Auto + OpenRouter (supported): medium is serialized.
        let body = reasoning_body_serializes(
            "https://openrouter.ai/api/v1",
            "any-model",
            Some(ReasoningEffort::Medium),
            ReasoningMode::Auto,
        );
        assert_eq!(
            body.get("reasoning_effort").and_then(|v| v.as_str()),
            Some("medium")
        );
    }

    #[tokio::test]
    async fn test_reasoning_routine_low_and_recovery_high() {
        let low = reasoning_body_serializes(
            "https://openrouter.ai/api/v1",
            "m",
            Some(ReasoningEffort::Low),
            ReasoningMode::Auto,
        );
        assert_eq!(
            low.get("reasoning_effort").and_then(|v| v.as_str()),
            Some("low")
        );
        let high = reasoning_body_serializes(
            "https://openrouter.ai/api/v1",
            "m",
            Some(ReasoningEffort::High),
            ReasoningMode::Auto,
        );
        assert_eq!(
            high.get("reasoning_effort").and_then(|v| v.as_str()),
            Some("high")
        );
    }

    #[tokio::test]
    async fn test_reasoning_off_sends_no_fields() {
        let body = reasoning_body_serializes(
            "https://openrouter.ai/api/v1",
            "m",
            Some(ReasoningEffort::High),
            ReasoningMode::Off,
        );
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("reasoning").is_none());
    }

    #[tokio::test]
    async fn test_reasoning_unsupported_model_sends_no_field_in_auto() {
        let body = reasoning_body_serializes(
            "https://api.openai.com/v1",
            "gpt-4o-mini",
            Some(ReasoningEffort::Medium),
            ReasoningMode::Auto,
        );
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("reasoning").is_none());
    }

    #[tokio::test]
    async fn test_reasoning_supported_openai_model_sends_field() {
        let body = reasoning_body_serializes(
            "https://api.openai.com/v1",
            "gpt-5-mini",
            Some(ReasoningEffort::Medium),
            ReasoningMode::Auto,
        );
        assert_eq!(
            body.get("reasoning_effort").and_then(|v| v.as_str()),
            Some("medium")
        );
        // Never send the legacy nested `reasoning` object alongside.
        assert!(body.get("reasoning").is_none());
    }

    #[tokio::test]
    async fn test_reasoning_unknown_provider_auto_sends_nothing_fixed_sends() {
        let auto_body = reasoning_body_serializes(
            "https://example.invalid/v1",
            "m",
            Some(ReasoningEffort::Medium),
            ReasoningMode::Auto,
        );
        assert!(auto_body.get("reasoning_effort").is_none());
        let fixed_body = reasoning_body_serializes(
            "https://example.invalid/v1",
            "m",
            Some(ReasoningEffort::Medium),
            ReasoningMode::Fixed,
        );
        assert_eq!(
            fixed_body.get("reasoning_effort").and_then(|v| v.as_str()),
            Some("medium")
        );
    }

    #[tokio::test]
    async fn test_reasoning_openrouter_never_sends_nested_reasoning_object() {
        // The old `reasoning: {enabled:true}` / `reasoning.effort` path is gone:
        // only top-level `reasoning_effort` may appear.
        let body = reasoning_body_serializes(
            "https://openrouter.ai/api/v1",
            "grok-4-fast",
            Some(ReasoningEffort::Medium),
            ReasoningMode::Auto,
        );
        assert!(body.get("reasoning").is_none());
        assert_eq!(
            body.get("reasoning_effort").and_then(|v| v.as_str()),
            Some("medium")
        );
    }

    #[tokio::test]
    async fn test_reasoning_usage_recorded_from_response() {
        let server = crate::test_support::HTTP_SERVER_POOL.get_server();
        server.expect(
            Expectation::matching(request::method_path("POST", "/v1/chat/completions"))
                .times(1)
                .respond_with(json_encoded(assistant_done_response_with_usage(Some(30)))),
        );
        let client = OpenAIClient::new(format!("{}/", server.url_str("")), "test-key").unwrap();
        assert!(!client.has_reasoning_usage());
        let msg = chat_tools_once_inner(
            &client,
            "gpt-test",
            &user_message(),
            &[],
            None,
            ReasoningMode::Off,
            None,
        )
        .await
        .expect("usage request");
        assert_eq!(msg.content.as_deref(), Some("done"));
        assert!(client.has_reasoning_usage());
        assert_eq!(client.get_reasoning_tokens_used(), 30);
        assert_eq!(client.get_total_reasoning_tokens_used(), 30);
    }

    #[tokio::test]
    async fn test_reasoning_http_body_contains_effort_for_openrouter() {
        let server = crate::test_support::HTTP_SERVER_POOL.get_server();
        // Verify the actual wire body carries `reasoning_effort` and no
        // nested `reasoning` object.
        server.expect(
            Expectation::matching(all_of![
                request::method_path("POST", "/v1/chat/completions"),
                request::body(matches("\"reasoning_effort\":\"low\"")),
                request::body(not(matches("\"reasoning\":"))),
            ])
            .times(1)
            .respond_with(json_encoded(assistant_done_response())),
        );
        // Point the client at an OpenRouter-shaped base URL is impossible with
        // httptest's local URL, so exercise the serialization contract via a
        // Fixed-mode unknown-provider request (same single-field shape).
        let client = OpenAIClient::new(format!("{}/", server.url_str("")), "test-key").unwrap();
        let msg = chat_tools_once_inner(
            &client,
            "any-model",
            &user_message(),
            &[],
            Some(ReasoningEffort::Low),
            ReasoningMode::Fixed,
            None,
        )
        .await
        .expect("fixed low request");
        assert_eq!(msg.content.as_deref(), Some("done"));
    }

    // --- Retry contract v1 regression tests (HTTP fixtures) ---

    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Clone)]
    struct ScriptedToolResponse {
        status: u16,
        body: serde_json::Value,
        retry_after: Option<String>,
    }

    async fn spawn_scripted_tool_server(
        script: Vec<ScriptedToolResponse>,
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

    fn tool_retry_client(base_url: String, max_retries: usize) -> OpenAIClient {
        use crate::config::LlmConfig;
        OpenAIClient::new(base_url, "test-key")
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

    fn error_body(code: &str) -> serde_json::Value {
        serde_json::json!({"error":{"code":code,"message":"test error"}})
    }

    #[tokio::test]
    async fn tool_request_max_retries_zero_sends_once() {
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_scripted_tool_server(
            vec![ScriptedToolResponse {
                status: 500,
                body: serde_json::json!({"error":"oops"}),
                retry_after: None,
            }],
            counter.clone(),
        )
        .await;
        let client = tool_retry_client(url, 0);
        let err = chat_tools_once(
            &client,
            "gpt-test",
            &user_message(),
            &[],
            None,
            ReasoningMode::Off,
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "max_retries=0 must send exactly once"
        );
        let msg = format!("{err:?}");
        assert!(
            !msg.contains("100"),
            "must not mention hard-coded 100 retries"
        );
    }

    #[tokio::test]
    async fn tool_request_max_retries_two_sends_at_most_three() {
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_scripted_tool_server(
            vec![
                ScriptedToolResponse {
                    status: 500,
                    body: serde_json::json!({"error":"oops"}),
                    retry_after: None,
                },
                ScriptedToolResponse {
                    status: 500,
                    body: serde_json::json!({"error":"oops"}),
                    retry_after: None,
                },
                ScriptedToolResponse {
                    status: 200,
                    body: assistant_done_response(),
                    retry_after: None,
                },
            ],
            counter.clone(),
        )
        .await;
        let client = tool_retry_client(url, 2);
        let msg = chat_tools_once(
            &client,
            "gpt-test",
            &user_message(),
            &[],
            None,
            ReasoningMode::Off,
            None,
            None,
        )
        .await
        .expect("retry then success");
        assert_eq!(msg.content.as_deref(), Some("done"));
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn tool_request_permanent_400_no_retry() {
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_scripted_tool_server(
            vec![ScriptedToolResponse {
                status: 400,
                body: error_body("invalid_request_error"),
                retry_after: None,
            }],
            counter.clone(),
        )
        .await;
        let client = tool_retry_client(url, 3);
        let _ = chat_tools_once(
            &client,
            "gpt-test",
            &user_message(),
            &[],
            None,
            ReasoningMode::Off,
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "permanent 400 must not retry"
        );
    }

    #[tokio::test]
    async fn tool_request_context_overflow_typed_no_retry() {
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_scripted_tool_server(
            vec![ScriptedToolResponse {
                status: 400,
                body: error_body("context_length_exceeded"),
                retry_after: None,
            }],
            counter.clone(),
        )
        .await;
        let client = tool_retry_client(url, 3);
        let err = chat_tools_once(
            &client,
            "gpt-test",
            &user_message(),
            &[],
            None,
            ReasoningMode::Off,
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert!(
            matches!(
                err.downcast_ref::<LlmErrorKind>(),
                Some(LlmErrorKind::ContextLengthExceeded)
            ),
            "must preserve typed ContextLengthExceeded for reactive compaction, got {err:?}"
        );
    }

    #[tokio::test]
    async fn tool_request_auth_no_retry() {
        for status in [401u16, 403] {
            let counter = Arc::new(AtomicUsize::new(0));
            let url = spawn_scripted_tool_server(
                vec![ScriptedToolResponse {
                    status,
                    body: serde_json::json!({"error":"auth"}),
                    retry_after: None,
                }],
                counter.clone(),
            )
            .await;
            let client = tool_retry_client(url, 3);
            let err = chat_tools_once(
                &client,
                "gpt-test",
                &user_message(),
                &[],
                None,
                ReasoningMode::Off,
                None,
                None,
            )
            .await
            .unwrap_err();
            assert_eq!(
                counter.load(Ordering::SeqCst),
                1,
                "status {status} must not retry"
            );
            assert!(
                matches!(
                    err.downcast_ref::<LlmErrorKind>(),
                    Some(LlmErrorKind::Authentication)
                ),
                "status {status} must be Authentication, got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn tool_request_temporary_429_retries() {
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_scripted_tool_server(
            vec![
                ScriptedToolResponse {
                    status: 429,
                    body: error_body("slow_down"),
                    retry_after: Some("0".into()),
                },
                ScriptedToolResponse {
                    status: 200,
                    body: assistant_done_response(),
                    retry_after: None,
                },
            ],
            counter.clone(),
        )
        .await;
        let client = tool_retry_client(url, 3);
        let msg = chat_tools_once(
            &client,
            "gpt-test",
            &user_message(),
            &[],
            None,
            ReasoningMode::Off,
            None,
            None,
        )
        .await
        .expect("transient 429 must retry");
        assert_eq!(msg.content.as_deref(), Some("done"));
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn tool_request_permanent_quota_429_no_retry() {
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_scripted_tool_server(
            vec![ScriptedToolResponse {
                status: 429,
                body: error_body("project_spend_limit_exceeded"),
                retry_after: None,
            }],
            counter.clone(),
        )
        .await;
        let client = tool_retry_client(url, 3);
        let _ = chat_tools_once(
            &client,
            "gpt-test",
            &user_message(),
            &[],
            None,
            ReasoningMode::Off,
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "quota 429 must not retry"
        );
    }

    #[tokio::test]
    async fn tool_request_503_retries_with_retry_after() {
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_scripted_tool_server(
            vec![
                ScriptedToolResponse {
                    status: 503,
                    body: serde_json::json!({"error":"busy"}),
                    retry_after: Some("0".into()),
                },
                ScriptedToolResponse {
                    status: 200,
                    body: assistant_done_response(),
                    retry_after: None,
                },
            ],
            counter.clone(),
        )
        .await;
        let client = tool_retry_client(url, 3);
        let msg = chat_tools_once(
            &client,
            "gpt-test",
            &user_message(),
            &[],
            None,
            ReasoningMode::Off,
            None,
            None,
        )
        .await
        .expect("503 must retry");
        assert_eq!(msg.content.as_deref(), Some("done"));
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn tool_request_cancel_during_retry_delay() {
        use tokio_util::sync::CancellationToken;
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_scripted_tool_server(
            vec![ScriptedToolResponse {
                status: 500,
                body: serde_json::json!({"error":"oops"}),
                retry_after: None,
            }],
            counter.clone(),
        )
        .await;
        use crate::config::LlmConfig;
        let client = OpenAIClient::new(url, "test-key")
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
        let watch_counter = counter.clone();
        tokio::spawn(async move {
            for _ in 0..100 {
                if watch_counter.load(Ordering::SeqCst) >= 1 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            canceller.cancel();
        });
        let messages = user_message();
        let res = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            chat_tools_once(
                &client,
                "gpt-test",
                &messages,
                &[],
                None,
                ReasoningMode::Off,
                Some(token),
                None,
            ),
        )
        .await
        .expect("cancel must finish quickly, not after 30s sleep");
        let err = res.unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<LlmErrorKind>(),
                Some(LlmErrorKind::Cancelled)
            ),
            "must be Cancelled, got {err:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "no second request after cancel"
        );
    }

    #[tokio::test]
    async fn tool_request_exhaustion_message_uses_attempts() {
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_scripted_tool_server(
            vec![ScriptedToolResponse {
                status: 500,
                body: serde_json::json!({"error":"oops"}),
                retry_after: None,
            }],
            counter.clone(),
        )
        .await;
        let client = tool_retry_client(url, 1);
        let err = chat_tools_once(
            &client,
            "gpt-test",
            &user_message(),
            &[],
            None,
            ReasoningMode::Off,
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(
            counter.load(Ordering::SeqCst),
            2,
            "max_retries=1 -> 2 attempts"
        );
        let msg = format!("{err:?}");
        assert!(
            msg.contains("2 attempts"),
            "must report attempts, got {msg}"
        );
        assert!(!msg.contains("100"), "must not mention 100");
    }

    #[tokio::test]
    async fn tool_request_success_records_usage_once() {
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_scripted_tool_server(
            vec![
                ScriptedToolResponse {
                    status: 500,
                    body: serde_json::json!({"error":"oops"}),
                    retry_after: None,
                },
                ScriptedToolResponse {
                    status: 200,
                    body: assistant_done_response_with_usage(Some(10)),
                    retry_after: None,
                },
            ],
            counter.clone(),
        )
        .await;
        let client = tool_retry_client(url, 3);
        let _ = chat_tools_once(
            &client,
            "gpt-test",
            &user_message(),
            &[],
            None,
            ReasoningMode::Off,
            None,
            None,
        )
        .await
        .expect("success");
        assert_eq!(counter.load(Ordering::SeqCst), 2);
        // Usage from the single successful response only; failed attempts
        // carry no usage. Double counting would show 300.
        assert_eq!(client.get_total_tokens_used(), 150);
    }

    #[tokio::test]
    async fn tool_request_error_body_secret_not_in_error() {
        let secret = "SECRET_PROVIDER_BODY_123";
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_scripted_tool_server(
            vec![ScriptedToolResponse {
                status: 400,
                body: serde_json::json!({"error": {"code": "invalid_request", "message": secret}}),
                retry_after: None,
            }],
            counter.clone(),
        )
        .await;
        let client = tool_retry_client(url, 0);
        let err = chat_tools_once(
            &client,
            "gpt-test",
            &user_message(),
            &[],
            None,
            ReasoningMode::Off,
            None,
            None,
        )
        .await
        .unwrap_err();
        let rendered = format!("{err:?} {}", err);
        assert!(
            !rendered.contains(secret),
            "provider error body must not leak, got {rendered}"
        );
        assert!(rendered.contains("400"));
        assert!(rendered.contains("invalid_request"));
    }

    #[tokio::test]
    async fn tool_request_response_summary_hides_tool_arguments() {
        let secret_arg = "SECRET_TOOL_ARGUMENT_76ef";
        let secret_output = "SECRET_MODEL_OUTPUT_cd34";
        let counter = Arc::new(AtomicUsize::new(0));
        let url = spawn_scripted_tool_server(
            vec![ScriptedToolResponse {
                status: 200,
                body: serde_json::json!({
                    "id": "test",
                    "choices": [{
                        "index": 0,
                        "finish_reason": "tool_calls",
                        "message": {
                            "role": "assistant",
                            "content": secret_output,
                            "tool_calls": [{
                                "id": "call-1",
                                "type": "function",
                                "function": {
                                    "name": "custom_tool",
                                    "arguments": format!("{{\"content\":\"{secret_arg}\"}}")
                                }
                            }]
                        }
                    }]
                }),
                retry_after: None,
            }],
            counter.clone(),
        )
        .await;
        let tools = vec![crate::llm::types::ToolDef {
            kind: "function".into(),
            function: crate::llm::types::ToolFunctionDef {
                name: "custom_tool".into(),
                description: "test tool".into(),
                parameters: serde_json::json!({"type": "object"}),
                strict: None,
            },
        }];
        let client = tool_retry_client(url, 0);
        let msg = chat_tools_once(
            &client,
            "gpt-test",
            &user_message(),
            &tools,
            None,
            ReasoningMode::Off,
            None,
            None,
        )
        .await
        .expect("tool response");
        // Display path preserves content.
        assert_eq!(msg.content.as_deref(), Some(secret_output));
        assert!(msg.tool_calls[0].function.arguments.contains(secret_arg));
        // Diagnostic summary must not retain either secret.
        let summary = crate::llm::telemetry::summarize_response_message(
            msg.content.as_deref(),
            msg.tool_calls.len(),
            msg.refusal.as_deref(),
            false,
            Some("tool_calls"),
            1024,
            1,
        );
        let rendered = format!("{summary:?}");
        assert!(!rendered.contains(secret_arg));
        assert!(!rendered.contains(secret_output));
        assert_eq!(summary.tool_call_count, 1);
    }
}
