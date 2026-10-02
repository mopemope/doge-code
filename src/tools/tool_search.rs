//! `tool_search` definition: discover and activate deferred tools.
//!
//! The execution handler lives in
//! `src/llm/tool_execution/dispatch/tools.rs` because activation requires
//! the agent's [`ToolRuntime`](crate::llm::tool_runtime::ToolRuntime). This
//! module intentionally holds only the LLM-visible schema plus argument
//! parsing so `ToolCatalog` never depends on dispatch.

use crate::llm::types::{ToolDef, ToolFunctionDef};
use serde_json::json;

pub const TOOL_SEARCH_TOOL_NAME: &str = "tool_search";

/// Per-hit description budget in `tool_search` results. Full schemas are
/// never echoed; they ship only in the next LLM request's `tools` array.
pub const TOOL_SEARCH_DESC_CHARS: usize = 250;

/// Whole-response budget for `tool_search` results (default-tier safety net).
pub const TOOL_SEARCH_RESULT_BUDGET_CHARS: usize = 4_000;

/// Maximum echoed query length in `tool_search` responses (display width,
/// `"..."` suffix included). Ranking always uses the full query; only the
/// echoed copy is bounded so a pathological multi-kilobyte query cannot blow
/// the response budget.
pub const TOOL_SEARCH_QUERY_ECHO_CHARS: usize = 500;

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: TOOL_SEARCH_TOOL_NAME.to_string(),
            description: "Search and activate deferred tools by capability. Use this when the currently available tools cannot perform the task or when access to an external service such as GitHub, Slack, Linear, a database, or another MCP server is needed.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Capability to search for (tool name, task description, resource, or service name)."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 10,
                        "description": "Maximum number of matches to activate. Defaults to the configured search_result_limit."
                    },
                    "server": {
                        "type": "string",
                        "description": "Optional MCP server name to restrict matches to."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            strict: None,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSearchParams {
    pub query: String,
    pub limit: Option<usize>,
    pub server: Option<String>,
}

impl ToolSearchParams {
    pub fn parse(args: &serde_json::Value) -> anyhow::Result<Self> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if query.is_empty() {
            anyhow::bail!("`query` must be a non-empty string");
        }
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        let server = args
            .get("server")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Ok(Self {
            query,
            limit,
            server,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_def_schema() {
        let def = tool_def();
        assert_eq!(def.function.name, "tool_search");
        assert!(
            def.function.description.contains("deferred")
                || def.function.description.contains("activate")
        );
        let required = def.function.parameters["required"]
            .as_array()
            .expect("required array");
        assert!(required.contains(&json!("query")));
        assert!(def.function.parameters["properties"].get("limit").is_some());
        assert!(
            def.function.parameters["properties"]
                .get("server")
                .is_some()
        );
    }

    #[test]
    fn test_parse_rejects_blank_query() {
        assert!(ToolSearchParams::parse(&json!({"query": "   "})).is_err());
        assert!(ToolSearchParams::parse(&json!({})).is_err());
    }

    #[test]
    fn test_parse_accepts_optional_fields() {
        let params =
            ToolSearchParams::parse(&json!({"query": "github", "limit": 3, "server": "github"}))
                .expect("valid params");
        assert_eq!(params.query, "github");
        assert_eq!(params.limit, Some(3));
        assert_eq!(params.server.as_deref(), Some("github"));
    }
}
