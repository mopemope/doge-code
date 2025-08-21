use anyhow::Result;
use std::path::PathBuf;
use crate::analysis::{RepoMap, SymbolInfo};
use crate::tools::repomap::types::{RepomapSearchResult, SearchRepomapArgs, SymbolSearchResult};
use crate::tools::repomap::filters::{group_by_file, filter_symbols_for_file};

pub struct RepomapSearchTools;

impl RepomapSearchTools {
    pub fn new() -> Self { Self }

    pub fn search_repomap(&self, map: &RepoMap, args: SearchRepomapArgs) -> Result<Vec<RepomapSearchResult>> {
        let results = Self::filter_and_group_symbols(map.symbols.clone(), args);
        Ok(results)
    }

    fn filter_and_group_symbols(symbols: Vec<SymbolInfo>, args: SearchRepomapArgs) -> Vec<RepomapSearchResult> {
        use std::collections::HashMap;

        let file_groups = group_by_file(symbols);
        let mut results = Vec::new();

        for (file_path, file_symbols) in file_groups {
            let file_total_lines = file_symbols
                .first()
                .map(|s| s.file_total_lines)
                .unwrap_or(0);

            if let Some(min_lines) = args.min_file_lines && file_total_lines < min_lines { continue; }
            if let Some(max_lines) = args.max_file_lines && file_total_lines > max_lines { continue; }
            if let Some(pattern) = &args.file_pattern && !file_path.to_string_lossy().contains(pattern) { continue; }

            let filtered_symbols = filter_symbols_for_file(file_symbols, &args);
            let symbol_count = filtered_symbols.len();
            if symbol_count == 0 { continue; }
            if let Some(min_symbols) = args.min_symbols_per_file && symbol_count < min_symbols { continue; }
            if let Some(max_symbols) = args.max_symbols_per_file && symbol_count > max_symbols { continue; }

            let symbol_results: Vec<SymbolSearchResult> = filtered_symbols.into_iter().map(SymbolSearchResult::from).collect();

            results.push(RepomapSearchResult {
                file: file_path,
                file_total_lines,
                symbols: symbol_results,
                symbol_count,
            });
        }

        // Sorting
        let sort_desc = args.sort_desc.unwrap_or(true);
        if let Some(sort_by) = &args.sort_by {
            match sort_by.as_str() {
                "file_lines" => {
                    results.sort_by(|a, b| if sort_desc { b.file_total_lines.cmp(&a.file_total_lines) } else { a.file_total_lines.cmp(&b.file_total_lines) });
                }
                "symbol_count" => {
                    results.sort_by(|a, b| if sort_desc { b.symbol_count.cmp(&a.symbol_count) } else { a.symbol_count.cmp(&b.symbol_count) });
                }
                "file_path" => {
                    results.sort_by(|a, b| if sort_desc { b.file.cmp(&a.file) } else { a.file.cmp(&b.file) });
                }
                "function_lines" => {
                    results.sort_by(|a, b| {
                        let a_max = a.symbols.iter().filter_map(|s| s.function_lines).max().unwrap_or(0);
                        let b_max = b.symbols.iter().filter_map(|s| s.function_lines).max().unwrap_or(0);
                        if sort_desc { b_max.cmp(&a_max) } else { a_max.cmp(&b_max) }
                    });
                }
                _ => {}
            }
        }

        results.truncate(args.limit.unwrap_or(50));
        results
    }
}
