use crate::Cli;
use crate::config::{AppConfig, FileConfig, load_project_config};
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

#[test]
fn test_auto_compact_threshold_overrides() {
    let temp_dir = TempDir::new().unwrap();
    let project_root = temp_dir.path();

    let doge_dir = project_root.join(".doge");
    fs::create_dir_all(&doge_dir).unwrap();

    let config_content = r#"
model = "project-model"
auto_compact_prompt_token_threshold = 111

[auto_compact_prompt_token_thresholds]
project-model = 222
other-model = 333
"#;

    fs::write(doge_dir.join("config.toml"), config_content).unwrap();

    let global_config_path = temp_dir.path().join("global.toml");
    fs::write(&global_config_path, "").unwrap();

    let prev_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(project_root).unwrap();

    let old_model = std::env::var("OPENAI_MODEL").ok();
    let old_threshold = std::env::var("DOGE_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD").ok();
    let old_config = std::env::var("DOGE_CODE_CONFIG").ok();

    let set_env = |key: &str, value: &str| unsafe { std::env::set_var(key, value) };
    let remove_env = |key: &str| unsafe { std::env::remove_var(key) };

    remove_env("OPENAI_MODEL");
    set_env("DOGE_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD", "4444");
    set_env("DOGE_CODE_CONFIG", global_config_path.to_str().unwrap());

    let cli = Cli {
        base_url: "".to_string(),
        model: "".to_string(),
        api_key: None,
        no_repomap: false,
        instructions_file: None,
        resume: false,
        command: None,
    };

    let cfg = AppConfig::from_cli(cli).unwrap();

    assert_eq!(cfg.auto_compact_prompt_token_threshold, 4444);
    assert_eq!(
        cfg.auto_compact_prompt_token_threshold_for_model("project-model"),
        222
    );
    assert_eq!(
        cfg.auto_compact_prompt_token_threshold_for_model("other-model"),
        333
    );
    assert_eq!(
        cfg.auto_compact_prompt_token_threshold_for_model("unknown"),
        4444
    );

    if let Some(value) = old_model {
        set_env("OPENAI_MODEL", &value);
    } else {
        remove_env("OPENAI_MODEL");
    }

    if let Some(value) = old_threshold {
        set_env("DOGE_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD", &value);
    } else {
        remove_env("DOGE_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD");
    }

    if let Some(value) = old_config {
        set_env("DOGE_CODE_CONFIG", &value);
    } else {
        remove_env("DOGE_CODE_CONFIG");
    }

    std::env::set_current_dir(prev_dir).unwrap();
}
