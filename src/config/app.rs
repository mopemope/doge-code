use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;

use super::llm::LlmConfig;
use super::mcp::McpServerConfig;
use super::test_fix::TestFixConfig;
use super::verification::VerificationConfig;
use super::watch::WatchConfig;
use crate::utils::get_git_repository_root;
// Re-import from mod or loading
use super::loading::{load_file_config, load_project_config};

// Default threshold for auto-compacting conversation history
pub const DEFAULT_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD: u32 = 250_000;
pub const DEFAULT_COMMAND_TIMEOUT_MS: u64 = 300_000;

#[derive(thiserror::Error, Debug)]
pub enum AppConfigError {
    #[error("Failed to load config: {0}")]
    Load(#[from] anyhow::Error),
    #[error("Missing configuration: {0}")]
    Missing(String),
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub project_root: PathBuf,
    pub git_root: Option<PathBuf>,
    pub llm: LlmConfig,
    pub watch_config: WatchConfig,
    pub enable_stream_tools: bool,
    pub theme: String,
    pub project_instructions_file: Option<String>,
    pub no_repomap: bool,
    pub resume: bool,
    pub auto_compact_prompt_token_threshold: u32,
    pub auto_compact_prompt_token_threshold_overrides: HashMap<String, u32>,
    pub show_diff: bool,
    pub allowed_commands: Vec<String>,
    pub allowed_paths: Vec<PathBuf>,
    pub command_timeout_ms: u64,
    pub mcp_servers: Vec<McpServerConfig>,
    pub rewrite_timeout_sec: u64,
    pub verification: VerificationConfig,
    pub test_fix: TestFixConfig,
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
            watch_config: WatchConfig::default(),
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
            command_timeout_ms: DEFAULT_COMMAND_TIMEOUT_MS,
            mcp_servers: vec![McpServerConfig::default()],
            rewrite_timeout_sec: 30,
            verification: VerificationConfig::default(),
            test_fix: TestFixConfig::default(),
        }
    }
}

impl AppConfig {
    pub fn auto_compact_prompt_token_threshold_for_model(&self, model: &str) -> u32 {
        self.auto_compact_prompt_token_threshold_overrides
            .get(model)
            .copied()
            .unwrap_or(self.auto_compact_prompt_token_threshold)
    }

    pub fn get_context_window_size(&self) -> Option<u32> {
        if let Some(size) = self.llm.context_window_size {
            return Some(size);
        }

        let model_lower = self.model.to_lowercase();
        if model_lower.contains("gpt-4o") {
            Some(128_000)
        } else if model_lower.contains("gpt-4") {
            Some(8_192)
        } else if model_lower.contains("gpt-3.5") {
            Some(16_385)
        } else if model_lower.contains("claude-3-5-sonnet")
            || model_lower.contains("claude-3-opus")
            || model_lower.contains("claude-3-haiku")
        {
            Some(200_000)
        } else if model_lower.contains("claude-2") {
            Some(100_000)
        } else if model_lower.contains("kwaipilot/kat-coder-pro") {
            Some(128_000)
        } else if model_lower.contains("qwen/qwen3-coder") {
            Some(32_768)
        } else if model_lower.contains("deepseek/deepseek-chat-v3.1") {
            Some(64_000)
        } else if model_lower.contains("llama-3") || model_lower.contains("gemma") {
            Some(8_192)
        } else {
            None
        }
    }

    pub fn auto_compact_prompt_token_threshold_for_current_model(&self) -> u32 {
        self.auto_compact_prompt_token_threshold_for_model(&self.model)
    }

    pub fn get_effective_compaction_limit(&self) -> u32 {
        let context_window_size = self.get_context_window_size().unwrap_or(128_000);
        let safety_limit = (context_window_size as f64 * 0.8) as u32;
        let auto_compact_threshold = self.auto_compact_prompt_token_threshold_for_current_model();
        std::cmp::min(auto_compact_threshold, safety_limit)
    }

    pub fn from_cli(cli: crate::Cli) -> Result<Self> {
        let project_root = std::env::current_dir().context("resolve current dir")?;
        let git_root = get_git_repository_root(&project_root);

        let project_cfg = load_project_config(&project_root).unwrap_or_default();
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

        let mut llm = LlmConfig::default();
        if let Some(f) = &file_cfg.llm {
            llm.apply_partial(f);
        }
        if let Some(p) = &project_cfg.llm {
            llm.apply_partial(p);
        }

        let mcp_servers = merge_mcp_servers(
            file_cfg.mcp_servers.as_ref(),
            project_cfg.mcp_servers.as_ref(),
        );

        let theme = project_cfg
            .theme
            .or(file_cfg.theme)
            .unwrap_or_else(|| "dark".to_string());
        let project_instructions_file = cli
            .instructions_file
            .or(project_cfg.project_instructions_file)
            .or(file_cfg.project_instructions_file);
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

        let mut watch_config = WatchConfig::default();
        if let Some(f) = &file_cfg.watch {
            watch_config.apply_partial(f);
        }
        if let Some(p) = &project_cfg.watch {
            watch_config.apply_partial(p);
        }

        let mut verification = VerificationConfig::default();
        if let Some(f) = &file_cfg.verification {
            verification.apply_partial(f);
        }
        if let Some(p) = &project_cfg.verification {
            verification.apply_partial(p);
        }

        let mut test_fix = TestFixConfig::default();
        if let Some(f) = &file_cfg.test_fix {
            test_fix.apply_partial(f);
        }
        if let Some(p) = &project_cfg.test_fix {
            test_fix.apply_partial(p);
        }

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
            watch_config,
            enable_stream_tools: std::env::var("DOGE_STREAM_TOOLS")
                .ok()
                .and_then(|v| v.parse().ok())
                .or(project_cfg.enable_stream_tools)
                .or(file_cfg.enable_stream_tools)
                .unwrap_or(false),
            theme,
            project_instructions_file,
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
            test_fix,
        })
    }
}

pub fn merge_mcp_servers(
    file_servers: Option<&Vec<super::mcp::PartialMcpServerConfig>>,
    project_servers: Option<&Vec<super::mcp::PartialMcpServerConfig>>,
) -> Vec<McpServerConfig> {
    let mut merged_mcp_servers = Vec::new();
    if let Some(file_mcp_servers) = file_servers {
        for server in file_mcp_servers {
            merged_mcp_servers.push(server.clone());
        }
    }
    if let Some(project_mcp_servers) = project_servers {
        for project_server in project_mcp_servers {
            if let Some(name) = &project_server.name {
                if let Some(existing_server) = merged_mcp_servers
                    .iter_mut()
                    .find(|s| s.name.as_ref() == Some(name))
                {
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
                    merged_mcp_servers.push(project_server.clone());
                }
            } else {
                merged_mcp_servers.push(project_server.clone());
            }
        }
    }

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
}
