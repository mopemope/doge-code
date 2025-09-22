use crate::llm::LlmErrorKind;
use crate::llm::tool_runtime::ToolRuntime;
use crate::llm::types::{ChatMessage, ChoiceMessage};
use crate::tools::FsTools;
use crate::tools::todo_write::TodoList;
use anyhow::{Result, anyhow};
use std::process::Command;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

#[allow(clippy::too_many_arguments)]
pub async fn run_agent_loop(
    client: &crate::llm::client_core::OpenAIClient,
    model: &str,
    fs: &FsTools,
    mut messages: Vec<ChatMessage>,
    ui_tx: Option<std::sync::mpsc::Sender<String>>,
    cancel: Option<CancellationToken>,
    cfg: &crate::config::AppConfig,
    tui_executor: Option<&crate::tui::commands::core::TuiExecutor>,
) -> Result<(Vec<ChatMessage>, ChoiceMessage)> {
    debug!("run_agent_loop called");
    let runtime = ToolRuntime::new(fs);
    let mut iters = 0usize;
    let cancel_token = cancel.unwrap_or_default();
    let mut file_was_written = false;

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
            res = crate::llm::tool_execution::requests::chat_tools_once(
                client,
                model,
                messages.clone(),
                &runtime.tools,
                Some(cancel_token.clone()),
            ) => {
                match res {
                    Ok(msg) => msg,
                    Err(e) => {
                        // Check if the error is due to context length exceeded
                        if let Some(LlmErrorKind::ContextLengthExceeded) = e.downcast_ref::<LlmErrorKind>()
                            && let Some(_executor) = tui_executor {
                                // Send a message to the UI to indicate that we are compacting
                                if let Some(tx) = &ui_tx {
                                    let _ = tx.send("[INFO] Context length exceeded. Compacting conversation history...".to_string());
                                }

                                // Call the compact command
                                // Since we don't have access to TuiApp here, we'll need to find a way to trigger the compact command.
                                // One approach is to send a special message to the UI to trigger the compact command.
                                // For now, we'll just return an error to indicate that the operation should be retried after compacting.
                                // A better approach would be to have a callback or a channel to notify the TUI to run the compact command.
                                // For now, we'll return the error to let the caller handle it.
                                return Err(anyhow!(LlmErrorKind::ContextLengthExceeded));
                            }
                        return Err(e);
                    }
                }
            },
        };

        // If assistant returned final content without tool calls, we are done.
        if msg.tool_calls.is_empty() {
            // Send final assistant content to UI (if present)
            if let Some(content) = &msg.content
                && !content.is_empty()
                && let Some(tx) = &ui_tx
            {
                debug!(response_content = ?content, "Sending LLM response content (final).");
                let _ = tx.send(format!("::status:done:{}", content));
            }

            messages.push(ChatMessage {
                role: "assistant".into(),
                content: msg.content.clone(),
                tool_calls: msg.tool_calls.clone(),
                tool_call_id: None,
            });

            // If files were written during tool execution, compute and send git diff
            if cfg.show_diff
                && file_was_written
                && let Some(tx) = &ui_tx
            {
                match Command::new("git")
                    .arg("diff")
                    .arg("--color=always")
                    .output()
                {
                    Ok(output) => {
                        if !output.stdout.is_empty() {
                            let diff_output = String::from_utf8_lossy(&output.stdout).to_string();
                            // Send diff output with a reserved prefix so the TUI can display it as a popup
                            let _ = tx.send(format!("::diff_output:{}", diff_output));
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to run git diff");
                        let _ = tx.send(format!("::diff_output:Failed to run git diff: {}", e));
                    }
                }
            }

            return Ok((
                messages,
                ChoiceMessage {
                    role: "assistant".into(),
                    content: msg.content.clone().unwrap_or_default(),
                },
            ));
        }

        // There are tool calls to process. Send intermediate content if available.
        if let Some(content) = &msg.content
            && !content.is_empty()
            && let Some(tx) = &ui_tx
        {
            debug!(response_content = ?content, "Sending intermediate LLM response content.");
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
                                if let Some(path_str) = value.as_str() {
                                    // Convert to relative path from project root
                                    let project_root = std::env::current_dir()
                                        .unwrap_or_else(|_| std::path::PathBuf::from("."));
                                    if let Ok(relative_path) =
                                        std::path::Path::new(path_str).strip_prefix(&project_root)
                                    {
                                        *value = format!("@{}", relative_path.display()).into();
                                    } else {
                                        // If we can't get a relative path, at least show the file name
                                        if let Some(file_name) = std::path::Path::new(path_str)
                                            .file_name()
                                            .and_then(|s| s.to_str())
                                        {
                                            *value = file_name.to_string().into();
                                        }
                                    }
                                }
                            } else if value.is_array()
                                && let Some(arr) = value.as_array_mut()
                            {
                                for item in arr.iter_mut() {
                                    if let Some(path_str) = item.as_str() {
                                        // Convert to relative path from project root
                                        let project_root = std::env::current_dir()
                                            .unwrap_or_else(|_| std::path::PathBuf::from("."));
                                        if let Ok(relative_path) = std::path::Path::new(path_str)
                                            .strip_prefix(&project_root)
                                        {
                                            *item = format!("@{}", relative_path.display()).into();
                                        } else {
                                            // If we can't get a relative path, at least show the file name
                                            if let Some(file_name) = std::path::Path::new(path_str)
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
                res = crate::llm::tool_execution::dispatch::dispatch_tool_call(&runtime, tc.clone()) => res,
            };

            // Set file_was_written flag for tools that modify files
            if (tc.function.name == "fs_write"
                || tc.function.name == "edit"
                || tc.function.name == "apply_patch")
                && res.is_ok()
            {
                file_was_written = true;
            }

            // Build tool message content (full JSON) for feeding back to the LLM
            let tool_message_content = match &res {
                Ok(value) => serde_json::to_string(value).unwrap_or_else(|_e| {
                    "{\"error\":\"failed to serialize tool result\"}".to_string()
                }),
                Err(e) => {
                    error!(error = %e, "tool execution failed");
                    serde_json::to_string(&serde_json::json!({ "error": e.to_string() }))
                        .unwrap_or_else(|_e| {
                            "{\"error\":\"failed to serialize error\"}".to_string()
                        })
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
                let combined = format!("[tool] {}({}) => {}", tc.function.name, args_str, status);
                let _ = tx.send(combined);
            }

            // Also emit structured debug/error logs (include truncated result summary for debugging)
            match &res {
                Ok(_) => debug!("[tool] {} succeeded: {}", tc.function.name, result_summary),
                Err(e) => error!("[tool] {} failed: {}", tc.function.name, e),
            }

            // Check if the tool call is todo_write and update the todo list in the UI
            if tc.function.name == "todo_write"
                && let Ok(tool_result) = &res
                && let Ok(todo_list) = serde_json::from_value::<TodoList>(tool_result.clone())
            {
                debug!(?todo_list, "Updated todo list from todo_write tool");
                // Send the todo list to the UI
                if let Some(tx) = &ui_tx {
                    // Serialize the todo list to JSON and send it to the UI
                    if let Ok(todo_list_json) = serde_json::to_string(&todo_list.todos) {
                        let _ = tx.send(format!("::todo_list:{}", todo_list_json));
                    }
                }
            }

            // tool message to feed back to the LLM
            messages.push(ChatMessage {
                role: "tool".into(),
                content: Some(tool_message_content),
                tool_calls: vec![],
                tool_call_id: tc.id,
            });
        }
    }
}
