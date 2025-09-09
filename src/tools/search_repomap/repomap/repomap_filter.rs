use std::collections::HashMap;
use std::path::PathBuf;

use super::{RepomapSearchResult, SearchRepomapArgs, SymbolSearchResult};
use crate::analysis::SymbolInfo;

pub(super) fn filter_and_group_symbols(
    symbols: Vec<SymbolInfo>,
    args: SearchRepomapArgs,
) -> Vec<RepomapSearchResult> {
    // Group symbols by file
    let mut file_groups: HashMap<PathBuf, Vec<SymbolInfo>> = HashMap::new();
    for symbol in symbols {
        file_groups
            .entry(symbol.file.clone())
            .or_default()
            .push(symbol);
    }

    let mut results = Vec::new();

    for (file_path, file_symbols) in file_groups {
        // Get file total lines from any symbol in the file (they should all have the same value)
        let file_total_lines = file_symbols
            .first()
            .map(|s| s.file_total_lines)
            .unwrap_or(0);

        // Apply file-level filters
        if let Some(min_lines) = args.min_file_lines
            && file_total_lines < min_lines
        {
            continue;
        }
        if let Some(max_lines) = args.max_file_lines
            && file_total_lines > max_lines
        {
            continue;
        }

        if let Some(pattern) = &args.file_pattern
            && !file_path.to_string_lossy().contains(pattern)
        {
            continue;
        }

        // Filter symbols within the file
        let mut filtered_symbols = Vec::new();
        for symbol in file_symbols {
            // Apply symbol kind filter
            if let Some(kinds) = &args.symbol_kinds
                && !kinds.contains(&symbol.kind.as_str().to_string())
            {
                continue;
            }

            // Apply function lines filter
            if let Some(min_func_lines) = args.min_function_lines {
                if let Some(func_lines) = symbol.function_lines {
                    if func_lines < min_func_lines {
                        continue;
                    }
                } else {
                    // For symbols without function_lines data, skip if we have a minimum requirement
                    continue;
                }
            }

            if let Some(max_func_lines) = args.max_function_lines {
                if let Some(func_lines) = symbol.function_lines {
                    if func_lines > max_func_lines {
                        continue;
                    }
                } else {
                    // For symbols without function_lines data, skip if we have a maximum requirement
                    continue;
                }
            }

            // Apply name search filter
            if let Some(name_searches) = &args.name {
                let mut found = false;
                let symbol_name_lower = symbol.name.to_lowercase();

                for name_search in name_searches {
                    if symbol_name_lower.contains(&name_search.to_lowercase()) {
                        found = true;
                        break;
                    }
                }

                if !found {
                    continue;
                }
            }

            // Apply keyword search filter
            if let Some(keyword_searches) = &args.keyword_search {
                let mut found = false;

                // Check if any of the symbol's keywords contain any of the search terms
                for keyword_search in keyword_searches {
                    let keyword_lower = keyword_search.to_lowercase();

                    for keyword in &symbol.keywords {
                        if keyword.to_lowercase().contains(&keyword_lower) {
                            found = true;
                            break;
                        }
                    }

                    if found {
                        break;
                    }
                }

                if !found {
                    continue;
                }
            }

            filtered_symbols.push(symbol);
        }

        // Apply symbol count filters
        let symbol_count = filtered_symbols.len();

        // Skip files with no matching symbols
        if symbol_count == 0 {
            continue;
        }

        if let Some(min_symbols) = args.min_symbols_per_file
            && symbol_count < min_symbols
        {
            continue;
        }
        if let Some(max_symbols) = args.max_symbols_per_file
            && symbol_count > max_symbols
        {
            continue;
        }

        // Convert to result format
        let symbol_results: Vec<SymbolSearchResult> = filtered_symbols
            .into_iter()
            .map(SymbolSearchResult::from)
            .collect();

        results.push(RepomapSearchResult {
            file: file_path,
            file_total_lines,
            symbols: symbol_results,
            symbol_count,
        });
    }

    // Sort results
    let sort_desc = args.sort_desc.unwrap_or(true);
    if let Some(sort_by) = &args.sort_by {
        match sort_by.as_str() {
            "file_lines" => {
                results.sort_by(|a, b| {
                    if sort_desc {
                        b.file_total_lines.cmp(&a.file_total_lines)
                    } else {
                        a.file_total_lines.cmp(&b.file_total_lines)
                    }
                });
            }
            "symbol_count" => {
                results.sort_by(|a, b| {
                    if sort_desc {
                        b.symbol_count.cmp(&a.symbol_count)
                    } else {
                        a.symbol_count.cmp(&b.symbol_count)
                    }
                });
            }
            "file_path" => {
                results.sort_by(|a, b| {
                    if sort_desc {
                        b.file.cmp(&a.file)
                    } else {
                        a.file.cmp(&b.file)
                    }
                });
            }
            "function_lines" => {
                // Sort by the largest function in each file
                results.sort_by(|a, b| {
                    let a_max_func = a
                        .symbols
                        .iter()
                        .filter_map(|s| s.function_lines)
                        .max()
                        .unwrap_or(0);
                    let b_max_func = b
                        .symbols
                        .iter()
                        .filter_map(|s| s.function_lines)
                        .max()
                        .unwrap_or(0);
                    if sort_desc {
                        b_max_func.cmp(&a_max_func)
                    } else {
                        a_max_func.cmp(&b_max_func)
                    }
                });
            }
            _ => {} // No sorting for unknown criteria
        }
    }

    // Apply limit
    let limit = args.limit.unwrap_or(20);
    results.truncate(limit);

    results
}
