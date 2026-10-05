//! Observation-aware preflight context governor (v1).
//!
//! Measures the *current* request footprint
//! (`canonical conversation + runtime overlay + active tool schemas +
//! request overhead`) before sending, classifies budget pressure, and lets
//! the agent loop reduce pressure in cache-aware order:
//! runtime overlay drop -> recoverable Observation Store offload ->
//! unseen-safe compaction (last resort).
//!
//! This module is pure measurement + decision. It never performs HTTP,
//! tool execution, conversation compaction, TUI, or Observation Store
//! mutation. History mutation lives in `HistoryManager`; orchestration
//! lives in the agent loop.

use crate::llm::types::{ChatMessage, ToolDef};
use anyhow::Result;

/// Fixed request overhead bytes covering `model`, `reasoning_effort`,
/// field names, and object delimiters. Messages/tools dominate, so a small
/// constant margin is sufficient for v1.
pub const REQUEST_FIXED_OVERHEAD_BYTES: u64 = 256;

/// Conservative bytes-per-token divisor for the bootstrap heuristic.
/// Deliberately conservative (3 bytes/token, not 4) so first-request
/// estimates err toward caution without a provider tokenizer.
pub const HEURISTIC_BYTES_PER_TOKEN: u64 = 3;

/// Fixed margin added to every heuristic estimate.
pub const HEURISTIC_MARGIN_TOKENS: u64 = 512;

/// Divisor for payload-shrink calibration. Shrinks are discounted
/// (divide by 6, not 3) so token reductions are never over-estimated.
pub const SHRINK_BYTES_PER_TOKEN: u64 = 6;

/// Cleanup boundary as a percentage of the effective compaction limit.
/// Matches the existing Observation offload trigger (`effective * 60%`).
pub const CLEANUP_THRESHOLD_PERCENT: u64 = 60;

/// Writer that counts serialized JSON bytes without allocating the payload.
#[derive(Default)]
struct CountingWriter {
    bytes: u64,
}

impl std::io::Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buf.len() as u64);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Measure serialized JSON size without building a temporary buffer.
///
/// Avoids `serde_json::to_vec(value)?.len()` which would allocate a second
/// copy of an already-large request purely for diagnostics.
pub fn serialized_size<T: serde::Serialize + ?Sized>(value: &T) -> Result<u64> {
    let mut writer = CountingWriter::default();
    serde_json::to_writer(&mut writer, value)
        .map_err(|e| anyhow::anyhow!("serialize size measurement failed: {e}"))?;
    Ok(writer.bytes)
}

/// Current-request footprint in serialized JSON bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestFootprint {
    /// Serialized bytes of the provider-bound messages (overlay included).
    pub message_json_bytes: u64,
    /// Serialized bytes of the active tool schemas.
    pub tool_schema_json_bytes: u64,
    /// Fixed request overhead (model, field names, delimiters).
    pub request_overhead_bytes: u64,
    /// Total bytes driving the token estimate.
    pub total_json_bytes: u64,
    /// Overlay portion of `message_json_bytes` (telemetry only).
    pub runtime_overlay_bytes: u64,
}

impl RequestFootprint {
    pub fn new(
        message_json_bytes: u64,
        tool_schema_json_bytes: u64,
        runtime_overlay_bytes: u64,
    ) -> Self {
        let total = message_json_bytes
            .saturating_add(tool_schema_json_bytes)
            .saturating_add(REQUEST_FIXED_OVERHEAD_BYTES);
        Self {
            message_json_bytes,
            tool_schema_json_bytes,
            request_overhead_bytes: REQUEST_FIXED_OVERHEAD_BYTES,
            total_json_bytes: total,
            runtime_overlay_bytes,
        }
    }
}

/// Where a token estimate came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenEstimateSource {
    /// Local bytes heuristic only (no provider usage observed yet).
    Heuristic,
    /// Previous provider `usage.prompt_tokens` plus conservative delta.
    Calibrated,
    /// Reserved for a future provider token-count API. Never produced in v1.
    ProviderExact,
}

/// Estimated prompt tokens for the current footprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenEstimate {
    pub prompt_tokens: u64,
    pub source: TokenEstimateSource,
}

/// Last observed provider usage for calibration. Governor-local only;
/// never persisted to session state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservedRequestUsage {
    pub footprint_bytes: u64,
    pub actual_prompt_tokens: u64,
}

/// Budget pressure classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetPressure {
    Healthy,
    Cleanup,
    Compact,
}

/// Ceiling division for `u64` without overflow panics.
fn ceil_div_u64(num: u64, denom: u64) -> u64 {
    if denom == 0 {
        return num;
    }
    (num as u128).div_ceil(denom as u128) as u64
}

/// Cleanup threshold: `effective_limit * 60 / 100` with `u128` intermediate.
pub fn cleanup_threshold(effective_limit: u64) -> u64 {
    ((effective_limit as u128 * CLEANUP_THRESHOLD_PERCENT as u128) / 100) as u64
}

/// Heuristic estimate: `ceil(total_bytes / 3) + 512`.
pub fn heuristic_estimate(total_bytes: u64) -> u64 {
    ceil_div_u64(total_bytes, HEURISTIC_BYTES_PER_TOKEN).saturating_add(HEURISTIC_MARGIN_TOKENS)
}

/// Calibrated estimate given the previous observation.
///
/// Growth is strict (`ceil(growth / 3)` added); shrink is conservative
/// (`floor(shrink / 6)` subtracted) so reductions never collapse the
/// estimate proportionally.
pub fn calibrated_estimate(current_bytes: u64, previous: ObservedRequestUsage) -> u64 {
    if current_bytes >= previous.footprint_bytes {
        let growth = current_bytes.saturating_sub(previous.footprint_bytes);
        previous
            .actual_prompt_tokens
            .saturating_add(ceil_div_u64(growth, HEURISTIC_BYTES_PER_TOKEN))
    } else {
        let shrink = previous.footprint_bytes.saturating_sub(current_bytes);
        previous
            .actual_prompt_tokens
            .saturating_sub(shrink / SHRINK_BYTES_PER_TOKEN)
    }
}

/// Pure measurement + decision governor. Holds only the last observed
/// provider usage; never mutates history.
pub struct ContextBudgetGovernor {
    config: crate::config::ContextBudgetConfig,
    previous_usage: Option<ObservedRequestUsage>,
}

impl ContextBudgetGovernor {
    pub fn new(config: crate::config::ContextBudgetConfig) -> Self {
        Self {
            config,
            previous_usage: None,
        }
    }

    pub fn config(&self) -> &crate::config::ContextBudgetConfig {
        &self.config
    }

    pub fn previous_usage(&self) -> Option<ObservedRequestUsage> {
        self.previous_usage
    }

    /// Measure the current request footprint without allocating the payload.
    pub fn measure(&self, messages: &[ChatMessage], tools: &[ToolDef]) -> Result<RequestFootprint> {
        let message_json_bytes = serialized_size(&messages)?;
        let tool_schema_json_bytes = serialized_size(&tools)?;
        Ok(RequestFootprint::new(
            message_json_bytes,
            tool_schema_json_bytes,
            0,
        ))
    }

    /// Measure the Responses wire projection, excluding opaque ciphertext from
    /// the text-token heuristic. Previous actual usage still calibrates pressure.
    /// The `compact_threshold` must match the actual request so the projection
    /// does not drift (`context_management` itself is tiny but part of the wire).
    pub fn measure_subscription(
        &self,
        account: &str,
        model: &str,
        messages: &[ChatMessage],
        tools: &[ToolDef],
        overlay: u64,
        compact_threshold: Option<u32>,
    ) -> Result<RequestFootprint> {
        self.measure_subscription_with_activation(
            account,
            model,
            messages,
            tools,
            tools,
            overlay,
            compact_threshold,
        )
    }

    /// Append-only Responses measurement: `base_tools` drive the stable
    /// top-level schema bytes; `active_tools` resolve `additional_tools`
    /// input suffix bytes. Never double-counts the same schema in both.
    #[allow(clippy::too_many_arguments)]
    pub fn measure_subscription_with_activation(
        &self,
        account: &str,
        model: &str,
        messages: &[ChatMessage],
        base_tools: &[ToolDef],
        active_tools: &[ToolDef],
        overlay: u64,
        compact_threshold: Option<u32>,
    ) -> Result<RequestFootprint> {
        let mut request = crate::features::openai_subscription::responses::build_with_activation(
            model,
            account,
            messages,
            base_tools,
            active_tools,
            None,
            compact_threshold,
        )?;
        let mut opaque_items = 0u64;
        for item in &mut request.input {
            if let Some(object) = item.as_object_mut()
                && object.remove("encrypted_content").is_some()
            {
                opaque_items += 1;
            }
        }
        let mut messages =
            serialized_size(&request.input)?.saturating_add(opaque_items.saturating_mul(1536));
        // Keep the projection honest with the actual request: include the
        // tiny `context_management` envelope when native compaction is on.
        if compact_threshold.is_some() {
            let envelope = serialized_size(&serde_json::json!({
                "context_management": [{"type": "compaction", "compact_threshold": compact_threshold}]
            }))
            .unwrap_or(0);
            messages = messages.saturating_add(envelope);
        }
        Ok(RequestFootprint::new(
            messages,
            serialized_size(&request.tools)?,
            overlay,
        ))
    }

    /// Measure with a separately known overlay size for telemetry.
    /// `message_json_bytes` already includes the overlay; this only records
    /// the overlay portion without changing the total.
    pub fn measure_with_overlay(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDef],
        runtime_overlay_bytes: u64,
    ) -> Result<RequestFootprint> {
        let mut footprint = self.measure(messages, tools)?;
        footprint.runtime_overlay_bytes = runtime_overlay_bytes;
        Ok(footprint)
    }

    /// Estimate prompt tokens: `max(heuristic, calibrated)`.
    /// Heuristic-only when no provider usage has been observed.
    pub fn estimate(&self, footprint: RequestFootprint) -> TokenEstimate {
        let heuristic = heuristic_estimate(footprint.total_json_bytes);
        match self.previous_usage {
            None => TokenEstimate {
                prompt_tokens: heuristic,
                source: TokenEstimateSource::Heuristic,
            },
            Some(previous) => {
                let calibrated = calibrated_estimate(footprint.total_json_bytes, previous);
                TokenEstimate {
                    prompt_tokens: heuristic.max(calibrated),
                    source: TokenEstimateSource::Calibrated,
                }
            }
        }
    }

    /// Classify pressure against the effective compaction limit.
    ///
    /// `estimate < cleanup => Healthy`,
    /// `cleanup <= estimate <= effective => Cleanup`,
    /// `estimate > effective => Compact`.
    pub fn classify(&self, estimate: TokenEstimate, effective_limit: u64) -> BudgetPressure {
        let cleanup = cleanup_threshold(effective_limit);
        if estimate.prompt_tokens < cleanup {
            BudgetPressure::Healthy
        } else if estimate.prompt_tokens <= effective_limit {
            BudgetPressure::Cleanup
        } else {
            BudgetPressure::Compact
        }
    }

    /// Record the actual provider usage for the footprint just sent.
    /// Call immediately after a successful response, before sub-agent
    /// execution can overwrite the shared per-request counter.
    pub fn observe_actual(&mut self, footprint: RequestFootprint, prompt_tokens: u32) {
        self.previous_usage = Some(ObservedRequestUsage {
            footprint_bytes: footprint.total_json_bytes,
            actual_prompt_tokens: prompt_tokens as u64,
        });
    }

    /// Reset calibration after a native server-side compaction boundary.
    /// The pre-compaction byte footprint no longer predicts post-compaction
    /// token usage, so the next normal response re-establishes the ratio.
    pub fn reset_calibration(&mut self) {
        self.previous_usage = None;
    }

    /// Alias honoring the spec's governor naming for native compaction.
    pub fn reset_after_native_compaction(&mut self) {
        self.reset_calibration();
    }
}

/// Whether preflight should attempt recoverable offload for this pressure.
/// Both `Cleanup` and `Compact` offload seen history first; `Healthy`
/// sends as-is.
pub fn should_offload_for_pressure(pressure: BudgetPressure) -> bool {
    matches!(pressure, BudgetPressure::Cleanup | BudgetPressure::Compact)
}

/// Whether preflight may compact for this pressure/source pair.
///
/// Only calibrated `Compact` estimates compact proactively. Heuristic-only
/// `Compact` best-effort sends to avoid false-positive LLM compaction and
/// prompt-cache destruction. Provider-confirmed
/// `context_length_exceeded` bypasses this gate via the reactive path.
pub fn should_compact_for_pressure(pressure: BudgetPressure, source: TokenEstimateSource) -> bool {
    matches!(pressure, BudgetPressure::Compact) && matches!(source, TokenEstimateSource::Calibrated)
}

/// One-retry guard for provider-confirmed overflow.
///
/// Success resets the guard; a second consecutive overflow errors instead
/// of compacting again (the protected unseen suffix would be unchanged, so
/// retrying the same bytes is futile).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReactiveRetryGuard {
    used: bool,
}

impl ReactiveRetryGuard {
    pub fn new() -> Self {
        Self { used: false }
    }

    /// True when a reactive compaction attempt is still allowed.
    pub fn should_attempt(&self) -> bool {
        !self.used
    }

    /// Record a reactive compaction attempt (successfully compacted).
    pub fn record_attempt(&mut self) {
        self.used = true;
    }

    /// Reset after any successful provider response.
    pub fn record_success(&mut self) {
        self.used = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ContextBudgetConfig;
    use crate::llm::types::{ChatMessage, ToolCall, ToolCallFunction, ToolDef, ToolFunctionDef};

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            provider_state: None,
            role: role.into(),
            content: Some(content.into()),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    fn tool_msg(id: &str, content: &str) -> ChatMessage {
        ChatMessage {
            provider_state: None,
            role: "tool".into(),
            content: Some(content.into()),
            tool_calls: vec![],
            tool_call_id: Some(id.into()),
        }
    }

    fn assistant_call(id: &str) -> ChatMessage {
        ChatMessage {
            provider_state: None,
            role: "assistant".into(),
            content: None,
            tool_calls: vec![ToolCall {
                id: Some(id.into()),
                r#type: "function".into(),
                function: ToolCallFunction {
                    name: "fs_read".into(),
                    arguments: "{}".into(),
                },
            }],
            tool_call_id: None,
        }
    }

    fn tool_def(name: &str, description: &str, properties: serde_json::Value) -> ToolDef {
        ToolDef {
            kind: "function".into(),
            function: ToolFunctionDef {
                name: name.into(),
                description: description.into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": properties,
                }),
                strict: None,
            },
        }
    }

    fn governor() -> ContextBudgetGovernor {
        ContextBudgetGovernor::new(ContextBudgetConfig::default())
    }

    #[test]
    fn test_message_growth_increases_footprint() {
        let g = governor();
        let tools: Vec<ToolDef> = vec![];
        let small = g
            .measure(&[msg("user", "hi")], &tools)
            .expect("measure small");
        let large = g
            .measure(&[msg("user", "hi"), msg("user", &"x".repeat(5000))], &tools)
            .expect("measure large");
        assert!(large.message_json_bytes > small.message_json_bytes);
        assert!(large.total_json_bytes > small.total_json_bytes);
        assert_eq!(
            large.total_json_bytes,
            large
                .message_json_bytes
                .saturating_add(large.tool_schema_json_bytes)
                .saturating_add(REQUEST_FIXED_OVERHEAD_BYTES)
        );
    }

    #[test]
    fn test_tool_growth_increases_schema_bytes() {
        let g = governor();
        let messages = vec![msg("user", "hi")];
        let one = g
            .measure(&messages, &[tool_def("a", "desc", serde_json::json!({}))])
            .expect("one tool");
        let two = g
            .measure(
                &messages,
                &[
                    tool_def("a", "desc", serde_json::json!({})),
                    tool_def("b", "desc", serde_json::json!({})),
                ],
            )
            .expect("two tools");
        assert!(two.tool_schema_json_bytes > one.tool_schema_json_bytes);
        assert!(two.total_json_bytes > one.total_json_bytes);
    }

    #[test]
    fn test_schema_growth_with_properties() {
        let g = governor();
        let messages = vec![msg("user", "hi")];
        let small = g
            .measure(
                &messages,
                &[tool_def(
                    "a",
                    "d",
                    serde_json::json!({"id": {"type": "string"}}),
                )],
            )
            .expect("small schema");
        let big = g
            .measure(
                &messages,
                &[tool_def(
                    "a",
                    "a much longer description with enums and strict details",
                    serde_json::json!({
                        "id": {"type": "string", "description": "resource id", "enum": ["a", "b", "c"]},
                        "path": {"type": "string", "description": "file path"},
                        "limit": {"type": "integer", "description": "page size"},
                    }),
                )],
            )
            .expect("big schema");
        assert!(big.tool_schema_json_bytes > small.tool_schema_json_bytes);
    }

    #[test]
    fn test_unicode_footprint() {
        let g = governor();
        let messages = vec![
            msg("user", "日本語テスト 🎉"),
            tool_msg("call-1", "ユーザー管理.rs 認証処理 🎉"),
            assistant_call("call-1"),
            msg("user", "/tmp/ユーザー管理/emoji-🎉.rs"),
        ];
        let fp = g.measure(&messages, &[]).expect("unicode measure");
        assert!(fp.message_json_bytes > 0);
        assert!(fp.total_json_bytes >= fp.message_json_bytes);
    }

    #[test]
    fn test_counting_writer_matches_vec_len() {
        let messages = vec![
            msg("system", "prompt"),
            assistant_call("call-1"),
            tool_msg("call-1", &"x".repeat(5000)),
            msg("user", "日本語 🎉"),
        ];
        let tools = vec![
            tool_def("a", "desc", serde_json::json!({"x": {"type": "string"}})),
            tool_def(
                "b",
                "longer desc",
                serde_json::json!({"y": {"type": "integer"}}),
            ),
        ];
        let counted_messages = serialized_size(&messages).expect("count messages");
        let actual_messages = serde_json::to_vec(&messages).expect("vec messages").len() as u64;
        assert_eq!(counted_messages, actual_messages);
        let counted_tools = serialized_size(&tools).expect("count tools");
        let actual_tools = serde_json::to_vec(&tools).expect("vec tools").len() as u64;
        assert_eq!(counted_tools, actual_tools);
        // Large fixture: 100k payload still matches without production allocation.
        let big = vec![msg("user", &"z".repeat(100_000))];
        assert_eq!(
            serialized_size(&big).expect("big"),
            serde_json::to_vec(&big).expect("big vec").len() as u64
        );
    }

    #[test]
    fn test_heuristic_formula() {
        // ceil(30_000 / 3) + 512 = 10_512.
        assert_eq!(heuristic_estimate(30_000), 10_000 + 512);
        assert_eq!(heuristic_estimate(0), 512);
        assert_eq!(heuristic_estimate(1), 1 + 512);
        assert_eq!(heuristic_estimate(4), 2 + 512);
    }

    #[test]
    fn test_calibrated_growth() {
        let g = {
            let mut gov = governor();
            gov.observe_actual(RequestFootprint::new(30_000, 0, 0), 12_000);
            gov
        };
        // Previous 30k B / 12k tokens; current 36k B (+6k growth => +2k).
        let fp = RequestFootprint::new(36_000, 0, 0);
        let est = g.estimate(fp);
        assert_eq!(est.source, TokenEstimateSource::Calibrated);
        // heuristic = ceil(36256/3)+512 = 12086+512 = 12598? total includes
        // 256 overhead, so calibrated (14_000) wins via max().
        assert_eq!(est.prompt_tokens, 14_000);
    }

    #[test]
    fn test_calibrated_shrink_is_conservative() {
        let mut gov = governor();
        gov.observe_actual(RequestFootprint::new(60_000, 0, 0), 22_000);
        let fp = RequestFootprint::new(30_000, 0, 0);
        let est = gov.estimate(fp);
        assert_eq!(est.source, TokenEstimateSource::Calibrated);
        // Shrink 30k bytes => floor(30k/6)=5k reduction => 17k calibrated.
        // Heuristic for ~30k total (~30256) is ~10598, so max is 17k.
        assert_eq!(est.prompt_tokens, 17_000);
        // Must not collapse proportionally to ~11k.
        assert!(est.prompt_tokens > 11_000);
    }

    #[test]
    fn test_final_estimate_takes_max() {
        let mut gov = governor();
        gov.observe_actual(RequestFootprint::new(10_000, 0, 0), 100);
        // Tiny previous actual but huge current payload: heuristic dominates.
        let fp = RequestFootprint::new(90_000, 0, 0);
        let est = gov.estimate(fp);
        assert_eq!(est.source, TokenEstimateSource::Calibrated);
        assert_eq!(est.prompt_tokens, heuristic_estimate(fp.total_json_bytes));
    }

    #[test]
    fn test_heuristic_only_without_previous() {
        let g = governor();
        let fp = RequestFootprint::new(30_000, 0, 0);
        let est = g.estimate(fp);
        assert_eq!(est.source, TokenEstimateSource::Heuristic);
        assert_eq!(est.prompt_tokens, heuristic_estimate(fp.total_json_bytes));
    }

    #[test]
    fn test_pressure_boundaries() {
        let g = governor();
        // cleanup = 60k, effective = 100k.
        assert_eq!(
            g.classify(
                TokenEstimate {
                    prompt_tokens: 59_999,
                    source: TokenEstimateSource::Heuristic
                },
                100_000
            ),
            BudgetPressure::Healthy
        );
        assert_eq!(
            g.classify(
                TokenEstimate {
                    prompt_tokens: 60_000,
                    source: TokenEstimateSource::Heuristic
                },
                100_000
            ),
            BudgetPressure::Cleanup
        );
        assert_eq!(
            g.classify(
                TokenEstimate {
                    prompt_tokens: 100_000,
                    source: TokenEstimateSource::Heuristic
                },
                100_000
            ),
            BudgetPressure::Cleanup
        );
        assert_eq!(
            g.classify(
                TokenEstimate {
                    prompt_tokens: 100_001,
                    source: TokenEstimateSource::Heuristic
                },
                100_000
            ),
            BudgetPressure::Compact
        );
    }

    #[test]
    fn test_cleanup_threshold_math() {
        assert_eq!(cleanup_threshold(100_000), 60_000);
        assert_eq!(cleanup_threshold(0), 0);
        // u64::MAX must not overflow (u128 intermediate).
        let huge = cleanup_threshold(u64::MAX);
        assert_eq!(huge, ((u64::MAX as u128 * 60) / 100) as u64);
    }

    #[test]
    fn test_observe_actual_records_footprint() {
        let mut gov = governor();
        assert!(gov.previous_usage().is_none());
        let fp = RequestFootprint::new(1000, 2000, 0);
        gov.observe_actual(fp, 1234);
        assert_eq!(
            gov.previous_usage(),
            Some(ObservedRequestUsage {
                footprint_bytes: fp.total_json_bytes,
                actual_prompt_tokens: 1234,
            })
        );
    }

    #[test]
    fn test_offload_gate() {
        assert!(!should_offload_for_pressure(BudgetPressure::Healthy));
        assert!(should_offload_for_pressure(BudgetPressure::Cleanup));
        assert!(should_offload_for_pressure(BudgetPressure::Compact));
    }

    #[test]
    fn test_compact_gate_requires_calibrated() {
        // Heuristic-only Compact never compacts proactively.
        assert!(!should_compact_for_pressure(
            BudgetPressure::Compact,
            TokenEstimateSource::Heuristic
        ));
        assert!(should_compact_for_pressure(
            BudgetPressure::Compact,
            TokenEstimateSource::Calibrated
        ));
        assert!(!should_compact_for_pressure(
            BudgetPressure::Cleanup,
            TokenEstimateSource::Calibrated
        ));
        assert!(!should_compact_for_pressure(
            BudgetPressure::Healthy,
            TokenEstimateSource::Calibrated
        ));
    }

    #[test]
    fn test_reactive_guard_allows_single_retry() {
        let mut guard = ReactiveRetryGuard::new();
        assert!(guard.should_attempt());
        guard.record_attempt();
        assert!(!guard.should_attempt(), "second attempt blocked");
        guard.record_success();
        assert!(guard.should_attempt(), "success resets guard");
    }

    #[test]
    fn test_overlay_increases_footprint() {
        let g = governor();
        let base = vec![msg("system", "prompt"), msg("user", "work")];
        let overlay = ChatMessage {
            provider_state: None,
            role: "system".into(),
            content: Some("<RuntimeContext>\nrecent files\n</RuntimeContext>".into()),
            tool_calls: vec![],
            tool_call_id: None,
        };
        let mut with_overlay = base.clone();
        with_overlay.insert(1, overlay);
        let without = g.measure(&base, &[]).expect("base");
        let with = g.measure(&with_overlay, &[]).expect("overlay");
        assert!(with.message_json_bytes > without.message_json_bytes);
        assert!(with.total_json_bytes > without.total_json_bytes);
    }

    #[test]
    fn test_tool_search_activation_grows_schema_footprint() {
        use crate::config::{ToolRoutingConfig, ToolRoutingMode};
        use crate::llm::tool_catalog::{ToolCatalog, ToolCatalogEntry, ToolSource};
        use crate::llm::tool_def::default_tools_def;

        // Deferred catalog: initial request carries core tools only.
        let entries: Vec<ToolCatalogEntry> = default_tools_def()
            .into_iter()
            .map(|def| {
                let text =
                    crate::llm::tool_catalog::build_searchable_text(&def, &ToolSource::Builtin);
                ToolCatalogEntry {
                    definition: def,
                    source: ToolSource::Builtin,
                    searchable_text: text,
                }
            })
            .collect();
        // Add large deferred remotes with big schemas.
        let mut with_remotes = entries;
        for i in 0..3 {
            let def = tool_def(
                &format!("mcp_srv_tool_{i}"),
                &format!("large remote helper {i} {}", "d".repeat(500)),
                serde_json::json!({
                    "id": {"type": "string", "description": "resource id with many details"},
                    "extra": {"type": "object", "description": "nested options"},
                }),
            );
            let text = crate::llm::tool_catalog::build_searchable_text(&def, &ToolSource::Builtin);
            with_remotes.push(ToolCatalogEntry {
                definition: def,
                source: ToolSource::Builtin,
                searchable_text: text,
            });
        }
        let catalog = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt")
            .block_on(async {
                let catalog = ToolCatalog::from_entries(
                    with_remotes,
                    &ToolRoutingConfig {
                        mode: ToolRoutingMode::Deferred,
                        search_result_limit: 5,
                    },
                );
                let before = catalog.active_tool_defs().await;
                catalog
                    .activate(&["mcp_srv_tool_0".to_string(), "mcp_srv_tool_1".to_string()])
                    .await;
                let after = catalog.active_tool_defs().await;
                (before, after)
            });
        let g = governor();
        let messages = vec![msg("user", "do work")];
        let before_fp = g.measure(&messages, &catalog.0).expect("before");
        let after_fp = g.measure(&messages, &catalog.1).expect("after");
        assert!(
            after_fp.tool_schema_json_bytes > before_fp.tool_schema_json_bytes,
            "newly activated MCP schemas must grow the measured footprint"
        );
        assert!(after_fp.total_json_bytes > before_fp.total_json_bytes);
    }

    #[test]
    fn native_compaction_resets_calibration_and_recalibrates() {
        let mut gov = governor();
        // Pre-compaction calibration exists.
        gov.observe_actual(RequestFootprint::new(100_000, 0, 0), 50_000);
        assert!(gov.previous_usage().is_some());
        // A native-compacted response must not become a sample.
        gov.reset_after_native_compaction();
        assert!(gov.previous_usage().is_none());
        // The next normal response re-establishes calibration from the
        // smaller canonical context.
        let fp = RequestFootprint::new(10_000, 0, 0);
        let expected_bytes = fp.total_json_bytes;
        gov.observe_actual(fp, 5_000);
        let usage = gov.previous_usage().expect("recalibrated");
        assert_eq!(usage.footprint_bytes, expected_bytes);
        assert_eq!(usage.actual_prompt_tokens, 5_000);
    }

    #[test]
    fn subscription_projection_includes_context_management_envelope() {
        let g = governor();
        let messages = vec![msg("user", "hi")];
        let without = g
            .measure_subscription("a", "m", &messages, &[], 0, None)
            .expect("without");
        let with = g
            .measure_subscription("a", "m", &messages, &[], 0, Some(102_400))
            .expect("with");
        // Envelope is tiny but present so the projection does not drift.
        assert!(with.total_json_bytes >= without.total_json_bytes);
    }
}

#[cfg(test)]
mod append_only_tests {
    use super::*;
    use crate::llm::types::{ChatMessage, ToolDef, ToolFunctionDef};

    fn tool_def(name: &str) -> ToolDef {
        ToolDef {
            kind: "function".into(),
            function: ToolFunctionDef {
                name: name.into(),
                description: format!("{name} helper"),
                parameters: serde_json::json!({"type":"object"}),
                strict: None,
            },
        }
    }

    fn user_msg() -> ChatMessage {
        ChatMessage {
            provider_state: None,
            role: "user".into(),
            content: Some("hi".into()),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    #[test]
    fn test_responses_schema_bytes_stable_input_grows() {
        let gov = ContextBudgetGovernor::new(crate::config::ContextBudgetConfig::default());
        let base = vec![tool_def("fs_read"), tool_def("tool_search")];
        let active = vec![
            tool_def("edit"),
            tool_def("fs_read"),
            tool_def("tool_search"),
        ];
        // Pre-activation history: no markers.
        let before_msgs = vec![user_msg()];
        // Post-activation history: tool_search call + result + marker.
        // Build marker via trusted active set (single edit).
        let search_call = ChatMessage {
            provider_state: None,
            role: "assistant".into(),
            content: None,
            tool_calls: vec![crate::llm::types::ToolCall {
                id: Some("c1".into()),
                r#type: "function".into(),
                function: crate::llm::types::ToolCallFunction {
                    name: "tool_search".into(),
                    arguments: "{}".into(),
                },
            }],
            tool_call_id: None,
        };
        let search_result = ChatMessage {
            provider_state: None,
            role: "tool".into(),
            content: Some("{}".into()),
            tool_calls: vec![],
            tool_call_id: Some("c1".into()),
        };
        let marker = ChatMessage {
            provider_state: Some(
                crate::features::openai_subscription::ProviderState::activation(
                    "a".into(),
                    "m".into(),
                    vec!["edit".into()],
                ),
            ),
            role: "developer".into(),
            content: None,
            tool_calls: vec![],
            tool_call_id: None,
        };
        let after_msgs = vec![user_msg(), search_call, search_result, marker];
        let before = gov
            .measure_subscription_with_activation("a", "m", &before_msgs, &base, &base, 0, None)
            .expect("before");
        let after = gov
            .measure_subscription_with_activation("a", "m", &after_msgs, &base, &active, 0, None)
            .expect("after");
        assert_eq!(
            before.tool_schema_json_bytes, after.tool_schema_json_bytes,
            "stable base top-level must not grow"
        );
        assert!(
            after.message_json_bytes > before.message_json_bytes,
            "additional suffix must be measured in input bytes"
        );
    }
}
