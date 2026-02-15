use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct WatchConfig {
    pub include_patterns: Option<Vec<String>>,
    pub exclude_patterns: Option<Vec<String>>,
    pub debounce_delay_ms: Option<u64>,
    pub rate_limit_duration_ms: Option<u64>,
    pub ai_comment_pattern: Option<String>,
    pub backup_enabled: Option<bool>,
    pub backup_dir: Option<String>,
    pub backup_keep: Option<usize>,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            include_patterns: Some(vec![
                "**/*.rs".to_string(),
                "**/*.js".to_string(),
                "**/*.ts".to_string(),
                "**/*.jsx".to_string(),
                "**/*.tsx".to_string(),
                "**/*.py".to_string(),
                "**/*.go".to_string(),
                "**/*.java".to_string(),
                "**/*.md".to_string(),
                "**/*.txt".to_string(),
                "**/*.yaml".to_string(),
                "**/*.yml".to_string(),
                "**/*.toml".to_string(),
                "**/*.json".to_string(),
                "**/*.html".to_string(),
                "**/*.css".to_string(),
                "**/*.xml".to_string(),
            ]),
            exclude_patterns: Some(vec![
                "**/node_modules/**".to_string(),
                "**/target/**".to_string(),
                "**/build/**".to_string(),
                "**/dist/**".to_string(),
                "**/.git/**".to_string(),
                "**/vendor/**".to_string(),
            ]),
            debounce_delay_ms: Some(500),
            rate_limit_duration_ms: Some(2000),
            ai_comment_pattern: Some("// AI!:".to_string()),
            backup_enabled: Some(true),
            backup_dir: Some(".doge/backup".to_string()),
            backup_keep: Some(5),
        }
    }
}

impl WatchConfig {
    pub fn apply_partial(&mut self, partial: &PartialWatchConfig) {
        if let Some(v) = &partial.include_patterns {
            self.include_patterns = Some(v.clone());
        }
        if let Some(v) = &partial.exclude_patterns {
            self.exclude_patterns = Some(v.clone());
        }
        if let Some(v) = partial.debounce_delay_ms {
            self.debounce_delay_ms = Some(v);
        }
        if let Some(v) = partial.rate_limit_duration_ms {
            self.rate_limit_duration_ms = Some(v);
        }
        if let Some(v) = &partial.ai_comment_pattern {
            self.ai_comment_pattern = Some(v.clone());
        }
        if let Some(v) = partial.backup_enabled {
            self.backup_enabled = Some(v);
        }
        if let Some(v) = &partial.backup_dir {
            self.backup_dir = Some(v.clone());
        }
        if let Some(v) = partial.backup_keep {
            self.backup_keep = Some(v);
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct PartialWatchConfig {
    pub include_patterns: Option<Vec<String>>,
    pub exclude_patterns: Option<Vec<String>>,
    pub debounce_delay_ms: Option<u64>,
    pub rate_limit_duration_ms: Option<u64>,
    pub ai_comment_pattern: Option<String>,
    pub backup_enabled: Option<bool>,
    pub backup_dir: Option<String>,
    pub backup_keep: Option<usize>,
}
