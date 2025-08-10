use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tracing::debug;

use crate::llm::tool_execution::{ToolRuntime, dispatch_tool_call as dispatch_sync_tool_call};
use crate::llm::types::{ToolCall as SyncToolCall, ToolCallFunction};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReconstructedToolCall {
    pub id: Option<String>,
    pub name: String,
    pub arguments: String, // raw JSON string (possibly pretty), expected to be valid JSON
}

// Buffer to reconstruct tool_calls from streamed deltas
#[derive(Debug, Default)]
pub struct ToolDeltaBuffer {
    // Each index may have an in-progress tool call
    calls: Vec<ReconstructedToolCall>,
}

impl ToolDeltaBuffer {
    pub fn new() -> Self {
        Self { calls: Vec::new() }
    }

    // Append one delta; index can be sparse/increasing; we resize as needed
    pub fn push_delta(
        &mut self,
        index: usize,
        name_delta: Option<&str>,
        args_delta: Option<&str>,
        id: Option<&str>,
    ) {
        if self.calls.len() <= index {
            self.calls.resize_with(index + 1, Default::default);
        }
        let slot = &mut self.calls[index];
        if let Some(idv) = id {
            if slot.id.is_none() {
                slot.id = Some(idv.to_string());
            }
        }
        if let Some(n) = name_delta {
            if !n.is_empty() {
                if slot.name.is_empty() {
                    slot.name = n.to_string();
                } else {
                    slot.name.push_str(n);
                }
            }
        }
        if let Some(a) = args_delta {
            if !a.is_empty() {
                if slot.arguments.is_empty() {
                    slot.arguments = a.to_string();
                } else {
                    slot.arguments.push_str(a);
                }
            }
        }
    }

    // Attempt to parse the arguments JSON for a given index, returning a structured SyncToolCall
    pub fn finalize_sync_call(&self, index: usize) -> Result<SyncToolCall> {
        let rc = self
            .calls
            .get(index)
            .ok_or_else(|| anyhow!("no tool call at index {index}"))?;
        // Validate minimal fields
        if rc.name.is_empty() {
            return Err(anyhow!("tool call missing name"));
        }
        // Ensure arguments is valid JSON string per OpenAI spec
        let _parsed: JsonValue = serde_json::from_str(&rc.arguments)
            .map_err(|e| anyhow!("invalid tool arguments JSON: {e}"))?;
        Ok(SyncToolCall {
            id: rc.id.clone(),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: rc.name.clone(),
                arguments: rc.arguments.clone(),
            },
        })
    }
}

// Execute a reconstructed tool call via existing dispatcher
pub async fn execute_tool_call(
    runtime: &ToolRuntime<'_>,
    index: usize,
    buf: &ToolDeltaBuffer,
) -> Result<serde_json::Value> {
    let sc = buf.finalize_sync_call(index)?;
    debug!(target: "llm", tool_call = ?sc, "executing reconstructed tool call");
    let res = dispatch_sync_tool_call(runtime, sc).await?;
    Ok(res)
}
