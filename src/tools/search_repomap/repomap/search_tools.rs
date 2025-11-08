use super::repomap_filter::filter_and_group_symbols;
use super::types::{SearchRepomapArgs, SearchRepomapResponse};
use crate::analysis::RepoMap;
use anyhow::Result;

#[derive(Debug, Clone)]
pub struct RepomapSearchTools;

impl Default for RepomapSearchTools {
    fn default() -> Self {
        Self::new()
    }
}

impl RepomapSearchTools {
    pub fn new() -> Self {
        Self
    }

    pub fn search_repomap(
        &self,
        map: &RepoMap,
        args: SearchRepomapArgs,
    ) -> Result<SearchRepomapResponse> {
        // Avoid cloning the entire symbols vector; pass a reference-aware API
        let results = filter_and_group_symbols(&map.symbols, args);
        Ok(results)
    }
}
