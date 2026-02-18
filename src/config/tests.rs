use crate::config::*;
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
    assert_eq!(merged[0].address, "1.2.3.4"); // Field from file preserved
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
