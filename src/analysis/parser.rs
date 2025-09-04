use crate::analysis::RepoMap;
use crate::analysis::language_config::{LanguageConfig, extension_map};
use anyhow::{Context, Result};
use std::{fs, path::Path};
use tree_sitter::{Language, Parser, Tree};

#[derive(Debug)]
pub struct ParseTask {
    pub file_path: std::path::PathBuf,
}

pub fn parse_single_file(
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

pub fn process_single_file(
    file_path: std::path::PathBuf,
    parse_result: (Tree, String, &'static LanguageConfig),
) -> Result<RepoMap> {
    let (tree, src, config) = parse_result;
    let mut map = RepoMap::default();
    config
        .collector
        .extract_symbols(&mut map, &tree, &src, &file_path)?;
    Ok(map)
}
