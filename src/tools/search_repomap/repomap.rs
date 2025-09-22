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
- `max_file_lines` / `max_function_lines`:
  - Use these to filter for code that might be too complex or require refactoring.
- `ranking_strategy`:
  - Use this to specify how the file-level match score (`file_match_score`) is calculated.
  - Options: `max_score` (default), `avg_score`, `sum_score`, `hybrid`.
- `sort_by`:
  - Use this to sort the results. 
  - In addition to existing options (`file_lines`, `function_lines`, `symbol_count`, `file_path`), you can now sort by `file_match_score`.

**Return Value:**
The tool returns a list of `RepomapSearchResult` objects, each containing file and symbol information, including the symbol's name, kind, location, associated keywords, and the **code_snippet**.
The `code_snippet` allows you to understand the code immediately without a followup `fs_read` call.
Each result also includes a `file_match_score` which indicates the relevance of the file based on the symbols it contains and the chosen `ranking_strategy`.
"#;

/// Placeholder function to match the required function signature for search_repomap
/// The actual implementation is in the RepomapSearchTools struct
pub fn search_repomap(
    _args: SearchRepomapArgs,
    _config: &AppConfig,
) -> Result<Vec<RepomapSearchResult>> {
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
                    }
                },
                "additionalProperties": false
            }),
        },
    }
}
