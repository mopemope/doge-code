use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Calculate BLAKE3 hash of a file using streaming to reduce memory usage
pub fn calculate_file_hash(file_path: &Path) -> Result<String> {
    let file = File::open(file_path)
        .with_context(|| format!("Failed to open file: {}", file_path.display()))?;
    let mut reader = BufReader::new(file);
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0; 8192]; // 8KB buffer

    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }

    let hash = hasher.finalize();
    Ok(hash.to_hex().to_string())
}

/// Calculate hashes of multiple files in parallel
pub async fn calculate_file_hashes(file_paths: &[PathBuf]) -> HashMap<PathBuf, String> {
    use tokio::task;

    // Handle empty file list case to avoid division by zero
    if file_paths.is_empty() {
        return HashMap::new();
    }

    let mut tasks = Vec::new();

    // Split files into chunks for parallel processing
    // Set the number of chunks to the minimum of the number of CPUs and the number of files
    let num_chunks = std::cmp::min(num_cpus::get(), file_paths.len());
    // Calculate the chunk size (num_chunks is guaranteed to be > 0 since file_paths is not empty)
    let chunk_size = std::cmp::max(1, file_paths.len().div_ceil(num_chunks));
    let chunks: Vec<Vec<PathBuf>> = file_paths
        .chunks(chunk_size)
        .map(|chunk: &[PathBuf]| chunk.to_vec())
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

    // Detect new or modified files
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

    // Detect deleted files
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
        // BLAKE3 hash of "Hello, World!"
        assert_eq!(
            hash,
            "288a86a79f20a3d6dccdca7713beaed178798296bdfa7913fa2a62d9727bf8f8"
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

    #[tokio::test]
    async fn test_calculate_file_hashes_empty() {
        let files: Vec<PathBuf> = vec![];
        let hashes = calculate_file_hashes(&files).await;

        assert_eq!(hashes.len(), 0);
    }

    #[test]
    fn test_calculate_hash_diff() {
        let mut old_hashes = HashMap::new();
        let mut new_hashes = HashMap::new();

        let file1 = PathBuf::from("file1.txt");
        let file2 = PathBuf::from("file2.txt");
        let file3 = PathBuf::from("file3.txt");
        let file4 = PathBuf::from("file4.txt");

        // Old hashes
        old_hashes.insert(file1.clone(), "hash1".to_string());
        old_hashes.insert(file2.clone(), "hash2".to_string());
        old_hashes.insert(file3.clone(), "hash3".to_string());

        // New hashes
        new_hashes.insert(file1.clone(), "hash1".to_string()); // No change
        new_hashes.insert(file2.clone(), "hash2_modified".to_string()); // Modified
        new_hashes.insert(file4.clone(), "hash4".to_string()); // Added
        // file3 is deleted

        let diff = calculate_hash_diff(&old_hashes, &new_hashes);

        assert_eq!(diff.added, vec![file4]);
        assert_eq!(diff.modified, vec![file2]);
        assert_eq!(diff.removed, vec![file3]);
        assert!(diff.has_changes());
        assert_eq!(diff.total_changes(), 3);
    }
    #[test]
    fn test_calculate_hash_empty_file() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("empty.txt");
        File::create(&file_path).unwrap();

        let hash = calculate_file_hash(&file_path).unwrap();
        // BLAKE3 hash of empty string: AF1349B9F5F9A1A6A0404DEA36DCC9499BCB25C9ADC112B7CC9A93CAE41F3262
        assert_eq!(
            hash,
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }
}
