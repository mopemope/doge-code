use crate::config::AppConfig;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use crate::tools::read::FsReadMode;
use anyhow::{Context, Result};
use glob::glob;
use serde::Serialize;
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
            strict: None,
            parameters: json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "A list of absolute file paths or glob patterns."
                    },
                    "exclude": {
                        "type": ["array", "null"],
                        "items": {"type": "string"},
                        "description": "Optional substrings to exclude from the resolved path list"
                    },
                    "recursive": {
                        "type": ["boolean", "null"],
                        "description": "Whether to expand glob patterns recursively"
                    },
                    "mode": {
                        "type": ["string", "null"],
                        "enum": ["summary", "full"],
                        "description": "Summary returns small snippets by default; full streams entire files"
                    },
                    "response_budget_chars": {
                        "type": ["integer", "null"],
                        "description": "Approximate character budget for combined snippets"
                    },
                    "cursor": {
                        "type": ["integer", "null"],
                        "description": "Start index (0-based) when paging through file list"
                    },
                    "page_size": {
                        "type": ["integer", "null"],
                        "description": "How many files to include in this page"
                    },
                    "max_entries": {
                        "type": ["integer", "null"],
                        "description": "Hard cap for files per response"
                    },
                    "snippet_max_chars": {
                        "type": ["integer", "null"],
                        "description": "Maximum snippet size per file before truncation"
                    }
                },
                "required": ["paths"]
            }),
        },
    }
}

const DEFAULT_MULTI_PAGE_SIZE: usize = 5;
const DEFAULT_MULTI_SNIPPET_LINES: usize = 40;
const DEFAULT_MULTI_SNIPPET_CHARS: usize = 1_200;
const DEFAULT_MULTI_BUDGET: usize = 8_000;

#[derive(Debug, Clone, Default)]
pub struct FsReadManyOptions {
    pub mode: FsReadMode,
    pub cursor: Option<usize>,
    pub page_size: Option<usize>,
    pub max_entries: Option<usize>,
    pub response_budget_chars: Option<usize>,
    pub snippet_max_chars: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct FileSnippet {
    pub path: String,
    pub total_bytes: u64,
    pub total_lines: usize,
    pub snippet: String,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct FsReadManyResponse {
    pub files: Vec<FileSnippet>,
    pub total_files: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

pub fn fs_read_many_files(
    paths: Vec<String>,
    exclude: Option<Vec<String>>,
    _recursive: Option<bool>,
    config: &AppConfig,
    options: FsReadManyOptions,
) -> Result<FsReadManyResponse> {
    let mut warnings = Vec::new();
    let mut all_paths = Vec::new();
    let project_root = &config.project_root;

    for path_pattern in paths {
        for entry in glob(&path_pattern)? {
            match entry {
                Ok(path) => {
                    let canonical_path = path.canonicalize().unwrap_or_else(|_| path.clone());

                    let is_allowed_path = config
                        .allowed_paths
                        .iter()
                        .any(|allowed_path| canonical_path.starts_with(allowed_path));

                    if canonical_path.starts_with(project_root) || is_allowed_path {
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

    let total_files = all_paths.len();
    if total_files == 0 {
        return Ok(FsReadManyResponse {
            files: Vec::new(),
            total_files,
            next_cursor: None,
            warnings,
        });
    }

    let cursor = options.cursor.unwrap_or(0).min(total_files);
    let mut page_size = options
        .page_size
        .or(options.max_entries)
        .unwrap_or(DEFAULT_MULTI_PAGE_SIZE);
    if options.mode == FsReadMode::Full {
        page_size = options.page_size.unwrap_or(DEFAULT_MULTI_PAGE_SIZE);
    }
    if let Some(max_entries) = options.max_entries {
        page_size = page_size.min(max_entries);
    }
    if page_size == 0 {
        page_size = 1;
    }

    let end_index = (cursor + page_size).min(total_files);
    let selected = &all_paths[cursor..end_index];

    let snippet_char_cap = options
        .snippet_max_chars
        .unwrap_or(DEFAULT_MULTI_SNIPPET_CHARS);
    let mut remaining_budget = options
        .response_budget_chars
        .unwrap_or(DEFAULT_MULTI_BUDGET);

    let mut files = Vec::new();

    for path in selected {
        if !path.is_file() {
            continue;
        }
        let p = Path::new(path);
        if !p.is_absolute() {
            anyhow::bail!("Path must be absolute: {}", p.display());
        }

        let mut f = fs::File::open(p).with_context(|| format!("open {}", p.display()))?;
        let mut s = String::new();
        f.read_to_string(&mut s)
            .with_context(|| format!("read {}", p.display()))?;
        let total_lines = s.lines().count();
        let mut snippet = s.clone();
        let mut truncated = false;

        if options.mode == FsReadMode::Summary {
            let snippet_lines: Vec<&str> = s.lines().take(DEFAULT_MULTI_SNIPPET_LINES).collect();
            snippet = snippet_lines.join("\n");
            truncated = total_lines > snippet_lines.len();
        }

        if snippet.len() > snippet_char_cap {
            let mut truncate_at = snippet_char_cap;
            while truncate_at > 0 && !snippet.is_char_boundary(truncate_at) {
                truncate_at -= 1;
            }
            if truncate_at == 0 {
                truncate_at = snippet_char_cap;
            }
            snippet.truncate(truncate_at);
            snippet.push_str("\n[[TRUNCATED]]");
            truncated = true;
        }

        if snippet.len() > remaining_budget {
            warnings.push(format!(
                "response budget exceeded after {} files; request cursor={} for remaining",
                files.len(),
                cursor + files.len()
            ));
            break;
        }

        remaining_budget = remaining_budget.saturating_sub(snippet.len());

        let metadata = fs::metadata(p).with_context(|| format!("metadata {}", p.display()))?;
        files.push(FileSnippet {
            path: p.display().to_string(),
            total_bytes: metadata.len(),
            total_lines,
            snippet,
            truncated,
        });
    }

    let next_cursor = if end_index < total_files {
        Some(end_index)
    } else {
        None
    };

    Ok(FsReadManyResponse {
        files,
        total_files,
        next_cursor,
        warnings,
    })
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

        let config = crate::tools::test_utils::create_test_config_with_temp_dir();

        let response = fs_read_many_files(
            vec![file1_path.clone(), file2_path.clone()],
            None,
            None,
            &config,
            FsReadManyOptions::default(),
        )
        .unwrap();

        assert_eq!(response.files.len(), 2);
        assert_eq!(response.files[0].path, file1_path);
        assert_eq!(response.files[0].snippet.trim(), "content1");
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

        let config = crate::tools::test_utils::create_test_config_with_temp_dir();

        let response = fs_read_many_files(
            vec![format!("{}/**/*", temp_dir.to_str().unwrap())],
            None,
            Some(true),
            &config,
            FsReadManyOptions::default(),
        )
        .unwrap();
        let paths: Vec<_> = response.files.iter().map(|f| f.path.clone()).collect();
        assert!(paths.contains(&file1_path));
        assert!(paths.contains(&file2_path));
    }
}
