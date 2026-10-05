use serde::Deserialize;

/// Reasoning effort level sent to the provider as `reasoning_effort`.
///
/// v1 only uses `low` / `medium` / `high` to keep provider/model
/// compatibility broad. The enum is left extensible for future levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Low,
    #[default]
    Medium,
    High,
}

impl ReasoningEffort {
    pub fn as_api_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Policy mode for reasoning budget control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningMode {
    #[default]
    Auto,
    Fixed,
    Off,
}

impl ReasoningMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Fixed => "fixed",
            Self::Off => "off",
        }
    }
}

/// Resolved reasoning configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningConfig {
    pub mode: ReasoningMode,
    pub initial_effort: ReasoningEffort,
    pub routine_effort: ReasoningEffort,
    pub deliberative_effort: ReasoningEffort,
    pub recovery_effort: ReasoningEffort,
    pub fixed_effort: ReasoningEffort,
}

impl Default for ReasoningConfig {
    fn default() -> Self {
        Self {
            mode: ReasoningMode::Auto,
            initial_effort: ReasoningEffort::Medium,
            routine_effort: ReasoningEffort::Low,
            deliberative_effort: ReasoningEffort::Medium,
            recovery_effort: ReasoningEffort::High,
            fixed_effort: ReasoningEffort::Medium,
        }
    }
}

impl ReasoningConfig {
    pub fn apply_partial(&mut self, partial: &PartialReasoningConfig) {
        if let Some(mode) = partial.mode {
            self.mode = mode;
        }
        if let Some(v) = partial.initial_effort {
            self.initial_effort = v;
        }
        if let Some(v) = partial.routine_effort {
            self.routine_effort = v;
        }
        if let Some(v) = partial.deliberative_effort {
            self.deliberative_effort = v;
        }
        if let Some(v) = partial.recovery_effort {
            self.recovery_effort = v;
        }
        if let Some(v) = partial.fixed_effort {
            self.fixed_effort = v;
        }
    }
}

/// Merge `[reasoning]` config with precedence:
/// default <- global config <- project config (field-wise).
pub fn merge_reasoning(
    file_cfg: Option<&PartialReasoningConfig>,
    project_cfg: Option<&PartialReasoningConfig>,
) -> ReasoningConfig {
    let mut resolved = ReasoningConfig::default();
    if let Some(partial) = file_cfg {
        resolved.apply_partial(partial);
    }
    if let Some(partial) = project_cfg {
        resolved.apply_partial(partial);
    }
    resolved
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PartialReasoningConfig {
    pub mode: Option<ReasoningMode>,
    pub initial_effort: Option<ReasoningEffort>,
    pub routine_effort: Option<ReasoningEffort>,
    pub deliberative_effort: Option<ReasoningEffort>,
    pub recovery_effort: Option<ReasoningEffort>,
    pub fixed_effort: Option<ReasoningEffort>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_values() {
        let cfg = ReasoningConfig::default();
        assert_eq!(cfg.mode, ReasoningMode::Auto);
        assert_eq!(cfg.initial_effort, ReasoningEffort::Medium);
        assert_eq!(cfg.routine_effort, ReasoningEffort::Low);
        assert_eq!(cfg.deliberative_effort, ReasoningEffort::Medium);
        assert_eq!(cfg.recovery_effort, ReasoningEffort::High);
        assert_eq!(cfg.fixed_effort, ReasoningEffort::Medium);
    }

    #[test]
    fn test_effort_api_str() {
        assert_eq!(ReasoningEffort::Low.as_api_str(), "low");
        assert_eq!(ReasoningEffort::Medium.as_api_str(), "medium");
        assert_eq!(ReasoningEffort::High.as_api_str(), "high");
    }

    #[test]
    fn test_merge_precedence_project_wins() {
        let file = PartialReasoningConfig {
            mode: Some(ReasoningMode::Fixed),
            routine_effort: Some(ReasoningEffort::Low),
            fixed_effort: Some(ReasoningEffort::High),
            ..Default::default()
        };
        let project = PartialReasoningConfig {
            mode: Some(ReasoningMode::Auto),
            routine_effort: None,
            ..Default::default()
        };
        let merged = merge_reasoning(Some(&file), Some(&project));
        assert_eq!(merged.mode, ReasoningMode::Auto);
        assert_eq!(merged.routine_effort, ReasoningEffort::Low);
        assert_eq!(merged.fixed_effort, ReasoningEffort::High);
    }

    #[test]
    fn test_parses_toml_section() {
        let cfg: PartialReasoningConfig = toml::from_str(
            r#"mode = "fixed"
               fixed_effort = "high"
               routine_effort = "low""#,
        )
        .expect("parse reasoning section");
        assert_eq!(cfg.mode, Some(ReasoningMode::Fixed));
        assert_eq!(cfg.fixed_effort, Some(ReasoningEffort::High));
        let resolved = merge_reasoning(None, Some(&cfg));
        assert_eq!(resolved.mode, ReasoningMode::Fixed);
        assert_eq!(resolved.fixed_effort, ReasoningEffort::High);
    }

    #[test]
    fn test_invalid_mode_is_rejected() {
        assert!(toml::from_str::<PartialReasoningConfig>(r#"mode = "bogus""#).is_err());
    }

    #[test]
    fn test_invalid_effort_is_rejected() {
        assert!(toml::from_str::<PartialReasoningConfig>(r#"routine_effort = "bogus""#).is_err());
    }
}
