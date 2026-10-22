use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;

use super::agent_budget::{AgentBudgetConfig, merge_agent_budget};
use super::context_budget::{ContextBudgetConfig, merge_context_budget};
use super::execution::{ExecutionConfig, merge_execution};
use super::llm::LlmConfig;
use super::mcp::{
    LocalMcpServerConfig, McpServerConfig, McpTransport, PartialLocalMcpServerConfig,
    validate_mcp_server_names,
};
use super::reasoning::{ReasoningConfig, merge_reasoning};
use super::subagent::{SubagentConfig, merge_subagent};
use super::tool_routing::{ToolRoutingConfig, merge_tool_routing};
use super::watch::WatchConfig;
use crate::utils::get_git_repository_root;
// Re-import from mod or loading
use super::loading::{load_file_config, load_project_config};

// Default threshold for auto-compacting conversation history
pub const DEFAULT_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD: u32 = 250_000;
pub const FALLBACK_CONTEXT_WINDOW: u32 = 128_000;
pub const DEFAULT_COMMAND_TIMEOUT_MS: u64 = 300_000;

#[derive(thiserror::Error, Debug)]
pub enum AppConfigError {
    #[error("Failed to load config: {0}")]
    Load(#[from] anyhow::Error),
    #[error("Missing configuration: {0}")]
    Missing(String),
}

#[derive(Clone)]
pub struct AppConfig {
    pub provider: crate::features::openai_subscription::ProviderKind,
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub openrouter_metadata:
        Option<std::sync::Arc<crate::features::openrouter::SelectedModelMetadata>>,
    /// Resolution provenance, retained independently of the credential value.
    pub api_key_origin: Option<super::ConfigOrigin>,
    pub base_url_origin: super::ConfigOrigin,
    pub provider_origin: super::ConfigOrigin,
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
    /// Tool routing policy (`[tool_routing]`). Token-efficiency layer only;
    /// never a substitute for execution/filesystem policy.
    pub tool_routing: ToolRoutingConfig,
    /// Adaptive reasoning policy (`[reasoning]`). Controls per-iteration
    /// `reasoning_effort` without changing tool routing or loop detection.
    pub reasoning: ReasoningConfig,
    /// Preflight context governor (`[context_budget]`). Measures the current
    /// request footprint before sending; never evicts tools or observations.
    pub context_budget: ContextBudgetConfig,
    pub subagent: SubagentConfig,
    /// Run-wide main-agent resource budget (`[agent_budget]`).
    pub agent_budget: AgentBudgetConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            provider: Default::default(),
            base_url: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            api_key: None,
            openrouter_metadata: None,
            api_key_origin: None,
            base_url_origin: super::ConfigOrigin::Default,
            provider_origin: super::ConfigOrigin::Default,
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
            tool_routing: ToolRoutingConfig::default(),
            reasoning: ReasoningConfig::default(),
            context_budget: ContextBudgetConfig::default(),
            subagent: SubagentConfig::default(),
            agent_budget: AgentBudgetConfig::default(),
        }
    }
}

impl AppConfig {
    /// Resolve the wire API once from the selected provider, its provenance,
    /// endpoint and model catalog. Provider selection remains independent of
    /// authentication: an explicitly selected compatible provider preserves
    /// Chat Completions even when it points at api.openai.com.
    pub(crate) fn resolved_api_for_model(
        &self,
        model: &str,
    ) -> Result<crate::llm::capabilities::ApiKind> {
        use crate::{
            features::{openai_subscription::ProviderKind, opencode},
            llm::capabilities::ApiKind,
        };

        match self.provider {
            ProviderKind::OpenaiCompatible => {
                if self.provider_origin == super::ConfigOrigin::Default
                    && normalized_official_openai_base(&self.base_url).is_some()
                {
                    Ok(ApiKind::Responses)
                } else {
                    Ok(ApiKind::ChatCompletions)
                }
            }
            ProviderKind::Openai => Ok(ApiKind::Responses),
            ProviderKind::Openrouter => Ok(ApiKind::ChatCompletions),
            provider => opencode::api(provider, model),
        }
    }

    /// Stable, non-secret identity for the API-key Responses protocol. The
    /// endpoint can only be selected by the strict official-host resolver.
    pub(crate) fn api_key_responses_identity(&self) -> Option<String> {
        (self.provider == crate::features::openai_subscription::ProviderKind::OpenaiCompatible
            && self.resolved_api_for_model(&self.model).ok()
                == Some(crate::llm::capabilities::ApiKind::Responses))
        .then(|| normalized_official_openai_base(&self.base_url))
        .flatten()
        .map(|base| format!("OpenaiCompatible:api-key:{base}:responses"))
    }

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

        if self.provider == crate::features::openai_subscription::ProviderKind::Openrouter {
            return self
                .openrouter_metadata
                .as_ref()
                .filter(|metadata| {
                    metadata.provider == self.provider && metadata.model.id == self.model
                })
                .and_then(|metadata| metadata.model.context_window)
                .filter(|size| *size > 0);
        }
        let api = self.resolved_api_for_model(&self.model).ok()?;
        let resolve = crate::llm::capabilities::resolve;
        resolve(self.provider, &self.base_url, api, &self.model, false).context_window
    }

    pub fn auto_compact_prompt_token_threshold_for_current_model(&self) -> u32 {
        self.auto_compact_prompt_token_threshold_for_model(&self.model)
    }

    pub fn get_effective_compaction_limit(&self) -> u32 {
        let context_window_size = self
            .get_context_window_size()
            .unwrap_or(FALLBACK_CONTEXT_WINDOW);
        let safety_limit = (context_window_size as f64 * 0.8) as u32;
        let auto_compact_threshold = self.auto_compact_prompt_token_threshold_for_current_model();
        std::cmp::min(auto_compact_threshold, safety_limit)
    }

    pub fn from_cli(cli: crate::Cli) -> Result<Self> {
        let explicit_resume = match &cli.command {
            Some(crate::Commands::Exec { resume, .. }) => {
                resume.clone().or_else(|| cli.resume.clone())
            }
            _ => cli.resume.clone(),
        };
        let project_root = std::env::current_dir().context("resolve current dir")?;
        let git_root = get_git_repository_root(&project_root);

        let project_cfg =
            load_project_config(&project_root).context("load project configuration")?;
        let file_cfg = load_file_config().context("load user configuration")?;

        super::trust::check_project_policy(&file_cfg, &project_cfg, &project_root)?;
        use super::ConfigOrigin;
        let provider_environment = std::env::var("DGC_PROVIDER").ok();
        let provider_origin = if cli.provider.is_some() {
            ConfigOrigin::Cli
        } else if provider_environment.is_some() {
            ConfigOrigin::Environment
        } else if project_cfg.provider.is_some() {
            ConfigOrigin::Project
        } else if file_cfg.provider.is_some() {
            ConfigOrigin::Global
        } else {
            ConfigOrigin::Default
        };
        let provider = resolve_provider(
            cli.provider,
            provider_environment.as_deref(),
            project_cfg.provider,
            file_cfg.provider,
        )?;
        if provider == crate::features::openai_subscription::ProviderKind::Openai
            && cli.api_key.is_some()
        {
            anyhow::bail!("--api-key cannot be combined with --provider openai");
        }
        let explicit_model = !cli.model.is_empty()
            || std::env::var("OPENAI_MODEL").is_ok_and(|model| !model.trim().is_empty())
            || project_cfg.model.is_some()
            || file_cfg.model.is_some();
        if provider == crate::features::openai_subscription::ProviderKind::Openai && !explicit_model
        {
            anyhow::bail!("Select a ChatGPT model with --model. Run dgc models --provider openai.");
        }
        let opencode_base = crate::features::opencode::default_base(provider);
        // Blank values are unset everywhere (empty exported variables, `--api-key ""`,
        // `api_key = ""`), so they never shadow a lower-precedence source.
        let sourced = |value: Option<String>, origin| {
            value
                .filter(|value| !value.trim().is_empty())
                .map(|value| (value, origin))
        };
        let global_base = crate::features::model_selection::config_value(
            provider,
            file_cfg.provider,
            file_cfg.base_url.clone(),
        );
        let trusted_base = global_base.clone().unwrap_or_else(|| {
            opencode_base
                .unwrap_or("https://api.openai.com/v1")
                .to_owned()
        });
        let is_openrouter =
            provider == crate::features::openai_subscription::ProviderKind::Openrouter;
        let (base_url, base_url_origin) = if is_openrouter {
            openrouter_value(
                Some(cli.base_url.clone()),
                None,
                project_cfg.provider,
                project_cfg.base_url.clone(),
                file_cfg.provider,
                file_cfg.base_url.clone(),
            )
            .unwrap_or_else(|| {
                (
                    crate::features::openrouter::BASE_URL.to_owned(),
                    ConfigOrigin::Default,
                )
            })
        } else if !cli.base_url.is_empty() {
            (cli.base_url, ConfigOrigin::Cli)
        } else {
            sourced(
                std::env::var(if opencode_base.is_some() {
                    "OPENCODE_BASE_URL"
                } else {
                    "OPENAI_BASE_URL"
                })
                .ok(),
                ConfigOrigin::Environment,
            )
            .or_else(|| {
                sourced(
                    crate::features::model_selection::config_value(
                        provider,
                        project_cfg.provider,
                        project_cfg.base_url.clone(),
                    ),
                    ConfigOrigin::Project,
                )
            })
            .or_else(|| sourced(global_base, ConfigOrigin::Global))
            .unwrap_or_else(|| (trusted_base.clone(), ConfigOrigin::Default))
        };
        // OAuth never uses this URL, and Go/Zen validate their pinned URL below.
        // Compatible routes must not pair inherited credentials with a new
        // project-selected destination. Reject even without a key, so keyless
        // startup cannot later acquire a credential for the same destination.
        if provider == crate::features::openai_subscription::ProviderKind::OpenaiCompatible
            && base_url_origin == ConfigOrigin::Project
            && base_url.trim_end_matches('/') != trusted_base.trim_end_matches('/')
        {
            return Err(super::trust::project_denied(
                "base_url (inherited credentials are not authorized for a project-selected endpoint)",
            ));
        }
        let key = if provider == crate::features::openai_subscription::ProviderKind::Openai {
            None
        } else if is_openrouter {
            openrouter_value(
                cli.api_key,
                std::env::var("OPENROUTER_API_KEY").ok(),
                project_cfg.provider,
                project_cfg.api_key.clone(),
                file_cfg.provider,
                file_cfg.api_key.clone(),
            )
        } else {
            sourced(cli.api_key, ConfigOrigin::Cli)
                .or_else(|| {
                    sourced(
                        std::env::var(if opencode_base.is_some() {
                            "OPENCODE_API_KEY"
                        } else {
                            "OPENAI_API_KEY"
                        })
                        .ok(),
                        ConfigOrigin::Environment,
                    )
                })
                .or_else(|| {
                    sourced(
                        opencode_base
                            .is_some()
                            .then(crate::features::opencode::key_store::stored_key)
                            .flatten(),
                        ConfigOrigin::Global,
                    )
                })
                .or_else(|| {
                    sourced(
                        crate::features::model_selection::config_value(
                            provider,
                            project_cfg.provider,
                            project_cfg.api_key.clone(),
                        ),
                        ConfigOrigin::Project,
                    )
                })
                .or_else(|| {
                    sourced(
                        crate::features::model_selection::config_value(
                            provider,
                            file_cfg.provider,
                            file_cfg.api_key.clone(),
                        ),
                        ConfigOrigin::Global,
                    )
                })
        };
        let api_key_origin = key.as_ref().map(|(_, origin)| *origin);
        let api_key = key.map(|(key, _)| key);
        let model = if is_openrouter {
            openrouter_value(
                Some(cli.model),
                std::env::var("OPENROUTER_MODEL").ok(),
                project_cfg.provider,
                project_cfg.model.clone(),
                file_cfg.provider,
                file_cfg.model.clone(),
            )
            .map(|(model, _)| model)
            .unwrap_or_default()
        } else if cli.model.is_empty() {
            std::env::var(if opencode_base.is_some() {
                "OPENCODE_MODEL"
            } else {
                "OPENAI_MODEL"
            })
            .ok()
            .filter(|model| !model.trim().is_empty())
            .or(crate::features::model_selection::config_value(
                provider,
                project_cfg.provider,
                project_cfg.model,
            ))
            .or(crate::features::model_selection::config_value(
                provider,
                file_cfg.provider,
                file_cfg.model,
            ))
            .unwrap_or_else(|| {
                if opencode_base.is_some() {
                    "gpt-6-luna"
                } else {
                    "gpt-4o-mini"
                }
                .to_string()
            })
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

        let tool_routing = merge_tool_routing(
            file_cfg.tool_routing.as_ref(),
            project_cfg.tool_routing.as_ref(),
        );

        let reasoning =
            merge_reasoning(file_cfg.reasoning.as_ref(), project_cfg.reasoning.as_ref());

        let subagent = merge_subagent(file_cfg.subagent.as_ref(), project_cfg.subagent.as_ref())?;
        let agent_budget = merge_agent_budget(
            file_cfg.agent_budget.as_ref(),
            project_cfg.agent_budget.as_ref(),
        )?;
        let context_budget = merge_context_budget(
            file_cfg.context_budget.as_ref(),
            project_cfg.context_budget.as_ref(),
        );

        let base_url = if provider == crate::features::openai_subscription::ProviderKind::Openai {
            if base_url != "https://api.openai.com/v1" {
                eprintln!(
                    "openai uses the official Responses endpoint; the configured base URL is not used."
                );
            }
            "https://api.openai.com/v1".to_owned()
        } else {
            base_url
        };
        let model = if is_openrouter {
            crate::features::openrouter::validate_base(&base_url)?;
            if model.is_empty() && explicit_resume.is_some() {
                model
            } else {
                anyhow::ensure!(
                    !model.is_empty(),
                    "Select an OpenRouter model explicitly with --model publisher/model. Run dgc models --provider openrouter to list models."
                );
                crate::features::openrouter::model_id(&model)?.to_owned()
            }
        } else {
            crate::features::opencode::model_id(provider, &model)?.to_owned()
        };
        let base_url = if is_openrouter {
            crate::features::openrouter::BASE_URL.to_owned()
        } else {
            base_url
        };
        crate::features::opencode::validate_base(provider, &base_url)?;
        let base_url = opencode_base.map(str::to_owned).unwrap_or(base_url);
        if opencode_base.is_some() {
            crate::features::opencode::api(provider, &model)?;
        }
        let config = Self {
            provider,
            base_url,
            openrouter_metadata: if is_openrouter {
                crate::features::openrouter::cached_metadata(&model)
            } else {
                None
            },
            model,
            api_key,
            api_key_origin,
            base_url_origin: if provider
                == crate::features::openai_subscription::ProviderKind::Openai
            {
                ConfigOrigin::Default
            } else {
                base_url_origin
            },
            provider_origin,
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
            resume: explicit_resume.or_else(|| {
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
            tool_routing,
            reasoning,
            context_budget,
            subagent,
            agent_budget,
        };
        if config.provider == crate::features::openai_subscription::ProviderKind::Openai
            && config.get_context_window_size().is_none()
        {
            eprintln!(
                "ChatGPT model context capacity is unknown. Set [llm] context_window_size explicitly; local estimates are approximate."
            );
        }
        Ok(config)
    }
}

/// Return the canonical identity for the one official API-key Responses base.
/// Do not treat a domain substring, proxy, custom path, or URL decoration as
/// authorization to change wire protocol.
pub(crate) fn normalized_official_openai_base(base_url: &str) -> Option<String> {
    let url = reqwest::Url::parse(base_url).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("api.openai.com")
        || url.port().is_some_and(|port| port != 443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "/v1" | "/v1/")
    {
        return None;
    }
    Some("https://api.openai.com/v1".into())
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

impl std::fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppConfig")
            .field("model", &self.model)
            .field("api_key", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

fn resolve_provider(
    cli: Option<crate::features::openai_subscription::ProviderKind>,
    environment: Option<&str>,
    project: Option<crate::features::openai_subscription::ProviderKind>,
    user: Option<crate::features::openai_subscription::ProviderKind>,
) -> Result<crate::features::openai_subscription::ProviderKind> {
    use crate::features::openai_subscription::ProviderKind;
    if let Some(provider) = cli {
        return Ok(provider);
    }
    if let Some(environment) = environment {
        return match environment {
            "openai-compatible" => Ok(ProviderKind::OpenaiCompatible),
            "openai" => Ok(ProviderKind::Openai),
            "opencode-go" | "go" => Ok(ProviderKind::OpencodeGo),
            "opencode-zen" | "zen" => Ok(ProviderKind::OpencodeZen),
            _ => anyhow::bail!("invalid DGC_PROVIDER"),
        };
    }
    Ok(project.or(user).unwrap_or_default())
}

/// OpenRouter never inherits values from another provider or an unscoped source.
pub(super) fn openrouter_value(
    cli: Option<String>,
    environment: Option<String>,
    project_provider: Option<crate::features::openai_subscription::ProviderKind>,
    project: Option<String>,
    global_provider: Option<crate::features::openai_subscription::ProviderKind>,
    global: Option<String>,
) -> Option<(String, super::ConfigOrigin)> {
    use super::ConfigOrigin;
    use crate::features::{model_selection, openai_subscription::ProviderKind};
    [
        (cli, ConfigOrigin::Cli),
        (environment, ConfigOrigin::Environment),
        (
            model_selection::config_value(ProviderKind::Openrouter, project_provider, project),
            ConfigOrigin::Project,
        ),
        (
            model_selection::config_value(ProviderKind::Openrouter, global_provider, global),
            ConfigOrigin::Global,
        ),
    ]
    .into_iter()
    .find_map(|(value, origin)| {
        value
            .filter(|value| !value.trim().is_empty())
            .map(|value| (value, origin))
    })
}

#[cfg(test)]
mod provider_tests {
    use super::*;
    use crate::features::openai_subscription::ProviderKind::{
        Openai, OpenaiCompatible as Compatible,
    };
    #[test]
    fn explicit_provider_precedence_and_invalid_environment() {
        assert_eq!(
            resolve_provider(
                Some(Openai),
                Some("invalid"),
                Some(Compatible),
                Some(Compatible)
            )
            .expect("CLI wins"),
            Openai
        );
        assert!(resolve_provider(None, Some("invalid"), None, None).is_err());
        assert_eq!(
            resolve_provider(None, Some("openai"), None, None).expect("environment openai"),
            Openai
        );
        assert_eq!(
            resolve_provider(None, Some("openai-compatible"), Some(Openai), Some(Openai))
                .expect("environment"),
            Compatible
        );
        assert_eq!(
            resolve_provider(None, None, Some(Openai), Some(Compatible)).expect("project"),
            Openai
        );
        assert_eq!(
            resolve_provider(None, None, None, Some(Openai)).expect("user"),
            Openai
        );
        assert_eq!(
            resolve_provider(None, None, None, None).expect("legacy"),
            Compatible
        );
    }
}
