use anyhow::Result;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, RETRY_AFTER};
use serde_json;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use super::OpenAIClient;
use crate::llm::LlmErrorKind;
use crate::llm::types::{ChatMessage, ChatRequest, ChatResponse, ChoiceMessage};

pub async fn chat_once(
    client: &OpenAIClient,
    model: &str,
    messages: Vec<ChatMessage>,
    cancel: Option<CancellationToken>,
) -> Result<ChoiceMessage> {
    let url = client.endpoint();
    let req = ChatRequest {
        model: model.to_string(),
        messages,
        temperature: None,
        stream: None,
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

    if let Ok(payload) = serde_json::to_string_pretty(&req) {
        debug!(payload=%payload, endpoint=%url, "sending chat.completions payload");
    }

    let max_attempts = client.llm_cfg.max_retries.saturating_add(1);
    let mut last_err: Option<anyhow::Error> = None;

    let cancel_token = cancel.unwrap_or_default();

    for attempt in 1..=max_attempts {
        let req_builder = client.inner.post(&url).headers(headers.clone()).json(&req);

        let resp_res = tokio::select! {
            biased;
            _ = cancel_token.cancelled() => {
                info!("chat_once cancelled before send");
                return Err(anyhow::anyhow!(LlmErrorKind::Cancelled));
            }
            res = req_builder.send() => res,
        };

        match resp_res {
            Err(e) => {
                error!(attempt, err=%e, "llm chat_once send error");
                last_err = Some(anyhow::Error::new(e).context("send chat request"));
            }
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    let retry_after = resp
                        .headers()
                        .get(RETRY_AFTER)
                        .and_then(|h| h.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok());

                    let text = tokio::select! {
                        biased;
                        _ = cancel_token.cancelled() => {
                            info!("chat_once cancelled during error body read");
                            return Err(anyhow::anyhow!(LlmErrorKind::Cancelled));
                        }
                        res = resp.text() => res.unwrap_or_default(),
                    };

                    error!(attempt, status=%status.as_u16(), body=%text, "llm chat_once non-success status");
                    let e = anyhow::anyhow!("chat error: {} - {}", status, text);
                    let kind = crate::llm::classify_error(Some(status), &e);
                    if should_retry(kind.clone()) && attempt < max_attempts {
                        let wait = backoff_delay(client, attempt, retry_after);
                        info!(attempt, kind=?kind, wait_ms=%wait.as_millis(), "retrying chat_once");
                        tokio::select! {
                            biased;
                            _ = cancel_token.cancelled() => {
                                info!("chat_once cancelled during retry sleep");
                                return Err(anyhow::anyhow!(LlmErrorKind::Cancelled));
                            }
                            _ = tokio::time::sleep(wait) => {}
                        }
                        continue;
                    } else {
                        return Err(e);
                    }
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
                        error!(attempt, err=%e, "llm chat_once read body error");
                        last_err = Some(anyhow::Error::new(e).context("read chat response body"));
                        let kind = crate::llm::classify_error(None, last_err.as_ref().unwrap());
                        if should_retry(kind.clone()) && attempt < max_attempts {
                            let wait = backoff_delay(client, attempt, None);
                            warn!(attempt, kind=?kind, "retrying after body read error");
                            tokio::select! {
                                biased;
                                _ = cancel_token.cancelled() => {
                                    info!("chat_once cancelled during retry sleep");
                                    return Err(anyhow::anyhow!(LlmErrorKind::Cancelled));
                                }
                                _ = tokio::time::sleep(wait) => {}
                            }
                            continue;
                        } else {
                            return Err(last_err.unwrap());
                        }
                    }
                };

                debug!("llm chat_once response");

                let body: Result<ChatResponse, _> = serde_json::from_str(&response_text);
                match body {
                    Ok(body) => {
                        // Track token usage if available
                        if let Some(usage) = &body.usage {
                            client.set_tokens(usage.total_tokens);
                            // Also track prompt tokens for non-streaming path so UI can display header info
                            client.set_prompt_tokens(usage.prompt_tokens);
                        }

                        if let Some(msg) = body.choices.into_iter().next().map(|c| c.message) {
                            return Ok(msg);
                        } else {
                            let e = anyhow::anyhow!("no choices returned");
                            return Err(e);
                        }
                    }
                    Err(e) => {
                        let kind = LlmErrorKind::Deserialize;
                        error!(attempt, err=%e, "llm chat_once deserialize error");
                        if should_retry(kind.clone()) && attempt < max_attempts {
                            let wait = backoff_delay(client, attempt, None);
                            warn!(attempt, kind=?kind, "retrying after deserialize error");
                            tokio::select! {
                                biased;
                                _ = cancel_token.cancelled() => {
                                    info!("chat_once cancelled during retry sleep");
                                    return Err(anyhow::anyhow!(LlmErrorKind::Cancelled));
                                }
                                _ = tokio::time::sleep(wait) => {}
                            }
                            last_err = Some(anyhow::Error::new(e).context("parse chat response"));
                            continue;
                        } else {
                            return Err(anyhow::Error::new(e).context("parse chat response"));
                        }
                    }
                }
            }
        }

        if attempt < max_attempts {
            let kind = crate::llm::classify_error(None, last_err.as_ref().unwrap());
            if should_retry(kind.clone()) {
                let wait = backoff_delay(client, attempt, None);
                info!(attempt, kind=?kind, wait_ms=%wait.as_millis(), "retrying chat_once");
                tokio::select! {
                    biased;
                    _ = cancel_token.cancelled() => {
                        info!("chat_once cancelled during retry sleep");
                        return Err(anyhow::anyhow!(LlmErrorKind::Cancelled));
                    }
                    _ = tokio::time::sleep(wait) => {}
                }
                continue;
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("unknown error")))
}

pub(crate) fn should_retry(kind: LlmErrorKind) -> bool {
    matches!(
        kind,
        LlmErrorKind::RateLimited
            | LlmErrorKind::Server
            | LlmErrorKind::Network
            | LlmErrorKind::Timeout
    )
}

pub(crate) fn backoff_delay(
    client: &OpenAIClient,
    attempt: usize,
    retry_after_secs: Option<u64>,
) -> Duration {
    if client.llm_cfg.respect_retry_after
        && let Some(secs) = retry_after_secs
    {
        return Duration::from_secs(secs);
    }
    let base = client.llm_cfg.retry_base_ms;
    let exp = base.saturating_mul(1u64 << (attempt as u32 - 1));
    let jitter = client.llm_cfg.retry_jitter_ms as i64;
    let half = jitter / 2;
    let rnd = fastrand::i64(-half..=half).max(0) as u64;
    Duration::from_millis(exp.saturating_add(rnd))
}
