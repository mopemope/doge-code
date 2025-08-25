use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Calculate SHA256 hash of a file
pub fn calculate_file_hash(file_path: &Path) -> Result<String> {
    let content = fs::read(file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

    let mut hasher = Sha256::new();
    hasher.update(&content);
    let hash = hasher.finalize();

    Ok(format!("{:x}", hash))
}

/// Calculate hashes of multiple files in parallel
pub async fn calculate_file_hashes(file_paths: &[PathBuf]) -> HashMap<PathBuf, String> {
    use tokio::task;

    let mut tasks = Vec::new();

    // Split files into chunks for parallel processing
    let chunk_size = std::cmp::max(1, file_paths.len() / num_cpus::get());
    let chunks: Vec<Vec<PathBuf>> = file_paths
        .chunks(chunk_size)
        .map(|chunk| chunk.to_vec())
        .collect();

    debug!(
        "Calculating hashes for {} files in {} chunks",
        file_paths.len(),
        chunks.len()
    );

    for chunk in chunks {
        let task = task::spawn_blocking(move || {
            let mut hashes = HashMap::new();
            for file_path in chunk {
                match calculate_file_hash(&file_path) {
                    Ok(hash) => {
                        hashes.insert(file_path, hash);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to calculate hash for {}: {}",
                            file_path.display(),
                            e
                        );
                    }
                }
            }
            hashes
        });
        tasks.push(task);
    }

    let mut all_hashes = HashMap::new();
    for task in tasks {
        match task.await {
            Ok(hashes) => {
                all_hashes.extend(hashes);
            }
            Err(e) => {
                warn!("Hash calculation task failed: {}", e);
            }
        }
    }

    debug!("Calculated hashes for {} files", all_hashes.len());
    all_hashes
}

/// Calculate file hash differences
pub fn calculate_hash_diff(
    old_hashes: &HashMap<PathBuf, String>,
    new_hashes: &HashMap<PathBuf, String>,
) -> HashDiff {
    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut removed = Vec::new();

    // 新しいファイルまたは変更されたファイルを検出
    for (path, new_hash) in new_hashes {
        match old_hashes.get(path) {
            Some(old_hash) if old_hash == new_hash => {
                // No changes if hashes are the same
            }
            Some(_) => {
                // Hash has changed
                modified.push(path.clone());
            }
            None => {
                // New file
                added.push(path.clone());
            }
        }
    }

    // 削除されたファイルを検出
    for path in old_hashes.keys() {
        if !new_hashes.contains_key(path) {
            removed.push(path.clone());
        }
    }

    HashDiff {
        added,
        modified,
        removed,
    }
}

/// File hash difference information
#[derive(Debug, Clone)]
pub struct HashDiff {
    pub added: Vec<PathBuf>,
    pub modified: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
}

impl HashDiff {
    /// Whether there are changes
    pub fn has_changes(&self) -> bool {
        !self.added.is_empty() || !self.modified.is_empty() || !self.removed.is_empty()
    }

    /// Total number of changed files
    pub fn total_changes(&self) -> usize {
        self.added.len() + self.modified.len() + self.removed.len()
    }

    /// List of changed files (added/modified only, excluding deleted)
    pub fn changed_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        files.extend(self.added.iter().cloned());
        files.extend(self.modified.iter().cloned());
        files
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_calculate_file_hash() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Hello, World!").unwrap();

        let hash = calculate_file_hash(&file_path).unwrap();
        // "Hello, World!"のSHA256ハッシュ
        assert_eq!(
            hash,
            "dffd6021bb2bd5b0af676290809ec3a53191dd81c7f70a4b28688a362182986f"
        );
    }

    #[tokio::test]
    async fn test_calculate_file_hashes() {
        let temp_dir = TempDir::new().unwrap();
        let file1 = temp_dir.path().join("file1.txt");
        let file2 = temp_dir.path().join("file2.txt");

        fs::write(&file1, "Content 1").unwrap();
        fs::write(&file2, "Content 2").unwrap();

        let files = vec![file1.clone(), file2.clone()];
        let hashes = calculate_file_hashes(&files).await;

        assert_eq!(hashes.len(), 2);
        assert!(hashes.contains_key(&file1));
        assert!(hashes.contains_key(&file2));
    }

    #[test]
    fn test_calculate_hash_diff() {
        let mut old_hashes = HashMap::new();
        let mut new_hashes = HashMap::new();

        let file1 = PathBuf::from("file1.txt");
        let file2 = PathBuf::from("file2.txt");
        let file3 = PathBuf::from("file3.txt");
        let file4 = PathBuf::from("file4.txt");

        // 古いハッシュ
        old_hashes.insert(file1.clone(), "hash1".to_string());
        old_hashes.insert(file2.clone(), "hash2".to_string());
        old_hashes.insert(file3.clone(), "hash3".to_string());

        // 新しいハッシュ
        new_hashes.insert(file1.clone(), "hash1".to_string()); // 変更なし
        new_hashes.insert(file2.clone(), "hash2_modified".to_string()); // 変更
        new_hashes.insert(file4.clone(), "hash4".to_string()); // 追加
        // file3は削除

        let diff = calculate_hash_diff(&old_hashes, &new_hashes);

        assert_eq!(diff.added, vec![file4]);
        assert_eq!(diff.modified, vec![file2]);
        assert_eq!(diff.removed, vec![file3]);
        assert!(diff.has_changes());
        assert_eq!(diff.total_changes(), 3);
    }
}
