use crate::utils::get_git_repository_root;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

pub const IGNORE_FILE: &str = ".dogeignore";

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub project_root: PathBuf,
    pub git_root: Option<PathBuf>,
    pub llm: LlmConfig,
    pub watch_config: WatchConfig, // Added watch configuration
    pub enable_stream_tools: bool,
    pub theme: String,                             // newly added
    pub project_instructions_file: Option<String>, // newly added
    pub no_repomap: bool,                          // newly added
    pub resume: bool,                              // newly added
    // Auto-compact threshold (configurable via env or config file)
    pub auto_compact_prompt_token_threshold: u32,
    // Per-model overrides for auto-compact threshold
    pub auto_compact_prompt_token_threshold_overrides: HashMap<String, u32>,
    pub show_diff: bool,
    // Allowed commands for execute_bash tool
    pub allowed_commands: Vec<String>,
    // Allowed paths for file access
    pub allowed_paths: Vec<PathBuf>,
    pub mcp_servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub enabled: bool,
    pub address: String,
    pub transport: String, // "stdio" or "http"
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            enabled: false,
            address: "127.0.0.1:8000".to_string(),
            transport: "http".to_string(),
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            api_key: None,
            project_root: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            git_root: None,
            llm: LlmConfig::default(),
            watch_config: WatchConfig::default(), // Added default watch config
            enable_stream_tools: false,
            theme: "dark".to_string(),
            project_instructions_file: None,
            no_repomap: false,
            resume: false,
            auto_compact_prompt_token_threshold: DEFAULT_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD,
            auto_compact_prompt_token_threshold_overrides: HashMap::new(),
            show_diff: false,
            allowed_commands: vec![],
            allowed_paths: vec![],
            mcp_servers: vec![McpServerConfig::default()],
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct LlmConfig {
    pub connect_timeout_ms: u64,
    pub request_timeout_ms: u64,
    pub read_idle_timeout_ms: u64,
    pub max_retries: usize,
    pub retry_base_ms: u64,
    pub retry_jitter_ms: u64,
    pub respect_retry_after: bool,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WatchConfig {
    pub include_patterns: Option<Vec<String>>,
    pub exclude_patterns: Option<Vec<String>>,
    pub debounce_delay_ms: Option<u64>,
    pub rate_limit_duration_ms: Option<u64>,
    pub ai_comment_pattern: Option<String>,
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
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            connect_timeout_ms: 5_000,
            request_timeout_ms: 60_000,
            read_idle_timeout_ms: 20_000,
            max_retries: 100,
            retry_base_ms: 1000,
            retry_jitter_ms: 5000,
            respect_retry_after: true,
            timeout_ms: 600_000, // 10 minutes
        }
    }
}

// Default threshold for auto-compacting conversation history
pub const DEFAULT_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD: u32 = 250_000;

// Threshold constant removed; use AppConfig.auto_compact_prompt_token_threshold at runtime

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct FileConfig {
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub project_root: Option<std::path::PathBuf>,
    pub llm: Option<PartialLlmConfig>,
    pub watch: Option<PartialWatchConfig>, // Added watch configuration
    pub enable_stream_tools: Option<bool>,
    pub theme: Option<String>,                     // newly added
    pub project_instructions_file: Option<String>, // newly added
    pub no_repomap: Option<bool>,                  // newly added
    pub resume: Option<bool>,                      // newly added
    // Auto-compact threshold (optional in config file)
    pub auto_compact_prompt_token_threshold: Option<u32>,
    // Auto-compact threshold overrides keyed by model name
    pub auto_compact_prompt_token_thresholds: Option<HashMap<String, u32>>,
    pub show_diff: Option<bool>,
    // Allowed commands for execute_bash tool
    pub allowed_commands: Option<Vec<String>>,
    // Allowed paths for file access
    pub allowed_paths: Option<Vec<PathBuf>>,
    pub mcp_servers: Option<Vec<PartialMcpServerConfig>>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct PartialWatchConfig {
    pub include_patterns: Option<Vec<String>>,
    pub exclude_patterns: Option<Vec<String>>,
    pub debounce_delay_ms: Option<u64>,
    pub rate_limit_duration_ms: Option<u64>,
    pub ai_comment_pattern: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct PartialMcpServerConfig {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub address: Option<String>,
    pub transport: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct PartialLlmConfig {
    pub connect_timeout_ms: Option<u64>,
    pub request_timeout_ms: Option<u64>,
    pub read_idle_timeout_ms: Option<u64>,
    pub max_retries: Option<usize>,
    pub retry_base_ms: Option<u64>,
    pub retry_jitter_ms: Option<u64>,
    pub respect_retry_after: Option<bool>,
    pub timeout_ms: Option<u64>,
}

impl AppConfig {
    pub fn auto_compact_prompt_token_threshold_for_model(&self, model: &str) -> u32 {
        self.auto_compact_prompt_token_threshold_overrides
            .get(model)
            .copied()
            .unwrap_or(self.auto_compact_prompt_token_threshold)
    }

    pub fn auto_compact_prompt_token_threshold_for_current_model(&self) -> u32 {
        self.auto_compact_prompt_token_threshold_for_model(&self.model)
    }

    pub fn from_cli(cli: crate::Cli) -> Result<Self> {
        let project_root = std::env::current_dir().context("resolve current dir")?;
        let git_root = get_git_repository_root(&project_root);

        // Load project-specific configuration first (highest priority after CLI args and env vars)
        let project_cfg = load_project_config(&project_root).unwrap_or_default();

        // Load global configuration
        let file_cfg = load_file_config().unwrap_or_default();

        let api_key = cli
            .api_key
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .or(project_cfg.api_key)
            .or(file_cfg.api_key);
        let base_url = if cli.base_url.is_empty() {
            std::env::var("OPENAI_BASE_URL")
                .ok()
                .or(project_cfg.base_url)
                .or(file_cfg.base_url)
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string())
        } else {
            cli.base_url
        };
        let model = if cli.model.is_empty() {
            std::env::var("OPENAI_MODEL")
                .ok()
                .or(project_cfg.model)
                .or(file_cfg.model)
                .unwrap_or_else(|| "gpt-4o-mini".to_string())
        } else {
            cli.model
        };
        let project_root = project_cfg
            .project_root
            .or(file_cfg.project_root)
            .unwrap_or(project_root);

        let llm_defaults = LlmConfig::default();
        let llm = {
            // Merge LLM config: project_cfg takes precedence over file_cfg
            let merged_llm_cfg = match (&project_cfg.llm, &file_cfg.llm) {
                (Some(project_llm), Some(file_llm)) => {
                    // Merge project and file configs
                    Some(PartialLlmConfig {
                        connect_timeout_ms: project_llm
                            .connect_timeout_ms
                            .or(file_llm.connect_timeout_ms),
                        request_timeout_ms: project_llm
                            .request_timeout_ms
                            .or(file_llm.request_timeout_ms),
                        read_idle_timeout_ms: project_llm
                            .read_idle_timeout_ms
                            .or(file_llm.read_idle_timeout_ms),
                        max_retries: project_llm.max_retries.or(file_llm.max_retries),
                        retry_base_ms: project_llm.retry_base_ms.or(file_llm.retry_base_ms),
                        retry_jitter_ms: project_llm.retry_jitter_ms.or(file_llm.retry_jitter_ms),
                        respect_retry_after: project_llm
                            .respect_retry_after
                            .or(file_llm.respect_retry_after),
                        timeout_ms: project_llm.timeout_ms.or(file_llm.timeout_ms),
                    })
                }
                (Some(project_llm), None) => Some(project_llm.clone()),
                (None, Some(file_llm)) => Some(file_llm.clone()),
                (None, None) => None,
            };

            if let Some(p) = merged_llm_cfg {
                LlmConfig {
                    connect_timeout_ms: p
                        .connect_timeout_ms
                        .unwrap_or(llm_defaults.connect_timeout_ms),
                    request_timeout_ms: p
                        .request_timeout_ms
                        .unwrap_or(llm_defaults.request_timeout_ms),
                    read_idle_timeout_ms: p
                        .read_idle_timeout_ms
                        .unwrap_or(llm_defaults.read_idle_timeout_ms),
                    max_retries: p.max_retries.unwrap_or(llm_defaults.max_retries),
                    retry_base_ms: p.retry_base_ms.unwrap_or(llm_defaults.retry_base_ms),
                    retry_jitter_ms: p.retry_jitter_ms.unwrap_or(llm_defaults.retry_jitter_ms),
                    respect_retry_after: p
                        .respect_retry_after
                        .unwrap_or(llm_defaults.respect_retry_after),
                    timeout_ms: p.timeout_ms.unwrap_or(llm_defaults.timeout_ms),
                }
            } else {
                llm_defaults
            }
        };

        // Handle MCP server configurations
        let mcp_servers = {
            // Merge MCP server configs from project and file configs
            let mut merged_mcp_servers = Vec::new();

            // Add servers from global config
            if let Some(file_mcp_servers) = &file_cfg.mcp_servers {
                for server in file_mcp_servers {
                    merged_mcp_servers.push(server.clone());
                }
            }

            // Add or override with servers from project config
            if let Some(project_mcp_servers) = &project_cfg.mcp_servers {
                for project_server in project_mcp_servers {
                    // Check if a server with the same name already exists
                    if let Some(name) = &project_server.name {
                        if let Some(existing_server) = merged_mcp_servers
                            .iter_mut()
                            .find(|s| s.name.as_ref() == Some(name))
                        {
                            // Override existing server settings
                            if let Some(enabled) = project_server.enabled {
                                existing_server.enabled = Some(enabled);
                            }
                            if let Some(address) = &project_server.address {
                                existing_server.address = Some(address.clone());
                            }
                            if let Some(transport) = &project_server.transport {
                                existing_server.transport = Some(transport.clone());
                            }
                        } else {
                            // Add new server
                            merged_mcp_servers.push(project_server.clone());
                        }
                    } else {
                        // Add new server without name check
                        merged_mcp_servers.push(project_server.clone());
                    }
                }
            }

            // Convert to McpServerConfig with defaults
            let mcp_defaults = McpServerConfig::default();
            merged_mcp_servers
                .into_iter()
                .map(|partial| McpServerConfig {
                    name: partial.name.unwrap_or_else(|| "default".to_string()),
                    enabled: partial.enabled.unwrap_or(mcp_defaults.enabled),
                    address: partial
                        .address
                        .unwrap_or_else(|| mcp_defaults.address.clone()),
                    transport: partial
                        .transport
                        .unwrap_or_else(|| mcp_defaults.transport.clone()),
                })
                .collect()
        };

        // Add theme setting (project config takes precedence)
        let theme = project_cfg
            .theme
            .or(file_cfg.theme)
            .unwrap_or_else(|| "dark".to_string());

        // Add project_instructions_file setting (CLI args take precedence)
        let project_instructions_file = cli
            .instructions_file
            .or(project_cfg.project_instructions_file)
            .or(file_cfg.project_instructions_file);

        // Determine auto-compact threshold (priority: env var -> project config -> global config -> default)
        let auto_compact_prompt_token_threshold =
            std::env::var("DOGE_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD")
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .or(project_cfg.auto_compact_prompt_token_threshold)
                .or(file_cfg.auto_compact_prompt_token_threshold)
                .unwrap_or(DEFAULT_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD);

        let mut auto_compact_prompt_token_threshold_overrides = file_cfg
            .auto_compact_prompt_token_thresholds
            .clone()
            .unwrap_or_default();
        if let Some(project_overrides) = project_cfg.auto_compact_prompt_token_thresholds.clone() {
            for (model, threshold) in project_overrides {
                auto_compact_prompt_token_threshold_overrides.insert(model, threshold);
            }
        }

        // Handle watch configuration (project config takes precedence over file config)
        let watch_config = {
            let default_watch_cfg = WatchConfig::default();
            let mut watch_cfg = default_watch_cfg.clone();

            // Apply file config values if present
            if let Some(file_watch) = &file_cfg.watch {
                if let Some(include_patterns) = &file_watch.include_patterns {
                    watch_cfg.include_patterns = Some(include_patterns.clone());
                }
                if let Some(exclude_patterns) = &file_watch.exclude_patterns {
                    watch_cfg.exclude_patterns = Some(exclude_patterns.clone());
                }
                if let Some(debounce_delay_ms) = file_watch.debounce_delay_ms {
                    watch_cfg.debounce_delay_ms = Some(debounce_delay_ms);
                }
                if let Some(rate_limit_duration_ms) = file_watch.rate_limit_duration_ms {
                    watch_cfg.rate_limit_duration_ms = Some(rate_limit_duration_ms);
                }
                if let Some(ai_comment_pattern) = &file_watch.ai_comment_pattern {
                    watch_cfg.ai_comment_pattern = Some(ai_comment_pattern.clone());
                }
            }

            // Apply project config values if present (overrides file config)
            if let Some(project_watch) = &project_cfg.watch {
                if let Some(include_patterns) = &project_watch.include_patterns {
                    watch_cfg.include_patterns = Some(include_patterns.clone());
                }
                if let Some(exclude_patterns) = &project_watch.exclude_patterns {
                    watch_cfg.exclude_patterns = Some(exclude_patterns.clone());
                }
                if let Some(debounce_delay_ms) = project_watch.debounce_delay_ms {
                    watch_cfg.debounce_delay_ms = Some(debounce_delay_ms);
                }
                if let Some(rate_limit_duration_ms) = project_watch.rate_limit_duration_ms {
                    watch_cfg.rate_limit_duration_ms = Some(rate_limit_duration_ms);
                }
                if let Some(ai_comment_pattern) = &project_watch.ai_comment_pattern {
                    watch_cfg.ai_comment_pattern = Some(ai_comment_pattern.clone());
                }
            }

            watch_cfg
        };

        Ok(Self {
            base_url,
            model,
            api_key,
            project_root,
            git_root,
            llm,
            watch_config, // Added watch config
            enable_stream_tools: std::env::var("DOGE_STREAM_TOOLS")
                .ok()
                .and_then(|v| v.parse().ok())
                .or(project_cfg.enable_stream_tools)
                .or(file_cfg.enable_stream_tools)
                .unwrap_or(false),
            theme,                     // newly added
            project_instructions_file, // newly added
            no_repomap: cli.no_repomap
                || project_cfg.no_repomap.unwrap_or(false)
                || file_cfg.no_repomap.unwrap_or(false),
            resume: cli.resume,
            auto_compact_prompt_token_threshold,
            auto_compact_prompt_token_threshold_overrides,
            show_diff: project_cfg
                .show_diff
                .or(file_cfg.show_diff)
                .unwrap_or(false),
            allowed_commands: project_cfg
                .allowed_commands
                .or(file_cfg.allowed_commands)
                .unwrap_or_default(),
            allowed_paths: project_cfg
                .allowed_paths
                .or(file_cfg.allowed_paths)
                .unwrap_or_default(),
            mcp_servers,
        })
    }
}

pub fn load_file_config() -> Result<FileConfig> {
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn candidate_paths() -> Vec<PathBuf> {
        let mut v = Vec::new();
        if let Ok(p) = env::var("DOGE_CODE_CONFIG") {
            v.push(PathBuf::from(p));
        }
        if let Ok(xdg_home) = env::var("XDG_CONFIG_HOME") {
            v.push(Path::new(&xdg_home).join("doge-code/config.toml"));
        } else if let Ok(home) = env::var("HOME") {
            v.push(Path::new(&home).join(".config/doge-code/config.toml"));
        }
        if let Ok(dirs) = env::var("XDG_CONFIG_DIRS") {
            for d in dirs.split(':') {
                if !d.is_empty() {
                    v.push(Path::new(d).join("doge-code/config.toml"));
                }
            }
        }
        v
    }

    for p in candidate_paths() {
        if p.exists() {
            let s = fs::read_to_string(&p)
                .with_context(|| format!("read config file: {}", p.display()))?;
            match toml::from_str::<FileConfig>(&s) {
                Ok(cfg) => {
                    info!(path=%p.display(), "loaded config file");
                    return Ok(cfg);
                }
                Err(e) => {
                    warn!(path=%p.display(), error=%e.to_string(), "parse config failed");
                    continue;
                }
            }
        }
    }
    Ok(FileConfig::default())
}

/// Load project-specific configuration from .doge/config.toml
pub fn load_project_config(project_root: &Path) -> Result<FileConfig> {
    let project_config_path = project_root.join(".doge").join("config.toml");

    if project_config_path.exists() {
        let s = fs::read_to_string(&project_config_path).with_context(|| {
            format!(
                "read project config file: {}",
                project_config_path.display()
            )
        })?;
        match toml::from_str::<FileConfig>(&s) {
            Ok(cfg) => {
                info!(path=%project_config_path.display(), "loaded project config file");
                Ok(cfg)
            }
            Err(e) => {
                warn!(path=%project_config_path.display(), error=%e.to_string(), "parse project config failed");
                Ok(FileConfig::default())
            }
        }
    } else {
        Ok(FileConfig::default())
    }
}

#[cfg(test)]
mod tests;
