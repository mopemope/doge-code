use crate::config::AppConfig;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;
use tokio::fs;

const DESCRIPTION: &str = "Replaces a single, unique text block in a file. `target_block` must match EXACTLY and be UNIQUE in the file context. Use this for surgical edits.";

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "edit".to_string(),
            description: DESCRIPTION.to_string(),
            strict: None,
            parameters: json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string", "description": "Absolute path to the file."},
                    "target_block": {"type": "string", "description": "The exact text block to be replaced."},
                    "new_block": {"type": "string", "description": "The new text block to replace the target."},
                    "start_line": {"type": "integer", "description": "Optional: 1-based start line to restrict the search scope."},
                    "end_line": {"type": "integer", "description": "Optional: 1-based end line to restrict the search scope."},
                    "allow_multiple": {"type": "boolean", "description": "Optional: If true, replace all occurrences in the scope. Default is false."}
                },
                "required": ["file_path", "target_block", "new_block"]
            }),
        },
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EditParams {
    pub file_path: String,
    pub target_block: String,
    pub new_block: String,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub allow_multiple: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EditResult {
    pub success: bool,
    pub message: String,
    pub diff: Option<String>,
    pub lines_edited: Option<u64>,
    /// Whether the diff was trimmed to fit the response budget.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub diff_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_lines: Option<Vec<usize>>,
}

pub async fn edit(params: EditParams, config: &AppConfig) -> Result<EditResult> {
    let file_path = params.file_path;
    let target_block = params.target_block;
    let new_block = params.new_block;
    let start_line = params.start_line.unwrap_or(1);
    let end_line = params.end_line.unwrap_or(usize::MAX);
    let allow_multiple = params.allow_multiple.unwrap_or(false);

    // Ensure the path is absolute
    let path = Path::new(&file_path);
    if !path.is_absolute() {
        anyhow::bail!("File path must be absolute: {}", file_path);
    }

    // Check if the path is within the project root or in allowed paths
    let project_root = &config.project_root;
    let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    let is_allowed_path = config
        .allowed_paths
        .iter()
        .any(|allowed_path| canonical_path.starts_with(allowed_path));

    if !canonical_path.starts_with(project_root) && !is_allowed_path {
        anyhow::bail!(
            "Access to files outside the project root is not allowed: {}",
            file_path
        );
    }

    // 1. Read file content
    let original_content = fs::read_to_string(path)
        .await
        .with_context(|| format!("Failed to read file: {}", path.display()))?;

    // 2. Find matches
    let mut matches = Vec::new();
    for (start_byte, _) in original_content.match_indices(&target_block) {
        let line_num = original_content[..start_byte].lines().count() + 1;
        // Check if the match falls within the specified line range
        if line_num >= start_line && line_num <= end_line {
            matches.push((start_byte, line_num));
        }
    }

    // 3. Validate matches
    if matches.is_empty() {
        // Idempotency guard:
        // if target is gone but the new block already exists in scope, treat as already applied.
        if !new_block.is_empty() {
            let already_applied_lines: Vec<usize> = original_content
                .match_indices(&new_block)
                .filter_map(|(start_byte, _)| {
                    let line_num = original_content[..start_byte].lines().count() + 1;
                    (line_num >= start_line && line_num <= end_line).then_some(line_num)
                })
                .collect();

            if !already_applied_lines.is_empty() {
                return Ok(EditResult {
                    success: true,
                    message: format!(
                        "No change needed: target block not found, and new block already exists at lines: {}.",
                        already_applied_lines
                            .iter()
                            .map(|line| line.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    diff: None,
                    lines_edited: Some(0),
                    diff_truncated: false,
                    candidate_lines: Some(already_applied_lines),
                });
            }
        }

        return Ok(EditResult {
            success: false,
            message: "Target block not found in the file (within the specified range).".to_string(),
            diff: None,
            lines_edited: None,
            diff_truncated: false,
            candidate_lines: None,
        });
    }

    if matches.len() > 1 && !allow_multiple {
        let line_numbers: Vec<String> = matches.iter().map(|(_, line)| line.to_string()).collect();
        return Ok(EditResult {
            success: false,
            message: format!(
                "Target block is not unique. Found {} occurrences at lines: {}. Please use `start_line`/`end_line` to narrow the scope or set `allow_multiple` to true.",
                matches.len(),
                line_numbers.join(", ")
            ),
            diff: None,
            lines_edited: None,
            diff_truncated: false,
            candidate_lines: Some(matches.iter().map(|(_, line)| *line).collect()),
        });
    }

    // 4. Perform the replacement
    // We construct the new content by string building to handle multiple matches correctly
    // working backwards to keep indices valid would be one way, but since we have simple string replacement,
    // we can use standard string replacement if replacing *all*, OR we construct it manually.
    // To respect the "specific matches only" (filtered by range), we must construct manually.

    let mut modified_content = String::with_capacity(original_content.len());
    let mut last_end = 0;

    for (start_byte, _) in matches {
        // Append content from last match end to current match start
        modified_content.push_str(&original_content[last_end..start_byte]);
        // Append new block
        modified_content.push_str(&new_block);
        // Update last matches end
        last_end = start_byte + target_block.len();
    }
    // Append remaining content
    modified_content.push_str(&original_content[last_end..]);

    // 5. Generate diff for successful operation
    // Budget the diff like `apply_patch` does so large edits stay under the
    // global tool-output caps (line counting uses the full diff first).
    let diff = diffy::create_patch(&original_content, &modified_content);
    let diff_text = diff.to_string();

    // 6. Count actual lines edited by comparing the diff
    let lines_edited = count_lines_in_diff(&diff_text);

    // 7. Write the modified content back to the file
    fs::write(path, &modified_content)
        .await
        .with_context(|| format!("Failed to write to file: {}", path.display()))?;

    let budgeted_diff = crate::tools::budget::head_truncate(
        &diff_text,
        crate::tools::budget::DEFAULT_TOOL_BUDGET_CHARS,
    );

    Ok(EditResult {
        success: true,
        message: "File updated successfully.".to_string(),
        diff: Some(budgeted_diff.text),
        lines_edited: Some(lines_edited),
        diff_truncated: budgeted_diff.truncated,
        candidate_lines: None,
    })
}

/// Count the actual number of lines edited based on the diff
fn count_lines_in_diff(diff_text: &str) -> u64 {
    let mut lines_edited = 0u64;
    for line in diff_text.lines() {
        // In a unified diff, lines starting with '+' or '-' indicate changes
        if line.starts_with('+') || line.starts_with('-') {
            // Skip the header lines that start with +++ or ---
            if !line.starts_with("+++") && !line.starts_with("---") {
                lines_edited += 1;
            }
        }
    }
    lines_edited
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    async fn edit(params: super::EditParams) -> anyhow::Result<super::EditResult> {
        let config = crate::tools::test_utils::create_test_config_with_temp_dir();
        super::edit(params, &config).await
    }

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

    #[tokio::test]
    async fn test_edit_success() {
        let original_content = "Hello, world!\nThis is a test.";
        let (_temp_file, file_path) = create_temp_file(original_content);

        let params = EditParams {
            file_path: file_path.clone(),
            target_block: "world".to_string(),
            new_block: "Rust".to_string(),
            start_line: None,
            end_line: None,
            allow_multiple: None,
        };

        let result = edit(params).await.unwrap();
        assert!(result.success);
        assert_eq!(result.message, "File updated successfully.");
        assert!(result.lines_edited.is_some());

        let new_content = tokio::fs::read_to_string(file_path).await.unwrap();
        assert_eq!(new_content, "Hello, Rust!\nThis is a test.");
    }

    #[tokio::test]
    async fn test_edit_no_hash_provided() {
        let original_content = "No hash provided test.";
        let (_temp_file, file_path) = create_temp_file(original_content);

        let params = EditParams {
            file_path: file_path.clone(),
            target_block: "provided".to_string(),
            new_block: "PROVIDED".to_string(),
            start_line: None,
            end_line: None,
            allow_multiple: None,
        };

        let result = edit(params).await.unwrap();
        assert!(result.success);
        assert!(result.lines_edited.is_some());

        let new_content = tokio::fs::read_to_string(file_path).await.unwrap();
        assert_eq!(new_content, "No hash PROVIDED test.");
    }

    #[tokio::test]
    async fn test_edit_target_not_found() {
        let original_content = "Hello World";
        let (_temp_file, file_path) = create_temp_file(original_content);

        let params = EditParams {
            file_path: file_path.clone(),
            target_block: "Goodbye".to_string(),
            new_block: "Greetings".to_string(),
            start_line: None,
            end_line: None,
            allow_multiple: None,
        };

        let result = edit(params).await.unwrap();

        assert!(!result.success);
        assert!(result.message.contains("not found"));
        assert!(result.candidate_lines.is_none());
    }

    #[tokio::test]
    async fn test_edit_idempotent_when_new_block_already_present() {
        let original_content = "line1\nnew text\nline3";
        let (_temp_file, file_path) = create_temp_file(original_content);

        let params = EditParams {
            file_path: file_path.clone(),
            target_block: "old text".to_string(),
            new_block: "new text".to_string(),
            start_line: None,
            end_line: None,
            allow_multiple: None,
        };

        let result = edit(params).await.unwrap();
        assert!(result.success);
        assert_eq!(result.lines_edited, Some(0));
        assert!(result.message.contains("No change needed"));
        assert_eq!(result.candidate_lines, Some(vec![2]));
    }

    #[tokio::test]
    async fn test_edit_target_not_unique() {
        let original_content = "Hello World\nHello World";
        let (_temp_file, file_path) = create_temp_file(original_content);

        let params = EditParams {
            file_path: file_path.clone(),
            target_block: "Hello World".to_string(),
            new_block: "Greetings".to_string(),
            start_line: None,
            end_line: None,
            allow_multiple: None,
        };

        let result = edit(params).await.unwrap();

        assert!(!result.success);
        assert!(result.message.contains("Target block is not unique"));
        assert!(result.message.contains("occurrences at lines: 1, 2"));
        assert_eq!(result.candidate_lines, Some(vec![1, 2]));
    }

    #[tokio::test]
    async fn test_edit_with_line_range() {
        let original_content = "Hello World\nHello World\nHello World";
        let (_temp_file, file_path) = create_temp_file(original_content);

        // Target the second occurrence only (line 2)
        let params = EditParams {
            file_path: file_path.clone(),
            target_block: "Hello World".to_string(),
            new_block: "Greetings".to_string(),
            start_line: Some(2),
            end_line: Some(2),
            allow_multiple: None,
        };

        let result = edit(params).await.unwrap();
        assert!(result.success);

        let new_content = tokio::fs::read_to_string(file_path).await.unwrap();
        assert_eq!(new_content, "Hello World\nGreetings\nHello World");
    }

    #[tokio::test]
    async fn test_edit_allow_multiple() {
        let original_content = "foo\nfoo\nbar";
        let (_temp_file, file_path) = create_temp_file(original_content);

        let params = EditParams {
            file_path: file_path.clone(),
            target_block: "foo".to_string(),
            new_block: "baz".to_string(),
            start_line: None,
            end_line: None,
            allow_multiple: Some(true),
        };

        let result = edit(params).await.unwrap();
        assert!(result.success);

        let new_content = tokio::fs::read_to_string(file_path).await.unwrap();
        assert_eq!(new_content, "baz\nbaz\nbar");
    }

    #[tokio::test]
    async fn test_edit_allow_multiple_with_range() {
        let original_content = "foo\nfoo\nfoo";
        let (_temp_file, file_path) = create_temp_file(original_content);

        // Replace first two occurrences only
        let params = EditParams {
            file_path: file_path.clone(),
            target_block: "foo".to_string(),
            new_block: "baz".to_string(),
            start_line: Some(1),
            end_line: Some(2),
            allow_multiple: Some(true),
        };

        let result = edit(params).await.unwrap();
        assert!(result.success);

        let new_content = tokio::fs::read_to_string(file_path).await.unwrap();
        assert_eq!(new_content, "baz\nbaz\nfoo");
    }

    #[tokio::test]
    async fn test_count_lines_in_diff() {
        // Test case 1: Simple addition
        let diff_text = "---\n+++\n@@ -1,1 +1,2 @@\n Line 1\n+Line 2";
        assert_eq!(count_lines_in_diff(diff_text), 1);

        // Test case 2: Simple deletion
        let diff_text = "---\n+++\n@@ -1,2 +1,1 @@\n Line 1\n-Line 2";
        assert_eq!(count_lines_in_diff(diff_text), 1);

        // Test case 3: Modification (delete + add)
        let diff_text = "---\n+++\n@@ -1,2 +1,2 @@\n Line 1\n-Line 2\n+Line Two";
        assert_eq!(count_lines_in_diff(diff_text), 2);

        // Test case 4: Multiple changes
        let diff_text = "---\n+++\n@@ -1,3 +1,3 @@\n Line 1\n-Line 2\n+Line Two\n Line 3\n+Line 4";
        assert_eq!(count_lines_in_diff(diff_text), 3);
    }

    #[tokio::test]
    async fn test_edit_write_failure() {
        use std::os::unix::fs::PermissionsExt;
        let original_content = "Read-only test.";
        let (_temp_file, file_path) = create_temp_file(original_content);

        // Make the file read-only
        let f = std::fs::File::open(&file_path).unwrap();
        let mut perms = f.metadata().unwrap().permissions();
        perms.set_mode(0o400); // User read-only
        std::fs::set_permissions(&file_path, perms).unwrap();

        let params = EditParams {
            file_path: file_path.clone(),
            target_block: "Read-only".to_string(),
            new_block: "Writable".to_string(),
            start_line: None,
            end_line: None,
            allow_multiple: None,
        };

        // Attempting to edit a read-only file should fail
        let result = edit(params).await;

        assert!(result.is_err(), "Edit should fail on read-only file");
    }
}
