use super::repomap_filter::filter_and_group_symbols;
use super::types::{SearchRepomapArgs, SearchRepomapResponse};
use crate::analysis::RepoMap;

use anyhow::Result;

use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct RepomapSearchTools {}

impl RepomapSearchTools {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn search_repomap(
        &self,
        map: &RepoMap,
        args: SearchRepomapArgs,
        _project_root: &Path,
    ) -> Result<SearchRepomapResponse> {
        let warnings = Vec::new();

        // Avoid cloning the entire symbols vector; pass a reference-aware API
        let mut response = filter_and_group_symbols(map, args);
        response.warnings.extend(warnings);
        Ok(response)
    }
}

#[cfg(test)]
mod tests {}
