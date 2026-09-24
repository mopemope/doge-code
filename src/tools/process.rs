//! `execute_process` tool: structured, shell-free process execution.
//!
//! Normal builds, tests, and git commands should use this tool. The program
//! and its arguments are kept separate and the process is spawned directly
//! (`Command::new(program).args(args)`), so shell metacharacters in arguments
//! can never trigger command injection.

use crate::llm::types::{ToolDef, ToolFunctionDef};
use serde_json::json;

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "execute_process".to_string(),
            description: "Executes a program directly WITHOUT a shell (preferred for builds, tests, git, and other single-program commands). `program` is the executable name and `args` are passed as-is; shell syntax (pipes, redirects, `&&`, `$()`) is NOT interpreted. Use `execute_bash` only when shell syntax is genuinely required, and `execute_shell` for persistent shell state.".to_string(),
            strict: Some(true),
            parameters: json!({
                "type": "object",
                "properties": {
                    "program": {
                        "type": "string",
                        "description": "Executable to run (e.g. \"cargo\", \"git\"). Must match allowed_programs exactly; no shell metacharacters."
                    },
                    "args": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Arguments passed directly to the program (no shell expansion)."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Working directory (absolute path preferred). Must be under the project root or allowed_paths."
                    },
                    "env": {
                        "type": "object",
                        "additionalProperties": {"type": "string"},
                        "description": "Environment variable overrides. Only keys listed in allowed_env are accepted."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Per-command timeout in milliseconds. Cannot extend beyond command_timeout_ms; command_timeout_ms = 0 means no configured process timeout."
                    }
                },
                "required": ["program"],
                "additionalProperties": false
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_def_schema() {
        let def = tool_def();
        assert_eq!(def.function.name, "execute_process");
        assert_eq!(def.function.strict, Some(true));
        let params = &def.function.parameters;
        assert_eq!(params["additionalProperties"], false);
        let required = params["required"].as_array().expect("required");
        assert!(required.iter().any(|v| v == "program"));
        assert!(params["properties"]["args"].is_object());
        assert!(params["properties"]["cwd"].is_object());
        assert!(params["properties"]["env"].is_object());
        assert!(params["properties"]["timeout_ms"].is_object());
    }

    #[test]
    fn test_tool_def_no_shell_in_description() {
        let def = tool_def();
        let desc = def.function.description.to_lowercase();
        assert!(desc.contains("without a shell"));
        assert!(desc.contains("execute_bash"));
    }
}
