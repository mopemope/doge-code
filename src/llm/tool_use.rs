use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use tracing::{debug, error, warn};

use crate::llm::client::{ChatMessage, ChoiceMessage, OpenAIClient};
use crate::tools::FsTools;

// Minimal structures to support tool-calling style chats (non-streaming).

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequestWithTools {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>, // {"type":"auto"}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String, // JSON string per OpenAI spec
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: Option<String>,
    pub r#type: String, // "function"
    pub function: ToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChoiceMessageWithTools {
    pub role: String,
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChoiceWithTools {
    pub index: usize,
    pub message: ChoiceMessageWithTools,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponseWithTools {
    pub id: Option<String>,
    pub choices: Vec<ChoiceWithTools>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMessagePayload {
    pub role: String,    // "tool"
    pub content: String, // JSON string content
    pub tool_call_id: Option<String>,
    pub name: Option<String>,
}

pub struct ToolRuntime<'a> {
    pub tools: Vec<ToolDef>,
    pub fs: &'a FsTools,
    pub max_iters: usize,
    pub request_timeout: Duration,
    pub tool_timeout: Duration,
}

impl<'a> ToolRuntime<'a> {
    pub fn default_with(fs: &'a FsTools) -> Self {
        Self {
            tools: default_tools_def(),
            fs,
            max_iters: 4,
            request_timeout: Duration::from_secs(60),
            tool_timeout: Duration::from_secs(10),
        }
    }
}

pub fn default_tools_def() -> Vec<ToolDef> {
    vec![
        ToolDef {
            kind: "function".into(),
            function: ToolFunctionDef {
                name: "fs_read".into(),
                description: "Read a text file inside the project root. Optionally specify offset and limit (lines).".into(),
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
                name: "fs_search".into(),
                description: "Search files using a regex pattern. Optionally limit by an include glob.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string"},
                        "include": {"type": "string"}
                    },
                    "required": ["pattern"]
                }),
            },
        },
        ToolDef {
            kind: "function".into(),
            function: ToolFunctionDef {
                name: "fs_write".into(),
                description: "Write text to a file inside the project root. Creates parent directories if needed.".into(),
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
    ]
}

pub async fn chat_tools_once(
    client: &OpenAIClient,
    model: &str,
    messages: Vec<ChatMessage>,
    tools: &[ToolDef],
) -> Result<ChoiceMessageWithTools> {
    use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap};

    let url = client.endpoint();
    let req = ChatRequestWithTools {
        model: model.to_string(),
        messages,
        temperature: None,
        tools: Some(tools.to_vec()),
        tool_choice: None,
    };

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());
    headers.insert(
        AUTHORIZATION,
        format!("Bearer {}", client.api_key).parse().unwrap(),
    );

    if let Ok(payload) = serde_json::to_string(&req) {
        debug!(target: "llm", payload=%payload, endpoint=%url, "sending chat.completions (tools) payload");
    }

    let resp = client
        .inner
        .post(&url)
        .headers(headers)
        .json(&req)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        error!(status=%status.as_u16(), body=%text, "llm chat_tools_once non-success status");
        return Err(anyhow!("chat (tools) error: {} - {}", status, text));
    }
    let body: ChatResponseWithTools = resp.json().await?;
    let msg = body
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no choices"))?
        .message;
    Ok(msg)
}

pub async fn run_agent_loop(
    client: &OpenAIClient,
    model: &str,
    fs: &FsTools,
    mut messages: Vec<ChatMessage>,
) -> Result<ChoiceMessage> {
    let runtime = ToolRuntime::default_with(fs);
    let mut iters = 0usize;
    loop {
        iters += 1;
        if iters > runtime.max_iters {
            warn!(iters, "max tool iterations reached");
            return Err(anyhow!("max tool iterations reached"));
        }
        let msg = tokio::time::timeout(
            runtime.request_timeout,
            chat_tools_once(client, model, messages.clone(), &runtime.tools),
        )
        .await
        .map_err(|_| {
            error!("llm chat_tools_once timed out");
            anyhow!("chat tools request timed out")
        })??;

        // If assistant returned final content without tool calls, we are done.
        if !msg.tool_calls.is_empty() {
            messages.push(ChatMessage {
                role: "assistant".into(),
                content: msg.content.clone().unwrap_or_else(|| "".to_string()),
            });
            for tc in msg.tool_calls {
                let res =
                    tokio::time::timeout(runtime.tool_timeout, dispatch_tool_call(&runtime, tc))
                        .await
                        .map_err(|_| {
                            error!("tool execution timed out");
                            anyhow!("tool execution timed out")
                        })??;
                // tool message to feed back
                messages.push(ChatMessage {
                    role: "tool".into(),
                    content: serde_json::to_string(&res).unwrap_or_else(|e| {
                        format!("{{\"error\":\"failed to serialize tool result: {e}\"}}")
                    }),
                });
            }
            continue;
        } else {
            // Final assistant message
            let content = msg.content.unwrap_or_default();
            return Ok(ChoiceMessage {
                role: "assistant".into(),
                content,
            });
        }
    }
}

async fn dispatch_tool_call(
    runtime: &ToolRuntime<'_>,
    call: ToolCall,
) -> Result<serde_json::Value> {
    if call.r#type != "function" {
        return Ok(json!({"error": format!("unsupported tool type: {}", call.r#type)}));
    }
    let name = call.function.name.as_str();
    let args_val: serde_json::Value = match serde_json::from_str(&call.function.arguments) {
        Ok(v) => v,
        Err(e) => return Ok(json!({"error": format!("invalid tool args: {e}")})),
    };
    match name {
        "fs_read" => {
            let path = args_val.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let offset = args_val
                .get("offset")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            let limit = args_val
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            match runtime.fs.fs_read(path, offset, limit) {
                Ok(text) => Ok(json!({"ok": true, "path": path, "content": text})),
                Err(e) => Ok(json!({"ok": false, "error": format!("{e}")})),
            }
        }
        "fs_search" => {
            let pattern = args_val
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let include = args_val.get("include").and_then(|v| v.as_str());
            match runtime.fs.fs_search(pattern, include) {
                Ok(rows) => {
                    let items: Vec<_> = rows
                        .into_iter()
                        .map(|(p, ln, text)| {
                            json!({
                                "path": p.display().to_string(),
                                "line": ln,
                                "text": text,
                            })
                        })
                        .collect();
                    Ok(json!({"ok": true, "results": items}))
                }
                Err(e) => Ok(json!({"ok": false, "error": format!("{e}")})),
            }
        }
        "fs_write" => {
            let path = args_val.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let content = args_val
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            match runtime.fs.fs_write(path, content) {
                Ok(()) => Ok(json!({"ok": true, "path": path, "bytesWritten": content.len()})),
                Err(e) => Ok(json!({"ok": false, "error": format!("{e}")})),
            }
        }
        other => Ok(json!({"error": format!("unknown tool: {other}")})),
    }
}
