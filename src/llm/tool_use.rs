use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use tracing::{debug, error, warn};

use crate::llm::client::{ChatMessage, ChoiceMessage, OpenAIClient, ToolCall};
use crate::tools::FsTools;

const MAX_ITERS: usize = 128;

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
            max_iters: MAX_ITERS,
            request_timeout: Duration::from_secs(60 * 5),
            tool_timeout: Duration::from_secs(10 * 60),
        }
    }
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
    let response_text = resp.text().await?;
    debug!(target: "llm", response_body=%response_text, "llm chat_tools_once response");
    let body: ChatResponseWithTools = serde_json::from_str(&response_text)?;
    let msg = body
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no choices"))?
        .message;
    Ok(msg)
}

pub async fn run_agent_streaming_once(
    client: &OpenAIClient,
    model: &str,
    fs: &FsTools,
    mut messages: Vec<ChatMessage>,
) -> Result<(Vec<ChatMessage>, Option<ChoiceMessage>)> {
    use crate::llm::stream_tools::{ToolDeltaBuffer, execute_tool_call};
    use futures::StreamExt;

    // Start stream
    let mut stream = client.chat_stream(model, messages.clone()).await?;
    let runtime = ToolRuntime::default_with(fs);
    let mut buf = ToolDeltaBuffer::new();
    let mut acc_text = String::new();

    while let Some(chunk) = stream.next().await {
        let delta = chunk?;
        if delta.is_empty() {
            break;
        }
        if let Some(rest) = delta.strip_prefix("__TOOL_CALLS_DELTA__:") {
            // Parse synthetic tool_calls marker and feed into buffer.
            if let Ok(deltas) = serde_json::from_str::<Vec<crate::llm::stream::ToolCallDelta>>(rest)
            {
                for d in deltas {
                    let idx = d.index.unwrap_or(0);
                    let (name_delta, args_delta) = if let Some(f) = d.function {
                        (Some(f.name), Some(f.arguments))
                    } else {
                        (None, None)
                    };
                    buf.push_delta(idx, name_delta.as_deref(), args_delta.as_deref(), None);
                    // Try finalize and execute immediately when JSON becomes valid
                    if buf.finalize_sync_call(idx).is_ok() {
                        let exec = execute_tool_call(&runtime, idx, &buf).await;
                        if let Ok(val) = exec {
                            messages.push(ChatMessage {
                                role: "tool".into(),
                                content: Some(
                                    serde_json::to_string(&val)
                                        .unwrap_or_else(|_| "{\"ok\":false}".to_string()),
                                ),
                                tool_calls: vec![],
                                tool_call_id: None,
                            });
                            // One tool exec per streaming round (minimal policy)
                            return Ok((messages, None));
                        }
                    }
                }
            }
        } else {
            // Accumulate plain text
            acc_text.push_str(&delta);
        }
    }

    // If we have accumulated content and no tool call executed, return it as assistant message
    if !acc_text.is_empty() {
        messages.push(ChatMessage {
            role: "assistant".into(),
            content: Some(acc_text.clone()),
            tool_calls: vec![],
            tool_call_id: None,
        });
        return Ok((
            messages,
            Some(ChoiceMessage {
                role: "assistant".into(),
                content: acc_text,
            }),
        ));
    }

    // No text; in future we will detect and execute tool_calls via ToolDeltaBuffer.
    Ok((messages, None))
}

pub async fn run_agent_loop(
    client: &OpenAIClient,
    model: &str,
    fs: &FsTools,
    mut messages: Vec<ChatMessage>,
    ui_tx: Option<std::sync::mpsc::Sender<String>>,
) -> Result<ChoiceMessage> {
    let runtime = ToolRuntime::default_with(fs);
    let mut iters = 0usize;
    loop {
        iters += 1;
        debug!(target: "llm", iteration = iters, messages = ?messages, "agent loop iteration");
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
            if let Some(content) = &msg.content {
                if !content.is_empty() {
                    if let Some(tx) = &ui_tx {
                        let _ = tx.send(content.clone());
                    }
                }
            }

            messages.push(ChatMessage {
                role: "assistant".into(),
                content: msg.content.clone(),
                tool_calls: msg.tool_calls.clone(),
                tool_call_id: None,
            });
            for tc in msg.tool_calls {
                if let Some(tx) = &ui_tx {
                    // fs_write の場合、contentを除外して表示
                    let log_message = if tc.function.name == "fs_write" {
                        let args_str = tc.function.arguments.clone();
                        // JSONをパースしてcontentを除外
                        if let Ok(mut args_val) =
                            serde_json::from_str::<serde_json::Value>(&args_str)
                        {
                            // contentフィールドを削除
                            args_val.as_object_mut().map(|obj| obj.remove("content"));
                            if let Ok(filtered_args_str) = serde_json::to_string(&args_val) {
                                format!("[tool] {}({})", tc.function.name, filtered_args_str)
                            } else {
                                // パースに失敗した場合は、元の引数をそのまま表示 (念のため)
                                format!("[tool] {}({})", tc.function.name, args_str)
                            }
                        } else {
                            // パースに失敗した場合は、元の引数をそのまま表示 (念のため)
                            format!("[tool] {}({})", tc.function.name, args_str)
                        }
                    } else {
                        format!("[tool] {}({})", tc.function.name, tc.function.arguments)
                    };
                    let _ = tx.send(log_message);
                }
                let res = tokio::time::timeout(runtime.tool_timeout, async {
                    dispatch_tool_call(&runtime, tc.clone()).await
                })
                .await
                .map_err(|_| {
                    error!("tool execution timed out");
                    anyhow!("tool execution timed out")
                })??;
                // tool message to feed back
                messages.push(ChatMessage {
                    role: "tool".into(),
                    content: Some(serde_json::to_string(&res).unwrap_or_else(|e| {
                        format!("{{\"error\":\"failed to serialize tool result: {e}\"}}")
                    })),
                    tool_calls: vec![],
                    tool_call_id: tc.id,
                });
            }
            continue;
        } else {
            // Final assistant message
            return Ok(ChoiceMessage {
                role: "assistant".into(),
                content: msg.content.unwrap_or_default(),
            });
        }
    }
}

pub async fn dispatch_tool_call(
    runtime: &ToolRuntime<'_>,
    call: ToolCall,
) -> Result<serde_json::Value> {
    debug!(target: "llm", tool_call = ?call, "dispatching tool call");
    if call.r#type != "function" {
        return Ok(json!({"error": format!("unsupported tool type: {}", call.r#type)}));
    }
    let name = call.function.name.as_str();
    let args_val: serde_json::Value = match serde_json::from_str(&call.function.arguments) {
        Ok(v) => v,
        Err(e) => return Ok(json!({"error": format!("invalid tool args: {e}")})),
    };
    let result = match name {
        "fs_list" => {
            let path = args_val.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let max_depth = args_val
                .get("max_depth")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            let pattern = args_val.get("pattern").and_then(|v| v.as_str());
            match runtime.fs.fs_list(path, max_depth, pattern) {
                Ok(files) => Ok(json!({"ok": true, "files": files})),
                Err(e) => Ok(json!({"ok": false, "error": format!("{e}")})),
            }
        }
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
        "search_text" => {
            let search_pattern = args_val
                .get("search_pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let file_glob = args_val.get("file_glob").and_then(|v| v.as_str());
            match runtime.fs.search_text(search_pattern, file_glob) {
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
        "get_symbol_info" => {
            let query = args_val.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let include = args_val.get("include").and_then(|v| v.as_str());
            let kind = args_val.get("kind").and_then(|v| v.as_str());
            let sym = crate::tools::symbol::SymbolTools::new();
            match sym.get_symbol_info(query, include, kind) {
                Ok(items) => Ok(json!({"ok": true, "symbols": items})),
                Err(e) => Ok(json!({"ok": false, "error": format!("{e}")})),
            }
        }
        "execute_bash" => {
            let command = args_val
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            match runtime.fs.execute_bash(command).await {
                Ok(output) => Ok(json!({"ok": true, "stdout": output})),
                Err(e) => Ok(json!({"ok": false, "error": format!("{e}")})),
            }
        }
        "get_file_sha256" => {
            let params = serde_json::from_value(args_val)?;
            match crate::tools::get_file_sha256::get_file_sha256(params).await {
                Ok(res) => Ok(serde_json::to_value(res)?),
                Err(e) => Ok(json!({"ok": false, "error": format!("{e}")})),
            }
        }
        "replace_text_block" => {
            let params = serde_json::from_value(args_val)?;
            match crate::tools::replace_text_block::replace_text_block(params).await {
                Ok(res) => Ok(serde_json::to_value(res)?),
                Err(e) => Ok(json!({"ok": false, "error": format!("{e}")})),
            }
        }
        "create_patch" => {
            let params = serde_json::from_value(args_val)?;
            match crate::tools::create_patch::create_patch(params).await {
                Ok(res) => Ok(serde_json::to_value(res)?),
                Err(e) => Ok(json!({"ok": false, "error": format!("{e}")})),
            }
        }
        "apply_patch" => {
            let params = serde_json::from_value(args_val)?;
            match crate::tools::apply_patch::apply_patch(params).await {
                Ok(res) => Ok(serde_json::to_value(res)?),
                Err(e) => Ok(json!({"ok": false, "error": format!("{e}")})),
            }
        }
        "find_file" => {
            let args = serde_json::from_value::<crate::tools::find_file::FindFileArgs>(args_val)?;
            match runtime.fs.find_file(&args.filename).await {
                Ok(res) => Ok(serde_json::to_value(res)?),
                Err(e) => Ok(json!({"ok": false, "error": format!("{e}")})),
            }
        }
        other => Ok(json!({"error": format!("unknown tool: {other}")})),
    };
    debug!(target: "llm", tool_result = ?result, "tool call result");
    result
}
