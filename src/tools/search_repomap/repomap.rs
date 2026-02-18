use crate::llm::types::{ToolDef, ToolFunctionDef};
use serde_json::json;

mod repomap_filter;
mod types;
pub use types::*;
mod search_tools;
pub use search_tools::RepomapSearchTools;

#[cfg(test)]
mod tests;

const DESCRIPTION: &str = r#"Advanced structural code search. Use `semantic_query` for natural language questions (e.g. 'how does auth work?'), `name` for specific symbol names, or `keyword_search` for exact string matches. Features: symbol-aware, relationship graph support."#;

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "search_repomap".to_string(),
            description: DESCRIPTION.to_owned(),
            strict: None,
            parameters: json!({
                "type": "object",
                "properties": {
            "result_density": {
                "type": ["string", "null"],
                "enum": ["compact", "full"],
                "description": "Controls verbosity. 'compact' disables snippets and caps per-file matches while 'full' preserves legacy output"
            },
            "max_file_lines": {
                "type": ["integer", "null"],
                "description": "Maximum number of lines in the file"
            },
                    "max_function_lines": {
                        "type": ["integer","null"],
                        "description": "Maximum number of lines in functions"
                    },
                    "file_pattern": {
                        "type": ["string", "null"],
                        "description": "File path pattern to match (substring match)"
                    },
                    "exclude_patterns": {
                        "type": ["array", "null"],
                        "items": {"type": "string"},
                        "description": "Paths to exclude (substring or glob-like tokens such as 'tests/' or 'generated')"
                    },
                    "language_filters": {
                        "type": ["array", "null"],
                        "items": {"type": "string"},
                        "description": "Filter by language or file extension (e.g. 'rust', 'py', 'ts', '.rs', '.tsx')"
                    },
                    "symbol_kinds": {
                        "type": ["array", "null"],
                        "items": {"type": "string"},
                        "description": "Filter results by symbol kind (e.g., 'Function', 'Struct', 'Trait')."
                    },
                    "sort_by": {
                        "type": ["string", "null"],
                        "enum": ["file_lines", "function_lines", "symbol_count", "file_path", "file_match_score"],
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
            "response_budget_chars": {
                "type": ["integer", "null"],
                "description": "Approximate upper bound (in characters) for the response; the tool will downscale limits/snippets when exceeded"
            },
            "keyword_search": {
                "type": ["array", "null"],
                "items": {"type": "string"},
                "description": "A list of search for symbols containing specific keywords in their associated comments"
            },
            "semantic_query": {
                "type": ["string", "null"],
                "description": "Natural language query to search for code by meaning (e.g. 'how is authentication handled?')"
            },
                    "name": {
                        "type": ["array", "null"],
                        "items": {"type": "string"},
                        "description": "A list of search for symbols containing symbol name"
                    },
                    "fields": {
                        "type": ["array","null"],
                        "items": {"type":"string"},
                        "description": "Fields to search in (name, keyword, code, doc). If omitted, all fields are searched."
                    },
                    "include_snippets": {
                        "type": ["boolean","null"],
                        "description": "Whether to include code snippets in the result (default: true)"
                    },
                    "context_lines": {
                        "type": ["integer","null"],
                        "description": "Number of context lines to include around matched symbol when snippets are returned"
                    },
                    "snippet_max_chars": {
                        "type": ["integer","null"],
                        "description": "Maximum characters for a snippet (truncate with '...' if exceeded)"
                    },
            "ranking_strategy": {
                "type": ["string", "null"],
                "enum": ["max_score", "avg_score", "sum_score", "hybrid"],
                "description": "Strategy for calculating file-level match score (default: max_score)"
            },
            "match_score_threshold": {
                "type": ["number", "null"],
                "description": "Minimum match_score (0.0-1.0) a symbol must meet to be returned"
            },
            "cursor": {
                "type": ["integer", "null"],
                "description": "Zero-based cursor for paging through sorted results"
            },
            "page_size": {
                "type": ["integer", "null"],
                "description": "Number of results to return from the cursor position (defaults to limit when unset)"
            },
            "include_relations": {
                "type": ["boolean", "null"],
                "description": "Whether to include related symbols (callers/callees) in the result (default: false)"
            }
                },
                "additionalProperties": false
            }),
        },
    }
}
