use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use super::llm::PartialLlmConfig;
use super::mcp::PartialMcpServerConfig;

use super::watch::PartialWatchConfig;

#[derive(Debug, Clone, Default, serde::Deserialize, PartialEq)]
pub struct FileConfig {
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub project_root: Option<PathBuf>,
    pub llm: Option<PartialLlmConfig>,
    pub watch: Option<PartialWatchConfig>,
    pub enable_stream_tools: Option<bool>,
    pub theme: Option<String>,
    pub project_instructions_file: Option<String>,
    pub no_repomap: Option<bool>,
    pub resume: Option<bool>,
    pub auto_compact_prompt_token_threshold: Option<u32>,
    pub auto_compact_prompt_token_thresholds: Option<HashMap<String, u32>>,
    pub show_diff: Option<bool>,
    pub allowed_commands: Option<Vec<String>>,
    pub allowed_paths: Option<Vec<PathBuf>>,
    pub mcp_servers: Option<Vec<PartialMcpServerConfig>>,
    pub rewrite_timeout_sec: Option<u64>,
    pub command_timeout_ms: Option<u64>,
}

pub fn get_default_config_content() -> String {
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

    if let Some(first_path) = candidate_paths.first() {
        if let Some(parent) = first_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create config directory: {}", parent.display())
            })?;
        }

        fs::write(first_path, get_default_config_content()).with_context(|| {
            format!(
                "failed to write default config file: {}",
                first_path.display()
            )
        })?;

        info!(path=%first_path.display(), "created default config file");

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

    Ok(FileConfig::default())
}

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
