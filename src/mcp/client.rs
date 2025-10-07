use anyhow::Result;
use reqwest::Url;
use rmcp::{
    RmcpError,
    model::{CallToolRequestParam, ListToolsResult},
    transport::{StreamableHttpClientTransport, TokioChildProcess},
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::time::{Duration, timeout};
use tracing::{debug, info, warn};

use crate::config::McpServerConfig;
use std::process::Stdio;

fn internal_error(message: impl Into<String>) -> RmcpError {
    RmcpError::TransportCreation {
        into_transport_type_name: "internal".into(),
        into_transport_type_id: std::any::TypeId::of::<()>(),
        error: message.into().into(),
    }
}

fn normalize_http_uri(address: &str) -> Result<String, RmcpError> {
    let trimmed = address.trim();
    if trimmed.is_empty() {
        return Err(internal_error("MCP HTTP address cannot be empty"));
    }

    let with_scheme = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("http://{}", trimmed)
    };

    let mut url = Url::parse(&with_scheme)
        .map_err(|e| internal_error(format!("Invalid MCP HTTP address: {}", e)))?;

    if url.path() == "/" {
        url.set_path("mcp");
    }

    Ok(url.to_string())
}

/// Represents an MCP client connection to a server
pub struct McpClient {
    client: rmcp::service::RunningService<rmcp::RoleClient, ()>,
    config: McpServerConfig,
}

impl McpClient {
    /// Create a new MCP client from a server configuration
    pub async fn from_config(config: &McpServerConfig) -> Result<Self, RmcpError> {
        // Validate transport type
        if config.transport != "stdio" && config.transport != "http" {
            return Err(internal_error(format!(
                "Unsupported transport type: {}",
                config.transport
            )));
        }

        // Create transport based on type
        let mut normalized_config = config.clone();

        let client = if config.transport == "stdio" {
            // For stdio transport, address is the command to run
            let mut parts = config.address.split_whitespace();
            let program = parts
                .next()
                .ok_or_else(|| internal_error("Invalid command for stdio transport"))?;

            let mut cmd = Command::new(program);
            for arg in parts {
                cmd.arg(arg);
            }

            let (transport, stderr_opt) = TokioChildProcess::builder(cmd)
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| RmcpError::TransportCreation {
                    into_transport_type_name: "TokioChildProcess".into(),
                    into_transport_type_id: std::any::TypeId::of::<TokioChildProcess>(),
                    error: Box::new(e),
                })?;

            if let Some(stderr) = stderr_opt {
                tokio::spawn(async move {
                    let mut lines = BufReader::new(stderr).lines();
                    loop {
                        match lines.next_line().await {
                            Ok(Some(line)) => {
                                warn!(target: "mcp::client", "MCP server stderr: {}", line);
                            }
                            Ok(None) => break,
                            Err(e) => {
                                warn!(target: "mcp::client", error = %e, "Failed to read MCP server stderr");
                                break;
                            }
                        }
                    }
                });
            }

            // Create client with timeout
            timeout(
                Duration::from_secs(30), // 30 second timeout for connection
                rmcp::service::serve_client((), transport),
            )
            .await
            .map_err(|_| internal_error("MCP connection timeout"))?
            .map_err(|e| {
                warn!(error = ?e, "Failed to create MCP client");
                RmcpError::TransportCreation {
                    into_transport_type_name: "TokioChildProcess".into(),
                    into_transport_type_id: std::any::TypeId::of::<TokioChildProcess>(),
                    error: Box::new(e),
                }
            })?
        } else {
            // For HTTP transport, address is the URL
            let http_uri = normalize_http_uri(&config.address)?;
            normalized_config.address = http_uri.clone();
            let transport = StreamableHttpClientTransport::from_uri(http_uri);

            // Create client with timeout
            timeout(
                Duration::from_secs(30), // 30 second timeout for connection
                rmcp::service::serve_client((), transport),
            )
            .await
            .map_err(|_| internal_error("MCP connection timeout"))?
            .map_err(|e| {
                warn!(error = ?e, "Failed to create MCP client");
                RmcpError::TransportCreation {
                    into_transport_type_name: "StreamableHttpClientTransport".into(),
                    into_transport_type_id: std::any::TypeId::of::<
                        StreamableHttpClientTransport<reqwest::Client>,
                    >(),
                    error: Box::new(e),
                }
            })?
        };

        Ok(Self {
            client,
            config: normalized_config,
        })
    }

    /// Get server information
    pub async fn get_server_info(&self) -> Result<rmcp::model::ServerInfo, RmcpError> {
        self.client
            .peer_info()
            .cloned()
            .ok_or_else(|| RmcpError::TransportCreation {
                into_transport_type_name: "Client".into(),
                into_transport_type_id: std::any::TypeId::of::<()>(),
                error: "Server info not available".into(),
            })
    }

    /// List available tools
    pub async fn list_tools(&self) -> Result<ListToolsResult, RmcpError> {
        // Add timeout for list_tools call
        let result = timeout(
            Duration::from_secs(10),
            self.client.list_tools(Default::default()),
        )
        .await
        .map_err(|_| internal_error("List tools timeout"))?;

        result.map_err(|e| RmcpError::TransportCreation {
            into_transport_type_name: "Client".into(),
            into_transport_type_id: std::any::TypeId::of::<()>(),
            error: format!("List tools error: {:?}", e).into(),
        })
    }

    /// Call a tool with parameters
    pub async fn call_tool(
        &self,
        params: CallToolRequestParam,
    ) -> Result<rmcp::model::CallToolResult, RmcpError> {
        info!(tool_name = %params.name, "Calling MCP tool");
        debug!(tool_params = ?params.arguments, "Tool parameters");

        // Add timeout for tool call
        let result = timeout(
            Duration::from_secs(30), // 30 second timeout for tool call
            self.client.call_tool(params),
        )
        .await
        .map_err(|_| internal_error("Tool call timeout"))?;

        debug!(tool_result = ?result, "Tool result");
        result.map_err(|e| RmcpError::TransportCreation {
            into_transport_type_name: "Client".into(),
            into_transport_type_id: std::any::TypeId::of::<()>(),
            error: format!("Call tool error: {:?}", e).into(),
        })
    }

    /// Get the server configuration
    pub fn get_config(&self) -> &McpServerConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_http_uri;

    #[test]
    fn normalize_adds_scheme_and_path_when_missing() {
        let normalized = normalize_http_uri("127.0.0.1:8000").expect("should normalize");
        assert_eq!(normalized, "http://127.0.0.1:8000/mcp");
    }

    #[test]
    fn normalize_preserves_existing_path() {
        let normalized =
            normalize_http_uri("http://127.0.0.1:8000/custom").expect("should normalize");
        assert_eq!(normalized, "http://127.0.0.1:8000/custom");
    }

    #[test]
    fn normalize_handles_https() {
        let normalized = normalize_http_uri("https://example.com/api").expect("should normalize");
        assert_eq!(normalized, "https://example.com/api");
    }

    #[test]
    fn normalize_errors_on_empty_input() {
        assert!(normalize_http_uri("").is_err());
    }
}
