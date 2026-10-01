use serde::Deserialize;

/// Routing mode for deferred tool exposure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolRoutingMode {
    /// Defer when the catalog is large enough, otherwise expose everything.
    #[default]
    Auto,
    /// Legacy behavior: expose every known tool immediately.
    Eager,
    /// Always expose only the core set plus `tool_search`.
    Deferred,
}

impl ToolRoutingMode {
    pub fn from_optional_str(value: Option<&str>) -> Self {
        match value.map(|v| v.trim().to_ascii_lowercase()) {
            Some(ref s) if s == "eager" => Self::Eager,
            Some(ref s) if s == "deferred" => Self::Deferred,
            _ => Self::Auto,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Eager => "eager",
            Self::Deferred => "deferred",
        }
    }
}

/// Resolved tool-routing configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRoutingConfig {
    pub mode: ToolRoutingMode,
    pub search_result_limit: usize,
}

impl Default for ToolRoutingConfig {
    fn default() -> Self {
        Self {
            mode: ToolRoutingMode::Auto,
            search_result_limit: DEFAULT_TOOL_SEARCH_RESULT_LIMIT,
        }
    }
}

impl ToolRoutingConfig {
    pub fn with_mode(mut self, mode: ToolRoutingMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn with_search_result_limit(mut self, limit: usize) -> Self {
        self.search_result_limit = clamp_search_result_limit(limit);
        self
    }

    pub fn apply_partial(&mut self, partial: &PartialToolRoutingConfig) {
        if let Some(mode) = &partial.mode {
            self.mode = ToolRoutingMode::from_optional_str(Some(mode));
        }
        if let Some(limit) = partial.search_result_limit {
            self.search_result_limit = clamp_search_result_limit(limit as usize);
        }
    }

    /// Configured per-search default used when the caller passes `limit == 0`
    /// or omits it. Always within 1..=10.
    pub fn effective_limit(&self) -> usize {
        clamp_search_result_limit(self.search_result_limit)
    }

    /// `auto` defers at or above the catalog-size threshold; explicit modes
    /// always apply. `count` includes the managed `tool_search` entry.
    pub fn is_deferred_for_count(&self, count: usize) -> bool {
        match self.mode {
            ToolRoutingMode::Eager => false,
            ToolRoutingMode::Deferred => true,
            ToolRoutingMode::Auto => count >= AUTO_DEFER_TOOL_COUNT_THRESHOLD,
        }
    }
}

/// Default number of matches returned by `tool_search` when the caller
/// does not specify `limit`.
pub const DEFAULT_TOOL_SEARCH_RESULT_LIMIT: usize = 5;
/// Hard upper bound for a single `tool_search` response.
pub const MAX_TOOL_SEARCH_RESULT_LIMIT: usize = 10;
/// Lower bound for a single `tool_search` response.
pub const MIN_TOOL_SEARCH_RESULT_LIMIT: usize = 1;
/// Catalog size at or above which `auto` mode switches to deferred routing.
pub const AUTO_DEFER_TOOL_COUNT_THRESHOLD: usize = 10;

pub fn clamp_search_result_limit(limit: usize) -> usize {
    limit.clamp(MIN_TOOL_SEARCH_RESULT_LIMIT, MAX_TOOL_SEARCH_RESULT_LIMIT)
}

pub fn merge_tool_routing(
    file_cfg: Option<&PartialToolRoutingConfig>,
    project_cfg: Option<&PartialToolRoutingConfig>,
) -> ToolRoutingConfig {
    let mut resolved = ToolRoutingConfig::default();
    if let Some(partial) = file_cfg {
        resolved.apply_partial(partial);
    }
    if let Some(partial) = project_cfg {
        resolved.apply_partial(partial);
    }
    resolved
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct PartialToolRoutingConfig {
    pub mode: Option<String>,
    pub search_result_limit: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_is_auto_with_limit_five() {
        let cfg = ToolRoutingConfig::default();
        assert_eq!(cfg.mode, ToolRoutingMode::Auto);
        assert_eq!(cfg.search_result_limit, 5);
    }

    #[test]
    fn test_clamp_search_result_limit_bounds() {
        assert_eq!(clamp_search_result_limit(0), 1);
        assert_eq!(clamp_search_result_limit(1), 1);
        assert_eq!(clamp_search_result_limit(5), 5);
        assert_eq!(clamp_search_result_limit(10), 10);
        assert_eq!(clamp_search_result_limit(99), 10);
    }

    #[test]
    fn test_merge_precedence_project_wins() {
        let file = PartialToolRoutingConfig {
            mode: Some("deferred".to_string()),
            search_result_limit: Some(3),
        };
        let project = PartialToolRoutingConfig {
            mode: Some("eager".to_string()),
            search_result_limit: None,
        };
        let merged = merge_tool_routing(Some(&file), Some(&project));
        assert_eq!(merged.mode, ToolRoutingMode::Eager);
        assert_eq!(merged.search_result_limit, 3);
    }

    #[test]
    fn test_parses_toml_section() {
        let cfg: PartialToolRoutingConfig = toml::from_str(
            r#"mode = "deferred"
               search_result_limit = 7"#,
        )
        .expect("parse tool_routing section");
        assert_eq!(cfg.mode.as_deref(), Some("deferred"));
        assert_eq!(cfg.search_result_limit, Some(7));
    }

    #[test]
    fn test_effective_limit_is_clamped_configured_value() {
        let cfg = ToolRoutingConfig::default();
        assert_eq!(cfg.effective_limit(), 5);
        let cfg = ToolRoutingConfig::default().with_search_result_limit(99);
        assert_eq!(cfg.effective_limit(), 10);
        let cfg = ToolRoutingConfig::default().with_search_result_limit(0);
        assert_eq!(cfg.effective_limit(), 1);
    }

    #[test]
    fn test_is_deferred_for_count_threshold() {
        let auto = ToolRoutingConfig::default();
        assert!(!auto.is_deferred_for_count(AUTO_DEFER_TOOL_COUNT_THRESHOLD - 1));
        assert!(auto.is_deferred_for_count(AUTO_DEFER_TOOL_COUNT_THRESHOLD));
        let eager = ToolRoutingConfig::default().with_mode(ToolRoutingMode::Eager);
        assert!(!eager.is_deferred_for_count(1_000));
        let deferred = ToolRoutingConfig::default().with_mode(ToolRoutingMode::Deferred);
        assert!(deferred.is_deferred_for_count(0));
    }
}
