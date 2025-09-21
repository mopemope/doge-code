use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use tokio::fs;

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "edit".to_string(),
            description: "Edit a single, unique block of text within a file with a new block of text. Use this for simple, targeted modifications like fixing a bug in a specific line, changing a variable name within a single function, or adjusting a small code snippet. The `target_block` must be unique within the file; otherwise, the tool will return an error. You can use `dry_run: true` to preview the changes as a diff without modifying the file.".to_string(),
            strict: None,
            parameters: json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string", "description": "Absolute path to the file."},
                    "target_block": {"type": "string", "description": "The exact, unique text block to be replaced."},
                    "new_block": {"type": "string", "description": "The new text block to replace the target."},
                    "dry_run": {"type": "boolean", "description": "If true, returns the diff of the proposed change without modifying the file."}
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
    pub dry_run: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EditResult {
    pub success: bool,
    pub message: String,
    pub diff: Option<String>,
    pub lines_edited: Option<u64>,
}

pub async fn edit(params: EditParams) -> Result<EditResult> {
    let file_path = &params.file_path;
    let target_block = &params.target_block;
    let new_block = &params.new_block;
    let dry_run = params.dry_run.unwrap_or(false);

    // Ensure the path is absolute
    let path = Path::new(file_path);
    if !path.is_absolute() {
        anyhow::bail!("File path must be absolute: {}", file_path);
    }

    // Check if the path is within the project root or in allowed paths
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    let config = crate::config::AppConfig::default();
    let is_allowed_path = config
        .allowed_paths
        .iter()
        .any(|allowed_path| canonical_path.starts_with(allowed_path));

    if !canonical_path.starts_with(&project_root) && !is_allowed_path {
        anyhow::bail!(
            "Access to files outside the project root is not allowed: {}",
            file_path
        );
    }

    // 1. Read file content
    let original_content = fs::read_to_string(path)
        .await
        .with_context(|| format!("Failed to read file: {}", path.display()))?;

    // 2. Find the target block
    let occurrences = original_content.matches(target_block).count();
    if occurrences == 0 {
        return Ok(EditResult {
            success: false,
            message: "Target block not found in the file.".to_string(),
            diff: None,
            lines_edited: None,
        });
    }
    if occurrences > 1 {
        return Ok(EditResult {
            success: false,
            message: "Target block is not unique. Found multiple occurrences.".to_string(),
            diff: None,
            lines_edited: None,
        });
    }

    // 3. Perform the replacement
    let modified_content = original_content.replace(target_block, new_block);

    // 4. Generate diff for dry_run or successful operation
    let diff = diffy::create_patch(&original_content, &modified_content);
    let diff_text = diff.to_string();

    // 5. Count actual lines edited by comparing the diff
    let lines_edited = count_lines_in_diff(&diff_text);

    if dry_run {
        return Ok(EditResult {
            success: true,
            message: "Dry run successful. No changes were made.".to_string(),
            diff: Some(diff_text),
            lines_edited: Some(lines_edited),
        });
    }

    // 6. Write the modified content back to the file
    let result = fs::write(path, modified_content)
        .await
        .with_context(|| format!("Failed to write to file: {}", path.display()));

    if result.is_ok() {
        // Update session with changed file
        if let Ok(current_dir) = std::env::current_dir()
            && let Ok(relative_path) = path.strip_prefix(current_dir)
        {
            let fs_tools = crate::tools::FsTools::default();
            let _ = fs_tools.update_session_with_changed_file(relative_path.to_path_buf());
        }
    }

    Ok(EditResult {
        success: true,
        message: "File updated successfully.".to_string(),
        diff: Some(diff_text),
        lines_edited: Some(lines_edited),
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
            dry_run: Some(false),
        };

        let result = edit(params).await.unwrap();
        assert!(result.success);
        assert_eq!(result.message, "File updated successfully.");
        assert!(result.lines_edited.is_some());

        let new_content = tokio::fs::read_to_string(file_path).await.unwrap();
        assert_eq!(new_content, "Hello, Rust!\nThis is a test.");
    }

    #[tokio::test]
    async fn test_edit_dry_run() {
        let original_content = "Dry run test.";
        let (_temp_file, file_path) = create_temp_file(original_content);

        let params = EditParams {
            file_path: file_path.clone(),
            target_block: "run".to_string(),
            new_block: "RUN".to_string(),
            dry_run: Some(true),
        };

        let result = edit(params).await.unwrap();
        assert!(result.success);
        assert!(result.diff.is_some());
        assert!(result.lines_edited.is_some());

        let content_after = tokio::fs::read_to_string(file_path).await.unwrap();
        assert_eq!(content_after, original_content);
    }

    #[tokio::test]
    async fn test_edit_no_hash_provided() {
        let original_content = "No hash provided test.";
        let (_temp_file, file_path) = create_temp_file(original_content);

        let params = EditParams {
            file_path: file_path.clone(),
            target_block: "provided".to_string(),
            new_block: "PROVIDED".to_string(),
            dry_run: Some(false),
        };

        let result = edit(params).await.unwrap();
        assert!(result.success);
        assert!(result.lines_edited.is_some());

        let new_content = tokio::fs::read_to_string(file_path).await.unwrap();
        assert_eq!(new_content, "No hash PROVIDED test.");
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
}
