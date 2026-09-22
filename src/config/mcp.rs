use serde::Deserialize;

/// Outbound/remote MCP endpoint configuration.
///
/// This describes a remote MCP server that Doge-Code connects *to* as a
/// client (see `RemoteToolManager` / `McpClient`). It is never used as the
/// configuration for Doge-Code's own built-in local HTTP listener; that is
/// [`LocalMcpServerConfig`] (`[mcp_server]`).
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub enabled: bool,
    pub address: String,
    pub transport: String, // "stdio" or "http"
}

/// Local MCP HTTP listener configuration (`[mcp_server]`).
///
/// This configures Doge-Code's own built-in Streamable HTTP listener.
/// It is intentionally separate from `[[mcp_servers]]`, which configures
/// outbound/remote MCP endpoints Doge-Code connects to.
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
            address: "127.0.0.1:8000".to_string(),
            transport: "http".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct PartialMcpServerConfig {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub address: Option<String>,
    pub transport: Option<String>,
}
