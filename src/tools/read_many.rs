use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use glob::glob;
use serde_json::json;
use std::fs;
use std::io::Read;
use std::path::Path;

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "fs_read_many_files".to_string(),
            description: "Reads the content of multiple files at once. You can specify a list of file paths or glob patterns. This is useful for getting a comprehensive overview of multiple files, such as all source files in a directory or a set of related configuration files.".to_string(),
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

    for path_pattern in paths {
        for entry in glob(&path_pattern)? {
            match entry {
                Ok(path) => {
                    all_paths.push(path);
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
    use tempfile::{NamedTempFile, tempdir};

    #[test]
    fn test_fs_read_many_files() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let mut file1 = NamedTempFile::new_in(root).unwrap();
        write!(file1, "content1").unwrap();
        let file1_path = file1.path().to_str().unwrap().to_string();

        let mut file2 = NamedTempFile::new_in(root).unwrap();
        write!(file2, "content2").unwrap();
        let file2_path = file2.path().to_str().unwrap().to_string();

        let content =
            fs_read_many_files(vec![file1_path.clone(), file2_path.clone()], None, None).unwrap();

        assert!(content.contains(&format!("--- {file1_path}---")));
        assert!(content.contains("content1"));
        assert!(content.contains(&format!("--- {file2_path}---")));
        assert!(content.contains("content2"));
    }

    #[test]
    fn test_fs_read_many_files_with_glob() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();

        let mut file1 = NamedTempFile::new_in(root.join("a")).unwrap();
        write!(file1, "content1").unwrap();
        let file1_path = file1.path().to_str().unwrap().to_string();

        let mut file2 = NamedTempFile::new_in(root.join("b")).unwrap();
        write!(file2, "content2").unwrap();
        let file2_path = file2.path().to_str().unwrap().to_string();

        let content = fs_read_many_files(
            vec![format!("{}/**/*", root.to_str().unwrap())],
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
