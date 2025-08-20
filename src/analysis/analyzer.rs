use crate::analysis::RepoMap;
use crate::analysis::cache::{RepomapCache, RepomapStore};
use crate::analysis::hash::{calculate_file_hashes, calculate_hash_diff};
use crate::analysis::{
    GoExtractor, JavaScriptExtractor, LanguageSpecificExtractor, PythonExtractor, RustExtractor,
    TypeScriptExtractor,
};
use anyhow::{Context, Result};
use num_cpus;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};
use tokio::task;
use tracing::{debug, error, info, warn};
use tree_sitter::{Language, Parser, Tree};

struct LanguageConfig {
    language: Language,
    collector: Box<dyn LanguageSpecificExtractor>,
    extensions: &'static [&'static str],
}

fn language_configs() -> &'static [LanguageConfig] {
    static CONFIGS: OnceLock<Vec<LanguageConfig>> = OnceLock::new();
    CONFIGS.get_or_init(|| {
        vec![
            LanguageConfig {
                language: tree_sitter_rust::LANGUAGE.into(),
                collector: Box::new(RustExtractor),
                extensions: &["rs"],
            },
            LanguageConfig {
                language: tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                collector: Box::new(TypeScriptExtractor),
                extensions: &["ts", "tsx"],
            },
            LanguageConfig {
                language: tree_sitter_javascript::LANGUAGE.into(),
                collector: Box::new(JavaScriptExtractor),
                extensions: &["js", "mjs", "cjs"],
            },
            LanguageConfig {
                language: tree_sitter_python::LANGUAGE.into(),
                collector: Box::new(PythonExtractor),
                extensions: &["py"],
            },
            LanguageConfig {
                language: tree_sitter_go::LANGUAGE.into(),
                collector: Box::new(GoExtractor),
                extensions: &["go"],
            },
        ]
    })
}

fn extension_map() -> &'static HashMap<&'static str, &'static LanguageConfig> {
    static EXT_MAP: OnceLock<HashMap<&'static str, &'static LanguageConfig>> = OnceLock::new();
    EXT_MAP.get_or_init(|| {
        let mut map = HashMap::new();
        for config in language_configs() {
            for &ext in config.extensions {
                map.insert(ext, config);
            }
        }
        map
    })
}

#[derive(Debug)]
pub struct ParseTask {
    pub file_path: PathBuf,
}

fn find_target_files(root: &Path) -> Result<Vec<PathBuf>> {
    let patterns: Vec<_> = language_configs()
        .iter()
        .flat_map(|c| c.extensions.iter().map(|ext| format!("**/*.{}", ext)))
        .collect();

    let walker = globwalk::GlobWalkerBuilder::from_patterns(root, &patterns)
        .follow_links(false)
        .case_insensitive(true)
        .build()
        .context("build glob walker")?;

    let mut files = Vec::new();
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!("Failed to walk entry: {:?}", e);
                continue;
            }
        };
        if entry.file_type().is_dir() {
            continue;
        }
        files.push(entry.path().to_path_buf());
    }
    Ok(files)
}

fn parse_single_file(
    file_path: &Path,
    parser: &mut Parser,
    current_lang: &mut Language,
) -> Result<Option<(Tree, String, &'static LanguageConfig)>> {
    let ext = match file_path.extension().and_then(|s| s.to_str()) {
        Some(ext) => ext,
        None => return Ok(None),
    };

    if let Some(config) = extension_map().get(ext) {
        let src = fs::read_to_string(file_path)
            .with_context(|| format!("read {}", file_path.display()))?;
        if *current_lang != config.language {
            parser
                .set_language(&config.language)
                .context("set language")?;
            *current_lang = config.language.clone();
        }
        let tree = parser
            .parse(&src, None)
            .ok_or_else(|| anyhow::anyhow!("parse returned None"))?;
        Ok(Some((tree, src, config)))
    } else {
        Ok(None)
    }
}

fn process_single_file(
    file_path: PathBuf,
    parse_result: (Tree, String, &'static LanguageConfig),
) -> Result<RepoMap> {
    let (tree, src, config) = parse_result;
    let mut map = RepoMap::default();
    config
        .collector
        .extract_symbols(&mut map, &tree, &src, &file_path)?;
    Ok(map)
}

pub struct Analyzer {
    root: PathBuf,
    parser: Parser,
    current_lang: Language,
    cache_store: RepomapStore,
}

impl Analyzer {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let mut parser = Parser::new();
        let lang: Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&lang).context("set rust language")?;
        let cache_store = RepomapStore::new(root.clone())?;
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
        let chunk_size = std::cmp::max(1, file_count / num_cpus);
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

    /// キャッシュを使用してrepomapを構築
    pub async fn build_with_cache(&mut self) -> Result<RepoMap> {
        info!(
            "Starting to build RepoMap with cache for project at {:?}",
            self.root
        );
        let start_time = std::time::Instant::now();

        // 対象ファイルを検索
        let files = find_target_files(&self.root)?;
        info!("Found {} target files for analysis", files.len());

        // 現在のファイルハッシュを計算
        let current_hashes = calculate_file_hashes(&files).await;
        info!("Calculated hashes for {} files", current_hashes.len());

        // キャッシュの有効性をチェック
        if self.cache_store.is_cache_valid(&current_hashes)? {
            info!("Cache is valid, loading from cache");
            if let Some(cache) = self.cache_store.load()? {
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

        // キャッシュが無効または存在しない場合は、差分更新を試行
        let repomap = if let Some(cached_data) = self.cache_store.load()? {
            info!("Cache exists but is invalid, attempting incremental update");
            self.build_incremental(cached_data, &current_hashes).await?
        } else {
            info!("No cache found, building from scratch");
            self.build_parallel().await?
        };

        // 新しいキャッシュを保存
        let cache = RepomapCache::new(self.root.clone(), repomap.clone(), current_hashes);
        if let Err(e) = self.cache_store.save(&cache) {
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

    /// 差分更新でrepomapを構築
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

        // 変更されたファイルのみを解析
        let changed_files = diff.changed_files();
        let mut new_maps = Vec::new();

        if !changed_files.is_empty() {
            info!("Re-analyzing {} changed files", changed_files.len());

            // 変更されたファイルを並列処理
            let chunk_size = std::cmp::max(1, changed_files.len() / num_cpus::get());
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

        // 既存のrepomapから削除されたファイルのシンボルを除去
        let mut updated_repomap = cached_data.repomap;
        if !diff.removed.is_empty() {
            info!("Removing symbols from {} deleted files", diff.removed.len());
            updated_repomap
                .symbols
                .retain(|symbol| !diff.removed.contains(&symbol.file));
        }

        // 変更されたファイルのシンボルを除去（新しいシンボルで置き換えるため）
        let changed_files_set: std::collections::HashSet<_> = changed_files.iter().collect();
        updated_repomap
            .symbols
            .retain(|symbol| !changed_files_set.contains(&symbol.file));

        // 新しいシンボルを追加
        for new_map in new_maps {
            updated_repomap = updated_repomap.merge(new_map);
        }

        info!(
            "Incremental update completed: {} symbols total",
            updated_repomap.symbols.len()
        );

        Ok(updated_repomap)
    }

    /// キャッシュをクリア
    pub fn clear_cache(&self) -> Result<()> {
        self.cache_store.clear()
    }
}
