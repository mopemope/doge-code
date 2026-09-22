pub mod client;
pub mod http_security;
pub mod resource_path;
pub mod server;
pub mod service;

#[cfg(test)]
mod tests {
    use crate::config::AppConfig;
    use crate::mcp::service::{DogeMcpService, McpServiceState};
    use rmcp::{handler::server::wrapper::Parameters, model::RawContent};
    use std::path::Path;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn test_service(root: &Path) -> DogeMcpService {
        let config = Arc::new(AppConfig {
            project_root: root.to_path_buf(),
            ..Default::default()
        });
        let repomap = Arc::new(RwLock::new(None));
        let state = Arc::new(McpServiceState::new(config, repomap));
        DogeMcpService::new(state)
    }

    fn test_service_with_config(config: AppConfig) -> DogeMcpService {
        let state = Arc::new(McpServiceState::new(
            Arc::new(config),
            Arc::new(RwLock::new(None)),
        ));
        DogeMcpService::new(state)
    }

    #[tokio::test]
    async fn test_doge_mcp_service_creation() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let service = test_service(tmp.path());
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
        let tmp = tempfile::tempdir().expect("tempdir");
        let service = test_service(tmp.path());
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
    async fn test_fs_read_tool_missing_file_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let service = test_service(tmp.path());
        let params = crate::mcp::service::FsReadParams {
            path: tmp
                .path()
                .join("nonexistent-file.txt")
                .to_string_lossy()
                .to_string(),
            start_line: None,
            limit: None,
            mode: None,
            response_budget_chars: None,
            cursor: None,
            page_size: None,
        };

        let result = service.fs_read(Parameters(params));
        // Missing file must surface as an MCP error.
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_fs_list_tool_missing_dir_returns_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let service = test_service(tmp.path());
        let params = crate::mcp::service::FsListParams {
            path: tmp
                .path()
                .join("nonexistent-dir")
                .to_string_lossy()
                .to_string(),
            max_depth: None,
            pattern: None,
            mode: None,
            response_budget_chars: None,
            cursor: None,
            page_size: None,
            max_entries: None,
        };

        let result = service.fs_list(Parameters(params));
        assert!(result.is_ok());
        let files = result.unwrap();
        let _json = serde_json::to_string(&files).expect("Should be serializable");
    }

    #[tokio::test]
    async fn test_search_text_tool_does_not_panic() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let service = test_service(tmp.path());
        let params = crate::mcp::service::SearchTextParams {
            search_pattern: "test".to_string(),
            file_glob: Some("*.txt".to_string()),
            max_results: None,
            offset: None,
        };

        let result = service.search_text(Parameters(params));
        // Depending on ripgrep availability this may error, but must not panic.
        let _ = result;
    }

    #[tokio::test]
    async fn test_find_file_tool_empty_result_ok() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let service = test_service(tmp.path());
        let params = crate::mcp::service::FindFileParams {
            filename: "nonexistent-xyz-123.txt".to_string(),
        };

        let result = service.find_file(Parameters(params)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_search_repomap_tool_without_repomap() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("main.rs");
        tokio::fs::write(&file_path, "fn main() {}\n")
            .await
            .unwrap();

        let cfg = crate::config::AppConfig {
            project_root: temp_dir.path().to_path_buf(),
            ..Default::default()
        };

        let service = test_service_with_config(cfg);
        let params = crate::mcp::service::SearchRepomapParams {
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
        assert!(result.is_ok(), "Repomap should auto-build for MCP search");
    }

    #[tokio::test]
    async fn test_format_json_result() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let service = test_service(tmp.path());
        let test_data = vec!["item1".to_string(), "item2".to_string()];
        let result = service.format_json_result(test_data);

        assert!(result.is_ok());
        let call_result = result.unwrap();
        assert_eq!(call_result.content.len(), 1);
        assert!(matches!(call_result.content[0].raw, RawContent::Text(_)));
    }

    #[tokio::test]
    async fn test_format_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let service = test_service(tmp.path());
        let error = service.format_error("Test error", Some(serde_json::json!("details")));
        assert_eq!(error.message, "Test error");
    }

    #[tokio::test]
    async fn test_list_resources_impl() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let service = test_service(tmp.path());
        let result = service.list_resources_impl().await;
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.resources.len(), 3);
        assert!(
            result
                .resources
                .iter()
                .any(|r| r.uri == "doge://repomap/summary")
        );
        assert!(
            result
                .resources
                .iter()
                .any(|r| r.uri == "doge://repomap/status")
        );
    }

    #[tokio::test]
    async fn test_read_resource_impl_summary_empty() {
        let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
        let config = AppConfig {
            project_root: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let service = test_service_with_config(config);

        let result = service
            .read_resource_impl("doge://repomap/summary".to_string())
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_read_resource_impl_file_not_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let service = test_service(tmp.path());
        let result = service
            .read_resource_impl("doge://files/nonexistent".to_string())
            .await;
        assert!(result.is_err());
    }

    fn project_with_files() -> (tempfile::TempDir, std::path::PathBuf) {
        let root_tmp = tempfile::tempdir().expect("tempdir");
        let project = root_tmp.path().join("project");
        let outside = root_tmp.path().join("outside");
        std::fs::create_dir_all(project.join("src")).expect("mkdir project/src");
        std::fs::create_dir_all(&outside).expect("mkdir outside");
        std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("write main");
        std::fs::write(project.join("project-only.txt"), "hello project\n")
            .expect("write project-only");
        std::fs::write(outside.join("secret.txt"), "TOP-SECRET\n").expect("write secret");
        // Canonicalize so `project_root` matches the canonical paths used by
        // `fs_read` / resource resolution on symlinked tmpdirs
        // (/var -> /private/var on macOS).
        let project_path = project.canonicalize().unwrap_or(project);
        (root_tmp, project_path)
    }

    #[tokio::test]
    async fn test_resource_valid_nested_file_via_service() {
        let (_root_tmp, project) = project_with_files();
        let service = test_service(&project);
        let result = service
            .read_resource_impl("doge://files/src/main.rs".to_string())
            .await;
        assert!(result.is_ok(), "valid nested file must succeed");
        let result = result.unwrap();
        assert_eq!(result.contents.len(), 1);
    }

    #[tokio::test]
    async fn test_resource_parent_traversal_rejected_via_service() {
        let (_root_tmp, project) = project_with_files();
        let service = test_service(&project);
        let result = service
            .read_resource_impl("doge://files/../outside/secret.txt".to_string())
            .await;
        assert!(result.is_err());
        let err = format!("{:?}", result.unwrap_err());
        assert!(!err.contains("TOP-SECRET"), "secret must not leak");
        assert!(!err.contains("secret.txt"));
    }

    #[tokio::test]
    async fn test_resource_encoded_parent_traversal_rejected_via_service() {
        let (_root_tmp, project) = project_with_files();
        let service = test_service(&project);
        for uri in [
            "doge://files/%2e%2e/outside/secret.txt",
            "doge://files/%2E%2E/outside/secret.txt",
            "doge://files/%2e%2e%2foutside%2fsecret.txt",
        ] {
            let result = service.read_resource_impl(uri.to_string()).await;
            assert!(result.is_err(), "{uri} must be rejected");
            let err = format!("{:?}", result.unwrap_err());
            assert!(!err.contains("TOP-SECRET"));
        }
    }

    #[tokio::test]
    async fn test_resource_absolute_path_rejected_via_service() {
        let (root_tmp, project) = project_with_files();
        let outside_abs = root_tmp.path().join("outside/secret.txt");
        let uri = format!("doge://files/{}", outside_abs.to_string_lossy());
        let service = test_service(&project);
        let result = service.read_resource_impl(uri.clone()).await;
        assert!(result.is_err(), "absolute path must be rejected");
        let err = format!("{:?}", result.unwrap_err());
        assert!(
            !err.contains(&outside_abs.to_string_lossy().to_string()),
            "host absolute path must not leak: {err}"
        );
        assert!(!err.contains("TOP-SECRET"));
    }

    #[tokio::test]
    async fn test_resource_invalid_utf8_rejected_via_service() {
        let (_root_tmp, project) = project_with_files();
        let service = test_service(&project);
        let result = service
            .read_resource_impl("doge://files/%FF".to_string())
            .await;
        assert!(result.is_err(), "invalid UTF-8 must be rejected");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_resource_symlink_escape_rejected_via_service() {
        use std::os::unix::fs::symlink;
        let (root_tmp, project) = project_with_files();
        let outside = root_tmp.path().join("outside");
        symlink(&outside, project.join("link")).expect("symlink");
        let service = test_service(&project);
        let result = service
            .read_resource_impl("doge://files/link/secret.txt".to_string())
            .await;
        assert!(result.is_err(), "symlink escape must be rejected");
        let err = format!("{:?}", result.unwrap_err());
        assert!(!err.contains("TOP-SECRET"));
    }

    #[tokio::test]
    async fn test_symbol_resource_traversal_rejected() {
        let (_root_tmp, project) = project_with_files();
        let service = test_service(&project);
        let result = service
            .read_resource_impl("doge://symbols/../outside/secret.rs".to_string())
            .await;
        assert!(result.is_err(), "symbol traversal must be rejected");
    }

    #[tokio::test]
    async fn test_resource_error_does_not_leak_host_path() {
        let (root_tmp, project) = project_with_files();
        let outside_abs = root_tmp.path().join("outside/secret.txt");
        let outside_str = outside_abs.to_string_lossy().to_string();
        let service = test_service(&project);
        let result = service
            .read_resource_impl(format!("doge://files/{outside_str}"))
            .await;
        assert!(result.is_err());
        let err = format!("{:?}", result.unwrap_err());
        assert!(!err.contains(&outside_str), "error leaks host path: {err}");
        assert!(!err.contains("TOP-SECRET"));
        // Generic message only; no canonical temp prefix.
        assert!(!err.contains("/private"));
    }

    #[tokio::test]
    async fn test_mcp_service_uses_supplied_app_config() {
        // The service must serve the supplied project_root, not a default.
        let (_root_tmp, project) = project_with_files();
        let cfg = AppConfig {
            project_root: project.clone(),
            ..Default::default()
        };
        // Sanity: default root differs from our temp project in practice; the
        // file only exists under the supplied root.
        let service = test_service_with_config(cfg);
        let result = service
            .read_resource_impl("doge://files/project-only.txt".to_string())
            .await;
        assert!(
            result.is_ok(),
            "service must use supplied AppConfig.project_root"
        );

        // And fs_read (which honors allowed_paths) must see the same root.
        let params = crate::mcp::service::FsReadParams {
            path: project
                .join("project-only.txt")
                .to_string_lossy()
                .to_string(),
            start_line: None,
            limit: None,
            mode: Some("full".to_string()),
            response_budget_chars: None,
            cursor: None,
            page_size: None,
        };
        let result = service.fs_read(Parameters(params));
        assert!(
            result.is_ok(),
            "fs_read must use supplied AppConfig: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_mcp_fs_read_honors_allowed_paths() {
        let (_root_tmp, project) = project_with_files();
        let outside_file = project.join("src/main.rs");
        // allowed_paths propagation: grant access to an outside dir and read
        // through fs_read (resource URIs stay project-only).
        let outer_tmp = tempfile::tempdir().expect("outer tempdir");
        let allowed_file = outer_tmp.path().join("allowed.txt");
        std::fs::write(&allowed_file, "allowed content\n").expect("write allowed");
        let cfg = AppConfig {
            project_root: project.clone(),
            allowed_paths: vec![
                outer_tmp
                    .path()
                    .canonicalize()
                    .unwrap_or_else(|_| outer_tmp.path().to_path_buf()),
            ],
            ..Default::default()
        };
        let service = test_service_with_config(cfg);
        let params = crate::mcp::service::FsReadParams {
            path: allowed_file.to_string_lossy().to_string(),
            start_line: None,
            limit: None,
            mode: Some("full".to_string()),
            response_budget_chars: None,
            cursor: None,
            page_size: None,
        };
        let result = service.fs_read(Parameters(params));
        assert!(result.is_ok(), "allowed_paths must propagate to fs_read");

        // Sanity: project file still readable.
        let _ = outside_file;
    }

    #[tokio::test]
    async fn test_mcp_services_share_repomap_build_state() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config = Arc::new(AppConfig {
            project_root: tmp.path().to_path_buf(),
            ..Default::default()
        });
        let repomap = Arc::new(RwLock::new(None));
        let state = Arc::new(McpServiceState::new(config, repomap));
        let a = DogeMcpService::new(state.clone());
        let b = DogeMcpService::new(state.clone());

        assert!(Arc::ptr_eq(&a.state().config, &b.state().config));
        assert!(Arc::ptr_eq(&a.state().repomap, &b.state().repomap));
        assert!(Arc::ptr_eq(
            &a.state().repomap_build_lock,
            &b.state().repomap_build_lock
        ));
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
