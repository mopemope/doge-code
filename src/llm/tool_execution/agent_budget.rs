//! Run-wide resource budget tracker for the main agent loop.
//!
//! Main semantics: iterations, main-dispatch tool calls, monotonic elapsed
//! time, and cumulative model tokens. Budget exhaustion is partial
//! completion, never cancellation.

use crate::config::AgentBudgetConfig;
use crate::llm::usage_ledger::UsageLedger;
use tokio::time::Instant;

/// Stable stop reason. Serialized values are part of the CLI/telemetry
/// contract and must not be renamed casually.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStopReason {
    IterationBudget,
    ToolCallBudget,
    TokenBudget,
    ElapsedBudget,
}

impl AgentStopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IterationBudget => "iteration_budget",
            Self::ToolCallBudget => "tool_call_budget",
            Self::TokenBudget => "token_budget",
            Self::ElapsedBudget => "elapsed_budget",
        }
    }
}

/// Run outcome. Errors and cancellations are not represented here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunStatus {
    Completed,
    Partial,
}

/// Content-free budget telemetry. `charged_tokens` drives policy;
/// `provider_reported_tokens` / `estimated_tokens` explain the charge.
/// The provider [`UsageLedger`] itself never receives estimates.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentBudgetUsage {
    pub iterations: usize,
    pub tool_calls: usize,
    pub charged_tokens: u64,
    pub elapsed_ms: u64,
    pub provider_reported_tokens: u64,
    pub estimated_tokens: u64,
    pub request_attempts: u64,
    pub usage_records: u64,
    pub finalization_attempted: bool,
    pub finalization_succeeded: bool,
}

/// Successful run result, including partial completions.
#[derive(Debug, Clone)]
pub struct AgentRunResult {
    pub messages: Vec<crate::llm::types::ChatMessage>,
    pub final_message: crate::llm::types::ChoiceMessage,
    pub status: AgentRunStatus,
    pub stop_reason: Option<AgentStopReason>,
    pub budget: AgentBudgetUsage,
}

pub struct AgentBudgetTracker {
    limits: AgentBudgetConfig,
    started_at: Instant,
    usage_before_run: UsageLedger,
    iterations: usize,
    tool_calls: usize,
    charged_tokens: u64,
    provider_reported_tokens: u64,
    estimated_tokens: u64,
    request_attempts: u64,
    usage_records: u64,
    pub finalization_attempted: bool,
    pub finalization_succeeded: bool,
}

impl AgentBudgetTracker {
    pub fn new(limits: AgentBudgetConfig, usage_before_run: UsageLedger) -> Self {
        Self {
            limits,
            started_at: Instant::now(),
            usage_before_run,
            iterations: 0,
            tool_calls: 0,
            charged_tokens: 0,
            provider_reported_tokens: 0,
            estimated_tokens: 0,
            request_attempts: 0,
            usage_records: 0,
            finalization_attempted: false,
            finalization_succeeded: false,
        }
    }

    #[cfg(test)]
    pub fn with_started_at(mut self, started_at: Instant) -> Self {
        self.started_at = started_at;
        self
    }

    pub fn limits(&self) -> &AgentBudgetConfig {
        &self.limits
    }

    pub fn iterations(&self) -> usize {
        self.iterations
    }

    pub fn tool_calls(&self) -> usize {
        self.tool_calls
    }

    pub fn charged_tokens(&self) -> u64 {
        self.charged_tokens
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis().min(u64::MAX as u128) as u64
    }

    /// Safe boundary only: never drop an in-flight tool on expiry.
    pub fn elapsed_stop(&self) -> Option<AgentStopReason> {
        match self.limits.max_elapsed_ms {
            Some(limit) => (self.started_at.elapsed().as_millis() >= u128::from(limit))
                .then_some(AgentStopReason::ElapsedBudget),
            None => None,
        }
    }

    fn token_stop_for_estimate(&self, estimate: u64) -> Option<AgentStopReason> {
        match self.limits.max_total_tokens {
            Some(limit) => {
                if self.charged_tokens >= limit
                    || estimate > limit.saturating_sub(self.charged_tokens)
                {
                    Some(AgentStopReason::TokenBudget)
                } else {
                    None
                }
            }
            None => None,
        }
    }

    /// Preflight before starting a normal agent request.
    pub fn request_stop(&self, estimate: u64) -> Option<AgentStopReason> {
        self.elapsed_stop().or_else(|| {
            if self.iterations >= self.limits.max_iterations {
                Some(AgentStopReason::IterationBudget)
            } else {
                self.token_stop_for_estimate(estimate)
            }
        })
    }

    /// Preflight before dispatching a tool batch. All-or-none: an oversized
    /// batch is rejected before any prefix executes.
    pub fn batch_stop(&self, size: usize) -> Option<AgentStopReason> {
        self.elapsed_stop().or_else(|| {
            if let Some(limit) = self.limits.max_total_tokens
                && self.charged_tokens >= limit
            {
                return Some(AgentStopReason::TokenBudget);
            }
            match self.limits.max_tool_calls {
                Some(max) => (size > max.saturating_sub(self.tool_calls))
                    .then_some(AgentStopReason::ToolCallBudget),
                None => None,
            }
        })
    }

    /// Whether a tools-free finalization request may start. Iteration and
    /// tool-call budgets never block finalization; token and elapsed do.
    pub fn finalization_stop(&self, estimate: u64) -> Option<AgentStopReason> {
        self.elapsed_stop()
            .or_else(|| self.token_stop_for_estimate(estimate))
    }

    pub fn record_iteration(&mut self) {
        self.iterations = self.iterations.saturating_add(1);
    }

    pub fn record_tool_call(&mut self) {
        self.tool_calls = self.tool_calls.saturating_add(1);
    }

    fn observe_ledger_delta(&mut self, before: &UsageLedger, after: &UsageLedger) {
        let attempts = after.attempts.saturating_sub(before.attempts);
        let records = after.usage_records.saturating_sub(before.usage_records);
        self.request_attempts = self.request_attempts.saturating_add(attempts);
        self.usage_records = self.usage_records.saturating_add(records);
    }

    fn add_charge(&mut self, provider_part: u64, estimated_part: u64) {
        self.provider_reported_tokens = self.provider_reported_tokens.saturating_add(provider_part);
        self.estimated_tokens = self.estimated_tokens.saturating_add(estimated_part);
        self.charged_tokens = self
            .charged_tokens
            .saturating_add(provider_part.saturating_add(estimated_part));
    }

    /// Reconcile one main LLM request. Exactly one usage record is
    /// attributable under the serial dispatch contract; ambiguous or missing
    /// usage falls back to the local estimate without touching provider
    /// telemetry. Extra attempts without records each cost one estimate.
    pub fn charge_request(&mut self, estimate: u64, before: &UsageLedger, after: &UsageLedger) {
        self.observe_ledger_delta(before, after);
        let attempts = after.attempts.saturating_sub(before.attempts);
        let records = after.usage_records.saturating_sub(before.usage_records);
        let reported = after.total_tokens.saturating_sub(before.total_tokens);
        if records == 1 {
            let base = estimate.max(reported);
            let extra = attempts.saturating_sub(records);
            let extra_charge = estimate.saturating_mul(extra);
            let charge = base.saturating_add(extra_charge);
            let estimated_part = charge.saturating_sub(reported);
            self.add_charge(reported, estimated_part);
        } else if records == 0 {
            let n = attempts.max(1);
            let charge = estimate.saturating_mul(n);
            self.add_charge(0, charge);
        } else {
            tracing::debug!(
                usage_records = records,
                "ambiguous main usage attribution; using request estimate"
            );
            self.add_charge(0, estimate);
        }
    }

    /// Reconcile internal/nested model work observed on the shared client.
    /// Multiple records are expected here, so the full reported delta is
    /// charged. Unknown attempts each cost one bounded estimate; a missing
    /// record with no ledger movement still costs the estimate so internal
    /// work is never free. Unlike `charge_request`, no `max(reported,
    /// estimate)` premium is applied: the caller passes a conservative
    /// bounded estimate (e.g. the effective compaction limit) that must not
    /// dominate a small reported total.
    pub fn charge_internal(&mut self, estimate: u64, before: &UsageLedger, after: &UsageLedger) {
        self.observe_ledger_delta(before, after);
        let attempts = after.attempts.saturating_sub(before.attempts);
        let records = after.usage_records.saturating_sub(before.usage_records);
        let reported = after.total_tokens.saturating_sub(before.total_tokens);
        if records >= 1 {
            let unknown = attempts.saturating_sub(records);
            let extra = estimate.saturating_mul(unknown);
            let charge = reported.saturating_add(extra);
            let estimated_part = charge.saturating_sub(reported);
            self.add_charge(reported, estimated_part);
        } else {
            let n = attempts.max(1);
            let charge = estimate.saturating_mul(n);
            self.add_charge(0, charge);
        }
    }

    /// Run-local provider delta, excluding estimates. Used for telemetry and
    /// for deriving bounded fallbacks, never as the policy counter directly.
    #[allow(dead_code)]
    pub fn run_reported_delta(&self, now: &UsageLedger) -> UsageLedger {
        now.difference(&self.usage_before_run)
    }

    pub fn usage(&self) -> AgentBudgetUsage {
        AgentBudgetUsage {
            iterations: self.iterations,
            tool_calls: self.tool_calls,
            charged_tokens: self.charged_tokens,
            elapsed_ms: self.elapsed_ms(),
            provider_reported_tokens: self.provider_reported_tokens,
            estimated_tokens: self.estimated_tokens,
            request_attempts: self.request_attempts,
            usage_records: self.usage_records,
            finalization_attempted: self.finalization_attempted,
            finalization_succeeded: self.finalization_succeeded,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentBudgetConfig;

    fn ledger(attempts: u64, records: u64, total: u64) -> UsageLedger {
        UsageLedger {
            attempts,
            usage_records: records,
            total_tokens: total,
            ..Default::default()
        }
    }

    fn cfg() -> AgentBudgetConfig {
        AgentBudgetConfig {
            max_iterations: 2,
            max_tool_calls: Some(3),
            max_elapsed_ms: Some(10_000),
            max_total_tokens: Some(50_000),
        }
    }

    #[test]
    fn exact_iteration_boundary() {
        let mut t = AgentBudgetTracker::new(
            AgentBudgetConfig {
                max_iterations: 2,
                ..Default::default()
            },
            UsageLedger::default(),
        );
        for _ in 0..2 {
            assert_eq!(t.request_stop(1), None);
            t.record_iteration();
        }
        assert_eq!(t.request_stop(1), Some(AgentStopReason::IterationBudget));
        assert_eq!(t.iterations, 2);
    }

    #[test]
    fn tool_batch_is_all_or_none() {
        let mut t = AgentBudgetTracker::new(cfg(), UsageLedger::default());
        t.tool_calls = 2;
        assert_eq!(t.batch_stop(2), Some(AgentStopReason::ToolCallBudget));
        assert_eq!(t.tool_calls, 2);
        assert_eq!(t.batch_stop(1), None);
        t.record_tool_call();
        assert_eq!(t.tool_calls, 3);
        assert_eq!(t.batch_stop(1), Some(AgentStopReason::ToolCallBudget));
    }

    #[test]
    fn elapsed_budget_checked_at_safe_boundary() {
        let t = AgentBudgetTracker::new(
            AgentBudgetConfig {
                max_elapsed_ms: Some(10),
                ..Default::default()
            },
            UsageLedger::default(),
        );
        assert_eq!(t.elapsed_stop(), None);
        let past = Instant::now() - std::time::Duration::from_millis(11);
        let t2 = AgentBudgetTracker::new(
            AgentBudgetConfig {
                max_elapsed_ms: Some(10),
                ..Default::default()
            },
            UsageLedger::default(),
        )
        .with_started_at(past);
        assert_eq!(t2.elapsed_stop(), Some(AgentStopReason::ElapsedBudget));
        assert_eq!(t2.batch_stop(1), Some(AgentStopReason::ElapsedBudget));
        assert_eq!(t2.request_stop(1), Some(AgentStopReason::ElapsedBudget));
        assert_eq!(
            t2.finalization_stop(1),
            Some(AgentStopReason::ElapsedBudget)
        );
    }

    #[test]
    fn token_preflight_exact_boundary() {
        let mut t = AgentBudgetTracker::new(
            AgentBudgetConfig {
                max_total_tokens: Some(50_000),
                ..Default::default()
            },
            UsageLedger::default(),
        );
        t.charge_request(20_000, &ledger(0, 0, 0), &ledger(0, 0, 0));
        assert_eq!(t.request_stop(30_000), None);
        assert_eq!(t.finalization_stop(30_000), None);
        t.charge_request(30_000, &ledger(0, 0, 0), &ledger(0, 0, 0));
        assert_eq!(t.charged_tokens, 50_000);
        assert_eq!(t.request_stop(1), Some(AgentStopReason::TokenBudget));
        assert_eq!(t.batch_stop(1), Some(AgentStopReason::TokenBudget));
        assert_eq!(t.finalization_stop(1), Some(AgentStopReason::TokenBudget));
    }

    #[test]
    fn reported_usage_beats_lower_estimate() {
        let mut t = AgentBudgetTracker::new(AgentBudgetConfig::default(), UsageLedger::default());
        t.charge_request(10_000, &ledger(0, 0, 0), &ledger(1, 1, 15_000));
        assert_eq!(t.charged_tokens, 15_000);
        assert_eq!(t.provider_reported_tokens, 15_000);
        assert_eq!(t.estimated_tokens, 0);
        assert_eq!(t.request_attempts, 1);
        assert_eq!(t.usage_records, 1);
    }

    #[test]
    fn missing_usage_uses_estimate() {
        let mut t = AgentBudgetTracker::new(AgentBudgetConfig::default(), UsageLedger::default());
        t.charge_request(7_500, &ledger(0, 0, 0), &ledger(1, 0, 0));
        assert_eq!(t.charged_tokens, 7_500);
        assert_eq!(t.provider_reported_tokens, 0);
        assert_eq!(t.estimated_tokens, 7_500);
    }

    #[test]
    fn retry_without_usage_is_not_free() {
        let mut t = AgentBudgetTracker::new(AgentBudgetConfig::default(), UsageLedger::default());
        // Two attempts, one reported record: failed retry costs one estimate.
        t.charge_request(5_000, &ledger(0, 0, 0), &ledger(2, 1, 4_000));
        assert_eq!(t.charged_tokens, 10_000);
        assert_eq!(t.provider_reported_tokens, 4_000);
        assert_eq!(t.estimated_tokens, 6_000);
        // Two attempts, no records: two estimates.
        let mut t2 = AgentBudgetTracker::new(AgentBudgetConfig::default(), UsageLedger::default());
        t2.charge_request(5_000, &ledger(0, 0, 0), &ledger(2, 0, 0));
        assert_eq!(t2.charged_tokens, 10_000);
    }

    #[test]
    fn saturating_counters_do_not_overflow() {
        let mut t = AgentBudgetTracker::new(
            AgentBudgetConfig {
                max_total_tokens: Some(u64::MAX),
                ..Default::default()
            },
            UsageLedger::default(),
        );
        t.charge_request(u64::MAX, &ledger(0, 0, 0), &ledger(0, 0, 0));
        t.charge_request(1, &ledger(0, 0, 0), &ledger(0, 0, 0));
        assert_eq!(t.charged_tokens, u64::MAX);
        t.iterations = usize::MAX;
        t.record_iteration();
        assert_eq!(t.iterations, usize::MAX);
        t.tool_calls = usize::MAX;
        t.record_tool_call();
        assert_eq!(t.tool_calls, usize::MAX);
    }

    #[test]
    fn disabled_optional_limits_do_not_stop() {
        let mut t = AgentBudgetTracker::new(
            AgentBudgetConfig {
                max_iterations: usize::MAX,
                max_tool_calls: None,
                max_elapsed_ms: None,
                max_total_tokens: None,
            },
            UsageLedger::default(),
        );
        t.charge_request(u64::MAX / 2, &ledger(0, 0, 0), &ledger(0, 0, 0));
        assert_eq!(t.request_stop(u64::MAX), None);
        assert_eq!(t.batch_stop(usize::MAX), None);
        assert_eq!(t.elapsed_stop(), None);
        assert_eq!(t.finalization_stop(u64::MAX), None);
    }

    #[test]
    fn internal_multi_record_charges_reported_sum() {
        let mut t = AgentBudgetTracker::new(AgentBudgetConfig::default(), UsageLedger::default());
        // Subagent with two reported requests.
        t.charge_internal(1_000, &ledger(0, 0, 0), &ledger(2, 2, 20_000));
        assert_eq!(t.charged_tokens, 20_000);
        assert_eq!(t.provider_reported_tokens, 20_000);
    }
}
