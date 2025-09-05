use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::Result;
use serde::Serialize;
use serde_json::json;
use std::path::PathBuf;

use crate::analysis::{RepoMap, SymbolInfo};

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "get_symbol_info".to_string(),
            description: "Queries the repository's static analysis data for symbols (functions, structs, enums, traits, etc.) by name substring. You can optionally filter by file path (`include`) and symbol kind (e.g., 'fn', 'struct'). This is useful for understanding the codebase structure, locating definitions, or getting context about specific code elements. For example, use it to find where a specific function is defined, or to see all methods of a particular struct. The returned information includes the symbol's kind, name, file path, line number, and a relevant code snippet.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "include": {"type": "string"},
                    "kind": {"type": "string"}
                },
                "required": ["query"]
            }),
        },
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SymbolQueryResult {
    pub name: String,
    pub kind: String,
    pub file: PathBuf,
    pub start_line: usize,
    pub end_line: usize,
    pub parent: Option<String>,
}

impl From<SymbolInfo> for SymbolQueryResult {
    fn from(s: SymbolInfo) -> Self {
        Self {
            name: s.name,
            kind: s.kind.as_str().to_string(),
            file: s.file,
            start_line: s.start_line,
            end_line: s.end_line,
            parent: s.parent,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SymbolTools;

impl Default for SymbolTools {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolTools {
    pub fn new() -> Self {
        Self
    }

    // This function is hard to test directly without a complex setup.
    // We will test the filtering logic in a separate, testable function.
    pub fn get_symbol_info(
        &self,
        map: &RepoMap,
        query: &str,
        include: Option<&str>,
        kind: Option<&str>,
    ) -> Result<Vec<SymbolQueryResult>> {
        let results = Self::filter_symbols(map.symbols.clone(), query, include, kind);
        Ok(results)
    }

    fn filter_symbols(
        symbols: Vec<SymbolInfo>,
        query: &str,
        include: Option<&str>,
        kind: Option<&str>,
    ) -> Vec<SymbolQueryResult> {
        let mut out = Vec::new();
        for s in symbols {
            if let Some(glob_like) = include
                && !s.file.to_string_lossy().contains(glob_like)
            {
                continue;
            }
            if !s.name.contains(query) {
                continue;
            }
            if let Some(k) = kind
                && s.kind.as_str() != k
            {
                continue;
            }
            out.push(SymbolQueryResult::from(s));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::SymbolKind;
    use std::path::PathBuf;

    fn create_dummy_symbol(name: &str, kind: SymbolKind, file: &str) -> SymbolInfo {
        SymbolInfo {
            name: name.to_string(),
            kind,
            file: PathBuf::from(file),
            start_line: 1,
            start_col: 1,
            end_line: 10,
            end_col: 10,
            parent: None,
            file_total_lines: 100,    // Dummy total lines in file
            function_lines: Some(10), // Dummy lines in function
            keywords: vec![],
        }
    }

    #[test]
    fn test_filter_symbols_by_name() {
        let symbols = vec![
            create_dummy_symbol("my_function", SymbolKind::Function, "src/main.rs"),
            create_dummy_symbol("another_function", SymbolKind::Function, "src/lib.rs"),
        ];
        let results = SymbolTools::filter_symbols(symbols, "my_function", None, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "my_function");
    }

    #[test]
    fn test_filter_symbols_by_kind() {
        let symbols = vec![
            create_dummy_symbol("my_function", SymbolKind::Function, "src/main.rs"),
            create_dummy_symbol("my_struct", SymbolKind::Struct, "src/main.rs"),
        ];
        let results = SymbolTools::filter_symbols(symbols, "my", None, Some("fn"));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, "fn");
    }

    #[test]
    fn test_filter_symbols_by_include() {
        let symbols = vec![
            create_dummy_symbol("main_func", SymbolKind::Function, "src/main.rs"),
            create_dummy_symbol("lib_func", SymbolKind::Function, "src/lib.rs"),
        ];
        let results = SymbolTools::filter_symbols(symbols, "func", Some("lib.rs"), None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "lib_func");
    }
}
