//! Utility functions module

use std::path::{Path, PathBuf};

/// Checks if the specified path is a Git repository.
/// It traverses parent directories to check.
///
/// # Arguments
/// * `path` - The path to check
///
/// # Returns
/// `true` if the path is a Git repository, `false` otherwise.
pub fn is_git_repository<P: AsRef<Path>>(path: P) -> bool {
    let mut current_path = path.as_ref();

    loop {
        // Check if .git directory exists in the current path
        if current_path.join(".git").is_dir() {
            return true;
        }

        // Check for bare repository (config file exists)
        let git_config_path = current_path.join("config");
        if git_config_path.is_file() {
            // If config file exists, check its contents to see if it's a Git repository
            if let Ok(content) = std::fs::read_to_string(&git_config_path)
                && content.contains("[core]")
            {
                return true;
            }
        }

        // Move to parent directory
        match current_path.parent() {
            Some(parent) => current_path = parent,
            None => break, // Reached root directory
        }
    }

    false
}

/// Gets the root path of a Git repository.
/// It traverses parent directories to check.
///
/// # Arguments
/// * `path` - The path to check
///
/// # Returns
/// The root path of the Git repository if the path is a Git repository, `None` otherwise.
pub fn get_git_repository_root<P: AsRef<Path>>(path: P) -> Option<PathBuf> {
    let mut current_path = path.as_ref();

    loop {
        // Check if .git directory exists in the current path
        if current_path.join(".git").is_dir() {
            return Some(current_path.to_path_buf());
        }

        // Check for bare repository (config file exists)
        let git_config_path = current_path.join("config");
        if git_config_path.is_file() {
            // If config file exists, check its contents to see if it's a Git repository
            if let Ok(content) = std::fs::read_to_string(&git_config_path)
                && content.contains("[core]")
            {
                return Some(current_path.to_path_buf());
            }
        }

        // Move to parent directory
        match current_path.parent() {
            Some(parent) => current_path = parent,
            None => break, // Reached root directory
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;
    use std::process::Command;

    #[test]
    fn test_is_git_repository_with_git_dir() {
        let temp = tempfile::tempdir().expect("Failed to create temp dir");
        let status = std::process::Command::new("git")
            .arg("init")
            .current_dir(temp.path())
            .status()
            .expect("git init failed");
        assert!(status.success());
        assert!(is_git_repository(temp.path()));
    }

    #[test]
    fn test_is_git_repository_without_git_dir() {
        // Create a temporary directory and confirm it's not a Git repository
        let temp_dir = env::temp_dir();
        assert!(!is_git_repository(&temp_dir));
    }

    #[test]
    fn test_is_git_repository_with_bare_repo() {
        // Create a bare repository for testing
        let temp_dir = env::temp_dir();
        let repo_path = temp_dir.join("test_bare_repo.git");

        // Cleanup after test
        let _ = fs::remove_dir_all(&repo_path);

        // Create bare repository
        let output = Command::new("git")
            .arg("init")
            .arg("--bare")
            .arg(&repo_path)
            .output();

        // Run test if git command is available
        if output.is_ok() && output.unwrap().status.success() {
            assert!(is_git_repository(&repo_path));

            // Cleanup
            let _ = fs::remove_dir_all(&repo_path);
        }
    }

    #[test]
    fn test_is_git_repository_in_subdirectory() {
        let temp = tempfile::tempdir().expect("Failed to create temp dir");
        let status = std::process::Command::new("git")
            .arg("init")
            .current_dir(temp.path())
            .status()
            .expect("git init failed");
        assert!(status.success());
        let sub_dir = temp.path().join("src");
        std::fs::create_dir(&sub_dir).expect("Failed to create sub dir");
        assert!(is_git_repository(&sub_dir));
    }

    #[test]
    fn test_get_git_repository_root() {
        let temp = tempfile::tempdir().expect("Failed to create temp dir");
        let status = std::process::Command::new("git")
            .arg("init")
            .current_dir(temp.path())
            .status()
            .expect("git init failed");
        assert!(status.success());

        // From repo root
        let root1 = get_git_repository_root(temp.path());
        assert!(root1.is_some());
        assert_eq!(root1.unwrap(), temp.path());

        // From sub dir
        let sub_dir = temp.path().join("src");
        std::fs::create_dir(&sub_dir).expect("Failed to create sub dir");
        let root2 = get_git_repository_root(&sub_dir);
        assert!(root2.is_some());
        assert_eq!(root2.unwrap(), temp.path());
    }

    #[test]
    fn test_get_git_repository_root_without_git() {
        // Get the root of a directory that is not a Git repository
        let temp_dir = env::temp_dir();
        let root = get_git_repository_root(&temp_dir);
        assert!(root.is_none());
    }
}
