use super::repomap_filter::filter_and_group_symbols;
use super::types::{SearchRepomapArgs, SearchRepomapResponse};
use crate::analysis::RepoMap;
use crate::analysis::semantic::SemanticService;
use anyhow::Result;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct RepomapSearchTools {
    semantic_service: Option<SemanticService>,
}

impl Default for RepomapSearchTools {
    fn default() -> Self {
        Self::new(None)
    }
}

impl RepomapSearchTools {
    pub fn new(semantic_service: Option<SemanticService>) -> Self {
        Self { semantic_service }
    }

    pub async fn search_repomap(
        &self,
        map: &RepoMap,
        args: SearchRepomapArgs,
        project_root: &Path,
    ) -> Result<SearchRepomapResponse> {
        if let (Some(query), Some(service)) = (&args.semantic_query, &self.semantic_service) {
            let limit = args.limit.unwrap_or(20);
            let semantic_results = service.search(query, limit, project_root).await?;

            let symbols: Vec<_> = semantic_results
                .into_iter()
                .filter_map(|(m, _score)| {
                    // Convert DB model to Analysis SymbolInfo
                    // We need to construct it manually if dao_conversions is not easily accessible
                    // But we can try using the conversion module.
                    crate::analysis::database::dao_conversions::active_model_to_symbol(m).ok()
                })
                .collect();

            // Pass the semantically retrieved symbols to the filter logic
            // to apply any additional filters (kinds, patterns, etc.)
            let results = filter_and_group_symbols(&symbols, args);
            return Ok(results);
        }

        // Avoid cloning the entire symbols vector; pass a reference-aware API
        let results = filter_and_group_symbols(&map.symbols, args);
        Ok(results)
    }
}
