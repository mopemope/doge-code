use crate::analysis::language_config::language_configs;
use crate::config::IGNORE_FILE;
use anyhow::{Context, Result};
use ignore::WalkBuilder;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
use tracing::debug;

pub fn find_target_files(root: &Path) -> Result<Vec<PathBuf>> {
    // Get the set of target extensions
    let target_extensions: HashSet<String> = language_configs()
        .iter()
        .flat_map(|c| c.extensions.iter())
        .map(|s| s.to_string())
        .collect();

    let mut files = Vec::new();

    // Use ignore crate to walk the directory and filter files
    // Add custom ignore file support for .dogeignore in addition to .gitignore
    let mut builder = WalkBuilder::new(root);
    builder
        .git_ignore(true) // Enable .gitignore
        .require_git(false) // Allow .gitignore outside of git repo
        .add_custom_ignore_filename(IGNORE_FILE); // Add .dogeignore support

    for result in builder.build() {
        let entry = result.context("walk entry")?;
        if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            let path = entry.path();
            // Check if the file extension is in the target extensions
            if let Some(ext) = path.extension().and_then(|e| e.to_str())
                && target_extensions.contains(ext)
            {
                files.push(path.to_path_buf());
            }
        }
    }

    // Debug log: Print all found files
    debug!("Found {} files:", files.len());
    for file in &files {
        debug!("  {}", file.display());
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_find_target_files_with_gitignore() {
        // Create a temporary directory for testing
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create a .gitignore file
        let gitignore_content = "target/";
        std::fs::write(root.join(".gitignore"), gitignore_content).unwrap();

        // Create a source file in the root directory
        let src_file = root.join("main.rs");
        std::fs::write(&src_file, "fn main() {}").unwrap();

        // Create a target directory and a source file inside it
        let target_dir = root.join("target");
        std::fs::create_dir(&target_dir).unwrap();
        let target_src_file = target_dir.join("generated.rs");
        std::fs::write(&target_src_file, "fn generated() {}").unwrap();

        // Call find_target_files
        let files = find_target_files(root).unwrap();

        // Check that the file in the root directory is found
        assert!(files.contains(&src_file));

        // Check that the file in the target directory is NOT found
        assert!(!files.contains(&target_src_file));

        // Print all found files for debugging
        println!("Found files:");
        for file in &files {
            println!("  {}", file.display());
        }
    }

    #[test]
    fn test_ignore_crate_with_gitignore() {
        use ignore::WalkBuilder;

        // Create a temporary directory for testing
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create a .gitignore file
        let gitignore_content = "target/";
        std::fs::write(root.join(".gitignore"), gitignore_content).unwrap();

        // Create a source file in the root directory
        let src_file = root.join("main.rs");
        std::fs::write(&src_file, "fn main() {}").unwrap();

        // Create a target directory and a source file inside it
        let target_dir = root.join("target");
        std::fs::create_dir(&target_dir).unwrap();
        let target_src_file = target_dir.join("generated.rs");
        std::fs::write(&target_src_file, "fn generated() {}").unwrap();

        // Use ignore crate to walk the directory with WalkBuilder
        let mut files = Vec::new();
        for result in WalkBuilder::new(root)
            .git_ignore(true) // Explicitly enable .gitignore
            .require_git(false) // Allow .gitignore outside of git repo
            .build()
        {
            let entry = result.unwrap();
            if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                files.push(entry.path().to_path_buf());
            }
        }

        // Check that the file in the root directory is found
        assert!(files.contains(&src_file));

        // Check that the file in the target directory is NOT found
        assert!(!files.contains(&target_src_file));

        // Print all found files for debugging
        println!("Found files with ignore crate (WalkBuilder):");
        for file in &files {
            println!("  {}", file.display());
        }
    }

    #[test]
    fn test_find_target_files_with_dogeignore() {
        // Create a temporary directory for testing
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create a .dogeignore file
        let dogeignore_content = "backlog/";
        std::fs::write(root.join(".dogeignore"), dogeignore_content).unwrap();

        // Create a source file in the root directory
        let src_file = root.join("main.rs");
        std::fs::write(&src_file, "fn main() {}").unwrap();

        // Create a backlog directory and a markdown file inside it
        let backlog_dir = root.join("backlog");
        std::fs::create_dir(&backlog_dir).unwrap();
        let backlog_file = backlog_dir.join("task.md");
        std::fs::write(&backlog_file, "# Task description").unwrap();

        // Call find_target_files
        let files = find_target_files(root).unwrap();

        // Check that the file in the root directory is found
        assert!(files.contains(&src_file));

        // Check that the file in the backlog directory is NOT found (because .dogeignore excludes it)
        assert!(!files.contains(&backlog_file));

        // Print all found files for debugging
        println!("Found files with .dogeignore test:");
        for file in &files {
            println!("  {}", file.display());
        }
    }
}
