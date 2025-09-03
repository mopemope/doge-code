use crate::llm::LlmErrorKind;
use crate::llm::chat_with_tools::{
    ChatRequestWithTools, ChatResponseWithTools, ChoiceMessageWithTools,
};
use crate::llm::client_core::OpenAIClient;
use crate::llm::types::ChatMessage;
use anyhow::{Result, anyhow};
use tokio::time::{Duration, sleep};
use tracing::{debug, error, warn};

pub async fn chat_tools_once(
    client: &OpenAIClient,
    model: &str,
    messages: Vec<ChatMessage>,
    tools: &[crate::llm::types::ToolDef],
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> Result<ChoiceMessageWithTools> {
    const MAX_RETRIES: u32 = 3;
    let mut last_error = anyhow!("Failed after {} retries", MAX_RETRIES);

    for attempt in 1..=MAX_RETRIES {
        match chat_tools_once_inner(client, model, messages.clone(), tools, cancel.clone()).await {
            Ok(result) => return Ok(result),
            Err(e) => {
                last_error = e;
                if attempt >= MAX_RETRIES {
                    break;
                }
                // Exponential backoff with jitter
                let delay_ms = (2_u64.pow(attempt) * 1000).min(60_000); // Max 60 seconds
                let jitter = rand::random::<u64>() % 1000; // Add up to 1 second of jitter
                let total_delay = Duration::from_millis(delay_ms + jitter);
                warn!(
                    attempt = attempt,
                    delay_ms = delay_ms + jitter,
                    "Retrying chat_tools_once after error"
                );
                sleep(total_delay).await;
            }
        }
    }

    Err(last_error)
}

async fn chat_tools_once_inner(
    client: &OpenAIClient,
    model: &str,
    messages: Vec<ChatMessage>,
    tools: &[crate::llm::types::ToolDef],
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> Result<ChoiceMessageWithTools> {
    use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap};

    let url = client.endpoint();
    let reasoning_effort = if client.reason_enable {
        Some("high".to_owned())
    } else {
        None
    };
    let req = ChatRequestWithTools {
        model: model.to_string(),
        messages,
        temperature: None,
        tools: Some(tools.to_vec()),
        tool_choice: None,
        reasoning_effort,
    };

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());
    headers.insert(
        AUTHORIZATION,
        format!("Bearer {}", client.api_key).parse().unwrap(),
    );

    if let Ok(payload) = serde_json::to_string_pretty(&req) {
        debug!(payload=%payload, endpoint=%url, "sending chat.completions (tools) payload");
    }

    let cancel_token = cancel.unwrap_or_default();
    let req_builder = client.inner.post(&url).headers(headers).json(&req);

    let resp = tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            warn!("chat_tools_once cancelled before send");
            return Err(anyhow!(LlmErrorKind::Cancelled));
        }
        res = req_builder.send() => res?,
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        error!(status=%status.as_u16(), body=%text, "llm chat_tools_once non-success status");
        return Err(anyhow!("chat (tools) error: {} - {}", status, text));
    }

    let response_text: String = tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            warn!("chat_tools_once cancelled during body read");
            return Err(anyhow!(LlmErrorKind::Cancelled));
        }
        res = resp.text() => res?,
    };

    debug!(response_body=%response_text, "llm chat_tools_once response");
    let body: ChatResponseWithTools = serde_json::from_str(&response_text)?;

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
