use crate::analysis::RepoMap;
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
}

impl Analyzer {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let mut parser = Parser::new();
        let lang: Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&lang).context("set rust language")?;
        Ok(Self {
            root: root.into(),
            parser,
            current_lang: lang,
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
        self.build_parallel().await
    }
}
