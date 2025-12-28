use super::repomap_filter::filter_and_group_symbols;
use super::types::{SearchRepomapArgs, SearchRepomapResponse};
use crate::analysis::RepoMap;
use crate::analysis::semantic::SemanticService;
use anyhow::Result;
use std::collections::HashSet;
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
        mut args: SearchRepomapArgs,
        project_root: &Path,
    ) -> Result<SearchRepomapResponse> {
        let mut warnings = Vec::new();
        let semantic_query = args
            .semantic_query
            .as_ref()
            .map(|q| q.trim())
            .filter(|q| !q.is_empty())
            .map(|q| q.to_string());

        if let Some(query) = semantic_query {
            if let Some(service) = &self.semantic_service {
                let limit = args.limit.unwrap_or(20);
                let mut has_embeddings = match service.has_embeddings(project_root).await {
                    Ok(value) => value,
                    Err(e) => {
                        let did_fallback = extend_keyword_fallback(&mut args, &query);
                        add_semantic_warning(
                            &mut warnings,
                            &format!("embedding lookup failed: {}", e),
                            did_fallback,
                        );
                        false
                    }
                };

                if !has_embeddings && service.auto_update_enabled() {
                    if let Err(e) = service.update_embeddings(project_root).await {
                        let did_fallback = extend_keyword_fallback(&mut args, &query);
                        add_semantic_warning(
                            &mut warnings,
                            &format!("embedding update failed: {}", e),
                            did_fallback,
                        );
                    }
                    has_embeddings = match service.has_embeddings(project_root).await {
                        Ok(value) => value,
                        Err(e) => {
                            let did_fallback = extend_keyword_fallback(&mut args, &query);
                            add_semantic_warning(
                                &mut warnings,
                                &format!("embedding lookup failed: {}", e),
                                did_fallback,
                            );
                            false
                        }
                    };
                }

                if has_embeddings {
                    match service.search(&query, limit, project_root).await {
                        Ok(semantic_results) => {
                            let symbols: Vec<_> = semantic_results
                                .into_iter()
                                .filter_map(|(m, _score)| {
                                    crate::analysis::database::dao_conversions::active_model_to_symbol(m)
                                        .ok()
                                })
                                .collect();

                            let mut response = filter_and_group_symbols(&symbols, args);
                            response.warnings.extend(warnings);
                            return Ok(response);
                        }
                        Err(e) => {
                            let did_fallback = extend_keyword_fallback(&mut args, &query);
                            add_semantic_warning(
                                &mut warnings,
                                &format!("semantic search failed: {}", e),
                                did_fallback,
                            );
                        }
                    }
                } else {
                    let did_fallback = extend_keyword_fallback(&mut args, &query);
                    add_semantic_warning(&mut warnings, "no embeddings available", did_fallback);
                }
            } else {
                let did_fallback = extend_keyword_fallback(&mut args, &query);
                add_semantic_warning(&mut warnings, "semantic search disabled", did_fallback);
            }
        }

        // Avoid cloning the entire symbols vector; pass a reference-aware API
        let mut response = filter_and_group_symbols(&map.symbols, args);
        response.warnings.extend(warnings);
        Ok(response)
    }
}

fn extend_keyword_fallback(args: &mut SearchRepomapArgs, query: &str) -> bool {
    let mut keywords = crate::analysis::collector::extract_keywords_from_comment(query);
    if keywords.is_empty() {
        let trimmed = query.trim();
        if !trimmed.is_empty() {
            keywords.push(trimmed.to_string());
        }
    }

    if keywords.is_empty() {
        return false;
    }

    let mut search_terms = args.keyword_search.take().unwrap_or_default();
    let mut seen: HashSet<String> = search_terms.iter().cloned().collect();
    for term in keywords {
        if seen.insert(term.clone()) {
            search_terms.push(term);
        }
    }

    if search_terms.is_empty() {
        return false;
    }

    args.keyword_search = Some(search_terms);
    true
}

fn add_semantic_warning(warnings: &mut Vec<String>, reason: &str, did_fallback: bool) {
    if did_fallback {
        warnings.push(format!(
            "semantic_query unavailable ({}); falling back to keyword_search",
            reason
        ));
    } else {
        warnings.push(format!(
            "semantic_query unavailable ({}); no fallback keywords extracted",
            reason
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{RepoMap, SymbolInfo, SymbolKind};
    use std::path::PathBuf;

    fn make_symbol(name: &str, keywords: Vec<String>) -> SymbolInfo {
        SymbolInfo {
            name: name.to_string(),
            kind: SymbolKind::Function,
            file: PathBuf::from("src/lib.rs"),
            start_line: 1,
            start_col: 1,
            end_line: 10,
            end_col: 1,
            parent: None,
            file_total_lines: 100,
            function_lines: Some(10),
            keywords,
        }
    }

    #[tokio::test]
    async fn test_semantic_query_fallback_to_keyword_search() {
        let map = RepoMap {
            symbols: vec![
                make_symbol("pay", vec!["payment".to_string(), "retry".to_string()]),
                make_symbol("noop", vec!["other".to_string()]),
            ],
        };
        let tools = RepomapSearchTools::new(None);
        let args = SearchRepomapArgs {
            semantic_query: Some("payment retry".to_string()),
            ..Default::default()
        };

        let response = tools
            .search_repomap(&map, args, Path::new("."))
            .await
            .unwrap();

        assert!(
            response
                .warnings
                .iter()
                .any(|w| w.contains("semantic_query")),
            "expected semantic warning"
        );
        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].symbols[0].name, "pay");
    }
}
