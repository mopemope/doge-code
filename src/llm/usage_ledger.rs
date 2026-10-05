//! Provider-reported subtotals, never local estimates or a billing guarantee.
use crate::llm::types::Usage;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageLedger {
    pub attempts: u64,
    pub usage_records: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub reasoning_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_usage_records: u64,
    pub cached_usage_records: u64,
    pub cache_write_usage_records: u64,
    pub historical_usage_unknown: bool,
}

fn add_optional(total: &mut Option<u64>, value: Option<u64>) {
    if let Some(value) = value {
        *total = Some(total.unwrap_or(0).saturating_add(value));
    }
}

impl UsageLedger {
    pub fn record(&mut self, usage: &Usage) {
        self.usage_records = self.usage_records.saturating_add(1);
        self.prompt_tokens = self
            .prompt_tokens
            .saturating_add(u64::from(usage.prompt_tokens));
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(u64::from(usage.completion_tokens));
        self.total_tokens = self
            .total_tokens
            .saturating_add(u64::from(usage.total_tokens));
        let reasoning = usage
            .completion_tokens_details
            .as_ref()
            .and_then(|d| d.reasoning_tokens);
        let cached = usage
            .prompt_tokens_details
            .as_ref()
            .and_then(|d| d.cached_tokens);
        let written = usage
            .prompt_tokens_details
            .as_ref()
            .and_then(|d| d.cache_write_tokens);
        add_optional(&mut self.reasoning_tokens, reasoning.map(u64::from));
        add_optional(&mut self.cached_tokens, cached.map(u64::from));
        add_optional(&mut self.cache_write_tokens, written.map(u64::from));
        self.reasoning_usage_records = self
            .reasoning_usage_records
            .saturating_add(u64::from(reasoning.is_some()));
        self.cached_usage_records = self
            .cached_usage_records
            .saturating_add(u64::from(cached.is_some()));
        self.cache_write_usage_records = self
            .cache_write_usage_records
            .saturating_add(u64::from(written.is_some()));
    }

    /// Both snapshots belong to the same monotonic shared client.
    pub fn difference(&self, before: &Self) -> Self {
        let reasoning_records = self
            .reasoning_usage_records
            .saturating_sub(before.reasoning_usage_records);
        let cached_records = self
            .cached_usage_records
            .saturating_sub(before.cached_usage_records);
        let written_records = self
            .cache_write_usage_records
            .saturating_sub(before.cache_write_usage_records);
        let metric = |now: Option<u64>, old: Option<u64>, records: u64| {
            (records > 0).then(|| now.unwrap_or(0).saturating_sub(old.unwrap_or(0)))
        };
        Self {
            attempts: self.attempts.saturating_sub(before.attempts),
            usage_records: self.usage_records.saturating_sub(before.usage_records),
            prompt_tokens: self.prompt_tokens.saturating_sub(before.prompt_tokens),
            completion_tokens: self
                .completion_tokens
                .saturating_sub(before.completion_tokens),
            total_tokens: self.total_tokens.saturating_sub(before.total_tokens),
            reasoning_tokens: metric(
                self.reasoning_tokens,
                before.reasoning_tokens,
                reasoning_records,
            ),
            cached_tokens: metric(self.cached_tokens, before.cached_tokens, cached_records),
            cache_write_tokens: metric(
                self.cache_write_tokens,
                before.cache_write_tokens,
                written_records,
            ),
            reasoning_usage_records: reasoning_records,
            cached_usage_records: cached_records,
            cache_write_usage_records: written_records,
            historical_usage_unknown: false,
        }
    }

    pub fn add(&mut self, delta: &Self) {
        self.attempts = self.attempts.saturating_add(delta.attempts);
        self.usage_records = self.usage_records.saturating_add(delta.usage_records);
        self.prompt_tokens = self.prompt_tokens.saturating_add(delta.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(delta.completion_tokens);
        self.total_tokens = self.total_tokens.saturating_add(delta.total_tokens);
        add_optional(&mut self.reasoning_tokens, delta.reasoning_tokens);
        add_optional(&mut self.cached_tokens, delta.cached_tokens);
        add_optional(&mut self.cache_write_tokens, delta.cache_write_tokens);
        self.reasoning_usage_records = self
            .reasoning_usage_records
            .saturating_add(delta.reasoning_usage_records);
        self.cached_usage_records = self
            .cached_usage_records
            .saturating_add(delta.cached_usage_records);
        self.cache_write_usage_records = self
            .cache_write_usage_records
            .saturating_add(delta.cache_write_usage_records);
        self.historical_usage_unknown |= delta.historical_usage_unknown;
    }

    pub fn unknown_usage_attempts(&self) -> u64 {
        self.attempts.saturating_sub(self.usage_records)
    }

    /// True when the delta carries any provider-observed activity.
    ///
    /// An attempt without a usage record still counts: the provider was
    /// contacted but no usage body was observed. Never treat that as zero
    /// tokens; it is unknown usage.
    pub fn has_activity(&self) -> bool {
        self.attempts != 0
            || self.usage_records != 0
            || self.prompt_tokens != 0
            || self.completion_tokens != 0
            || self.total_tokens != 0
            || self.reasoning_usage_records != 0
            || self.cached_usage_records != 0
            || self.cache_write_usage_records != 0
    }

    /// True when every tracked attempt has a provider usage report and no
    /// legacy historical gap exists. Single source of truth for both
    /// [`Self::report`] and UI surfaces; do not reimplement the predicate
    /// elsewhere.
    pub fn all_tracked_attempts_reported(&self) -> bool {
        self.unknown_usage_attempts() == 0 && !self.historical_usage_unknown
    }

    pub fn report(&self) -> serde_json::Value {
        self.report_with_scope(UsageReportScope::AgentRun)
    }

    pub fn report_with_scope(&self, scope: UsageReportScope) -> serde_json::Value {
        let mut value = serde_json::json!(self);
        value["unknown_usage_attempts"] = self.unknown_usage_attempts().into();
        value["all_tracked_attempts_reported"] = self.all_tracked_attempts_reported().into();
        value["scope"] = scope.description().into();
        value
    }
}

/// Typed usage-report scope. Provider usage and local budget estimates stay
/// separate in both scopes; cached tokens remain part of input totals and
/// reasoning tokens remain part of output totals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageReportScope {
    /// Current agent run: main requests, retries, automatic compaction, task
    /// subagents and nested shared-client model work.
    AgentRun,
    /// Durable persisted session usage across runs.
    PersistedSession,
}

impl UsageReportScope {
    pub fn description(&self) -> &'static str {
        match self {
            Self::AgentRun => {
                "current agent run including main requests, retries, automatic compaction, task subagents and nested shared-client model work; excluding manual foreground jobs outside this run, external MCP/provider usage not observed by this client and usage lost before the local client records it"
            }
            Self::PersistedSession => {
                "persisted session usage including agent turns, retries, automatic summaries/compactions, task subagents, nested doc_generate requests, manual local /compact requests and /edit-symbol and /fix LLM requests; excluding external MCP services' internal model usage, provider usage never returned/observed before hard process termination, external/manual model calls and independent programs outside dgc"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_cache_is_reported_and_missing_is_unknown() {
        let usage: Usage = serde_json::from_value(serde_json::json!({"prompt_tokens":100,"completion_tokens":50,"total_tokens":150,"prompt_tokens_details":{"cached_tokens":0}})).expect("usage");
        let mut ledger = UsageLedger {
            attempts: 2,
            ..Default::default()
        };
        ledger.record(&usage);
        assert_eq!(ledger.cached_tokens, Some(0));
        assert_eq!(ledger.reasoning_tokens, None);
        assert_eq!(ledger.unknown_usage_attempts(), 1);
        assert_eq!(ledger.report()["all_tracked_attempts_reported"], false);
        let before = ledger;
        ledger.attempts += 1;
        let delta = ledger.difference(&before);
        assert_eq!(delta.cached_tokens, None);
        assert_eq!(delta.attempts, 1);
        assert_eq!(delta.total_tokens, 0);
    }

    #[test]
    fn empty_delta_has_no_activity() {
        assert!(!UsageLedger::default().has_activity());
    }

    #[test]
    fn attempt_only_delta_has_activity() {
        let delta = UsageLedger {
            attempts: 1,
            ..Default::default()
        };
        assert!(delta.has_activity());
        assert_eq!(delta.unknown_usage_attempts(), 1);
    }

    #[test]
    fn usage_record_delta_has_activity() {
        let usage: Usage = serde_json::from_value(
            serde_json::json!({"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}),
        )
        .expect("usage");
        let mut ledger = UsageLedger {
            attempts: 1,
            ..Default::default()
        };
        ledger.record(&usage);
        let before = UsageLedger::default();
        let delta = ledger.difference(&before);
        assert!(delta.has_activity());
        assert_eq!(delta.total_tokens, 120);
    }

    #[test]
    fn completeness_rejects_unknown_attempt() {
        let ledger = UsageLedger {
            attempts: 3,
            usage_records: 2,
            ..Default::default()
        };
        assert!(!ledger.all_tracked_attempts_reported());
        assert_eq!(ledger.unknown_usage_attempts(), 1);
        assert_eq!(ledger.report()["all_tracked_attempts_reported"], false);
    }

    #[test]
    fn completeness_rejects_historical_unknown() {
        let ledger = UsageLedger {
            historical_usage_unknown: true,
            ..Default::default()
        };
        assert!(!ledger.all_tracked_attempts_reported());
        let complete = UsageLedger::default();
        assert!(complete.all_tracked_attempts_reported());
    }

    #[test]
    fn agent_run_scope_mentions_nested_shared_client_work() {
        let scope = UsageReportScope::AgentRun.description();
        assert!(scope.contains("nested shared-client model work"));
        let report = UsageLedger::default().report();
        assert_eq!(
            report["scope"],
            serde_json::Value::String(scope.to_string())
        );
    }

    #[test]
    fn exec_json_usage_shape_has_required_keys() {
        let mut ledger = UsageLedger {
            attempts: 2,
            ..Default::default()
        };
        let usage: Usage = serde_json::from_value(
            serde_json::json!({"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}),
        )
        .expect("usage");
        ledger.record(&usage);
        let report = ledger.report();
        for key in [
            "attempts",
            "usage_records",
            "prompt_tokens",
            "completion_tokens",
            "total_tokens",
            "unknown_usage_attempts",
            "all_tracked_attempts_reported",
        ] {
            assert!(report.get(key).is_some(), "missing key {key}: {report}");
        }
        assert_eq!(report["attempts"], 2);
        assert_eq!(report["usage_records"], 1);
        assert_eq!(report["unknown_usage_attempts"], 1);
    }
}
