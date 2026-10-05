//! Shared LLM request retry policy (single source of truth).
//!
//! All OpenAI-compatible `POST /v1/chat/completions` paths must use this
//! module so `[llm] max_retries / retry_base_ms / retry_jitter_ms /
//! respect_retry_after` cannot drift apart again.
//!
//! Contract:
//! - `max_retries` = additional retries allowed after the initial attempt.
//! - `max_attempts = max_retries + 1` (overflow-safe).
//! - Only transient failures are retried (bounded). Permanent 4xx,
//!   cancellation, serialization errors and `context_length_exceeded` fail
//!   fast so the agent loop can run its own recovery.
//! - `Retry-After: <integer seconds>` is honoured when
//!   `respect_retry_after = true`. It is never shortened by the local cap;
//!   an unacceptably large hint declines the retry instead.
//! - Fallback backoff is exponential (`base, base*2, base*4, …`) plus jitter,
//!   overflow-safe and bounded.

use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{HeaderMap, RETRY_AFTER};
use tokio_util::sync::CancellationToken;

use crate::config::LlmConfig;
use crate::llm::LlmErrorKind;

/// Local exponential backoff cap (fallback when no usable `Retry-After`).
pub(crate) const MAX_LOCAL_BACKOFF_MS: u64 = 30_000;
/// Largest server `Retry-After` we are willing to honour. Larger hints
/// decline the retry instead of sleeping early or forever.
pub(crate) const MAX_RETRY_AFTER_SECS: u64 = 300;
/// Caps the `2^n` shift so huge attempt numbers cannot overflow/panic.
const MAX_BACKOFF_SHIFT: u32 = 20;

/// Typed per-attempt failure, separating retry decision inputs from
/// user-facing text and transport details.
pub(crate) struct RequestAttemptFailure {
    pub kind: LlmErrorKind,
    pub status: Option<StatusCode>,
    pub retry_after: Option<Duration>,
    pub provider_code: Option<String>,
    pub source: anyhow::Error,
}

impl std::fmt::Debug for RequestAttemptFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestAttemptFailure")
            .field("kind", &self.kind)
            .field("status", &self.status)
            .field("retry_after", &self.retry_after)
            .field("provider_code", &self.provider_code)
            .field("source", &format_args!("{}", self.source))
            .finish()
    }
}

impl RequestAttemptFailure {
    pub(crate) fn new(
        kind: LlmErrorKind,
        status: Option<StatusCode>,
        retry_after: Option<Duration>,
        provider_code: Option<String>,
        source: anyhow::Error,
    ) -> Self {
        Self {
            kind,
            status,
            retry_after,
            provider_code,
            source,
        }
    }
}

/// `max_retries` = retries allowed after the first attempt.
pub(crate) fn max_attempts(max_retries: usize) -> usize {
    max_retries.saturating_add(1)
}

/// Extract `error.code` from an OpenAI-compatible error body, when present.
/// Only structured `error.code` strings are used; no substring heuristics.
/// The returned code is passed through the shared safe-identifier filter so
/// it can appear in diagnostic logs without injection or content leakage.
pub(crate) fn extract_provider_code(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let code = value.get("error")?.get("code")?.as_str()?;
    let safe = crate::logging::safe_identifier(code);
    if safe.is_empty() { None } else { Some(safe) }
}

/// True for the provider signal that must reach the agent loop untouched so
/// reactive compaction can run exactly once at the agent level.
pub(crate) fn is_context_length_exceeded_code(code: &str) -> bool {
    code == "context_length_exceeded"
}

/// Known quota / billing / spend-limit codes that require user action.
/// A 429 carrying one of these must never be retried.
pub(crate) fn is_permanent_quota_code(code: &str) -> bool {
    matches!(
        code.to_ascii_lowercase().as_str(),
        "insufficient_quota"
            | "quota_exceeded"
            | "organization_quota_exceeded"
            | "organization_spend_limit_exceeded"
            | "project_spend_limit_exceeded"
            | "organization_usage_limit_exceeded"
            | "billing_hard_limit_exceeded"
            | "spend_limit_exceeded"
            | "usage_limit_exceeded"
    )
}

/// Map an HTTP status to the telemetry [`LlmErrorKind`] used for retry
/// decisions and logging. Single source of truth for all chat-completions
/// paths (`chat_tools_once`, `chat_once_request`, `chat_stream`
/// establishment) so kind assignment cannot drift apart again.
pub(crate) fn kind_for_status(status: StatusCode) -> LlmErrorKind {
    match status.as_u16() {
        408 => LlmErrorKind::Timeout,
        429 => LlmErrorKind::RateLimited,
        409 => LlmErrorKind::Server,
        _ if status.is_server_error() => LlmErrorKind::Server,
        _ => LlmErrorKind::Client,
    }
}

/// Decide whether a failed attempt may be retried (budget still applies at
/// the call site via `max_attempts`).
///
/// Fail-closed: unknown 4xx (including unknown 400/404/422 shapes) return
/// `false`. Only proven-transient failures return `true`.
pub(crate) fn should_retry(failure: &RequestAttemptFailure) -> bool {
    match failure.kind {
        LlmErrorKind::Cancelled
        | LlmErrorKind::Authentication
        | LlmErrorKind::ContextLengthExceeded
        | LlmErrorKind::Deserialize => return false,
        _ => {}
    }

    if let Some(status) = failure.status {
        match status.as_u16() {
            401 | 403 => return false,
            400 | 404 | 422 => return false,
            408 | 409 => return true,
            429 => {
                if let Some(code) = failure.provider_code.as_deref()
                    && is_permanent_quota_code(code)
                {
                    return false;
                }
                // Transient 429 (`slow_down`, `rate_limit_exceeded`, …) and
                // unknown 429 codes are bounded-retried; only proven
                // permanent quota codes fail fast.
                return true;
            }
            _ if status.is_server_error() => return true,
            _ if status.is_client_error() => return false,
            _ => return false,
        }
    }

    matches!(
        failure.kind,
        LlmErrorKind::RateLimited
            | LlmErrorKind::Server
            | LlmErrorKind::Network
            | LlmErrorKind::Timeout
    )
}

/// Parse a `Retry-After` header value. v1 supports integer seconds only;
/// missing / empty / invalid / negative / HTTP-date values return `None`
/// so the caller falls back to local backoff.
pub(crate) fn parse_retry_after_value(value: &str) -> Option<Duration> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.parse::<u64>() {
        Ok(secs) => Some(Duration::from_secs(secs)),
        Err(_) => None,
    }
}

/// Extract `Retry-After` from response headers (integer seconds only).
pub(crate) fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_retry_after_value)
}

/// Local exponential backoff: `base, base*2, base*4, …` plus uniform
/// `0..=jitter_ms`, capped and overflow-safe.
/// `failed_attempt` is 1-based (the attempt that just failed).
pub(crate) fn local_backoff_delay(config: &LlmConfig, failed_attempt: usize) -> Duration {
    let shift = (failed_attempt.saturating_sub(1)).min(MAX_BACKOFF_SHIFT as usize) as u32;
    let mult = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
    let exp = config.retry_base_ms.saturating_mul(mult);
    let capped = exp.min(MAX_LOCAL_BACKOFF_MS);
    let jitter = if config.retry_jitter_ms == 0 {
        0
    } else {
        fastrand::u64(0..=config.retry_jitter_ms)
    };
    Duration::from_millis(capped.saturating_add(jitter))
}

/// Retry delay decision for a failed attempt.
pub(crate) enum RetryDelayDecision {
    Sleep(Duration),
    /// Do not retry (e.g. `Retry-After` too large to honour). The caller
    /// must return the original error without an early re-send.
    Decline,
}

/// Compute the delay before the next retry.
///
/// - When `respect_retry_after` is true and a valid `Retry-After` is
///   present, sleep exactly that long (never shortened by the local cap).
///   Hints larger than [`MAX_RETRY_AFTER_SECS`] decline the retry.
/// - Otherwise use local exponential backoff + jitter.
pub(crate) fn compute_retry_delay(
    config: &LlmConfig,
    failed_attempt: usize,
    retry_after: Option<Duration>,
) -> RetryDelayDecision {
    if config.respect_retry_after
        && let Some(hint) = retry_after
    {
        if hint > Duration::from_secs(MAX_RETRY_AFTER_SECS) {
            return RetryDelayDecision::Decline;
        }
        return RetryDelayDecision::Sleep(hint);
    }
    RetryDelayDecision::Sleep(local_backoff_delay(config, failed_attempt))
}

/// Cancellation-aware sleep. Returns `true` when the token fired first.
pub(crate) async fn cancel_aware_sleep(delay: Duration, cancel: &CancellationToken) -> bool {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => true,
        _ = tokio::time::sleep(delay) => false,
    }
}

/// Classify a `reqwest` transport error when no HTTP status exists.
pub(crate) fn classify_transport(reqwest_err: &reqwest::Error) -> LlmErrorKind {
    if reqwest_err.is_timeout() {
        LlmErrorKind::Timeout
    } else {
        LlmErrorKind::Network
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> LlmConfig {
        LlmConfig {
            max_retries: 3,
            retry_base_ms: 1000,
            retry_jitter_ms: 0,
            respect_retry_after: true,
            ..LlmConfig::default()
        }
    }

    fn failure(
        kind: LlmErrorKind,
        status: Option<u16>,
        code: Option<&str>,
    ) -> RequestAttemptFailure {
        RequestAttemptFailure::new(
            kind,
            status.map(|s| StatusCode::from_u16(s).expect("status")),
            None,
            code.map(|c| c.to_owned()),
            anyhow::anyhow!("test"),
        )
    }

    #[test]
    fn max_attempts_semantics() {
        assert_eq!(max_attempts(0), 1);
        assert_eq!(max_attempts(1), 2);
        assert_eq!(max_attempts(3), 4);
        assert_eq!(max_attempts(usize::MAX), usize::MAX);
    }

    #[test]
    fn kind_for_status_mapping() {
        assert_eq!(
            kind_for_status(StatusCode::from_u16(408).expect("status")),
            LlmErrorKind::Timeout
        );
        assert_eq!(
            kind_for_status(StatusCode::from_u16(409).expect("status")),
            LlmErrorKind::Server
        );
        assert_eq!(
            kind_for_status(StatusCode::from_u16(429).expect("status")),
            LlmErrorKind::RateLimited
        );
        assert_eq!(
            kind_for_status(StatusCode::from_u16(503).expect("status")),
            LlmErrorKind::Server
        );
        assert_eq!(
            kind_for_status(StatusCode::from_u16(400).expect("status")),
            LlmErrorKind::Client
        );
    }

    #[test]
    fn permanent_4xx_never_retry() {
        for status in [400u16, 401, 403, 404, 422] {
            let f = failure(LlmErrorKind::Client, Some(status), None);
            assert!(!should_retry(&f), "status {status} must not retry");
        }
        // Unknown 4xx fail closed.
        for status in [405u16, 410, 418, 451] {
            let kind = if status == 429 {
                LlmErrorKind::RateLimited
            } else {
                LlmErrorKind::Client
            };
            let f = failure(kind, Some(status), None);
            assert!(!should_retry(&f), "status {status} must not retry");
        }
    }

    #[test]
    fn auth_and_context_and_deserialize_never_retry() {
        assert!(!should_retry(&failure(
            LlmErrorKind::Authentication,
            Some(401),
            None
        )));
        assert!(!should_retry(&failure(
            LlmErrorKind::ContextLengthExceeded,
            Some(400),
            Some("context_length_exceeded"),
        )));
        assert!(!should_retry(&failure(
            LlmErrorKind::Deserialize,
            None,
            None
        )));
        assert!(!should_retry(&failure(LlmErrorKind::Cancelled, None, None)));
    }

    #[test]
    fn retryable_statuses() {
        assert!(should_retry(&failure(
            LlmErrorKind::Timeout,
            Some(408),
            None
        )));
        assert!(should_retry(&failure(
            LlmErrorKind::Server,
            Some(409),
            None
        )));
        assert!(should_retry(&failure(
            LlmErrorKind::Server,
            Some(500),
            None
        )));
        assert!(should_retry(&failure(
            LlmErrorKind::Server,
            Some(503),
            None
        )));
        assert!(should_retry(&failure(LlmErrorKind::Network, None, None)));
        assert!(should_retry(&failure(LlmErrorKind::Timeout, None, None)));
    }

    #[test]
    fn transient_429_retries_permanent_quota_does_not() {
        assert!(should_retry(&failure(
            LlmErrorKind::RateLimited,
            Some(429),
            Some("slow_down"),
        )));
        assert!(should_retry(&failure(
            LlmErrorKind::RateLimited,
            Some(429),
            Some("rate_limit_exceeded"),
        )));
        assert!(should_retry(&failure(
            LlmErrorKind::RateLimited,
            Some(429),
            None
        )));
        for code in [
            "project_spend_limit_exceeded",
            "organization_spend_limit_exceeded",
            "organization_usage_limit_exceeded",
            "insufficient_quota",
        ] {
            assert!(
                !should_retry(&failure(LlmErrorKind::RateLimited, Some(429), Some(code))),
                "quota code {code} must not retry"
            );
        }
    }

    #[test]
    fn retry_after_parsing() {
        assert_eq!(parse_retry_after_value("5"), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after_value("0"), Some(Duration::from_secs(0)));
        assert_eq!(
            parse_retry_after_value("  7  "),
            Some(Duration::from_secs(7))
        );
        assert_eq!(parse_retry_after_value(""), None);
        assert_eq!(parse_retry_after_value("   "), None);
        assert_eq!(parse_retry_after_value("not-a-number"), None);
        assert_eq!(parse_retry_after_value("-5"), None);
        assert_eq!(
            parse_retry_after_value("Wed, 21 Oct 2015 07:28:00 GMT"),
            None
        );
    }

    #[test]
    fn respect_retry_after_false_uses_local_backoff() {
        let mut cfg = test_config();
        cfg.respect_retry_after = false;
        cfg.retry_base_ms = 1000;
        cfg.retry_jitter_ms = 0;
        match compute_retry_delay(&cfg, 1, Some(Duration::from_secs(5))) {
            RetryDelayDecision::Sleep(d) => assert_eq!(d, Duration::from_millis(1000)),
            RetryDelayDecision::Decline => panic!("must not decline"),
        }
    }

    #[test]
    fn retry_after_honoured_not_shortened() {
        let cfg = test_config();
        match compute_retry_delay(&cfg, 1, Some(Duration::from_secs(5))) {
            RetryDelayDecision::Sleep(d) => assert_eq!(d, Duration::from_secs(5)),
            RetryDelayDecision::Decline => panic!("must sleep"),
        }
        // Zero is honoured (immediate retry).
        match compute_retry_delay(&cfg, 1, Some(Duration::from_secs(0))) {
            RetryDelayDecision::Sleep(d) => assert_eq!(d, Duration::from_secs(0)),
            RetryDelayDecision::Decline => panic!("must sleep"),
        }
    }

    #[test]
    fn oversized_retry_after_declines() {
        let cfg = test_config();
        match compute_retry_delay(&cfg, 1, Some(Duration::from_secs(MAX_RETRY_AFTER_SECS + 1))) {
            RetryDelayDecision::Decline => {}
            RetryDelayDecision::Sleep(_) => panic!("oversized hint must decline"),
        }
    }

    #[test]
    fn backoff_overflow_safe() {
        let cfg = LlmConfig {
            retry_base_ms: u64::MAX / 2,
            retry_jitter_ms: u64::MAX / 2,
            ..LlmConfig::default()
        };
        let d = local_backoff_delay(&cfg, usize::MAX);
        assert!(d.as_millis() <= (MAX_LOCAL_BACKOFF_MS as u128 + cfg.retry_jitter_ms as u128));
        let d2 = local_backoff_delay(&test_config(), usize::MAX);
        assert!(d2.as_millis() <= MAX_LOCAL_BACKOFF_MS as u128);
        // Normal exponential shape with zero jitter.
        let cfg2 = test_config();
        assert_eq!(local_backoff_delay(&cfg2, 1), Duration::from_millis(1000));
        assert_eq!(local_backoff_delay(&cfg2, 2), Duration::from_millis(2000));
        assert_eq!(local_backoff_delay(&cfg2, 3), Duration::from_millis(4000));
    }

    #[test]
    fn extract_provider_code_shapes() {
        assert_eq!(
            extract_provider_code(r#"{"error":{"code":"slow_down"}}"#).as_deref(),
            Some("slow_down")
        );
        assert_eq!(
            extract_provider_code(r#"{"error":{"code":"project_spend_limit_exceeded"}}"#)
                .as_deref(),
            Some("project_spend_limit_exceeded")
        );
        assert_eq!(extract_provider_code("not json"), None);
        assert_eq!(extract_provider_code(r#"{"error":{}}"#), None);
        assert_eq!(extract_provider_code(r#"{"error":{"code":123}}"#), None);
    }
}
