//! Prompt-cache telemetry + client-side prefix-stability diagnostics (v1).
//!
//! Observational only. This module never changes provider routing or caching
//! semantics.
//!
//! Responsibilities:
//! - [`PromptCacheCounters`]: last-request + session totals for
//!   `prompt_tokens_details.cached_tokens` / `cache_write_tokens`.
//! - [`PromptPrefixSignature`]: content-free fingerprint of the
//!   cache-relevant request prefix (`tool definitions`, leading `system`
//!   instructions, `reasoning effort`, `model`).
//! - [`PromptPrefixChange`]: boolean diff between consecutive signatures.
//!
//! This is **not** a provider cache key. It only diagnoses whether
//! Doge-Code-side prefix components changed between requests. Never log raw
//! prompt / tool / user content from here; only short fingerprints, byte
//! counts, tool counts, boolean flags, token counts, and ratios.
//!
//! Provider-dependent prefix: deferred `tool_search` activation changes the
//! tool schema prefix for OpenAI-compatible Chat Completions (active schemas
//! move into the next request's top-level `tools`), but not for
//! `openai` Responses append-only wiring (stable base namespace;
//! activations append as `additional_tools` input suffix). Callers must pass
//! the wire-relevant tool set: stable base for Responses, live active for
//! compatible.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::config::ReasoningEffort;
use crate::llm::types::{ChatMessage, PromptTokensDetails, ToolDef};

/// Last-request + session totals for prompt-cache telemetry.
///
/// All fields are atomics so a shared [`crate::llm::client_core::OpenAIClient`]
/// stays `Clone`-shareable. `Option<u32>` semantics from the wire are
/// preserved: an absent field means "provider did not report", which is
/// distinct from an explicit `0`.
#[derive(Debug, Default)]
pub struct PromptCacheCounters {
    last_cached_tokens: AtomicU32,
    last_cache_write_tokens: AtomicU32,
    total_cached_tokens: AtomicU64,
    total_cache_write_tokens: AtomicU64,
    last_cached_reported: AtomicBool,
    last_cache_write_reported: AtomicBool,
    cached_usage_seen: AtomicBool,
    cache_write_usage_seen: AtomicBool,
}

impl PromptCacheCounters {
    /// Reset per-request state. Called at the start of every `record_usage`
    /// so a stale previous value can never leak into the next request.
    pub fn reset_last(&self) {
        self.last_cached_tokens.store(0, Ordering::Relaxed);
        self.last_cache_write_tokens.store(0, Ordering::Relaxed);
        self.last_cached_reported.store(false, Ordering::Relaxed);
        self.last_cache_write_reported
            .store(false, Ordering::Relaxed);
    }

    /// Record one response's `prompt_tokens_details`.
    /// `None` details (or absent fields) leave `seen` flags untouched.
    /// An explicit `Some(0)` still marks reported/seen as true.
    pub fn record(&self, details: Option<&PromptTokensDetails>) {
        self.reset_last();
        let Some(details) = details else {
            return;
        };
        if let Some(cached) = details.cached_tokens {
            self.last_cached_tokens.store(cached, Ordering::Relaxed);
            self.last_cached_reported.store(true, Ordering::Relaxed);
            self.cached_usage_seen.store(true, Ordering::Relaxed);
            self.total_cached_tokens
                .fetch_add(cached as u64, Ordering::Relaxed);
        }
        if let Some(written) = details.cache_write_tokens {
            self.last_cache_write_tokens
                .store(written, Ordering::Relaxed);
            self.last_cache_write_reported
                .store(true, Ordering::Relaxed);
            self.cache_write_usage_seen.store(true, Ordering::Relaxed);
            self.total_cache_write_tokens
                .fetch_add(written as u64, Ordering::Relaxed);
        }
    }

    /// Snapshot the last-request values for sub-agent isolation.
    pub fn snapshot_last(&self) -> PromptCacheUsageSnapshot {
        PromptCacheUsageSnapshot {
            cached_tokens: if self.last_cached_reported.load(Ordering::Relaxed) {
                Some(self.last_cached_tokens.load(Ordering::Relaxed))
            } else {
                None
            },
            cache_write_tokens: if self.last_cache_write_reported.load(Ordering::Relaxed) {
                Some(self.last_cache_write_tokens.load(Ordering::Relaxed))
            } else {
                None
            },
        }
    }

    /// Restore a previously snapshotted last-request state.
    /// Session totals are never restored here: sub-agent requests remain
    /// part of session cost.
    pub fn restore_last(&self, snapshot: PromptCacheUsageSnapshot) {
        match snapshot.cached_tokens {
            Some(v) => {
                self.last_cached_tokens.store(v, Ordering::Relaxed);
                self.last_cached_reported.store(true, Ordering::Relaxed);
            }
            None => {
                self.last_cached_tokens.store(0, Ordering::Relaxed);
                self.last_cached_reported.store(false, Ordering::Relaxed);
            }
        }
        match snapshot.cache_write_tokens {
            Some(v) => {
                self.last_cache_write_tokens.store(v, Ordering::Relaxed);
                self.last_cache_write_reported
                    .store(true, Ordering::Relaxed);
            }
            None => {
                self.last_cache_write_tokens.store(0, Ordering::Relaxed);
                self.last_cache_write_reported
                    .store(false, Ordering::Relaxed);
            }
        }
    }

    /// Reset session totals + seen flags + last-request state.
    pub fn clear(&self) {
        self.total_cached_tokens.store(0, Ordering::Relaxed);
        self.total_cache_write_tokens.store(0, Ordering::Relaxed);
        self.cached_usage_seen.store(false, Ordering::Relaxed);
        self.cache_write_usage_seen.store(false, Ordering::Relaxed);
        self.reset_last();
    }

    pub fn total_cached(&self) -> u64 {
        self.total_cached_tokens.load(Ordering::Relaxed)
    }

    pub fn total_cache_write(&self) -> u64 {
        self.total_cache_write_tokens.load(Ordering::Relaxed)
    }

    pub fn has_cached(&self) -> bool {
        self.cached_usage_seen.load(Ordering::Relaxed)
    }

    pub fn has_cache_write(&self) -> bool {
        self.cache_write_usage_seen.load(Ordering::Relaxed)
    }
}

/// Last-request cache values. `None` means "not reported by the provider",
/// which is distinct from an explicit `0`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PromptCacheUsageSnapshot {
    pub cached_tokens: Option<u32>,
    pub cache_write_tokens: Option<u32>,
}

/// Session-level cache totals plus the denominator they relate to.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PromptCacheSessionUsage {
    pub total_cached_tokens: u64,
    pub total_cache_write_tokens: u64,
    pub total_prompt_tokens: u64,
}

impl PromptCacheSessionUsage {
    /// Session read ratio `total_cached / total_prompt`.
    /// `None` when the denominator is zero or the ratio is non-finite.
    pub fn hit_ratio(&self) -> Option<f64> {
        if self.total_prompt_tokens == 0 {
            return None;
        }
        let ratio = self.total_cached_tokens as f64 / self.total_prompt_tokens as f64;
        if ratio.is_finite() { Some(ratio) } else { None }
    }
}

/// Format a `0.0..=1.0`-style ratio as `70.0%` with one decimal.
/// Returns `None` for non-finite input so callers never display NaN/inf.
pub fn format_hit_ratio_percent(ratio: f64) -> Option<String> {
    if !ratio.is_finite() {
        return None;
    }
    Some(format!("{:.1}%", ratio * 100.0))
}

/// Client-side stable-prefix diagnostic signature.
///
/// This is **not** a provider cache key and must never be named like one
/// (`cache_key`, `provider_cache_hash`, ...). It only records whether
/// Doge-Code-side cache-relevant components changed between requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptPrefixSignature {
    pub tool_schema_hash: blake3::Hash,
    pub leading_system_hash: blake3::Hash,
    pub tool_count: usize,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub model: String,
}

/// Boolean diff between consecutive prefix signatures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PromptPrefixChange {
    pub model_changed: bool,
    pub tool_schema_changed: bool,
    pub leading_system_changed: bool,
    pub reasoning_effort_changed: bool,
}

impl PromptPrefixChange {
    /// True when any cache-relevant component changed.
    pub fn any_changed(&self) -> bool {
        self.model_changed
            || self.tool_schema_changed
            || self.leading_system_changed
            || self.reasoning_effort_changed
    }
}

impl PromptPrefixSignature {
    /// Diff against the previous signature.
    /// `None` (first request) yields all-false: there is no change yet.
    pub fn diff(&self, previous: Option<&Self>) -> PromptPrefixChange {
        let Some(prev) = previous else {
            return PromptPrefixChange::default();
        };
        PromptPrefixChange {
            model_changed: self.model != prev.model,
            tool_schema_changed: self.tool_schema_hash != prev.tool_schema_hash
                || self.tool_count != prev.tool_count,
            leading_system_changed: self.leading_system_hash != prev.leading_system_hash,
            reasoning_effort_changed: self.reasoning_effort != prev.reasoning_effort,
        }
    }

    /// Short hex prefix for debug display (16 chars). Internal comparison
    /// always uses the full BLAKE3 hash.
    pub fn short_tool_hash(&self) -> String {
        short_hash(&self.tool_schema_hash)
    }

    /// Short hex prefix for debug display (16 chars).
    pub fn short_system_hash(&self) -> String {
        short_hash(&self.leading_system_hash)
    }
}

/// First 16 hex chars of a BLAKE3 hash for debug display.
pub fn short_hash(hash: &blake3::Hash) -> String {
    hash.to_hex().as_str()[..16].to_owned()
}

/// Streaming writer that feeds serialized JSON bytes into a BLAKE3 hasher
/// without allocating a temporary buffer (same pattern as
/// `context_budget::serialized_size`'s `CountingWriter`).
struct Blake3Writer<'a> {
    hasher: &'a mut blake3::Hasher,
}

impl std::io::Write for Blake3Writer<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.hasher.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Deterministic fingerprint of the exact tool schemas about to be sent,
/// in send order. Ordering is cache-relevant, so tools are never sorted.
pub fn tool_schema_fingerprint(tools: &[ToolDef]) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    for tool in tools {
        {
            let mut writer = Blake3Writer {
                hasher: &mut hasher,
            };
            if serde_json::to_writer(&mut writer, tool).is_err() {
                // Serialization of in-memory ToolDef should be infallible
                // (the writer never fails); fall back to the tool name so a
                // pathological schema still perturbs the fingerprint.
                // Note: `writer` still borrows `hasher` here, so feed the
                // fallback through the writer path instead of `hasher`
                // directly to satisfy the borrow checker.
                use std::io::Write as _;
                let _ = writer.write_all(tool.function.name.as_bytes());
            }
        }
        // Frame each tool so concatenations stay unambiguous.
        hasher.update(&[0x00]);
    }
    hasher.finalize()
}

/// True when a message body is the rendered one-shot runtime overlay.
///
/// Matches the envelope shape produced by
/// [`crate::llm::runtime_context::RuntimeContextSnapshot::render`]
/// (`"<RuntimeContext>\n…\n</RuntimeContext>"`), not a bare substring, so a
/// project instruction that merely mentions the marker mid-text is still
/// treated as stable-prefix content.
fn is_runtime_overlay_content(content: Option<&str>) -> bool {
    let Some(content) = content else {
        return false;
    };
    let trimmed = content.trim_start();
    trimmed.starts_with("<RuntimeContext>") && trimmed.contains("</RuntimeContext>")
}

/// Fingerprint of the leading contiguous `role == "system"` messages only.
///
/// The conversation suffix always grows, so hashing the whole history would
/// report "changed" on every request. Only the stable prefix is observed.
/// The one-shot `<RuntimeContext>` overlay is explicitly excluded: it is a
/// transient bootstrap hint, not part of the stable prefix.
///
/// Callers should pass canonical history (which never contains the overlay);
/// the envelope check below is defense-in-depth for projected inputs.
pub fn leading_system_fingerprint(messages: &[ChatMessage]) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    for msg in messages.iter().take_while(|m| m.role == "system") {
        if is_runtime_overlay_content(msg.content.as_deref()) {
            continue;
        }
        // Presence flag distinguishes None from Some("").
        hasher.update(if msg.content.is_some() {
            &[0x01]
        } else {
            &[0x00]
        });
        if let Some(content) = msg.content.as_deref() {
            hasher.update(content.as_bytes());
        }
        // Frame each message so concatenations stay unambiguous.
        hasher.update(&[0x00]);
    }
    hasher.finalize()
}

/// Build the client-side prefix signature for the exact request about to be
/// sent. Call after preflight-governor reductions with the final
/// `active_tools` snapshot; `messages` should be the canonical history
/// (overlay excluded by [`leading_system_fingerprint`] in any case).
pub fn compute_prefix_signature(
    model: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    reasoning_effort: Option<ReasoningEffort>,
) -> PromptPrefixSignature {
    PromptPrefixSignature {
        tool_schema_hash: tool_schema_fingerprint(tools),
        leading_system_hash: leading_system_fingerprint(messages),
        tool_count: tools.len(),
        reasoning_effort,
        model: model.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::ToolFunctionDef;

    fn tool_def(name: &str, description: &str, params: serde_json::Value) -> ToolDef {
        ToolDef {
            kind: "function".into(),
            function: ToolFunctionDef {
                name: name.into(),
                description: description.into(),
                parameters: params,
                strict: None,
            },
        }
    }

    fn system_msg(content: &str) -> ChatMessage {
        ChatMessage {
            reasoning: Default::default(),
            provider_state: None,
            role: "system".into(),
            content: Some(content.into()),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    fn user_msg(content: &str) -> ChatMessage {
        ChatMessage {
            reasoning: Default::default(),
            provider_state: None,
            role: "user".into(),
            content: Some(content.into()),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    #[test]
    fn test_counters_record_and_reset_last() {
        let counters = PromptCacheCounters::default();
        counters.record(Some(&PromptTokensDetails {
            cached_tokens: Some(8000),
            cache_write_tokens: Some(2000),
            extra: Default::default(),
        }));
        let snap = counters.snapshot_last();
        assert_eq!(snap.cached_tokens, Some(8000));
        assert_eq!(snap.cache_write_tokens, Some(2000));
        assert!(counters.has_cached());
        assert!(counters.has_cache_write());
        assert_eq!(counters.total_cached(), 8000);
        assert_eq!(counters.total_cache_write(), 2000);

        // Absent details reset last state without touching seen/totals.
        counters.record(None);
        let snap = counters.snapshot_last();
        assert_eq!(snap.cached_tokens, None);
        assert_eq!(snap.cache_write_tokens, None);
        assert!(counters.has_cached());
        assert_eq!(counters.total_cached(), 8000);
    }

    #[test]
    fn test_counters_explicit_zero_marks_seen() {
        let counters = PromptCacheCounters::default();
        counters.record(Some(&PromptTokensDetails {
            cached_tokens: Some(0),
            cache_write_tokens: Some(0),
            extra: Default::default(),
        }));
        let snap = counters.snapshot_last();
        assert_eq!(snap.cached_tokens, Some(0));
        assert_eq!(snap.cache_write_tokens, Some(0));
        assert!(counters.has_cached());
        assert!(counters.has_cache_write());
    }

    #[test]
    fn test_counters_clear_resets_everything() {
        let counters = PromptCacheCounters::default();
        counters.record(Some(&PromptTokensDetails {
            cached_tokens: Some(100),
            cache_write_tokens: Some(50),
            extra: Default::default(),
        }));
        counters.clear();
        assert_eq!(counters.total_cached(), 0);
        assert_eq!(counters.total_cache_write(), 0);
        assert!(!counters.has_cached());
        assert!(!counters.has_cache_write());
        let snap = counters.snapshot_last();
        assert_eq!(snap.cached_tokens, None);
        assert_eq!(snap.cache_write_tokens, None);
    }

    #[test]
    fn test_snapshot_restore_roundtrip() {
        let counters = PromptCacheCounters::default();
        counters.record(Some(&PromptTokensDetails {
            cached_tokens: Some(6000),
            cache_write_tokens: None,
            extra: Default::default(),
        }));
        let saved = counters.snapshot_last();
        counters.record(Some(&PromptTokensDetails {
            cached_tokens: Some(2000),
            cache_write_tokens: Some(100),
            extra: Default::default(),
        }));
        counters.restore_last(saved);
        let restored = counters.snapshot_last();
        assert_eq!(restored.cached_tokens, Some(6000));
        assert_eq!(restored.cache_write_tokens, None);
        // Totals keep accumulating; restore never rewinds them.
        assert_eq!(counters.total_cached(), 8000);
    }

    #[test]
    fn test_session_hit_ratio() {
        let usage = PromptCacheSessionUsage {
            total_cached_tokens: 15_000,
            total_cache_write_tokens: 0,
            total_prompt_tokens: 20_000,
        };
        let ratio = usage.hit_ratio().expect("ratio");
        assert!((ratio - 0.75).abs() < f64::EPSILON);
        let zero = PromptCacheSessionUsage::default();
        assert!(zero.hit_ratio().is_none());
    }

    #[test]
    fn test_format_hit_ratio_percent_rejects_non_finite() {
        assert_eq!(format_hit_ratio_percent(0.704), Some("70.4%".to_string()));
        assert!(format_hit_ratio_percent(f64::NAN).is_none());
        assert!(format_hit_ratio_percent(f64::INFINITY).is_none());
    }

    #[test]
    fn test_prefix_signature_deterministic() {
        let tools = vec![
            tool_def("a", "desc a", serde_json::json!({"type":"object"})),
            tool_def("b", "desc b", serde_json::json!({"type":"object"})),
        ];
        let messages = vec![system_msg("prompt"), user_msg("hi")];
        let first = compute_prefix_signature("m", &messages, &tools, Some(ReasoningEffort::Medium));
        let second =
            compute_prefix_signature("m", &messages, &tools, Some(ReasoningEffort::Medium));
        assert_eq!(first, second);
        assert_eq!(first.diff(Some(&second)), PromptPrefixChange::default());
    }

    #[test]
    fn test_tool_ordering_changes_fingerprint() {
        let ab = vec![
            tool_def("a", "d", serde_json::json!({"type":"object"})),
            tool_def("b", "d", serde_json::json!({"type":"object"})),
        ];
        let ba = vec![
            tool_def("b", "d", serde_json::json!({"type":"object"})),
            tool_def("a", "d", serde_json::json!({"type":"object"})),
        ];
        assert_ne!(tool_schema_fingerprint(&ab), tool_schema_fingerprint(&ba));
        let messages = vec![system_msg("s")];
        let sig_ab = compute_prefix_signature("m", &messages, &ab, None);
        let sig_ba = compute_prefix_signature("m", &messages, &ba, None);
        let change = sig_ba.diff(Some(&sig_ab));
        assert!(change.tool_schema_changed);
        assert!(!change.leading_system_changed);
    }

    #[test]
    fn test_tool_schema_change_detected() {
        let before = vec![tool_def(
            "edit",
            "old description",
            serde_json::json!({"type":"object"}),
        )];
        let after = vec![tool_def(
            "edit",
            "new description",
            serde_json::json!({"type":"object"}),
        )];
        assert_ne!(
            tool_schema_fingerprint(&before),
            tool_schema_fingerprint(&after)
        );
        // Parameter / strict changes also perturb the fingerprint.
        let strict_before = vec![ToolDef {
            kind: "function".into(),
            function: ToolFunctionDef {
                name: "edit".into(),
                description: "d".into(),
                parameters: serde_json::json!({"type":"object"}),
                strict: None,
            },
        }];
        let strict_after = vec![ToolDef {
            kind: "function".into(),
            function: ToolFunctionDef {
                name: "edit".into(),
                description: "d".into(),
                parameters: serde_json::json!({"type":"object"}),
                strict: Some(true),
            },
        }];
        assert_ne!(
            tool_schema_fingerprint(&strict_before),
            tool_schema_fingerprint(&strict_after)
        );
    }

    #[test]
    fn test_stable_repeated_request_ignores_suffix() {
        let tools = vec![tool_def("a", "d", serde_json::json!({"type":"object"}))];
        let first = vec![system_msg("stable"), user_msg("one")];
        let mut second = first.clone();
        second.push(ChatMessage {
            reasoning: Default::default(),
            provider_state: None,
            role: "assistant".into(),
            content: Some("answer".into()),
            tool_calls: vec![],
            tool_call_id: None,
        });
        second.push(user_msg("two"));
        let sig_first = compute_prefix_signature("m", &first, &tools, Some(ReasoningEffort::Low));
        let sig_second = compute_prefix_signature("m", &second, &tools, Some(ReasoningEffort::Low));
        assert_eq!(sig_first, sig_second);
    }

    #[test]
    fn test_reasoning_effort_change_only() {
        let tools = vec![tool_def("a", "d", serde_json::json!({"type":"object"}))];
        let messages = vec![system_msg("s")];
        let medium =
            compute_prefix_signature("m", &messages, &tools, Some(ReasoningEffort::Medium));
        let low = compute_prefix_signature("m", &messages, &tools, Some(ReasoningEffort::Low));
        let change = low.diff(Some(&medium));
        assert!(change.reasoning_effort_changed);
        assert!(!change.tool_schema_changed);
        assert!(!change.leading_system_changed);
        assert!(!change.model_changed);
    }

    #[test]
    fn test_system_prefix_change_detected() {
        let tools = vec![tool_def("a", "d", serde_json::json!({"type":"object"}))];
        let before = vec![system_msg("before"), user_msg("hi")];
        let after = vec![system_msg("after"), user_msg("hi")];
        let sig_before = compute_prefix_signature("m", &before, &tools, None);
        let sig_after = compute_prefix_signature("m", &after, &tools, None);
        let change = sig_after.diff(Some(&sig_before));
        assert!(change.leading_system_changed);
        assert!(!change.tool_schema_changed);
    }

    #[test]
    fn test_model_change_detected() {
        let tools = vec![tool_def("a", "d", serde_json::json!({"type":"object"}))];
        let messages = vec![system_msg("s")];
        let first = compute_prefix_signature("model-a", &messages, &tools, None);
        let second = compute_prefix_signature("model-b", &messages, &tools, None);
        let change = second.diff(Some(&first));
        assert!(change.model_changed);
        assert!(!change.tool_schema_changed);
    }

    #[test]
    fn test_runtime_overlay_excluded_from_system_prefix() {
        let tools = vec![tool_def("a", "d", serde_json::json!({"type":"object"}))];
        let base = vec![system_msg("stable"), user_msg("hi")];
        let mut with_overlay = vec![system_msg("stable")];
        with_overlay.push(ChatMessage {
            reasoning: Default::default(),
            provider_state: None,
            role: "system".into(),
            content: Some("<RuntimeContext>\nrecent\n</RuntimeContext>".into()),
            tool_calls: vec![],
            tool_call_id: None,
        });
        with_overlay.push(user_msg("hi"));
        let sig_base = compute_prefix_signature("m", &base, &tools, None);
        let sig_overlay = compute_prefix_signature("m", &with_overlay, &tools, None);
        // Overlay-only difference must not perturb the stable prefix.
        assert_eq!(
            sig_base.leading_system_hash,
            sig_overlay.leading_system_hash
        );
    }

    #[test]
    fn test_mid_text_marker_mention_is_stable_content() {
        // A project instruction that merely mentions the marker mid-text is
        // stable-prefix content, not the overlay envelope, so it must perturb
        // the fingerprint.
        assert!(!is_runtime_overlay_content(Some(
            "See docs about <RuntimeContext> handling for details"
        )));
        assert!(!is_runtime_overlay_content(None));
        assert!(is_runtime_overlay_content(Some(
            "<RuntimeContext>\nrecent\n</RuntimeContext>"
        )));
        assert!(is_runtime_overlay_content(Some(
            "  <RuntimeContext>\nrecent\n</RuntimeContext>"
        )));
        let tools = vec![tool_def("a", "d", serde_json::json!({"type":"object"}))];
        let before = vec![
            system_msg("See docs about <RuntimeContext> handling"),
            user_msg("hi"),
        ];
        let after = vec![system_msg("different content"), user_msg("hi")];
        let sig_before = compute_prefix_signature("m", &before, &tools, None);
        let sig_after = compute_prefix_signature("m", &after, &tools, None);
        assert_ne!(
            sig_before.leading_system_hash,
            sig_after.leading_system_hash
        );
    }

    #[test]
    fn test_short_hash_length() {
        let tools = vec![tool_def("a", "d", serde_json::json!({"type":"object"}))];
        let hash = tool_schema_fingerprint(&tools);
        assert_eq!(short_hash(&hash).len(), 16);
    }

    #[test]
    fn test_deferred_activation_changes_schema() {
        // Initial: core tools + tool_search. After activation: core + newly
        // activated tool. The fingerprint must change.
        let initial = vec![
            tool_def("fs_read", "read", serde_json::json!({"type":"object"})),
            tool_def(
                "tool_search",
                "search",
                serde_json::json!({"type":"object"}),
            ),
        ];
        let mut activated = initial.clone();
        activated.push(tool_def(
            "edit",
            "edit file",
            serde_json::json!({"type":"object"}),
        ));
        // Keep send-order contract: ToolCatalog emits stable name order,
        // so sort to mirror the real request order before hashing.
        activated.sort_by(|a, b| a.function.name.cmp(&b.function.name));
        let messages = vec![system_msg("stable")];
        let before =
            compute_prefix_signature("m", &messages, &initial, Some(ReasoningEffort::Medium));
        let after =
            compute_prefix_signature("m", &messages, &activated, Some(ReasoningEffort::Medium));
        let change = after.diff(Some(&before));
        assert!(change.tool_schema_changed);
        assert!(!change.leading_system_changed);
        assert!(!change.reasoning_effort_changed);
        assert!(!change.model_changed);
    }

    #[test]
    fn test_discovery_retirement_changes_schema_deterministically() {
        // Retirement is observable as a prefix change (tool_search leaves the
        // schema surface) and recomputation stays stable.
        let initial = vec![
            tool_def("edit", "edit file", serde_json::json!({"type":"object"})),
            tool_def("fs_read", "read", serde_json::json!({"type":"object"})),
            tool_def(
                "tool_search",
                "search",
                serde_json::json!({"type":"object"}),
            ),
        ];
        let retired = vec![
            tool_def("edit", "edit file", serde_json::json!({"type":"object"})),
            tool_def("fs_read", "read", serde_json::json!({"type":"object"})),
        ];
        assert_ne!(
            tool_schema_fingerprint(&initial),
            tool_schema_fingerprint(&retired)
        );
        assert_eq!(
            tool_schema_fingerprint(&retired),
            tool_schema_fingerprint(&retired)
        );
        let messages = vec![system_msg("stable")];
        let before = compute_prefix_signature("m", &messages, &initial, None);
        let after = compute_prefix_signature("m", &messages, &retired, None);
        let change = after.diff(Some(&before));
        assert!(change.tool_schema_changed);
        assert!(!change.leading_system_changed);
        assert!(!change.model_changed);
    }

    #[test]
    fn test_first_request_has_no_change() {
        let tools = vec![tool_def("a", "d", serde_json::json!({"type":"object"}))];
        let messages = vec![system_msg("s")];
        let sig = compute_prefix_signature("m", &messages, &tools, None);
        assert_eq!(sig.diff(None), PromptPrefixChange::default());
        assert!(!sig.diff(None).any_changed());
    }
}

#[cfg(test)]
mod append_only_tests {
    use super::*;

    fn tool_def(name: &str) -> crate::llm::types::ToolDef {
        crate::llm::types::ToolDef {
            kind: "function".into(),
            function: crate::llm::types::ToolFunctionDef {
                name: name.into(),
                description: format!("{name} helper"),
                parameters: serde_json::json!({"type":"object"}),
                strict: None,
            },
        }
    }

    fn sys_msg() -> ChatMessage {
        ChatMessage {
            reasoning: Default::default(),
            provider_state: None,
            role: "system".into(),
            content: Some("stable".into()),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    #[test]
    fn test_responses_base_stable_generic_changes() {
        let base = vec![tool_def("fs_read"), tool_def("tool_search")];
        let mut active = base.clone();
        active.push(tool_def("edit"));
        active.sort_by(|a, b| a.function.name.cmp(&b.function.name));
        let messages = vec![sys_msg()];
        // Responses: stable base -> no change.
        let before = compute_prefix_signature("m", &messages, &base, None);
        let after = compute_prefix_signature("m", &messages, &base, None);
        assert!(!after.diff(Some(&before)).tool_schema_changed);
        // Generic: live active set -> change detected.
        let before_g = compute_prefix_signature("m", &messages, &base, None);
        let after_g = compute_prefix_signature("m", &messages, &active, None);
        assert!(after_g.diff(Some(&before_g)).tool_schema_changed);
    }

    #[test]
    fn test_cache_counters_preserve_explicit_zero_and_hits() {
        let counters = PromptCacheCounters::default();
        counters.record(Some(&crate::llm::types::PromptTokensDetails {
            cached_tokens: Some(0),
            cache_write_tokens: Some(0),
            extra: Default::default(),
        }));
        assert_eq!(counters.snapshot_last().cached_tokens, Some(0));
        counters.record(Some(&crate::llm::types::PromptTokensDetails {
            cached_tokens: Some(5000),
            cache_write_tokens: None,
            extra: Default::default(),
        }));
        assert_eq!(counters.snapshot_last().cached_tokens, Some(5000));
        assert_eq!(counters.total_cached(), 5000);
    }
}
