use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
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

    // Ensure the path is absolute
    let path = Path::new(file_path);
    if !path.is_absolute() {
        anyhow::bail!("File path must be absolute: {}", file_path);
    }

    let content = fs::read(path)
        .await
        .with_context(|| format!("Failed to read file: {}", path.display()))?;

    let mut hasher = Sha256::new();
    hasher.update(&content);
    let sha256_hash = format!("{:x}", hasher.finalize());

    Ok(GetFileSha256Result {
        file_path: file_path.clone(),
        sha256_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_get_file_sha256_success() {
        let content = b"hello world";
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content).unwrap();
        let path = file.path().to_str().unwrap().to_string();

        let params = GetFileSha256Params { file_path: path };
        let result = get_file_sha256(params).await.unwrap();

        let mut hasher = Sha256::new();
        hasher.update(content);
        let expected_hash = format!("{:x}", hasher.finalize());

        assert_eq!(result.sha256_hash, expected_hash);
    }

    #[tokio::test]
    async fn test_get_file_sha256_file_not_found() {
        let params = GetFileSha256Params {
            file_path: "/path/to/non/existent/file".to_string(),
        };
        let result = get_file_sha256(params).await;
        assert!(result.is_err());
    }
}
