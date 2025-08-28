use crate::llm::LlmErrorKind;
use crate::llm::chat_with_tools::{
    ChatRequestWithTools, ChatResponseWithTools, ChoiceMessageWithTools,
};
use crate::llm::client_core::OpenAIClient;
use crate::llm::tool_runtime::ToolRuntime;
use crate::llm::types::{ChatMessage, ChoiceMessage, ToolCall, ToolDef};
use crate::tools::FsTools;
use anyhow::{Result, anyhow};
use serde_json::json;
use tokio::time::{Duration, sleep};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

#[allow(unused_assignments)]
pub async fn chat_tools_once(
    client: &OpenAIClient,
    model: &str,
    messages: Vec<ChatMessage>,
    tools: &[ToolDef],
    cancel: Option<CancellationToken>,
) -> Result<ChoiceMessageWithTools> {
    const MAX_RETRIES: u32 = 3;
    let mut attempt = 0;

    // First attempt
    attempt += 1;
    match chat_tools_once_inner(client, model, messages.clone(), tools, cancel.clone()).await {
        Ok(result) => return Ok(result),
        Err(e) => {
            if attempt > MAX_RETRIES {
                return Err(e);
            }
            // Exponential backoff with jitter for first retry
            let delay_ms = (2_u64.pow(attempt - 1) * 1000).min(60_000); // Max 60 seconds
            let jitter = rand::random::<u64>() % 1000; // Add up to 1 second of jitter
            let total_delay = Duration::from_millis(delay_ms + jitter);
            warn!(
                attempt = attempt,
                delay_ms = delay_ms + jitter,
                "Retrying chat_tools_once after error"
            );
            sleep(total_delay).await;
        }
    }

    // Subsequent retries
    let mut last_error: anyhow::Error = anyhow!("Initial error placeholder"); // This will be overwritten
    loop {
        attempt += 1;
        match chat_tools_once_inner(client, model, messages.clone(), tools, cancel.clone()).await {
            Ok(result) => return Ok(result),
            Err(e) => {
                last_error = e;
                if attempt > MAX_RETRIES {
                    break;
                }
                // Exponential backoff with jitter
                let delay_ms = (2_u64.pow(attempt - 1) * 1000).min(60_000); // Max 60 seconds
                let jitter = rand::random::<u64>() % 1000; // Add up to 1 second of jitter
                let total_delay = Duration::from_millis(delay_ms + jitter);
                warn!(
                    attempt = attempt,
                    delay_ms = delay_ms + jitter,
                    "Retrying chat_tools_once after error"
                );
                sleep(total_delay).await;
            }
        }
    }

    Err(last_error)
}

async fn chat_tools_once_inner(
    client: &OpenAIClient,
    model: &str,
    messages: Vec<ChatMessage>,
    tools: &[ToolDef],
    cancel: Option<CancellationToken>,
) -> Result<ChoiceMessageWithTools> {
    use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap};

    let url = client.endpoint();
    let reasoning_effort = if client.reason_enable {
        Some("high".to_owned())
    } else {
        None
    };
    let req = ChatRequestWithTools {
        model: model.to_string(),
        messages,
        temperature: None,
        tools: Some(tools.to_vec()),
        tool_choice: None,
        reasoning_effort,
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

    let cancel_token = cancel.unwrap_or_default();
    let req_builder = client.inner.post(&url).headers(headers).json(&req);

    let resp = tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            warn!("chat_tools_once cancelled before send");
            return Err(anyhow!(LlmErrorKind::Cancelled));
        }
        res = req_builder.send() => res?,
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        error!(status=%status.as_u16(), body=%text, "llm chat_tools_once non-success status");
        return Err(anyhow!("chat (tools) error: {} - {}", status, text));
    }

    let response_text: String = tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            warn!("chat_tools_once cancelled during body read");
            return Err(anyhow!(LlmErrorKind::Cancelled));
        }
        res = resp.text() => res?,
    };

    debug!(
        response_body=%response_text,
        "llm chat_tools_once response"
    );
    let body: ChatResponseWithTools = serde_json::from_str(&response_text)?;

    // Track token usage if available
    if let Some(usage) = &body.usage {
        client.add_tokens(usage.total_tokens);
    }

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
    cancel: Option<CancellationToken>,
) -> Result<(Vec<ChatMessage>, Option<ChoiceMessage>)> {
    use crate::llm::stream_tools::{ToolDeltaBuffer, execute_tool_call};
    use futures::StreamExt;

    let cancel_token = cancel.unwrap_or_default();

    // Start stream
    let mut stream = client
        .chat_stream(model, messages.clone(), Some(cancel_token.clone()))
        .await?;
    let runtime = ToolRuntime::new(fs);
    let mut buf = ToolDeltaBuffer::new();
    let mut acc_text = String::new();

    loop {
        let chunk = tokio::select! {
            biased;
            _ = cancel_token.cancelled() => {
                warn!("run_agent_streaming_once cancelled");
                return Err(anyhow!(LlmErrorKind::Cancelled));
            }
            chunk = stream.next() => chunk,
        };

        match chunk {
            Some(Ok(delta)) => {
                if delta.is_empty() {
                    break;
                }
                if let Some(rest) = delta.strip_prefix("__TOOL_CALLS_DELTA__:") {
                    // Parse synthetic tool_calls marker and feed into buffer.
                    if let Ok(deltas) =
                        serde_json::from_str::<Vec<crate::llm::stream::ToolCallDelta>>(rest)
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
            Some(Err(e)) => {
                if let Some(kind) = e.downcast_ref::<LlmErrorKind>()
                    && kind == &LlmErrorKind::Cancelled
                {
                    warn!("run_agent_streaming_once stream cancelled");
                    return Err(anyhow!(LlmErrorKind::Cancelled));
                }
                return Err(e);
            }
            None => break, // End of stream
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
    cancel: Option<CancellationToken>,
) -> Result<(Vec<ChatMessage>, ChoiceMessage)> {
    debug!("run_agent_loop called");
    let runtime = ToolRuntime::new(fs);
    let mut iters = 0usize;
    let cancel_token = cancel.unwrap_or_default();

    loop {
        iters += 1;
        debug!(iteration = iters, messages = ?messages, "agent loop iteration");
        if iters > runtime.max_iters {
            warn!(iters, "max tool iterations reached");
            return Err(anyhow!("max tool iterations reached"));
        }

        let msg = tokio::select! {
            biased;
            _ = cancel_token.cancelled() => {
                warn!("run_agent_loop cancelled before chat_tools_once");
                return Err(anyhow!(LlmErrorKind::Cancelled));
            }
            res = chat_tools_once(client, model, messages.clone(), &runtime.tools, Some(cancel_token.clone())) => res?,
        };

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
                // Always send processing status to UI if available
                if let Some(tx) = &ui_tx {
                    let _ = tx.send("::status:processing".into());
                }

                // Prepare and sanitize arguments for logging
                let mut args_str = tc.function.arguments.clone();
                if let Ok(mut args_val) = serde_json::from_str::<serde_json::Value>(&args_str) {
                    if let Some(obj) = args_val.as_object_mut() {
                        if tc.function.name == "fs_write" {
                            obj.remove("content");
                        }

                        for key in ["path", "paths", "file_path", "filename"].iter() {
                            if let Some(value) = obj.get_mut(*key) {
                                if value.is_string() {
                                    if let Some(path_str) = value.as_str()
                                        && let Some(file_name) = std::path::Path::new(path_str)
                                            .file_name()
                                            .and_then(|s| s.to_str())
                                    {
                                        *value = file_name.to_string().into();
                                    }
                                } else if value.is_array()
                                    && let Some(arr) = value.as_array_mut()
                                {
                                    for item in arr.iter_mut() {
                                        if let Some(path_str) = item.as_str()
                                            && let Some(file_name) = std::path::Path::new(path_str)
                                                .file_name()
                                                .and_then(|s| s.to_str())
                                        {
                                            *item = file_name.to_string().into();
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if let Ok(modified_args_str) = serde_json::to_string(&args_val) {
                        args_str = modified_args_str;
                    }
                }

                const MAX_ARG_LEN: usize = 120;
                if args_str.len() > MAX_ARG_LEN {
                    let mut truncated = args_str.chars().take(MAX_ARG_LEN - 3).collect::<String>();
                    truncated.push_str("...");
                    args_str = truncated;
                }

                let res = tokio::select! {
                    biased;
                    _ = cancel_token.cancelled() => {
                        warn!("run_agent_loop cancelled before dispatch_tool_call");
                        return Err(anyhow!(LlmErrorKind::Cancelled));
                    }
                    res = dispatch_tool_call(&runtime, tc.clone()) => res,
                };

                // Build tool message content (full JSON) for feeding back to the LLM
                let tool_message_content = match &res {
                    Ok(value) => serde_json::to_string(value).unwrap_or_else(|_e| {
                        "{\"error\":\"failed to serialize tool result\"}".to_string()
                    }),
                    Err(e) => {
                        warn!(error = %e, "tool execution failed");
                        serde_json::to_string(&json!({ "error": e.to_string() })).unwrap_or_else(
                            |_e| "{\"error\":\"failed to serialize error\"}".to_string(),
                        )
                    }
                };

                // Prepare a short result summary for UI log and truncate if necessary
                let mut result_summary = tool_message_content.clone();
                const MAX_RESULT_LEN: usize = 200;
                if result_summary.len() > MAX_RESULT_LEN {
                    let mut t = result_summary
                        .chars()
                        .take(MAX_RESULT_LEN - 3)
                        .collect::<String>();
                    t.push_str("...");
                    result_summary = t;
                }

                // Send a single combined log line: usage + success/failure marker
                if let Some(tx) = &ui_tx {
                    let success = res.is_ok();
                    let status = if success { "OK" } else { "ERR" };
                    let combined =
                        format!("[tool] {}({}) => {}", tc.function.name, args_str, status);
                    let _ = tx.send(combined);
                }

                // Also emit structured debug/warn logs (include truncated result summary for debugging)
                match &res {
                    Ok(_) => {
                        debug!(target: "llm", "[tool] {} succeeded: {}", tc.function.name, result_summary)
                    }
                    Err(e) => warn!(target: "llm", "[tool] {} failed: {}", tc.function.name, e),
                }

                // tool message to feed back to the LLM
                messages.push(ChatMessage {
                    role: "tool".into(),
                    content: Some(tool_message_content),
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
        return Err(anyhow!("unsupported tool type: {}", call.r#type));
    }
    let name = call.function.name.as_str();
    let args_val: serde_json::Value = serde_json::from_str(&call.function.arguments)
        .map_err(|e| anyhow!("invalid tool args: {e}"))?;

    let result = match name {
        "fs_list" => {
            let path = args_val.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let max_depth = args_val
                .get("max_depth")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            let pattern = args_val.get("pattern").and_then(|v| v.as_str());
            match runtime.fs.fs_list(path, max_depth, pattern) {
                Ok(files) => Ok(json!({ "ok": true, "files": files })),
                Err(e) => Err(anyhow!("{e}")),
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
                Ok(text) => Ok(json!({ "ok": true, "path": path, "content": text })),
                Err(e) => Err(anyhow!("{e}")),
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
                    Ok(json!({ "ok": true, "results": items }))
                }
                Err(e) => Err(anyhow!("{e}")),
            }
        }
        "fs_write" => {
            let path = args_val.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let content = args_val
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            match runtime.fs.fs_write(path, content) {
                Ok(()) => Ok(json!({ "ok": true, "path": path, "bytesWritten": content.len() })),
                Err(e) => Err(anyhow!("{e}")),
            }
        }
        "get_symbol_info" => {
            let query = args_val.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let include = args_val.get("include").and_then(|v| v.as_str());
            let kind = args_val.get("kind").and_then(|v| v.as_str());

            match runtime.fs.get_symbol_info(query, include, kind).await {
                Ok(items) => Ok(json!({ "ok": true, "symbols": items })),
                Err(e) => Err(anyhow!("{e}")),
            }
        }
        "search_repomap" => {
            let args = serde_json::from_value::<crate::tools::search_repomap::SearchRepomapArgs>(
                args_val.clone(),
            )?;
            match runtime.fs.search_repomap(args).await {
                Ok(results) => Ok(json!({ "ok": true, "results": results })),
                Err(e) => Err(anyhow!("{e}")),
            }
        }
        "execute_bash" => {
            let command = args_val
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            match runtime.fs.execute_bash(command).await {
                Ok(output) => Ok(json!({ "ok": true, "stdout": output })),
                Err(e) => Err(anyhow!("{e}")),
            }
        }

        "edit" => {
            let params = serde_json::from_value(args_val.clone())?;
            match crate::tools::edit::edit(params).await {
                Ok(res) => Ok(serde_json::to_value(res)?),
                Err(e) => Err(anyhow!("{e}")),
            }
        }
        "create_patch" => {
            let params = serde_json::from_value(args_val.clone())?;
            match crate::tools::create_patch::create_patch(params).await {
                Ok(res) => Ok(serde_json::to_value(res)?),
                Err(e) => Err(anyhow!("{e}")),
            }
        }
        "apply_patch" => {
            let params = serde_json::from_value(args_val.clone())?;
            match crate::tools::apply_patch::apply_patch(params).await {
                Ok(res) => Ok(serde_json::to_value(res)?),
                Err(e) => Err(anyhow!("{e}")),
            }
        }
        "find_file" => {
            let args =
                serde_json::from_value::<crate::tools::find_file::FindFileArgs>(args_val.clone())?;
            match runtime.fs.find_file(&args.filename).await {
                Ok(res) => Ok(serde_json::to_value(res)?),
                Err(e) => Err(anyhow!("{e}")),
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
                Ok(content) => Ok(json!({ "ok": true, "content": content })),
                Err(e) => Err(anyhow!("{e}")),
            }
        }
        other => Err(anyhow!("unknown tool: {other}")),
    };

    debug!(target: "llm", tool_result = ?result, "tool call result");
    result
}
