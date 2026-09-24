use crate::config::*;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_llm_config_apply_partial() {
    let mut config = LlmConfig::default();
    let partial = PartialLlmConfig {
        connect_timeout_ms: Some(999),
        max_retries: Some(5),
        ..Default::default()
    };
    config.apply_partial(&partial);
    assert_eq!(config.connect_timeout_ms, 999);
    assert_eq!(config.max_retries, 5);
    // Other fields should remain default
    assert_eq!(
        config.request_timeout_ms,
        LlmConfig::default().request_timeout_ms
    );
}

#[test]
fn test_watch_config_apply_partial() {
    let mut config = WatchConfig::default();
    let partial = PartialWatchConfig {
        debounce_delay_ms: Some(123),
        backup_enabled: Some(false),
        ..Default::default()
    };
    config.apply_partial(&partial);
    assert_eq!(config.debounce_delay_ms, Some(123));
    assert!(!config.backup_enabled.unwrap_or(true));
}

#[test]
fn test_mcp_servers_merge_logic() {
    let file_servers = vec![PartialMcpServerConfig {
        name: Some("server1".to_string()),
        enabled: Some(true),
        address: Some("1.2.3.4".to_string()),
        ..Default::default()
    }];
    let project_servers = vec![PartialMcpServerConfig {
        name: Some("server1".to_string()),
        enabled: Some(false),
        ..Default::default()
    }];

    let merged = merge_mcp_servers(Some(&file_servers), Some(&project_servers));
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].name, "server1");
    assert!(!merged[0].enabled); // Project overrides file
    assert_eq!(merged[0].address.as_deref(), Some("1.2.3.4")); // Field from file preserved
}

#[test]
fn test_mcp_structured_stdio_merge_preserves_argv_and_env_precedence() {
    let global = PartialMcpServerConfig {
        name: Some("filesystem".to_string()),
        enabled: Some(true),
        transport: Some(McpTransport::Stdio),
        command: Some("/tmp/foo server".to_string()),
        args: Some(vec![
            "--config".to_string(),
            "/tmp/foo config.json".to_string(),
        ]),
        env: Some(BTreeMap::from([
            ("DOGE_GLOBAL".to_string(), "yes".to_string()),
            ("DOGE_OVERRIDE".to_string(), "global".to_string()),
        ])),
        connect_timeout_ms: Some(11_000),
        list_timeout_ms: Some(12_000),
        call_timeout_ms: Some(13_000),
        ..Default::default()
    };
    let project = PartialMcpServerConfig {
        name: Some("filesystem".to_string()),
        args: Some(vec![
            "--project".to_string(),
            "value with spaces".to_string(),
        ]),
        env: Some(BTreeMap::from([(
            "DOGE_OVERRIDE".to_string(),
            "project".to_string(),
        )])),
        call_timeout_ms: Some(0),
        ..Default::default()
    };

    let global_servers = vec![global];
    let project_servers = vec![project];
    let merged = merge_mcp_servers(Some(&global_servers), Some(&project_servers));
    assert_eq!(merged.len(), 1);
    let server = &merged[0];
    assert_eq!(server.transport, McpTransport::Stdio);
    assert_eq!(server.command.as_deref(), Some("/tmp/foo server"));
    assert_eq!(server.args, vec!["--project", "value with spaces"]);
    assert_eq!(server.address, None);
    assert_eq!(
        server.env.get("DOGE_GLOBAL").map(String::as_str),
        Some("yes")
    );
    assert_eq!(
        server.env.get("DOGE_OVERRIDE").map(String::as_str),
        Some("project")
    );
    assert_eq!(server.connect_timeout_ms, 11_000);
    assert_eq!(server.list_timeout_ms, 12_000);
    assert_eq!(server.call_timeout_ms, 0);
    server.validate().expect("merged structured stdio config");
}

#[test]
fn test_mcp_project_structured_command_does_not_inherit_http_address() {
    let global = vec![PartialMcpServerConfig {
        name: Some("switch".to_string()),
        enabled: Some(true),
        transport: Some(McpTransport::Http),
        address: Some("http://127.0.0.1:8000/mcp".to_string()),
        ..Default::default()
    }];
    let project = vec![PartialMcpServerConfig {
        name: Some("switch".to_string()),
        command: Some("/tmp/mcp server".to_string()),
        ..Default::default()
    }];

    let merged = merge_mcp_servers(Some(&global), Some(&project));
    let server = &merged[0];
    assert_eq!(server.transport, McpTransport::Stdio);
    assert_eq!(server.command.as_deref(), Some("/tmp/mcp server"));
    assert_eq!(server.address, None);
    server
        .validate()
        .expect("structured stdio override should validate");
}

#[test]
fn test_mcp_project_http_switch_drops_inherited_stdio_fields() {
    let global = vec![PartialMcpServerConfig {
        name: Some("switch".to_string()),
        enabled: Some(true),
        transport: Some(McpTransport::Stdio),
        address: Some("legacy-server --flag".to_string()),
        command: Some("legacy-server".to_string()),
        args: Some(vec!["--flag".to_string()]),
        ..Default::default()
    }];
    let project = vec![PartialMcpServerConfig {
        name: Some("switch".to_string()),
        transport: Some(McpTransport::Http),
        ..Default::default()
    }];

    let merged = merge_mcp_servers(Some(&global), Some(&project));
    let server = &merged[0];
    assert_eq!(server.transport, McpTransport::Http);
    assert_eq!(server.address.as_deref(), Some("legacy-server --flag"));
    assert_eq!(server.command, None);
    assert!(server.args.is_empty());
    server
        .validate()
        .expect_err("legacy command text is not a valid HTTP URL");
}

#[test]
fn test_mcp_legacy_stdio_address_remains_valid() {
    let parsed: FileConfig = toml::from_str(
        r#"
        [[mcp_servers]]
        name = "legacy"
        enabled = true
        transport = "stdio"
        address = "server --foo bar"
        "#,
    )
    .expect("legacy MCP config should parse");
    let merged = merge_mcp_servers(parsed.mcp_servers.as_ref(), None);
    assert_eq!(merged[0].address.as_deref(), Some("server --foo bar"));
    assert!(merged[0].validate().is_ok());
}

#[test]
fn test_mcp_servers_no_duplicates() {
    let file_servers = vec![PartialMcpServerConfig {
        name: Some("server1".to_string()),
        ..Default::default()
    }];
    let project_servers = vec![PartialMcpServerConfig {
        name: Some("server2".to_string()),
        ..Default::default()
    }];

    let merged = merge_mcp_servers(Some(&file_servers), Some(&project_servers));
    assert_eq!(merged.len(), 2);
}

#[test]
fn test_local_mcp_config_defaults() {
    let merged = merge_local_mcp_server(None, None);
    assert!(!merged.enabled);
    assert_eq!(merged.address, "127.0.0.1:8000");
}

#[test]
fn test_local_mcp_config_project_overrides_global() {
    let global = PartialLocalMcpServerConfig {
        enabled: Some(true),
        address: Some("127.0.0.1:8001".to_string()),
    };
    let project = PartialLocalMcpServerConfig {
        enabled: None,
        address: Some("127.0.0.1:9000".to_string()),
    };

    let merged = merge_local_mcp_server(Some(&global), Some(&project));
    assert!(merged.enabled);
    assert_eq!(merged.address, "127.0.0.1:9000");
}

#[test]
fn test_local_mcp_config_parses_toml() {
    let toml_str = r#"
        [mcp_server]
        enabled = true
        address = "127.0.0.1:8001"

        [[mcp_servers]]
        name = "remote"
        enabled = true
        address = "http://example.com/mcp"
        transport = "http"
    "#;
    let cfg: FileConfig = toml::from_str(toml_str).expect("parse local mcp config");
    let local = cfg.mcp_server.expect("mcp_server section");
    assert_eq!(local.enabled, Some(true));
    assert_eq!(local.address.as_deref(), Some("127.0.0.1:8001"));
    let remotes = cfg.mcp_servers.expect("mcp_servers section");
    assert_eq!(remotes.len(), 1);
    assert_eq!(remotes[0].name.as_deref(), Some("remote"));
}

#[test]
fn test_load_file_config_creates_default() {
    let temp_dir = TempDir::new().unwrap();
    let original_home = env::var("HOME").ok();
    let original_xdg_config_home = env::var("XDG_CONFIG_HOME").ok();

    // Set up test environment
    unsafe {
        std::env::set_var("HOME", temp_dir.path());
        let config_home = temp_dir.path().join(".config");
        std::env::set_var("XDG_CONFIG_HOME", &config_home);
    }

    // Remove any existing config file to test creation
    let config_path = temp_dir
        .path()
        .join(".config")
        .join("doge-code")
        .join("config.toml");

    // Load config (this should create the file)
    let result = load_file_config();
    assert!(result.is_ok());

    // Check that the config file was created
    assert!(
        config_path.exists(),
        "Config file should be created at {:?}",
        config_path
    );

    // Check that the file contains content
    let content = fs::read_to_string(&config_path).unwrap();
    assert!(!content.is_empty(), "Config file should not be empty");
    assert!(
        content.contains("# Doge-Code Configuration"),
        "Config should contain comment header"
    );

    // Restore original environment
    unsafe {
        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(xdg_home) = original_xdg_config_home {
            std::env::set_var("XDG_CONFIG_HOME", xdg_home);
        } else {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }
}

#[test]
fn test_execution_merge_project_wins() {
    let file = PartialExecutionConfig {
        mode: Some(ExecutionMode::Allowlist),
        allowed_programs: Some(vec!["cargo".to_string()]),
        allow_shell: Some(false),
        allowed_env: None,
    };
    let project = PartialExecutionConfig {
        mode: None,
        allowed_programs: Some(vec!["cargo".to_string(), "git".to_string()]),
        allow_shell: None,
        allowed_env: Some(vec!["RUST_BACKTRACE".to_string()]),
    };
    let merged = merge_execution(Some(&file), Some(&project)).expect("merged");
    // Project list replaces global list (documented precedence).
    assert_eq!(
        merged.allowed_programs,
        Some(vec!["cargo".to_string(), "git".to_string()])
    );
    assert_eq!(merged.mode, Some(ExecutionMode::Allowlist));
    assert_eq!(merged.allow_shell, Some(false));

    // Resolved config applies the merge.
    let mut cfg = ExecutionConfig::default();
    cfg.apply_partial(&merged);
    assert_eq!(cfg.mode, ExecutionMode::Allowlist);
    assert_eq!(
        cfg.allowed_programs,
        vec!["cargo".to_string(), "git".to_string()]
    );
    assert!(!cfg.allow_shell);
}

#[test]
fn test_execution_config_parses_toml() {
    let toml_str = r#"
        [execution]
        mode = "allowlist"
        allowed_programs = ["cargo", "git"]
        allow_shell = false
        allowed_env = ["RUST_BACKTRACE"]
    "#;
    let cfg: FileConfig = toml::from_str(toml_str).expect("parse execution config");
    let exec = cfg.execution.expect("execution section");
    assert_eq!(exec.mode, Some(ExecutionMode::Allowlist));
    assert_eq!(
        exec.allowed_programs,
        Some(vec!["cargo".to_string(), "git".to_string()])
    );
    assert_eq!(exec.allow_shell, Some(false));
}

#[test]
fn test_file_config_ignores_unknown_fields() {
    let toml_str = r#"
        [llm]
        connect_timeout_ms = 1000

        # These fields are removed config
        [verification]
        enabled = true

        [test_fix]
        max_iterations = 5
    "#;

    let config: Result<FileConfig, _> = toml::from_str(toml_str);
    assert!(
        config.is_ok(),
        "Should parse successfully confirming unknown fields are ignored"
    );
    let config = config.unwrap();
    assert_eq!(config.llm.unwrap().connect_timeout_ms, Some(1000));
}
