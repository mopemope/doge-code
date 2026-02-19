use crate::llm::LlmErrorKind;
use crate::llm::chat_with_tools::{ChatResponseWithTools, ChoiceMessageWithTools, Reasoning};
use crate::llm::client_core::OpenAIClient;
use crate::llm::message_utils::clean_json_text;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<Reasoning>, // OpenRouter reasoning parameter
}

pub async fn chat_tools_once(
    client: &OpenAIClient,
    model: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    cancel: Option<tokio_util::sync::CancellationToken>,
    ui_tx: Option<Sender<String>>,
) -> Result<ChoiceMessageWithTools> {
    const MAX_RETRIES: u64 = 100;
    const MAX_TIMEOUT_RETRIES: u64 = 20;
    let mut last_error = anyhow!("Failed after {} retries", MAX_RETRIES);
    let mut timeout_retries = 0u64;

    for attempt in 1..=MAX_RETRIES {
        match chat_tools_once_inner(client, model, messages, tools, cancel.clone()).await {
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

async fn chat_tools_once_inner(
    client: &OpenAIClient,
    model: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> Result<ChoiceMessageWithTools> {
    use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap};

    let url = client.endpoint();
    let reasoning_effort = client.reason_enable.then_some("high");

    let reasoning = if model.contains("grok-4-fast") {
        Some(Reasoning {
            effort: None,
            max_tokens: None,
            enabled: Some(true),
        })
    } else {
        None
    };
    let req = ChatRequestWithToolsRef {
        model,
        messages,
        temperature: None,
        tools: (!tools.is_empty()).then_some(tools),
        tool_choice: None,
        reasoning_effort,
        reasoning,
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

    // Track token usage if available
    if let Some(usage) = &body.usage {
        client.set_tokens(usage.total_tokens);
        // Also track prompt tokens for non-streaming tools path
        client.set_prompt_tokens(usage.prompt_tokens);
    }

    let msg = body
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no choices"))?;

    debug!("llm response message {:?}", msg);
    Ok(msg.message)
}
