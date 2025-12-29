use crate::analysis::semantic::SemanticService;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::Result;
use serde_json::json;

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "search_history".to_string(),
            description: "Search the agent's action history semantically. Use this to find how similar problems were solved in the past, or to check past actions in the current or previous sessions. Useful for 'learning' from past mistakes or recalling successful patterns.".to_string(),
            strict: Some(true),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural language query describing the situation, error, or action you are looking for."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default 5).",
                        "default": 5
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        },
    }
}

pub async fn search_history(
    semantic_service: &Option<SemanticService>,
    query: &str,
    limit: usize,
) -> Result<String> {
    if let Some(service) = semantic_service {
        let results = service.search_action_history(query, limit).await?;

        if results.is_empty() {
            return Ok("No relevant history found.".to_string());
        }

        let mut output = String::new();
        output.push_str("Found relevant history:\n\n");

        for (log, score) in results {
            output.push_str(&format!("--- (Score: {:.2}) ---\n", score));
            output.push_str(&format!("Action: {}\n", log.action_type));
            output.push_str(&format!("Timestamp: {}\n", log.timestamp));
            output.push_str(&format!("Content: {}\n", log.content));
            output.push_str(&format!("Metadata: {}\n", log.metadata));
            output.push('\n');
        }
        Ok(output)
    } else {
        Ok("History search unavailable (RAG/Semantic Service disabled).".to_string())
    }
}
