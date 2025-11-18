//! Utility functions module
//!
//! This module provides common utility functions used throughout the application,
//! including Git repository detection, temporary directory management, and
//! error handling helpers.

use anyhow::{Context, Result};
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

/// Safely locks a mutex with error context
pub fn safe_lock<'a, T>(
    mutex: &'a tokio::sync::Mutex<T>,
    resource_name: &str,
) -> Result<tokio::sync::MutexGuard<'a, T>> {
    mutex
        .try_lock()
        .with_context(|| format!("Failed to lock {} mutex", resource_name))
}

/// Safely locks a RwLock for reading with error context
pub fn safe_read_lock<'a, T>(
    rwlock: &'a tokio::sync::RwLock<T>,
    resource_name: &str,
) -> Result<tokio::sync::RwLockReadGuard<'a, T>> {
    rwlock
        .try_read()
        .with_context(|| format!("Failed to read-lock {} rwlock", resource_name))
}

/// Safely locks a RwLock for writing with error context
pub fn safe_write_lock<'a, T>(
    rwlock: &'a tokio::sync::RwLock<T>,
    resource_name: &str,
) -> Result<tokio::sync::RwLockWriteGuard<'a, T>> {
    rwlock
        .try_write()
        .with_context(|| format!("Failed to write-lock {} rwlock", resource_name))
}

/// Safely locks a std::sync::Mutex with error context
pub fn safe_std_lock<'a, T>(
    mutex: &'a std::sync::Mutex<T>,
    resource_name: &str,
) -> Result<std::sync::MutexGuard<'a, T>> {
    mutex
        .lock()
        .map_err(|e| anyhow::anyhow!("Failed to lock {} std::mutex: {}", resource_name, e))
}

/// Safely locks a std::sync::RwLock for reading with error context
pub fn safe_std_read_lock<'a, T>(
    rwlock: &'a std::sync::RwLock<T>,
    resource_name: &str,
) -> Result<std::sync::RwLockReadGuard<'a, T>> {
    rwlock
        .read()
        .map_err(|e| anyhow::anyhow!("Failed to read-lock {} std::rwlock: {}", resource_name, e))
}

/// Safely locks a std::sync::RwLock for writing with error context
pub fn safe_std_write_lock<'a, T>(
    rwlock: &'a std::sync::RwLock<T>,
    resource_name: &str,
) -> Result<std::sync::RwLockWriteGuard<'a, T>> {
    rwlock
        .write()
        .map_err(|e| anyhow::anyhow!("Failed to write-lock {} std::rwlock: {}", resource_name, e))
}

/// Creates a temporary directory with error context
pub fn create_temp_dir_with_context(context: &str) -> Result<std::path::PathBuf> {
    let temp_dir = std::env::temp_dir();
    let temp_path = temp_dir.join(format!("doge-code-{}-{}", context, fastrand::u64(..)));
    std::fs::create_dir_all(&temp_path).with_context(|| {
        format!(
            "Failed to create temporary directory: {}",
            temp_path.display()
        )
    })?;
    Ok(temp_path)
}

/// Safely reads a file with error context
pub fn read_file_safe<P: AsRef<Path>>(path: P) -> Result<String> {
    let path = path.as_ref();
    std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read file: {}", path.display()))
}

/// Safely writes to a file with error context
pub fn write_file_safe<P: AsRef<Path>>(path: P, content: &str) -> Result<()> {
    let path = path.as_ref();
    std::fs::write(path, content)
        .with_context(|| format!("Failed to write file: {}", path.display()))
}

/// Safely creates a directory with error context
pub fn create_dir_safe<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref();
    std::fs::create_dir_all(path)
        .with_context(|| format!("Failed to create directory: {}", path.display()))
}

/// Safely checks if a path exists
pub fn path_exists_safe<P: AsRef<Path>>(path: P) -> bool {
    path.as_ref().exists()
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
