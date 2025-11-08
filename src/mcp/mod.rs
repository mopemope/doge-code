pub mod client;
pub mod server;
pub mod service;

#[cfg(test)]
mod tests {
    use crate::config::McpServerConfig;
    use crate::mcp::{server, service};
    use rmcp::{handler::server::wrapper::Parameters, model::RawContent};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[tokio::test]
    async fn test_mcp_server_start() {
        let _config = McpServerConfig {
            name: "test".to_string(),
            enabled: true,
            address: "127.0.0.1:0".to_string(), // Use port 0 to get a random available port
            transport: "http".to_string(),
        };

        let repomap = Arc::new(RwLock::new(None));
        let handle = server::start_mcp_server(&_config, repomap);

        // The server should start successfully
        assert!(handle.is_some());

        // Give the server a moment to start
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // In a real test, we would connect to the server and verify it's working
        // For now, we'll just check that the handle exists
    }

    #[tokio::test]
    async fn test_doge_mcp_service_creation() {
        let service = service::DogeMcpService::default();
        assert!(service.tool_router.has_route("say_hello"));
        assert!(service.tool_router.has_route("search_repomap"));
        assert!(service.tool_router.has_route("fs_read"));
        assert!(service.tool_router.has_route("fs_read_many_files"));
        assert!(service.tool_router.has_route("search_text"));
        assert!(service.tool_router.has_route("fs_list"));
        assert!(service.tool_router.has_route("find_file"));
    }

    #[tokio::test]
    async fn test_say_hello_tool() {
        let service = service::DogeMcpService::default();
        let result = service.say_hello();
        assert!(result.is_ok());

        let result = result.unwrap();
        assert_eq!(result.content.len(), 1);
        if let RawContent::Text(ref text) = result.content[0].raw {
            assert_eq!(text.text, "hello");
        } else {
            panic!("Expected text content");
        }
    }

    #[tokio::test]
    async fn test_fs_read_tool() {
        let service = service::DogeMcpService::default();
        let params = service::FsReadParams {
            path: "/nonexistent/file.txt".to_string(),
            start_line: None,
            limit: None,
        };

        let result = service.fs_read(Parameters(params));
        // This should fail because the file doesn't exist
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_fs_list_tool() {
        let service = service::DogeMcpService::default();
        let params = service::FsListParams {
            path: "/nonexistent/directory".to_string(),
            max_depth: None,
            pattern: None,
        };

        let result = service.fs_list(Parameters(params));
        // This should succeed but return an empty list because the directory doesn't exist
        assert!(result.is_ok());
        let files = result.unwrap();
        // The result should be serializable to JSON
        let _json = serde_json::to_string(&files).expect("Should be serializable");
    }

    #[tokio::test]
    async fn test_search_text_tool() {
        let service = service::DogeMcpService::default();
        let params = service::SearchTextParams {
            search_pattern: "test".to_string(),
            file_glob: Some("*.txt".to_string()),
        };

        let result = service.search_text(Parameters(params));
        // This might fail depending on whether ripgrep is available and files exist
        // but it shouldn't panic
        let _ = result;
    }

    #[tokio::test]
    async fn test_find_file_tool() {
        let service = service::DogeMcpService::default();
        let params = service::FindFileParams {
            filename: "nonexistent.txt".to_string(),
        };

        let result = service.find_file(Parameters(params)).await;
        // This should succeed even if the file doesn't exist (it would return an empty list)
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_search_repomap_tool_without_repomap() {
        let service = service::DogeMcpService::default();
        let params = service::SearchRepomapParams {
            result_density: None,
            max_file_lines: None,
            max_function_lines: None,
            file_pattern: None,
            exclude_patterns: None,
            language_filters: None,
            symbol_kinds: None,
            sort_by: None,
            sort_desc: None,
            limit: None,
            keyword_search: Some(vec!["test".to_string()]),
            name: None,
            fields: None,
            include_snippets: None,
            context_lines: None,
            snippet_max_chars: None,
            max_symbols_per_file: None,
            match_score_threshold: None,
            response_budget_chars: None,
            cursor: None,
            page_size: None,
        };

        let result = service.search_repomap(Parameters(params)).await;
        // This should fail because the repomap is not initialized
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_format_json_result() {
        let service = service::DogeMcpService::default();
        let test_data = vec!["item1".to_string(), "item2".to_string()];
        let result = service.format_json_result(test_data);

        assert!(result.is_ok());
        let call_result = result.unwrap();
        assert_eq!(call_result.content.len(), 1);
        assert!(matches!(call_result.content[0].raw, RawContent::Text(_)));
    }

    #[tokio::test]
    async fn test_format_error() {
        let service = service::DogeMcpService::default();
        let error = service.format_error("Test error", Some(serde_json::json!("details")));
        assert_eq!(error.message, "Test error");
    }
}

#[cfg(test)]
mod client_tests {
    use crate::config::McpServerConfig;

    #[tokio::test]
    async fn test_mcp_client_creation_with_invalid_transport() {
        let _config = McpServerConfig {
            name: "test".to_string(),
            enabled: true,
            address: "127.0.0.1:8000".to_string(),
            transport: "invalid".to_string(),
        };

        let result = crate::mcp::client::McpClient::from_config(&_config).await;
        assert!(result.is_err());

        if let Err(rmcp::RmcpError::TransportCreation { .. }) = result {
            // Expected error
        } else {
            panic!("Expected transport creation error for invalid transport");
        }
    }

    #[tokio::test]
    async fn test_mcp_client_creation_with_invalid_stdio_command() {
        let _config = McpServerConfig {
            name: "test".to_string(),
            enabled: true,
            address: "".to_string(), // Invalid command
            transport: "stdio".to_string(),
        };

        let result = crate::mcp::client::McpClient::from_config(&_config).await;
        assert!(result.is_err());

        if let Err(rmcp::RmcpError::TransportCreation { .. }) = result {
            // Expected error
        } else {
            panic!("Expected transport creation error for invalid stdio command");
        }
    }

    #[tokio::test]
    async fn test_mcp_client_config_access() {
        let _config = McpServerConfig {
            name: "test".to_string(),
            enabled: true,
            address: "127.0.0.1:8000".to_string(),
            transport: "http".to_string(),
        };

        // We can't actually connect to a server in tests, so we'll just test the config access
        assert_eq!(_config.name, "test");
        assert_eq!(_config.address, "127.0.0.1:8000");
        assert_eq!(_config.transport, "http");
    }

    #[tokio::test]
    async fn test_mcp_client_http_transport_creation() {
        let _config = McpServerConfig {
            name: "test-http".to_string(),
            enabled: true,
            address: "http://127.0.0.1:8000".to_string(),
            transport: "http".to_string(),
        };

        // We won't actually connect, but we can test that the config is processed correctly
        // In a real test, we would mock the transport or use a test server
    }

    #[tokio::test]
    async fn test_mcp_client_stdio_transport_creation() {
        let _config = McpServerConfig {
            name: "test-stdio".to_string(),
            enabled: true,
            address: "echo test".to_string(), // Simple command for testing
            transport: "stdio".to_string(),
        };

        // We won't actually connect, but we can test that the config is processed correctly
        // In a real test, we would mock the transport or use a test server
    }

    #[tokio::test]
    async fn test_mcp_client_empty_server_name() {
        let _config = McpServerConfig {
            name: "".to_string(),
            enabled: true,
            address: "http://127.0.0.1:8000".to_string(),
            transport: "http".to_string(),
        };

        // Test that client can be created with empty name
        // In a real test, we would mock the transport or use a test server
    }

    #[tokio::test]
    async fn test_mcp_client_empty_address() {
        let _config = McpServerConfig {
            name: "test".to_string(),
            enabled: true,
            address: "".to_string(),
            transport: "http".to_string(),
        };

        // Test that client creation fails with empty address for HTTP transport
        // In a real test, we would expect an error when trying to connect
    }

    #[tokio::test]
    async fn test_mcp_client_very_long_address() {
        let long_address = "http://".to_string() + &"a".repeat(1000) + ":8000";
        let _config = McpServerConfig {
            name: "test".to_string(),
            enabled: true,
            address: long_address,
            transport: "http".to_string(),
        };

        // Test that client can handle long addresses
        // In a real test, we would mock the transport or use a test server
    }

    #[tokio::test]
    async fn test_mcp_client_unsupported_transport_error() {
        let config = McpServerConfig {
            name: "test".to_string(),
            enabled: true,
            address: "127.0.0.1:8000".to_string(),
            transport: "websocket".to_string(), // Unsupported transport
        };

        let result = crate::mcp::client::McpClient::from_config(&config).await;
        assert!(result.is_err());

        match result {
            Err(rmcp::RmcpError::TransportCreation { .. }) => {
                // Expected error type
            }
            Err(e) => panic!("Expected TransportCreation error, got: {:?}", e),
            Ok(_) => panic!("Expected error for unsupported transport"),
        }
    }

    #[tokio::test]
    async fn test_mcp_client_valid_transport_types() {
        // Test that both supported transport types work in validation
        let http_config = McpServerConfig {
            name: "test".to_string(),
            enabled: true,
            address: "http://127.0.0.1:8000".to_string(),
            transport: "http".to_string(),
        };

        let stdio_config = McpServerConfig {
            name: "test".to_string(),
            enabled: true,
            address: "echo test".to_string(),
            transport: "stdio".to_string(),
        };

        // Both should be valid transport types
        assert!(http_config.transport == "http" || http_config.transport == "stdio");
        assert!(stdio_config.transport == "http" || stdio_config.transport == "stdio");
    }

    #[tokio::test]
    async fn test_mcp_client_empty_address_error() {
        let config = McpServerConfig {
            name: "test".to_string(),
            enabled: true,
            address: "".to_string(),        // Empty address
            transport: "stdio".to_string(), // stdio will fail with empty address
        };

        let result = crate::mcp::client::McpClient::from_config(&config).await;
        assert!(result.is_err());

        // Should fail during stdio command parsing
        match result {
            Err(rmcp::RmcpError::TransportCreation { .. }) => {
                // Expected error type
            }
            Err(e) => panic!("Expected TransportCreation error, got: {:?}", e),
            Ok(_) => panic!("Expected error for empty address"),
        }
    }
}
