use crate::config::AppConfig;
use crate::tools::FsTools;
use anyhow::Result;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::RwLock;

#[tokio::test]
async fn test_execute_bash_with_permissions_allowed() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let project_root = temp_dir.path().to_path_buf();

    // Create a config with allowed commands
    let cfg = AppConfig {
        project_root: project_root.clone(),
        allowed_commands: vec!["echo".to_string(), "ls".to_string()],
        ..Default::default()
    };

    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

    // This should succeed because "echo" is in the allowed list
    let result = fs_tools.execute_bash("echo 'hello world'").await;
    assert!(result.is_ok());

    Ok(())
}

#[tokio::test]
async fn test_execute_bash_with_permissions_not_allowed() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let project_root = temp_dir.path().to_path_buf();

    // Create a config with allowed commands
    let cfg = AppConfig {
        project_root: project_root.clone(),
        allowed_commands: vec!["echo".to_string(), "ls".to_string()],
        ..Default::default()
    };

    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

    // This should fail because "rm" is not in the allowed list
    let result = fs_tools.execute_bash("rm -rf /").await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not allowed"));

    Ok(())
}

#[tokio::test]
async fn test_execute_bash_with_permissions_no_config() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let project_root = temp_dir.path().to_path_buf();

    // Create a config without allowed commands
    let cfg = AppConfig {
        project_root: project_root.clone(),
        allowed_commands: vec![], // Empty list means all commands are allowed
        ..Default::default()
    };

    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

    // This should be allowed because the allowed_commands list is empty
    let result = fs_tools.execute_bash("echo 'hello world'").await;
    assert!(result.is_ok());

    Ok(())
}

#[tokio::test]
async fn test_is_command_allowed_exact_match() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let project_root = temp_dir.path().to_path_buf();

    // Create a config with allowed commands
    let cfg = AppConfig {
        project_root: project_root.clone(),
        allowed_commands: vec!["cargo".to_string(), "ls".to_string()],
        ..Default::default()
    };

    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

    // Exact match should be allowed
    assert!(fs_tools.is_command_allowed("cargo"));

    Ok(())
}

#[tokio::test]
async fn test_is_command_allowed_prefix_match() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let project_root = temp_dir.path().to_path_buf();

    // Create a config with allowed commands
    let cfg = AppConfig {
        project_root: project_root.clone(),
        allowed_commands: vec!["cargo".to_string(), "ls".to_string()],
        ..Default::default()
    };

    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

    // Prefix match should be allowed
    assert!(fs_tools.is_command_allowed("cargo build"));

    Ok(())
}

#[tokio::test]
async fn test_is_command_allowed_not_allowed() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let project_root = temp_dir.path().to_path_buf();

    // Create a config with allowed commands
    let cfg = AppConfig {
        project_root: project_root.clone(),
        allowed_commands: vec!["cargo".to_string(), "ls".to_string()],
        ..Default::default()
    };

    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

    // Command not in the allowed list should not be allowed
    assert!(!fs_tools.is_command_allowed("rm"));

    Ok(())
}
