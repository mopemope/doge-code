use crate::analysis::RepoMap;
use crate::analysis::cache::{RepomapCache, RepomapStore};
use crate::analysis::file_finder::find_target_files;
use crate::analysis::hash::{calculate_file_hashes, calculate_hash_diff};
use crate::analysis::parser::{parse_single_file, process_single_file};
use anyhow::{Context, Result};
use num_cpus;
use std::{collections::HashMap, path::PathBuf};
use tokio::task;
use tracing::{debug, error, info, warn};
use tree_sitter::{Language, Parser};

pub struct Analyzer {
    root: PathBuf,
    parser: Parser,
    current_lang: Language,
    cache_store: RepomapStore,
}

impl Analyzer {
    pub async fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let mut parser = Parser::new();
        let lang: Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&lang).context("set rust language")?;
        let cache_store = RepomapStore::new(root.clone())
            .await
            .context("Failed to create RepomapStore")?;
        Ok(Self {
            root,
            parser,
            current_lang: lang,
            cache_store,
        })
    }

    pub async fn build_sequential(&mut self) -> Result<RepoMap> {
        info!(
            "Starting to build RepoMap for project at {:?}, sequential",
            self.root
        );
        let start_time = std::time::Instant::now();

        let files = find_target_files(&self.root)?;
        let mut maps = Vec::new();

        for file_path in files {
            match parse_single_file(&file_path, &mut self.parser, &mut self.current_lang) {
                Ok(Some(parse_result)) => {
                    match process_single_file(file_path.clone(), parse_result) {
                        Ok(map) => maps.push(map),
                        Err(e) => error!("Failed to process file {}: {:?}", file_path.display(), e),
                    }
                }
                Ok(None) => {
                    debug!("Skipped file (no parser): {}", file_path.display());
                }
                Err(e) => {
                    error!("Failed to parse file {}: {:?}", file_path.display(), e);
                }
            }
        }

        let maps_len = maps.len();
        let final_map = RepoMap::merge_many(maps);
        let duration = start_time.elapsed();
        info!(
            "Finished building RepoMap (sequential). Parsed {} files in {:?}. Found {} symbols.",
            maps_len,
            duration,
            final_map.symbols.len()
        );
        Ok(final_map)
    }

    pub async fn build_parallel(&mut self) -> Result<RepoMap> {
        info!(
            "Starting to build RepoMap (parallel) for project at {:?}, parallel",
            self.root
        );
        let start_time = std::time::Instant::now();

        let files = find_target_files(&self.root)?;
        let file_count = files.len();
        info!("Found {} target files for analysis", file_count);

        let num_cpus = num_cpus::get();
        // Set the number of chunks to the minimum of the number of CPUs and the number of files
        let num_chunks = std::cmp::min(num_cpus, file_count);
        // Calculate the chunk size (set to 1 if the number of files is 0)
        let chunk_size = std::cmp::max(1, file_count.div_ceil(num_chunks));
        let chunks: Vec<Vec<PathBuf>> = files
            .chunks(chunk_size)
            .map(|chunk| chunk.to_vec())
            .collect();

        info!(
            "Divided {} files into {} chunks (chunk size: {})",
            file_count,
            chunks.len(),
            chunk_size
        );

        let mut tasks = Vec::new();
        for chunk in chunks {
            let _root_clone = self.root.clone();

            let task = task::spawn(async move {
                let mut parser = Parser::new();
                let mut current_lang: Language = tree_sitter_rust::LANGUAGE.into();
                parser
                    .set_language(&current_lang)
                    .context("set rust language in task")?;

                let mut map = RepoMap::default();
                let mut _parsed_file_count = 0;

                for file_path in chunk {
                    match parse_single_file(&file_path, &mut parser, &mut current_lang) {
                        Ok(Some(parse_result)) => {
                            match process_single_file(file_path.clone(), parse_result) {
                                Ok(single_file_map) => map = map.merge(single_file_map),
                                Err(e) => error!(
                                    "Failed to process file {}: {:?}",
                                    file_path.display(),
                                    e
                                ),
                            }
                        }
                        Ok(None) => {
                            debug!("Skipped file (no parser): {}", file_path.display());
                        }
                        Err(e) => {
                            error!("Failed to parse file {}: {:?}", file_path.display(), e);
                        }
                    }
                }

                Ok::<RepoMap, anyhow::Error>(map)
            });
            tasks.push(task);
        }

        let mut maps = Vec::new();
        for task in tasks {
            match task.await {
                Ok(Ok(map)) => {
                    maps.push(map);
                }
                Ok(Err(e)) => {
                    error!("Task failed with error: {:?}", e);
                }
                Err(_e) => {
                    // error!("Task join failed with error: {:?}", e);
                }
            }
        }

        let final_map = RepoMap::merge_many(maps);

        let duration = start_time.elapsed();
        info!(
            "Finished building RepoMap (parallel). Parsed {} files in {:?}. Found {} symbols.",
            file_count,
            duration,
            final_map.symbols.len()
        );

        Ok(final_map)
    }

    pub async fn build(&mut self) -> Result<RepoMap> {
        self.build_with_cache().await
    }

    /// Build repomap using cache
    pub async fn build_with_cache(&mut self) -> Result<RepoMap> {
        info!(
            "Starting to build RepoMap with cache for project at {:?}",
            self.root
        );
        let start_time = std::time::Instant::now();

        // Search for target files
        let files = find_target_files(&self.root)?;
        info!("Found {} target files for analysis", files.len());

        // Calculate current file hashes
        let current_hashes = calculate_file_hashes(&files).await;
        info!("Calculated hashes for {} files", current_hashes.len());

        // Check cache validity
        if self
            .cache_store
            .is_cache_valid(&current_hashes)
            .await
            .context("Failed to check cache validity")?
        {
            info!("Cache is valid, loading from cache");
            if let Some(cache) = self
                .cache_store
                .load()
                .await
                .context("Failed to load cache")?
            {
                let duration = start_time.elapsed();
                info!(
                    "Loaded RepoMap from cache in {:?}. Found {} symbols from {} files.",
                    duration,
                    cache.repomap.symbols.len(),
                    cache.file_hashes.len()
                );
                return Ok(cache.repomap);
            }
        }

        // If the cache is invalid or does not exist, try a differential update
        let repomap = if let Some(cached_data) = self
            .cache_store
            .load()
            .await
            .context("Failed to load cache")?
        {
            info!("Cache exists but is invalid, attempting incremental update");
            self.build_incremental(cached_data, &current_hashes).await?
        } else {
            info!("No cache found, building from scratch");
            self.build_parallel().await?
        };

        // Save new cache
        let cache = RepomapCache::new(self.root.clone(), repomap.clone(), current_hashes);
        if let Err(e) = self.cache_store.save(&cache).await {
            warn!("Failed to save repomap cache: {}", e);
        }

        let duration = start_time.elapsed();
        info!(
            "Built RepoMap with cache in {:?}. Found {} symbols from {} files.",
            duration,
            repomap.symbols.len(),
            files.len()
        );

        Ok(repomap)
    }

    /// Build repomap with incremental updates
    async fn build_incremental(
        &mut self,
        cached_data: RepomapCache,
        current_hashes: &HashMap<PathBuf, String>,
    ) -> Result<RepoMap> {
        info!("Building RepoMap incrementally");

        let diff = calculate_hash_diff(&cached_data.file_hashes, current_hashes);

        if !diff.has_changes() {
            info!("No file changes detected, using cached repomap");
            return Ok(cached_data.repomap);
        }

        info!(
            "Detected {} file changes: {} added, {} modified, {} removed",
            diff.total_changes(),
            diff.added.len(),
            diff.modified.len(),
            diff.removed.len()
        );

        // Analyze only changed files
        let changed_files = diff.changed_files();
        let mut new_maps = Vec::new();

        if !changed_files.is_empty() {
            info!("Re-analyzing {} changed files", changed_files.len());

            // Set the number of chunks to the minimum of the number of CPUs and the number of files
            let num_chunks = std::cmp::min(num_cpus::get(), changed_files.len());
            // Calculate the chunk size (set to 1 if the number of files is 0)
            let chunk_size = std::cmp::max(1, changed_files.len().div_ceil(num_chunks));
            let chunks: Vec<Vec<PathBuf>> = changed_files
                .chunks(chunk_size)
                .map(|chunk| chunk.to_vec())
                .collect();

            let mut tasks = Vec::new();
            for chunk in chunks {
                let task = task::spawn(async move {
                    let mut parser = Parser::new();
                    let mut current_lang: Language = tree_sitter_rust::LANGUAGE.into();
                    parser
                        .set_language(&current_lang)
                        .context("set rust language in incremental task")?;

                    let mut map = RepoMap::default();
                    for file_path in chunk {
                        match parse_single_file(&file_path, &mut parser, &mut current_lang) {
                            Ok(Some(parse_result)) => {
                                match process_single_file(file_path.clone(), parse_result) {
                                    Ok(single_file_map) => map = map.merge(single_file_map),
                                    Err(e) => error!(
                                        "Failed to process file {}: {:?}",
                                        file_path.display(),
                                        e
                                    ),
                                }
                            }
                            Ok(None) => {
                                debug!("Skipped file (no parser): {}", file_path.display());
                            }
                            Err(e) => {
                                error!("Failed to parse file {}: {:?}", file_path.display(), e);
                            }
                        }
                    }
                    Ok::<RepoMap, anyhow::Error>(map)
                });
                tasks.push(task);
            }

            for task in tasks {
                match task.await {
                    Ok(Ok(map)) => new_maps.push(map),
                    Ok(Err(e)) => error!("Incremental task failed: {:?}", e),
                    Err(e) => error!("Incremental task join failed: {:?}", e),
                }
            }
        }

        // Remove symbols from deleted files from the existing repomap
        let mut updated_repomap = cached_data.repomap;
        if !diff.removed.is_empty() {
            info!("Removing symbols from {} deleted files", diff.removed.len());
            updated_repomap
                .symbols
                .retain(|symbol| !diff.removed.contains(&symbol.file));
        }

        // Remove symbols from changed files (to be replaced with new symbols)
        let changed_files_set: std::collections::HashSet<_> = changed_files.iter().collect();
        updated_repomap
            .symbols
            .retain(|symbol| !changed_files_set.contains(&symbol.file));

        // Add new symbols
        for new_map in new_maps {
            updated_repomap = updated_repomap.merge(new_map);
        }

        info!(
            "Incremental update completed: {} symbols total",
            updated_repomap.symbols.len()
        );

        Ok(updated_repomap)
    }

    /// Clear cache
    pub async fn clear_cache(&self) -> Result<()> {
        self.cache_store
            .clear()
            .await
            .context("Failed to clear cache")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_find_target_files_with_gitignore() {
        // Create a temporary directory for testing
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create a .gitignore file
        let gitignore_content = "target/";
        std::fs::write(root.join(".gitignore"), gitignore_content).unwrap();

        // Create a source file in the root directory
        let src_file = root.join("main.rs");
        std::fs::write(&src_file, "fn main() {}").unwrap();

        // Create a target directory and a source file inside it
        let target_dir = root.join("target");
        std::fs::create_dir(&target_dir).unwrap();
        let target_src_file = target_dir.join("generated.rs");
        std::fs::write(&target_src_file, "fn generated() {}").unwrap();

        // Call find_target_files
        let files = find_target_files(root).unwrap();

        // Check that the file in the root directory is found
        assert!(files.contains(&src_file));

        // Check that the file in the target directory is NOT found
        assert!(!files.contains(&target_src_file));

        // Print all found files for debugging
        println!("Found files:");
        for file in &files {
            println!("  {}", file.display());
        }
    }

    #[test]
    fn test_ignore_crate_with_gitignore() {
        use ignore::WalkBuilder;

        // Create a temporary directory for testing
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create a .gitignore file
        let gitignore_content = "target/";
        std::fs::write(root.join(".gitignore"), gitignore_content).unwrap();

        // Create a source file in the root directory
        let src_file = root.join("main.rs");
        std::fs::write(&src_file, "fn main() {}").unwrap();

        // Create a target directory and a source file inside it
        let target_dir = root.join("target");
        std::fs::create_dir(&target_dir).unwrap();
        let target_src_file = target_dir.join("generated.rs");
        std::fs::write(&target_src_file, "fn generated() {}").unwrap();

        // Use ignore crate to walk the directory with WalkBuilder
        let mut files = Vec::new();
        for result in WalkBuilder::new(root)
            .git_ignore(true) // Explicitly enable .gitignore
            .require_git(false) // Allow .gitignore outside of git repo
            .build()
        {
            let entry = result.unwrap();
            if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                files.push(entry.path().to_path_buf());
            }
        }

        // Check that the file in the root directory is found
        assert!(files.contains(&src_file));

        // Check that the file in the target directory is NOT found
        assert!(!files.contains(&target_src_file));

        // Print all found files for debugging
        println!("Found files with ignore crate (WalkBuilder):");
        for file in &files {
            println!("  {}", file.display());
        }
    }

    #[tokio::test]
    async fn test_build_incremental_with_changes() {
        use crate::analysis::hash::calculate_file_hashes;
        use std::collections::HashMap;
        use tempfile::TempDir;

        // Create a temporary directory for testing
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create an initial source file
        let initial_file = root.join("lib.rs");
        let initial_content = r#"
            fn main() {
                println!("Hello, world!");
            }
        "#;
        std::fs::write(&initial_file, initial_content).unwrap();

        // Create an Analyzer and build initial repomap
        let mut analyzer = Analyzer::new(root).await.unwrap();
        let initial_repomap = analyzer.build().await.unwrap();

        // Modify the source file
        let modified_content = r#"
            fn main() {
                println!("Hello, modified world!");
            }

            fn new_function() {
                println!("This is a new function.");
            }
        "#;
        std::fs::write(&initial_file, modified_content).unwrap();

        // Calculate new hashes
        let files = vec![initial_file.clone()];
        let new_hashes = calculate_file_hashes(&files).await;

        // Perform incremental update
        let updated_repomap = analyzer
            .build_incremental(
                crate::analysis::cache::RepomapCache::new(
                    root.to_path_buf(),
                    initial_repomap.clone(),
                    HashMap::new(), // Empty initial hashes for test simplicity
                ),
                &new_hashes,
            )
            .await
            .unwrap();

        // Verify that the repomap has been updated
        assert_ne!(initial_repomap.symbols.len(), updated_repomap.symbols.len());
        assert!(
            updated_repomap
                .symbols
                .iter()
                .any(|s| s.name == "new_function")
        );
    }
}
