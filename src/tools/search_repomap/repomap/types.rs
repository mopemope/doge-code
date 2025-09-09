use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::analysis::SymbolInfo;

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
    pub keyword_search: Option<Vec<String>>,
    /// Search for symbols containing symbol name
    pub name: Option<Vec<String>>,
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
