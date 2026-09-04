//! `task` tool: delegates focused exploration or research to an isolated
//! sub-agent loop with read-only tools, returning only a condensed summary.
//!
//! Keeping the sub-agent's tool traffic out of the main conversation prevents
//! large codebases from flooding the main context window.

use crate::llm::types::{ToolDef, ToolFunctionDef};
use serde_json::json;

pub const TASK_TOOL_NAME: &str = "task";

/// Hard iteration bound for a single sub-agent run. Sub-agent work should be
/// focused; 40 iterations is ample for exploration tasks.
pub const SUBAGENT_MAX_ITERS: usize = 40;

/// Character budget for the summary returned to the main agent.
pub const SUBAGENT_SUMMARY_BUDGET_CHARS: usize = 4_000;

/// Tools the sub-agent may use. Read-only: the sub-agent cannot modify the
/// repository or run commands.
pub const SUBAGENT_ALLOWED_TOOLS: &[&str] = &[
    "fs_read",
    "fs_read_many_files",
    "fs_list",
    "find_file",
    "search_text",
    "search_repomap",
];

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: TASK_TOOL_NAME.to_string(),
            description: "Delegates a focused exploration or research task to an isolated sub-agent with read-only tools (fs_read, search_text, search_repomap, fs_list, find_file). The sub-agent works in its own context and returns ONLY a concise summary (facts found, files involved, recommended approach). Use for broad multi-file investigations to keep the main conversation small. The sub-agent CANNOT edit files or run commands.".to_string(),
            strict: None,
            parameters: json!({
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "Short (3-6 words) description of what the sub-agent will investigate, shown to the user."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "Detailed, self-contained instruction for the sub-agent. Include enough context (paths, symbols, what to report) since it does not see the main conversation."
                    }
                },
                "required": ["description", "prompt"]
            }),
        },
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct TaskParams {
    pub description: String,
    pub prompt: String,
}

/// System prompt for the sub-agent: terse, output-shape focused.
pub fn subagent_system_prompt(project_dir: &str) -> String {
    format!(
        "You are a focused research sub-agent working in {project_dir}. \
Your findings will be summarized for a lead agent, so be precise and complete.\n\n\
Rules:\n\
1. You have READ-ONLY tools: fs_read, fs_read_many_files, fs_list, find_file, search_text, search_repomap. You cannot edit files or run commands.\n\
2. Investigate efficiently: prefer search_repomap/search_text over reading whole files; read only the regions you need.\n\
3. Your FINAL answer must be a dense summary (max ~400 words) with these sections:\n\
   - Facts: concrete findings (symbol names, file paths, line references, signatures)\n\
   - Files: the files you examined and what each contains relevant to the task\n\
   - Recommendation: suggested approach for the lead agent, open questions if any\n\
4. Do not include large code blocks in the final answer; reference locations instead."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_def_schema() {
        let def = tool_def();
        assert_eq!(def.function.name, "task");
        let required = def.function.parameters["required"]
            .as_array()
            .expect("required array");
        assert_eq!(required.len(), 2);
        assert!(
            def.function.parameters["properties"]
                .get("prompt")
                .is_some()
        );
    }

    #[test]
    fn test_subagent_tools_are_read_only() {
        for tool in SUBAGENT_ALLOWED_TOOLS {
            assert!(
                !matches!(
                    *tool,
                    "fs_write" | "edit" | "apply_patch" | "execute_bash" | "execute_shell" | "undo"
                ),
                "{tool} must not be available to the sub-agent"
            );
        }
    }

    #[test]
    fn test_subagent_system_prompt_mentions_sections() {
        let prompt = subagent_system_prompt("/tmp/proj");
        assert!(prompt.contains("/tmp/proj"));
        assert!(prompt.contains("Facts"));
        assert!(prompt.contains("Files"));
        assert!(prompt.contains("Recommendation"));
    }
}
