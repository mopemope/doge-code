use crate::{
    config::IGNORE_FILE,
    llm::types::{ToolDef, ToolFunctionDef},
    utils::get_git_repository_root,
};
use anyhow::Result;
use serde_json::json;
use std::path::{Path, PathBuf};
use tracing::debug;

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "fs_list".to_string(),
            description: "Lists files and directories within a specified path. You can limit the depth of recursion and filter results by a glob pattern. The default maximum depth is 1. This tool is useful for exploring the project structure, finding specific files, or getting an overview of the codebase before starting a task. For example, use it to see what files are in a directory or to find all `.rs` files.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "max_depth": {"type": "integer"},
                    "pattern": {"type": "string"}
                },
                "required": ["path"]
            }),
        },
    }
}

/// Lists files and directories within a specified path.
///
/// # Arguments
///
/// * `path` - The absolute path to list files and directories.
/// * `max_depth` - The maximum depth to traverse. Defaults to 1.
/// * `pattern` - An optional glob pattern to filter files.
///
/// # Returns
///
/// A vector of strings representing the file and directory paths.
pub fn fs_list(path: &str, max_depth: Option<usize>, pattern: Option<&str>) -> Result<Vec<String>> {
    let full_path = Path::new(path);

    // Ensure the path is absolute
    if !full_path.is_absolute() {
        anyhow::bail!("Path must be absolute: {}", path);
    }

    let git_root = get_git_repository_root(path).unwrap_or(PathBuf::from(path));

    let mut files = Vec::new();
    let mut binding = ignore::WalkBuilder::new(path);
    let mut walker = binding.ignore(false).hidden(false);
    if pattern.is_some() {
        walker = walker.overrides(
            ignore::overrides::OverrideBuilder::new(path)
                .add(pattern.unwrap_or("*"))?
                .build()?,
        )
    }

    let walker = walker
        .max_depth(Some(max_depth.unwrap_or(1)))
        .add_custom_ignore_filename(git_root.join(IGNORE_FILE))
        .build();

    for result in walker {
        match result {
            Ok(entry) => {
                let path = entry.path();
                debug!("Found entry: {}", path.display());
                files.push(path.to_string_lossy().to_string());
            }
            Err(e) => {
                debug!("Error walking directory: {}", e);
            }
        }
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_fs_list_simple() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("a")).unwrap();
        fs::write(root.join("a/b.txt"), "").unwrap();
        fs::write(root.join("c.txt"), "").unwrap();

        let root_str = root.to_str().unwrap();
        // With max_depth=1, we should only see direct children of the root.
        let result = fs_list(root_str, Some(1), None).unwrap();
        let mut expected = vec![
            format!("{}", root_str),
            format!("{}/a", root_str),
            format!("{}/c.txt", root_str),
        ];
        expected.sort();
        let mut sorted_result = result;
        sorted_result.sort();

        assert_eq!(sorted_result, expected);
    }

    #[test]
    fn test_fs_list_with_depth() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("a/b/c.txt"), "").unwrap();

        let root_str = root.to_str().unwrap();
        let result = fs_list(&format!("{root_str}"), Some(3), None).unwrap();
        assert_eq!(
            result,
            vec![
                format!("{root_str}"),
                format!("{root_str}/a"),
                format!("{root_str}/a/b"),
                format!("{root_str}/a/b/c.txt")
            ]
        );
    }

    #[test]
    fn test_fs_list_with_pattern() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.txt"), "").unwrap();
        fs::write(root.join("b.log"), "").unwrap();

        let root_str = root.to_str().unwrap();
        let result = fs_list(root_str, None, Some("*.txt")).unwrap();
        assert_eq!(
            result,
            vec![format!("{root_str}"), format!("{root_str}/a.txt"),]
        );
    }
}
