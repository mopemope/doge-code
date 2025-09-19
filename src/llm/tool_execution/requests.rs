use crate::llm::LlmErrorKind;
use crate::llm::chat_with_tools::{
    ChatRequestWithTools, ChatResponseWithTools, ChoiceMessageWithTools,
};
use crate::llm::client_core::OpenAIClient;
use crate::llm::types::ChatMessage;
use anyhow::{Result, anyhow};
use std::ops::Mul;
use tokio::time::{Duration, sleep};
use tracing::{debug, error, warn};

pub async fn chat_tools_once(
    client: &OpenAIClient,
    model: &str,
    messages: Vec<ChatMessage>,
    tools: &[crate::llm::types::ToolDef],
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> Result<ChoiceMessageWithTools> {
    const MAX_RETRIES: u64 = 30;
    const MAX_TIMEOUT_RETRIES: u64 = 1; // Only retry once on timeout
    let mut last_error = anyhow!("Failed after {} retries", MAX_RETRIES);
    let mut timeout_retries = 0u64;

    for attempt in 1..=MAX_RETRIES {
        match chat_tools_once_inner(client, model, messages.clone(), tools, cancel.clone()).await {
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
                }

                // Exponential backoff with jitter
                let delay_ms = (2_u64.mul(attempt) * 1000).min(60_000); // Max 60 seconds
                let jitter = rand::random::<u64>() % 5000; // Add up to 5 second of jitter
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
    headers.insert(
        "HTTP-Refer",
        "https://github.com/mopemope/doge-code".parse().unwrap(),
    );
    headers.insert("X-Title", "Doge-Code".parse().unwrap());
    headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());
    headers.insert(
        AUTHORIZATION,
        format!("Bearer {}", client.api_key).parse().unwrap(),
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
