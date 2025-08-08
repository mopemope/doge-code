use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::fs;

#[derive(Debug, Serialize, Deserialize)]
pub struct GetFileSha256Params {
    pub file_path: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GetFileSha256Result {
    pub file_path: String,
    pub sha256_hash: String,
}

pub async fn get_file_sha256(params: GetFileSha256Params) -> Result<GetFileSha256Result> {
    let file_path = &params.file_path;

    let content = fs::read(file_path)
        .await
        .with_context(|| format!("Failed to read file: {file_path}"))?;

    let mut hasher = Sha256::new();
    hasher.update(&content);
    let sha256_hash = format!("{:x}", hasher.finalize());

    Ok(GetFileSha256Result {
        file_path: file_path.clone(),
        sha256_hash,
    })
}
