use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use glob::glob;
use serde_json::json;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "fs_read_many_files".to_string(),
            description: "Reads the content of multiple files at once. You can specify a list of file paths or glob patterns. This is useful for getting a comprehensive overview of multiple files, such as all source files in a directory or a set of related configuration files.".to_string(),
            strict: None,
            parameters: json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "A list of absolute file paths or glob patterns."
                    },
                },
                "required": ["paths"]
            }),
        },
    }
}

#[allow(unused_variables)]
pub fn fs_read_many_files(
    paths: Vec<String>,
    exclude: Option<Vec<String>>,
    recursive: Option<bool>,
) -> Result<String> {
    let mut content = String::new();
    let mut all_paths = Vec::new();
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let config = crate::config::AppConfig::default();

    for path_pattern in paths {
        for entry in glob(&path_pattern)? {
            match entry {
                Ok(path) => {
                    let canonical_path = path.canonicalize().unwrap_or_else(|_| path.clone());

                    let is_allowed_path = config
                        .allowed_paths
                        .iter()
                        .any(|allowed_path| canonical_path.starts_with(allowed_path));

                    if canonical_path.starts_with(&project_root) || is_allowed_path {
                        all_paths.push(path);
                    } else {
                        // Optionally, you can log or handle paths outside the project root
                        // For now, we'll just skip them
                        continue;
                    }
                }
                Err(e) => return Err(anyhow::anyhow!("{}", e)),
            }
        }
    }

    if let Some(exclude_patterns) = exclude {
        for pattern in exclude_patterns {
            all_paths.retain(|path: &std::path::PathBuf| {
                !path.to_str().unwrap_or("").contains(&pattern)
            });
        }
    }

    for path in all_paths {
        if path.is_file() {
            let p = Path::new(&path);

            if !p.is_absolute() {
                anyhow::bail!("Path must be absolute: {}", p.display());
            }

            let mut f = fs::File::open(p).with_context(|| format!("open {}", p.display()))?;
            let mut s = String::new();
            f.read_to_string(&mut s)
                .with_context(|| format!("read {}", p.display()))?;
            content.push_str(&format!("--- {}---\n", p.display()));
            content.push_str(&s);
            content.push('\n');
        }
    }

    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn create_temp_file(content: &str) -> (NamedTempFile, String) {
        let temp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("temp");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let temp_file = tempfile::Builder::new()
            .prefix("test_")
            .suffix(".txt")
            .tempfile_in(&temp_dir)
            .unwrap();
        let file_path = temp_file.path().to_str().unwrap().to_string();
        std::fs::write(&file_path, content).unwrap();
        (temp_file, file_path.clone())
    }

    fn create_temp_dir() -> PathBuf {
        let temp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("temp");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let dir = tempfile::Builder::new()
            .prefix("test_dir_")
            .tempdir_in(&temp_dir)
            .unwrap();
        #[allow(deprecated)]
        dir.into_path()
    }

    #[test]
    fn test_fs_read_many_files() {
        let (_temp_file1, file1_path) = create_temp_file("content1");
        let (_temp_file2, file2_path) = create_temp_file("content2");

        let content =
            fs_read_many_files(vec![file1_path.clone(), file2_path.clone()], None, None).unwrap();

        assert!(content.contains(&format!("--- {file1_path}---")));
        assert!(content.contains("content1"));
        assert!(content.contains(&format!("--- {file2_path}---")));
        assert!(content.contains("content2"));
    }

    #[test]
    fn test_fs_read_many_files_with_glob() {
        let temp_dir = create_temp_dir();
        std::fs::create_dir_all(temp_dir.join("a")).unwrap();
        std::fs::create_dir_all(temp_dir.join("b")).unwrap();

        let mut file1 = tempfile::Builder::new()
            .prefix("test_")
            .suffix(".txt")
            .tempfile_in(temp_dir.join("a"))
            .unwrap();
        write!(file1, "content1").unwrap();
        let file1_path = file1.path().to_str().unwrap().to_string();

        let mut file2 = tempfile::Builder::new()
            .prefix("test_")
            .suffix(".txt")
            .tempfile_in(temp_dir.join("b"))
            .unwrap();
        write!(file2, "content2").unwrap();
        let file2_path = file2.path().to_str().unwrap().to_string();

        let content = fs_read_many_files(
            vec![format!("{}/**/*", temp_dir.to_str().unwrap())],
            None,
            Some(true),
        )
        .unwrap();
        assert!(content.contains(&file1_path));
        assert!(content.contains("content1"));
        assert!(content.contains(&file2_path));
        assert!(content.contains("content2"));
    }
}
