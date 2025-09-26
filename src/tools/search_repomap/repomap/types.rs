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
    /// File path substrings/glob-like tokens to exclude (applied before other filters)
    pub exclude_patterns: Option<Vec<String>>,
    /// Language or file-extension filters (e.g. "rust", "ts", "py", "md", "tsx")
    pub language_filters: Option<Vec<String>>,
    pub min_symbols_per_file: Option<usize>,
    pub max_symbols_per_file: Option<usize>,
    pub sort_by: Option<String>,
    pub sort_desc: Option<bool>,
    pub limit: Option<usize>,
    /// Minimum symbol match_score (0.0 - 1.0) required to include a symbol/result
    pub match_score_threshold: Option<f64>,
    /// Search for symbols containing specific keywords
    pub keyword_search: Option<Vec<String>>,
    /// Search for symbols containing symbol name
    pub name: Option<Vec<String>>,
    /// Fields to search in. If None, all supported fields are searched.
    /// Supported: "name", "keyword", "code", "doc"
    pub fields: Option<Vec<String>>,
    /// Whether to include code snippets in the result (default: true)
    pub include_snippets: Option<bool>,
    /// Number of context lines to include around matched symbol when snippets are returned
    pub context_lines: Option<usize>,
    /// Maximum characters for a snippet (truncate with "..." if exceeded)
    pub snippet_max_chars: Option<usize>,
    /// Strategy for calculating file-level match score ('max_score', 'avg_score', 'sum_score', 'hybrid')
    pub ranking_strategy: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepomapSearchResult {
    pub file: PathBuf,
    pub file_total_lines: usize,
    pub symbols: Vec<SymbolSearchResult>,
    pub symbol_count: usize,
    /// Aggregate match score for the file (max of symbol match scores), 0.0..1.0
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_match_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MatchSpan {
    /// Field that matched, e.g. "name", "keyword", "doc", "code", "kind"
    pub field: String,
    pub start_line: usize,
    pub end_line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_col: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_col: Option<usize>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub matched_text: String,
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
    /// Score in range 0.0..1.0 indicating how well this symbol matched the query
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_score: Option<f64>,
    /// Detailed match spans explaining why this symbol matched
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<MatchSpan>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub code_snippet: String,
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
            match_score: None,
            matches: Vec::new(),
            code_snippet: String::new(),
        }
    }
}

impl From<&SymbolInfo> for SymbolSearchResult {
    fn from(s: &SymbolInfo) -> Self {
        Self {
            name: s.name.clone(),
            kind: s.kind.as_str().to_string(),
            start_line: s.start_line,
            end_line: s.end_line,
            function_lines: s.function_lines,
            parent: s.parent.clone(),
            keywords: s.keywords.clone(),
            match_score: None,
            matches: Vec::new(),
            code_snippet: String::new(),
        }
    }
}
