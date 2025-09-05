use crate::analysis::SymbolInfo;
use crate::tools::repomap::types::SearchRepomapArgs;
use crate::analysis::symbol::SymbolKind;
use std::path::PathBuf;

pub fn group_by_file(symbols: Vec<SymbolInfo>) -> std::collections::HashMap<std::path::PathBuf, Vec<SymbolInfo>> {
    let mut file_groups = std::collections::HashMap::new();
    for symbol in symbols {
        file_groups
            .entry(symbol.file.clone())
            .or_default()
            .push(symbol);
    }
    file_groups
}

pub fn filter_symbols_for_file(mut file_symbols: Vec<SymbolInfo>, args: &SearchRepomapArgs) -> Vec<SymbolInfo> {
    let mut filtered = Vec::new();
    for symbol in file_symbols.drain(..) {
        if let Some(kinds) = &args.symbol_kinds {
            if !kinds.contains(&symbol.kind.as_str().to_string()) {
                continue;
            }
        }

        if let Some(min_func_lines) = args.min_function_lines {
            if let Some(func_lines) = symbol.function_lines {
                if func_lines < min_func_lines {
                    continue;
                }
            } else {
                continue;
            }
        }

        if let Some(max_func_lines) = args.max_function_lines {
            if let Some(func_lines) = symbol.function_lines {
                if func_lines > max_func_lines {
                    continue;
                }
            } else {
                continue;
            }
        }

        // Apply keyword search filter
        if let Some(keyword_search) = &args.keyword_search {
            let keyword_lower = keyword_search.to_lowercase();
            let mut found = false;
            
            // Check if the symbol name contains the keyword
            if symbol.name.to_lowercase().contains(&keyword_lower) {
                found = true;
            }
            
            // Check if any of the symbol's keywords contain the search term
            if !found {
                for keyword in &symbol.keywords {
                    if keyword.to_lowercase().contains(&keyword_lower) {
                        found = true;
                        break;
                    }
                }
            }
            
            if !found {
                continue;
            }
        }

        filtered.push(symbol);
    }
    filtered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::symbol::SymbolKind;
    use std::path::PathBuf;

    fn create_test_symbol_with_keywords(
        name: &str,
        kind: SymbolKind,
        file: &str,
        file_total_lines: usize,
        function_lines: Option<usize>,
        keywords: Vec<String>,
    ) -> SymbolInfo {
        SymbolInfo {
            name: name.to_string(),
            kind,
            file: PathBuf::from(file),
            start_line: 1,
            start_col: 0,
            end_line: function_lines.map(|l| l + 1).unwrap_or(10),
            end_col: 1,
            parent: None,
            file_total_lines,
            function_lines,
            keywords,
        }
    }

    #[test]
    fn test_filter_with_keyword_search() {
        let symbols = vec![
            create_test_symbol_with_keywords(
                "test_function",
                SymbolKind::Function,
                "test.rs",
                100,
                Some(10),
                vec!["testing".to_string(), "functionality".to_string()],
            ),
            create_test_symbol_with_keywords(
                "other_function",
                SymbolKind::Function,
                "test.rs",
                100,
                Some(15),
                vec!["other".to_string(), "utility".to_string()],
            ),
        ];

        let args = SearchRepomapArgs {
            keyword_search: Some("testing".to_string()),
            ..Default::default()
        };

        let filtered = filter_symbols_for_file(symbols, &args);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "test_function");
    }
}
