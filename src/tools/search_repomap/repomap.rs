use crate::config::AppConfig;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::Result;
use serde_json::json;

mod repomap_filter;
mod types;
pub use types::*;
mod search_tools;
pub use search_tools::RepomapSearchTools;

#[cfg(test)]
mod tests;

const DESCRIPTION: &str = r#"
This is an advanced, structural code search tool that you MUST use as the first step for any code analysis or modification task.
It is your primary tool for understanding the codebase. Unlike simple text search, it understands code structure (symbols, comments) and helps you locate relevant code with surgical precision.

**Workflow:**
1.  **ALWAYS start with this tool.** Analyze the user's request to identify keywords (features, concepts, variable names).
2.  Use these keywords in the `keyword_search` or `name` parameter to find the most relevant code locations.
3.  Analyze the results to determine your next step.

**Primary Use Cases:**
- **Mandatory first step:** Finding the location of code related to any feature or bug.
- **Code analysis:** Finding refactoring candidates (e.g., large files, complex functions) or analyzing the codebase structure.

**Key Parameters:**

- `keyword_search`:
  - **Your primary search parameter.** Use this to find symbols and comments related to a feature or concept.
  - Extract keywords from the user's request and provide them as a list.
  - Example: For a request like "fix the login button", you would use `keyword_search: ["login", "button", "auth"]`.
- `name`:
  - Use this when you are looking for a specific, named symbol (function, class, etc.).
- `symbol_kinds`:
  - Use this to narrow your search to specific types of symbols.
  - Example: `symbol_kinds: ["Function", "Struct"]`
- `fields`:
  - Optional list of fields to search in. Supported values: `name`, `keyword`, `code`, `doc`.
  - If omitted, all fields are searched. Use `fields` to narrow scope and save tokens (e.g., `fields:["name","doc"]`).
- `exclude_patterns` / `file_pattern`:
  - `exclude_patterns` lets you drop paths matching substrings or simple glob-like tokens (e.g., `"tests/"`, `"generated"`).
  - Pair with `file_pattern` when you need both allow- and deny-lists for file paths.
- `language_filters`:
  - Restrict results to specific languages or extensions (`"rust"`, `"py"`, `"ts"`, `.tsx`). Mixed forms are accepted.
- `max_symbols_per_file`:
  - Caps how many symbols are returned for a single file. The most relevant matches are kept.
- `match_score_threshold`:
  - Require a minimum per-symbol `match_score` (0.0–1.0) to filter out weaker matches.
- `result_density`:
  - Choose between `compact` (default) and `full`. Compact mode disables snippets, caps symbols per file, and trims limits to preserve tokens.
- `response_budget_chars`:
  - Provide an approximate upper bound (e.g. 5000). The tool will automatically tighten `limit`, `max_symbols_per_file`, and snippet sizes to stay within budget when possible.
- `cursor` / `page_size`:
  - Use these to paginate through sorted results without pulling everything at once. Combine with `response_budget_chars` for predictable payload sizes.
- `max_file_lines` / `max_function_lines`:
  - Use these to filter for code that might be too complex or require refactoring.
- `ranking_strategy`:
  - Use this to specify how the file-level match score (`file_match_score`) is calculated.
  - Options: `max_score` (default), `avg_score`, `sum_score`, `hybrid`.
- `sort_by`:
  - Use this to sort the results. 
  - In addition to existing options (`file_lines`, `function_lines`, `symbol_count`, `file_path`), you can now sort by `file_match_score`.

**Return Value:**
The tool returns a `SearchRepomapResponse` structure. The `results` field contains the familiar list of `RepomapSearchResult` objects (with names, kinds, locations, keywords, and optional **code_snippet** data). The response may also include a `next_cursor` for pagination, `warnings` when budgets force aggressive trimming, and an `applied_budget` summary so you know which constraints were tightened automatically.
"#;

/// Placeholder function to match the required function signature for search_repomap
/// The actual implementation is in the RepomapSearchTools struct
pub fn search_repomap(
    _args: SearchRepomapArgs,
    _config: &AppConfig,
) -> Result<SearchRepomapResponse> {
    // This is a placeholder implementation to match the required function signature
    // The actual implementation is in the RepomapSearchTools struct
    todo!("This function is not meant to be called directly. Use FsTools::search_repomap instead.")
}

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
            }
                },
                "additionalProperties": false
            }),
        },
    }
}
