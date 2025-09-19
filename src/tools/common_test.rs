use crate::config::AppConfig;
use crate::tools::FsTools;
use crate::tools::execute;
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

    // This should return a JSON string with success = false because "rm" is not in the allowed list
    let result_str = fs_tools.execute_bash("rm -rf /").await.unwrap();
    let result: execute::ExecuteBashResult = serde_json::from_str(&result_str).unwrap();
    assert!(!result.success);
    assert!(result.stderr.contains("not allowed"));

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

// Additional tests for edge cases in allowed_commands functionality
#[tokio::test]
async fn test_is_command_allowed_partial_match_edge_case() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let project_root = temp_dir.path().to_path_buf();

    // Create a config with allowed commands
    let cfg = AppConfig {
        project_root: project_root.clone(),
        allowed_commands: vec!["cargo".to_string(), "ls".to_string()],
        ..Default::default()
    };

    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

    // "carg" should not match "cargo" (partial match without space should not be allowed)
    assert!(!fs_tools.is_command_allowed("carg"));

    // "cargox" should not match "cargo" (extra characters without space should not be allowed)
    assert!(!fs_tools.is_command_allowed("cargox"));

    Ok(())
}

#[tokio::test]
async fn test_is_command_allowed_space_separation() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let project_root = temp_dir.path().to_path_buf();

    // Create a config with allowed commands
    let cfg = AppConfig {
        project_root: project_root.clone(),
        allowed_commands: vec!["git".to_string(), "ls".to_string()],
        ..Default::default()
    };

    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

    // Valid commands with proper space separation should be allowed
    assert!(fs_tools.is_command_allowed("git status"));
    assert!(fs_tools.is_command_allowed("ls -la"));

    // Commands with no space after should not be allowed
    assert!(!fs_tools.is_command_allowed("gitstatus"));

    Ok(())
}

#[tokio::test]
async fn test_is_command_allowed_complex_commands() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let project_root = temp_dir.path().to_path_buf();

    // Create a config with complex allowed commands
    let cfg = AppConfig {
        project_root: project_root.clone(),
        allowed_commands: vec!["cargo build".to_string(), "git status".to_string()],
        ..Default::default()
    };

    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

    // Commands matching the specific allowed commands should be allowed
    assert!(fs_tools.is_command_allowed("cargo build"));
    assert!(fs_tools.is_command_allowed("git status"));

    // Different commands should not be allowed
    assert!(!fs_tools.is_command_allowed("cargo test"));
    assert!(!fs_tools.is_command_allowed("git commit"));

    Ok(())
}

#[tokio::test]
async fn test_execute_bash_complex_allowed_command() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let project_root = temp_dir.path().to_path_buf();

    // Create a config with a complex allowed command
    let cfg = AppConfig {
        project_root: project_root.clone(),
        allowed_commands: vec!["echo 'hello world'".to_string()],
        ..Default::default()
    };

    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

    // This should succeed because the exact command is allowed
    let result = fs_tools.execute_bash("echo 'hello world'").await;
    assert!(result.is_ok());

    Ok(())
}

#[tokio::test]
async fn test_execute_bash_with_empty_allowed_commands() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let project_root = temp_dir.path().to_path_buf();

    // Create a config with no allowed commands (should allow all)
    let cfg = AppConfig {
        project_root: project_root.clone(),
        allowed_commands: vec![],
        ..Default::default()
    };

    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

    // All commands should be allowed when allowed_commands list is empty
    let result = fs_tools.execute_bash("echo 'test'").await;
    assert!(result.is_ok());

    let result_str = result.unwrap();
    let result: execute::ExecuteBashResult = serde_json::from_str(&result_str).unwrap();
    assert!(result.success);
    assert_eq!(result.stdout.trim(), "test");

    let result = fs_tools.execute_bash("ls -la").await;
    assert!(result.is_ok());

    // We're not checking the exact output of ls -la, just that it doesn't return an error

    // Test with a command that fails
    let result = fs_tools.execute_bash("invalid_command").await;
    assert!(result.is_ok()); // Should still return Ok with a JSON string

    let result_str = result.unwrap();
    let result: execute::ExecuteBashResult = serde_json::from_str(&result_str).unwrap();
    assert!(!result.success);

    Ok(())
}
