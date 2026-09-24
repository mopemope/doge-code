use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use url::Url;

/// Default timeout for establishing an outbound MCP connection.
pub const DEFAULT_MCP_CONNECT_TIMEOUT_MS: u64 = 30_000;
/// Default timeout for one complete paginated `tools/list` operation.
pub const DEFAULT_MCP_LIST_TIMEOUT_MS: u64 = 10_000;
/// Default timeout for one `tools/call` operation.
pub const DEFAULT_MCP_CALL_TIMEOUT_MS: u64 = 30_000;

/// Transport used to reach an outbound MCP server.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum McpTransport {
    /// A child process communicating over stdin/stdout.
    Stdio,
    /// A Streamable HTTP endpoint.
    #[default]
    Http,
}

/// Errors found while validating an outbound MCP server configuration.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum McpConfigError {
    #[error("MCP server name must not be empty")]
    EmptyName,
    #[error("MCP HTTP server '{name}' requires an address")]
    MissingHttpAddress { name: String },
    #[error("MCP HTTP server '{name}' has an invalid address: {reason}")]
    InvalidHttpAddress { name: String, reason: String },
    #[error("MCP HTTP server '{name}' cannot specify command, args, or env")]
    HttpStdioFields { name: String },
    #[error("MCP stdio server '{name}' requires command or legacy address")]
    MissingStdioCommand { name: String },
    #[error(
        "MCP stdio server '{name}' cannot specify both command and legacy address; use command and args"
    )]
    StdioCommandAndAddress { name: String },
    #[error("MCP stdio server '{name}' cannot specify args or env without command")]
    StdioArgsWithoutCommand { name: String },
    #[error("MCP stdio server '{name}' has an empty command")]
    EmptyStdioCommand { name: String },
    #[error("MCP server name '{name}' is configured more than once")]
    DuplicateServerName { name: String },
}

fn default_connect_timeout_ms() -> u64 {
    DEFAULT_MCP_CONNECT_TIMEOUT_MS
}

fn default_list_timeout_ms() -> u64 {
    DEFAULT_MCP_LIST_TIMEOUT_MS
}

fn default_call_timeout_ms() -> u64 {
    DEFAULT_MCP_CALL_TIMEOUT_MS
}

/// Outbound/remote MCP endpoint configuration.
///
/// This describes a remote MCP server that Doge-Code connects *to* as a
/// client. Structured stdio configuration is preferred: `command` is executed
/// directly and `args` are passed as an argv array, never through a shell.
/// `address` is retained for HTTP endpoints and as a deprecated stdio
/// fallback.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct McpServerConfig {
    pub name: String,
    pub enabled: bool,
    #[serde(default)]
    pub transport: McpTransport,
    /// HTTP URL, or the deprecated whitespace-separated stdio command line.
    pub address: Option<String>,
    /// Executable for structured stdio transport.
    pub command: Option<String>,
    /// Arguments for structured stdio transport.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables for structured stdio transport.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Connection timeout in milliseconds; `0` means unlimited.
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    /// Complete tools/list timeout in milliseconds; `0` means unlimited.
    #[serde(default = "default_list_timeout_ms")]
    pub list_timeout_ms: u64,
    /// Tool call timeout in milliseconds; `0` means unlimited.
    #[serde(default = "default_call_timeout_ms")]
    pub call_timeout_ms: u64,
}

impl std::fmt::Debug for McpServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServerConfig")
            .field("name", &self.name)
            .field("enabled", &self.enabled)
            .field("transport", &self.transport)
            .field("address", &self.address.as_ref().map(|_| "<configured>"))
            .field("command", &self.command.as_ref().map(|_| "<configured>"))
            .field("arg_count", &self.args.len())
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .field("connect_timeout_ms", &self.connect_timeout_ms)
            .field("list_timeout_ms", &self.list_timeout_ms)
            .field("call_timeout_ms", &self.call_timeout_ms)
            .finish()
    }
}

impl McpServerConfig {
    /// Validate transport-specific fields before attempting a connection.
    pub fn validate(&self) -> Result<(), McpConfigError> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(McpConfigError::EmptyName);
        }

        match self.transport {
            McpTransport::Http => {
                if self.command.is_some() || !self.args.is_empty() || !self.env.is_empty() {
                    return Err(McpConfigError::HttpStdioFields {
                        name: self.name.clone(),
                    });
                }
                let address = self
                    .address
                    .as_deref()
                    .map(str::trim)
                    .filter(|address| !address.is_empty())
                    .ok_or_else(|| McpConfigError::MissingHttpAddress {
                        name: self.name.clone(),
                    })?;
                validate_http_address(self.name.clone(), address)
            }
            McpTransport::Stdio => {
                if self.command.is_some() && self.address.is_some() {
                    return Err(McpConfigError::StdioCommandAndAddress {
                        name: self.name.clone(),
                    });
                }
                if self.command.is_none() && (!self.args.is_empty() || !self.env.is_empty()) {
                    return Err(McpConfigError::StdioArgsWithoutCommand {
                        name: self.name.clone(),
                    });
                }
                if let Some(command) = &self.command {
                    if command.trim().is_empty() {
                        return Err(McpConfigError::EmptyStdioCommand {
                            name: self.name.clone(),
                        });
                    }
                    return Ok(());
                }
                let legacy_address = self
                    .address
                    .as_deref()
                    .map(str::trim)
                    .filter(|address| !address.is_empty())
                    .ok_or_else(|| McpConfigError::MissingStdioCommand {
                        name: self.name.clone(),
                    })?;
                if legacy_address.split_whitespace().next().is_none() {
                    return Err(McpConfigError::MissingStdioCommand {
                        name: self.name.clone(),
                    });
                }
                Ok(())
            }
        }
    }
}

pub fn validate_mcp_server_names(servers: &[McpServerConfig]) -> Result<(), McpConfigError> {
    let mut names = HashSet::new();
    for server in servers.iter().filter(|server| server.enabled) {
        if !names.insert(server.name.as_str()) {
            return Err(McpConfigError::DuplicateServerName {
                name: server.name.clone(),
            });
        }
    }
    Ok(())
}

fn validate_http_address(name: String, address: &str) -> Result<(), McpConfigError> {
    let candidate = if address.contains("://") {
        address.to_string()
    } else {
        format!("http://{address}")
    };
    // Keep parser diagnostics free of the raw URL. The URL may contain
    // credentials or query parameters that must not reach logs or status text.
    let parsed = Url::parse(&candidate).map_err(|_| McpConfigError::InvalidHttpAddress {
        name: name.clone(),
        reason: "invalid URL".to_string(),
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(McpConfigError::InvalidHttpAddress {
            name,
            reason: format!("unsupported URL scheme '{}'", parsed.scheme()),
        });
    }
    if parsed.host_str().is_none() {
        return Err(McpConfigError::InvalidHttpAddress {
            name,
            reason: "URL must include a host".to_string(),
        });
    }
    Ok(())
}

/// Local MCP HTTP listener configuration (`[mcp_server]`).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct LocalMcpServerConfig {
    pub enabled: bool,
    pub address: String,
}

impl Default for LocalMcpServerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            address: "127.0.0.1:8000".to_string(),
        }
    }
}

/// Layered-config partial for `[mcp_server]` (global <- project merge).
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct PartialLocalMcpServerConfig {
    pub enabled: Option<bool>,
    pub address: Option<String>,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            enabled: false,
            transport: McpTransport::Http,
            address: None,
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            connect_timeout_ms: DEFAULT_MCP_CONNECT_TIMEOUT_MS,
            list_timeout_ms: DEFAULT_MCP_LIST_TIMEOUT_MS,
            call_timeout_ms: DEFAULT_MCP_CALL_TIMEOUT_MS,
        }
    }
}

/// Partial outbound MCP configuration used while merging global and project
/// files. Project scalar fields win; project environment keys win per key.
#[derive(Clone, Default, Deserialize, PartialEq)]
pub struct PartialMcpServerConfig {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub address: Option<String>,
    pub transport: Option<McpTransport>,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub env: Option<BTreeMap<String, String>>,
    pub connect_timeout_ms: Option<u64>,
    pub list_timeout_ms: Option<u64>,
    pub call_timeout_ms: Option<u64>,
}

impl std::fmt::Debug for PartialMcpServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PartialMcpServerConfig")
            .field("name", &self.name)
            .field("enabled", &self.enabled)
            .field("transport", &self.transport)
            .field("address", &self.address.as_ref().map(|_| "<configured>"))
            .field("command", &self.command.as_ref().map(|_| "<configured>"))
            .field("arg_count", &self.args.as_ref().map(Vec::len))
            .field(
                "env_keys",
                &self.env.as_ref().map(|env| env.keys().collect::<Vec<_>>()),
            )
            .field("connect_timeout_ms", &self.connect_timeout_ms)
            .field("list_timeout_ms", &self.list_timeout_ms)
            .field("call_timeout_ms", &self.call_timeout_ms)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_structured_stdio_config_parses_and_validates() {
        let config: McpServerConfig = toml::from_str(
            r#"
            name = "filesystem"
            enabled = true
            transport = "stdio"
            command = "/tmp/foo server"
            args = ["--config", "/tmp/foo config.json"]

            [env]
            DOGE_TEST_VALUE = "abc"
            "#,
        )
        .expect("structured stdio config should parse");
        config
            .validate()
            .expect("structured stdio config should validate");
        assert_eq!(config.command.as_deref(), Some("/tmp/foo server"));
        assert_eq!(config.args, vec!["--config", "/tmp/foo config.json"]);
        assert_eq!(
            config.env.get("DOGE_TEST_VALUE").map(String::as_str),
            Some("abc")
        );
    }

    #[test]
    fn test_legacy_direct_config_keeps_timeout_defaults() {
        let config: McpServerConfig = toml::from_str(
            r#"
            name = "legacy"
            enabled = true
            transport = "http"
            address = "http://127.0.0.1:8000/mcp"
            "#,
        )
        .expect("legacy direct config should parse");
        assert_eq!(config.connect_timeout_ms, DEFAULT_MCP_CONNECT_TIMEOUT_MS);
        assert_eq!(config.list_timeout_ms, DEFAULT_MCP_LIST_TIMEOUT_MS);
        assert_eq!(config.call_timeout_ms, DEFAULT_MCP_CALL_TIMEOUT_MS);
    }

    #[test]
    fn test_http_rejects_stdio_fields() {
        let config = McpServerConfig {
            name: "remote".to_string(),
            enabled: true,
            transport: McpTransport::Http,
            address: Some("https://example.com/mcp".to_string()),
            command: Some("server".to_string()),
            ..McpServerConfig::default()
        };
        assert_eq!(
            config.validate(),
            Err(McpConfigError::HttpStdioFields {
                name: "remote".to_string()
            })
        );
    }

    #[test]
    fn test_stdio_rejects_command_and_address_together() {
        let config = McpServerConfig {
            name: "remote".to_string(),
            enabled: true,
            transport: McpTransport::Stdio,
            address: Some("server --legacy".to_string()),
            command: Some("server".to_string()),
            ..McpServerConfig::default()
        };
        assert_eq!(
            config.validate(),
            Err(McpConfigError::StdioCommandAndAddress {
                name: "remote".to_string()
            })
        );
    }

    #[test]
    fn test_server_config_debug_redacts_command_and_environment_values() {
        let config = McpServerConfig {
            name: "stdio".to_string(),
            enabled: true,
            transport: McpTransport::Stdio,
            command: Some("/private/server path".to_string()),
            env: BTreeMap::from([("TOKEN".to_string(), "secret-value".to_string())]),
            ..McpServerConfig::default()
        };
        let debug = format!("{config:?}");
        assert!(!debug.contains("secret-value"));
        assert!(!debug.contains("server path"));
        assert!(debug.contains("TOKEN"));
    }

    #[test]
    fn test_duplicate_enabled_server_names_are_rejected() {
        let servers = vec![
            McpServerConfig {
                name: "duplicate".to_string(),
                enabled: true,
                ..McpServerConfig::default()
            },
            McpServerConfig {
                name: "duplicate".to_string(),
                enabled: true,
                ..McpServerConfig::default()
            },
        ];
        assert_eq!(
            validate_mcp_server_names(&servers),
            Err(McpConfigError::DuplicateServerName {
                name: "duplicate".to_string()
            })
        );
    }

    #[test]
    fn test_invalid_http_config_error_redacts_credentials() {
        let config = McpServerConfig {
            name: "remote".to_string(),
            enabled: true,
            transport: McpTransport::Http,
            address: Some("https://user:secret@".to_string()),
            ..McpServerConfig::default()
        };
        let error = format!("{}", config.validate().expect_err("invalid URL"));
        assert!(!error.contains("secret"));
        assert!(!error.contains("user"));
    }

    #[test]
    fn test_http_rejects_non_http_scheme() {
        let config = McpServerConfig {
            name: "remote".to_string(),
            enabled: true,
            transport: McpTransport::Http,
            address: Some("file:///tmp/server".to_string()),
            ..McpServerConfig::default()
        };
        assert!(matches!(
            config.validate(),
            Err(McpConfigError::InvalidHttpAddress { .. })
        ));
    }
}
