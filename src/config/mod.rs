use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub no_tui: bool,
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub log_level: String,
    pub project_root: PathBuf,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileConfig {
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub log_level: Option<String>,
    pub project_root: Option<std::path::PathBuf>,
}

impl AppConfig {
    pub fn from_cli(cli: crate::Cli) -> Result<Self> {
        let project_root = std::env::current_dir().context("resolve current dir")?;
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
        let log_level = if cli.log_level.is_empty() {
            std::env::var("DOGE_LOG")
                .ok()
                .or(file_cfg.log_level)
                .unwrap_or_else(|| "info".to_string())
        } else {
            cli.log_level
        };
        let project_root = file_cfg.project_root.unwrap_or(project_root);
        Ok(Self {
            no_tui: cli.no_tui,
            base_url,
            model,
            api_key,
            log_level,
            project_root,
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
