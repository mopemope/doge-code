use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::fs;

#[derive(Debug, Serialize, Deserialize)]
pub struct ReplaceTextBlockParams {
    pub file_path: String,
    pub target_block: String,
    pub new_block: String,
    pub file_hash_sha256: String,
    pub dry_run: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReplaceTextBlockResult {
    pub success: bool,
    pub message: String,
    pub diff: Option<String>,
}

pub async fn replace_text_block(params: ReplaceTextBlockParams) -> Result<ReplaceTextBlockResult> {
    let file_path = &params.file_path;
    let target_block = &params.target_block;
    let new_block = &params.new_block;
    let expected_hash = &params.file_hash_sha256;
    let dry_run = params.dry_run.unwrap_or(false);

    // 1. Read file content
    let original_content = fs::read_to_string(file_path)
        .await
        .with_context(|| format!("Failed to read file: {file_path}"))?;

    // 2. Verify file hash
    let mut hasher = Sha256::new();
    hasher.update(original_content.as_bytes());
    let actual_hash = format!("{:x}", hasher.finalize());

    if &actual_hash != expected_hash {
        return Ok(ReplaceTextBlockResult {
            success: false,
            message: "File hash mismatch. The file content has changed since it was last read."
                .to_string(),
            diff: None,
        });
    }

    // 3. Find the target block
    let occurrences = original_content.matches(target_block).count();
    if occurrences == 0 {
        return Ok(ReplaceTextBlockResult {
            success: false,
            message: "Target block not found in the file.".to_string(),
            diff: None,
        });
    }
    if occurrences > 1 {
        return Ok(ReplaceTextBlockResult {
            success: false,
            message: "Target block is not unique. Found multiple occurrences.".to_string(),
            diff: None,
        });
    }

    // 4. Perform the replacement
    let modified_content = original_content.replace(target_block, new_block);

    // 5. Generate diff for dry_run or successful operation
    let diff = diffy::create_patch(&original_content, &modified_content);
    let diff_text = diff.to_string();

    if dry_run {
        return Ok(ReplaceTextBlockResult {
            success: true,
            message: "Dry run successful. No changes were made.".to_string(),
            diff: Some(diff_text),
        });
    }

    // 6. Write the modified content back to the file
    fs::write(file_path, modified_content)
        .await
        .with_context(|| format!("Failed to write to file: {file_path}"))?;

    Ok(ReplaceTextBlockResult {
        success: true,
        message: "File updated successfully.".to_string(),
        diff: Some(diff_text),
    })
}
