use anyhow::Result;
use rmcp::{
    RmcpError,
    model::{CallToolRequestParam, ListToolsResult},
    transport::{StreamableHttpClientTransport, TokioChildProcess},
};
use tokio::process::Command;
use tokio::time::{Duration, timeout};
use tracing::{debug, info, warn};

use crate::config::McpServerConfig;

fn internal_error(message: impl Into<String>) -> RmcpError {
    RmcpError::TransportCreation {
        into_transport_type_name: "internal".into(),
        into_transport_type_id: std::any::TypeId::of::<()>(),
        error: message.into().into(),
    }
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

            let transport =
                TokioChildProcess::new(cmd).map_err(|e| RmcpError::TransportCreation {
                    into_transport_type_name: "TokioChildProcess".into(),
                    into_transport_type_id: std::any::TypeId::of::<TokioChildProcess>(),
                    error: Box::new(e),
                })?;

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
            let transport = StreamableHttpClientTransport::from_uri(config.address.clone());

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
            config: config.clone(),
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
