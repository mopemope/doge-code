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
            description: "Reads multiple files at once via paths or globs. Efficient for context gathering. Returns file summaries with actual unread-path cursors; use fs_read for truncated file content.".to_string(),
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
                        "description": "Summary limits snippets to 40 lines; full skips that line cap but still obeys all character and serialized JSON budgets"
                    },
                    "response_budget_chars": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "description": "Positive Unicode scalar character budget for combined snippets (default 8000, capped at 40000; JSON overhead may reduce output)"
                    },
                    "cursor": {
                        "type": ["integer", "null"],
                        "minimum": 0,
                        "description": "Start index (0-based) when paging through file list"
                    },
                    "page_size": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "description": "How many files to include in this page"
                    },
                    "max_entries": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "description": "Hard cap for files per response"
                    },
                    "snippet_max_chars": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "description": "Positive Unicode scalar character cap per file (default 1200, at most 40000; truncation is marked in metadata)"
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
    for (name, value) in [
        ("page_size", options.page_size),
        ("max_entries", options.max_entries),
        ("response_budget_chars", options.response_budget_chars),
        ("snippet_max_chars", options.snippet_max_chars),
    ] {
        super::budget::positive_read_option(name, value)?;
    }
    let mut warnings = Vec::new();
    let mut all_paths = Vec::new();

    for path_pattern in paths {
        for entry in glob(&path_pattern)? {
            match entry {
                Ok(path) => {
                    // Roots and target share one canonical-path contract.
                    if crate::tools::scope::ensure_in_project_scope(&path, config).is_ok() {
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
    let cursor = options.cursor.unwrap_or(0).min(total_files);
    let page_size = options
        .page_size
        .or(options.max_entries)
        .unwrap_or(DEFAULT_MULTI_PAGE_SIZE)
        .min(options.max_entries.unwrap_or(usize::MAX));
    let end_index = cursor.saturating_add(page_size).min(total_files);
    let requested_budget = options
        .response_budget_chars
        .unwrap_or(DEFAULT_MULTI_BUDGET);
    let mut remaining_budget = requested_budget.min(super::budget::READ_TOOL_OUTPUT_MAX_CHARS);
    let snippet_cap = options
        .snippet_max_chars
        .unwrap_or(DEFAULT_MULTI_SNIPPET_CHARS)
        .min(super::budget::READ_TOOL_OUTPUT_MAX_CHARS);
    if requested_budget > remaining_budget
        || options
            .snippet_max_chars
            .is_some_and(|cap| cap > snippet_cap)
    {
        warnings.push(
            "character budgets capped at 40000; serialized JSON overhead also applies".into(),
        );
    }
    let mut response = FsReadManyResponse {
        files: Vec::new(),
        total_files,
        next_cursor: (cursor < total_files).then_some(cursor),
        warnings,
    };
    let mut consumed = cursor;
    let mut file_indices = Vec::new();
    for (index, path) in all_paths.iter().enumerate().take(end_index).skip(cursor) {
        if !path.is_file() {
            consumed = index + 1;
            continue;
        }
        let p = Path::new(path);
        anyhow::ensure!(p.is_absolute(), "Path must be absolute");
        let mut f = fs::File::open(p).with_context(|| format!("open {}", p.display()))?;
        let mut s = String::new();
        f.read_to_string(&mut s)
            .with_context(|| format!("read {}", p.display()))?;
        let total_lines = s.lines().count();
        let snippet = if options.mode == FsReadMode::Summary {
            s.lines()
                .take(DEFAULT_MULTI_SNIPPET_LINES)
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            s
        };
        let snippet_chars = snippet.chars().count();
        let mut wanted = snippet_chars.min(snippet_cap);
        if wanted > remaining_budget {
            if !response.files.is_empty() {
                break;
            }
            wanted = remaining_budget;
        }
        let metadata = fs::metadata(p).with_context(|| format!("metadata {}", p.display()))?;
        response.files.push(FileSnippet {
            path: p.display().to_string(),
            total_bytes: metadata.len(),
            total_lines,
            snippet: super::budget::safe_take_chars(&snippet, wanted).to_owned(),
            truncated: wanted < snippet_chars
                || (options.mode == FsReadMode::Summary
                    && total_lines > DEFAULT_MULTI_SNIPPET_LINES),
        });
        response.next_cursor = (index + 1 < total_files).then_some(index + 1);
        // Include continuation guidance in the measured payload, not in snippet text.
        if response.files.iter().any(|file| file.truncated)
            && !response.warnings.iter().any(|w| w.starts_with("truncated"))
        {
            response.warnings.push("truncated snippets are file summaries; use fs_read on the file for remaining lines".into());
        }
        if !super::budget::read_result_fits(&response)? {
            if response.files.len() > 1 {
                response.files.pop();
                response.next_cursor = Some(index);
                break;
            }
            // A first-file summary may be shortened safely; never consume a file
            // whose metadata or even one available character cannot fit.
            let mut low = 0;
            let mut high = wanted;
            while low < high {
                let mid = low + (high - low).div_ceil(2);
                response.files[0].snippet =
                    super::budget::safe_take_chars(&snippet, mid).to_owned();
                response.files[0].truncated = true;
                if !response.warnings.iter().any(|w| w.starts_with("truncated")) {
                    response.warnings.push("truncated snippets are file summaries; use fs_read on the file for remaining lines".into());
                }
                if super::budget::read_result_fits(&response)? {
                    low = mid;
                } else {
                    high = mid - 1;
                }
            }
            wanted = low;
            response.files[0].snippet = super::budget::safe_take_chars(&snippet, wanted).to_owned();
            response.files[0].truncated = true;
            anyhow::ensure!(
                super::budget::read_result_fits(&response)? && (wanted > 0 || snippet_chars == 0),
                "first file cannot fit the 40000-character serialized JSON limit; no file consumed, use fs_read with a shorter path"
            );
        }
        remaining_budget -= wanted;
        file_indices.push(index);
        consumed = index + 1;
    }
    response.next_cursor = (consumed < total_files).then_some(consumed);
    // A later truncation warning or cursor can add overhead. Restore the actual
    // first removed path index if that pushes the final envelope over its cap.
    while !super::budget::read_result_fits(&response)? {
        anyhow::ensure!(
            response.files.len() > 1,
            "read result metadata exceeds the 40000-character serialized JSON limit; use shorter paths"
        );
        response.files.pop();
        response.next_cursor = file_indices.pop();
    }
    Ok(response)
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
