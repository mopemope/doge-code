//! Daemon configuration file (`config.toml`).

use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Default cap for one agent's event log file: 32 MiB.
pub const DEFAULT_EVENTS_MAX_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read config {}: {source}", path.display())]
    Read { path: PathBuf, source: io::Error },
    #[error("invalid config {}: {source}", path.display())]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    /// Override for the `claude` executable.
    pub claude_path: Option<PathBuf>,
    /// Override for the `codex` executable.
    pub codex_path: Option<PathBuf>,
    /// Size at which an agent's event log rotates.
    pub events_max_bytes: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            claude_path: None,
            codex_path: None,
            events_max_bytes: DEFAULT_EVENTS_MAX_BYTES,
        }
    }
}

impl DaemonConfig {
    /// Load from `path`. A missing file yields defaults; a malformed file or an
    /// unknown key is an error.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(ConfigError::Read {
                    path: path.to_owned(),
                    source,
                });
            }
        };
        toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source,
        })
    }
}
