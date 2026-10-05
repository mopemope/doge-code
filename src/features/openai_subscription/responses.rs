use super::{
    ProviderError, ProviderState,
    auth::AuthHandle,
    sse::{Decoder, InferenceEvent},
};
use crate::{
    config::ReasoningEffort,
    llm::{ChatMessage, ChoiceMessageWithTools, ToolCall, ToolCallFunction, ToolDef, Usage},
};
use anyhow::{Context, Result, bail};
use futures::StreamExt;
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Serialize)]
pub struct Request {
    model: String,
    pub(crate) input: Vec<Value>,
    pub(crate) tools: Vec<Value>,
    store: bool,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_management: Option<Vec<ContextManagement>>,
}

/// Server-side native compaction directive for `POST /responses`.
///
/// Sent as `context_management: [{type: "compaction", compact_threshold}]`
/// with `store:false` / `stream:true` preserved. Never interpret, summarize,
/// or rewrite the opaque compaction state server-side produces.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ContextManagement {
    #[serde(rename = "type")]
    kind: &'static str,
    compact_threshold: u32,
}

impl ContextManagement {
    pub fn compaction(compact_threshold: u32) -> Self {
        Self {
            kind: "compaction",
            compact_threshold,
        }
    }
}

/// Minimum `compact_threshold` accepted by the Responses API.
pub const RESPONSES_MIN_COMPACT_THRESHOLD: u32 = 1_000;

/// Validate a resolved native-compaction threshold without silent clamping.
pub fn validate_compact_threshold(threshold: u32) -> Result<()> {
    if threshold < RESPONSES_MIN_COMPACT_THRESHOLD {
        bail!(
            "Responses native compaction threshold {threshold} is below the API minimum {}; lower the configured auto-compaction threshold is not supported for openai-chatgpt",
            RESPONSES_MIN_COMPACT_THRESHOLD
        );
    }
    Ok(())
}

/// True when a Responses output item is an opaque server-side compaction item.
pub fn is_compaction_item(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("compaction")
}

fn validate_compaction_item(item: &Value) -> Result<()> {
    if item.get("type").and_then(Value::as_str) != Some("compaction") {
        bail!("Responses compaction item must have type compaction");
    }
    let encrypted = item.get("encrypted_content").and_then(Value::as_str);
    if encrypted.is_none_or(|s| s.is_empty()) {
        bail!("Responses compaction item missing encrypted_content");
    }
    if let Some(id) = item.get("id")
        && !id.is_string()
    {
        bail!("Responses compaction item id must be a string");
    }
    Ok(())
}

/// Index of the latest compaction item in a Responses output array, if any.
pub fn latest_compaction_index(output: &[Value]) -> Option<usize> {
    output.iter().rposition(is_compaction_item)
}

/// Canonical continuation output: everything from the latest compaction item
/// onward (inclusive). Items before the boundary are not needed for
/// continuation and must not be replayed or dispatched from.
pub fn canonical_output(output: &[Value]) -> &[Value] {
    match latest_compaction_index(output) {
        Some(index) => &output[index..],
        None => output,
    }
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponsesRequest")
            .field("model", &self.model)
            .field("input_items", &self.input.len())
            .field("tool_namespaces", &self.tools.len())
            .finish_non_exhaustive()
    }
}

pub fn build(
    model: &str,
    account: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    effort: Option<ReasoningEffort>,
    compact_threshold: Option<u32>,
) -> Result<Request> {
    if let Some(threshold) = compact_threshold {
        validate_compact_threshold(threshold)?;
    }
    let mut input = Vec::new();
    let mut calls = std::collections::HashSet::new();
    let mut outputs = std::collections::HashSet::new();
    for message in messages {
        if let Some(state) = &message.provider_state {
            if state.version != 1 || state.account != account || state.model != model {
                bail!(
                    "Responses session belongs to a different account or model; start a new session or restore its original selection"
                );
            }
            if message.role != "assistant" {
                bail!("provider state is only valid on assistant turns");
            }
            for item in &state.output {
                validate_output(item)?;
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    let id = item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .context("function call ID missing")?;
                    if !calls.insert(id.to_owned()) {
                        bail!("duplicate function call ID");
                    }
                }
                input.push(item.clone());
            }
            continue;
        }
        match message.role.as_str() {
            "system" | "developer" | "user" | "assistant" => {
                if let Some(content) = &message.content {
                    input.push(json!({"role": if message.role == "system" { "developer" } else { &message.role }, "content": content}));
                }
                for call in &message.tool_calls {
                    let id = call
                        .id
                        .as_deref()
                        .context("tool call ID missing in history")?;
                    if !calls.insert(id.to_owned()) {
                        bail!("duplicate tool call ID");
                    }
                    input.push(json!({"type":"function_call","name":call.function.name,"namespace":"dgc","arguments":call.function.arguments,"call_id":id}));
                }
            }
            "tool" => {
                let id = message
                    .tool_call_id
                    .as_deref()
                    .context("tool result call ID missing")?;
                if !calls.contains(id) || !outputs.insert(id.to_owned()) {
                    bail!("orphan or duplicate tool result");
                }
                input.push(json!({"type":"function_call_output","call_id":id,"output":message.content.as_deref().unwrap_or("")}));
            }
            _ => bail!("unsupported conversation role"),
        }
    }
    let functions: Vec<_> = tools.iter().map(|tool| json!({"type":"function", "name":tool.function.name, "description":tool.function.description, "parameters":tool.function.parameters, "strict":tool.function.strict.unwrap_or(false)})).collect();
    Ok(Request {
        model: model.to_owned(),
        input,
        tools: if functions.is_empty() {
            vec![]
        } else {
            vec![
                json!({"type":"namespace","name":"dgc","description":"doge-code local tools","tools":functions}),
            ]
        },
        store: false,
        stream: true,
        reasoning: effort.map(|e| json!({"effort":e.as_api_str()})),
        context_management: compact_threshold
            .map(|threshold| vec![ContextManagement::compaction(threshold)]),
    })
}

fn validate_output(item: &Value) -> Result<()> {
    // Opaque server-side compaction items carry no `status`; they must be
    // accepted without interpreting the ciphertext.
    if is_compaction_item(item) {
        return validate_compaction_item(item);
    }
    if item
        .get("status")
        .is_some_and(|status| status != "completed")
    {
        bail!("Responses output item was not completed; no tools were executed");
    }
    match item.get("type").and_then(Value::as_str) {
        Some("message") => {
            if item.get("role").and_then(Value::as_str) != Some("assistant") {
                bail!("Responses output message must have assistant role");
            }
            Ok(())
        }
        Some("reasoning") => Ok(()),
        Some("function_call") => {
            if item
                .get("namespace")
                .and_then(Value::as_str)
                .is_some_and(|v| v != "dgc")
            {
                bail!("unrecognized Responses function namespace");
            }
            if item
                .get("call_id")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .is_none()
            {
                bail!("Responses function call ID missing");
            }
            Ok(())
        }
        _ => bail!("unsupported Responses output item; no tools were executed"),
    }
}

pub fn completed(
    response: &Value,
    account: &str,
    model: &str,
    tools: &[ToolDef],
) -> Result<(ChoiceMessageWithTools, Option<Usage>)> {
    if response.get("status").and_then(Value::as_str) != Some("completed") {
        bail!("Responses terminal status was not completed");
    }
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .context("Responses output missing")?;
    // The latest compaction item is the canonical continuation boundary.
    // Only items at/after it are validated, dispatched, and persisted.
    // Pre-boundary tool calls must never dispatch from this response.
    let canonical: Vec<Value> = canonical_output(output).to_vec();
    let mut text = String::new();
    let mut refusal = None;
    let mut tool_calls = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for item in &canonical {
        validate_output(item)?;
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                if let Some(content) = item.get("content").and_then(Value::as_array) {
                    for part in content {
                        match part.get("type").and_then(Value::as_str) {
                            Some("output_text") => text
                                .push_str(part.get("text").and_then(Value::as_str).unwrap_or("")),
                            Some("refusal") => {
                                refusal = Some(
                                    part.get("refusal")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .to_owned(),
                                )
                            }
                            _ => bail!("unsupported Responses message content"),
                        }
                    }
                }
            }
            Some("function_call") => {
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .context("Responses tool name missing")?;
                if !tools.iter().any(|t| t.function.name == name) {
                    bail!("Responses requested a tool outside the active catalog");
                }
                let id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .context("Responses call ID missing")?;
                if !ids.insert(id) {
                    bail!("duplicate Responses call ID");
                }
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .context("Responses arguments missing")?;
                let value: Value = serde_json::from_str(arguments)
                    .map_err(|_| anyhow::anyhow!("invalid Responses function arguments"))?;
                if !value.is_object() {
                    bail!("Responses function arguments must be an object");
                }
                tool_calls.push(ToolCall {
                    id: Some(id.to_owned()),
                    r#type: "function".into(),
                    function: ToolCallFunction {
                        name: name.to_owned(),
                        arguments: arguments.to_owned(),
                    },
                });
            }
            _ => {}
        }
    }
    let usage = response.get("usage").filter(|v| !v.is_null()).map(|value| {
        let mapped = json!({"prompt_tokens":value.get("input_tokens"),"completion_tokens":value.get("output_tokens"),"total_tokens":value.get("total_tokens"),"prompt_tokens_details":value.get("input_tokens_details"),"completion_tokens_details":value.get("output_tokens_details")});
        serde_json::from_value::<Usage>(mapped).context("invalid Responses usage")
    }).transpose()?;
    Ok((
        ChoiceMessageWithTools {
            refusal,
            role: "assistant".into(),
            content: Some(text),
            tool_calls,
            provider_state: Some(ProviderState {
                version: 1,
                account: account.to_owned(),
                model: model.to_owned(),
                output: canonical,
            }),
        },
        usage,
    ))
}

pub async fn infer(
    client: &crate::llm::OpenAIClient,
    auth: &AuthHandle,
    model: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    effort: Option<ReasoningEffort>,
    cancel: CancellationToken,
) -> Result<ChoiceMessageWithTools> {
    let compact_threshold = client.responses_compact_threshold();
    let request = build(
        model,
        &auth.account,
        messages,
        tools,
        effort,
        compact_threshold,
    )?;
    if tracing::enabled!(tracing::Level::DEBUG) {
        tracing::debug!(
            responses_native_compaction_enabled = compact_threshold.is_some(),
            responses_compact_threshold = compact_threshold.unwrap_or(0),
            "responses request contract"
        );
    }
    let attempts = client.llm_cfg.max_retries.min(3) + 1;
    let deadline = tokio::time::Instant::now()
        + Duration::from_millis(client.llm_cfg.request_timeout_ms.max(1));
    let result = async {
        for attempt in 0..attempts {
            let bearer = auth.bearer(&cancel).await?;
            client.record_request_attempt();
            let response = auth
                .http
                .post(format!("{}/responses", auth.resource))
                // Override the shorter OAuth timeout for long-running inference.
                // The outer deadline includes refresh and all retry attempts.
                .timeout(Duration::from_millis(
                    client.llm_cfg.request_timeout_ms.max(1),
                ))
                .bearer_auth(bearer)
                .json(&request)
                .send()
                .await
                .map_err(|_| {
                    anyhow::anyhow!("Responses connection failed; no automatic replay was made")
                })?;
            let request_id = response
                .headers()
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            if !response.status().is_success() {
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(1)
                    .min(30);
                let error = match super::auth::json_response(response).await {
                    Err(e) => e,
                    Ok(_) => anyhow::anyhow!("unexpected Responses HTTP status"),
                };
                if error
                    .downcast_ref::<ProviderError>()
                    .is_some_and(|e| e.retryable)
                    && attempt + 1 < attempts
                {
                    tokio::time::sleep(Duration::from_secs(
                        retry_after.saturating_mul(attempt as u64 + 1).min(30),
                    ))
                    .await;
                    continue;
                }
                return Err(error);
            }
            let mut stream = response.bytes_stream();
            let mut decoder = Decoder::default();
            let mut total = 0usize;
            while let Some(chunk) = tokio::time::timeout(
                Duration::from_millis(client.llm_cfg.read_idle_timeout_ms.max(1)),
                stream.next(),
            )
            .await
            .context("Responses stream idle timeout")?
            {
                let bytes = chunk.map_err(|_| {
                    anyhow::anyhow!("Responses stream interrupted; no automatic replay was made")
                })?;
                total = total.saturating_add(bytes.len());
                if total > 32 * 1024 * 1024 {
                    bail!("Responses stream exceeds size limit");
                }
                for event in decoder.push(&bytes)? {
                    let event: InferenceEvent = serde_json::from_value(event)
                        .map_err(|_| anyhow::anyhow!("invalid Responses event fields"))?;
                    match event {
                        InferenceEvent::Completed { response } => {
                            let (result, usage) =
                                completed(&response, &auth.account, model, tools)?;
                            if let Some(usage) = usage {
                                client.record_usage(&usage);
                            }
                            return Ok(result);
                        }
                        InferenceEvent::Failed { response } => {
                            return Err(ProviderError::from_body(
                                None,
                                &response,
                                request_id.clone(),
                            )
                            .during_stream()
                            .into());
                        }
                        InferenceEvent::Error { body } => {
                            return Err(ProviderError::from_body(
                                None,
                                &Value::Object(body),
                                request_id.clone(),
                            )
                            .during_stream()
                            .into());
                        }
                        InferenceEvent::Incomplete => {
                            bail!("Responses generation incomplete; no tools were executed")
                        }
                        _ => {} // Deltas are advisory. Only completed output authorizes tool dispatch.
                    }
                }
            }
            bail!("Responses stream ended without response.completed; no tools were executed");
        }
        bail!("Responses retry budget exhausted")
    };
    tokio::select! { biased; _ = cancel.cancelled() => bail!(crate::llm::LlmErrorKind::Cancelled), result = tokio::time::timeout_at(deadline, result) => result.context("Responses request deadline exceeded")? }
}
