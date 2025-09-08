use crate::llm::types::{ToolDef, ToolFunctionDef};
use serde_json::json;

mod repomap_filter;
mod types;
pub use types::*;
mod search_tools;
pub use search_tools::RepomapSearchTools;

const DESCRIPTION: &str = "
This is an advanced, structural code search tool for the entire repository.
Unlike simple text-matching tools like grep or ripgrep, it understands code structure (symbol names, comments) and metrics (file size, function size), allowing you to combine these criteria for precise searches.
This is the primary, first-choice tool that should be used to investigate the codebase or pinpoint locations for modification.
By passing feature names or keywords from a user's request to the `keyword_search` parameter, you can quickly and accurately discover relevant code and files.

Primary Use Cases:
- Identifying the location of code related to a specific feature.
- Finding refactoring candidates, such as large files or complex functions.
- Analyzing patterns of the codebase's overall structure and complexity.

Key Parameter:

- keyword_search:
 - Specifies the core keywords, feature names, or relevant terms for the search.
 - It searches against both symbol names (e.g., function/class names) and the comments associated with those symbols.
 - Set the most critical terms extracted from the user's instructions here.
- name:
 - Searches directly for a specific symbol by its name (e.g., function name, class name, variable name).
- max_file_lines:
 - Filters files based on the number of lines. Comparison operators can be used.
- max_function_lines
 - Filters for files containing functions that meet the specified line count criteria. Comparison operators can be used.";

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "search_repomap".to_string(),
            description: DESCRIPTION.to_owned(),
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
