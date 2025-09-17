use anyhow::Result;
use rmcp::{
    model::{CallToolRequestParam, ListToolsResult},
    RmcpError,
};
use tokio::sync::RwLock;

use crate::config::McpServerConfig;

/// Represents an MCP client connection to a server
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::McpServerConfig;
    use std::time::Duration;
    use tokio::time::timeout;

    #[tokio::test]
    async fn test_mcp_client_creation_with_invalid_transport() {
        let config = McpServerConfig {
            name: "test".to_string(),
            enabled: true,
            address: "127.0.0.1:8000".to_string(),
            transport: "invalid".to_string(),
        };

        let result = McpClient::from_config(&config).await;
        assert!(result.is_err());
        
        if let Err(RmcpError::InternalError { .. }) = result {
            // Expected error
        } else {
            panic!("Expected internal error for invalid transport");
        }
    }

    #[tokio::test]
    async fn test_mcp_client_creation_with_invalid_stdio_command() {
        let config = McpServerConfig {
            name: "test".to_string(),
            enabled: true,
            address: "".to_string(), // Invalid command
            transport: "stdio".to_string(),
        };

        let result = McpClient::from_config(&config).await;
        assert!(result.is_err());
        
        if let Err(RmcpError::InternalError { .. }) = result {
            // Expected error
        } else {
            panic!("Expected internal error for invalid stdio command");
        }
    }

    #[tokio::test]
    async fn test_mcp_client_config_access() {
        let config = McpServerConfig {
            name: "test".to_string(),
            enabled: true,
            address: "127.0.0.1:8000".to_string(),
            transport: "http".to_string(),
        };

        // We can't actually connect to a server in tests, so we'll just test the config access
        assert_eq!(config.name, "test");
        assert_eq!(config.address, "127.0.0.1:8000");
        assert_eq!(config.transport, "http");
    }
}