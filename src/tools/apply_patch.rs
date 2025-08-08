use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::fs;

#[derive(Debug, Serialize, Deserialize)]
pub struct ApplyPatchParams {
    pub file_path: String,
    pub patch_content: String,
    pub file_hash_sha256: Option<String>,
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
    let file_path = &params.file_path;
    let patch_content = &params.patch_content;
    let dry_run = params.dry_run.unwrap_or(false);

    // 1. Read file content
    let original_content = fs::read_to_string(file_path)
        .await
        .with_context(|| format!("Failed to read file: {file_path}"))?;

    // 2. Verify file hash (if provided)
    if let Some(expected_hash) = &params.file_hash_sha256 {
        let mut hasher = Sha256::new();
        hasher.update(original_content.as_bytes());
        let actual_hash = format!("{:x}", hasher.finalize());

        if &actual_hash != expected_hash {
            return Ok(ApplyPatchResult {
                success: false,
                message: "File hash mismatch. The file content has changed since it was last read."
                    .to_string(),
                original_content: Some(original_content),
                modified_content: None,
            });
        }
    }

    // 3. Apply the patch
    let patch = diffy::Patch::from_str(patch_content)
        .map_err(|e| anyhow::anyhow!("Failed to parse patch: {e}"))?;

    let patched_content = match diffy::apply(&original_content, &patch) {
        Ok(content) => content,
        Err(e) => {
            return Ok(ApplyPatchResult {
                success: false,
                message: format!("Failed to apply patch: {e}"),
                original_content: Some(original_content),
                modified_content: None,
            });
        }
    };

    if dry_run {
        return Ok(ApplyPatchResult {
            success: true,
            message: "Dry run successful. Patch can be applied cleanly.".to_string(),
            original_content: Some(original_content),
            modified_content: Some(patched_content),
        });
    }

    // 4. Write the modified content back to the file
    fs::write(file_path, &patched_content)
        .await
        .with_context(|| format!("Failed to write to file: {file_path}"))?;

    Ok(ApplyPatchResult {
        success: true,
        message: "File patched successfully.".to_string(),
        original_content: Some(original_content),
        modified_content: Some(patched_content),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn create_patch_content(original: &str, modified: &str) -> String {
        let patch = diffy::create_patch(original, modified);
        patch.to_string()
    }

    fn calculate_sha256(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    #[tokio::test]
    async fn test_apply_patch_success() {
        let original_content = "Hello, world!\nThis is the original file.\n";
        let modified_content = "Hello, Rust!\nThis is the modified file.\n";

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(original_content.as_bytes()).unwrap();
        let file_path = temp_file.path().to_str().unwrap().to_string();

        let patch_content = create_patch_content(original_content, modified_content);
        let file_hash = calculate_sha256(original_content);

        let params = ApplyPatchParams {
            file_path: file_path.clone(),
            patch_content,
            file_hash_sha256: Some(file_hash),
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
            file_hash_sha256: None,
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
    async fn test_apply_patch_hash_mismatch() {
        let original_content = "Content\n";
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(original_content.as_bytes()).unwrap();
        let file_path = temp_file.path().to_str().unwrap().to_string();

        let params = ApplyPatchParams {
            file_path,
            patch_content: "".to_string(),
            file_hash_sha256: Some("wrong_hash".to_string()),
            dry_run: Some(false),
        };

        let result = apply_patch(params).await.unwrap();
        assert!(!result.success);
        assert!(result.message.contains("File hash mismatch"));
    }
}
