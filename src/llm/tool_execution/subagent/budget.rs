use crate::config::SubagentConfig;
use crate::llm::client_core::UsageTotalsSnapshot;
use tokio::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStopReason {
    IterationBudget,
    ToolCallBudget,
    TokenBudget,
    ElapsedBudget,
    ContextBudget,
    ProviderContextExceeded,
}

impl SubagentStopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IterationBudget => "iteration_budget",
            Self::ToolCallBudget => "tool_call_budget",
            Self::TokenBudget => "token_budget",
            Self::ElapsedBudget => "elapsed_budget",
            Self::ContextBudget => "context_budget",
            Self::ProviderContextExceeded => "provider_context_exceeded",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentRunStatus {
    Completed,
    Partial,
}

pub(super) struct SubagentBudgetTracker {
    limits: SubagentConfig,
    pub context_limit: u64,
    pub total_limit: u64,
    pub started_at: Instant,
    pub research_iterations: usize,
    pub executed_tool_calls: usize,
    pub charged_tokens: u64,
    pub reported_usage_requests: usize,
    pub estimated_usage_requests: usize,
    pub finalization_attempted: bool,
    pub finalization_succeeded: bool,
}

impl SubagentBudgetTracker {
    pub fn new(limits: SubagentConfig, context_limit: u64) -> Self {
        Self {
            total_limit: limits.max_total_tokens.unwrap_or(context_limit),
            context_limit,
            limits,
            started_at: Instant::now(),
            research_iterations: 0,
            executed_tool_calls: 0,
            charged_tokens: 0,
            reported_usage_requests: 0,
            estimated_usage_requests: 0,
            finalization_attempted: false,
            finalization_succeeded: false,
        }
    }

    /// Safe boundaries only: never drop an in-flight tool on elapsed expiry.
    pub fn elapsed_stop(&self) -> Option<SubagentStopReason> {
        (self.started_at.elapsed().as_millis() >= u128::from(self.limits.max_elapsed_ms))
            .then_some(SubagentStopReason::ElapsedBudget)
    }

    pub fn request_stop(&self, estimate: u64, research: bool) -> Option<SubagentStopReason> {
        self.elapsed_stop().or_else(|| {
            if research && self.research_iterations >= self.limits.max_iterations {
                Some(SubagentStopReason::IterationBudget)
            } else if estimate > self.context_limit {
                Some(SubagentStopReason::ContextBudget)
            } else if self.charged_tokens >= self.total_limit
                || estimate > self.total_limit.saturating_sub(self.charged_tokens)
            {
                Some(SubagentStopReason::TokenBudget)
            } else {
                None
            }
        })
    }

    pub fn batch_stop(&self, size: usize) -> Option<SubagentStopReason> {
        self.elapsed_stop().or_else(|| {
            if self.charged_tokens >= self.total_limit {
                return Some(SubagentStopReason::TokenBudget);
            }
            (size
                > self
                    .limits
                    .max_tool_calls
                    .saturating_sub(self.executed_tool_calls))
            .then_some(SubagentStopReason::ToolCallBudget)
        })
    }

    pub fn record_research_request(&mut self) {
        self.research_iterations = self.research_iterations.saturating_add(1);
    }

    pub fn record_tool_call(&mut self) {
        self.executed_tool_calls = self.executed_tool_calls.saturating_add(1);
    }

    /// Estimates only affect worker policy; they never enter provider telemetry.
    /// Exactly one usage record is attributable under the serial dispatch contract.
    pub fn charge(
        &mut self,
        estimate: u64,
        before: UsageTotalsSnapshot,
        after: UsageTotalsSnapshot,
    ) -> Option<u32> {
        let records = after.record_count.saturating_sub(before.record_count);
        let reported = records == 1 && after.record_count > before.record_count;
        let charge = if reported {
            self.reported_usage_requests = self.reported_usage_requests.saturating_add(1);
            estimate.max(after.total_tokens.saturating_sub(before.total_tokens))
        } else {
            if records > 1 {
                tracing::debug!(
                    usage_records = records,
                    "ambiguous subagent usage attribution; using request estimate"
                );
            }
            self.estimated_usage_requests = self.estimated_usage_requests.saturating_add(1);
            estimate
        };
        self.charged_tokens = self.charged_tokens.saturating_add(charge);
        reported
            .then(|| u32::try_from(after.prompt_tokens.saturating_sub(before.prompt_tokens)).ok())
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(total: u64, prompt: u64, count: u64) -> UsageTotalsSnapshot {
        UsageTotalsSnapshot {
            total_tokens: total,
            prompt_tokens: prompt,
            record_count: count,
        }
    }
    #[test]
    fn test_exact_iteration_limit() {
        let mut tracker = SubagentBudgetTracker::new(
            SubagentConfig {
                max_iterations: 2,
                ..Default::default()
            },
            100,
        );
        for _ in 0..2 {
            assert_eq!(tracker.request_stop(1, true), None);
            tracker.record_research_request();
        }
        assert_eq!(
            tracker.request_stop(1, true),
            Some(SubagentStopReason::IterationBudget)
        );
        assert_eq!(tracker.research_iterations, 2);
        assert_eq!(tracker.request_stop(1, false), None);
    }
    #[test]
    fn test_batch_is_all_or_none() {
        let mut tracker = SubagentBudgetTracker::new(
            SubagentConfig {
                max_tool_calls: 3,
                ..Default::default()
            },
            100,
        );
        tracker.executed_tool_calls = 2;
        assert_eq!(
            tracker.batch_stop(2),
            Some(SubagentStopReason::ToolCallBudget)
        );
        assert_eq!(tracker.executed_tool_calls, 2);
        assert_eq!(tracker.batch_stop(1), None);
    }
    #[test]
    fn test_estimate_fallback_and_exact_token_boundary() {
        let mut tracker = SubagentBudgetTracker::new(SubagentConfig::default(), 50_000);
        tracker.charge(20_000, snapshot(0, 0, 0), snapshot(0, 0, 0));
        assert_eq!(tracker.request_stop(30_000, true), None);
        tracker.charge(30_000, snapshot(0, 0, 0), snapshot(0, 0, 0));
        assert_eq!(tracker.charged_tokens, 50_000);
        assert_eq!(
            tracker.request_stop(1, false),
            Some(SubagentStopReason::TokenBudget)
        );
    }
    #[test]
    fn test_reported_usage_max_without_double_counting() {
        let mut tracker = SubagentBudgetTracker::new(SubagentConfig::default(), 100_000);
        assert_eq!(
            tracker.charge(10_000, snapshot(0, 0, 0), snapshot(15_000, 12_000, 1)),
            Some(12_000)
        );
        tracker.charge(
            15_000,
            snapshot(15_000, 12_000, 1),
            snapshot(25_000, 20_000, 2),
        );
        assert_eq!(tracker.charged_tokens, 30_000);
        tracker.charge(
            1_000,
            snapshot(25_000, 20_000, 2),
            snapshot(99_000, 90_000, 4),
        );
        assert_eq!(tracker.charged_tokens, 31_000);
        assert_eq!(
            (
                tracker.reported_usage_requests,
                tracker.estimated_usage_requests
            ),
            (2, 1)
        );
    }
    #[test]
    fn test_context_boundary_and_saturating_charge() {
        let mut tracker = SubagentBudgetTracker::new(
            SubagentConfig {
                max_total_tokens: Some(u64::MAX),
                ..Default::default()
            },
            100,
        );
        assert_eq!(tracker.request_stop(100, true), None);
        assert_eq!(
            tracker.request_stop(101, true),
            Some(SubagentStopReason::ContextBudget)
        );
        tracker.charge(u64::MAX, snapshot(0, 0, 0), snapshot(0, 0, 0));
        tracker.charge(1, snapshot(0, 0, 0), snapshot(0, 0, 0));
        assert_eq!(tracker.charged_tokens, u64::MAX);
    }
    #[test]
    fn test_elapsed_safe_boundary() {
        let mut tracker = SubagentBudgetTracker::new(
            SubagentConfig {
                max_elapsed_ms: 10,
                ..Default::default()
            },
            100,
        );
        assert_eq!(tracker.elapsed_stop(), None);
        tracker.started_at -= std::time::Duration::from_millis(10);
        assert_eq!(
            tracker.batch_stop(1),
            Some(SubagentStopReason::ElapsedBudget)
        );
        assert_eq!(
            tracker.request_stop(1, false),
            Some(SubagentStopReason::ElapsedBudget)
        );
    }
}
