use crate::analysis::RepoMap;
use crate::analysis::collector::{
    collect_symbols_js, collect_symbols_py, collect_symbols_rust, collect_symbols_ts,
};
use anyhow::{Context, Result};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};
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

    fn parse_file(
        &mut self,
        path: &Path,
    ) -> Result<Option<(Tree, String, &'static LanguageConfig)>> {
        let ext = match path.extension().and_then(|s| s.to_str()) {
            Some(ext) => ext,
            None => return Ok(None),
        };

        if let Some(config) = extension_map().get(ext) {
            let src =
                fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
            if self.current_lang != config.language {
                self.parser
                    .set_language(&config.language)
                    .context("set language")?;
                self.current_lang = config.language.clone();
            }
            let tree = self
                .parser
                .parse(&src, None)
                .ok_or_else(|| anyhow::anyhow!("parse returned None"))?;
            Ok(Some((tree, src, config)))
        } else {
            Ok(None)
        }
    }

    pub fn build(&mut self) -> Result<RepoMap> {
        info!("Starting to build RepoMap for project at {:?}", self.root);
        let start_time = std::time::Instant::now();

        let mut map = RepoMap::default();
        let patterns: Vec<_> = language_configs()
            .iter()
            .flat_map(|c| c.extensions.iter().map(|ext| format!("**/*.{}", ext)))
            .collect();

        let walker = globwalk::GlobWalkerBuilder::from_patterns(&self.root, &patterns)
            .follow_links(false)
            .case_insensitive(true)
            .build()
            .context("build glob walker")?;

        let mut file_count = 0;
        let mut parsed_file_count = 0;
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
            file_count += 1;
            let p = entry.path().to_path_buf();
            match self.parse_file(&p) {
                Ok(Some((tree, src, config))) => {
                    (config.collector)(&mut map, &tree, &src, &p);
                    parsed_file_count += 1;
                }
                Ok(None) => {
                    // ファイルが対応していない拡張子の場合など
                    debug!("Skipped file (no parser): {}", p.display());
                }
                Err(e) => {
                    error!("Failed to parse file {}: {:?}", p.display(), e);
                }
            }
        }

        let duration = start_time.elapsed();
        info!(
            "Finished building RepoMap. Parsed {}/{} files in {:?}. Found {} symbols.",
            parsed_file_count,
            file_count,
            duration,
            map.symbols.len()
        );

        Ok(map)
    }
}
