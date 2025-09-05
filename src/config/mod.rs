use crate::utils::get_git_repository_root;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
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
    pub enable_stream_tools: bool,
    pub theme: String,                             // newly added
    pub project_instructions_file: Option<String>, // newly added
    pub no_repomap: bool,                          // newly added
    pub resume: bool,                              // newly added
    // Auto-compact threshold (configurable via env or config file)
    pub auto_compact_prompt_token_threshold: u32,
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
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            connect_timeout_ms: 5_000,
            request_timeout_ms: 60_000,
            read_idle_timeout_ms: 20_000,
            max_retries: 3,
            retry_base_ms: 300,
            retry_jitter_ms: 200,
            respect_retry_after: true,
        }
    }
}

// Default threshold for auto-compacting conversation history
pub const DEFAULT_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD: u32 = 250_000;

// Threshold constant removed; use AppConfig.auto_compact_prompt_token_threshold at runtime

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileConfig {
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub project_root: Option<std::path::PathBuf>,
    pub llm: Option<PartialLlmConfig>,
    pub enable_stream_tools: Option<bool>,
    pub theme: Option<String>,                     // newly added
    pub project_instructions_file: Option<String>, // newly added
    pub no_repomap: Option<bool>,                  // newly added
    pub resume: Option<bool>,                      // newly added
    // Auto-compact threshold (optional in config file)
    pub auto_compact_prompt_token_threshold: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PartialLlmConfig {
    pub connect_timeout_ms: Option<u64>,
    pub request_timeout_ms: Option<u64>,
    pub read_idle_timeout_ms: Option<u64>,
    pub max_retries: Option<usize>,
    pub retry_base_ms: Option<u64>,
    pub retry_jitter_ms: Option<u64>,
    pub respect_retry_after: Option<bool>,
}

impl AppConfig {
    pub fn from_cli(cli: crate::Cli) -> Result<Self> {
        let project_root = std::env::current_dir().context("resolve current dir")?;
        let git_root = get_git_repository_root(&project_root);

        let file_cfg = load_file_config().unwrap_or_default();
        let api_key = cli
            .api_key
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .or(file_cfg.api_key);
        let base_url = if cli.base_url.is_empty() {
            std::env::var("OPENAI_BASE_URL")
                .ok()
                .or(file_cfg.base_url)
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string())
        } else {
            cli.base_url
        };
        let model = if cli.model.is_empty() {
            std::env::var("OPENAI_MODEL")
                .ok()
                .or(file_cfg.model)
                .unwrap_or_else(|| "gpt-4o-mini".to_string())
        } else {
            cli.model
        };
        let project_root = file_cfg.project_root.unwrap_or(project_root);

        let llm_defaults = LlmConfig::default();
        let llm = if let Some(p) = file_cfg.llm {
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
            }
        } else {
            llm_defaults
        };

        // Add theme setting
        let theme = file_cfg.theme.unwrap_or_else(|| "dark".to_string());
        // Add project_instructions_file setting
        let project_instructions_file =
            cli.instructions_file.or(file_cfg.project_instructions_file);

        // Determine auto-compact threshold (priority: env var -> config file -> default)
        let auto_compact_prompt_token_threshold =
            std::env::var("DOGE_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD")
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .or(file_cfg.auto_compact_prompt_token_threshold)
                .unwrap_or(DEFAULT_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD);

        Ok(Self {
            base_url,
            model,
            api_key,
            project_root,
            git_root,
            llm,
            enable_stream_tools: std::env::var("DOGE_STREAM_TOOLS")
                .ok()
                .and_then(|v| v.parse().ok())
                .or(file_cfg.enable_stream_tools)
                .unwrap_or(false),
            theme,                     // newly added
            project_instructions_file, // newly added
            no_repomap: cli.no_repomap || file_cfg.no_repomap.unwrap_or(false),
            resume: cli.resume,
            auto_compact_prompt_token_threshold,
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
