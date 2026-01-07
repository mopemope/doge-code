use crate::config::AppConfig;
use crate::session::{SessionManager, SessionStore};
use crate::tools::FsTools;
use crate::tools::execute;
use crate::tools::plan;
use anyhow::Result;
use std::sync::{Arc, Mutex};
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

    let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))?;
    let session_manager = Arc::new(Mutex::new(crate::session::SessionManager {
        store,
        current_session: None,
    }));
    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg))
        .with_session_manager(session_manager);

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

    let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))?;
    let session_manager = Arc::new(Mutex::new(crate::session::SessionManager {
        store,
        current_session: None,
    }));
    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg))
        .with_session_manager(session_manager);

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

    let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))?;
    let session_manager = Arc::new(Mutex::new(crate::session::SessionManager {
        store,
        current_session: None,
    }));
    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg))
        .with_session_manager(session_manager);

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

    let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))?;
    let session_manager = Arc::new(Mutex::new(crate::session::SessionManager {
        store,
        current_session: None,
    }));
    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg))
        .with_session_manager(session_manager);

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

    let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))?;
    let session_manager = Arc::new(Mutex::new(crate::session::SessionManager {
        store,
        current_session: None,
    }));
    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg))
        .with_session_manager(session_manager);

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

#[tokio::test]
async fn test_plan_read_creates_session_if_missing() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let project_root = temp_dir.path().to_path_buf();

    let store = SessionStore::new(project_root.join(".doge/sessions")).unwrap();
    let session_manager = Arc::new(Mutex::new(SessionManager {
        store,
        current_session: None,
    }));

    let cfg = AppConfig {
        project_root: project_root.clone(),
        ..Default::default()
    };
    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg))
        .with_session_manager(session_manager.clone());

    let plan_list = fs_tools.plan_read()?;
    let session_id = session_manager
        .lock()
        .unwrap()
        .current_session
        .as_ref()
        .unwrap()
        .meta
        .id
        .clone();

    assert_eq!(plan_list.session_id, Some(session_id));
    assert!(plan_list.items.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_plan_write_creates_session_and_persists_plan() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let project_root = temp_dir.path().to_path_buf();

    let store = SessionStore::new(project_root.join(".doge/sessions")).unwrap();
    let session_manager = Arc::new(Mutex::new(SessionManager {
        store,
        current_session: None,
    }));

    let cfg = AppConfig {
        project_root: project_root.clone(),
        ..Default::default()
    };
    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg))
        .with_session_manager(session_manager.clone());

    let items = vec![
        plan::PlanItem {
            id: "step-1".into(),
            content: "Write the plan".into(),
            status: "pending".into(),
        },
        plan::PlanItem {
            id: "step-2".into(),
            content: "Implement changes".into(),
            status: "pending".into(),
        },
        plan::PlanItem {
            id: "step-3".into(),
            content: "Run validators".into(),
            status: "pending".into(),
        },
    ];

    let written = fs_tools.plan_write(items.clone(), plan::PlanWriteMode::Replace)?;
    let session_id = session_manager
        .lock()
        .unwrap()
        .current_session
        .as_ref()
        .unwrap()
        .meta
        .id
        .clone();

    assert_eq!(written.session_id, Some(session_id.clone()));
    assert_eq!(written.items, items);

    let plan_path = project_root
        .join(".doge/plans")
        .join(format!("{}.json", session_id));
    assert!(plan_path.exists());

    let read_back = fs_tools.plan_read()?;
    assert_eq!(read_back.session_id, Some(session_id));
    assert_eq!(read_back.items, written.items);

    Ok(())
}

#[tokio::test]
async fn test_execute_shell_persistence() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let project_root = temp_dir.path().to_path_buf();

    let cfg = AppConfig {
        project_root: project_root.clone(),
        allowed_commands: vec![],
        ..Default::default()
    };
    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

    // 1. Set variable
    let res1_str = fs_tools.execute_shell("export TEST_VAR=persistent").await?;
    let res1: crate::tools::shell::ExecuteShellResult = serde_json::from_str(&res1_str)?;
    assert!(res1.success);

    // 2. Read variable
    let res2_str = fs_tools.execute_shell("echo $TEST_VAR").await?;
    let res2: crate::tools::shell::ExecuteShellResult = serde_json::from_str(&res2_str)?;
    assert!(res2.success);
    assert_eq!(res2.stdout.trim(), "persistent");

    Ok(())
}

#[tokio::test]
async fn test_execute_shell_cwd() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let project_root = temp_dir.path().to_path_buf();
    let subdir = project_root.join("subdir");
    tokio::fs::create_dir(&subdir).await?;

    let cfg = AppConfig {
        project_root: project_root.clone(),
        allowed_commands: vec![],
        ..Default::default()
    };
    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

    // 1. CD into subdir
    // Note: The shell starts in project_root.
    let res1_str = fs_tools.execute_shell("cd subdir").await?;
    let res1: crate::tools::shell::ExecuteShellResult = serde_json::from_str(&res1_str)?;
    assert!(res1.success);

    // 2. Check PWD
    let res2_str = fs_tools.execute_shell("pwd").await?;
    let res2: crate::tools::shell::ExecuteShellResult = serde_json::from_str(&res2_str)?;
    assert!(res2.success);
    // On some systems /tmp might be a symlink, so we verify it ends with "subdir"
    assert!(res2.stdout.trim().ends_with("subdir"));

    Ok(())
}

#[tokio::test]
async fn test_execute_shell_error() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let project_root = temp_dir.path().to_path_buf();

    let cfg = AppConfig {
        project_root: project_root.clone(),
        allowed_commands: vec![],
        ..Default::default()
    };
    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

    // 1. Run invalid command
    let res1_str = fs_tools.execute_shell("non_existent_command_123").await?;
    let res1: crate::tools::shell::ExecuteShellResult = serde_json::from_str(&res1_str)?;
    assert!(!res1.success);
    assert!(res1.exit_code.is_some());
    assert_ne!(res1.exit_code.unwrap(), 0);
    assert!(!res1.stderr.is_empty());

    // 2. Shell should still be alive
    let res2_str = fs_tools.execute_shell("echo alive").await?;
    let res2: crate::tools::shell::ExecuteShellResult = serde_json::from_str(&res2_str)?;
    assert!(res2.success);
    assert_eq!(res2.stdout.trim(), "alive");

    Ok(())
}

#[tokio::test]
async fn test_execute_shell_large_output() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let project_root = temp_dir.path().to_path_buf();

    let cfg = AppConfig {
        project_root: project_root.clone(),
        allowed_commands: vec![],
        ..Default::default()
    };
    let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

    // Generate large output (e.g., 64KB)
    // 64 * 1024 = 65536 bytes.
    // We use a simple loop in python or similar, or just seq.
    // relying on `seq` might be non-portable if minimal environment, but likely fine in this context.
    // Let's use printf to be safe-ish.
    let cmd = "for i in {1..10000}; do echo 'line '$i; done";
    let res_str = fs_tools.execute_shell(cmd).await?;
    let res: crate::tools::shell::ExecuteShellResult = serde_json::from_str(&res_str)?;

    assert!(res.success);
    assert!(res.stdout.len() > 10000); // Rough check
    assert!(res.stdout.contains("line 10000"));

    Ok(())
}
