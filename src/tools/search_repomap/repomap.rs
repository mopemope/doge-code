use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;

use crate::analysis::{RepoMap, SymbolInfo};

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "search_repomap".to_string(),
            description: "Advanced search functionality for the repository map. Allows filtering by file size, function size, symbol counts, and other metrics. Useful for finding large files (>500 lines), large functions (>100 lines), files with many symbols, or analyzing code complexity patterns. You can combine multiple filters to find specific patterns in the codebase.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "min_file_lines": {
                        "type": "integer",
                        "description": "Minimum number of lines in the file (e.g., 500 for large files)"
                    },
                    "max_file_lines": {
                        "type": "integer", 
                        "description": "Maximum number of lines in the file"
                    },
                    "min_function_lines": {
                        "type": "integer",
                        "description": "Minimum number of lines in functions (e.g., 100 for large functions)"
                    },
                    "max_function_lines": {
                        "type": "integer",
                        "description": "Maximum number of lines in functions"
                    },
                    "symbol_kinds": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Filter by symbol kinds: 'fn', 'struct', 'enum', 'trait', 'impl', 'method', 'assoc_fn', 'mod', 'var'"
                    },
                    "file_pattern": {
                        "type": "string",
                        "description": "File path pattern to match (substring match)"
                    },
                    "min_symbols_per_file": {
                        "type": "integer",
                        "description": "Minimum number of symbols per file"
                    },
                    "max_symbols_per_file": {
                        "type": "integer",
                        "description": "Maximum number of symbols per file"
                    },
                    "sort_by": {
                        "type": "string",
                        "enum": ["file_lines", "function_lines", "symbol_count", "file_path"],
                        "description": "Sort results by specified criteria"
                    },
                    "sort_desc": {
                        "type": "boolean",
                        "description": "Sort in descending order (default: true)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default: 50)"
                    }
                },
                "required": []
            }),
        },
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SearchRepomapArgs {
    pub min_file_lines: Option<usize>,
    pub max_file_lines: Option<usize>,
    pub min_function_lines: Option<usize>,
    pub max_function_lines: Option<usize>,
    pub symbol_kinds: Option<Vec<String>>,
    pub file_pattern: Option<String>,
    pub min_symbols_per_file: Option<usize>,
    pub max_symbols_per_file: Option<usize>,
    pub sort_by: Option<String>,
    pub sort_desc: Option<bool>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepomapSearchResult {
    pub file: PathBuf,
    pub file_total_lines: usize,
    pub symbols: Vec<SymbolSearchResult>,
    pub symbol_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SymbolSearchResult {
    pub name: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
    pub function_lines: Option<usize>,
    pub parent: Option<String>,
}

impl From<SymbolInfo> for SymbolSearchResult {
    fn from(s: SymbolInfo) -> Self {
        Self {
            name: s.name,
            kind: s.kind.as_str().to_string(),
            start_line: s.start_line,
            end_line: s.end_line,
            function_lines: s.function_lines,
            parent: s.parent,
        }
    }
}

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
    ) -> Result<Vec<RepomapSearchResult>> {
        let results = Self::filter_and_group_symbols(map.symbols.clone(), args);
        Ok(results)
    }

    fn filter_and_group_symbols(
        symbols: Vec<SymbolInfo>,
        args: SearchRepomapArgs,
    ) -> Vec<RepomapSearchResult> {
        use std::collections::HashMap;

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
        let limit = args.limit.unwrap_or(50);
        results.truncate(limit);

        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::SymbolKind;
    use std::path::PathBuf;

    fn create_test_symbol(
        name: &str,
        kind: SymbolKind,
        file: &str,
        file_total_lines: usize,
        function_lines: Option<usize>,
    ) -> SymbolInfo {
        SymbolInfo {
            name: name.to_string(),
            kind,
            file: PathBuf::from(file),
            start_line: 1,
            start_col: 1,
            end_line: function_lines.map(|l| l + 1).unwrap_or(10),
            end_col: 10,
            parent: None,
            file_total_lines,
            function_lines,
        }
    }

    #[test]
    fn test_filter_by_file_lines() {
        let symbols = vec![
            create_test_symbol("func1", SymbolKind::Function, "small.rs", 50, Some(10)),
            create_test_symbol("func2", SymbolKind::Function, "large.rs", 600, Some(20)),
        ];

        let args = SearchRepomapArgs {
            min_file_lines: Some(500),
            ..Default::default()
        };

        let results = RepomapSearchTools::filter_and_group_symbols(symbols, args);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file, PathBuf::from("large.rs"));
        assert_eq!(results[0].file_total_lines, 600);
    }

    #[test]
    fn test_filter_by_function_lines() {
        let symbols = vec![
            create_test_symbol("small_func", SymbolKind::Function, "test.rs", 200, Some(50)),
            create_test_symbol(
                "large_func",
                SymbolKind::Function,
                "test.rs",
                200,
                Some(150),
            ),
        ];

        let args = SearchRepomapArgs {
            min_function_lines: Some(100),
            ..Default::default()
        };

        let results = RepomapSearchTools::filter_and_group_symbols(symbols, args);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].symbols.len(), 1);
        assert_eq!(results[0].symbols[0].name, "large_func");
    }

    #[test]
    fn test_filter_by_symbol_kind() {
        let symbols = vec![
            create_test_symbol("my_func", SymbolKind::Function, "test.rs", 100, Some(10)),
            create_test_symbol("MyStruct", SymbolKind::Struct, "test.rs", 100, None),
        ];

        let args = SearchRepomapArgs {
            symbol_kinds: Some(vec!["fn".to_string()]),
            ..Default::default()
        };

        let results = RepomapSearchTools::filter_and_group_symbols(symbols, args);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].symbols.len(), 1);
        assert_eq!(results[0].symbols[0].kind, "fn");
    }

    #[test]
    fn test_sort_by_file_lines() {
        let symbols = vec![
            create_test_symbol("func1", SymbolKind::Function, "small.rs", 100, Some(10)),
            create_test_symbol("func2", SymbolKind::Function, "large.rs", 500, Some(20)),
            create_test_symbol("func3", SymbolKind::Function, "medium.rs", 300, Some(15)),
        ];

        let args = SearchRepomapArgs {
            sort_by: Some("file_lines".to_string()),
            sort_desc: Some(true),
            ..Default::default()
        };

        let results = RepomapSearchTools::filter_and_group_symbols(symbols, args);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].file_total_lines, 500); // largest first
        assert_eq!(results[1].file_total_lines, 300);
        assert_eq!(results[2].file_total_lines, 100);
    }

    #[test]
    fn test_limit_results() {
        let symbols = vec![
            create_test_symbol("func1", SymbolKind::Function, "file1.rs", 100, Some(10)),
            create_test_symbol("func2", SymbolKind::Function, "file2.rs", 200, Some(20)),
            create_test_symbol("func3", SymbolKind::Function, "file3.rs", 300, Some(30)),
        ];

        let args = SearchRepomapArgs {
            limit: Some(2),
            ..Default::default()
        };

        let results = RepomapSearchTools::filter_and_group_symbols(symbols, args);
        assert_eq!(results.len(), 2);
    }
}
