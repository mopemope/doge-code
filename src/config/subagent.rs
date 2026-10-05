//! User/admin policy for ephemeral read-only workers. No tool input overrides.
use anyhow::{Result, ensure};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentConfig {
    pub max_iterations: usize,
    pub max_tool_calls: usize,
    pub max_elapsed_ms: u64,
    /// None derives a finite budget from the current model's effective limit.
    pub max_total_tokens: Option<u64>,
}

impl Default for SubagentConfig {
    fn default() -> Self {
        Self {
            max_iterations: 40,
            max_tool_calls: 64,
            max_elapsed_ms: 180_000,
            max_total_tokens: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PartialSubagentConfig {
    pub max_iterations: Option<usize>,
    pub max_tool_calls: Option<usize>,
    pub max_elapsed_ms: Option<u64>,
    pub max_total_tokens: Option<u64>,
}

impl PartialSubagentConfig {
    pub fn validate(&self) -> Result<()> {
        for (name, invalid) in [
            ("max_iterations", self.max_iterations == Some(0)),
            ("max_tool_calls", self.max_tool_calls == Some(0)),
            ("max_elapsed_ms", self.max_elapsed_ms == Some(0)),
            ("max_total_tokens", self.max_total_tokens == Some(0)),
        ] {
            ensure!(!invalid, "subagent.{name} must be greater than zero");
        }
        Ok(())
    }
}

/// Default <- user <- project, field-wise. Validate each explicit source so
/// a later override cannot hide invalid administrator configuration.
pub fn merge_subagent(
    user: Option<&PartialSubagentConfig>,
    project: Option<&PartialSubagentConfig>,
) -> Result<SubagentConfig> {
    let mut resolved = SubagentConfig::default();
    for partial in [user, project].into_iter().flatten() {
        partial.validate()?;
        if let Some(v) = partial.max_iterations {
            resolved.max_iterations = v;
        }
        if let Some(v) = partial.max_tool_calls {
            resolved.max_tool_calls = v;
        }
        if let Some(v) = partial.max_elapsed_ms {
            resolved.max_elapsed_ms = v;
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
    fn test_defaults_and_auto_budget() {
        let cfg = merge_subagent(None, None).expect("default");
        assert_eq!(
            cfg,
            SubagentConfig {
                max_iterations: 40,
                max_tool_calls: 64,
                max_elapsed_ms: 180_000,
                max_total_tokens: None
            }
        );
    }
    #[test]
    fn test_field_wise_precedence() {
        let user: PartialSubagentConfig =
            toml::from_str("max_iterations=8\nmax_tool_calls=12\nmax_total_tokens=5000")
                .expect("user");
        let project: PartialSubagentConfig =
            toml::from_str("max_iterations=2\nmax_elapsed_ms=1000").expect("project");
        let global = merge_subagent(Some(&user), None).expect("global");
        assert_eq!(global.max_iterations, 8);
        let cfg = merge_subagent(Some(&user), Some(&project)).expect("merged");
        assert_eq!(
            (
                cfg.max_iterations,
                cfg.max_tool_calls,
                cfg.max_elapsed_ms,
                cfg.max_total_tokens
            ),
            (2, 12, 1000, Some(5000))
        );
    }
    #[test]
    fn test_zero_config_rejected_even_when_overridden() {
        for field in [
            "max_iterations",
            "max_tool_calls",
            "max_elapsed_ms",
            "max_total_tokens",
        ] {
            let bad: PartialSubagentConfig = toml::from_str(&format!("{field}=0")).expect("parse");
            let good: PartialSubagentConfig = toml::from_str(&format!("{field}=1")).expect("parse");
            assert!(merge_subagent(Some(&bad), Some(&good)).is_err(), "{field}");
            assert!(merge_subagent(None, Some(&bad)).is_err(), "{field}");
        }
    }
}
