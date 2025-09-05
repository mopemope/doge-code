use crate::analysis::database::entities::file_hash::ActiveModel as FileHashActiveModel;
use crate::analysis::database::entities::symbol_info::ActiveModel as SymbolInfoActiveModel;
use crate::analysis::database::entities::symbol_info::Model as SymbolInfoModel;
use crate::analysis::database::entities::{FileHashEntity, SymbolInfoEntity};
use crate::analysis::{RepoMap, SymbolInfo as AnalysisSymbolInfo};
use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set,
    TransactionTrait,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info};

/// Data Access Object for Repomap persistence.
pub struct RepomapDAO;

impl RepomapDAO {
    /// Saves a RepoMap and its associated file hashes to the database.
    ///
    /// # Arguments
    /// * `conn` - The database connection.
    /// * `repomap` - The RepoMap to save.
    /// * `hashes` - The file hashes to save.
    /// * `project_root` - The project root path.
    ///
    /// # Returns
    /// * `Result<()>` - Ok if successful, Err otherwise.
    pub async fn save_repomap(
        conn: &DatabaseConnection,
        repomap: &RepoMap,
        hashes: &HashMap<PathBuf, String>,
        project_root: &Path,
    ) -> Result<()> {
        let project_root_str = project_root
            .to_str()
            .context("Project root path is not valid UTF-8")?;

        info!(
            "Saving repomap to database for project: {}",
            project_root_str
        );

        // Start a transaction
        let txn = conn
            .begin()
            .await
            .context("Failed to start database transaction")?;

        // Clear existing data for this project root to ensure a clean save
        Self::clear_repomap(&txn, project_root).await?;

        // Insert symbols
        for symbol in &repomap.symbols {
            let symbol_model = symbol_to_active_model(symbol, project_root_str)?;
            symbol_model
                .insert(&txn)
                .await
                .context("Failed to insert symbol")?;
        }

        // Insert file hashes
        for (file_path, hash) in hashes {
            let file_path_str = file_path.to_str().context("File path is not valid UTF-8")?;
            let file_hash_model = file_hash_to_active_model(file_path_str, hash, project_root_str);
            file_hash_model
                .insert(&txn)
                .await
                .context("Failed to insert file hash")?;
        }

        // Commit the transaction
        txn.commit()
            .await
            .context("Failed to commit database transaction")?;

        info!("Repomap saved successfully");
        Ok(())
    }

    /// Loads a RepoMap and its associated file hashes from the database.
    ///
    /// # Arguments
    /// * `conn` - The database connection.
    /// * `project_root` - The project root path.
    ///
    /// # Returns
    /// * `Result<Option<(RepoMap, HashMap<PathBuf, String>)>>` - The loaded RepoMap and hashes, or None if neither symbols nor hashes are found.
    pub async fn load_repomap(
        conn: &DatabaseConnection,
        project_root: &Path,
    ) -> Result<Option<(RepoMap, HashMap<PathBuf, String>)>> {
        let project_root_str = project_root
            .to_str()
            .context("Project root path is not valid UTF-8")?;

        info!(
            "Loading repomap from database for project: {}",
            project_root_str
        );

        // Load symbols
        let symbol_models = SymbolInfoEntity::find()
            .filter(
                crate::analysis::database::entities::symbol_info::Column::ProjectRoot
                    .eq(project_root_str),
            )
            .all(conn)
            .await
            .context("Failed to load symbols from database")?;

        // Load file hashes
        let file_hash_models = FileHashEntity::find()
            .filter(
                crate::analysis::database::entities::file_hash::Column::ProjectRoot
                    .eq(project_root_str),
            )
            .all(conn)
            .await
            .context("Failed to load file hashes from database")?;

        // If both are empty, return None
        if symbol_models.is_empty() && file_hash_models.is_empty() {
            debug!(
                "No symbols or file hashes found in database for project: {}",
                project_root_str
            );
            return Ok(None);
        }

        let symbols: Vec<AnalysisSymbolInfo> = if symbol_models.is_empty() {
            vec![] // symbols can be empty
        } else {
            symbol_models
                .into_iter()
                .map(active_model_to_symbol)
                .collect::<Result<Vec<_>>>()
                .context("Failed to convert database symbols to analysis symbols")?
        };

        let file_hashes: HashMap<PathBuf, String> = file_hash_models
            .into_iter()
            .map(|m| (PathBuf::from(m.file_path), m.hash))
            .collect();

        let repomap = RepoMap { symbols };

        info!(
            "Loaded repomap from database: {} symbols, {} file hashes",
            repomap.symbols.len(),
            file_hashes.len()
        );

        Ok(Some((repomap, file_hashes)))
    }

    /// Clears all repomap data (symbols and hashes) for a given project root.
    ///
    /// # Arguments
    /// * `conn` - The database connection or transaction.
    /// * `project_root` - The project root path.
    ///
    /// # Returns
    /// * `Result<()>` - Ok if successful, Err otherwise.
    pub async fn clear_repomap<C>(conn: &C, project_root: &Path) -> Result<()>
    where
        C: sea_orm::ConnectionTrait,
    {
        let project_root_str = project_root
            .to_str()
            .context("Project root path is not valid UTF-8")?;

        info!(
            "Clearing repomap data from database for project: {}",
            project_root_str
        );

        // Delete file hashes
        let _deleted_hashes = FileHashEntity::delete_many()
            .filter(
                crate::analysis::database::entities::file_hash::Column::ProjectRoot
                    .eq(project_root_str),
            )
            .exec(conn)
            .await
            .context("Failed to delete file hashes")?;

        // Delete symbols
        let _deleted_symbols = SymbolInfoEntity::delete_many()
            .filter(
                crate::analysis::database::entities::symbol_info::Column::ProjectRoot
                    .eq(project_root_str),
            )
            .exec(conn)
            .await
            .context("Failed to delete symbols")?;

        info!("Repomap data cleared successfully");
        Ok(())
    }

    /// Checks if the stored repomap is valid by comparing file hashes.
    ///
    /// # Arguments
    /// * `conn` - The database connection.
    /// * `project_root` - The project root path.
    /// * `current_hashes` - The current file hashes.
    ///
    /// # Returns
    /// * `Result<bool>` - True if valid, False otherwise.
    pub async fn is_repomap_valid(
        conn: &DatabaseConnection,
        project_root: &Path,
        current_hashes: &HashMap<PathBuf, String>,
    ) -> Result<bool> {
        let project_root_str = project_root
            .to_str()
            .context("Project root path is not valid UTF-8")?;

        // Load stored hashes
        let stored_data = Self::load_repomap(conn, project_root).await?;
        let Some((_, stored_hashes)) = stored_data else {
            debug!("No stored repomap found for project: {}", project_root_str);
            return Ok(false);
        };

        // Compare hashes
        let is_valid = stored_hashes == *current_hashes;
        debug!(
            "Repomap validity check for project {}: {}",
            project_root_str, is_valid
        );
        Ok(is_valid)
    }

    /// Gets a list of changed files by comparing stored hashes with current hashes.
    ///
    /// # Arguments
    /// * `conn` - The database connection.
    /// * `project_root` - The project root path.
    /// * `current_hashes` - The current file hashes.
    ///
    /// # Returns
    /// * `Result<Vec<PathBuf>>` - List of changed file paths.
    pub async fn get_changed_files(
        conn: &DatabaseConnection,
        project_root: &Path,
        current_hashes: &HashMap<PathBuf, String>,
    ) -> Result<Vec<PathBuf>> {
        let project_root_str = project_root
            .to_str()
            .context("Project root path is not valid UTF-8")?;

        // Load stored hashes
        let stored_data = Self::load_repomap(conn, project_root).await?;
        let Some((_, stored_hashes)) = stored_data else {
            // If no stored data, all current files are considered changed
            return Ok(current_hashes.keys().cloned().collect());
        };

        let mut changed_files = Vec::new();

        // Check for new or modified files
        for (path, current_hash) in current_hashes {
            match stored_hashes.get(path) {
                Some(stored_hash) if stored_hash == current_hash => {
                    // Unchanged
                }
                _ => {
                    // New or modified
                    changed_files.push(path.clone());
                }
            }
        }

        // Check for deleted files
        for stored_path in stored_hashes.keys() {
            if !current_hashes.contains_key(stored_path) {
                changed_files.push(stored_path.clone());
            }
        }

        debug!(
            "Found {} changed files for project {}",
            changed_files.len(),
            project_root_str
        );
        Ok(changed_files)
    }
}

/// Converts an AnalysisSymbolInfo to a SymbolInfo ActiveModel for database insertion.
fn symbol_to_active_model(
    symbol: &AnalysisSymbolInfo,
    project_root: &str,
) -> Result<SymbolInfoActiveModel> {
    use crate::analysis::database::entities::symbol_info::*;
    let file_path_str = symbol
        .file
        .to_str()
        .context("File path is not valid UTF-8")?;

    // Convert keywords to a JSON string
    let keywords_json =
        serde_json::to_string(&symbol.keywords).unwrap_or_else(|_| "[]".to_string());

    Ok(ActiveModel {
        id: Default::default(), // Auto-increment
        name: Set(symbol.name.clone()),
        kind: Set(match symbol.kind {
            crate::analysis::symbol::SymbolKind::Function => "fn".to_string(),
            crate::analysis::symbol::SymbolKind::Struct => "struct".to_string(),
            crate::analysis::symbol::SymbolKind::Enum => "enum".to_string(),
            crate::analysis::symbol::SymbolKind::Trait => "trait".to_string(),
            crate::analysis::symbol::SymbolKind::Impl => "impl".to_string(),
            crate::analysis::symbol::SymbolKind::Method => "method".to_string(),
            crate::analysis::symbol::SymbolKind::AssocFn => "assoc_fn".to_string(),
            crate::analysis::symbol::SymbolKind::Mod => "mod".to_string(),
            crate::analysis::symbol::SymbolKind::Variable => "var".to_string(),
            crate::analysis::symbol::SymbolKind::Comment => "comment".to_string(),
        }),
        file_path: Set(file_path_str.to_string()),
        start_line: Set(symbol.start_line as i32),
        start_col: Set(symbol.start_col as i32),
        end_line: Set(symbol.end_line as i32),
        end_col: Set(symbol.end_col as i32),
        parent: Set(symbol.parent.clone()),
        file_total_lines: Set(symbol.file_total_lines as i32),
        function_lines: Set(symbol.function_lines.map(|l| l as i32)),
        project_root: Set(project_root.to_string()),
        keywords: Set(keywords_json),
        created_at: Set(Utc::now()),
    })
}

/// Converts a SymbolInfo Model from the database to an AnalysisSymbolInfo.
fn active_model_to_symbol(model: SymbolInfoModel) -> Result<AnalysisSymbolInfo> {
    let kind = match model.kind.as_str() {
        "fn" => crate::analysis::symbol::SymbolKind::Function,
        "struct" => crate::analysis::symbol::SymbolKind::Struct,
        "enum" => crate::analysis::symbol::SymbolKind::Enum,
        "trait" => crate::analysis::symbol::SymbolKind::Trait,
        "impl" => crate::analysis::symbol::SymbolKind::Impl,
        "method" => crate::analysis::symbol::SymbolKind::Method,
        "assoc_fn" => crate::analysis::symbol::SymbolKind::AssocFn,
        "mod" => crate::analysis::symbol::SymbolKind::Mod,
        "var" => crate::analysis::symbol::SymbolKind::Variable,
        "comment" => crate::analysis::symbol::SymbolKind::Comment,
        _ => {
            return Err(anyhow::anyhow!(
                "unexpected value for SymbolKind: {}",
                model.kind
            ));
        }
    };

    // Parse keywords from JSON string
    let keywords: Vec<String> =
        serde_json::from_str(&model.keywords).unwrap_or_else(|_| Vec::new());

    Ok(AnalysisSymbolInfo {
        name: model.name,
        kind,
        file: PathBuf::from(model.file_path),
        start_line: model.start_line as usize,
        start_col: model.start_col as usize,
        end_line: model.end_line as usize,
        end_col: model.end_col as usize,
        parent: model.parent,
        file_total_lines: model.file_total_lines as usize,
        function_lines: model.function_lines.map(|l| l as usize),
        keywords,
    })
}

/// Converts file path and hash to a FileHash ActiveModel for database insertion.
fn file_hash_to_active_model(
    file_path: &str,
    hash: &str,
    project_root: &str,
) -> FileHashActiveModel {
    use crate::analysis::database::entities::file_hash::*;
    ActiveModel {
        id: Default::default(), // Auto-increment
        file_path: Set(file_path.to_string()),
        hash: Set(hash.to_string()),
        project_root: Set(project_root.to_string()),
        created_at: Set(Utc::now()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::symbol::SymbolKind;
    use sea_orm::{Database, DatabaseConnection};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tempfile::TempDir;

    async fn setup_test_db() -> (TempDir, DatabaseConnection) {
        let tmp_dir = TempDir::new().expect("Failed to create temp dir");
        let db_path = tmp_dir.path().join("test.db");
        // Add `mode=rwc` query parameter to ensure the file is created
        let db_url = format!("sqlite://{}?mode=rwc", db_path.to_string_lossy());
        let db = Database::connect(&db_url)
            .await
            .expect("Failed to connect to test database");
        // Run migrations
        crate::analysis::database::migration::run_migrations(&db)
            .await
            .expect("Failed to run migrations");
        (tmp_dir, db)
    }

    #[tokio::test]
    async fn test_save_and_load_repomap() {
        let (_tmp_dir, db) = setup_test_db().await;
        let project_root = PathBuf::from("/test/project");
        let symbols = vec![AnalysisSymbolInfo {
            name: "test_function".to_string(),
            kind: SymbolKind::Function,
            file: PathBuf::from("/test/project/src/main.rs"),
            start_line: 1,
            start_col: 0,
            end_line: 3,
            end_col: 1,
            parent: None,
            file_total_lines: 10,
            function_lines: Some(3),
            keywords: vec![],
        }];
        let repomap = RepoMap { symbols };
        let mut hashes = HashMap::new();
        hashes.insert(
            PathBuf::from("/test/project/src/main.rs"),
            "hash1".to_string(),
        );

        // Save
        assert!(
            RepomapDAO::save_repomap(&db, &repomap, &hashes, &project_root)
                .await
                .is_ok()
        );

        // Load
        let loaded = RepomapDAO::load_repomap(&db, &project_root)
            .await
            .expect("Failed to load repomap");
        assert!(loaded.is_some());
        let (loaded_repomap, loaded_hashes) = loaded.unwrap();
        assert_eq!(loaded_repomap.symbols.len(), 1);
        assert_eq!(loaded_repomap.symbols[0].name, "test_function");
        assert_eq!(loaded_hashes.len(), 1);
        assert_eq!(
            loaded_hashes.get(&PathBuf::from("/test/project/src/main.rs")),
            Some(&"hash1".to_string())
        );
    }

    #[tokio::test]
    async fn test_clear_repomap() {
        let (_tmp_dir, db) = setup_test_db().await;
        let project_root = PathBuf::from("/test/project");
        let repomap = RepoMap { symbols: vec![] };
        let mut hashes = HashMap::new();
        hashes.insert(
            PathBuf::from("/test/project/src/main.rs"),
            "hash1".to_string(),
        );

        // Save
        assert!(
            RepomapDAO::save_repomap(&db, &repomap, &hashes, &project_root)
                .await
                .is_ok()
        );

        // Clear
        assert!(RepomapDAO::clear_repomap(&db, &project_root).await.is_ok());

        // Load - should be None
        let loaded = RepomapDAO::load_repomap(&db, &project_root)
            .await
            .expect("Failed to load repomap");
        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn test_is_repomap_valid() {
        let (_tmp_dir, db) = setup_test_db().await;
        let project_root = PathBuf::from("/test/project");
        let repomap = RepoMap { symbols: vec![] };
        let mut hashes = HashMap::new();
        hashes.insert(
            PathBuf::from("/test/project/src/main.rs"),
            "hash1".to_string(),
        );

        // Save
        assert!(
            RepomapDAO::save_repomap(&db, &repomap, &hashes, &project_root)
                .await
                .is_ok()
        );

        // Valid
        assert!(
            RepomapDAO::is_repomap_valid(&db, &project_root, &hashes)
                .await
                .expect("Failed to check validity")
        );

        // Invalid
        let mut modified_hashes = hashes.clone();
        modified_hashes.insert(
            PathBuf::from("/test/project/src/other.rs"),
            "hash2".to_string(),
        );
        assert!(
            !RepomapDAO::is_repomap_valid(&db, &project_root, &modified_hashes)
                .await
                .expect("Failed to check validity")
        );
    }

    #[tokio::test]
    async fn test_get_changed_files() {
        let (_tmp_dir, db) = setup_test_db().await;
        let project_root = PathBuf::from("/test/project");
        let repomap = RepoMap { symbols: vec![] };
        let mut hashes = HashMap::new();
        hashes.insert(
            PathBuf::from("/test/project/src/main.rs"),
            "hash1".to_string(),
        );
        hashes.insert(
            PathBuf::from("/test/project/src/lib.rs"),
            "hash2".to_string(),
        );

        // Save
        assert!(
            RepomapDAO::save_repomap(&db, &repomap, &hashes, &project_root)
                .await
                .is_ok()
        );

        // No changes
        let changed = RepomapDAO::get_changed_files(&db, &project_root, &hashes)
            .await
            .expect("Failed to get changed files");
        assert_eq!(changed.len(), 0);

        // Added file
        let mut modified_hashes = hashes.clone();
        modified_hashes.insert(
            PathBuf::from("/test/project/src/new.rs"),
            "hash3".to_string(),
        );
        let changed = RepomapDAO::get_changed_files(&db, &project_root, &modified_hashes)
            .await
            .expect("Failed to get changed files");
        assert_eq!(changed.len(), 1);
        assert!(changed.contains(&PathBuf::from("/test/project/src/new.rs")));

        // Modified file
        let mut modified_hashes = hashes.clone();
        modified_hashes.insert(
            PathBuf::from("/test/project/src/main.rs"),
            "hash1_modified".to_string(),
        );
        let changed = RepomapDAO::get_changed_files(&db, &project_root, &modified_hashes)
            .await
            .expect("Failed to get changed files");
        assert_eq!(changed.len(), 1);
        assert!(changed.contains(&PathBuf::from("/test/project/src/main.rs")));

        // Deleted file
        let mut modified_hashes = HashMap::new();
        modified_hashes.insert(
            PathBuf::from("/test/project/src/main.rs"),
            "hash1".to_string(),
        );
        let changed = RepomapDAO::get_changed_files(&db, &project_root, &modified_hashes)
            .await
            .expect("Failed to get changed files");
        assert_eq!(changed.len(), 1);
        assert!(changed.contains(&PathBuf::from("/test/project/src/lib.rs")));
    }
}
