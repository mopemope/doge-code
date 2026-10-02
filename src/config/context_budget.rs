use serde::Deserialize;

/// Mode for the observation-aware preflight context governor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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
    pub fn from_optional_str(value: Option<&str>) -> Self {
        match value.map(|v| v.trim().to_ascii_lowercase()) {
            Some(ref s) if s == "observe" => Self::Observe,
            Some(ref s) if s == "off" => Self::Off,
            _ => Self::Auto,
        }
    }

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
        if let Some(mode) = &partial.mode {
            self.mode = ContextBudgetMode::from_optional_str(Some(mode));
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
pub struct PartialContextBudgetConfig {
    pub mode: Option<String>,
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
        assert_eq!(
            ContextBudgetMode::from_optional_str(Some("auto")),
            ContextBudgetMode::Auto
        );
        assert_eq!(
            ContextBudgetMode::from_optional_str(Some("observe")),
            ContextBudgetMode::Observe
        );
        assert_eq!(
            ContextBudgetMode::from_optional_str(Some("off")),
            ContextBudgetMode::Off
        );
        assert_eq!(
            ContextBudgetMode::from_optional_str(Some("bogus")),
            ContextBudgetMode::Auto
        );
        assert_eq!(
            ContextBudgetMode::from_optional_str(None),
            ContextBudgetMode::Auto
        );
    }

    #[test]
    fn test_merge_precedence_project_wins() {
        let file = PartialContextBudgetConfig {
            mode: Some("observe".to_string()),
        };
        let project = PartialContextBudgetConfig {
            mode: Some("off".to_string()),
        };
        let merged = merge_context_budget(Some(&file), Some(&project));
        assert_eq!(merged.mode, ContextBudgetMode::Off);
    }

    #[test]
    fn test_parses_toml_section() {
        let cfg: PartialContextBudgetConfig =
            toml::from_str(r#"mode = "observe""#).expect("parse context_budget section");
        assert_eq!(cfg.mode.as_deref(), Some("observe"));
        let resolved = merge_context_budget(None, Some(&cfg));
        assert_eq!(resolved.mode, ContextBudgetMode::Observe);
    }
}
