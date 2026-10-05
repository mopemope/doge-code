use anyhow::Result;
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::config::LlmConfig;
use crate::llm::prompt_cache::{
    PromptCacheCounters, PromptCacheSessionUsage, PromptCacheUsageSnapshot,
};
use crate::llm::types::{ChatMessage, ChoiceMessage, Usage};

mod network;

/// Session totals used for conservative serial-request attribution. No estimates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageTotalsSnapshot {
    pub total_tokens: u64,
    pub prompt_tokens: u64,
    pub record_count: u64,
}

#[derive(Clone)]
pub struct OpenAIClient {
    pub(crate) subscription: Option<crate::features::openai_subscription::auth::AuthHandle>,
    pub base_url: String,
    pub api_key: String,
    pub(crate) inner: reqwest::Client,
    pub llm_cfg: LlmConfig,
    /// Resolved Responses `context_management.compact_threshold` for the
    /// ChatGPT subscription route. `None` for OpenAI-compatible providers
    /// (local compaction path unchanged).
    pub(crate) responses_compact_threshold: Option<u32>,
    /// Tracks total tokens used by this client
    pub tokens_used: Arc<AtomicU32>,
    /// Tracks prompt tokens used by this client (for header display)
    pub prompt_tokens_used: Arc<AtomicU32>,
    /// Cumulative total tokens across the whole session (never reset by
    /// per-request tracking; only cleared explicitly via `clear_totals`).
    pub total_tokens_used: Arc<AtomicU64>,
    /// Monotonic usage records; never populated from local token estimates.
    pub usage_record_count: Arc<AtomicU64>,
    /// Process-lifetime ledger shared by clones, independent of UI resets.
    pub(crate) usage_ledger: Arc<std::sync::Mutex<crate::llm::usage_ledger::UsageLedger>>,
    /// Cumulative prompt tokens across the whole session.
    pub total_prompt_tokens_used: Arc<AtomicU64>,
    /// Last request's reasoning tokens (from `completion_tokens_details`).
    pub reasoning_tokens_used: Arc<AtomicU32>,
    /// Cumulative reasoning tokens across the session.
    pub total_reasoning_tokens_used: Arc<AtomicU64>,
    /// True once any response carried `completion_tokens_details.reasoning_tokens`
    /// (including an explicit `0`). False means the provider does not report
    /// reasoning usage and callers must not display `0 tokens`.
    pub reasoning_usage_seen: Arc<AtomicBool>,
    /// Prompt-cache telemetry (last-request + session totals).
    /// Shared via `Arc` like the other counters so `Clone` shares state.
    pub prompt_cache_counters: Arc<PromptCacheCounters>,
}

impl std::fmt::Debug for OpenAIClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAIClient")
            .field("base_url", &self.base_url)
            .field("api_key", &"[REDACTED]")
            .field("subscription", &self.subscription)
            .finish_non_exhaustive()
    }
}

impl OpenAIClient {
    pub fn from_config(cfg: &crate::config::AppConfig) -> Result<Option<Self>> {
        use crate::features::openai_subscription::{
            ProviderKind, auth::AuthHandle, credentials::CredentialStore,
        };
        if cfg.provider == ProviderKind::OpenaiChatgpt {
            let auth = AuthHandle::selected(CredentialStore::default_path()?)?;
            let threshold = cfg.get_effective_compaction_limit();
            crate::features::openai_subscription::responses::validate_compact_threshold(threshold)?;
            let mut client =
                Self::new("https://api.openai.com/v1", "")?.with_llm_config(cfg.llm.clone());
            client.subscription = Some(auth);
            client.responses_compact_threshold = Some(threshold);
            tracing::debug!(
                responses_native_compaction_enabled = true,
                responses_compact_threshold = threshold,
                "subscription native compaction configured"
            );
            Ok(Some(client))
        } else {
            cfg.api_key
                .as_ref()
                .map(|key| {
                    Self::new(&cfg.base_url, key).map(|c| c.with_llm_config(cfg.llm.clone()))
                })
                .transpose()
        }
    }

    pub fn is_subscription(&self) -> bool {
        self.subscription.is_some()
    }

    /// Resolved Responses native-compaction threshold, if this client uses
    /// the ChatGPT subscription route.
    pub fn responses_compact_threshold(&self) -> Option<u32> {
        self.responses_compact_threshold
    }

    /// True when server-side Responses compaction is configured for this
    /// client. Local Chat Completions summarization must not run then.
    pub fn native_responses_compaction_enabled(&self) -> bool {
        self.responses_compact_threshold.is_some()
    }

    /// Test/fixture override for the native-compaction threshold.
    /// Rejects below-minimum values instead of clamping.
    pub fn with_responses_compact_threshold(mut self, threshold: Option<u32>) -> Result<Self> {
        if let Some(value) = threshold {
            crate::features::openai_subscription::responses::validate_compact_threshold(value)?;
        }
        self.responses_compact_threshold = threshold;
        Ok(self)
    }
    pub fn account_label(&self) -> Option<&str> {
        self.subscription.as_ref().map(|a| a.account.as_str())
    }

    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Result<Self> {
        let url = base_url.into();
        let inner = reqwest::Client::builder().build()?;
        Ok(Self {
            subscription: None,
            base_url: url,
            api_key: api_key.into(),
            inner,
            llm_cfg: LlmConfig::default(),
            responses_compact_threshold: None,
            tokens_used: Arc::new(AtomicU32::new(0)),
            prompt_tokens_used: Arc::new(AtomicU32::new(0)),
            total_tokens_used: Arc::new(AtomicU64::new(0)),
            usage_record_count: Arc::new(AtomicU64::new(0)),
            usage_ledger: Arc::new(std::sync::Mutex::new(Default::default())),
            total_prompt_tokens_used: Arc::new(AtomicU64::new(0)),
            reasoning_tokens_used: Arc::new(AtomicU32::new(0)),
            total_reasoning_tokens_used: Arc::new(AtomicU64::new(0)),
            reasoning_usage_seen: Arc::new(AtomicBool::new(false)),
            prompt_cache_counters: Arc::new(PromptCacheCounters::default()),
        })
    }

    pub fn with_llm_config(mut self, cfg: LlmConfig) -> Self {
        // Rebuild reqwest client with timeouts from cfg to ensure network layer reaches server in tests and prod.
        let builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(cfg.connect_timeout_ms))
            .timeout(Duration::from_millis(cfg.timeout_ms)) // Use timeout_ms for overall request timeout
            .read_timeout(Duration::from_millis(cfg.timeout_ms)); // Add timeout settings
        // If building fails, keep existing client to avoid panic; but in normal cases it should succeed.
        if let Ok(c) = builder.build() {
            self.inner = c;
        }
        self.llm_cfg = cfg;
        self
    }

    pub(crate) fn endpoint(&self) -> String {
        let mut base = self.base_url.trim_end_matches('/').to_string();
        if let Some(pos) = base.rfind("/v1") {
            base.truncate(pos);
            base = base.trim_end_matches('/').to_string();
        }
        format!("{base}/v1/chat/completions")
    }

    /// Get the total number of tokens used by this client
    pub fn get_tokens_used(&self) -> u32 {
        self.tokens_used.load(Ordering::Relaxed)
    }

    /// Add tokens to the total count
    pub fn add_tokens(&self, tokens: u32) {
        self.tokens_used.fetch_add(tokens, Ordering::Relaxed);
    }

    pub fn set_tokens(&self, tokens: u32) {
        self.tokens_used.store(tokens, Ordering::Relaxed);
    }

    /// Get the total number of prompt tokens used by this client
    pub fn get_prompt_tokens_used(&self) -> u32 {
        self.prompt_tokens_used.load(Ordering::Relaxed)
    }

    /// Add prompt tokens to the prompt count
    pub fn add_prompt_tokens(&self, tokens: u32) {
        self.prompt_tokens_used.fetch_add(tokens, Ordering::Relaxed);
    }

    pub fn set_prompt_tokens(&self, tokens: u32) {
        self.prompt_tokens_used.store(tokens, Ordering::Relaxed);
    }

    /// Get the cumulative total tokens used across the session.
    pub fn get_total_tokens_used(&self) -> u64 {
        self.total_tokens_used.load(Ordering::Relaxed)
    }

    /// Get the cumulative prompt tokens used across the session.
    pub fn get_total_prompt_tokens_used(&self) -> u64 {
        self.total_prompt_tokens_used.load(Ordering::Relaxed)
    }

    /// Accumulate a request's usage into the session totals.
    pub fn add_total_tokens(&self, total_tokens: u32, prompt_tokens: u32) {
        self.total_tokens_used
            .fetch_add(total_tokens as u64, Ordering::Relaxed);
        self.total_prompt_tokens_used
            .fetch_add(prompt_tokens as u64, Ordering::Relaxed);
    }

    /// Reset the cumulative session totals (used by `/clear`).
    pub fn clear_totals(&self) {
        self.total_tokens_used.store(0, Ordering::Relaxed);
        self.total_prompt_tokens_used.store(0, Ordering::Relaxed);
        self.total_reasoning_tokens_used.store(0, Ordering::Relaxed);
        self.reasoning_usage_seen.store(false, Ordering::Relaxed);
        self.prompt_cache_counters.clear();
    }

    /// Get the last request's reasoning tokens.
    pub fn get_reasoning_tokens_used(&self) -> u32 {
        self.reasoning_tokens_used.load(Ordering::Relaxed)
    }

    /// Get the cumulative reasoning tokens across the session.
    pub fn get_total_reasoning_tokens_used(&self) -> u64 {
        self.total_reasoning_tokens_used.load(Ordering::Relaxed)
    }

    /// Whether any response has carried reasoning token details yet.
    /// `false` means the provider does not report them; do not display `0`.
    pub fn has_reasoning_usage(&self) -> bool {
        self.reasoning_usage_seen.load(Ordering::Relaxed)
    }

    pub fn set_reasoning_tokens(&self, tokens: u32) {
        self.reasoning_tokens_used.store(tokens, Ordering::Relaxed);
    }

    pub fn add_total_reasoning_tokens(&self, tokens: u32) {
        self.total_reasoning_tokens_used
            .fetch_add(tokens as u64, Ordering::Relaxed);
    }

    /// Snapshot provider totals for serial-request attribution. The record
    /// count is monotonic and is not reset with session totals.
    pub fn usage_totals_snapshot(&self) -> UsageTotalsSnapshot {
        UsageTotalsSnapshot {
            total_tokens: self.get_total_tokens_used(),
            prompt_tokens: self.get_total_prompt_tokens_used(),
            record_count: self.usage_record_count.load(Ordering::Relaxed),
        }
    }

    /// Record one response's [`Usage`], including optional reasoning details.
    /// A present `reasoning_tokens` field (even `0`) marks usage as seen;
    /// an absent field leaves `reasoning_usage_seen` untouched.
    /// Prompt-cache details follow the same contract: an explicit `0` marks
    /// reported/seen as true, while absent details reset the last-request
    /// state to "not reported" without touching the ever-seen flags.
    /// This is the single source of truth for all request paths
    /// (`chat_tools_once`, `chat_once_request`, compaction, sub-agent).
    pub fn usage_snapshot(&self) -> crate::llm::usage_ledger::UsageLedger {
        *self
            .usage_ledger
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub(crate) fn record_request_attempt(&self) {
        let mut ledger = self
            .usage_ledger
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        ledger.attempts = ledger.attempts.saturating_add(1);
    }

    pub fn record_usage(&self, usage: &Usage) {
        self.usage_ledger
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .record(usage);
        self.set_tokens(usage.total_tokens);
        self.set_prompt_tokens(usage.prompt_tokens);
        self.add_total_tokens(usage.total_tokens, usage.prompt_tokens);
        if let Some(details) = &usage.completion_tokens_details
            && let Some(reasoning) = details.reasoning_tokens
        {
            self.set_reasoning_tokens(reasoning);
            self.add_total_reasoning_tokens(reasoning);
            self.reasoning_usage_seen.store(true, Ordering::Relaxed);
        }
        self.prompt_cache_counters
            .record(usage.prompt_tokens_details.as_ref());
        let mut count = self.usage_record_count.load(Ordering::Relaxed);
        loop {
            match self.usage_record_count.compare_exchange_weak(
                count,
                count.saturating_add(1),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(current) => count = current,
            }
        }
    }

    /// Last request's prompt-cache usage. `None` fields mean the provider did
    /// not report that metric for the last request (distinct from `Some(0)`).
    pub fn last_prompt_cache_usage(&self) -> PromptCacheUsageSnapshot {
        self.prompt_cache_counters.snapshot_last()
    }

    /// Session total of `cached_tokens` (cache reads).
    pub fn get_total_cached_prompt_tokens(&self) -> u64 {
        self.prompt_cache_counters.total_cached()
    }

    /// Session total of `cache_write_tokens` (cache writes).
    pub fn get_total_cache_write_tokens(&self) -> u64 {
        self.prompt_cache_counters.total_cache_write()
    }

    /// True once any response carried `cached_tokens` (including `0`).
    pub fn has_cached_prompt_usage(&self) -> bool {
        self.prompt_cache_counters.has_cached()
    }

    /// True once any response carried `cache_write_tokens` (including `0`).
    pub fn has_cache_write_usage(&self) -> bool {
        self.prompt_cache_counters.has_cache_write()
    }

    /// Session-level cache usage with the prompt-total denominator.
    /// Cached tokens still occupy the context window, so callers must not
    /// subtract them from context-budget calculations.
    pub fn prompt_cache_session_usage(&self) -> PromptCacheSessionUsage {
        PromptCacheSessionUsage {
            total_cached_tokens: self.get_total_cached_prompt_tokens(),
            total_cache_write_tokens: self.get_total_cache_write_tokens(),
            total_prompt_tokens: self.get_total_prompt_tokens_used(),
        }
    }

    /// Session token cache-hit ratio: `total_cached / total_prompt`.
    /// `None` when no prompt tokens exist yet or the provider never reported
    /// cache telemetry. Cache writes are never mixed into this ratio.
    pub fn prompt_cache_hit_ratio(&self) -> Option<f64> {
        if !self.has_cached_prompt_usage() {
            return None;
        }
        self.prompt_cache_session_usage().hit_ratio()
    }

    /// Last-request token cache-hit ratio: `last_cached / last_prompt`.
    /// `None` when the last response did not report `cached_tokens` or the
    /// last prompt size is zero.
    pub fn last_prompt_cache_hit_ratio(&self) -> Option<f64> {
        let last = self.last_prompt_cache_usage();
        let cached = last.cached_tokens?;
        let last_prompt = self.get_prompt_tokens_used();
        if last_prompt == 0 {
            return None;
        }
        let ratio = cached as f64 / last_prompt as f64;
        if ratio.is_finite() { Some(ratio) } else { None }
    }

    /// Restore a previously snapshotted last-request cache state.
    /// Used by the `task` sub-agent to protect the main loop's telemetry;
    /// session totals are intentionally left accumulated.
    pub fn restore_last_prompt_cache_usage(&self, snapshot: PromptCacheUsageSnapshot) {
        self.prompt_cache_counters.restore_last(snapshot);
    }

    #[allow(dead_code)]
    pub async fn chat_once(
        &self,
        model: &str,
        messages: Vec<ChatMessage>,
        cancel: Option<CancellationToken>,
    ) -> Result<ChoiceMessage> {
        // Delegate to network module implementation for clarity and to keep this file small
        if self.is_subscription() {
            return self
                .chat_once_request(
                    &crate::llm::types::ChatRequest {
                        model: model.into(),
                        messages,
                        temperature: None,
                        stream: None,
                    },
                    cancel,
                )
                .await;
        }
        crate::llm::client_core::network::chat_once(self, model, messages, cancel).await
    }

    pub(crate) async fn chat_once_request<T: Serialize + ?Sized>(
        &self,
        req: &T,
        cancel: Option<CancellationToken>,
    ) -> Result<ChoiceMessage> {
        if let Some(auth) = &self.subscription {
            let value = serde_json::to_value(req)?;
            let model = value
                .get("model")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("request model missing"))?;
            let messages: Vec<ChatMessage> = serde_json::from_value(
                value
                    .get("messages")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("request messages missing"))?,
            )?;
            let result = crate::features::openai_subscription::responses::infer(
                self,
                auth,
                model,
                &messages,
                &[],
                None,
                cancel.unwrap_or_default(),
            )
            .await?;
            return Ok(ChoiceMessage {
                role: result.role,
                content: result.content.unwrap_or_default(),
            });
        }
        crate::llm::client_core::network::chat_once_request(self, req, cancel).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::ChatMessage;
    use httptest::{Expectation, matchers::*, responders::*};

    #[tokio::test]
    async fn chat_once_happy_path() {
        let server = crate::test_support::HTTP_SERVER_POOL.get_server();
        server.expect(
            Expectation::matching(all_of![
                request::method_path("POST", "/v1/chat/completions"),
                request::headers(contains(key("authorization"))),
            ])
            .times(1)
            .respond_with(json_encoded(serde_json::json!({
                "id": "test",
                "choices": [
                    {"index":0, "message": {"role":"assistant","content":"hello"}}
                ]
            }))),
        );

        let client = OpenAIClient::new(format!("{}/", server.url_str("")), "test-key").unwrap();
        let msg = client
            .chat_once(
                "gpt-test",
                vec![ChatMessage {
                    provider_state: None,
                    role: "user".into(),
                    content: Some("hi".into()),
                    tool_calls: vec![],
                    tool_call_id: None,
                }],
                None,
            )
            .await
            .unwrap();
        assert_eq!(msg.content, "hello");
    }

    #[test]
    fn test_usage_totals_snapshot_counts_records_without_estimates() {
        let client = OpenAIClient::new("https://example.invalid/v1", "fixture").expect("client");
        let before = client.usage_totals_snapshot();
        assert_eq!(before.record_count, 0);
        let usage: Usage = serde_json::from_value(serde_json::json!({"total_tokens":150,"prompt_tokens":100,"completion_tokens":50,"prompt_tokens_details":{"cached_tokens":80},"completion_tokens_details":{"reasoning_tokens":20}})).expect("usage");
        client.record_usage(&usage);
        let after = client.usage_totals_snapshot();
        assert_eq!(
            after,
            UsageTotalsSnapshot {
                total_tokens: 150,
                prompt_tokens: 100,
                record_count: 1
            }
        );
        client.set_tokens(1);
        client.set_prompt_tokens(1);
        client.restore_last_prompt_cache_usage(Default::default());
        assert_eq!(client.usage_totals_snapshot(), after);
        assert_eq!(client.get_total_cached_prompt_tokens(), 80);
        assert_eq!(client.get_total_reasoning_tokens_used(), 20);
    }

    #[test]
    fn endpoint_normalization() {
        let c = OpenAIClient {
            subscription: None,
            base_url: "https://api.example.com/v1/".into(),
            api_key: "x".into(),
            inner: reqwest::Client::new(),
            llm_cfg: LlmConfig::default(),
            responses_compact_threshold: None,
            tokens_used: Arc::new(AtomicU32::new(0)),
            prompt_tokens_used: Arc::new(AtomicU32::new(0)),
            total_tokens_used: Arc::new(AtomicU64::new(0)),
            usage_record_count: Arc::new(AtomicU64::new(0)),
            usage_ledger: Arc::new(std::sync::Mutex::new(Default::default())),
            total_prompt_tokens_used: Arc::new(AtomicU64::new(0)),
            reasoning_tokens_used: Arc::new(AtomicU32::new(0)),
            total_reasoning_tokens_used: Arc::new(AtomicU64::new(0)),
            reasoning_usage_seen: Arc::new(AtomicBool::new(false)),
            prompt_cache_counters: Arc::new(PromptCacheCounters::default()),
        };
        assert_eq!(c.endpoint(), "https://api.example.com/v1/chat/completions");
        let c2 = OpenAIClient {
            subscription: None,
            base_url: "https://api.example.com/".into(),
            api_key: "x".into(),
            inner: reqwest::Client::new(),
            llm_cfg: LlmConfig::default(),
            responses_compact_threshold: None,
            tokens_used: Arc::new(AtomicU32::new(0)),
            prompt_tokens_used: Arc::new(AtomicU32::new(0)),
            total_tokens_used: Arc::new(AtomicU64::new(0)),
            usage_record_count: Arc::new(AtomicU64::new(0)),
            usage_ledger: Arc::new(std::sync::Mutex::new(Default::default())),
            total_prompt_tokens_used: Arc::new(AtomicU64::new(0)),
            reasoning_tokens_used: Arc::new(AtomicU32::new(0)),
            total_reasoning_tokens_used: Arc::new(AtomicU64::new(0)),
            reasoning_usage_seen: Arc::new(AtomicBool::new(false)),
            prompt_cache_counters: Arc::new(PromptCacheCounters::default()),
        };
        assert_eq!(c2.endpoint(), "https://api.example.com/v1/chat/completions");
    }

    #[test]
    fn token_tracking() {
        let client = OpenAIClient::new("https://api.example.com/", "x").unwrap();
        assert_eq!(client.get_tokens_used(), 0);
        client.add_tokens(100);
        assert_eq!(client.get_tokens_used(), 100);
        client.add_tokens(50);
        assert_eq!(client.get_tokens_used(), 150);
    }

    #[test]
    fn total_token_tracking_accumulates() {
        let client = OpenAIClient::new("https://api.example.com/", "x").unwrap();
        client.add_total_tokens(1_000, 800);
        client.add_total_tokens(2_000, 1_500);
        assert_eq!(client.get_total_tokens_used(), 3_000);
        assert_eq!(client.get_total_prompt_tokens_used(), 2_300);
        // Per-request values are independent (used by compaction thresholds).
        client.set_tokens(2_000);
        client.set_prompt_tokens(1_500);
        assert_eq!(client.get_tokens_used(), 2_000);
        assert_eq!(client.get_prompt_tokens_used(), 1_500);
        client.clear_totals();
        assert_eq!(client.get_total_tokens_used(), 0);
        assert_eq!(client.get_total_prompt_tokens_used(), 0);
    }

    #[test]
    fn reasoning_token_tracking_marks_seen_only_when_present() {
        use crate::llm::types::{CompletionTokensDetails, Usage};
        let client = OpenAIClient::new("https://api.example.com/", "x").unwrap();
        assert!(!client.has_reasoning_usage());
        client.record_usage(&Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            prompt_tokens_details: None,
            completion_tokens_details: None,
        });
        assert!(!client.has_reasoning_usage());
        assert_eq!(client.get_reasoning_tokens_used(), 0);
        assert_eq!(client.get_total_reasoning_tokens_used(), 0);

        client.record_usage(&Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            prompt_tokens_details: None,
            completion_tokens_details: Some(CompletionTokensDetails {
                reasoning_tokens: Some(30),
                extra: Default::default(),
            }),
        });
        assert!(client.has_reasoning_usage());
        assert_eq!(client.get_reasoning_tokens_used(), 30);
        assert_eq!(client.get_total_reasoning_tokens_used(), 30);

        // Explicit zero still counts as seen.
        client.record_usage(&Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            prompt_tokens_details: None,
            completion_tokens_details: Some(CompletionTokensDetails {
                reasoning_tokens: Some(0),
                extra: Default::default(),
            }),
        });
        assert!(client.has_reasoning_usage());
        assert_eq!(client.get_reasoning_tokens_used(), 0);
        assert_eq!(client.get_total_reasoning_tokens_used(), 30);

        client.clear_totals();
        assert!(!client.has_reasoning_usage());
        assert_eq!(client.get_total_reasoning_tokens_used(), 0);
    }

    #[test]
    fn prompt_cache_record_accumulates_and_resets_last() {
        use crate::llm::types::{PromptTokensDetails, Usage};
        let client = OpenAIClient::new("https://api.example.com/", "x").unwrap();
        assert!(!client.has_cached_prompt_usage());
        assert!(!client.has_cache_write_usage());
        assert!(client.prompt_cache_hit_ratio().is_none());

        client.record_usage(&Usage {
            prompt_tokens: 10_000,
            completion_tokens: 100,
            total_tokens: 10_100,
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: Some(8000),
                cache_write_tokens: Some(2000),
                extra: Default::default(),
            }),
            completion_tokens_details: None,
        });
        assert!(client.has_cached_prompt_usage());
        assert!(client.has_cache_write_usage());
        assert_eq!(client.get_total_cached_prompt_tokens(), 8000);
        assert_eq!(client.get_total_cache_write_tokens(), 2000);
        assert_eq!(
            client.last_prompt_cache_usage(),
            crate::llm::prompt_cache::PromptCacheUsageSnapshot {
                cached_tokens: Some(8000),
                cache_write_tokens: Some(2000),
            }
        );

        client.record_usage(&Usage {
            prompt_tokens: 12_000,
            completion_tokens: 100,
            total_tokens: 12_100,
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: Some(10_000),
                cache_write_tokens: Some(0),
                extra: Default::default(),
            }),
            completion_tokens_details: None,
        });
        assert_eq!(client.get_total_cached_prompt_tokens(), 18_000);
        assert_eq!(client.get_total_cache_write_tokens(), 2000);
        assert_eq!(client.get_total_prompt_tokens_used(), 22_000);
    }

    #[test]
    fn prompt_cache_last_resets_when_absent() {
        use crate::llm::types::{PromptTokensDetails, Usage};
        let client = OpenAIClient::new("https://api.example.com/", "x").unwrap();
        client.record_usage(&Usage {
            prompt_tokens: 10_000,
            completion_tokens: 100,
            total_tokens: 10_100,
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: Some(8000),
                cache_write_tokens: None,
                extra: Default::default(),
            }),
            completion_tokens_details: None,
        });
        assert_eq!(client.last_prompt_cache_usage().cached_tokens, Some(8000));
        // Absent details must not leave a stale 8000 behind.
        client.record_usage(&Usage {
            prompt_tokens: 12_000,
            completion_tokens: 100,
            total_tokens: 12_100,
            prompt_tokens_details: None,
            completion_tokens_details: None,
        });
        assert_eq!(client.last_prompt_cache_usage().cached_tokens, None);
        assert_eq!(client.last_prompt_cache_usage().cache_write_tokens, None);
        // Session totals keep the first request's contribution.
        assert_eq!(client.get_total_cached_prompt_tokens(), 8000);
        // Ever-seen stays true so session display knows telemetry exists.
        assert!(client.has_cached_prompt_usage());
        assert!(client.prompt_cache_hit_ratio().is_some());
    }

    #[test]
    fn prompt_cache_hit_ratio_math_and_zero_denominator() {
        use crate::llm::types::{PromptTokensDetails, Usage};
        let client = OpenAIClient::new("https://api.example.com/", "x").unwrap();
        // No telemetry yet -> None, never 0.
        assert!(client.prompt_cache_hit_ratio().is_none());
        assert!(client.last_prompt_cache_hit_ratio().is_none());
        client.record_usage(&Usage {
            prompt_tokens: 20_000,
            completion_tokens: 100,
            total_tokens: 20_100,
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: Some(15_000),
                cache_write_tokens: None,
                extra: Default::default(),
            }),
            completion_tokens_details: None,
        });
        let session_ratio = client.prompt_cache_hit_ratio().expect("ratio");
        assert!((session_ratio - 0.75).abs() < 1e-9);
        let last_ratio = client.last_prompt_cache_hit_ratio().expect("last ratio");
        assert!((last_ratio - 0.75).abs() < 1e-9);

        // Zero denominator never divides: fresh client with no prompt tokens.
        let empty = OpenAIClient::new("https://api.example.com/", "x").unwrap();
        assert!(empty.prompt_cache_hit_ratio().is_none());
    }

    #[test]
    fn prompt_cache_clear_totals_resets_cache() {
        use crate::llm::types::{PromptTokensDetails, Usage};
        let client = OpenAIClient::new("https://api.example.com/", "x").unwrap();
        client.record_usage(&Usage {
            prompt_tokens: 100,
            completion_tokens: 10,
            total_tokens: 110,
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: Some(60),
                cache_write_tokens: Some(20),
                extra: Default::default(),
            }),
            completion_tokens_details: None,
        });
        client.clear_totals();
        assert_eq!(client.get_total_cached_prompt_tokens(), 0);
        assert_eq!(client.get_total_cache_write_tokens(), 0);
        assert!(!client.has_cached_prompt_usage());
        assert!(!client.has_cache_write_usage());
        assert_eq!(client.last_prompt_cache_usage().cached_tokens, None);
        assert!(client.prompt_cache_hit_ratio().is_none());
    }

    #[test]
    fn prompt_cache_restore_last_keeps_session_totals() {
        use crate::llm::types::{PromptTokensDetails, Usage};
        let client = OpenAIClient::new("https://api.example.com/", "x").unwrap();
        client.record_usage(&Usage {
            prompt_tokens: 10_000,
            completion_tokens: 100,
            total_tokens: 10_100,
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: Some(6000),
                cache_write_tokens: None,
                extra: Default::default(),
            }),
            completion_tokens_details: None,
        });
        let saved = client.last_prompt_cache_usage();
        client.record_usage(&Usage {
            prompt_tokens: 5_000,
            completion_tokens: 50,
            total_tokens: 5_050,
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: Some(2000),
                cache_write_tokens: None,
                extra: Default::default(),
            }),
            completion_tokens_details: None,
        });
        client.restore_last_prompt_cache_usage(saved);
        assert_eq!(client.last_prompt_cache_usage().cached_tokens, Some(6000));
        // Sub-agent usage stays in the session totals.
        assert_eq!(client.get_total_cached_prompt_tokens(), 8000);
    }

    #[test]
    fn prompt_cache_coexists_with_reasoning_telemetry() {
        use crate::llm::types::{CompletionTokensDetails, PromptTokensDetails, Usage};
        let client = OpenAIClient::new("https://api.example.com/", "x").unwrap();
        client.record_usage(&Usage {
            prompt_tokens: 12_000,
            completion_tokens: 500,
            total_tokens: 12_500,
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: Some(9000),
                cache_write_tokens: Some(3000),
                extra: Default::default(),
            }),
            completion_tokens_details: Some(CompletionTokensDetails {
                reasoning_tokens: Some(800),
                extra: Default::default(),
            }),
        });
        // Both telemetry families are recorded independently.
        assert!(client.has_cached_prompt_usage());
        assert!(client.has_reasoning_usage());
        assert_eq!(client.get_total_cached_prompt_tokens(), 9000);
        assert_eq!(client.get_total_reasoning_tokens_used(), 800);
        assert_eq!(client.get_total_prompt_tokens_used(), 12_000);
        // Compaction-style follow-up accumulates into the same session totals.
        client.record_usage(&Usage {
            prompt_tokens: 8_000,
            completion_tokens: 200,
            total_tokens: 8_200,
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: Some(6000),
                cache_write_tokens: None,
                extra: Default::default(),
            }),
            completion_tokens_details: Some(CompletionTokensDetails {
                reasoning_tokens: Some(100),
                extra: Default::default(),
            }),
        });
        assert_eq!(client.get_total_cached_prompt_tokens(), 15_000);
        assert_eq!(client.get_total_reasoning_tokens_used(), 900);
        assert_eq!(client.get_total_prompt_tokens_used(), 20_000);
    }
}
