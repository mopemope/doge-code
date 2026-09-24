use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;

use super::execution::{ExecutionConfig, merge_execution};
use super::llm::LlmConfig;
use super::mcp::{
    LocalMcpServerConfig, McpServerConfig, McpTransport, PartialLocalMcpServerConfig,
    validate_mcp_server_names,
};
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
    /// Session resume target: `None` = start fresh, `Some("latest")` = resume
    /// the most recently updated session, `Some(id)` = resume a specific session.
    pub resume: Option<String>,
    pub auto_compact_prompt_token_threshold: u32,
    pub auto_compact_prompt_token_threshold_overrides: HashMap<String, u32>,
    pub show_diff: bool,
    pub allowed_commands: Vec<String>,
    pub allowed_paths: Vec<PathBuf>,
    /// Maximum duration for managed finite commands; `0` means unlimited.
    pub command_timeout_ms: u64,
    pub execution: ExecutionConfig,
    /// True when an explicit `[execution]` section was present in either the
    /// global/user config or the project config. Used to decide whether legacy
    /// `allowed_commands` acts as a fallback.
    pub execution_configured: bool,
    pub mcp_servers: Vec<McpServerConfig>,
    /// Local MCP HTTP listener config (`[mcp_server]`).
    pub local_mcp_server: LocalMcpServerConfig,
    pub rewrite_timeout_sec: u64,
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
            resume: None,
            auto_compact_prompt_token_threshold: DEFAULT_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD,
            auto_compact_prompt_token_threshold_overrides: HashMap::new(),
            show_diff: true,
            allowed_commands: vec![],
            allowed_paths: vec![],
            command_timeout_ms: DEFAULT_COMMAND_TIMEOUT_MS,
            execution: ExecutionConfig::default(),
            execution_configured: false,
            mcp_servers: vec![McpServerConfig::default()],
            local_mcp_server: LocalMcpServerConfig::default(),
            rewrite_timeout_sec: 30,
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
        // Enabled remote endpoints are validated at the configuration
        // boundary as well as by `McpClient`. Disabled entries remain
        // harmless and can be completed later without being contacted.
        for server in &mcp_servers {
            if server.enabled {
                server.validate().map_err(|error| {
                    anyhow::anyhow!("invalid MCP server '{}': {error}", server.name)
                })?;
            }
        }
        validate_mcp_server_names(&mcp_servers)?;

        let local_mcp_server = merge_local_mcp_server(
            file_cfg.mcp_server.as_ref(),
            project_cfg.mcp_server.as_ref(),
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

        let command_timeout_ms = project_cfg
            .command_timeout_ms
            .or(file_cfg.command_timeout_ms)
            .unwrap_or(DEFAULT_COMMAND_TIMEOUT_MS);

        let merged_execution_partial =
            merge_execution(file_cfg.execution.as_ref(), project_cfg.execution.as_ref());
        let execution_configured = merged_execution_partial.is_some();
        let mut execution = ExecutionConfig::default();
        if let Some(ref partial) = merged_execution_partial {
            execution.apply_partial(partial);
        }

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
            // CLI flag wins; otherwise honor `resume = true` in the config
            // files (previously this file-based setting was ignored).
            resume: cli.resume.or_else(|| {
                project_cfg
                    .resume
                    .or(file_cfg.resume)
                    .filter(|resume| *resume)
                    .map(|_| "latest".to_string())
            }),
            auto_compact_prompt_token_threshold,
            auto_compact_prompt_token_threshold_overrides,
            show_diff: project_cfg.show_diff.or(file_cfg.show_diff).unwrap_or(true),
            allowed_commands: project_cfg
                .allowed_commands
                .or(file_cfg.allowed_commands)
                .unwrap_or_default(),
            allowed_paths: project_cfg
                .allowed_paths
                .or(file_cfg.allowed_paths)
                .unwrap_or_default(),
            command_timeout_ms,
            execution,
            execution_configured,
            mcp_servers,
            local_mcp_server,
            rewrite_timeout_sec: project_cfg
                .rewrite_timeout_sec
                .or(file_cfg.rewrite_timeout_sec)
                .unwrap_or(30),
        })
    }
}

pub fn merge_mcp_servers(
    file_servers: Option<&Vec<super::mcp::PartialMcpServerConfig>>,
    project_servers: Option<&Vec<super::mcp::PartialMcpServerConfig>>,
) -> Vec<McpServerConfig> {
    let mut merged: Vec<super::mcp::PartialMcpServerConfig> = Vec::new();

    if let Some(file_mcp_servers) = file_servers {
        merged.extend(file_mcp_servers.iter().cloned());
    }

    if let Some(project_mcp_servers) = project_servers {
        for project_server in project_mcp_servers {
            if let Some(name) = &project_server.name
                && let Some(existing) = merged
                    .iter_mut()
                    .find(|server| server.name.as_deref() == Some(name.as_str()))
            {
                // Scalar fields are field-wise merged. Environment variables
                // are merged per key so a project can override one secret
                // without discarding unrelated global variables.
                if project_server.enabled.is_some() {
                    existing.enabled = project_server.enabled;
                }
                if project_server.address.is_some() {
                    existing.address = project_server.address.clone();
                }
                if project_server.transport.is_some() {
                    existing.transport = project_server.transport;
                }
                if project_server.command.is_some() {
                    existing.command = project_server.command.clone();
                    // A structured command cannot inherit an HTTP/legacy
                    // address from the global entry unless the project
                    // explicitly supplied one (which remains an error).
                    if project_server.address.is_none() {
                        existing.address = None;
                    }
                }
                if project_server.args.is_some() {
                    existing.args = project_server.args.clone();
                }
                if let Some(project_env) = &project_server.env {
                    let mut merged_env = existing.env.clone().unwrap_or_default();
                    merged_env.extend(
                        project_env
                            .iter()
                            .map(|(key, value)| (key.clone(), value.clone())),
                    );
                    existing.env = Some(merged_env);
                }
                match project_server.transport {
                    Some(McpTransport::Http) if project_server.command.is_none() => {
                        // Switching to HTTP must not retain stdio-only
                        // fields from the global entry. Explicit project
                        // args/env are preserved so validation can report
                        // the conflict instead of silently accepting it.
                        existing.command = None;
                        if project_server.args.is_none() {
                            existing.args = Some(Vec::new());
                        }
                        if project_server.env.is_none() {
                            existing.env = None;
                        }
                    }
                    Some(McpTransport::Stdio) if project_server.command.is_none() => {
                        // A stdio transport without a structured command uses
                        // the legacy address fallback; do not carry a global
                        // structured command/argv/env into that mode.
                        existing.command = None;
                        if project_server.args.is_none() {
                            existing.args = Some(Vec::new());
                        }
                        if project_server.env.is_none() {
                            existing.env = None;
                        }
                    }
                    None if project_server.command.is_some() => {
                        // `command` is an unambiguous stdio signal when a
                        // project omits the otherwise-default transport.
                        existing.transport = Some(McpTransport::Stdio);
                    }
                    _ => {}
                }
                if project_server.connect_timeout_ms.is_some() {
                    existing.connect_timeout_ms = project_server.connect_timeout_ms;
                }
                if project_server.list_timeout_ms.is_some() {
                    existing.list_timeout_ms = project_server.list_timeout_ms;
                }
                if project_server.call_timeout_ms.is_some() {
                    existing.call_timeout_ms = project_server.call_timeout_ms;
                }
            } else {
                merged.push(project_server.clone());
            }
        }
    }

    let defaults = McpServerConfig::default();
    merged
        .into_iter()
        .map(|partial| {
            let transport = partial.transport.unwrap_or_else(|| {
                if partial.command.is_some() {
                    McpTransport::Stdio
                } else {
                    defaults.transport
                }
            });
            // Never manufacture an address for a structured/stdio entry. An
            // explicit stdio address remains available for the deprecated
            // fallback, while HTTP entries must provide their endpoint.
            let address =
                if partial.command.is_some() || transport == super::mcp::McpTransport::Stdio {
                    partial.address
                } else {
                    partial.address.or_else(|| defaults.address.clone())
                };
            McpServerConfig {
                name: partial.name.unwrap_or_else(|| defaults.name.clone()),
                enabled: partial.enabled.unwrap_or(defaults.enabled),
                address,
                transport,
                command: partial.command,
                args: partial.args.unwrap_or_default(),
                env: partial.env.unwrap_or_default(),
                connect_timeout_ms: partial
                    .connect_timeout_ms
                    .unwrap_or(defaults.connect_timeout_ms),
                list_timeout_ms: partial.list_timeout_ms.unwrap_or(defaults.list_timeout_ms),
                call_timeout_ms: partial.call_timeout_ms.unwrap_or(defaults.call_timeout_ms),
            }
        })
        .collect()
}

/// Merge `[mcp_server]` local-listener config with precedence:
/// default <- global config <- project config (field-wise).
pub fn merge_local_mcp_server(
    file_cfg: Option<&PartialLocalMcpServerConfig>,
    project_cfg: Option<&PartialLocalMcpServerConfig>,
) -> LocalMcpServerConfig {
    let defaults = LocalMcpServerConfig::default();
    LocalMcpServerConfig {
        enabled: project_cfg
            .and_then(|p| p.enabled)
            .or_else(|| file_cfg.and_then(|f| f.enabled))
            .unwrap_or(defaults.enabled),
        address: project_cfg
            .and_then(|p| p.address.clone())
            .or_else(|| file_cfg.and_then(|f| f.address.clone()))
            .unwrap_or(defaults.address),
    }
}
