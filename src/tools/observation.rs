//! Read-only `observation_read` builtin: retrieve an offloaded tool result
//! from the conversation-owned Observation Store without re-running the
//! original tool.

use crate::llm::observation::{
    MAX_OBSERVATION_READ_CHARS, OBSERVATION_READ_TOOL_NAME, ObservationStore,
};
use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::json;

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: OBSERVATION_READ_TOOL_NAME.to_string(),
            strict: None,
            description: "Retrieve a tool result that history says was offloaded. Use this instead of rerunning the original operation merely to recover its old output.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Observation id, e.g. obs-000001."},
                    "offset": {"type": "integer", "description": "Byte offset into the stored result (default 0)."},
                    "limit": {"type": "integer", "description": "Maximum bytes to return (default 6000, max 6000)."}
                },
                "required": ["id"]
            }),
        },
    }
}

#[derive(Debug, Deserialize)]
pub struct ObservationReadArgs {
    pub id: String,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Exact targeted retrieval only: one id per call, bounded paging.
/// Never dispatches `read_file`/`search`/`execute`/MCP behind the scenes.
pub fn observation_read(
    store: &ObservationStore,
    args: ObservationReadArgs,
) -> Result<ObservationReadResponse> {
    let id = args.id.trim();
    if id.is_empty() {
        anyhow::bail!("id is required");
    }
    let offset = args.offset.unwrap_or(0);
    let limit = args.limit.unwrap_or(MAX_OBSERVATION_READ_CHARS);
    if limit > MAX_OBSERVATION_READ_CHARS {
        anyhow::bail!("limit exceeds maximum {} bytes", MAX_OBSERVATION_READ_CHARS);
    }
    let page = store
        .read_paged(id, offset, limit)
        .map_err(|e| anyhow!(e.to_string()))?;
    Ok(ObservationReadResponse::from_page(&page))
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ObservationReadResponse {
    pub ok: bool,
    pub id: String,
    pub tool: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub total_bytes: usize,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl ObservationReadResponse {
    pub fn from_page(page: &crate::llm::observation::PagedObservation) -> Self {
        Self {
            ok: true,
            id: page.id.clone(),
            tool: page.tool_name.clone(),
            start_byte: page.start_byte,
            end_byte: page.end_byte,
            total_bytes: page.total_bytes,
            content: page.page.clone(),
            next_cursor: page.next_cursor,
            warnings: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_store() -> (ObservationStore, String) {
        let mut store = ObservationStore::new();
        let content = "αβγ日本語🎉".repeat(200);
        let id = store
            .insert("call-1".into(), "fs_read".into(), content, 100)
            .unwrap();
        (store, id)
    }

    #[test]
    fn test_observation_read_exact_recovery() {
        let (store, id) = seeded_store();
        let original = store.get(&id).unwrap().content.clone();
        // Reassemble through paging.
        let mut out = String::new();
        let mut offset = 0usize;
        loop {
            let res = observation_read(
                &store,
                ObservationReadArgs {
                    id: id.clone(),
                    offset: Some(offset),
                    limit: Some(500),
                },
            )
            .unwrap();
            out.push_str(&res.content);
            match res.next_cursor {
                Some(n) => offset = n,
                None => break,
            }
        }
        assert_eq!(out, original);
    }

    #[test]
    fn test_observation_read_unknown_id_errors_without_fallback() {
        let store = ObservationStore::new();
        let err = observation_read(
            &store,
            ObservationReadArgs {
                id: "obs-999999".into(),
                offset: None,
                limit: None,
            },
        )
        .expect_err("unknown id must error");
        assert!(err.to_string().contains("unknown observation"));
    }

    #[test]
    fn test_observation_read_rejects_unbounded() {
        let (store, id) = seeded_store();
        let err = observation_read(
            &store,
            ObservationReadArgs {
                id,
                offset: None,
                limit: Some(1_000_000),
            },
        )
        .expect_err("over-limit must error");
        assert!(err.to_string().contains("exceeds maximum"));
    }
}
