use crate::llm::LlmErrorKind;
use crate::llm::tool_execution::error::{AgentLoopError, handle_agent_error};
use crate::llm::tool_runtime::ToolRuntime;
use crate::llm::types::{ChatMessage, ChoiceMessage};
use crate::tools::FsTools;
use crate::tools::plan::PlanList;
use anyhow::{Result, anyhow};
use chrono::{DateTime, FixedOffset, Utc};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

const SYSTEM_PROMPT: &str = r#"You are Doge-Code, an expert AI coding agent.
Your goal is to solve the user's coding tasks autonomously, efficiently, and accurately.

# Core Guidelines
1.  **Reasoning**: You MUST use `<thinking>` tags to explain your thought process, analysis, and plan before executing tools.
2.  **Accuracy**: Verify your assumptions. Read files before editing them. Check for mistakes after editing.
3.  **Efficiency**: Avoid repetitive tool calls. Use `read_many_files` to read multiple files at once.
4.  **Stability**: If a tool fails, analyze the error message carefully. Do not blindly retry the same arguments.

# Communication
- Be concise in your final responses.
- Use Markdown for code navigation.
"#;

fn truncate_tool_output(content: String, tool_name: &str) -> String {
    const DEFAULT_MAX_LEN: usize = 8000;
    const READ_MAX_LEN: usize = 40000; // Allow more context for reading files

    let max_len = if tool_name == "fs_read" || tool_name == "fs_read_many_files" {
        READ_MAX_LEN
    } else {
        DEFAULT_MAX_LEN
    };

    if content.len() > max_len {
        let truncated: String = content.chars().take(max_len).collect();
        format!(
            "{}... (Output truncated. Total length: {} chars. Refine your tool call to reduce output.)",
            truncated,
            content.len()
        )
    } else {
        content
    }
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
    _tui_executor: Option<&crate::tui::commands::core::TuiExecutor>,
) -> Result<(Vec<ChatMessage>, ChoiceMessage)> {
    debug!("run_agent_loop called");

    debug!("run_agent_loop called");

    // Inject System Prompt
    {
        let system_msg = ChatMessage {
            role: "system".into(),
            content: Some(SYSTEM_PROMPT.to_string()),
            tool_calls: vec![],
            tool_call_id: None,
        };
        messages.insert(0, system_msg);
    }

    // Inject Proactive Context
    {
        let cm = fs.context_manager.read().await;
        let context_prompt = cm.get_context_prompt().await;
        if !context_prompt.is_empty() {
            let context_msg = ChatMessage {
                role: "system".into(),
                content: Some(context_prompt),
                tool_calls: vec![],
                tool_call_id: None,
            };
            // Insert before the last message if it's a User message to provide immediate context
            if !messages.is_empty() && messages.last().map(|m| m.role == "user").unwrap_or(false) {
                let idx = messages.len() - 1;
                messages.insert(idx, context_msg);
            } else {
                messages.push(context_msg);
            }
        }
    }

    let runtime = ToolRuntime::build(fs).await?;
    let mut iters = 0usize;
    let cancel_token = cancel.unwrap_or_default();
    let mut file_was_written = false;
    let mut loop_detector = crate::analysis::LoopDetector::new();
    let mut task_sentinel = crate::analysis::TaskSentinel::new();

    loop {
        iters += 1;
        debug!(iteration = iters, messages = ?messages, "agent loop iteration");
        if iters > runtime.max_iters {
            warn!(iters, "max tool iterations reached");
            return Err(AgentLoopError::MaxIterations(iters).into());
        }

        // --- Proactive Compaction Check ---
        let threshold = cfg.auto_compact_prompt_token_threshold_for_current_model();
        let context_limit = cfg.get_context_window_size().unwrap_or(128_000); // Default to a safe large value if unknown
        let safety_limit = (context_limit as f64 * 0.9) as u32;
        let effective_limit = std::cmp::min(threshold, safety_limit);
        let last_prompt_tokens = client.get_prompt_tokens_used();

        // Only compact if we are over the limit AND we have enough history to meaningful compact (avoid loops)
        // We typically want at least System + User + Assistant + Tool (4 messages) or similar complexity before compacting becomes the only option.
        // But strictly > 2 (System + User + something) matches our plan.
        if last_prompt_tokens > effective_limit && messages.len() > 2 {
            warn!(
                current_tokens = last_prompt_tokens,
                limit = effective_limit,
                "Proactive compaction triggered"
            );

            if let Some(tx) = &ui_tx {
                let _ = tx.send(
                    "::status:compacting:Context limits approaching, summarizing history..."
                        .to_string(),
                );
            }

            let params = crate::llm::CompactParams {
                client: client.clone(),
                model: model.to_string(),
                fs_tools: fs.clone(),
                history: messages.clone(),
                cfg: cfg.clone(),
            };

            match crate::llm::compact_conversation_history(params).await {
                Ok(compact_result) => {
                    if compact_result.metadata.success {
                        info!("Proactive history compaction successful.");

                        // Preserve System Prompt if present
                        let system_prompt = messages.iter().find(|m| m.role == "system").cloned();
                        messages.clear();
                        if let Some(sys) = system_prompt {
                            messages.push(sys);
                        }
                        messages.push(compact_result.compacted_message);

                        if let Some(tx) = &ui_tx {
                            let _ = tx.send(
                                "::status:waiting:History compacted. Continuing...".to_string(),
                            );
                        }
                        // Continue loop with new compacted history
                        continue;
                    } else {
                        error!(
                            "Proactive compaction failed: {:?}",
                            compact_result.metadata.error_message
                        );
                    }
                }
                Err(e) => {
                    error!("Proactive compaction error: {}", e);
                }
            }
        }
        // ----------------------------------

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
                        if let Some(LlmErrorKind::ContextLengthExceeded) = e.downcast_ref::<LlmErrorKind>() {
                            warn!("Context length exceeded in agent loop. Attempting to compact history.");

                            // Send a message to the UI to indicate that we are compacting
                            if let Some(tx) = &ui_tx {
                                let _ = tx.send("::status:compacting:Context limits reached, summarizing history...".to_string());
                            }

                            // Perform compaction
                            let params = crate::llm::CompactParams {
                                client: client.clone(),
                                model: model.to_string(),
                                fs_tools: fs.clone(), // FsTools is cheap to clone
                                history: messages.clone(),
                                cfg: cfg.clone(),
                            };

                            match crate::llm::compact_conversation_history(params).await {
                                Ok(compact_result) => {
                                    if compact_result.metadata.success {
                                        info!("History compaction successful. Resuming with compacted history.");

                                        // Preserve System Prompt if present
                                        let system_prompt = messages.iter().find(|m| m.role == "system").cloned();
                                        messages.clear();
                                        if let Some(sys) = system_prompt {
                                            messages.push(sys);
                                        }
                                        messages.push(compact_result.compacted_message);

                                        // Inform UI
                                        if let Some(tx) = &ui_tx {
                                            let _ = tx.send("::status:waiting:History compacted. Retrying...".to_string());
                                        }

                                        // Retry the loop iteration with the new history
                                        continue;
                                    } else {
                                        error!("History compaction failed: {:?}", compact_result.metadata.error_message);
                                        // Fall through to return the original error if compaction failed
                                    }
                                }
                                Err(compact_err) => {
                                    error!("Error during history compaction: {}", compact_err);
                                    // Fall through to return the original error
                                }
                            }
                        }
                        let agent_error = AgentLoopError::Llm(e.to_string());
                        handle_agent_error(&agent_error, &ui_tx);
                        return Err(agent_error.into());
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
                match crate::llm::tool_execution::collect_diff_review_payload().await {
                    Ok(Some(payload)) => match serde_json::to_string(&payload) {
                        Ok(json) => {
                            let _ = tx.send(format!("::diff_review:{}", json));
                        }
                        Err(e) => {
                            let agent_error = AgentLoopError::Serialization(e.to_string());
                            handle_agent_error(&agent_error, &ui_tx);
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
                        let agent_error = AgentLoopError::DiffCollection(e.to_string());
                        handle_agent_error(&agent_error, &ui_tx);
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
                Ok(value) => {
                    let json_str = serde_json::to_string(value).unwrap_or_else(|_e| {
                        "{\"error\":\"failed to serialize tool result\"}".to_string()
                    });
                    truncate_tool_output(json_str, &tc.function.name)
                }
                Err(e) => {
                    error!(error = %e, "tool execution failed");
                    let err_json = serde_json::json!({ "error": e.to_string() });
                    let json_str = serde_json::to_string(&err_json).unwrap_or_else(|_e| {
                        "{\"error\":\"failed to serialize error\"}".to_string()
                    });
                    truncate_tool_output(json_str, &tc.function.name)
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
                    "plan_write" => "🗂️",
                    "plan_read" => "🗂️",
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

                // For fs_list, show the directory path right after SUCCESS
                if tool_name == "fs_list"
                    && let Ok(args) =
                        serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                    && let Some(path) = args.get("path").and_then(|v| v.as_str())
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

                // For search_repomap, show the search keywords right after SUCCESS
                if tool_name == "search_repomap"
                    && let Ok(args) =
                        serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                    && success
                {
                    // Check keyword_search field
                    if let Some(keyword_search) =
                        args.get("keyword_search").and_then(|v| v.as_array())
                        && !keyword_search.is_empty()
                    {
                        let keywords: Vec<String> = keyword_search
                            .iter()
                            .filter_map(|v| v.as_str())
                            .map(|s| s.to_string())
                            .collect();
                        if !keywords.is_empty() {
                            let _ = tx.send(format!("Keywords: {}", keywords.join(", ")));
                        }
                    }
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

            // Check if the tool call is plan_write/plan_read and update the plan list in the UI
            if matches!(tc.function.name.as_str(), "plan_write" | "plan_read")
                && let Ok(tool_result) = &res
                && let Ok(plan_list) = serde_json::from_value::<PlanList>(tool_result.clone())
            {
                debug!(?plan_list, tool = %tc.function.name, "Updated plan list from plan tool");
                // Send the plan list to the UI
                if let Some(tx) = &ui_tx {
                    // Serialize the plan list to JSON and send it to the UI
                    if let Ok(plan_list_json) = serde_json::to_string(&plan_list.items) {
                        let _ = tx.send(format!("::plan_list:{}", plan_list_json));
                    }
                }
            }

            // tool message to feed back to the LLM
            messages.push(ChatMessage {
                role: "tool".into(),
                content: Some(tool_message_content),
                tool_calls: vec![],
                tool_call_id: tc.id.clone(),
            });

            // Loop Detection
            loop_detector.record_tool_call(&tc);
            if let Some(loop_type) = loop_detector.detect_loop() {
                let warning_msg = match loop_type {
                    crate::analysis::loop_detector::LoopType::ConsecutiveRepetition(name) => {
                        format!("WARNING: You are repeatedly calling the tool '{}' with the same arguments. STOP. This strategy is NOT working.\n1. Analyze WHY it is failing.\n2. Read the error message carefully.\n3. Try a DIFFERENT tool or approach (e.g., if `edit` fails, use `fs_read` to verify the file content first).", name)
                    }
                    crate::analysis::loop_detector::LoopType::CycleRepetition => {
                        "WARNING: You are in a repetitive loop (A -> B -> A -> B). Your current mental model is likely incorrect. STOP. Reset your plan. Use `plan_write` to outline a NEW approach.".to_string()
                    }
                };

                warn!("Loop detected: {}", warning_msg);
                if let Some(tx) = &ui_tx {
                    let _ = tx.send("::status:warning:Loop detected. Intervening...".to_string());
                }

                messages.push(ChatMessage {
                    role: "user".into(),
                    content: Some(warning_msg),
                    tool_calls: vec![],
                    tool_call_id: None,
                });
            }

            // Task Sentinel (Stalled Progress Check)
            task_sentinel.record_tool_call(&tc.function.name, res.is_ok());
            if let Some(stall_warning) = task_sentinel.check_stalled() {
                warn!("Stalled progress detected: {}", stall_warning);
                if let Some(tx) = &ui_tx {
                    let _ =
                        tx.send("::status:warning:Progress stalled. Intervening...".to_string());
                }
                messages.push(ChatMessage {
                    role: "user".into(),
                    content: Some(stall_warning),
                    tool_calls: vec![],
                    tool_call_id: None,
                });
            }

            // Specific Error Recovery Hints
            if let Err(e) = &res {
                let err_str = e.to_string();
                if let Some(hint) = crate::llm::tool_execution::error::get_error_hint(&err_str) {
                    messages.push(ChatMessage {
                        role: "user".into(),
                        content: Some(hint.to_string()),
                        tool_calls: vec![],
                        tool_call_id: None,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_tool_output() {
        let short = "short output";
        assert_eq!(truncate_tool_output(short.to_string(), "any_tool"), short);

        let long = "na".repeat(5000); // 10000 chars
        assert!(long.len() > 8000);
        let truncated = truncate_tool_output(long.clone(), "any_tool");
        assert!(truncated.contains("truncated"));
        assert!(truncated.len() < long.len());

        // Exception for fs_read
        let read_content = "na".repeat(15000); // 30000 chars
        let not_truncated = truncate_tool_output(read_content.clone(), "fs_read");
        assert_eq!(not_truncated.len(), 30000);
        assert!(!not_truncated.contains("truncated"));

        // fs_read too huge
        let huge_read = "na".repeat(21000); // 42000 chars
        let huge_truncated = truncate_tool_output(huge_read.clone(), "fs_read");
        assert!(huge_truncated.contains("truncated"));
    }
}
