use crate::analysis::RepoMap;
use crate::analysis::collector::{
    collect_symbols_js, collect_symbols_py, collect_symbols_rust, collect_symbols_ts,
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

type CollectorFn = fn(&mut RepoMap, &Tree, &str, &Path);

struct LanguageConfig {
    language: Language,
    collector: CollectorFn,
    extensions: &'static [&'static str],
}

fn language_configs() -> &'static [LanguageConfig] {
    static CONFIGS: OnceLock<Vec<LanguageConfig>> = OnceLock::new();
    CONFIGS.get_or_init(|| {
        vec![
            LanguageConfig {
                language: tree_sitter_rust::LANGUAGE.into(),
                collector: collect_symbols_rust,
                extensions: &["rs"],
            },
            LanguageConfig {
                language: tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                collector: collect_symbols_ts,
                extensions: &["ts", "tsx"],
            },
            LanguageConfig {
                language: tree_sitter_javascript::LANGUAGE.into(),
                collector: collect_symbols_js,
                extensions: &["js", "mjs", "cjs"],
            },
            LanguageConfig {
                language: tree_sitter_python::LANGUAGE.into(),
                collector: collect_symbols_py,
                extensions: &["py"],
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

// 並列処理のためのタスク定義
#[derive(Debug)]
pub struct ParseTask {
    pub file_path: PathBuf,
}

// ファイル走査ロジック
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

// 単一ファイルパースロジック
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

// 単一ファイルのシンボル抽出ロジック
fn process_single_file(
    file_path: PathBuf,
    parse_result: (Tree, String, &'static LanguageConfig),
) -> RepoMap {
    let (tree, src, config) = parse_result;
    let mut map = RepoMap::default();
    (config.collector)(&mut map, &tree, &src, &file_path);
    map
}

pub struct Analyzer {
    root: PathBuf,
    parser: Parser,
    current_lang: Language,
}

impl Analyzer {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let mut parser = Parser::new();
        // Default to rust
        let lang: Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&lang).context("set rust language")?;
        Ok(Self {
            root: root.into(),
            parser,
            current_lang: lang,
        })
    }

    /// 逐次処理バージョン (将来の並列化を見据えてインターフェースを分離)
    pub async fn build_sequential(&mut self) -> Result<RepoMap> {
        info!("Starting to build RepoMap for project at {:?}", self.root);
        let start_time = std::time::Instant::now();

        let files = find_target_files(&self.root)?;
        let mut maps = Vec::new();

        for file_path in files {
            match parse_single_file(&file_path, &mut self.parser, &mut self.current_lang) {
                Ok(Some(parse_result)) => {
                    let map = process_single_file(file_path.clone(), parse_result);
                    maps.push(map);
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
            maps_len, // ここで maps_len を使用
            duration,
            final_map.symbols.len()
        );
        Ok(final_map)
    }

    /// 並列処理バージョン
    pub async fn build_parallel(&mut self) -> Result<RepoMap> {
        info!(
            "Starting to build RepoMap (parallel) for project at {:?}",
            self.root
        );
        let start_time = std::time::Instant::now();

        // 1. ファイルリストを取得
        let files = find_target_files(&self.root)?;
        let file_count = files.len();
        info!("Found {} target files for analysis", file_count);

        // 2. ファイルリストをチャンクに分割
        // ここでは、単純にファイル数をCPU数で割ってチャンクサイズを決定します。
        // ただし、最小チャンクサイズを設定して、小さなプロジェクトでも並列処理が有効になるようにします。
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

        // 3. 各チャンクをtokio::spawnで処理
        let mut tasks = Vec::new();
        for chunk in chunks {
            // 各タスクに必要なデータをmoveで渡す
            let _root_clone = self.root.clone();

            let task = task::spawn(async move {
                // 各タスクで新しい Parser を作成
                let mut parser = Parser::new();
                // Default to rust
                let mut current_lang: Language = tree_sitter_rust::LANGUAGE.into();
                parser
                    .set_language(&current_lang)
                    .context("set rust language in task")?;

                let mut map = RepoMap::default();
                let mut _parsed_file_count = 0;

                for file_path in chunk {
                    match parse_single_file(&file_path, &mut parser, &mut current_lang) {
                        Ok(Some(parse_result)) => {
                            let single_file_map =
                                process_single_file(file_path.clone(), parse_result);
                            map = map.merge(single_file_map);
                            // parsed_file_count += 1; // この行を削除
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

        // 4. すべてのタスクの結果を収集
        let mut maps = Vec::new();
        for task in tasks {
            match task.await {
                Ok(Ok(map)) => {
                    maps.push(map);
                }
                Ok(Err(e)) => {
                    error!("Task failed with error: {:?}", e);
                }
                Err(e) => {
                    error!("Task join failed with error: {:?}", e);
                }
            }
        }

        // 5. 結果をマージ
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

    /// デフォルトのビルドメソッド
    pub async fn build(&mut self) -> Result<RepoMap> {
        // 並列処理を使用
        self.build_parallel().await
    }
}
