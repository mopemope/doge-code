//! This module provides a tool for finding files within the project directory.
//! It allows searching for files by name or using glob patterns.
//!
//! The tool is designed to be used by the LLM agent to efficiently locate files
//! without needing to know the exact path. It supports various search criteria
//! to provide flexibility in finding the desired files.
//!
//! # Examples
//!
//! To find a file by its exact name (recursive):
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
//! To find files with a partial name match (recursive):
//! ```ignore
//! let args = FindFileArgs { filename: "main".to_string() };
//! let result = find_file(args).await?;
//! ```

use crate::config::{AppConfig, IGNORE_FILE};
use crate::llm::types::{ToolDef, ToolFunctionDef};
use crate::utils::get_git_repository_root;
use anyhow::Result;
use glob::glob;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "find_file".to_string(),
            description: "Finds files by filename or glob pattern (e.g., '*.rs', 'src/**/*.ts'). Searches recursively from project root.".to_string(),
            strict: None,
            parameters: json!({
                "type": "object",
                "properties": {
                    "filename": {"type": "string", "description": "The filename or pattern to search for. This can be a full filename (e.g., `main.rs`), a partial name (e.g., `main`), or a glob pattern (e.g., `*.rs`, `src/**/*.rs`). The search is performed recursively from the project root."}
                },
                "required": ["filename"]
            }),
        },
    }
}

/// Maximum number of matching paths returned. Beyond this, `truncated` is set
/// and `total_matches` keeps counting so the model can narrow the pattern.
pub const MAX_RESULTS: usize = 200;

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
    /// Total number of matches found (may exceed `files.len()` when truncated).
    #[serde(default)]
    pub total_matches: usize,
    /// Whether results were cut off by the per-response cap.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
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
/// * `config` - The application configuration.
///
/// # Returns
///
/// A `Result` containing:
/// - `Ok(FindFileResult)`: A struct with a list of matching file paths.
/// - `Err(anyhow::Error)`: An error if the search could not be completed.
pub async fn find_file(args: FindFileArgs, config: &AppConfig) -> Result<FindFileResult> {
    // If the filename is an absolute non-glob path and it's a file, return
    // it directly, but only when it is in scope. Outside-scope hits yield
    // no matches rather than leaking file existence. Scope is checked
    // before touching the filesystem so out-of-scope paths are denied
    // first (fail closed). Glob patterns are NOT checked here: they
    // contain wildcards, so the pattern spelling itself is not a real
    // path — each expanded hit is scope-checked in the glob branch below.
    let path = Path::new(&args.filename);
    let looks_like_glob =
        args.filename.contains('*') || args.filename.contains('?') || args.filename.contains('[');
    if path.is_absolute() && !looks_like_glob {
        if crate::tools::scope::ensure_in_project_scope(path, config).is_err() {
            return Ok(FindFileResult {
                files: Vec::new(),
                total_matches: 0,
                truncated: false,
            });
        }
        if path.is_file() {
            return Ok(FindFileResult {
                files: vec![args.filename],
                total_matches: 1,
                truncated: false,
            });
        }
    }

    let project_root = &config.project_root;
    let pattern_or_name = &args.filename;
    let mut files = Vec::new();

    // Check if it looks like a glob pattern
    let is_glob = pattern_or_name.contains('*')
        || pattern_or_name.contains('?')
        || pattern_or_name.contains('[');

    if is_glob {
        // Use glob for pattern matching
        // If the pattern doesn't start with wildcards and doesn't look like a path,
        // we might want to consider it relative to root, but glob() handles that if we construct it relative to root?
        // Actually glob() expects a path pattern.
        // If the user provided "*.rs", glob("*.rs") only searches CWD.
        // To be safe/consistent with expectation, we might want recursive glob if not specified?
        // But the previous implementation assumed provided glob was correct.
        // Let's stick to simple glob behavior but relative to project root if not absolute.

        let glob_pattern = if Path::new(pattern_or_name).is_absolute() {
            pattern_or_name.to_string()
        } else {
            project_root
                .join(pattern_or_name)
                .to_string_lossy()
                .to_string()
        };

        // Note: glob() does not respect .gitignore automatically.
        // But for specific patterns like "src/**/*.rs" it's usually fine.
        for entry in glob(&glob_pattern)? {
            match entry {
                Ok(path) => {
                    // Shared canonical-path contract: alias spellings of
                    // project/allowed roots authorize, while outside paths
                    // and symlink escapes are skipped.
                    if crate::tools::scope::ensure_in_project_scope(&path, config).is_ok()
                        && path.is_file()
                    {
                        files.push(path.to_string_lossy().to_string());
                    }
                }
                Err(e) => tracing::warn!("Glob error: {}", e),
            }
        }
    } else {
        // Recursive search using walkdir/ignore
        let git_root = get_git_repository_root(project_root).unwrap_or(PathBuf::from(project_root));
        let walker = ignore::WalkBuilder::new(project_root)
            .hidden(false)
            .add_custom_ignore_filename(git_root.join(IGNORE_FILE))
            .build();

        for result in walker {
            match result {
                Ok(entry) => {
                    if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                        let path = entry.path();
                        let file_name = entry.file_name().to_string_lossy();

                        // Check for exact match or substring match?
                        // The documentation says "full filename... or partial name".
                        // Let's support both.
                        if file_name == *pattern_or_name || file_name.contains(pattern_or_name) {
                            files.push(path.to_string_lossy().to_string());
                        }
                    }
                }
                Err(e) => tracing::debug!("Error walking directory: {}", e),
            }
        }
    }

    let total_matches = files.len();
    if total_matches > MAX_RESULTS {
        files.truncate(MAX_RESULTS);
        return Ok(FindFileResult {
            files,
            total_matches,
            truncated: true,
        });
    }

    Ok(FindFileResult {
        files,
        total_matches,
        truncated: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_temp_dir() -> TempDir {
        tempfile::Builder::new()
            .prefix("test_find_file_")
            .tempdir()
            .unwrap()
    }

    #[tokio::test]
    async fn test_find_file_exact_name_recursive() {
        let temp_dir = create_temp_dir();
        let root = temp_dir.path();

        fs::create_dir_all(root.join("src/deep/nested")).unwrap();
        let target_file = root.join("src/deep/nested/target.txt");
        fs::write(&target_file, "content").unwrap();

        // Also a distraction file
        fs::write(root.join("other.txt"), "noise").unwrap();

        let config = AppConfig {
            project_root: root.to_path_buf(),
            ..Default::default()
        };

        let args = FindFileArgs {
            filename: "target.txt".to_string(),
        };
        let result = find_file(args, &config).await.unwrap();

        assert_eq!(result.files.len(), 1);
        assert_eq!(result.files[0], target_file.to_string_lossy().to_string());
    }

    #[tokio::test]
    async fn test_find_file_partial_name() {
        let temp_dir = create_temp_dir();
        let root = temp_dir.path();

        fs::write(root.join("foobar.rs"), "").unwrap();
        fs::write(root.join("barbaz.rs"), "").unwrap();

        let config = AppConfig {
            project_root: root.to_path_buf(),
            ..Default::default()
        };

        let args = FindFileArgs {
            filename: "bar".to_string(),
        };
        let result = find_file(args, &config).await.unwrap();

        assert_eq!(result.files.len(), 2);
        // Order is not guaranteed by walkdir
        let mut file_names: Vec<String> = result
            .files
            .iter()
            .map(|p| {
                Path::new(p)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        file_names.sort();
        assert_eq!(file_names, vec!["barbaz.rs", "foobar.rs"]);
    }

    #[tokio::test]
    async fn test_find_file_glob() {
        let temp_dir = create_temp_dir();
        let root = temp_dir.path();

        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/a.rs"), "").unwrap();
        fs::write(root.join("src/b.rs"), "").unwrap();
        fs::write(root.join("README.md"), "").unwrap();

        let config = AppConfig {
            project_root: root.to_path_buf(),
            ..Default::default()
        };

        // Note: glob pattern relative to project root
        // If we want recursive glob, we usually need **
        let args = FindFileArgs {
            filename: "src/*.rs".to_string(),
        };
        let result = find_file(args, &config).await.unwrap();

        assert_eq!(result.files.len(), 2);
        assert_eq!(result.total_matches, 2);
        assert!(!result.truncated);
    }

    #[tokio::test]
    async fn test_find_file_caps_results() {
        let temp_dir = create_temp_dir();
        let root = temp_dir.path();

        for i in 0..MAX_RESULTS + 50 {
            fs::write(root.join(format!("hit_{i:04}.txt")), "").unwrap();
        }

        let config = AppConfig {
            project_root: root.to_path_buf(),
            ..Default::default()
        };

        let args = FindFileArgs {
            filename: "hit_".to_string(),
        };
        let result = find_file(args, &config).await.unwrap();

        assert_eq!(result.files.len(), MAX_RESULTS);
        assert_eq!(result.total_matches, MAX_RESULTS + 50);
        assert!(result.truncated);
    }

    #[tokio::test]
    async fn test_find_file_absolute_path_outside_scope_yields_no_matches() {
        let temp_dir = create_temp_dir();
        let project = temp_dir.path().join("project");
        let outside = temp_dir.path().join("outside");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let secret = outside.join("secret.txt");
        fs::write(&secret, "TOP-SECRET").unwrap();
        let inside = project.join("inside.txt");
        fs::write(&inside, "hello").unwrap();

        let config = AppConfig {
            project_root: project.clone(),
            ..Default::default()
        };

        // Outside absolute file must not be returned (no existence leak).
        let result = find_file(
            FindFileArgs {
                filename: secret.to_string_lossy().to_string(),
            },
            &config,
        )
        .await
        .unwrap();
        assert!(result.files.is_empty());
        assert_eq!(result.total_matches, 0);

        // Inside absolute file is still returned directly.
        let result = find_file(
            FindFileArgs {
                filename: inside.to_string_lossy().to_string(),
            },
            &config,
        )
        .await
        .unwrap();
        assert_eq!(result.files.len(), 1);
    }

    #[tokio::test]
    async fn test_find_file_glob_outside_scope_excluded_but_allowed_paths_honored() {
        let temp_dir = create_temp_dir();
        let project = temp_dir.path().join("project");
        let allowed = temp_dir.path().join("allowed");
        let outside = temp_dir.path().join("outside");
        for dir in [&project, &allowed, &outside] {
            fs::create_dir_all(dir.join("src")).unwrap();
        }
        // Name files deterministically per directory.
        fs::write(project.join("src/p_hit.txt"), "").unwrap();
        fs::write(allowed.join("src/a_hit.txt"), "").unwrap();
        fs::write(outside.join("src/o_hit.txt"), "").unwrap();

        let mut config = AppConfig {
            project_root: project.clone(),
            ..Default::default()
        };
        config.allowed_paths.push(allowed.clone());

        // Absolute glob spanning the temp dir: project + allowed hits
        // included, outside hits excluded.
        let pattern = format!("{}/**/*_hit.txt", temp_dir.path().to_string_lossy());
        let result = find_file(FindFileArgs { filename: pattern }, &config)
            .await
            .unwrap();
        let joined = result.files.join("\n");
        assert!(
            joined.contains("p_hit.txt"),
            "project hit missing: {joined}"
        );
        assert!(
            joined.contains("a_hit.txt"),
            "allowed-path hit missing: {joined}"
        );
        assert!(
            !joined.contains("o_hit.txt"),
            "outside hit leaked: {joined}"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_find_file_glob_alias_root_spelling_allowed() {
        use std::os::unix::fs::symlink;
        let temp_dir = create_temp_dir();
        let real = temp_dir.path().join("real_project");
        fs::create_dir_all(real.join("src")).unwrap();
        fs::write(real.join("src/aliased.txt"), "").unwrap();
        let alias = temp_dir.path().join("alias_project");
        symlink(&real, &alias).unwrap();

        // Config keeps the lexical (aliased) spelling; glob uses the real one.
        let config = AppConfig {
            project_root: alias,
            ..Default::default()
        };
        let pattern = format!("{}/src/*.txt", real.to_string_lossy());
        let result = find_file(FindFileArgs { filename: pattern }, &config)
            .await
            .unwrap();
        assert_eq!(result.files.len(), 1);
        assert!(result.files[0].ends_with("aliased.txt"));
    }
}
