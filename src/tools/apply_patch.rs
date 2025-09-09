use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;

pub mod apply;
use apply::apply_patch as apply_impl;

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "apply_patch".to_string(),
            description: "Atomically applies a patch to a file in the unified diff format. This is a powerful and safe way to perform complex, multi-location edits.

This tool is typically used in a sequence:
1. Read the original file content and its hash using `fs_read`.
2. Generate the desired `modified_content`.
3. Generate the `patch_content`.
4. Call this tool, `apply_patch`, with the `patch_content` and the original hash to safely modify the file.

Arguments:
- `file_path` (string, required): The absolute path to the file you want to modify.
- `patch_content` (string, required): The patch to apply, formatted as a unified diff. Example:
  ```diff
  --- a/original_file.txt
  +++ b/modified_file.txt
  @@ -1,3 +1,3 @@
   line 1
  -line 2 to be removed
  +line 2 to be added
   line 3
  ```
- `dry_run` (boolean, optional): If `true`, the tool will check if the patch can be applied cleanly and show the potential result without actually modifying the file. Defaults to `false`.

Returns a detailed result object, indicating success or failure with a descriptive message.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string", "description": "Absolute path to the file."},
                    "patch_content": {"type": "string", "description": "The patch content in the unified diff format."},
                    "dry_run": {"type": "boolean", "description": "If true, checks if the patch can be applied cleanly without modifying the file."}
                },
                "required": ["file_path", "patch_content"]
            }),
        },
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApplyPatchParams {
    pub file_path: String,
    pub patch_content: String,
    pub dry_run: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApplyPatchResult {
    pub success: bool,
    pub message: String,
    pub original_content: Option<String>,
    pub modified_content: Option<String>,
}

pub async fn apply_patch(params: ApplyPatchParams) -> Result<ApplyPatchResult> {
    apply_impl(params).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::{NamedTempFile, tempdir};
    use tokio::fs;

    fn create_patch_content(original: &str, modified: &str) -> String {
        let patch = diffy::create_patch(original, modified);
        patch.to_string()
    }

    #[tokio::test]
    async fn test_apply_patch_success() {
        let original_content = "Hello, world!\nThis is the original file.\n";
        let modified_content = "Hello, Rust!\nThis is the modified file.\n";

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(original_content.as_bytes()).unwrap();
        let file_path = temp_file.path().to_str().unwrap().to_string();

        let patch_content = create_patch_content(original_content, modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
            dry_run: Some(false),
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);
        assert_eq!(result.message, "File patched successfully.");

        let final_content = fs::read_to_string(file_path).await.unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_apply_patch_dry_run() {
        let original_content = "Line 1\nLine 2\n";
        let modified_content = "Line 1\nLine Two\n";

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(original_content.as_bytes()).unwrap();
        let file_path = temp_file.path().to_str().unwrap().to_string();

        let patch_content = create_patch_content(original_content, modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
            dry_run: Some(true),
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);
        assert_eq!(
            result.message,
            "Dry run successful. Patch can be applied cleanly."
        );
        assert_eq!(result.original_content.unwrap(), original_content);
        assert_eq!(result.modified_content.unwrap(), modified_content);

        // Ensure the original file was not changed
        let final_content = fs::read_to_string(file_path).await.unwrap();
        assert_eq!(final_content, original_content);
    }

    #[tokio::test]
    async fn test_create_and_apply_patch_integration() {
        let original_content = "line 1\nline 2\nline 3\n";
        let modified_content = "line 1\nline two\nline 3\n";

        // Create a temporary file with the original content
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(original_content.as_bytes()).unwrap();
        let file_path = temp_file.path().to_str().unwrap().to_string();

        // 1. Create the patch
        let patch_content = create_patch_content(original_content, modified_content);
        assert!(patch_content.contains("-line 2"));
        assert!(patch_content.contains("+line two"));

        // 2. Apply the patch
        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
            dry_run: Some(false),
        };
        let result = apply_patch(params).await.unwrap();

        // 3. Verify the result
        assert!(result.success);
        let final_content = fs::read_to_string(file_path).await.unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_apply_patch_conflict() {
        let original_content = "line A\nline B\nline C\n";
        let modified_content = "line A\nline Bee\nline C\n";
        let actual_content_in_file = "line A\nline Z\nline C\n"; // This is different from original_content

        // Create a temporary file with the "actual" content
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file
            .write_all(actual_content_in_file.as_bytes())
            .unwrap();
        let file_path = temp_file.path().to_str().unwrap().to_string();

        // 1. Create the patch based on the "original" content
        let patch_content = create_patch_content(original_content, modified_content);

        // 2. Attempt to apply the patch to the "actual" content
        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
            // Note: We use the hash of the *actual* content for the check to pass
            dry_run: Some(false),
        };
        let result = apply_patch(params).await.unwrap();

        // 3. Verify that the patch application failed due to content mismatch
        assert!(!result.success);
        assert!(result.message.contains("Failed to apply patch"));

        // Ensure the file content remains unchanged
        let final_content = fs::read_to_string(file_path).await.unwrap();
        assert_eq!(final_content, actual_content_in_file);
    }

    #[tokio::test]
    async fn test_apply_patch_requires_absolute_path() {
        let params = ApplyPatchParams {
            file_path: "relative/path/to/file.txt".to_string(),
            patch_content: "any patch".to_string(),
            dry_run: Some(false),
        };

        let result = apply_patch(params).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("File path must be absolute")
        );
    }

    #[tokio::test]
    async fn test_apply_patch_with_crlf_line_endings() {
        let original_content = "first line\r\nsecond line\r\n";
        let modified_content = "first line\r\nsecond line modified\r\n";

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(original_content.as_bytes()).unwrap();
        let file_path = temp_file.path().to_str().unwrap().to_string();

        // Create patch using LF-normalized content, as our tool now handles this internally
        let patch_content = create_patch_content(
            &original_content.replace("\r\n", "\n"),
            &modified_content.replace("\r\n", "\n"),
        );

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
            dry_run: Some(false),
        };

        let result = apply_patch(params).await.unwrap();
        assert!(
            result.success,
            "Patch should apply cleanly. Message: {}",
            result.message
        );

        let final_content = fs::read_to_string(file_path).await.unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_apply_patch_no_newline_at_end_of_file() {
        let original_content = "hello";
        let modified_content = "hello world";

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(original_content.as_bytes()).unwrap();
        let file_path = temp_file.path().to_str().unwrap().to_string();

        let patch_content = create_patch_content(original_content, modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
            dry_run: Some(false),
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);

        let final_content = fs::read_to_string(file_path).await.unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_apply_patch_large_change() {
        let original_content = "line\n".repeat(100);
        let modified_content = "changed line\n".repeat(100);

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(original_content.as_bytes()).unwrap();
        let file_path = temp_file.path().to_str().unwrap().to_string();

        let patch_content = create_patch_content(&original_content, &modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
            dry_run: Some(false),
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);

        let final_content = fs::read_to_string(file_path).await.unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_apply_patch_to_non_existent_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("non_existent_file.txt");
        let params = ApplyPatchParams {
            file_path: file_path.to_str().unwrap().to_string(),
            patch_content: "... a patch ...".to_string(),
            dry_run: Some(false),
        };

        let result = apply_patch(params).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("File does not exist"));
    }

    #[tokio::test]
    async fn test_apply_patch_to_directory() {
        let dir = tempdir().unwrap();
        let params = ApplyPatchParams {
            file_path: dir.path().to_str().unwrap().to_string(),
            patch_content: "... a patch ...".to_string(),
            dry_run: Some(false),
        };

        let result = apply_patch(params).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Path is a directory"));
    }

    #[tokio::test]
    async fn test_apply_patch_to_read_only_file() {
        let original_content = "read only content";
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(original_content.as_bytes()).unwrap();
        let file_path = temp_file.path();

        let mut perms = fs::metadata(file_path).await.unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(file_path, perms).await.unwrap();

        let patch_content = create_patch_content(original_content, "new content");

        let params = ApplyPatchParams {
            file_path: file_path.to_str().unwrap().to_string(),
            patch_content,
            dry_run: Some(false),
        };

        let result = apply_patch(params).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Failed to write to file"));

        // Cleanup: make writable again to allow deletion by tempfile
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(file_path).await.unwrap().permissions();
            // Restore writable permissions for owner (rw-r--r--)
            perms.set_mode(0o644);
            fs::set_permissions(file_path, perms).await.unwrap();
        }
        #[cfg(not(unix))]
        {
            let mut perms = fs::metadata(file_path).await.unwrap().permissions();
            perms.set_readonly(false);
            fs::set_permissions(file_path, perms).await.unwrap();
        }
    }

    #[tokio::test]
    async fn test_apply_patch_with_malformed_patch_content() {
        let original_content = "some content";
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(original_content.as_bytes()).unwrap();
        let file_path = temp_file.path().to_str().unwrap().to_string();

        let params = ApplyPatchParams {
            file_path,
            patch_content: "this is not a valid patch".to_string(),
            dry_run: Some(false),
        };

        let result = apply_patch(params).await.unwrap();
        assert!(!result.success);
        assert!(
            result
                .message
                .contains("Patch content is invalid or results in no changes.")
        );
    }

    #[tokio::test]
    async fn test_patch_to_make_file_empty() {
        let original_content = "delete me";
        let modified_content = "";
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(original_content.as_bytes()).unwrap();
        let file_path = temp_file.path().to_str().unwrap().to_string();

        let patch_content = create_patch_content(original_content, modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
            dry_run: Some(false),
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);

        let final_content = fs::read_to_string(file_path).await.unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_patch_on_empty_file() {
        let original_content = "";
        let modified_content = "add me";
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(original_content.as_bytes()).unwrap();
        let file_path = temp_file.path().to_str().unwrap().to_string();

        let patch_content = create_patch_content(original_content, modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
            dry_run: Some(false),
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);

        let final_content = fs::read_to_string(file_path).await.unwrap();
        assert_eq!(final_content, modified_content);
    }
}
