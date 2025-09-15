use crate::config::{FileConfig, load_project_config};
use std::fs;
use tempfile::TempDir;

#[test]
fn test_load_project_config() {
    let temp_dir = TempDir::new().unwrap();
    let project_root = temp_dir.path();

    // Create .doge directory and config file
    let doge_dir = project_root.join(".doge");
    fs::create_dir_all(&doge_dir).unwrap();

    let config_content = r#"
model = "gpt-4o"
theme = "light"
show_diff = false

[llm]
max_retries = 5
retry_base_ms = 500
"#;

    fs::write(doge_dir.join("config.toml"), config_content).unwrap();

    // Load project config
    let project_cfg = load_project_config(project_root).unwrap();

    // Verify the config was loaded correctly
    assert_eq!(project_cfg.model, Some("gpt-4o".to_string()));
    assert_eq!(project_cfg.theme, Some("light".to_string()));
    assert_eq!(project_cfg.show_diff, Some(false));

    // Verify LLM config
    assert!(project_cfg.llm.is_some());
    let llm_cfg = project_cfg.llm.unwrap();
    assert_eq!(llm_cfg.max_retries, Some(5));
    assert_eq!(llm_cfg.retry_base_ms, Some(500));
}

#[test]
fn test_load_project_config_not_exists() {
    let temp_dir = TempDir::new().unwrap();
    let project_root = temp_dir.path();

    // Load project config when no config file exists
    let project_cfg = load_project_config(project_root).unwrap();

    // Should return default config
    assert_eq!(project_cfg, FileConfig::default());
}
