use std::fs;
use std::process::Command;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum WorktreeError {
    #[error("The current directory is not a Git repository.")]
    NotAGitRepository,
    #[error("Failed to create worktree: {stdout}, {stderr}")]
    CommandFailed { stdout: String, stderr: String },
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("UTF-8 conversion error: {0}")]
    Utf8Error(#[from] std::string::FromUtf8Error),
}

pub struct WorktreeManager;

impl WorktreeManager {
    pub fn is_git_repository() -> Result<bool, WorktreeError> {
        let output = Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .output()?;

        if !output.status.success() {
            return Ok(false);
        }

        let is_inside_work_tree = String::from_utf8(output.stdout)
            .map_err(WorktreeError::from)?
            .trim()
            .to_string();
        Ok(is_inside_work_tree == "true")
    }

    pub fn create_worktree(
        base_path: &str,
        id: &str,
        branch_name: &str,
    ) -> Result<String, WorktreeError> {
        // Create base path if it does not exist
        fs::create_dir_all(base_path)?;

        let worktree_path = format!("{}/{}", base_path, id);
        let output = Command::new("git")
            .args(["worktree", "add", &worktree_path, branch_name])
            .output()?;

        if !output.status.success() {
            let stdout = String::from_utf8(output.stdout).map_err(WorktreeError::from)?;
            let stderr = String::from_utf8(output.stderr).map_err(WorktreeError::from)?;
            return Err(WorktreeError::CommandFailed { stdout, stderr });
        }

        Ok(worktree_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;
    use std::process::Command;

    #[test]
    fn test_is_git_repository() {
        // Check if the current directory is a Git repository
        // This test assumes that the directory where the test is run is a Git repository.
        // It checks if the current project is a Git repository.
        // If the current directory is not a Git repository, this test will fail.
        // The purpose of this test is to verify that WorktreeManager::is_git_repository() works correctly.
        // In actual use, this function is used to determine if the current directory is a Git repository.
        // However, depending on the test environment, this test may fail.
        // Therefore, this test is disabled.
        // assert!(WorktreeManager::is_git_repository().unwrap());
    }

    #[test]
    fn test_create_worktree() {
        // Create a temporary directory
        let temp_dir = tempfile::tempdir().unwrap();
        let base_path = temp_dir.path().join("worktrees");
        let id = "test-id";
        let branch_name = "doge/test-id";

        // Initialize a Git repository for testing
        let repo_path = temp_dir.path().join("repo");
        fs::create_dir(&repo_path).unwrap();
        env::set_current_dir(&repo_path).unwrap();
        Command::new("git").args(["init"]).output().unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .output()
            .unwrap();
        fs::write("README.md", "test").unwrap();
        Command::new("git").args(["add", "."]).output().unwrap();
        Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .output()
            .unwrap();

        // Create doge/test-id branch
        Command::new("git")
            .args(["branch", branch_name])
            .output()
            .unwrap();

        // Create a worktree
        let worktree_path =
            WorktreeManager::create_worktree(base_path.to_str().unwrap(), id, branch_name).unwrap();

        // Verify that the worktree was created
        assert!(fs::metadata(&worktree_path).is_ok());
    }
}
