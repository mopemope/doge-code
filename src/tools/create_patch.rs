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
