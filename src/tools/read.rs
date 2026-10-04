use crate::config::AppConfig;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::json;
use std::fs;
use std::io::Read;
use std::path::Path;

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "fs_read".to_string(),
            strict: None,
            description: "Reads a text file from an absolute path. Supports partial reading via `start_line`/`limit` or `mode='summary'` for large files. Returns complete lines within Unicode-character and serialized JSON budgets; full mode remains bounded. Always read files before editing.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Absolute path of the file to read."},
                    "start_line": {"type": "integer", "minimum": 1, "description": "1-based line number to start reading from. Legacy: prefer `cursor` for pagination."},
                    "limit": {"type": "integer", "minimum": 1, "description": "Maximum number of lines to return."},
                    "cursor": {"type": "integer", "minimum": 1, "description": "1-based alias for start_line when paginating from previous response"},
                    "page_size": {"type": "integer", "minimum": 1, "description": "Number of lines to return (overrides limit)"},
                    "response_budget_chars": {"type": "integer", "minimum": 1, "description": "Positive Unicode scalar character budget including line separators (default 6000, capped at 40000; JSON overhead may reduce a page)"},
                    "mode": {"type": "string", "enum": ["summary", "full"], "description": "Summary limits line count; full considers all lines but remains budgeted"}
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

    // Check if the path is within the project root or in allowed paths.
    // Roots and target share one canonical-path contract so symlink-alias
    // spellings (e.g. macOS `/var` vs `/private/var`) authorize correctly.
    crate::tools::scope::ensure_in_project_scope(p, config).map_err(|e| {
        anyhow::anyhow!(
            "Access to files outside the project root is not allowed: {} ({e})",
            path
        )
    })?;

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

    for (name, value) in [
        ("start_line", opts.start_line),
        ("cursor", opts.cursor),
        ("limit", opts.limit),
        ("page_size", opts.page_size),
        ("response_budget_chars", opts.response_budget_chars),
    ] {
        super::budget::positive_read_option(name, value)?;
    }
    let requested = opts.cursor.or(opts.start_line).unwrap_or(1);
    let start_index = requested.saturating_sub(1).min(total_lines);
    let line_limit = opts.page_size.or(opts.limit).unwrap_or(match opts.mode {
        FsReadMode::Summary => DEFAULT_SUMMARY_LINES,
        FsReadMode::Full => total_lines.saturating_sub(start_index),
    });
    let requested_budget = opts
        .response_budget_chars
        .unwrap_or(DEFAULT_SUMMARY_BUDGET_CHARS);
    let budget = requested_budget.min(super::budget::READ_TOOL_OUTPUT_MAX_CHARS);
    let mut warnings = Vec::new();
    if requested_budget > budget {
        warnings.push(
            "response_budget_chars capped at 40000; serialized JSON overhead also applies".into(),
        );
    }
    let mut end_index = start_index;
    let mut chars = 0usize;
    for line in &lines[start_index..start_index.saturating_add(line_limit).min(total_lines)] {
        let needed = line
            .chars()
            .count()
            .saturating_add(usize::from(end_index > start_index));
        if needed > budget.saturating_sub(chars) {
            if end_index == start_index {
                anyhow::bail!(
                    "response_budget_chars too small: line {requested} requires at least {} Unicode characters; retry from the same start_line={requested} with a larger budget (maximum 40000, including a separate serialized JSON limit). Single-line offset pagination is unavailable",
                    line.chars().count()
                );
            }
            break;
        }
        chars += needed;
        end_index += 1;
    }
    let mut result = FsReadResult {
        path: path.to_string(),
        content: lines[start_index..end_index].join("\n"),
        start_line: if total_lines == 0 { 0 } else { requested },
        end_line: end_index,
        total_lines,
        truncated: end_index < total_lines,
        next_cursor: (end_index < total_lines).then(|| end_index.saturating_add(1)),
        warnings,
    };
    if result.truncated {
        result
            .warnings
            .push("additional complete lines available; request next_cursor".into());
    }
    if !super::budget::read_result_fits(&result)? {
        anyhow::ensure!(
            end_index > start_index,
            "read result metadata exceeds the 40000-character serialized JSON limit; use a shorter path"
        );
        result
            .warnings
            .push("serialized JSON limit applied; request next_cursor for unread lines".into());
        // Escaping may expand thousands of tiny lines. Find a complete prefix
        // in logarithmic serialization passes rather than deleting one at a time.
        let mut low = start_index;
        let mut high = end_index - 1;
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            set_page_end(&mut result, &lines, start_index, mid);
            if super::budget::read_result_fits(&result)? {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        set_page_end(&mut result, &lines, start_index, low);
        anyhow::ensure!(
            low > start_index && super::budget::read_result_fits(&result)?,
            "line {requested} cannot fit the 40000-character serialized JSON limit with its escaping and metadata; no lines consumed, retry the same start_line after reducing line size or path length. Single-line offset pagination is unavailable"
        );
    }
    Ok(result)
}

fn set_page_end(result: &mut FsReadResult, lines: &[&str], start: usize, end: usize) {
    result.content = lines[start..end].join("\n");
    result.end_line = end;
    result.truncated = end < result.total_lines;
    result.next_cursor = result.truncated.then(|| end.saturating_add(1));
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
        let result = crate::tools::test_utils::test_fs_read(&file_path, Some(2), Some(2)).unwrap();
        assert_eq!(result.content, "line2\nline3");
        assert_eq!(result.start_line, 2);
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
