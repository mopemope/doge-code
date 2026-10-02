use crate::config::{ReasoningEffort, ReasoningMode};
use crate::llm::LlmErrorKind;
use crate::llm::chat_with_tools::{ChatResponseWithTools, ChoiceMessageWithTools};
use crate::llm::client_core::OpenAIClient;
use crate::llm::message_utils::clean_json_text;
use crate::llm::reasoning::resolve_reasoning_hint;
use crate::llm::types::{ChatMessage, ToolDef};
use anyhow::{Result, anyhow};
use serde::Serialize;
use std::ops::Mul;
use std::sync::mpsc::Sender;
use tokio::time::{Duration, sleep};
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
    const MAX_RETRIES: u64 = 100;
    const MAX_TIMEOUT_RETRIES: u64 = 20;
    let mut last_error = anyhow!("Failed after {} retries", MAX_RETRIES);
    let mut timeout_retries = 0u64;

    for attempt in 1..=MAX_RETRIES {
        match chat_tools_once_inner(
            client,
            model,
            messages,
            tools,
            reasoning_effort,
            reasoning_mode,
            cancel.clone(),
        )
        .await
        {
            Ok(result) => return Ok(result),
            Err(e) => {
                last_error = e;
                // Check if the error is a timeout
                let is_timeout = last_error.to_string().contains("timed out")
                    || matches!(
                        last_error.downcast_ref::<LlmErrorKind>(),
                        Some(LlmErrorKind::Timeout)
                    );

                // If it's a timeout, limit retries
                if is_timeout {
                    timeout_retries += 1;
                    if timeout_retries > MAX_TIMEOUT_RETRIES {
                        error!("Timeout error occurred: {:?}", &last_error);
                        break;
                    }
                } else if attempt >= MAX_RETRIES {
                    error!("Error occurred: {:?}", &last_error);
                    break;
                } else if matches!(
                    last_error.downcast_ref::<LlmErrorKind>(),
                    Some(LlmErrorKind::Deserialize)
                ) {
                    error!("Deserialization error, not retrying: {:?}", &last_error);
                    break;
                } else if matches!(
                    last_error.downcast_ref::<LlmErrorKind>(),
                    Some(LlmErrorKind::Authentication)
                ) {
                    error!("Authentication error, not retrying: {:?}", &last_error);
                    break;
                }

                // Exponential backoff with jitter
                let delay_ms = (2_u64.mul(attempt) * 1000).min(60_000);
                let jitter = rand::random::<u64>() % 5000;
                let total_delay = Duration::from_millis(delay_ms + jitter);
                warn!(
                    attempt = attempt,
                    delay_ms = delay_ms + jitter,
                    "Retrying chat_tools_once after error"
                );
                if let Some(ref tx) = ui_tx {
                    let _ = tx.send(format!(
                        "::status:waiting:Retrying request (Attempt {}/{})...",
                        attempt + 1,
                        MAX_RETRIES
                    ));
                }
                sleep(total_delay).await;
            }
        }
    }

    Err(last_error)
}

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
    use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap};

    let url = client.endpoint();
    // Provider gating lives at the HTTP boundary: the agent loop decides the
    // policy effort, this layer decides whether it may be serialized.
    let resolved =
        resolve_reasoning_hint(&client.base_url, model, &reasoning_mode, reasoning_effort);
    let reasoning_effort_str = resolved.map(|e| e.as_api_str());
    let hint_sent = reasoning_effort_str.is_some();
    debug!(
        reasoning_mode = reasoning_mode.as_str(),
        reasoning_effort = reasoning_effort.map(|e| e.as_api_str()),
        reasoning_hint_sent = hint_sent,
        endpoint = %url,
        "reasoning decision for chat_tools_once",
    );

    let req = ChatRequestWithToolsRef {
        model,
        messages,
        temperature: None,
        tools: (!tools.is_empty()).then_some(tools),
        tool_choice: None,
        reasoning_effort: reasoning_effort_str,
    };

    let mut headers = HeaderMap::new();
    headers.insert(
        "HTTP-Referer",
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

    // if let Ok(payload) = serde_json::to_string_pretty(&req) {
    //     debug!(payload=%payload, endpoint=%url, "sending chat.completions (tools) payload");
    // }
    // if let Ok(messages) = serde_json::to_string_pretty(&req.messages) {
    //     debug!(messages=%messages, endpoint=%url, "sending chat.completions (tools) messages");
    // }

    let cancel_token = cancel.unwrap_or_default();
    let req_builder = client.inner.post(&url).headers(headers).json(&req);

    // Set timeout for the request
    let timeout_duration = Duration::from_millis(client.llm_cfg.timeout_ms);
    let resp_fut = tokio::time::timeout(timeout_duration, req_builder.send());

    let resp_result = tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            warn!("chat_tools_once cancelled before send");
            return Err(anyhow!(LlmErrorKind::Cancelled));
        }
        res = resp_fut => {
            match res {
                Ok(Ok(resp)) => Ok(resp),
                Ok(Err(e)) => Err(anyhow::Error::new(e).context("send chat request (tools)")),
                Err(_) => Err(anyhow!(LlmErrorKind::Timeout)),
            }
        }
    };

    let resp = match resp_result {
        Ok(resp) => resp,
        Err(e) => return Err(e),
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default().trim().to_owned();
        error!(status=%status.as_u16(), body=%text, "llm chat_tools_once non-success status");

        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(anyhow!(LlmErrorKind::Authentication)
                .context(format!("chat (tools) auth error: {} - {}", status, text)));
        }

        // Check if the error is due to context length exceeded
        if status.as_u16() == 400
            && let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
            && let Some(code) = json
                .get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_str())
            && code == "context_length_exceeded"
        {
            return Err(anyhow!(LlmErrorKind::ContextLengthExceeded));
        }

        return Err(anyhow!("chat (tools) error: {} - {}", status, text));
    }

    // Set timeout for reading the response body
    let response_text_fut = tokio::time::timeout(timeout_duration, resp.text());

    let response_text_result: Result<String, anyhow::Error> = tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            warn!("chat_tools_once cancelled during body read");
            Err(anyhow!(LlmErrorKind::Cancelled))
        }
        res = response_text_fut => {
            match res {
                Ok(Ok(text)) => Ok(text),
                Ok(Err(e)) => Err(anyhow::Error::new(e).context("read chat response body (tools)")),
                Err(_) => Err(anyhow!(LlmErrorKind::Timeout)),
            }
        }
    };

    let response_text: String = match response_text_result {
        Ok(text) => text.trim().to_owned(),
        Err(e) => return Err(e),
    };

    debug!(response_body=%response_text, "llm chat_tools_once response");
    // Clean JSON text (remove markdown code blocks if present)
    let cleaned_text = clean_json_text(&response_text);
    let body: ChatResponseWithTools = serde_json::from_str(&cleaned_text)
        .map_err(|e| anyhow!(LlmErrorKind::Deserialize).context(e))?;

    // Track token usage if available (including reasoning details when present).
    if let Some(usage) = &body.usage {
        client.record_usage(usage);
    }

    let msg = body
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no choices"))?;

    debug!("llm response message {:?}", msg);
    Ok(msg.message)
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
    use httptest::{Expectation, ServerBuilder, matchers::*, responders::*};

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
            role: "user".into(),
            content: Some("do the thing".into()),
            tool_calls: vec![],
            tool_call_id: None,
        }]
    }

    fn start_server() -> Option<httptest::Server> {
        if std::env::var("DOGE_SKIP_HTTPTEST").is_ok() {
            eprintln!("Skipping httptest-based test (DOGE_SKIP_HTTPTEST set)");
            return None;
        }
        match ServerBuilder::new().run() {
            Ok(server) => Some(server),
            Err(err) => {
                eprintln!("Skipping httptest-based test (server start failed: {err})");
                None
            }
        }
    }

    fn test_client_for(server: &httptest::Server) -> OpenAIClient {
        OpenAIClient::new(format!("{}/", server.url_str("")), "test-key").unwrap()
    }

    #[tokio::test]
    async fn test_first_payload_defers_remote_and_builtin_schemas() {
        let Some(server) = start_server() else { return };
        let catalog = deferred_fixture_catalog();
        // Sanity: remotes and non-core builtins really are deferred here.
        assert!(!catalog.is_active("mcp_github_get_pull_request").await);
        assert!(!catalog.is_active("edit").await);
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
                "execute_process",
                "fs_read",
                "observation_read",
                "search_repomap",
                "search_text",
                "task",
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
        let Some(server) = start_server() else { return };
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
        let Some(server) = start_server() else { return };
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
        let Some(server) = start_server() else { return };
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
}
