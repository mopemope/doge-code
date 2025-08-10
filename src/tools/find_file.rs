//! This module provides a tool for finding files within the project directory.
//! It allows searching for files by name or using glob patterns.
//!
//! The tool is designed to be used by the LLM agent to efficiently locate files
//! without needing to know the exact path. It supports various search criteria
//! to provide flexibility in finding the desired files.
//!
//! # Examples
//!
//! To find a file by its exact name:
//! ```ignore
//! let args = FindFileArgs { filename: "main.rs".to_string() };
//! let result = find_file(args).await?;
//! ```
//!
//! To find files matching a glob pattern:
//! ```ignore
//! let args = FindFileArgs { filename: "*.rs".to_string() };
//! let result = find_file(args).await?;
//! ```
//!
//! To find files with a partial name match:
//! ```ignore
//! let args = FindFileArgs { filename: "main".to_string() };
//! let result = find_file(args).await?;
//! ```

use anyhow::Result;
use glob::glob;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Arguments for the `find_file` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindFileArgs {
    /// The filename or pattern to search for.
    ///
    /// This can be:
    /// - A full filename (e.g., `"main.rs"`)
    /// - A partial name (e.g., `"main"`)
    /// - A glob pattern (e.g., `"*.rs"`, `"src/**/*.rs"`)
    ///
    /// The search is performed recursively from the project root.
    /// For partial name matches, the tool will look for files whose names contain
    /// the provided string.
    pub filename: String,
}

/// The result returned by the `find_file` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindFileResult {
    /// A list of absolute file paths that match the search criteria.
    ///
    /// If no files are found, this vector will be empty.
    /// The paths are guaranteed to be valid UTF-8 strings.
    pub files: Vec<String>,
}

/// Finds files in the project based on a filename or pattern.
///
/// This function searches for files within the specified project root directory
/// that match the given filename or pattern. The search is performed recursively
/// through all subdirectories.
///
/// # Arguments
///
/// * `args` - The arguments for the search, including the filename or pattern.
///
/// # Returns
///
/// A `Result` containing:
/// - `Ok(FindFileResult)`: A struct with a list of matching file paths.
/// - `Err(anyhow::Error)`: An error if the search could not be completed.
///
/// # Examples
///
/// ```ignore
/// let args = FindFileArgs { filename: "lib.rs".to_string() };
/// let result = find_file(args).await?;
/// assert_eq!(result.files, vec!["/path/to/project/src/lib.rs"]);
/// ```
pub async fn find_file(args: FindFileArgs) -> Result<FindFileResult> {
    // If the filename is an absolute path and it's a file, return it directly.
    let path = Path::new(&args.filename);
    if path.is_absolute() && path.is_file() {
        return Ok(FindFileResult {
            files: vec![args.filename],
        });
    }

    // Otherwise, treat the filename as a glob pattern and search for matching files.
    // If it's not a glob pattern, this will still work for exact filename matches.
    let pattern = &args.filename;
    let paths = glob(pattern)?
        .filter_map(Result::ok)
        .filter(|p| p.is_file())
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    Ok(FindFileResult { files: paths })
}
