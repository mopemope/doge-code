use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;

use crate::analysis::{RepoMap, SymbolInfo};

mod repomap_filter;
use repomap_filter::filter_and_group_symbols;

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "search_repomap".to_string(),
            description: "Advanced search functionality for the repository map. Allows filtering by file size, function size, symbol counts, and other metrics. Useful for finding large files (>500 lines), large functions (>100 lines), files with many symbols, or analyzing code complexity patterns. You can combine multiple filters to find specific patterns in the codebase. Search for specific symbols by name or filter by keywords, feature names, and other relevant terms in symbol comments.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "max_file_lines": {
                        "type": "integer", 
                        "description": "Maximum number of lines in the file"
                    },
                    "max_function_lines": {
                        "type": "integer",
                        "description": "Maximum number of lines in functions"
                    },
                    "file_pattern": {
                        "type": "string",
                        "description": "File path pattern to match (substring match)"
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
                    },
                    "keyword_search": {
                        "type": "string",
                        "description": "Search for symbols containing specific keywords, feature names, and other relevant terms in their associated comments"
                    },
                    "name": {
                        "type": "string",
                        "description": "Search for symbols containing symbol name"
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
    /// Search for symbols containing specific keywords
    pub keyword_search: Option<String>,
    /// Search for symbols containing symbol name
    pub name: Option<String>,
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
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
            keywords: s.keywords,
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
        let results = filter_and_group_symbols(map.symbols.clone(), args);
        Ok(results)
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
            keywords: vec![],
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

        let results = filter_and_group_symbols(symbols, args);
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

        let results = filter_and_group_symbols(symbols, args);
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

        let results = filter_and_group_symbols(symbols, args);
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

        let results = filter_and_group_symbols(symbols, args);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].file_total_lines, 500); // largest first
        assert_eq!(results[1].file_total_lines, 300);
        assert_eq!(results[2].file_total_lines, 100);
    }

    #[test]
    fn test_keyword_search() {
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

        let results = filter_and_group_symbols(symbols, args);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].symbols.len(), 1);
        assert_eq!(results[0].symbols[0].name, "test_function");
    }

    #[test]
    fn test_name_search() {
        let symbols = vec![
            create_test_symbol_with_keywords(
                "calculate_total",
                SymbolKind::Function,
                "math.rs",
                100,
                Some(10),
                vec!["math".to_string(), "calculation".to_string()],
            ),
            create_test_symbol_with_keywords(
                "format_string",
                SymbolKind::Function,
                "string.rs",
                100,
                Some(15),
                vec!["string".to_string(), "format".to_string()],
            ),
        ];

        let args = SearchRepomapArgs {
            name: Some("calculate".to_string()),
            ..Default::default()
        };

        let results = filter_and_group_symbols(symbols, args);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].symbols.len(), 1);
        assert_eq!(results[0].symbols[0].name, "calculate_total");
    }

    // Helper function to create a test symbol with keywords
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
}

#[cfg(test)]
mod tests_dup {
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
            keywords: vec![],
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

        let results = filter_and_group_symbols(symbols, args);
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

        let results = filter_and_group_symbols(symbols, args);
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

        let results = filter_and_group_symbols(symbols, args);
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

        let results = filter_and_group_symbols(symbols, args);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].file_total_lines, 500); // largest first
        assert_eq!(results[1].file_total_lines, 300);
        assert_eq!(results[2].file_total_lines, 100);
    }

    #[test]
    fn test_keyword_search() {
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

        let results = filter_and_group_symbols(symbols, args);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].symbols.len(), 1);
        assert_eq!(results[0].symbols[0].name, "test_function");
    }

    #[test]
    fn test_name_search() {
        let symbols = vec![
            create_test_symbol_with_keywords(
                "calculate_total",
                SymbolKind::Function,
                "math.rs",
                100,
                Some(10),
                vec!["math".to_string(), "calculation".to_string()],
            ),
            create_test_symbol_with_keywords(
                "format_string",
                SymbolKind::Function,
                "string.rs",
                100,
                Some(15),
                vec!["string".to_string(), "format".to_string()],
            ),
        ];

        let args = SearchRepomapArgs {
            name: Some("calculate".to_string()),
            ..Default::default()
        };

        let results = filter_and_group_symbols(symbols, args);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].symbols.len(), 1);
        assert_eq!(results[0].symbols[0].name, "calculate_total");
    }

    // Helper function to create a test symbol with keywords
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
}
