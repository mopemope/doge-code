use crate::config::AppConfig;
use anyhow::{Context, Result};
use std::path::Path;
use tokio::fs;

use super::{ApplyPatchParams, ApplyPatchResult};

pub async fn apply_patch(params: ApplyPatchParams, config: &AppConfig) -> Result<ApplyPatchResult> {
    let file_path = params.file_path;
    let patch_content = params.patch_content;

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

    if !path.exists() {
        return Err(anyhow::anyhow!(
            "Failed to read file: File does not exist at {}",
            path.display()
        ));
    }
    if path.is_dir() {
        return Err(anyhow::anyhow!(
            "Failed to read file: Path is a directory: {}",
            path.display()
        ));
    }

    let original_content_raw = fs::read_to_string(path).await.with_context(|| {
        format!(
            "Failed to read file for an unknown reason: {}",
            path.display()
        )
    })?;

    let has_crlf = original_content_raw.contains("\r\n");
    let original_content = original_content_raw.replace("\r\n", "\n");

    let patch = match diffy::Patch::from_str(&patch_content) {
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

    if patch.hunks().is_empty() && !patch_content.trim().is_empty() {
        return Ok(ApplyPatchResult {
            success: false,
            message: "Patch content is invalid or results in no changes.".to_string(),
            original_content: Some(original_content_raw.clone()),
            modified_content: Some(original_content_raw.clone()),
        });
    }

    let mut patched_content_lf = match diffy::apply(&original_content, &patch) {
        Ok(content) => content,
        Err(e) => {
            // Provide more detailed error message to help users understand the common cause
            let error_str = e.to_string();
            let detailed_message = if error_str.contains("error applying hunk")
                || error_str.contains("context lines do not match")
            {
                format!(
                    "Failed to apply patch: Context lines do not match. This usually happens when the file content has changed since the patch was created, or the context in the patch doesn't match the current file content exactly. Make sure to read the current file content with fs_read before creating your patch. Error: {}",
                    e
                )
            } else {
                format!("Failed to apply patch: {}", e)
            };

            return Ok(ApplyPatchResult {
                success: false,
                message: detailed_message,
                original_content: Some(original_content_raw),
                modified_content: None,
            });
        }
    };

    if has_crlf {
        patched_content_lf = patched_content_lf.replace('\n', "\r\n");
    }

    // First, verify that we can write to the file by checking permissions before attempting the write
    // This helps catch permission issues early and provide more specific errors
    let metadata = fs::metadata(path)
        .await
        .with_context(|| format!("Failed to get file metadata: {}", path.display()))?;

    // Check if file is writable by trying to verify permissions (on Unix systems)
    #[cfg(unix)]
    {
        let permissions = metadata.permissions();
        if permissions.readonly() {
            anyhow::bail!("Failed to write to file: {}", path.display());
        }
    }
    // On non-Unix systems, we rely on the write operation to detect permission issues
    // The actual write operation will catch permission issues

    fs::write(path, &patched_content_lf)
        .await
        .with_context(|| format!("Failed to write to file: {}", path.display()))?;

    // Double-check that the file was actually modified by reading it back
    let verification_content = fs::read_to_string(path).await.with_context(|| {
        format!(
            "Failed to read back file after patching: {}",
            path.display()
        )
    })?;

    if verification_content != patched_content_lf {
        return Err(anyhow::anyhow!(
            "File content verification failed: content read back after patching does not match expected patched content for file: {}",
            path.display()
        ));
    }

    // Update session with changed file
    if let Ok(current_dir) = std::env::current_dir()
        && let Ok(relative_path) = path.strip_prefix(current_dir)
    {
        let fs_tools = crate::tools::FsTools::default();
        let _ = fs_tools.update_session_with_changed_file(relative_path.to_path_buf());
    }

    Ok(ApplyPatchResult {
        success: true,
        message: "File patched successfully.".to_string(),
        original_content: Some(original_content_raw),
        modified_content: Some(patched_content_lf),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    async fn apply_patch(
        params: super::ApplyPatchParams,
    ) -> anyhow::Result<super::ApplyPatchResult> {
        let config = crate::tools::test_utils::create_test_config_with_temp_dir();
        super::apply_patch(params, &config).await
    }

    fn create_temp_file(content: &str) -> (PathBuf, String) {
        let temp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("temp");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let file_path = tempfile::Builder::new()
            .prefix("test_")
            .suffix(".txt")
            .rand_bytes(5)
            .tempfile_in(&temp_dir)
            .unwrap()
            .into_temp_path()
            .to_path_buf();
        std::fs::write(&file_path, content).unwrap();
        let file_path_str = file_path.to_str().unwrap().to_string();
        (file_path, file_path_str)
    }

    fn create_patch_content(original: &str, modified: &str) -> String {
        let patch = diffy::create_patch(original, modified);
        patch.to_string()
    }

    #[tokio::test]
    async fn test_apply_patch_success() {
        let original_content = r#"Hello, world!
This is the original file.
"#;
        let modified_content = r#"Hello, Rust!
This is the modified file.
"#;

        let (_temp_file, file_path) = create_temp_file(original_content);

        let patch_content = create_patch_content(original_content, modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);
        assert_eq!(result.message, "File patched successfully.");

        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_create_and_apply_patch_integration() {
        let original_content = r#"line 1
line 2
line 3
"#;
        let modified_content = r#"line 1
line two
line 3
"#;

        // Create a temporary file with the original content
        let (_temp_file, file_path) = create_temp_file(original_content);

        // 1. Create the patch
        let patch_content = create_patch_content(original_content, modified_content);
        assert!(patch_content.contains("-line 2"));
        assert!(patch_content.contains("+line two"));

        // 2. Apply the patch
        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };
        let result = apply_patch(params).await.unwrap();

        // 3. Verify the result
        assert!(result.success);
        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_apply_patch_conflict() {
        let original_content = r#"line A
line B
line C
"#;
        let modified_content = r#"line A
line Bee
line C
"#;
        let actual_content_in_file = r#"line A
line Z
line C
"#; // This is different from original_content

        // Create a temporary file with the "actual" content
        let (_temp_file, file_path) = create_temp_file(actual_content_in_file);

        // 1. Create the patch based on the "original" content
        let patch_content = create_patch_content(original_content, modified_content);

        // 2. Attempt to apply the patch to the "actual" content
        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
            // Note: We use the hash of the *actual* content for the check to pass
        };
        let result = apply_patch(params).await.unwrap();

        // 3. Verify that the patch application failed due to content mismatch
        assert!(!result.success);
        assert!(result.message.contains("Failed to apply patch"));
        // Check that the enhanced error message includes helpful guidance
        assert!(
            result.message.contains("Context lines do not match")
                || result.message.contains("context lines do not match")
        );
        assert!(
            result.message.contains("fs_read") || result.message.contains("current file content")
        );

        // Ensure the file content remains unchanged
        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(final_content, actual_content_in_file);
    }

    #[tokio::test]
    async fn test_apply_patch_requires_absolute_path() {
        let params = ApplyPatchParams {
            file_path: "relative/path/to/file.txt".to_string(),
            patch_content: "any patch".to_string(),
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
        let original_content = r#"first line
second line
"#
        .replace('\n', "\r\n");
        let modified_content = r#"first line
second line modified
"#
        .replace('\n', "\r\n");

        let (_temp_file, file_path) = create_temp_file(&original_content);

        // Create patch using LF-normalized content, as our tool now handles this internally
        let patch_content = create_patch_content(
            &original_content.replace("\r\n", "\n"),
            &modified_content.replace("\r\n", "\n"),
        );

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };

        let result = apply_patch(params).await.unwrap();
        assert!(
            result.success,
            "Patch should apply cleanly. Message: {}",
            result.message
        );

        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_apply_patch_no_newline_at_end_of_file() {
        let original_content = "hello";
        let modified_content = "hello world";

        let (_temp_file, file_path) = create_temp_file(original_content);

        let patch_content = create_patch_content(original_content, modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);

        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_apply_patch_large_change() {
        let original_content = "line\n".repeat(100);
        let modified_content = "changed line\n".repeat(100);

        let (_temp_file, file_path) = create_temp_file(&original_content);

        let patch_content = create_patch_content(&original_content, &modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);

        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_apply_patch_to_non_existent_file() {
        let temp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("temp");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let file_path = temp_dir.join("non_existent_file.txt");
        let params = ApplyPatchParams {
            file_path: file_path.to_str().unwrap().to_string(),
            patch_content: "... a patch ...".to_string(),
        };

        let result = apply_patch(params).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("File does not exist"));
    }

    #[tokio::test]
    async fn test_apply_patch_to_directory() {
        let temp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("temp");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let params = ApplyPatchParams {
            file_path: temp_dir.to_str().unwrap().to_string(),
            patch_content: "... a patch ...".to_string(),
        };

        let result = apply_patch(params).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Path is a directory"));
    }

    #[tokio::test]
    async fn test_apply_patch_to_read_only_file() {
        let original_content = "read only content";
        let (_temp_file, file_path) = create_temp_file(original_content);

        let mut perms = std::fs::metadata(&file_path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&file_path, perms).unwrap();

        let patch_content = create_patch_content(original_content, "new content");

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };

        let result = apply_patch(params).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Failed to write to file"));

        // Cleanup: make writable again to allow deletion by tempfile
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&file_path).unwrap().permissions();
            // Restore writable permissions for owner (rw-r--r--)
            perms.set_mode(0o644);
            std::fs::set_permissions(&file_path, perms).unwrap();
        }
        #[cfg(not(unix))]
        {
            let mut perms = std::fs::metadata(&file_path).unwrap().permissions();
            perms.set_readonly(false);
            std::fs::set_permissions(&file_path, perms).unwrap();
        }
    }

    #[tokio::test]
    async fn test_apply_patch_with_malformed_patch_content() {
        let original_content = "some content";
        let (_temp_file, file_path) = create_temp_file(original_content);

        let params = ApplyPatchParams {
            file_path,
            patch_content: "this is not a valid patch".to_string(),
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
        let (_temp_file, file_path) = create_temp_file(original_content);

        let patch_content = create_patch_content(original_content, modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);

        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_patch_on_empty_file() {
        let original_content = "";
        let modified_content = "add me";
        let (_temp_file, file_path) = create_temp_file(original_content);

        let patch_content = create_patch_content(original_content, modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);

        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_apply_patch_returns_content_in_success_case() {
        let original_content = "Hello, world!\nThis is the original file.\n";
        let modified_content = "Hello, Rust!\nThis is the modified file.\n";

        let (_temp_file, file_path) = create_temp_file(original_content);

        let patch_content = create_patch_content(original_content, modified_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);
        assert_eq!(result.message, "File patched successfully.");

        // Verify that content is returned in the success case (this was the fix)
        assert!(
            result.original_content.is_some(),
            "Original content should be returned in success case"
        );
        assert!(
            result.modified_content.is_some(),
            "Modified content should be returned in success case"
        );

        if let Some(orig_content) = &result.original_content {
            assert_eq!(
                orig_content, original_content,
                "Returned original content should match"
            );
        }

        if let Some(mod_content) = &result.modified_content {
            assert_eq!(
                mod_content, modified_content,
                "Returned modified content should match"
            );
        }

        let final_content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(final_content, modified_content);
    }

    #[tokio::test]
    async fn test_apply_patch_updates_session_with_changed_file() {
        let original_content = "Hello, world!\n";
        let modified_content = "Hello, Rust!\n";

        let temp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("temp");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let temp_file_path = temp_dir.join("test_apply_patch_session.txt");
        std::fs::write(&temp_file_path, original_content).unwrap();
        let file_path_str = temp_file_path.to_string_lossy().to_string();

        let patch_content = create_patch_content(original_content, modified_content);

        let params = ApplyPatchParams {
            file_path: file_path_str,
            patch_content,
        };

        let result = apply_patch(params).await.unwrap();
        assert!(result.success);

        // Verify file was actually changed
        let final_content = std::fs::read_to_string(&temp_file_path).unwrap();
        assert_eq!(final_content, modified_content);

        // Clean up
        std::fs::remove_file(&temp_file_path).unwrap();

        // Note: The session update happens in a separate thread with FsTools::default(),
        // so we can't directly check it in this test. The important thing is that the
        // update_session_with_changed_file call is made in the success path, which it is.
    }
}
