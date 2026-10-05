use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use super::agent_budget::PartialAgentBudgetConfig;
use super::context_budget::PartialContextBudgetConfig;
use super::execution::PartialExecutionConfig;
use super::llm::PartialLlmConfig;
use super::mcp::{PartialLocalMcpServerConfig, PartialMcpServerConfig};
use super::reasoning::PartialReasoningConfig;
use super::subagent::PartialSubagentConfig;
use super::tool_routing::PartialToolRoutingConfig;

use super::watch::PartialWatchConfig;
use std::collections::HashMap;

#[derive(Clone, Default, serde::Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    pub provider: Option<crate::features::openai_subscription::ProviderKind>,
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
    /// Local MCP HTTP listener (`[mcp_server]`). Separate from `[[mcp_servers]]`
    /// remote/outbound endpoints.
    pub mcp_server: Option<PartialLocalMcpServerConfig>,
    pub rewrite_timeout_sec: Option<u64>,
    pub command_timeout_ms: Option<u64>,
    pub execution: Option<PartialExecutionConfig>,
    pub tool_routing: Option<PartialToolRoutingConfig>,
    pub reasoning: Option<PartialReasoningConfig>,
    pub context_budget: Option<PartialContextBudgetConfig>,
    pub subagent: Option<PartialSubagentConfig>,
    pub agent_budget: Option<PartialAgentBudgetConfig>,
}

/// Configuration load contract (v1):
///
/// - Missing config → runtime `Default` implementations, no filesystem mutation.
/// - Existing + valid config → parse, merge, success.
/// - Existing + invalid/unreadable config → startup error, file untouched.
/// - `DOGE_CODE_CONFIG` set → that path alone is authoritative (no fallback).
///
/// Loading is a read path. It never creates, repairs, rewrites, deletes, or
/// renames user configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigSourceKind {
    ExplicitEnvironment,
    User,
    System,
    Project,
}

#[derive(Debug, Clone)]
pub(crate) struct ConfigSource {
    path: PathBuf,
    kind: ConfigSourceKind,
}

impl ConfigSource {
    fn describe(&self) -> &'static str {
        match self.kind {
            ConfigSourceKind::ExplicitEnvironment => "explicit configuration",
            ConfigSourceKind::User => "user configuration",
            ConfigSourceKind::System => "system configuration",
            ConfigSourceKind::Project => "project configuration",
        }
    }
}

/// Byte-exact content of the legacy auto-generated default config.
///
/// Previous dgc versions wrote this template to the global config path on
/// first launch. It is invalid TOML (`project_instructions_file = null`) and
/// carries stale defaults (e.g. `max_retries = 100`, stale `[rag]` section)
/// that have drifted from the runtime `Default` implementations.
///
/// This constant exists solely for exact-match compatibility detection of
/// untouched legacy files. It is never written to disk and never used as a
/// source of runtime defaults.
const LEGACY_GENERATED_DEFAULT: &str = r#"# Doge-Code Configuration
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
show_diff = true
auto_compact_prompt_token_threshold = 250000
# auto_compact_prompt_token_thresholds can be defined as a map of model names to thresholds
resume = false
rewrite_timeout_sec = 30
# Managed finite-command timeout; 0 means unlimited (cancellation still works)
command_timeout_ms = 300000

# Allowed commands for execute_bash tool (deprecated; prefer [execution])
# allowed_commands = ["git", "ls", "cat", "grep", "find"]

# Structured execution policy. When [execution] is absent, legacy
# `allowed_commands` is used as a fallback.
# [execution]
# mode = "allowlist"  # unrestricted / allowlist / deny
# allowed_programs = ["cargo", "rustc", "git", "rg"]
# allow_shell = false
# allowed_env = ["RUST_BACKTRACE", "RUST_LOG", "CARGO_TERM_COLOR"]

# Tool routing: keep large tool catalogs out of the initial LLM context.
# Core tools load eagerly; other built-in and MCP tools activate on demand
# via `tool_search`.
# [tool_routing]
# mode = "auto"  # auto / eager / deferred
# search_result_limit = 5  # 1-10

# Adaptive reasoning policy: control provider reasoning budget per iteration.
# [reasoning]
# mode = "auto"  # auto / fixed / off
# initial_effort = "medium"  # low / medium / high
# routine_effort = "low"
# deliberative_effort = "medium"
# recovery_effort = "high"
# fixed_effort = "medium"

# Preflight context governor: measure the current request footprint
# (messages + active tool schemas + runtime overlay) before sending.
# [context_budget]
# mode = "auto"  # auto / observe / off

# Allowed paths for file access
# allowed_paths = ["/tmp", "/home/user/project"]

# Local MCP HTTP listener (Doge-Code's own server)
[mcp_server]
enabled = false
address = "127.0.0.1:8000"

# Remote MCP servers Doge-Code connects to (outbound endpoints)
# Structured stdio: command is executed directly; args are an argv array.
# [[mcp_servers]]
# name = "filesystem"
# enabled = true
# transport = "stdio"
# command = "/usr/local/bin/mcp-filesystem"
# args = ["--root", "/workspace"]
# connect_timeout_ms = 30000  # 0 means unlimited
# list_timeout_ms = 10000
# call_timeout_ms = 30000
# # [mcp_servers.env] is the TOML form for literal environment values.
# # DOGE_LOG_LEVEL = "info"

# Streamable HTTP example:
# [[mcp_servers]]
# name = "remote"
# enabled = true
# transport = "http"
# address = "https://example.com/mcp"

# Legacy stdio address syntax is deprecated; use command + args instead:
# [[mcp_servers]]
# name = "legacy"
# enabled = true
# transport = "stdio"
# address = "server --foo bar"
"#;

/// Returns true only for a byte-exact (modulo CRLF normalization) match with
/// the known legacy auto-generated default content.
///
/// Substring or field-level heuristics are deliberately rejected: a
/// user-edited file that merely contains `project_instructions_file = null`,
/// `max_retries = 100`, or `[rag]` must not be classified as legacy.
pub(crate) fn is_legacy_generated_default(raw: &str) -> bool {
    fn normalize(input: &str) -> String {
        input.replace("\r\n", "\n")
    }
    normalize(raw) == normalize(LEGACY_GENERATED_DEFAULT)
}

/// Test-only accessor for the exact legacy fixture bytes.
#[cfg(test)]
pub(crate) fn legacy_generated_default_content() -> &'static str {
    LEGACY_GENERATED_DEFAULT
}

fn normalize_path_for_error(path: &Path) -> String {
    path.display().to_string()
}

fn parse_config_str(raw: &str, source: &ConfigSource) -> Result<FileConfig> {
    let cfg = toml::from_str::<FileConfig>(raw).with_context(|| {
        format!(
            "failed to parse {} {}",
            source.describe(),
            normalize_path_for_error(&source.path)
        )
    })?;
    if let Some(subagent) = &cfg.subagent {
        subagent.validate().with_context(|| {
            format!(
                "invalid subagent configuration in {}",
                normalize_path_for_error(&source.path)
            )
        })?;
    }
    if let Some(agent_budget) = &cfg.agent_budget {
        agent_budget.validate().with_context(|| {
            format!(
                "invalid agent_budget configuration in {}",
                normalize_path_for_error(&source.path)
            )
        })?;
    }
    if let Some(tool_routing) = &cfg.tool_routing {
        tool_routing.validate().with_context(|| {
            format!(
                "invalid tool_routing configuration in {}",
                normalize_path_for_error(&source.path)
            )
        })?;
    }
    Ok(cfg)
}

/// Explicit `DOGE_CODE_CONFIG` path, if set to a non-empty value.
///
/// An empty value is treated as unset to avoid surprising startup failures
/// from inherited empty environment entries.
fn explicit_config_path() -> Option<PathBuf> {
    std::env::var("DOGE_CODE_CONFIG")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
}

/// Implicit global candidates in precedence order:
///
/// `XDG_CONFIG_HOME` (or `HOME/.config`) first, then each `XDG_CONFIG_DIRS`
/// entry. The order is unchanged from previous versions; only the
/// missing-vs-broken semantics are now explicit.
///
/// Empty `XDG_CONFIG_HOME`/`HOME` values are treated as unset: otherwise
/// `Path::new("").join(...)` would produce a relative candidate that could
/// accidentally match a repo-local file.
fn implicit_candidate_paths() -> Vec<ConfigSource> {
    use std::env;

    let mut sources = Vec::new();
    if let Ok(xdg_home) = env::var("XDG_CONFIG_HOME")
        && !xdg_home.is_empty()
    {
        sources.push(ConfigSource {
            path: Path::new(&xdg_home).join("doge-code/config.toml"),
            kind: ConfigSourceKind::User,
        });
    } else if let Ok(home) = env::var("HOME")
        && !home.is_empty()
    {
        sources.push(ConfigSource {
            path: Path::new(&home).join(".config/doge-code/config.toml"),
            kind: ConfigSourceKind::User,
        });
    }
    if let Ok(dirs) = env::var("XDG_CONFIG_DIRS") {
        for dir in dirs.split(':') {
            if !dir.is_empty() {
                sources.push(ConfigSource {
                    path: Path::new(dir).join("doge-code/config.toml"),
                    kind: ConfigSourceKind::System,
                });
            }
        }
    }
    sources
}

fn handle_legacy_file(source: &ConfigSource) {
    warn!(
        path = %source.path.display(),
        kind = source.describe(),
        "Detected an untouched legacy auto-generated config with invalid/stale defaults; ignoring it and using current runtime defaults. Replace or remove the file to customize configuration."
    );
}

/// True when a failed read means the path is genuinely absent.
///
/// `read_to_string` reports `NotFound` both for absent paths and for
/// dangling symlinks, so consult metadata without following symlinks: if the
/// metadata itself is missing, the candidate is absent; otherwise the entry
/// exists but cannot be read and must be an error (fail-closed).
fn is_truly_missing(path: &Path, read_err: &std::io::Error) -> bool {
    read_err.kind() == std::io::ErrorKind::NotFound && path.symlink_metadata().is_err()
}

/// Test-convenience wrapper: kinds are approximated by position (first =
/// user, rest = system) because injected paths carry no origin metadata.
/// The production path (`load_file_config`) calls `load_from_sources`
/// directly with the true kinds from `implicit_candidate_paths()`.
#[cfg(test)]
pub(crate) fn load_file_config_from_candidates(
    explicit: Option<&Path>,
    implicit: &[PathBuf],
) -> Result<FileConfig> {
    let sources: Vec<ConfigSource> = implicit
        .iter()
        .enumerate()
        .map(|(index, path)| ConfigSource {
            path: path.clone(),
            kind: if index == 0 {
                ConfigSourceKind::User
            } else {
                ConfigSourceKind::System
            },
        })
        .collect();
    load_from_sources(explicit, &sources)
}

/// Core loader over resolved [`ConfigSource`]s.
///
/// - `explicit` is authoritative when `Some`: missing/unreadable/invalid is
///   an error and implicit candidates are never consulted.
/// - Each implicit candidate: genuinely missing → next candidate; present
///   but unreadable/invalid → error (no fallback to lower-priority files).
///   Missing is determined by the read itself (plus a symlink-aware
///   metadata probe) rather than a prior `exists()` check, so dangling
///   symlinks and unreadable parent directories are not misclassified as
///   missing, and there is no exists-then-read TOCTOU.
/// - Untouched legacy auto-generated content is treated as missing (warn and
///   continue for implicit candidates; warn and use defaults for explicit).
/// - No filesystem mutation is performed.
fn load_from_sources(explicit: Option<&Path>, implicit: &[ConfigSource]) -> Result<FileConfig> {
    if let Some(explicit_path) = explicit {
        let source = ConfigSource {
            path: explicit_path.to_path_buf(),
            kind: ConfigSourceKind::ExplicitEnvironment,
        };
        let raw = match fs::read_to_string(&source.path) {
            Ok(raw) => raw,
            Err(e) if is_truly_missing(&source.path, &e) => {
                anyhow::bail!(
                    "explicit configuration file not found {} (DOGE_CODE_CONFIG is authoritative; no fallback is attempted)",
                    normalize_path_for_error(&source.path)
                );
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "failed to read {} {}",
                        source.describe(),
                        normalize_path_for_error(&source.path)
                    )
                });
            }
        };
        if is_legacy_generated_default(&raw) {
            handle_legacy_file(&source);
            return Ok(FileConfig::default());
        }
        let cfg = parse_config_str(&raw, &source)?;
        info!(path=%source.path.display(), "loaded config file");
        return Ok(cfg);
    }

    for source in implicit {
        let raw = match fs::read_to_string(&source.path) {
            Ok(raw) => raw,
            Err(e) if is_truly_missing(&source.path, &e) => continue,
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "failed to read {} {}",
                        source.describe(),
                        normalize_path_for_error(&source.path)
                    )
                });
            }
        };
        if is_legacy_generated_default(&raw) {
            handle_legacy_file(source);
            continue;
        }
        let cfg = parse_config_str(&raw, source)?;
        info!(path=%source.path.display(), "loaded config file");
        return Ok(cfg);
    }

    Ok(FileConfig::default())
}

pub fn load_file_config() -> Result<FileConfig> {
    if let Some(explicit) = explicit_config_path() {
        return load_from_sources(Some(&explicit), &[]);
    }
    let implicit = implicit_candidate_paths();
    load_from_sources(None, &implicit)
}

pub fn load_project_config(project_root: &Path) -> Result<FileConfig> {
    let project_config_path = project_root.join(".doge").join("config.toml");
    let source = ConfigSource {
        path: project_config_path,
        kind: ConfigSourceKind::Project,
    };

    let raw = match fs::read_to_string(&source.path) {
        Ok(raw) => raw,
        Err(e) if is_truly_missing(&source.path, &e) => {
            return Ok(FileConfig::default());
        }
        Err(e) => {
            return Err(e).with_context(|| {
                format!(
                    "failed to read {} {}",
                    source.describe(),
                    normalize_path_for_error(&source.path)
                )
            });
        }
    };
    // Defensive: the legacy template was only ever auto-generated at the
    // global path, but a verbatim copy pasted into `.doge/config.toml` is
    // still an untouched app-generated file rather than a user-owned broken
    // config, so it receives the same warn-and-default treatment.
    if is_legacy_generated_default(&raw) {
        handle_legacy_file(&source);
        return Ok(FileConfig::default());
    }
    let cfg = parse_config_str(&raw, &source)?;
    info!(path=%source.path.display(), "loaded project config file");
    Ok(cfg)
}

impl std::fmt::Debug for FileConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileConfig")
            .field("model", &self.model)
            .field("api_key", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}
