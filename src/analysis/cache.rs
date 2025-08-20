use crate::analysis::RepoMap;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// repomapキャッシュのメタデータ
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepomapMetadata {
    pub version: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub project_root: PathBuf,
    pub total_files: usize,
    pub total_symbols: usize,
}

impl RepomapMetadata {
    pub fn new(project_root: PathBuf, total_files: usize, total_symbols: usize) -> Self {
        let now = Utc::now();
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            created_at: now,
            updated_at: now,
            project_root,
            total_files,
            total_symbols,
        }
    }

    pub fn update(&mut self, total_files: usize, total_symbols: usize) {
        self.updated_at = Utc::now();
        self.total_files = total_files;
        self.total_symbols = total_symbols;
    }
}

/// repomapキャッシュ全体のデータ構造
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepomapCache {
    pub metadata: RepomapMetadata,
    pub repomap: RepoMap,
    pub file_hashes: HashMap<PathBuf, String>, // ファイルパス -> SHA256ハッシュ
}

impl RepomapCache {
    pub fn new(
        project_root: PathBuf,
        repomap: RepoMap,
        file_hashes: HashMap<PathBuf, String>,
    ) -> Self {
        let metadata = RepomapMetadata::new(project_root, file_hashes.len(), repomap.symbols.len());
        Self {
            metadata,
            repomap,
            file_hashes,
        }
    }

    pub fn update(&mut self, repomap: RepoMap, file_hashes: HashMap<PathBuf, String>) {
        self.metadata
            .update(file_hashes.len(), repomap.symbols.len());
        self.repomap = repomap;
        self.file_hashes = file_hashes;
    }
}

/// repomapの永続化を管理するストア
pub struct RepomapStore {
    cache_dir: PathBuf,
    project_root: PathBuf,
}

impl RepomapStore {
    /// 新しいRepomapStoreを作成
    pub fn new(project_root: PathBuf) -> Result<Self> {
        let cache_dir = project_root.join(".doge").join("repomap");
        std::fs::create_dir_all(&cache_dir).with_context(|| {
            format!("Failed to create cache directory: {}", cache_dir.display())
        })?;

        Ok(Self {
            cache_dir,
            project_root,
        })
    }

    /// メタデータファイルのパス
    fn metadata_path(&self) -> PathBuf {
        self.cache_dir.join("metadata.json")
    }

    /// repomapデータファイルのパス
    fn repomap_path(&self) -> PathBuf {
        self.cache_dir.join("repomap.json")
    }

    /// ファイルハッシュキャッシュファイルのパス
    fn file_hashes_path(&self) -> PathBuf {
        self.cache_dir.join("file_hashes.json")
    }

    /// キャッシュが存在するかチェック
    pub fn cache_exists(&self) -> bool {
        self.metadata_path().exists()
            && self.repomap_path().exists()
            && self.file_hashes_path().exists()
    }

    /// キャッシュからrepomapを読み込み
    pub fn load(&self) -> Result<Option<RepomapCache>> {
        if !self.cache_exists() {
            debug!("Repomap cache does not exist");
            return Ok(None);
        }

        info!("Loading repomap cache from {}", self.cache_dir.display());

        // メタデータを読み込み
        let metadata_content = std::fs::read_to_string(self.metadata_path())
            .context("Failed to read metadata file")?;
        let metadata: RepomapMetadata =
            serde_json::from_str(&metadata_content).context("Failed to parse metadata")?;

        // repomapデータを読み込み
        let repomap_content =
            std::fs::read_to_string(self.repomap_path()).context("Failed to read repomap file")?;
        let repomap: RepoMap =
            serde_json::from_str(&repomap_content).context("Failed to parse repomap")?;

        // ファイルハッシュを読み込み
        let file_hashes_content = std::fs::read_to_string(self.file_hashes_path())
            .context("Failed to read file hashes file")?;
        let file_hashes: HashMap<PathBuf, String> =
            serde_json::from_str(&file_hashes_content).context("Failed to parse file hashes")?;

        let cache = RepomapCache {
            metadata,
            repomap,
            file_hashes,
        };

        info!(
            "Loaded repomap cache: {} symbols from {} files",
            cache.metadata.total_symbols, cache.metadata.total_files
        );

        Ok(Some(cache))
    }

    /// repomapをキャッシュに保存
    pub fn save(&self, cache: &RepomapCache) -> Result<()> {
        info!(
            "Saving repomap cache to {}: {} symbols from {} files",
            self.cache_dir.display(),
            cache.metadata.total_symbols,
            cache.metadata.total_files
        );

        // メタデータを保存
        let metadata_json = serde_json::to_string_pretty(&cache.metadata)
            .context("Failed to serialize metadata")?;
        std::fs::write(self.metadata_path(), metadata_json)
            .context("Failed to write metadata file")?;

        // repomapデータを保存
        let repomap_json =
            serde_json::to_string_pretty(&cache.repomap).context("Failed to serialize repomap")?;
        std::fs::write(self.repomap_path(), repomap_json)
            .context("Failed to write repomap file")?;

        // ファイルハッシュを保存
        let file_hashes_json = serde_json::to_string_pretty(&cache.file_hashes)
            .context("Failed to serialize file hashes")?;
        std::fs::write(self.file_hashes_path(), file_hashes_json)
            .context("Failed to write file hashes file")?;

        info!("Repomap cache saved successfully");
        Ok(())
    }

    /// キャッシュを削除
    pub fn clear(&self) -> Result<()> {
        info!("Clearing repomap cache from {}", self.cache_dir.display());

        for path in [
            self.metadata_path(),
            self.repomap_path(),
            self.file_hashes_path(),
        ] {
            if path.exists() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("Failed to remove cache file: {}", path.display()))?;
            }
        }

        info!("Repomap cache cleared successfully");
        Ok(())
    }

    /// キャッシュの有効性をチェック
    pub fn is_cache_valid(&self, current_file_hashes: &HashMap<PathBuf, String>) -> Result<bool> {
        let cache = match self.load()? {
            Some(cache) => cache,
            None => {
                debug!("No cache found, cache is invalid");
                return Ok(false);
            }
        };

        // プロジェクトルートが変わっていないかチェック
        if cache.metadata.project_root != self.project_root {
            warn!(
                "Project root changed: {} -> {}",
                cache.metadata.project_root.display(),
                self.project_root.display()
            );
            return Ok(false);
        }

        // バージョンが変わっていないかチェック
        let current_version = env!("CARGO_PKG_VERSION");
        if cache.metadata.version != current_version {
            warn!(
                "Version changed: {} -> {}",
                cache.metadata.version, current_version
            );
            return Ok(false);
        }

        // ファイルハッシュが変わっていないかチェック
        if cache.file_hashes != *current_file_hashes {
            debug!("File hashes changed, cache is invalid");
            return Ok(false);
        }

        debug!("Cache is valid");
        Ok(true)
    }

    /// 変更されたファイルを検出
    pub fn get_changed_files(
        &self,
        current_file_hashes: &HashMap<PathBuf, String>,
    ) -> Result<Vec<PathBuf>> {
        let cache = match self.load()? {
            Some(cache) => cache,
            None => {
                // キャッシュがない場合は全ファイルが変更されたとみなす
                return Ok(current_file_hashes.keys().cloned().collect());
            }
        };

        let mut changed_files = Vec::new();

        // 新しいファイルまたは変更されたファイルを検出
        for (path, hash) in current_file_hashes {
            match cache.file_hashes.get(path) {
                Some(cached_hash) if cached_hash == hash => {
                    // ハッシュが同じ場合は変更なし
                }
                _ => {
                    // 新しいファイルまたはハッシュが変更されたファイル
                    changed_files.push(path.clone());
                }
            }
        }

        // 削除されたファイルも変更として扱う（repomapから削除する必要がある）
        for path in cache.file_hashes.keys() {
            if !current_file_hashes.contains_key(path) {
                changed_files.push(path.clone());
            }
        }

        debug!("Found {} changed files", changed_files.len());
        Ok(changed_files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_repomap_metadata_creation() {
        let project_root = PathBuf::from("/test/project");
        let metadata = RepomapMetadata::new(project_root.clone(), 10, 100);

        assert_eq!(metadata.project_root, project_root);
        assert_eq!(metadata.total_files, 10);
        assert_eq!(metadata.total_symbols, 100);
        assert_eq!(metadata.version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn test_repomap_store_creation() {
        let temp_dir = TempDir::new().unwrap();
        let project_root = temp_dir.path().to_path_buf();

        let store = RepomapStore::new(project_root.clone()).unwrap();
        assert!(store.cache_dir.exists());
        assert_eq!(store.project_root, project_root);
    }

    #[test]
    fn test_cache_exists() {
        let temp_dir = TempDir::new().unwrap();
        let project_root = temp_dir.path().to_path_buf();
        let store = RepomapStore::new(project_root).unwrap();

        // 初期状態ではキャッシュは存在しない
        assert!(!store.cache_exists());

        // ファイルを作成
        std::fs::write(store.metadata_path(), "{}").unwrap();
        std::fs::write(store.repomap_path(), "{}").unwrap();
        std::fs::write(store.file_hashes_path(), "{}").unwrap();

        // 全ファイルが存在する場合はtrue
        assert!(store.cache_exists());
    }
}
