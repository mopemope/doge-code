use anyhow::Result;
use std::env;
use uuid::Uuid;

use crate::features::worktree_manager::{WorktreeError, WorktreeManager};

pub fn handle_git_worktree() -> Result<String, WorktreeError> {
    // 1. Verify that the current working directory is under Git repository management.
    if !WorktreeManager::is_git_repository()? {
        return Err(WorktreeError::NotAGitRepository);
    }

    // 2. Generate a unique ID for the new worktree.
    let id = Uuid::new_v4().to_string();
    let branch_name = format!("doge/{}", id);

    // Get project name
    let current_dir = env::current_dir()?;
    let project_name = current_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown");

    // Base directory to create worktree
    let base_path = format!("/tmp/doge-code/{}", project_name);

    // 3. Using the generated ID, create a new branch (`doge/<ID>`) and a new worktree.
    let worktree_path = WorktreeManager::create_worktree(&base_path, &id, &branch_name)?;

    // 4. Immediately change the agent's execution context (current working directory) to the path of the newly created worktree.
    // Note: This part is deeply related to the agent's overall context management, so only the path is notified here.
    // The actual context change should be performed by the calling process.

    // Success output
    Ok(format!(
        "Worktree created.\nPath: {}\nBranch: {}",
        worktree_path, branch_name
    ))
}
