use crate::config::SubagentConfig;
use crate::llm::client_core::RequestAttemptPolicy;
use crate::llm::types::Usage;
use std::sync::{Arc, Mutex};
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

    pub fn request_policy(&self, estimate: u64) -> Arc<SubagentRequestPolicy> {
        Arc::new(SubagentRequestPolicy {
            estimate,
            total_limit: self.total_limit,
            started_at: self.started_at,
            max_elapsed_ms: self.limits.max_elapsed_ms,
            state: Mutex::new(RequestCharges {
                charged_tokens: self.charged_tokens,
                ..Default::default()
            }),
        })
    }

    /// Apply this operation's transport-local charges even on failure/cancel.
    /// Shared provider ledger deltas are never used for worker attribution.
    pub fn charge(&mut self, policy: &SubagentRequestPolicy) -> Option<u32> {
        let state = policy
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        self.charged_tokens = state.charged_tokens;
        self.reported_usage_requests = self.reported_usage_requests.saturating_add(state.reported);
        self.estimated_usage_requests = self
            .estimated_usage_requests
            .saturating_add(state.estimated);
        state.prompt_tokens
    }
}

#[derive(Debug)]
pub(super) struct RequestBudgetExceeded(pub SubagentStopReason);

impl std::fmt::Display for RequestBudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "subagent request budget exhausted: {}", self.0.as_str())
    }
}
impl std::error::Error for RequestBudgetExceeded {}

#[derive(Default)]
struct RequestCharges {
    charged_tokens: u64,
    reported: usize,
    estimated: usize,
    prompt_tokens: Option<u32>,
    awaiting_usage: bool,
}

pub(super) struct SubagentRequestPolicy {
    estimate: u64,
    total_limit: u64,
    started_at: Instant,
    max_elapsed_ms: u64,
    state: Mutex<RequestCharges>,
}

impl RequestAttemptPolicy for SubagentRequestPolicy {
    fn before_attempt(&self) -> anyhow::Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let reason = if self.started_at.elapsed().as_millis() >= u128::from(self.max_elapsed_ms) {
            Some(SubagentStopReason::ElapsedBudget)
        } else if state.charged_tokens >= self.total_limit
            || self.estimate > self.total_limit.saturating_sub(state.charged_tokens)
        {
            Some(SubagentStopReason::TokenBudget)
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(anyhow::anyhow!(RequestBudgetExceeded(reason)));
        }
        // Reserve before send. Failed/dropped attempts keep the reservation.
        state.charged_tokens = state.charged_tokens.saturating_add(self.estimate);
        state.estimated = state.estimated.saturating_add(1);
        state.awaiting_usage = true;
        Ok(())
    }

    fn observe_usage(&self, usage: &Usage) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if !state.awaiting_usage {
            return;
        }
        state.awaiting_usage = false;
        state.charged_tokens = state
            .charged_tokens
            .saturating_add(u64::from(usage.total_tokens).saturating_sub(self.estimate));
        state.estimated -= 1;
        state.reported = state.reported.saturating_add(1);
        state.prompt_tokens = Some(usage.prompt_tokens);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn charge(
        tracker: &mut SubagentBudgetTracker,
        estimate: u64,
        usage: Option<(u32, u32)>,
        attempts: usize,
    ) -> Option<u32> {
        let policy = tracker.request_policy(estimate);
        for _ in 0..attempts {
            policy.before_attempt().unwrap();
        }
        if let Some((total, prompt)) = usage {
            policy.observe_usage(
                &serde_json::from_value(serde_json::json!({
                    "total_tokens":total,"prompt_tokens":prompt,"completion_tokens":total-prompt
                }))
                .unwrap(),
            );
        }
        tracker.charge(&policy)
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
        charge(&mut tracker, 20_000, None, 1);
        assert_eq!(tracker.request_stop(30_000, true), None);
        charge(&mut tracker, 30_000, None, 1);
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
            charge(&mut tracker, 10_000, Some((15_000, 12_000)), 1),
            Some(12_000)
        );
        charge(&mut tracker, 15_000, Some((10_000, 8_000)), 1);
        assert_eq!(tracker.charged_tokens, 30_000);
        charge(&mut tracker, 1_000, None, 2);
        assert_eq!(tracker.charged_tokens, 32_000);
        assert_eq!(
            (
                tracker.reported_usage_requests,
                tracker.estimated_usage_requests
            ),
            (2, 2)
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
        charge(&mut tracker, u64::MAX, None, 1);
        assert!(tracker.request_policy(1).before_attempt().is_err());
        assert_eq!(tracker.charged_tokens, u64::MAX);
    }

    #[test]
    fn attempt_budget_retry_success_tops_up_only_the_reported_attempt() {
        let mut tracker = SubagentBudgetTracker::new(SubagentConfig::default(), 1000);
        assert_eq!(charge(&mut tracker, 100, Some((150, 100)), 2), Some(100));
        assert_eq!(tracker.charged_tokens, 250);
        assert_eq!(tracker.reported_usage_requests, 1);
        assert_eq!(tracker.estimated_usage_requests, 1);
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

    #[test]
    fn attempt_budget_elapsed_retry_denial_retains_failed_reservation() {
        let mut tracker = SubagentBudgetTracker::new(
            SubagentConfig {
                max_elapsed_ms: 10,
                ..Default::default()
            },
            100,
        );
        let mut policy = tracker.request_policy(20);
        policy.before_attempt().unwrap();
        Arc::get_mut(&mut policy).unwrap().started_at -= std::time::Duration::from_millis(10);
        let error = policy.before_attempt().unwrap_err();
        assert_eq!(
            error.downcast_ref::<RequestBudgetExceeded>().unwrap().0,
            SubagentStopReason::ElapsedBudget
        );
        assert_eq!(tracker.charge(&policy), None);
        assert_eq!(tracker.charged_tokens, 20);
        assert_eq!(tracker.estimated_usage_requests, 1);
    }
}
