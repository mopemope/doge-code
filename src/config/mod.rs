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
    // Timeout for tool-executed commands (execute_bash/execute_shell)
    pub command_timeout_ms: u64,
    pub mcp_servers: Vec<McpServerConfig>,
    pub rewrite_timeout_sec: u64,
    pub verification: VerificationConfig,
    pub rag: RagConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RagConfig {
    pub batch_size: usize,
    pub enabled: bool,
    pub auto_update: bool,
}

impl Default for RagConfig {
    fn default() -> Self {
        Self {
            batch_size: 8,
            enabled: true,
            auto_update: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct VerificationConfig {
    pub enabled: bool,
    pub timeout_ms: u64,
    pub commands: VerificationCommands,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VerificationCommands {
    pub rust: Vec<String>,
    pub python: Vec<String>,
    pub node: Vec<String>,
    pub typescript: Vec<String>,
    pub go: Vec<String>,
}

impl Default for VerificationCommands {
    fn default() -> Self {
        Self {
            rust: vec![
                "cargo".to_string(),
                "check".to_string(),
                "--quiet".to_string(),
                "--message-format=short".to_string(),
            ],
            python: vec![
                "python3".to_string(),
                "-m".to_string(),
                "py_compile".to_string(),
                "{path}".to_string(),
            ],
            node: vec![
                "node".to_string(),
                "--check".to_string(),
                "{path}".to_string(),
            ],
            typescript: vec![
                "tsc".to_string(),
                "--noEmit".to_string(),
                "--allowSyntheticDefaultImports".to_string(),
                "--target".to_string(),
                "esnext".to_string(),
                "--moduleResolution".to_string(),
                "node".to_string(),
                "{path}".to_string(),
            ],
            go: vec!["go".to_string(), "vet".to_string(), "{path}".to_string()],
        }
    }
}

impl Default for VerificationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_ms: 120_000,
            commands: VerificationCommands::default(),
        }
    }
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
            theme: "cyberpunk".to_string(),
            project_instructions_file: None,
            no_repomap: false,
            resume: false,
            auto_compact_prompt_token_threshold: DEFAULT_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD,
            auto_compact_prompt_token_threshold_overrides: HashMap::new(),
            show_diff: false,
            allowed_commands: vec![],
            allowed_paths: vec![],
            command_timeout_ms: DEFAULT_COMMAND_TIMEOUT_MS,
            mcp_servers: vec![McpServerConfig::default()],
            rewrite_timeout_sec: 30,
            verification: VerificationConfig::default(),
            rag: RagConfig::default(),
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
    /// Context window size in tokens for the model
    pub context_window_size: Option<u32>,
}

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
            context_window_size: None,
        }
    }
}

// Default threshold for auto-compacting conversation history
pub const DEFAULT_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD: u32 = 250_000;
pub const DEFAULT_COMMAND_TIMEOUT_MS: u64 = 300_000;

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
    pub rewrite_timeout_sec: Option<u64>,
    pub command_timeout_ms: Option<u64>,
    pub verification: Option<PartialVerificationConfig>,
    pub rag: Option<PartialRagConfig>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct PartialRagConfig {
    pub batch_size: Option<usize>,
    pub enabled: Option<bool>,
    pub auto_update: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct PartialVerificationConfig {
    pub enabled: Option<bool>,
    pub timeout_ms: Option<u64>,
    pub commands: Option<PartialVerificationCommands>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct PartialVerificationCommands {
    pub rust: Option<Vec<String>>,
    pub python: Option<Vec<String>>,
    pub node: Option<Vec<String>>,
    pub typescript: Option<Vec<String>>,
    pub go: Option<Vec<String>>,
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
    /// Context window size in tokens for the model
    pub context_window_size: Option<u32>,
}

impl AppConfig {
    pub fn auto_compact_prompt_token_threshold_for_model(&self, model: &str) -> u32 {
        self.auto_compact_prompt_token_threshold_overrides
            .get(model)
            .copied()
            .unwrap_or(self.auto_compact_prompt_token_threshold)
    }

    /// Get context window size for the current model
    /// Returns the configured size or a default based on model name
    pub fn get_context_window_size(&self) -> Option<u32> {
        // First check if explicitly configured
        if let Some(size) = self.llm.context_window_size {
            return Some(size);
        }

        // Use defaults based on model name
        let model_lower = self.model.to_lowercase();

        // OpenAI models
        if model_lower.contains("gpt-4o") {
            Some(128_000)
        } else if model_lower.contains("gpt-4") {
            Some(8_192)
        } else if model_lower.contains("gpt-3.5") {
            Some(16_385)
        }
        // Anthropic Claude models
        else if model_lower.contains("claude-3-5-sonnet")
            || model_lower.contains("claude-3-opus")
            || model_lower.contains("claude-3-haiku")
        {
            Some(200_000)
        } else if model_lower.contains("claude-2") {
            Some(100_000)
        }
        // OpenRouter models
        else if model_lower.contains("kwaipilot/kat-coder-pro") {
            Some(128_000)
        } else if model_lower.contains("qwen/qwen3-coder") {
            Some(32_768)
        } else if model_lower.contains("deepseek/deepseek-chat-v3.1") {
            Some(64_000)
        }
        // Other common models
        else if model_lower.contains("llama-3") || model_lower.contains("gemma") {
            Some(8_192)
        } else {
            // Unknown model - return None
            None
        }
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
                        context_window_size: project_llm
                            .context_window_size
                            .or(file_llm.context_window_size),
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
                    context_window_size: p.context_window_size.or(llm_defaults.context_window_size),
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
            .unwrap_or_else(|| "cyberpunk".to_string());

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
                if let Some(backup_enabled) = file_watch.backup_enabled {
                    watch_cfg.backup_enabled = Some(backup_enabled);
                }
                if let Some(backup_dir) = &file_watch.backup_dir {
                    watch_cfg.backup_dir = Some(backup_dir.clone());
                }
                if let Some(backup_keep) = file_watch.backup_keep {
                    watch_cfg.backup_keep = Some(backup_keep);
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
                if let Some(backup_enabled) = project_watch.backup_enabled {
                    watch_cfg.backup_enabled = Some(backup_enabled);
                }
                if let Some(backup_dir) = &project_watch.backup_dir {
                    watch_cfg.backup_dir = Some(backup_dir.clone());
                }
                if let Some(backup_keep) = project_watch.backup_keep {
                    watch_cfg.backup_keep = Some(backup_keep);
                }
            }

            watch_cfg
        };

        let rag = {
            let default_rag = RagConfig::default();
            let mut rag_cfg = default_rag.clone();

            if let Some(file_rag) = &file_cfg.rag {
                if let Some(batch_size) = file_rag.batch_size {
                    rag_cfg.batch_size = batch_size;
                }
                if let Some(enabled) = file_rag.enabled {
                    rag_cfg.enabled = enabled;
                }
                if let Some(auto_update) = file_rag.auto_update {
                    rag_cfg.auto_update = auto_update;
                }
            }

            if let Some(project_rag) = &project_cfg.rag {
                if let Some(batch_size) = project_rag.batch_size {
                    rag_cfg.batch_size = batch_size;
                }
                if let Some(enabled) = project_rag.enabled {
                    rag_cfg.enabled = enabled;
                }
                if let Some(auto_update) = project_rag.auto_update {
                    rag_cfg.auto_update = auto_update;
                }
            }
            rag_cfg
        };

        let verification = {
            let default_verification = VerificationConfig::default();
            let mut verification_cfg = default_verification.clone();

            if let Some(file_verification) = &file_cfg.verification {
                if let Some(enabled) = file_verification.enabled {
                    verification_cfg.enabled = enabled;
                }
                if let Some(timeout_ms) = file_verification.timeout_ms {
                    verification_cfg.timeout_ms = timeout_ms;
                }
                if let Some(commands) = &file_verification.commands {
                    if let Some(rust) = &commands.rust {
                        verification_cfg.commands.rust = rust.clone();
                    }
                    if let Some(python) = &commands.python {
                        verification_cfg.commands.python = python.clone();
                    }
                    if let Some(node) = &commands.node {
                        verification_cfg.commands.node = node.clone();
                    }
                    if let Some(typescript) = &commands.typescript {
                        verification_cfg.commands.typescript = typescript.clone();
                    }
                    if let Some(go) = &commands.go {
                        verification_cfg.commands.go = go.clone();
                    }
                }
            }

            if let Some(project_verification) = &project_cfg.verification {
                if let Some(enabled) = project_verification.enabled {
                    verification_cfg.enabled = enabled;
                }
                if let Some(timeout_ms) = project_verification.timeout_ms {
                    verification_cfg.timeout_ms = timeout_ms;
                }
                if let Some(commands) = &project_verification.commands {
                    if let Some(rust) = &commands.rust {
                        verification_cfg.commands.rust = rust.clone();
                    }
                    if let Some(python) = &commands.python {
                        verification_cfg.commands.python = python.clone();
                    }
                    if let Some(node) = &commands.node {
                        verification_cfg.commands.node = node.clone();
                    }
                    if let Some(typescript) = &commands.typescript {
                        verification_cfg.commands.typescript = typescript.clone();
                    }
                    if let Some(go) = &commands.go {
                        verification_cfg.commands.go = go.clone();
                    }
                }
            }

            verification_cfg
        };

        let command_timeout_ms = project_cfg
            .command_timeout_ms
            .or(file_cfg.command_timeout_ms)
            .unwrap_or(DEFAULT_COMMAND_TIMEOUT_MS);

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
            command_timeout_ms,
            mcp_servers,
            rewrite_timeout_sec: project_cfg
                .rewrite_timeout_sec
                .or(file_cfg.rewrite_timeout_sec)
                .unwrap_or(30),
            verification,
            rag,
        })
    }
}

fn get_default_config_content() -> String {
    r#"# Doge-Code Configuration
# This file contains the default configuration for doge-code

# OpenAI-compatible API settings
# base_url = "https://api.openai.com/v1"
# model = "gpt-4o-mini"
# api_key = "your-api-key-here"  # Consider using environment variable OPENAI_API_KEY instead

# LLM settings
[llm]
connect_timeout_ms = 5000
request_timeout_ms = 60000
read_idle_timeout_ms = 20000
max_retries = 100
retry_base_ms = 1000
retry_jitter_ms = 5000
respect_retry_after = true
timeout_ms = 600000  # 10 minutes
# context_window_size = 128000  # Optional context window size for the model

# RAG settings
[rag]
batch_size = 8
# enabled = true
# auto_update = true

# Verification settings
[verification]
enabled = true
timeout_ms = 120000

[verification.commands]
rust = ["cargo", "check", "--quiet", "--message-format=short"]
python = ["python3", "-m", "py_compile", "{path}"]
node = ["node", "--check", "{path}"]
typescript = ["tsc", "--noEmit", "--allowSyntheticDefaultImports", "--target", "esnext", "--moduleResolution", "node", "{path}"]
go = ["go", "vet", "{path}"]

# Watch mode settings
[watch]
# include_patterns = ["**/*.rs", "**/*.js", "**/*.ts", "**/*.jsx", "**/*.tsx", "**/*.py", "**/*.go", "**/*.java", "**/*.md", "**/*.txt", "**/*.yaml", "**/*.yml", "**/*.toml", "**/*.json", "**/*.html", "**/*.css", "**/*.xml"]
# exclude_patterns = ["**/node_modules/**", "**/target/**", "**/build/**", "**/dist/**", "**/.git/**", "**/vendor/**"]
# debounce_delay_ms = 500
# rate_limit_duration_ms = 2000
# ai_comment_pattern = "// AI!:"
# backup_enabled = true
# backup_dir = ".doge/backup"
# backup_keep = 5

# UI settings
theme = "dark"  # "dark" or "light"

# Other settings
enable_stream_tools = false
project_instructions_file = null
no_repomap = false
show_diff = false
auto_compact_prompt_token_threshold = 250000
# auto_compact_prompt_token_thresholds can be defined as a map of model names to thresholds
resume = false
rewrite_timeout_sec = 30
command_timeout_ms = 300000

# Allowed commands for execute_bash tool
# allowed_commands = ["git", "ls", "cat", "grep", "find"]

# Allowed paths for file access
# allowed_paths = ["/tmp", "/home/user/project"]

# MCP server configurations
[[mcp_servers]]
name = "default"
enabled = false
address = "127.0.0.1:8000"
transport = "http"
"#.to_string()
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

    let candidate_paths = candidate_paths();

    // First, check if any config file already exists
    for p in &candidate_paths {
        if p.exists() {
            let s = fs::read_to_string(p)
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

    // If no config file exists, create one in the most appropriate location
    if let Some(first_path) = candidate_paths.first() {
        // Ensure parent directory exists
        if let Some(parent) = first_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create config directory: {}", parent.display())
            })?;
        }

        // Write default config content to the first candidate path
        fs::write(first_path, get_default_config_content()).with_context(|| {
            format!(
                "failed to write default config file: {}",
                first_path.display()
            )
        })?;

        info!(path=%first_path.display(), "created default config file");

        // Now load the newly created config file
        let s = fs::read_to_string(first_path)
            .with_context(|| format!("read newly created config file: {}", first_path.display()))?;
        match toml::from_str::<FileConfig>(&s) {
            Ok(cfg) => {
                info!(path=%first_path.display(), "loaded newly created config file");
                return Ok(cfg);
            }
            Err(e) => {
                warn!(path=%first_path.display(), error=%e.to_string(), "parse newly created config failed, using defaults");
                return Ok(FileConfig::default());
            }
        }
    }

    // Fallback to default config if no candidate paths are available
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
mod tests {
    use super::*;
    use std::env;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_load_file_config_creates_default() {
        let temp_dir = TempDir::new().unwrap();
        let original_home = env::var("HOME").ok();
        let original_xdg_config_home = env::var("XDG_CONFIG_HOME").ok();

        // Set up test environment
        unsafe {
            std::env::set_var("HOME", temp_dir.path());
            let config_home = temp_dir.path().join(".config");
            std::env::set_var("XDG_CONFIG_HOME", &config_home);
        }

        // Remove any existing config file to test creation
        let config_path = temp_dir
            .path()
            .join(".config")
            .join("doge-code")
            .join("config.toml");

        // Load config (this should create the file)
        let result = load_file_config();
        assert!(result.is_ok());

        // Check that the config file was created
        assert!(
            config_path.exists(),
            "Config file should be created at {:?}",
            config_path
        );

        // Check that the file contains content
        let content = fs::read_to_string(&config_path).unwrap();
        assert!(!content.is_empty(), "Config file should not be empty");
        assert!(
            content.contains("# Doge-Code Configuration"),
            "Config should contain comment header"
        );

        // Restore original environment
        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
            if let Some(xdg_home) = original_xdg_config_home {
                std::env::set_var("XDG_CONFIG_HOME", xdg_home);
            } else {
                std::env::remove_var("XDG_CONFIG_HOME");
            }
        }
    }
}
