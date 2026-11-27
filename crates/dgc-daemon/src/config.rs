//! Daemon configuration file (`config.toml`).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use dgc_protocol::methods::ProfileInfo;
use dgc_protocol::model::ProviderKind;
use serde::{Deserialize, Deserializer, de};

use crate::provider::validate_mode;

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
    #[error("invalid profile {name:?}: {reason}")]
    InvalidProfile { name: String, reason: String },
    #[error("max_agent_depth must be in 1..=8, got {0}")]
    InvalidAgentDepth(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileConfig {
    pub provider: ProviderKind,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub notes: Option<String>,
    pub mcp: Option<bool>,
}

fn deserialize_profiles<'de, D>(input: D) -> Result<BTreeMap<String, ProfileConfig>, D::Error>
where
    D: Deserializer<'de>,
{
    // TOML can highlight a field inside a table without naming the table.
    BTreeMap::<String, toml::Value>::deserialize(input)?
        .into_iter()
        .map(|(name, value)| {
            let profile = value
                .try_into()
                .map_err(|err| de::Error::custom(format!("invalid profile {name:?}: {err}")))?;
            Ok((name, profile))
        })
        .collect()
}

impl ProfileConfig {
    fn auto_approve(&self, name: &str) -> Result<bool, ConfigError> {
        validate_mode(self.provider, self.mode.as_deref()).map_err(|reason| {
            ConfigError::InvalidProfile {
                name: name.to_owned(),
                reason,
            }
        })
    }
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
    /// Maximum nesting depth of a child agent.
    pub max_agent_depth: u32,
    /// Named launch settings, ordered by name.
    #[serde(deserialize_with = "deserialize_profiles")]
    pub profiles: BTreeMap<String, ProfileConfig>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            claude_path: None,
            codex_path: None,
            events_max_bytes: DEFAULT_EVENTS_MAX_BYTES,
            max_agent_depth: 3,
            profiles: BTreeMap::new(),
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
        let config: Self = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source,
        })?;
        config.validate()?;
        Ok(config)
    }

    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if !(1..=8).contains(&self.max_agent_depth) {
            return Err(ConfigError::InvalidAgentDepth(self.max_agent_depth));
        }
        for (name, profile) in &self.profiles {
            if !(1..=32).contains(&name.len())
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return Err(ConfigError::InvalidProfile {
                    name: name.clone(),
                    reason: "profile name must match [A-Za-z0-9_-]{1,32}".to_owned(),
                });
            }
            profile.auto_approve(name)?;
        }
        Ok(())
    }

    pub(crate) fn profile_infos(&self) -> Result<Vec<ProfileInfo>, ConfigError> {
        self.profiles
            .iter()
            .map(|(name, profile)| {
                Ok(ProfileInfo {
                    name: name.clone(),
                    provider: profile.provider,
                    model: profile.model.clone(),
                    mode: profile.mode.clone(),
                    notes: profile.notes.clone(),
                    auto_approve: profile.auto_approve(name)?,
                    mcp: profile.mcp.unwrap_or_default(),
                })
            })
            .collect()
    }
}
