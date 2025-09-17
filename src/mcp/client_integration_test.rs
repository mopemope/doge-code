// Integration tests for MCP client
// These tests require a running MCP server

use doge_code::config::McpServerConfig;
use doge_code::mcp::client::McpClient;
use rmcp::model::{CallToolRequestParam, TextContent};
use serde_json::json;

#[tokio::test]
async fn test_mcp_client_integration_with_http_server() {
    // This test requires a running HTTP MCP server
    // It's commented out by default as it requires external setup
    /*
    let config = McpServerConfig {
        name: "integration-test-http".to_string(),
        enabled: true,
        address: "http://127.0.0.1:8000".to_string(), // Adjust to your test server
        transport: "http".to_string(),
    };

    let client = McpClient::from_config(&config).await.expect("Failed to create client");
    
    // Test getting server info
    let server_info = client.get_server_info().await.expect("Failed to get server info");
    println!("Server info: {:?}", server_info);
    
    // Test listing tools
    let tools = client.list_tools().await.expect("Failed to list tools");
    println!("Available tools: {:?}", tools);
    
    // Test calling a tool (adjust to a tool your server provides)
    // let tool_result = client.call_tool(CallToolRequestParam {
    //     name: "echo".to_string(),
    //     arguments: Some(serde_json::Map::from_iter(vec![
    //         ("message".to_string(), json!("Hello, MCP!"))
    //     ])),
    // }).await.expect("Failed to call tool");
    // println!("Tool result: {:?}", tool_result);
    */
}

#[tokio::test]
async fn test_mcp_client_integration_with_stdio_server() {
    // This test requires an MCP server that can be started with a command
    // It's commented out by default as it requires external setup
    /*
    let config = McpServerConfig {
        name: "integration-test-stdio".to_string(),
        enabled: true,
        address: "uvx mcp-server-git".to_string(), // Adjust to your test server command
        transport: "stdio".to_string(),
    };

    let client = McpClient::from_config(&config).await.expect("Failed to create client");
    
    // Test getting server info
    let server_info = client.get_server_info().await.expect("Failed to get server info");
    println!("Server info: {:?}", server_info);
    
    // Test listing tools
    let tools = client.list_tools().await.expect("Failed to list tools");
    println!("Available tools: {:?}", tools);
    */
}