use serde::Deserialize;

/// Mode for the observation-aware preflight context governor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextBudgetMode {
    /// Measure the current request footprint and automatically reduce
    /// pressure (overlay drop -> recoverable offload -> unseen-safe
    /// compaction).
    #[default]
    Auto,
    /// Log the new estimate and recommended action but keep the existing
    /// previous-usage proactive behavior. Migration/debug only.
    Observe,
    /// Disable the governor entirely; legacy behavior only.
    Off,
}

impl ContextBudgetMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Observe => "observe",
            Self::Off => "off",
        }
    }
}

/// Resolved preflight context-budget configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBudgetConfig {
    pub mode: ContextBudgetMode,
}

impl Default for ContextBudgetConfig {
    fn default() -> Self {
        Self {
            mode: ContextBudgetMode::Auto,
        }
    }
}

impl ContextBudgetConfig {
    pub fn with_mode(mut self, mode: ContextBudgetMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn apply_partial(&mut self, partial: &PartialContextBudgetConfig) {
        if let Some(mode) = partial.mode {
            self.mode = mode;
        }
    }
}

/// Merge `[context_budget]` config with precedence:
/// default <- global config <- project config (field-wise).
pub fn merge_context_budget(
    file_cfg: Option<&PartialContextBudgetConfig>,
    project_cfg: Option<&PartialContextBudgetConfig>,
) -> ContextBudgetConfig {
    let mut resolved = ContextBudgetConfig::default();
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
pub struct PartialContextBudgetConfig {
    pub mode: Option<ContextBudgetMode>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_is_auto() {
        let cfg = ContextBudgetConfig::default();
        assert_eq!(cfg.mode, ContextBudgetMode::Auto);
    }

    #[test]
    fn test_mode_parsing() {
        let auto: PartialContextBudgetConfig =
            toml::from_str(r#"mode = "auto""#).expect("parse auto");
        assert_eq!(auto.mode, Some(ContextBudgetMode::Auto));
        let observe: PartialContextBudgetConfig =
            toml::from_str(r#"mode = "observe""#).expect("parse observe");
        assert_eq!(observe.mode, Some(ContextBudgetMode::Observe));
        let off: PartialContextBudgetConfig = toml::from_str(r#"mode = "off""#).expect("parse off");
        assert_eq!(off.mode, Some(ContextBudgetMode::Off));
        assert!(toml::from_str::<PartialContextBudgetConfig>(r#"mode = "bogus""#).is_err());
    }

    #[test]
    fn test_merge_precedence_project_wins() {
        let file = PartialContextBudgetConfig {
            mode: Some(ContextBudgetMode::Observe),
        };
        let project = PartialContextBudgetConfig {
            mode: Some(ContextBudgetMode::Off),
        };
        let merged = merge_context_budget(Some(&file), Some(&project));
        assert_eq!(merged.mode, ContextBudgetMode::Off);
    }

    #[test]
    fn test_parses_toml_section() {
        let cfg: PartialContextBudgetConfig =
            toml::from_str(r#"mode = "observe""#).expect("parse context_budget section");
        assert_eq!(cfg.mode, Some(ContextBudgetMode::Observe));
        let resolved = merge_context_budget(None, Some(&cfg));
        assert_eq!(resolved.mode, ContextBudgetMode::Observe);
    }
}
