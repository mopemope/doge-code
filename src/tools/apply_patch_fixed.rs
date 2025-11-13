use crate::config::AppConfig;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use diffy;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;
use tokio::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(not(unix))]
use tokio::io::AsyncWriteExt;

// ===== Public Data Structures =====

/// apply_patch tool's input parameters
#[derive(Debug, Serialize, Deserialize)]
pub struct ApplyPatchParams {
    /// Absolute path to the target file
    pub file_path: String,
    /// Unified diff patch content
    pub patch_content: String,
}

/// apply_patch tool's output result
#[derive(Debug, Serialize, Deserialize)]
pub struct ApplyPatchResult {
    /// Whether the patch was applied successfully
    pub success: bool,
    /// Result message
    pub message: String,
    /// Original file content (included in both success and failure cases)
    pub original_content: Option<String>,
    /// Modified file content (success case only)
    pub modified_content: Option<String>,
}

// ===== Tool Definition =====

/// Returns the tool definition for apply_patch
pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "apply_patch".to_string(),
            description: create_tool_description(),
            strict: None,
            parameters: create_tool_parameters(),
        },
    }
}

/// Creates the detailed tool description
fn create_tool_description() -> String {
    r#"Applies a unified diff patch to a file.

REQUIRED PARAMETERS:
- file_path: ABSOLUTE path to target file (e.g., '/home/user/project/src/main.rs')
- patch_content: Unified diff content in proper format

CRITICAL RULES:
1. ALWAYS read current file content with fs_read first
2. Context lines (starting with ' ') must EXACTLY match current file content  
3. Use proper unified diff format with correct @@ line numbers
4. NEVER use relative paths - always use absolute paths starting with project root

WORKFLOW:
fs_read → analyze current content → create precise diff → apply_patch

EXAMPLE (CORRECT):
```diff
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,3 @@
 fn main() {
-    println!("Hello");
+    println!("Hello, world!");
 }
```

FAILURE MODES & SOLUTIONS:
- 'Context lines do not match': File content changed - re-read file and create new patch
- 'File path must be absolute': Use absolute path like '/project/src/main.rs', not 'src/main.rs'
- 'Failed to parse patch content': Check unified diff format syntax
- 'Failed to write to file': Check file permissions and ensure write access

COMMON MISTAKES TO AVOID:
- ❌ Using relative paths: 'src/main.rs'
- ❌ Creating patch before reading current file content
- ❌ Insufficient context lines (need 3-5 lines before/after change)
- ❌ Incorrect line numbers in @@ headers
- ❌ Whitespace differences in context lines"#
        .to_string()
}

/// Creates the tool parameters schema
fn create_tool_parameters() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "file_path": {
                "type": "string",
                "description": "ABSOLUTE path to target file. Must start with project root. Example: '/home/user/project/src/main.rs'. NEVER use relative paths like 'src/main.rs'."
            },
            "patch_content": {
                "type": "string", 
                "description": "Unified diff content. Must use proper format with @@ line numbers. Context lines (starting with ' ') must exactly match current file content. Example: '--- a/src/main.rs\\\\n+++ b/src/main.rs\\\\n@@ -1,3 +1,3 @@\\\\n fn main() {\\\\n-    println!(\\\"Hello\\\");\\\\n+    println!(\\\"Hello, world!\\\");\\\\n }'"
            }
        },
        "required": ["file_path", "patch_content"]
    })
}

// ===== Public Interface =====

/// Main interface function for apply_patch tool
pub async fn apply_patch(params: ApplyPatchParams, config: &AppConfig) -> Result<ApplyPatchResult> {
    apply_patch_impl(params, config).await
}

// ===== Core Implementation =====

/// Main implementation of apply_patch
async fn apply_patch_impl(
    params: ApplyPatchParams,
    config: &AppConfig,
) -> Result<ApplyPatchResult> {
    let path = Path::new(¶ms.file_path);
    
    // Step 1: Validate file path and access
    validate_file_path_and_access(¶ms.file_path, config).await?;
    
    // Step 2: Validate file exists and is readable
    validate_file_exists_and_readable(path).await?;
    
    // Step 3: Read file content
    let original_content_raw = fs::read_to_string(path)
        .await
        .with_context(|| format!("Failed to read file: {}", path.display()))?;
    
    // Step 4: Normalize line endings
    let (original_content, has_crlf) = normalize_line_endings(&original_content_raw);
    
    // Step 5: Parse patch
    let patch = match parse_patch(¶ms.patch_content) {
        Ok(patch) => patch,
        Err(e) => {
            return Ok(ApplyPatchResult {
                success: false,
                message: format!("Failed to parse patch content: {}", e),
                original_content: Some(original_content_raw),
                modified_content: None,
            });
        }
    };
    
    // Step 6: Check for empty changes
    if is_empty_patch(&patch, ¶ms.patch_content) {
        return Ok(ApplyPatchResult {
            success: false,
            message: "Patch content is invalid or results in no changes.".to_string(),
            original_content: Some(original_content_raw.clone()),
            modified_content: Some(original_content_raw.clone()),
        });
    }
    
    // Step 7: Apply patch
    let patched_content_lf = apply_patch_to_content(&original_content, &patch, path)?;
    
    // Step 8: Restore original line endings
    let patched_content = if has_crlf {
        patched_content_lf.replace('\n', "\r\n")
    } else {
        patched_content_lf
    };
    
    // Step 9: Validate write permissions
    validate_write_permissions(path).await?;
    
    // Step 10: Write file
    fs::write(path, &patched_content)
        .await
        .with_context(|| format!("Failed to write to file: {}", path.display()))?;
    
    // Step 11: Verify write
    verify_file_content(path, &patched_content).await?;
    
    // Step 12: Update session
    update_session_with_changed_file(path).await;
    
    // Step 13: Return success result
    Ok(ApplyPatchResult {
        success: true,
        message: "File patched successfully.".to_string(),
        original_content: Some(original_content_raw),
        modified_content: Some(patched_content),
    })
}