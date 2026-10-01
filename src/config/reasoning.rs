use serde::Deserialize;

/// Reasoning effort level sent to the provider as `reasoning_effort`.
///
/// v1 only uses `low` / `medium` / `high` to keep provider/model
/// compatibility broad. The enum is left extensible for future levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
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

    pub fn from_optional_str(value: Option<&str>, fallback: Self) -> Self {
        match value.map(|v| v.trim().to_ascii_lowercase()) {
            Some(ref s) if s == "low" => Self::Low,
            Some(ref s) if s == "medium" => Self::Medium,
            Some(ref s) if s == "high" => Self::High,
            _ => fallback,
        }
    }
}

/// Policy mode for reasoning budget control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningMode {
    #[default]
    Auto,
    Fixed,
    Off,
}

impl ReasoningMode {
    pub fn from_optional_str(value: Option<&str>) -> Self {
        match value.map(|v| v.trim().to_ascii_lowercase()) {
            Some(ref s) if s == "fixed" => Self::Fixed,
            Some(ref s) if s == "off" => Self::Off,
            _ => Self::Auto,
        }
    }

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
        if let Some(mode) = &partial.mode {
            self.mode = ReasoningMode::from_optional_str(Some(mode));
        }
        if let Some(v) = &partial.initial_effort {
            self.initial_effort =
                ReasoningEffort::from_optional_str(Some(v), ReasoningEffort::Medium);
        }
        if let Some(v) = &partial.routine_effort {
            self.routine_effort = ReasoningEffort::from_optional_str(Some(v), ReasoningEffort::Low);
        }
        if let Some(v) = &partial.deliberative_effort {
            self.deliberative_effort =
                ReasoningEffort::from_optional_str(Some(v), ReasoningEffort::Medium);
        }
        if let Some(v) = &partial.recovery_effort {
            self.recovery_effort =
                ReasoningEffort::from_optional_str(Some(v), ReasoningEffort::High);
        }
        if let Some(v) = &partial.fixed_effort {
            self.fixed_effort =
                ReasoningEffort::from_optional_str(Some(v), ReasoningEffort::Medium);
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
pub struct PartialReasoningConfig {
    pub mode: Option<String>,
    pub initial_effort: Option<String>,
    pub routine_effort: Option<String>,
    pub deliberative_effort: Option<String>,
    pub recovery_effort: Option<String>,
    pub fixed_effort: Option<String>,
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
            mode: Some("fixed".to_string()),
            routine_effort: Some("low".to_string()),
            fixed_effort: Some("high".to_string()),
            ..Default::default()
        };
        let project = PartialReasoningConfig {
            mode: Some("auto".to_string()),
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
        assert_eq!(cfg.mode.as_deref(), Some("fixed"));
        assert_eq!(cfg.fixed_effort.as_deref(), Some("high"));
        let resolved = merge_reasoning(None, Some(&cfg));
        assert_eq!(resolved.mode, ReasoningMode::Fixed);
        assert_eq!(resolved.fixed_effort, ReasoningEffort::High);
    }

    #[test]
    fn test_invalid_mode_falls_back_to_auto() {
        let cfg = ReasoningMode::from_optional_str(Some("bogus"));
        assert_eq!(cfg, ReasoningMode::Auto);
        let cfg = ReasoningMode::from_optional_str(None);
        assert_eq!(cfg, ReasoningMode::Auto);
    }

    #[test]
    fn test_invalid_effort_falls_back_without_panic() {
        let resolved = {
            let mut cfg = ReasoningConfig::default();
            cfg.apply_partial(&PartialReasoningConfig {
                routine_effort: Some("bogus".to_string()),
                ..Default::default()
            });
            cfg
        };
        assert_eq!(resolved.routine_effort, ReasoningEffort::Low);
    }
}
