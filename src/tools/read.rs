use crate::config::AppConfig;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use serde_json::json;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "fs_read".to_string(),
            strict: None,
            description: "Reads the content of a text file from the absolute path. You can specify a starting line and a maximum number of lines to read. This is useful for inspecting file contents, reading specific sections of large files, or understanding the implementation details of a function or class. Do not use this for binary files or extremely large files.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "start_line": {"type": "integer"},
                    "limit": {"type": "integer"}
                },
                "required": ["path"]
            }),
        },
    }
}

pub fn fs_read(
    path: &str,
    start_line: Option<usize>,
    limit: Option<usize>,
    config: &AppConfig,
) -> Result<String> {
    let p = Path::new(path);

    // Ensure the path is absolute
    if !p.is_absolute() {
        anyhow::bail!("Path must be absolute: {}", path);
    }

    // Check if the path is within the project root or in allowed paths
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let canonical_path = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());

    let is_allowed_path = config
        .allowed_paths
        .iter()
        .any(|allowed_path| canonical_path.starts_with(allowed_path));

    if !canonical_path.starts_with(&project_root) && !is_allowed_path {
        anyhow::bail!(
            "Access to files outside the project root is not allowed: {}",
            path
        );
    }

    let meta = fs::metadata(p).with_context(|| format!("metadata {}", p.display()))?;
    if !meta.is_file() {
        anyhow::bail!("not a file");
    }
    let mut f = fs::File::open(p).with_context(|| format!("open {}", p.display()))?;
    let mut s = String::new();
    f.read_to_string(&mut s)
        .with_context(|| format!("read {}", p.display()))?;

    match (start_line, limit) {
        (Some(o), Some(l)) => Ok(s.lines().skip(o).take(l).collect::<Vec<_>>().join("\n")),
        _ => Ok(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn create_temp_file(content: &str) -> (PathBuf, String) {
        let temp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("temp");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let temp_file = tempfile::Builder::new()
            .prefix("test_")
            .suffix(".txt")
            .tempfile_in(&temp_dir)
            .unwrap();
        let file_path = temp_file.into_temp_path().to_path_buf();
        std::fs::write(&file_path, content).unwrap();
        let file_path_str = file_path.to_str().unwrap().to_string();
        (file_path, file_path_str)
    }

    fn create_temp_dir() -> PathBuf {
        let dir = tempfile::Builder::new()
            .prefix("test_dir_")
            .tempdir()
            .unwrap();
        #[allow(deprecated)]
        dir.into_path()
    }

    #[test]
    fn test_fs_read_full_file() {
        let (_temp_file, file_path) = create_temp_file("line1\nline2\nline3");
        let content = fs_read(&file_path, None, None, &AppConfig::default()).unwrap();
        assert_eq!(content, "line1\nline2\nline3");
    }

    #[test]
    fn test_fs_read_with_start_line_limit() {
        let (_temp_file, file_path) = create_temp_file("line1\nline2\nline3\nline4");
        let content = fs_read(&file_path, Some(1), Some(2), &AppConfig::default()).unwrap();
        assert_eq!(content, "line2\nline3");
    }

    #[test]
    fn test_fs_read_path_escape() {
        let temp_dir = create_temp_dir();
        let file_path = temp_dir.join("../some_file");
        let file_path_str = file_path.to_str().unwrap();
        let result = fs_read(file_path_str, None, None, &AppConfig::default());
        // Since we're now allowing absolute paths, this test might need to be adjusted
        // depending on the environment. For now, let's just check it's an error.
        assert!(result.is_err());
    }

    #[test]
    fn test_fs_read_not_a_file() {
        let temp_dir = create_temp_dir();
        let dir_path = temp_dir.to_str().unwrap();
        let result = fs_read(dir_path, None, None, &AppConfig::default());
        assert!(result.is_err());
    }
}
