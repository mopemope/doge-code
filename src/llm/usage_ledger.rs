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

    pub fn report(&self) -> serde_json::Value {
        let mut value = serde_json::json!(self);
        value["unknown_usage_attempts"] = self.unknown_usage_attempts().into();
        value["all_tracked_attempts_reported"] =
            (self.unknown_usage_attempts() == 0 && !self.historical_usage_unknown).into();
        value["scope"] = "agent turns and internal summaries/subagents; manual jobs and independent doc_generate clients excluded".into();
        value
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
}
