use crate::config::AppConfig;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::json;
use std::cmp::min;
use std::fs;
use std::io::Read;
use std::path::Path;

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
                    "limit": {"type": "integer"},
                    "cursor": {"type": "integer", "description": "Alias for start_line when paginating from previous response"},
                    "page_size": {"type": "integer", "description": "Number of lines to return (overrides limit)"},
                    "response_budget_chars": {"type": "integer", "description": "Approximate maximum characters for the snippet (default 6000)"},
                    "mode": {"type": "string", "enum": ["summary", "full"], "description": "Summary avoids long files unless full is requested"}
                },
                "required": ["path"]
            }),
        },
    }
}

const DEFAULT_SUMMARY_LINES: usize = 400;
const DEFAULT_SUMMARY_BUDGET_CHARS: usize = 6_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FsReadMode {
    #[default]
    Summary,
    Full,
}

impl FsReadMode {
    pub fn from_optional_str(value: Option<&str>) -> Self {
        match value.map(|v| v.to_ascii_lowercase()).as_deref() {
            Some("full") => FsReadMode::Full,
            _ => FsReadMode::Summary,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FsReadOptions {
    pub start_line: Option<usize>,
    pub limit: Option<usize>,
    pub cursor: Option<usize>,
    pub page_size: Option<usize>,
    pub response_budget_chars: Option<usize>,
    pub mode: FsReadMode,
}

impl Default for FsReadOptions {
    fn default() -> Self {
        Self {
            start_line: None,
            limit: None,
            cursor: None,
            page_size: None,
            response_budget_chars: None,
            mode: FsReadMode::Summary,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct FsReadResult {
    pub path: String,
    pub content: String,
    pub start_line: usize,
    pub end_line: usize,
    pub total_lines: usize,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

pub fn fs_read(path: &str, opts: FsReadOptions, config: &AppConfig) -> Result<FsReadResult> {
    let p = Path::new(path);

    // Ensure the path is absolute
    if !p.is_absolute() {
        anyhow::bail!("Path must be absolute: {}", path);
    }

    // Check if the path is within the project root or in allowed paths
    let project_root = &config.project_root;
    let canonical_path = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());

    let is_allowed_path = config
        .allowed_paths
        .iter()
        .any(|allowed_path| canonical_path.starts_with(allowed_path));

    if !canonical_path.starts_with(project_root) && !is_allowed_path {
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

    let lines: Vec<&str> = s.lines().collect();
    let total_lines = lines.len();

    let start_line = opts
        .cursor
        .or(opts.start_line)
        .unwrap_or(0)
        .min(total_lines);

    let mut line_limit = opts
        .page_size
        .or(opts.limit)
        .unwrap_or_else(|| match opts.mode {
            FsReadMode::Summary => DEFAULT_SUMMARY_LINES,
            FsReadMode::Full => total_lines.saturating_sub(start_line),
        });

    if opts.mode == FsReadMode::Full && opts.limit.is_none() && opts.page_size.is_none() {
        line_limit = total_lines.saturating_sub(start_line);
    }

    let end_line = min(start_line.saturating_add(line_limit), total_lines);

    let mut content = lines[start_line..end_line].join("\n");
    let mut truncated = end_line < total_lines;
    let mut warnings = Vec::new();

    let budget = opts
        .response_budget_chars
        .unwrap_or(DEFAULT_SUMMARY_BUDGET_CHARS);
    if !content.is_empty() && content.len() > budget {
        let mut truncate_at = budget.min(content.len());
        while truncate_at > 0 && !content.is_char_boundary(truncate_at) {
            truncate_at -= 1;
        }
        if truncate_at == 0 {
            truncate_at = budget.min(content.len());
        }
        content.truncate(truncate_at);
        content.push_str("\n[[TRUNCATED BY BUDGET]]");
        truncated = true;
        warnings.push(format!(
            "content trimmed to {} chars; rerun with higher response_budget_chars or mode='full'",
            budget
        ));
    }

    let next_cursor = if truncated { Some(end_line) } else { None };
    if truncated && warnings.is_empty() {
        warnings.push("additional content available; increase limit or request next cursor".into());
    }

    Ok(FsReadResult {
        path: path.to_string(),
        content,
        start_line,
        end_line,
        total_lines,
        truncated,
        next_cursor,
        warnings,
    })
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
        let result = crate::tools::test_utils::test_fs_read(&file_path, None, None).unwrap();
        assert_eq!(result.content, "line1\nline2\nline3");
        assert!(!result.truncated);
    }

    #[test]
    fn test_fs_read_with_start_line_limit() {
        let (_temp_file, file_path) = create_temp_file("line1\nline2\nline3\nline4");
        let result = crate::tools::test_utils::test_fs_read(&file_path, Some(1), Some(2)).unwrap();
        assert_eq!(result.content, "line2\nline3");
        assert_eq!(result.start_line, 1);
        assert_eq!(result.end_line, 3);
    }

    #[test]
    fn test_fs_read_path_escape() {
        let temp_dir = create_temp_dir();
        let file_path = temp_dir.join("../some_file");
        let file_path_str = file_path.to_str().unwrap();
        let result = fs_read(
            file_path_str,
            FsReadOptions::default(),
            &AppConfig::default(),
        );
        // Since we're now allowing absolute paths, this test might need to be adjusted
        // depending on the environment. For now, let's just check it's an error.
        assert!(result.is_err());
    }

    #[test]
    fn test_fs_read_not_a_file() {
        let temp_dir = create_temp_dir();
        let dir_path = temp_dir.to_str().unwrap();
        let result = fs_read(dir_path, FsReadOptions::default(), &AppConfig::default());
        assert!(result.is_err());
    }
}
