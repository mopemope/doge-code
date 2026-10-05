//! Run-wide resource budget for the main agent loop.
//!
//! The main agent loop previously enforced only a fixed iteration cap via
//! `ToolRuntime::max_iters`. This module owns the run-wide policy:
//! iterations, main-dispatch tool calls, elapsed wall time, and cumulative
//! model tokens. Budget exhaustion is normal partial completion, never
//! cancellation or a provider error.

use anyhow::{Result, ensure};
use serde::Deserialize;

/// Resolved run-wide budget. Optional limits are disabled when `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentBudgetConfig {
    pub max_iterations: usize,
    pub max_tool_calls: Option<usize>,
    pub max_elapsed_ms: Option<u64>,
    pub max_total_tokens: Option<u64>,
}

impl Default for AgentBudgetConfig {
    fn default() -> Self {
        Self {
            max_iterations: 256,
            max_tool_calls: None,
            max_elapsed_ms: None,
            max_total_tokens: None,
        }
    }
}

/// Partial `[agent_budget]` table. Strict schema: unknown keys are rejected by
/// `#[serde(deny_unknown_fields)]` on the file config.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PartialAgentBudgetConfig {
    pub max_iterations: Option<usize>,
    pub max_tool_calls: Option<usize>,
    pub max_elapsed_ms: Option<u64>,
    pub max_total_tokens: Option<u64>,
}

impl PartialAgentBudgetConfig {
    pub fn validate(&self) -> Result<()> {
        for (name, invalid) in [
            ("max_iterations", self.max_iterations == Some(0)),
            ("max_tool_calls", self.max_tool_calls == Some(0)),
            ("max_elapsed_ms", self.max_elapsed_ms == Some(0)),
            ("max_total_tokens", self.max_total_tokens == Some(0)),
        ] {
            ensure!(!invalid, "agent_budget.{name} must be greater than zero");
        }
        Ok(())
    }
}

/// Merge `[agent_budget]` with precedence:
/// default <- user/global config <- project config (field-wise).
pub fn merge_agent_budget(
    user: Option<&PartialAgentBudgetConfig>,
    project: Option<&PartialAgentBudgetConfig>,
) -> Result<AgentBudgetConfig> {
    let mut resolved = AgentBudgetConfig::default();
    for partial in [user, project].into_iter().flatten() {
        partial.validate()?;
        if let Some(v) = partial.max_iterations {
            resolved.max_iterations = v;
        }
        if let Some(v) = partial.max_tool_calls {
            resolved.max_tool_calls = Some(v);
        }
        if let Some(v) = partial.max_elapsed_ms {
            resolved.max_elapsed_ms = Some(v);
        }
        if let Some(v) = partial.max_total_tokens {
            resolved.max_total_tokens = Some(v);
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_budget_defaults_preserve_existing_iteration_limit() {
        let cfg = merge_agent_budget(None, None).expect("default");
        assert_eq!(cfg.max_iterations, 256);
        assert_eq!(cfg.max_tool_calls, None);
        assert_eq!(cfg.max_elapsed_ms, None);
        assert_eq!(cfg.max_total_tokens, None);
    }

    #[test]
    fn agent_budget_global_project_field_wise_precedence() {
        let user: PartialAgentBudgetConfig =
            toml::from_str("max_iterations = 128\nmax_total_tokens = 800000").expect("user");
        let project: PartialAgentBudgetConfig =
            toml::from_str("max_elapsed_ms = 900000").expect("project");
        let cfg = merge_agent_budget(Some(&user), Some(&project)).expect("merged");
        assert_eq!(cfg.max_iterations, 128);
        assert_eq!(cfg.max_total_tokens, Some(800000));
        assert_eq!(cfg.max_elapsed_ms, Some(900000));
        assert_eq!(cfg.max_tool_calls, None);
    }

    #[test]
    fn agent_budget_zero_rejected() {
        for field in [
            "max_iterations",
            "max_tool_calls",
            "max_elapsed_ms",
            "max_total_tokens",
        ] {
            let bad: PartialAgentBudgetConfig =
                toml::from_str(&format!("{field} = 0")).expect("parse");
            let good: PartialAgentBudgetConfig =
                toml::from_str(&format!("{field} = 1")).expect("parse");
            assert!(
                merge_agent_budget(Some(&bad), Some(&good)).is_err(),
                "{field}"
            );
            assert!(merge_agent_budget(None, Some(&bad)).is_err(), "{field}");
        }
    }
}
