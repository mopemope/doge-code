use crate::{
    config::{AppConfig, IGNORE_FILE},
    llm::types::{ToolDef, ToolFunctionDef},
    utils::get_git_repository_root,
};
use anyhow::Result;
use serde::Serialize;
use serde_json::json;
use std::path::{Path, PathBuf};
use tracing::debug;

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "fs_list".to_string(),
            description: "Lists files and directories within a specified path. You can limit the depth of recursion and filter results by a glob pattern. The default maximum depth is 1. This tool is useful for exploring the project structure, finding specific files, or getting an overview of the codebase before starting a task. For example, use it to see what files are in a directory or to find all `.rs` files.".to_string(),
            strict: None,
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Absolute path of the directory to list."},
                    "max_depth": {"type": "integer", "description": "Maximum depth to traverse (default: 1)."},
                    "pattern": {"type": "string", "description": "Glob pattern to filter entries (e.g. '*.rs')."},
                    "mode": {"type": "string", "enum": ["summary", "full"], "description": "Summary limits entries and budget automatically"},
                    "response_budget_chars": {"type": "integer", "description": "Approximate maximum characters to return"},
                    "cursor": {"type": "integer", "description": "Use this to continue listing from the previous response"},
                    "page_size": {"type": "integer", "description": "Maximum entries to include in this response"},
                    "max_entries": {"type": "integer", "description": "Absolute cap on entries returned"}
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
const DEFAULT_LIST_PAGE_SIZE: usize = 200;
/// Must stay below the 8,000-char global truncation cap, including per-entry
/// JSON overhead.
const DEFAULT_LIST_BUDGET: usize = 6_000;
/// Approximate serialized JSON overhead per entry (`{"path":"..","is_dir":..}`).
const PER_ENTRY_OVERHEAD: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FsListMode {
    #[default]
    Summary,
    Full,
}

impl FsListMode {
    pub fn from_optional_str(value: Option<&str>) -> Self {
        match value.map(|v| v.to_ascii_lowercase()).as_deref() {
            Some("full") => FsListMode::Full,
            _ => FsListMode::Summary,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct FsListOptions {
    pub mode: FsListMode,
    pub cursor: Option<usize>,
    pub page_size: Option<usize>,
    pub max_entries: Option<usize>,
    pub response_budget_chars: Option<usize>,
}

#[derive(Serialize, Debug, Clone)]
pub struct FsListEntry {
    pub path: String,
    pub is_dir: bool,
}

#[derive(Serialize, Debug)]
pub struct FsListResponse {
    pub entries: Vec<FsListEntry>,
    pub total_entries: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

pub fn fs_list(
    path: &str,
    max_depth: Option<usize>,
    pattern: Option<&str>,
    config: &AppConfig,
    options: FsListOptions,
) -> Result<FsListResponse> {
    let p = Path::new(path);

    // Ensure the path is absolute
    if !p.is_absolute() {
        anyhow::bail!("Path must be absolute: {}", path);
    }

    // Check if the path exists
    if !p.exists() {
        // If the path doesn't exist, return an empty list instead of an error
        return Ok(FsListResponse {
            entries: Vec::new(),
            total_entries: 0,
            next_cursor: None,
            warnings: Vec::new(),
        });
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
                files.push(FsListEntry {
                    path: path.to_string_lossy().to_string(),
                    is_dir: entry.file_type().map(|t| t.is_dir()).unwrap_or(false),
                });
            }
            Err(e) => {
                debug!("Error walking directory: {}", e);
            }
        }
    }

    let total_entries = files.len();
    if total_entries == 0 {
        return Ok(FsListResponse {
            entries: Vec::new(),
            total_entries,
            next_cursor: None,
            warnings: Vec::new(),
        });
    }

    let cursor = options.cursor.unwrap_or(0).min(total_entries);
    let default_page = match options.mode {
        FsListMode::Summary => DEFAULT_LIST_PAGE_SIZE,
        FsListMode::Full => total_entries.saturating_sub(cursor).max(1),
    };
    let mut page_size = options
        .page_size
        .or(options.max_entries)
        .unwrap_or(default_page);
    if page_size == 0 {
        page_size = 1;
    }
    if let Some(max_entries) = options.max_entries {
        page_size = page_size.min(max_entries);
    }

    let end_index = (cursor + page_size).min(total_entries);
    let mut remaining_budget = options.response_budget_chars.unwrap_or(DEFAULT_LIST_BUDGET);
    let mut warnings = Vec::new();

    let mut entries = Vec::new();
    for entry in &files[cursor..end_index] {
        let entry_cost = entry.path.len() + PER_ENTRY_OVERHEAD;
        if entry_cost > remaining_budget {
            warnings
                .push("response budget reached; request next cursor for remaining entries".into());
            break;
        }
        remaining_budget = remaining_budget.saturating_sub(entry_cost);
        entries.push(entry.clone());
    }

    // If the budget cut the page short, resume from the last emitted entry so
    // no entries are skipped between pages. If even the first entry did not
    // fit the budget, still emit it and advance the cursor — otherwise the
    // client would re-request the same cursor forever.
    let budget_cut_short = entries.len() < end_index - cursor;
    if budget_cut_short && entries.is_empty() && end_index > cursor {
        let entry = &files[cursor];
        entries.push(entry.clone());
        warnings.push(
            "single entry exceeds response budget; emitting it anyway to keep pagination moving"
                .into(),
        );
    }

    let next_cursor = if budget_cut_short && !entries.is_empty() {
        Some(cursor + entries.len())
    } else if end_index < total_entries {
        Some(end_index)
    } else {
        None
    };

    Ok(FsListResponse {
        entries,
        total_entries,
        next_cursor,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn create_temp_dir() -> PathBuf {
        let temp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("temp");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let dir = tempfile::Builder::new()
            .prefix("test_")
            .tempdir_in(&temp_dir)
            .unwrap();
        #[allow(deprecated)]
        dir.into_path()
    }

    #[test]
    fn test_fs_list_simple() {
        let root = create_temp_dir();
        fs::create_dir(root.join("a")).unwrap();
        fs::write(root.join("a/b.txt"), "").unwrap();
        fs::write(root.join("c.txt"), "").unwrap();

        let root_str = root.to_str().unwrap();
        let config = crate::tools::test_utils::create_test_config_with_temp_dir();
        // With max_depth=1, we should only see direct children of the root.
        let response = fs_list(root_str, Some(1), None, &config, FsListOptions::default()).unwrap();
        let mut expected = vec![
            root_str.to_string(),
            format!("{}/a", root_str),
            format!("{}/c.txt", root_str),
        ];
        expected.sort();
        let mut got: Vec<_> = response.entries.iter().map(|e| e.path.clone()).collect();
        got.sort();

        assert_eq!(got, expected);
    }

    #[test]
    fn test_fs_list_with_depth() {
        let root = create_temp_dir();
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("a/b/c.txt"), "").unwrap();

        let root_str = root.to_str().unwrap();
        let config = crate::tools::test_utils::create_test_config_with_temp_dir();
        let result = fs_list(root_str, Some(3), None, &config, FsListOptions::default()).unwrap();
        let paths: Vec<_> = result.entries.iter().map(|e| e.path.clone()).collect();
        assert!(paths.contains(&root_str.to_string()));
        assert!(paths.contains(&format!("{root_str}/a")));
        assert!(paths.contains(&format!("{root_str}/a/b")));
        assert!(paths.contains(&format!("{root_str}/a/b/c.txt")));
    }

    #[test]
    fn test_fs_list_budget_cut_resumes_without_skipping_entries() {
        let root = create_temp_dir();
        for i in 0..50 {
            fs::write(root.join(format!("file_{i:03}.txt")), "").unwrap();
        }

        let root_str = root.to_str().unwrap();
        let config = crate::tools::test_utils::create_test_config_with_temp_dir();
        let options = FsListOptions {
            page_size: Some(50),
            response_budget_chars: Some(600),
            ..Default::default()
        };

        let mut seen = std::collections::HashSet::new();
        let mut cursor = None;
        let mut pages = 0;
        loop {
            let response = fs_list(
                root_str,
                Some(1),
                None,
                &config,
                FsListOptions { cursor, ..options },
            )
            .unwrap();
            let got: Vec<_> = response
                .entries
                .iter()
                .map(|e| e.path.clone())
                .filter(|p| !seen.insert(p.clone()))
                .collect();
            assert!(got.is_empty(), "duplicate/skipped entries: {got:?}");
            pages += 1;
            match response.next_cursor {
                Some(next) if pages < 100 => cursor = Some(next),
                _ => break,
            }
        }
        // All 50 files (+ root dir entry) must be reachable via pagination.
        assert!(seen.len() >= 51, "expected all entries, got {}", seen.len());
        assert!(pages > 1, "budget should force multiple pages");
    }

    #[test]
    fn test_fs_list_budget_too_small_for_entry_still_advances() {
        let root = create_temp_dir();
        fs::write(root.join("a_very_long_file_name_here.txt"), "").unwrap();
        fs::write(root.join("b.txt"), "").unwrap();

        let root_str = root.to_str().unwrap();
        let config = crate::tools::test_utils::create_test_config_with_temp_dir();
        // Budget smaller than a single entry's path + overhead.
        let response = fs_list(
            root_str,
            Some(1),
            None,
            &config,
            FsListOptions {
                page_size: Some(2),
                response_budget_chars: Some(50),
                ..Default::default()
            },
        )
        .unwrap();

        // Must not return an empty page with a self-referencing cursor.
        assert!(
            !response.entries.is_empty(),
            "empty page would loop forever on the same cursor"
        );
        if let Some(next) = response.next_cursor {
            assert!(next > 0, "cursor must advance past the emitted entries");
        }
        assert!(!response.warnings.is_empty());
    }

    #[test]
    fn test_fs_list_with_pattern() {
        let root = create_temp_dir();
        fs::write(root.join("a.txt"), "").unwrap();
        fs::write(root.join("b.log"), "").unwrap();

        let root_str = root.to_str().unwrap();
        let config = crate::tools::test_utils::create_test_config_with_temp_dir();
        let result = fs_list(
            root_str,
            None,
            Some("*.txt"),
            &config,
            FsListOptions::default(),
        )
        .unwrap();
        let paths: Vec<_> = result.entries.iter().map(|e| e.path.clone()).collect();
        assert!(paths.contains(&root_str.to_string()));
        assert!(paths.contains(&format!("{root_str}/a.txt")));
        assert!(!paths.contains(&format!("{root_str}/b.log")));
    }
}
