use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct CreatePatchParams {
    pub original_content: String,
    pub modified_content: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreatePatchResult {
    pub patch_content: String,
}

pub async fn create_patch(params: CreatePatchParams) -> Result<CreatePatchResult> {
    let patch = diffy::create_patch(&params.original_content, &params.modified_content);
    Ok(CreatePatchResult {
        patch_content: patch.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_patch_simple() {
        let params = CreatePatchParams {
            original_content: "hello world\n".to_string(),
            modified_content: "hello rust\n".to_string(),
        };
        let result = create_patch(params).await.unwrap();
        assert!(result.patch_content.contains("-hello world"));
        assert!(result.patch_content.contains("+hello rust"));
    }

    #[tokio::test]
    async fn test_create_patch_multiline() {
        let params = CreatePatchParams {
            original_content: "line one\nline two\nline three\n".to_string(),
            modified_content: "line one\nline 2\nline three\n".to_string(),
        };
        let result = create_patch(params).await.unwrap();
        assert!(result.patch_content.contains("-line two"));
        assert!(result.patch_content.contains("+line 2"));
    }
}
