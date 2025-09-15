use crate::llm::types::{ToolDef, ToolFunctionDef};
use serde_json::json;

mod repomap_filter;
mod types;
pub use types::*;
mod search_tools;
pub use search_tools::RepomapSearchTools;

#[cfg(test)]
mod tests;

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
 - Filters for files containing functions that meet the specified line count criteria. Comparison operators can be used.

Return Value:
The tool returns a list of `RepomapSearchResult` objects, each representing a file that matches the search criteria. Each object has the following structure:
- `file`: The absolute path to the file.
- `file_total_lines`: The total number of lines in the file.
- `symbol_count`: The number of symbols found in the file that match the criteria.
- `symbols`: A list of `SymbolSearchResult` objects, each containing details about a matched symbol:
  - `name`: The name of the symbol (e.g., function name, class name).
  - `kind`: The type of the symbol (e.g., 'Function', 'Class', 'Method').
  - `start_line`: The starting line number of the symbol definition.
  - `end_line`: The ending line number of the symbol definition.
  - `function_lines`: The number of lines in the function, if applicable.
  - `parent`: The name of the parent symbol, if any.
  - `keywords`: A list of keywords extracted from the comments associated with the symbol.";

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
                    "sort_by": {
                        "type": ["string", "null"],
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
                        "type": ["array", "null"],
                        "items": {"type": "string"},
                        "description": "A list of search for symbols containing specific keywords in their associated comments"
                    },
                    "name": {
                        "type": ["array", "null"],
                        "items": {"type": "string"},
                        "description": "A list of search for symbols containing symbol name"
                    }
                },
                "additionalProperties": false
            }),
        },
    }
}
