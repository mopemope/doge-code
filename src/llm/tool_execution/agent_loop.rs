use crate::diff_review::DiffReviewPayload;
use crate::llm::LlmErrorKind;
use crate::llm::tool_runtime::ToolRuntime;
use crate::llm::types::{ChatMessage, ChoiceMessage};
use crate::tools::FsTools;
use crate::tools::todo_write::TodoList;
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, FixedOffset, Utc};
use std::process::Command;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

fn collect_diff_review_payload() -> Result<Option<DiffReviewPayload>> {
    let tracked_diff = Command::new("git")
        .arg("diff")
        .arg("--color=never")
        .output()
        .context("failed to run git diff --color=never")?;

    let mut diff_sections = Vec::new();
    if !tracked_diff.stdout.is_empty() {
        let diff = String::from_utf8(tracked_diff.stdout)
            .context("git diff output was not valid UTF-8")?;
        diff_sections.push(diff);
    }

    let names_output = Command::new("git")
        .arg("diff")
        .arg("--name-only")
        .output()
        .context("failed to run git diff --name-only")?;

    let mut files = String::from_utf8(names_output.stdout)
        .context("git diff --name-only output was not valid UTF-8")?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    let status_output = Command::new("git")
        .arg("status")
        .arg("--porcelain=v1")
        .output()
        .context("failed to run git status --porcelain")?;

    let status_text = String::from_utf8(status_output.stdout)
        .context("git status --porcelain output was not valid UTF-8")?;

    for line in status_text.lines() {
        let Some(path) = line.strip_prefix("?? ") else {
            continue;
        };

        if path.trim().is_empty() || path.ends_with('/') {
            continue;
        }

        let path = path.trim();

        let untracked_diff = Command::new("git")
            .arg("diff")
            .arg("--color=never")
            .arg("--no-index")
            .arg("/dev/null")
            .arg(path)
            .output()
            .with_context(|| format!("failed to diff untracked file {path}"))?;

        if !untracked_diff.stdout.is_empty() {
            let diff = String::from_utf8(untracked_diff.stdout)
                .context("git diff --no-index output for untracked file was not valid UTF-8")?;
            diff_sections.push(diff);
        }

        let path_string = path.to_string();
        if !files.contains(&path_string) {
            files.push(path_string);
        }
    }

    if diff_sections.is_empty() {
        return Ok(None);
    }

    let mut combined_diff = diff_sections.join("\n");
    if !combined_diff.ends_with('\n') {
        combined_diff.push('\n');
    }

    Ok(Some(DiffReviewPayload {
        diff: combined_diff,
        files,
    }))
}

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
    let runtime = ToolRuntime::build(fs).await?;
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
                match collect_diff_review_payload() {
                    Ok(Some(payload)) => match serde_json::to_string(&payload) {
                        Ok(json) => {
                            let _ = tx.send(format!("::diff_review:{}", json));
                        }
                        Err(e) => {
                            error!(error = %e, "Failed to serialize diff review payload");
                            let _ = tx.send(format!(
                                    "::diff_review:{}",
                                    serde_json::json!({
                                        "error": format!("Failed to serialize diff review payload: {}", e)
                                    })
                                ));
                        }
                    },
                    Ok(None) => {
                        debug!("No diff detected after tool execution");
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to collect diff review payload");
                        let _ = tx.send(format!(
                            "::diff_review:{}",
                            serde_json::json!({
                                "error": format!("Failed to collect diff review payload: {}", e)
                            })
                        ));
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
            let args_str = tc.function.arguments.clone();
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

                let _ = serde_json::to_string(&args_val);
            }

            let mut args_str_truncated = args_str;
            const MAX_ARG_LEN: usize = 120;
            if args_str_truncated.len() > MAX_ARG_LEN {
                args_str_truncated = format!(
                    "{}...",
                    args_str_truncated
                        .chars()
                        .take(MAX_ARG_LEN - 3)
                        .collect::<String>()
                );
            }

            // Currently args_str_truncated is only used for potential future logging.
            let _ = &args_str_truncated;
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

            // Send a more visually appealing multi-line tool execution display
            if let Some(tx) = &ui_tx {
                let success = res.is_ok();
                let status_text = if success { "✅ SUCCESS" } else { "❌ FAILED" };

                let tool_name = tc.function.name.as_str();

                // Map tool names to appropriate icons
                let tool_icon = match tool_name {
                    "fs_list" => "🗂️",
                    "fs_read" => "📖",
                    "fs_read_many_files" => "📚",
                    "fs_write" => "📝",
                    "search_text" => "🔍",
                    "execute_bash" => "🔧",
                    "find_file" => "📁",
                    "search_repomap" => "🗺️",
                    "edit" => "✏️",
                    "apply_patch" => "🧩",
                    "todo_write" => "📋",
                    "todo_read" => "📋",
                    _ => "🔧", // default icon
                };

                let start_time = std::time::SystemTime::now();
                let utc_datetime: DateTime<Utc> = start_time.into();
                let jst_offset = FixedOffset::east_opt(9 * 3600).unwrap(); // JST is UTC+9
                let jst_datetime = utc_datetime.with_timezone(&jst_offset);
                let timestamp_short = jst_datetime.format("%H:%M:%S").to_string(); // HH:MM:SS format in JST

                // Send indented lines to create a visually distinct tool execution display
                let header_line =
                    format!("🛠️  [{timestamp_short}] {tool_icon} {tool_name} => {status_text}");
                let _ = tx.send(header_line);

                // For fs_read, show the file path right after SUCCESS
                if tool_name == "fs_read"
                    && let Ok(args) =
                        serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                    && let Some(path) = args.get("path").and_then(|v| v.as_str())
                    && success
                {
                    let _ = tx.send(path.to_string());
                }

                // For edit, show the file path right after SUCCESS
                if tool_name == "edit"
                    && let Ok(args) =
                        serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                    && let Some(path) = args.get("file_path").and_then(|v| v.as_str())
                    && success
                {
                    let _ = tx.send(path.to_string());
                }

                // For search_text, show the search keyword right after SUCCESS
                if tool_name == "search_text"
                    && let Ok(args) =
                        serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                    && let Some(keyword) = args.get("search_pattern").and_then(|v| v.as_str())
                    && success
                {
                    let _ = tx.send(format!("Keyword: {}", keyword));
                }

                // For execute_bash, show the command that was executed right after SUCCESS
                if tool_name == "execute_bash"
                    && let Ok(args) =
                        serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                    && let Some(command) = args.get("command").and_then(|v| v.as_str())
                    && success
                {
                    let _ = tx.send(format!("Command: {}", command));
                }

                // Tool arguments and results are intentionally not displayed in the TUI to avoid leaking sensitive data.

                let _ = tx.send("".to_string()); // Extra blank line for spacing
            }

            // Also emit structured debug/error logs (include truncated result summary for debugging)
            match &res {
                Ok(_) => debug!("[tool] {} succeeded: {}", tc.function.name, result_summary),
                Err(e) => error!("[tool] {} failed: {}", tc.function.name, e),
            }

            // Inform the UI that tool processing is complete and we are waiting for the LLM
            if let Some(tx) = &ui_tx {
                let _ = tx.send("::status:waiting".into());
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
