use crate::analysis::RepoMap;
use crate::analysis::database::connection::{connect_database, get_default_db_path};
use crate::analysis::database::dao::RepomapDAO;
use crate::analysis::database::migration::run_migrations;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs; // 追加
use std::path::{Path, PathBuf}; // 追加
use tracing::{debug, info};

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
    project_root: PathBuf,
    db_conn: DatabaseConnection,
}

impl RepomapStore {
    /// 新しいRepomapStoreを作成
    pub async fn new(project_root: PathBuf) -> Result<Self> {
        let db_path = get_default_db_path(&project_root);

        // 親ディレクトリ `.doge` が存在することを確認
        if let Some(parent_dir) = Path::new(&db_path).parent() {
            fs::create_dir_all(parent_dir)
                .with_context(|| format!("Failed to create directory: {}", parent_dir.display()))?;
        }

        let db_conn = connect_database(&db_path)
            .await
            .context("Failed to connect to database")?;

        // Run migrations
        run_migrations(&db_conn)
            .await
            .context("Failed to run database migrations")?;

        Ok(Self {
            project_root,
            db_conn,
        })
    }

    /// キャッシュからrepomapを読み込み
    pub async fn load(&self) -> Result<Option<RepomapCache>> {
        info!("Loading repomap cache from database");

        match RepomapDAO::load_repomap(&self.db_conn, &self.project_root)
            .await
            .context("Failed to load repomap from database")?
        {
            Some((repomap, file_hashes)) => {
                let metadata = RepomapMetadata::new(
                    self.project_root.clone(),
                    file_hashes.len(),
                    repomap.symbols.len(),
                );
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
            None => {
                debug!("No repomap cache found in database");
                Ok(None)
            }
        }
    }

    /// repomapをキャッシュに保存
    pub async fn save(&self, cache: &RepomapCache) -> Result<()> {
        info!(
            "Saving repomap cache to database: {} symbols from {} files",
            cache.metadata.total_symbols, cache.metadata.total_files
        );

        RepomapDAO::save_repomap(
            &self.db_conn,
            &cache.repomap,
            &cache.file_hashes,
            &self.project_root,
        )
        .await
        .context("Failed to save repomap to database")?;

        info!("Repomap cache saved successfully");
        Ok(())
    }

    /// キャッシュを削除
    pub async fn clear(&self) -> Result<()> {
        info!("Clearing repomap cache from database");

        RepomapDAO::clear_repomap(&self.db_conn, &self.project_root)
            .await
            .context("Failed to clear repomap from database")?;

        info!("Repomap cache cleared successfully");
        Ok(())
    }

    /// キャッシュの有効性をチェック
    pub async fn is_cache_valid(
        &self,
        current_file_hashes: &HashMap<PathBuf, String>,
    ) -> Result<bool> {
        // バージョンが変わっていないかチェック
        // TODO: データベースにバージョン情報を保存し、比較するロジックが必要

        // ファイルハッシュが変わっていないかチェック
        let is_valid =
            RepomapDAO::is_repomap_valid(&self.db_conn, &self.project_root, current_file_hashes)
                .await
                .context("Failed to check repomap validity")?;

        if is_valid {
            debug!("Cache is valid");
        } else {
            debug!("Cache is invalid");
        }
        Ok(is_valid)
    }

    /// 変更されたファイルを検出
    pub async fn get_changed_files(
        &self,
        current_file_hashes: &HashMap<PathBuf, String>,
    ) -> Result<Vec<PathBuf>> {
        let changed_files =
            RepomapDAO::get_changed_files(&self.db_conn, &self.project_root, current_file_hashes)
                .await
                .context("Failed to get changed files")?;

        debug!("Found {} changed files", changed_files.len());
        Ok(changed_files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::symbol::{SymbolInfo, SymbolKind};
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_repomap_store_save_and_load() {
        let tmp_dir = TempDir::new().expect("Failed to create temp dir");
        let project_root = tmp_dir.path().to_path_buf();
        let store = RepomapStore::new(project_root.clone())
            .await
            .expect("Failed to create RepomapStore");

        let symbols = vec![SymbolInfo {
            name: "test_function".to_string(),
            kind: SymbolKind::Function,
            file: project_root.join("src/main.rs"),
            start_line: 1,
            start_col: 0,
            end_line: 3,
            end_col: 1,
            parent: None,
            file_total_lines: 10,
            function_lines: Some(3),
        }];
        let repomap = RepoMap { symbols };
        let mut hashes = HashMap::new();
        hashes.insert(project_root.join("src/main.rs"), "hash1".to_string());
        let cache = RepomapCache::new(project_root.clone(), repomap, hashes);

        // Save
        assert!(store.save(&cache).await.is_ok());

        // Load
        let loaded_cache = store.load().await.expect("Failed to load cache");
        assert!(loaded_cache.is_some());
        let loaded_cache = loaded_cache.unwrap();
        assert_eq!(loaded_cache.repomap.symbols.len(), 1);
        assert_eq!(loaded_cache.repomap.symbols[0].name, "test_function");
        assert_eq!(loaded_cache.file_hashes.len(), 1);
        assert_eq!(
            loaded_cache
                .file_hashes
                .get(&project_root.join("src/main.rs")),
            Some(&"hash1".to_string())
        );
    }

    #[tokio::test]
    async fn test_repomap_store_clear() {
        let tmp_dir = TempDir::new().expect("Failed to create temp dir");
        let project_root = tmp_dir.path().to_path_buf();
        let store = RepomapStore::new(project_root.clone())
            .await
            .expect("Failed to create RepomapStore");

        let repomap = RepoMap { symbols: vec![] };
        let mut hashes = HashMap::new();
        hashes.insert(project_root.join("src/main.rs"), "hash1".to_string());
        let cache = RepomapCache::new(project_root.clone(), repomap, hashes);

        // Save
        assert!(store.save(&cache).await.is_ok());

        // Clear
        assert!(store.clear().await.is_ok());

        // Load - should be None
        let loaded_cache = store.load().await.expect("Failed to load cache");
        assert!(loaded_cache.is_none());
    }

    #[tokio::test]
    async fn test_repomap_store_is_cache_valid() {
        let tmp_dir = TempDir::new().expect("Failed to create temp dir");
        let project_root = tmp_dir.path().to_path_buf();
        let store = RepomapStore::new(project_root.clone())
            .await
            .expect("Failed to create RepomapStore");

        let repomap = RepoMap { symbols: vec![] };
        let mut hashes = HashMap::new();
        hashes.insert(project_root.join("src/main.rs"), "hash1".to_string());
        let cache = RepomapCache::new(project_root.clone(), repomap, hashes.clone());

        // Save
        assert!(store.save(&cache).await.is_ok());

        // Valid
        assert!(
            store
                .is_cache_valid(&hashes)
                .await
                .expect("Failed to check cache validity")
        );

        // Invalid
        let mut modified_hashes = hashes.clone();
        modified_hashes.insert(project_root.join("src/other.rs"), "hash2".to_string());
        assert!(
            !store
                .is_cache_valid(&modified_hashes)
                .await
                .expect("Failed to check cache validity")
        );
    }

    #[tokio::test]
    async fn test_repomap_store_get_changed_files() {
        let tmp_dir = TempDir::new().expect("Failed to create temp dir");
        let project_root = tmp_dir.path().to_path_buf();
        let store = RepomapStore::new(project_root.clone())
            .await
            .expect("Failed to create RepomapStore");

        let repomap = RepoMap { symbols: vec![] };
        let mut hashes = HashMap::new();
        hashes.insert(project_root.join("src/main.rs"), "hash1".to_string());
        hashes.insert(project_root.join("src/lib.rs"), "hash2".to_string());
        let cache = RepomapCache::new(project_root.clone(), repomap, hashes.clone());

        // Save
        assert!(store.save(&cache).await.is_ok());

        // No changes
        let changed = store
            .get_changed_files(&hashes)
            .await
            .expect("Failed to get changed files");
        assert_eq!(changed.len(), 0);

        // Added file
        let mut modified_hashes = hashes.clone();
        modified_hashes.insert(project_root.join("src/new.rs"), "hash3".to_string());
        let changed = store
            .get_changed_files(&modified_hashes)
            .await
            .expect("Failed to get changed files");
        assert_eq!(changed.len(), 1);
        assert!(changed.contains(&project_root.join("src/new.rs")));

        // Modified file
        let mut modified_hashes = hashes.clone();
        modified_hashes.insert(
            project_root.join("src/main.rs"),
            "hash1_modified".to_string(),
        );
        let changed = store
            .get_changed_files(&modified_hashes)
            .await
            .expect("Failed to get changed files");
        assert_eq!(changed.len(), 1);
        assert!(changed.contains(&project_root.join("src/main.rs")));

        // Deleted file
        let mut modified_hashes = HashMap::new();
        modified_hashes.insert(project_root.join("src/main.rs"), "hash1".to_string());
        let changed = store
            .get_changed_files(&modified_hashes)
            .await
            .expect("Failed to get changed files");
        assert_eq!(changed.len(), 1);
        assert!(changed.contains(&project_root.join("src/lib.rs")));
    }
}
