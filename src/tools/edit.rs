use crate::config::AppConfig;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use crate::tools::mutation::{
    MutationExecution, MutationReceipt, MutationSnapshot, MutationTargetReceipt, build_receipt,
    commit_text_candidate, mutation_changed, read_text_snapshot_async,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};

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
    #[serde(default)]
    pub changed: bool,
    pub message: String,
    pub diff: Option<String>,
    pub lines_edited: Option<u64>,
    /// Whether the diff was trimmed to fit the response budget.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub diff_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_lines: Option<Vec<usize>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Pure mutation with receipt (no session/undo/provenance side effects).
///
/// Flow: validate path -> exact before snapshot -> find target -> generate
/// candidate -> no-op check -> shared commit -> receipt. Failures
/// (missing/ambiguous target, write error, concurrent modification) produce
/// no receipt and must not touch the undo stack.
pub async fn edit_with_receipt(
    params: EditParams,
    config: &AppConfig,
) -> Result<MutationExecution<EditResult>> {
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

    // Check if the path is within the project root or in allowed paths.
    // Roots and target share one canonical-path contract so symlink-alias
    // spellings (e.g. macOS `/var` vs `/private/var`) authorize correctly.
    crate::tools::scope::ensure_in_project_scope(path, config).map_err(|e| {
        anyhow::anyhow!(
            "Access to files outside the project root is not allowed: {} ({e})",
            file_path
        )
    })?;

    // 1. Exact before snapshot (rejects directories / binary like text tools).
    let before: MutationSnapshot = read_text_snapshot_async(path)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read file {}: {e}", path.display()))?;
    let original_content = before.content.clone().unwrap_or_default();

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
                return Ok(MutationExecution {
                    result: EditResult {
                        success: true,
                        changed: false,
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
                        warnings: vec![],
                    },
                    receipt: None,
                });
            }
        }

        return Ok(MutationExecution {
            result: EditResult {
                success: false,
                changed: false,
                message: "Target block not found in the file (within the specified range)."
                    .to_string(),
                diff: None,
                lines_edited: None,
                diff_truncated: false,
                candidate_lines: None,
                warnings: vec![],
            },
            receipt: None,
        });
    }

    if matches.len() > 1 && !allow_multiple {
        let line_numbers: Vec<String> = matches.iter().map(|(_, line)| line.to_string()).collect();
        return Ok(MutationExecution {
            result: EditResult {
                success: false,
                changed: false,
                message: format!(
                    "Target block is not unique. Found {} occurrences at lines: {}. Please use `start_line`/`end_line` to narrow the scope or set `allow_multiple` to true.",
                    matches.len(),
                    line_numbers.join(", ")
                ),
                diff: None,
                lines_edited: None,
                diff_truncated: false,
                candidate_lines: Some(matches.iter().map(|(_, line)| *line).collect()),
                warnings: vec![],
            },
            receipt: None,
        });
    }

    // 4. Generate the candidate
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

    // No-op check: identical candidate commits nothing.
    if !mutation_changed(&before, &modified_content) {
        tracing::info!(file = %file_path, "mutation.noop");
        return Ok(MutationExecution {
            result: EditResult {
                success: true,
                changed: false,
                message: "No change needed: content is identical.".to_string(),
                diff: None,
                lines_edited: Some(0),
                diff_truncated: false,
                candidate_lines: None,
                warnings: vec![],
            },
            receipt: None,
        });
    }

    // 5. Shared commit (pre-write race check + atomic replace + verify).
    // A race reflects a changed world, not bad arguments: report it as an
    // unsuccessful result (no receipt, no undo, no provenance), the same
    // shape as `apply_patch`, so dispatch treats both tools alike.
    let after = match commit_text_candidate(Path::new(&file_path), &before, &modified_content).await
    {
        Ok(after) => after,
        Err(crate::tools::mutation::MutationCommitError::ConcurrentModification) => {
            return Ok(MutationExecution {
                result: EditResult {
                    success: false,
                    changed: false,
                    message:
                        "File changed during commit; refusing to overwrite. Re-read the file and retry."
                            .to_string(),
                    diff: None,
                    lines_edited: None,
                    diff_truncated: false,
                    candidate_lines: None,
                    warnings: vec![],
                },
                receipt: None,
            });
        }
        Err(other) => return Err(anyhow::anyhow!("{other}")),
    };
    let receipt: MutationReceipt = build_receipt(
        crate::provenance::ChangeKind::TextEdit,
        PathBuf::from(&file_path),
        before,
        after,
        MutationTargetReceipt::File,
    );

    // 6. Budget the public diff (provenance keeps the full diff).
    let budgeted_diff = crate::tools::budget::head_truncate(
        &receipt.diff,
        crate::tools::budget::DEFAULT_TOOL_BUDGET_CHARS,
    );

    Ok(MutationExecution {
        result: EditResult {
            success: true,
            changed: true,
            message: "File updated successfully.".to_string(),
            diff: Some(budgeted_diff.text.clone()),
            lines_edited: Some((receipt.lines_added + receipt.lines_removed) as u64),
            diff_truncated: budgeted_diff.truncated,
            candidate_lines: None,
            warnings: vec![],
        },
        receipt: Some(receipt),
    })
}

pub async fn edit(params: EditParams, config: &AppConfig) -> Result<EditResult> {
    Ok(edit_with_receipt(params, config).await?.result)
}

#[cfg(test)]
/// Count the actual number of lines edited based on the diff (test helper).
fn count_lines_in_diff(diff_text: &str) -> u64 {
    crate::tools::mutation::count_diff_lines(diff_text).0 as u64
        + crate::tools::mutation::count_diff_lines(diff_text).1 as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    fn test_config_with_root(root: &Path) -> AppConfig {
        AppConfig {
            project_root: root.to_path_buf(),
            ..AppConfig::default()
        }
    }

    async fn edit_in(dir: &Path, params: EditParams) -> anyhow::Result<super::EditResult> {
        let config = test_config_with_root(dir);
        super::edit(params, &config).await
    }

    fn create_temp_file(dir: &Path, content: &str) -> String {
        let file = tempfile::Builder::new()
            .prefix("test_")
            .suffix(".txt")
            .tempfile_in(dir)
            .unwrap();
        let path = file.path().to_str().unwrap().to_string();
        // Keep the file alive by forgetting the handle's deletion: write via path.
        let _ = file.keep();
        std::fs::write(&path, content).unwrap();
        path
    }

    #[tokio::test]
    async fn test_edit_success() {
        let dir = TempDir::new().unwrap();
        let original_content = "Hello, world!\nThis is a test.";
        let file_path = create_temp_file(dir.path(), original_content);

        let params = EditParams {
            file_path: file_path.clone(),
            target_block: "world".to_string(),
            new_block: "Rust".to_string(),
            start_line: None,
            end_line: None,
            allow_multiple: None,
        };

        let result = edit_in(dir.path(), params).await.unwrap();
        assert!(result.success);
        assert!(result.changed);
        assert_eq!(result.message, "File updated successfully.");
        assert!(result.lines_edited.is_some());

        let new_content = tokio::fs::read_to_string(file_path).await.unwrap();
        assert_eq!(new_content, "Hello, Rust!\nThis is a test.");
    }

    #[tokio::test]
    async fn test_edit_with_receipt_success() {
        let dir = TempDir::new().unwrap();
        let file_path = create_temp_file(dir.path(), "foo\n");
        let config = test_config_with_root(dir.path());
        let exec = edit_with_receipt(
            EditParams {
                file_path: file_path.clone(),
                target_block: "foo".to_string(),
                new_block: "bar".to_string(),
                start_line: None,
                end_line: None,
                allow_multiple: None,
            },
            &config,
        )
        .await
        .unwrap();
        assert!(exec.result.changed);
        let receipt = exec.receipt.expect("receipt");
        assert_eq!(receipt.kind, crate::provenance::ChangeKind::TextEdit);
        assert!(!receipt.diff.is_empty());
    }

    #[tokio::test]
    async fn test_edit_noop_identical() {
        let dir = TempDir::new().unwrap();
        let file_path = create_temp_file(dir.path(), "same\n");
        let config = test_config_with_root(dir.path());
        let exec = edit_with_receipt(
            EditParams {
                file_path,
                target_block: "same".to_string(),
                new_block: "same".to_string(),
                start_line: None,
                end_line: None,
                allow_multiple: None,
            },
            &config,
        )
        .await
        .unwrap();
        assert!(exec.result.success);
        assert!(!exec.result.changed);
        assert!(exec.receipt.is_none());
    }

    #[tokio::test]
    async fn test_edit_no_hash_provided() {
        let dir = TempDir::new().unwrap();
        let original_content = "No hash provided test.";
        let file_path = create_temp_file(dir.path(), original_content);

        let params = EditParams {
            file_path: file_path.clone(),
            target_block: "provided".to_string(),
            new_block: "PROVIDED".to_string(),
            start_line: None,
            end_line: None,
            allow_multiple: None,
        };

        let result = edit_in(dir.path(), params).await.unwrap();
        assert!(result.success);
        assert!(result.lines_edited.is_some());

        let new_content = tokio::fs::read_to_string(file_path).await.unwrap();
        assert_eq!(new_content, "No hash PROVIDED test.");
    }

    #[tokio::test]
    async fn test_edit_failures_produce_no_receipt() {
        let dir = TempDir::new().unwrap();
        let file_path = create_temp_file(dir.path(), "Hello World");
        let config = test_config_with_root(dir.path());
        // Missing target.
        let exec = edit_with_receipt(
            EditParams {
                file_path: file_path.clone(),
                target_block: "Goodbye".to_string(),
                new_block: "Greetings".to_string(),
                start_line: None,
                end_line: None,
                allow_multiple: None,
            },
            &config,
        )
        .await
        .unwrap();
        assert!(!exec.result.success);
        assert!(!exec.result.changed);
        assert!(exec.receipt.is_none());
        // Ambiguous target.
        std::fs::write(&file_path, "dup\ndup\n").unwrap();
        let exec = edit_with_receipt(
            EditParams {
                file_path,
                target_block: "dup".to_string(),
                new_block: "x".to_string(),
                start_line: None,
                end_line: None,
                allow_multiple: None,
            },
            &config,
        )
        .await
        .unwrap();
        assert!(!exec.result.success);
        assert!(exec.receipt.is_none());
    }

    #[tokio::test]
    async fn test_edit_target_not_found() {
        let dir = TempDir::new().unwrap();
        let original_content = "Hello World";
        let file_path = create_temp_file(dir.path(), original_content);

        let params = EditParams {
            file_path: file_path.clone(),
            target_block: "Goodbye".to_string(),
            new_block: "Greetings".to_string(),
            start_line: None,
            end_line: None,
            allow_multiple: None,
        };

        let result = edit_in(dir.path(), params).await.unwrap();

        assert!(!result.success);
        assert!(!result.changed);
        assert!(result.message.contains("not found"));
        assert!(result.candidate_lines.is_none());
    }

    #[tokio::test]
    async fn test_edit_idempotent_when_new_block_already_present() {
        let dir = TempDir::new().unwrap();
        let original_content = "line1\nnew text\nline3";
        let file_path = create_temp_file(dir.path(), original_content);

        let params = EditParams {
            file_path: file_path.clone(),
            target_block: "old text".to_string(),
            new_block: "new text".to_string(),
            start_line: None,
            end_line: None,
            allow_multiple: None,
        };

        let result = edit_in(dir.path(), params).await.unwrap();
        assert!(result.success);
        assert!(!result.changed);
        assert_eq!(result.lines_edited, Some(0));
        assert!(result.message.contains("No change needed"));
        assert_eq!(result.candidate_lines, Some(vec![2]));
    }

    #[tokio::test]
    async fn test_edit_target_not_unique() {
        let dir = TempDir::new().unwrap();
        let original_content = "Hello World\nHello World";
        let file_path = create_temp_file(dir.path(), original_content);

        let params = EditParams {
            file_path: file_path.clone(),
            target_block: "Hello World".to_string(),
            new_block: "Greetings".to_string(),
            start_line: None,
            end_line: None,
            allow_multiple: None,
        };

        let result = edit_in(dir.path(), params).await.unwrap();

        assert!(!result.success);
        assert!(result.message.contains("Target block is not unique"));
        assert!(result.message.contains("occurrences at lines: 1, 2"));
        assert_eq!(result.candidate_lines, Some(vec![1, 2]));
    }

    #[tokio::test]
    async fn test_edit_with_line_range() {
        let dir = TempDir::new().unwrap();
        let original_content = "Hello World\nHello World\nHello World";
        let file_path = create_temp_file(dir.path(), original_content);

        // Target the second occurrence only (line 2)
        let params = EditParams {
            file_path: file_path.clone(),
            target_block: "Hello World".to_string(),
            new_block: "Greetings".to_string(),
            start_line: Some(2),
            end_line: Some(2),
            allow_multiple: None,
        };

        let result = edit_in(dir.path(), params).await.unwrap();
        assert!(result.success);

        let new_content = tokio::fs::read_to_string(file_path).await.unwrap();
        assert_eq!(new_content, "Hello World\nGreetings\nHello World");
    }

    #[tokio::test]
    async fn test_edit_allow_multiple() {
        let dir = TempDir::new().unwrap();
        let original_content = "foo\nfoo\nbar";
        let file_path = create_temp_file(dir.path(), original_content);

        let params = EditParams {
            file_path: file_path.clone(),
            target_block: "foo".to_string(),
            new_block: "baz".to_string(),
            start_line: None,
            end_line: None,
            allow_multiple: Some(true),
        };

        let result = edit_in(dir.path(), params).await.unwrap();
        assert!(result.success);

        let new_content = tokio::fs::read_to_string(file_path).await.unwrap();
        assert_eq!(new_content, "baz\nbaz\nbar");
    }

    #[tokio::test]
    async fn test_edit_allow_multiple_with_range() {
        let dir = TempDir::new().unwrap();
        let original_content = "foo\nfoo\nfoo";
        let file_path = create_temp_file(dir.path(), original_content);

        // Replace first two occurrences only
        let params = EditParams {
            file_path: file_path.clone(),
            target_block: "foo".to_string(),
            new_block: "baz".to_string(),
            start_line: Some(1),
            end_line: Some(2),
            allow_multiple: Some(true),
        };

        let result = edit_in(dir.path(), params).await.unwrap();
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
        let dir = TempDir::new().unwrap();
        let original_content = "Read-only test.";
        let file_path = create_temp_file(dir.path(), original_content);

        // Make the file read-only
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let f = std::fs::File::open(&file_path).unwrap();
            let mut perms = f.metadata().unwrap().permissions();
            perms.set_mode(0o400); // User read-only
            std::fs::set_permissions(&file_path, perms).unwrap();
        }

        let params = EditParams {
            file_path: file_path.clone(),
            target_block: "Read-only".to_string(),
            new_block: "Writable".to_string(),
            start_line: None,
            end_line: None,
            allow_multiple: None,
        };

        // Attempting to edit a read-only file should fail for non-root.
        // Running as root bypasses permissions; accept either outcome.
        let config = test_config_with_root(dir.path());
        let result = edit_with_receipt(params, &config).await;
        match result {
            Err(e) => assert!(!e.to_string().is_empty()),
            Ok(exec) => {
                // Root: the write went through as a real mutation.
                assert!(exec.result.changed);
            }
        }
    }
}
