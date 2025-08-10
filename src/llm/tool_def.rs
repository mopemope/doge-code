use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value, // JSON Schema object
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub kind: String, // "function"
    pub function: ToolFunctionDef,
}

pub fn default_tools_def() -> Vec<ToolDef> {
    vec![
        ToolDef {
            kind: "function".into(),
            function: ToolFunctionDef {
                name: "fs_list".into(),
                description: "Lists files and directories within a specified path. You can limit the depth of recursion and filter results by a glob pattern. The default maximum depth is 1. This tool is useful for exploring the project structure, finding specific files, or getting an overview of the codebase before starting a task. For example, use it to see what files are in a directory or to find all `.rs` files.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "max_depth": {"type": "integer"},
                        "pattern": {"type": "string"}
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDef {
            kind: "function".into(),
            function: ToolFunctionDef {
                name: "fs_read".into(),
                description: "Reads the content of a text file from the absolute path. You can specify a starting line offset and a maximum number of lines to read. This is useful for inspecting file contents, reading specific sections of large files, or understanding the implementation details of a function or class. Do not use this for binary files or extremely large files.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "offset": {"type": "integer"},
                        "limit": {"type": "integer"}
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDef {
            kind: "function".into(),
            function: ToolFunctionDef {
                name: "search_text".into(),
                description: "Searches for a regular expression `search_pattern` within the content of files matching the `file_glob` pattern. It returns matching lines along with their file paths and line numbers. This tool is specifically for searching within file contents, not file names. For example, use it to locate all usages of a specific API, trace the origin of an error message, or find where a particular variable name is used.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "search_pattern": {
                            "type": "string",
                            "description": "The regular expression to search for within file contents."
                        },
                        "file_glob": {
                            "type": "string",
                            "description": "A glob pattern to filter which files are searched (e.g., 'src/**/*.rs', '*.toml'). Defaults to all files if not provided."
                        }
                    },
                    "required": ["search_pattern"]
                }),
            },
        },
        ToolDef {
            kind: "function".into(),
            function: ToolFunctionDef {
                name: "fs_write".into(),
                description: "Writes or overwrites text content to a specified file from the absolute path. It automatically creates parent directories if they don't exist. Use this tool for creating new files from scratch (e.g., a new module, test file, or configuration file) or for completely replacing the content of an existing file (e.g., resetting a config file to its default state, updating a generated code file). For partial modifications to existing files, `replace_text_block` or `apply_patch` are generally safer and recommended.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "content": {"type": "string"}
                    },
                    "required": ["path", "content"]
                }),
            },
        },
        ToolDef {
            kind: "function".into(),
            function: ToolFunctionDef {
                name: "get_symbol_info".into(),
                description: "Queries the repository's static analysis data for symbols (functions, structs, enums, traits, etc.) by name substring. You can optionally filter by file path (`include`) and symbol kind (e.g., 'fn', 'struct'). This is useful for understanding the codebase structure, locating definitions, or getting context about specific code elements. For example, use it to find where a specific function is defined, or to see all methods of a particular struct. The returned information includes the symbol's kind, name, file path, line number, and a relevant code snippet.".into(),
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
        },
        ToolDef {
            kind: "function".into(),
            function: ToolFunctionDef {
                name: "execute_bash".into(),
                description: "Executes an arbitrary bash command within the project root directory. It captures and returns both standard output (stdout) and standard error (stderr). Use this for tasks that require shell interaction, such as running build commands (`cargo build`), tests (`cargo test`), or external utilities (`git status`). Be cautious with commands that modify the file system (e.g., `rm`, `mv`) and consider their impact beforehand. Interactive commands are not supported.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string"}
                    },
                    "required": ["command"]
                }),
            },
        },
        ToolDef {
            kind: "function".into(),
            function: ToolFunctionDef {
                name: "get_file_sha256".into(),
                description: "Calculates the SHA256 hash of a file. This is useful for verifying file integrity or for providing the `file_hash_sha256` parameter to other tools like `apply_patch` or `replace_text_block` for safe file modifications.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "file_path": {"type": "string", "description": "The absolute path to the file."}
                    },
                    "required": ["file_path"]
                }),
            },
        },
        ToolDef {
            kind: "function".into(),
            function: ToolFunctionDef {
                name: "replace_text_block".into(),
                description: "Replaces a single, unique block of text within a file with a new block of text. It ensures file integrity by verifying the SHA256 hash of the file content, preventing accidental overwrites if the file has changed since it was last read. Use this for simple, targeted modifications like fixing a bug in a specific line, changing a variable name within a single function, or adjusting a small code snippet. The `target_block` must be unique within the file; otherwise, the tool will return an error. You can use `dry_run: true` to preview the changes as a diff without modifying the file.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "file_path": {"type": "string", "description": "Absolute path to the file."},
                        "target_block": {"type": "string", "description": "The exact, unique text block to be replaced."},
                        "new_block": {"type": "string", "description": "The new text block to replace the target."},
                        "file_hash_sha256": {"type": "string", "description": "The SHA256 hash of the original file content to prevent race conditions."},
                        "dry_run": {"type": "boolean", "description": "If true, returns the diff of the proposed change without modifying the file."}
                    },
                    "required": ["file_path", "target_block", "new_block", "file_hash_sha256"]
                }),
            },
        },
        ToolDef {
            kind: "function".into(),
            function: ToolFunctionDef {
                name: "create_patch".into(),
                description: "Generates a patch in the unified diff format by comparing the `original_content` of a file with its `modified_content`. This tool is crucial for preparing complex, multi-location changes that will be applied using `apply_patch`. First, use `fs_read` to get the `original_content` and its hash. Then, generate the `modified_content` (the entire desired file content after changes) in your mind or through internal reasoning. Finally, call this tool with both contents to obtain the `patch_content` string, which can then be passed to `apply_patch`.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "original_content": {"type": "string", "description": "The original content of the file."},
                        "modified_content": {"type": "string", "description": "The full desired content of the file after modification."}
                    },
                    "required": ["original_content", "modified_content"]
                }),
            },
        },
        ToolDef {
            kind: "function".into(),
            function: ToolFunctionDef {
                name: "apply_patch".into(),
                description: "Applies a patch (in unified diff format) to a file. This is a powerful tool for applying complex changes that affect multiple locations within a file, typically generated by `create_patch`. The `file_hash_sha256` parameter is optional; if provided, it verifies the file's SHA256 hash against it to ensure the file has not been modified externally, providing a safe application. If `file_hash_sha256` is not provided, the tool will proceed without this safety check. If the patch cannot be applied cleanly (e.g., due to conflicts), it will return an error. You can use `dry_run: true` to check if the patch can be applied and to preview the resulting file content without making actual changes.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "file_path": {"type": "string", "description": "Absolute path to the file."},
                        "patch_content": {"type": "string", "description": "The patch content in the unified diff format."},
                        "file_hash_sha256": {"type": "string", "description": "The SHA256 hash of the original file content."},
                        "dry_run": {"type": "boolean", "description": "If true, checks if the patch can be applied cleanly without modifying the file."}
                    },
                    "required": ["file_path", "patch_content"]
                }),
            },
        },
        ToolDef {
            kind: "function".into(),
            function: ToolFunctionDef {
                name: "find_file".into(),
                description: "Finds files in the project based on a filename or pattern. It allows searching for files by name or using glob patterns. The tool is designed to be used by the LLM agent to efficiently locate files without needing to know the exact path. It supports various search criteria to provide flexibility in finding the desired files.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "filename": {"type": "string", "description": "The filename or pattern to search for. This can be a full filename (e.g., `main.rs`), a partial name (e.g., `main`), or a glob pattern (e.g., `*.rs`, `src/**/*.rs`). The search is performed recursively from the project root."}
                    },
                    "required": ["filename"]
                }),
            },
        },
        ToolDef {
            kind: "function".into(),
            function: ToolFunctionDef {
                name: "fs_read_many_files".into(),
                description: "Reads the content of multiple files at once. You can specify a list of file paths or glob patterns. This is useful for getting a comprehensive overview of multiple files, such as all source files in a directory or a set of related configuration files.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "paths": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "A list of absolute file paths or glob patterns."
                        },
                    },
                    "required": ["paths"]
                }),
            },
        },
    ]
}
