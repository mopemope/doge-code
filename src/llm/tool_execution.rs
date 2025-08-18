use crate::llm::chat_with_tools::{
    ChatRequestWithTools, ChatResponseWithTools, ChoiceMessageWithTools,
};
use crate::llm::client_core::OpenAIClient;
use crate::llm::tool_runtime::ToolRuntime;
use crate::llm::types::{ChatMessage, ChoiceMessage, ToolCall, ToolDef};
use crate::tools::FsTools;
use anyhow::{Result, anyhow};
use serde_json::json;
use tracing::{debug, error, warn};

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

    if let Ok(payload) = serde_json::to_string_pretty(&req) {
        debug!(
            payload=%payload,
            endpoint=%url,
            "sending chat.completions (tools) payload",
        );
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
    let response_text: String = resp.text().await?;
    debug!(
        response_body=%response_text,
        "llm chat_tools_once response"
    );
    let body: ChatResponseWithTools = serde_json::from_str(&response_text)?;
    let msg = body
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no choices"))?;
    Ok(msg.message)
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
    let runtime = ToolRuntime::new(fs);
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
) -> Result<(Vec<ChatMessage>, ChoiceMessage)> {
    debug!("run_agent_loop called");
    let runtime = ToolRuntime::new(fs);
    let mut iters = 0usize;
    loop {
        iters += 1;
        debug!(iteration = iters, messages = ?messages, "agent loop iteration");
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
            if let Some(content) = &msg.content
                && !content.is_empty()
                && let Some(tx) = &ui_tx
            {
                let _ = tx.send(content.clone());
            }

            messages.push(ChatMessage {
                role: "assistant".into(),
                content: msg.content.clone(),
                tool_calls: msg.tool_calls.clone(),
                tool_call_id: None,
            });
            for tc in msg.tool_calls {
                if let Some(tx) = &ui_tx {
                    // For fs_write, exclude the content field when logging
                    let log_message = if tc.function.name == "fs_write" {
                        let args_str = tc.function.arguments.clone();
                        // Parse JSON and remove content field
                        if let Ok(mut args_val) =
                            serde_json::from_str::<serde_json::Value>(&args_str)
                        {
                            // remove the content field
                            args_val.as_object_mut().map(|obj| obj.remove("content"));
                            if let Ok(filtered_args_str) = serde_json::to_string(&args_val) {
                                format!("[tool] {}({})", tc.function.name, filtered_args_str)
                            } else {
                                // If serialization fails, fall back to original args
                                format!("[tool] {}({})", tc.function.name, args_str)
                            }
                        } else {
                            // If parsing fails, fall back to original args
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
            debug!(target: "llm", message_content = ?msg.content, "Final assistant message. Content: {:?}", msg.content.as_deref());
            // Final assistant message
            if let Some(tx) = ui_tx {
                debug!(target: "llm", response_content = ?msg.content, "Sending LLM response content. Content: {:?}", msg.content.as_deref());
                let _ = tx.send(format!(
                    "::status:done:{}",
                    msg.content.clone().unwrap_or_default()
                ));
            }
            return Ok((
                messages,
                ChoiceMessage {
                    role: "assistant".into(),
                    content: msg.content.clone().unwrap_or_default(),
                },
            ));
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

            match runtime.fs.get_symbol_info(query, include, kind).await {
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
        "edit" => {
            let params = serde_json::from_value(args_val)?;
            match crate::tools::edit::edit(params).await {
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
        "fs_read_many_files" => {
            let paths = args_val
                .get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let exclude = args_val
                .get("exclude")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect::<Vec<_>>()
                });
            let recursive = args_val.get("recursive").and_then(|v| v.as_bool());
            match runtime.fs.fs_read_many_files(paths, exclude, recursive) {
                Ok(content) => Ok(json!({"ok": true, "content": content})),
                Err(e) => Ok(json!({"ok": false, "error": format!("{e}")})),
            }
        }
        other => Ok(json!({"error": format!("unknown tool: {other}")})),
    };
    debug!(target: "llm", tool_result = ?result, "tool call result");
    result
}
