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

use super::ui_rendering::truncate_string_with_graphemes;
use crate::llm::message_utils::truncate_tool_output;
use crate::tui::commands::prompt::build_system_prompt;

#[allow(clippy::too_many_arguments)]
pub async fn run_agent_loop(
    client: &crate::llm::client_core::OpenAIClient,
    model: &str,
    fs: &FsTools,
    messages: Vec<ChatMessage>,
    ui_tx: Option<std::sync::mpsc::Sender<String>>,
    cancel: Option<CancellationToken>,
    cfg: &crate::config::AppConfig,
    _tui_executor: Option<&crate::tui::commands::core::TuiExecutor>,
) -> Result<(Vec<ChatMessage>, ChoiceMessage)> {
    debug!("run_agent_loop called");

    // Initialize HistoryManager
    let mut history = crate::llm::tool_execution::history::HistoryManager::new(
        client.clone(),
        messages,
        ui_tx.clone(),
        fs.clone(),
        cfg.clone(),
    );

    // Inject System Prompt if not already present
    {
        // Check if there's already a system message in the history
        let has_system_prompt = history.iter().any(|m| m.role == "system");

        if !has_system_prompt {
            debug!("Injecting default system prompt");
            let system_msg = ChatMessage {
                role: "system".into(),
                content: Some(build_system_prompt(cfg)),
                tool_calls: vec![],
                tool_call_id: None,
            };
            history.insert(0, system_msg);
        } else {
            debug!("System prompt already present, skipping injection");
        }
    }

    // Inject Proactive Context (Files + Smart Memory)
    if let Err(e) = history.inject_context().await {
        warn!("Failed to inject context: {}", e);
    }

    let runtime = ToolRuntime::build(fs).await?;
    let mut iters = 0usize;
    let cancel_token = cancel.unwrap_or_default();
    let mut file_was_written = false;
    let mut loop_detector = crate::analysis::LoopDetector::new();
    let mut task_sentinel = crate::analysis::TaskSentinel::new();

    loop {
        iters += 1;
        debug!(
            iteration = iters,
            messages_len = history.len(),
            "agent loop iteration"
        );
        if iters > runtime.max_iters {
            warn!(iters, "max tool iterations reached");
            return Err(AgentLoopError::MaxIterations(iters).into());
        }

        // --- Proactive Compaction Check ---
        if let Err(e) = history.check_and_compact_proactive().await {
            error!("Proactive compaction error: {}", e);
            // Continue even if compaction failed, hoping context length isn't fatal yet
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
                history.as_slice(),
                &runtime.tools,
                Some(cancel_token.clone()),
                ui_tx.clone(),
            ) => {
                match res {
                    Ok(msg) => msg,
                    Err(e) => {
                        // Check if the error is due to context length exceeded
                        if let Some(LlmErrorKind::ContextLengthExceeded) = e.downcast_ref::<LlmErrorKind>() {
                            match history.compact_reactive().await {
                                Ok(true) => {
                                    info!("History compaction successful (reactive). Resuming.");
                                    continue;
                                }
                                Ok(false) => {
                                     // Should not happen if compact_reactive returns true only on success
                                }
                                Err(compact_err) => {
                                     error!("Error during reactive history compaction: {}", compact_err);
                                }
                            }
                        }

                        if let Some(LlmErrorKind::Deserialize) = e.downcast_ref::<LlmErrorKind>() {
                            warn!("JSON parse error from LLM: {}", e);
                            let feedback = format!("Error: Invalid JSON format in your response: {}. Please correct your output to be valid JSON. Ensure you are not using markdown code blocks for the entire response if it's not required by the tool.", e);
                            history.push(ChatMessage {
                                role: "user".into(),
                                content: Some(feedback),
                                tool_calls: vec![],
                                tool_call_id: None,
                            });
                             if let Some(tx) = &ui_tx {
                                let _ = tx.send("::status:warning:Invalid JSON received. Requesting correction...".to_string());
                            }
                            continue;
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

            history.push(ChatMessage {
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
                match crate::llm::tool_execution::collect_diff_review_payload(&cfg.project_root)
                    .await
                {
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
                history.into_messages(),
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

        history.push(ChatMessage {
            role: "assistant".into(),
            content: msg.content.clone(),
            tool_calls: msg.tool_calls.clone(),
            tool_call_id: None,
        });

        let mut loop_detected = false;
        for tc in msg.tool_calls {
            if loop_detected {
                // Skip remaining tool calls in the batch
                debug!(tool = %tc.function.name, "Skipping tool call due to loop detection in same batch");
                history.push(ChatMessage {
                    role: "tool".into(),
                    content: Some("{\"error\":\"Loop detected in current tool batch. Execution skipped to allow for immediate reassessment.\"}".to_string()),
                    tool_calls: vec![],
                    tool_call_id: tc.id.clone(),
                });
                continue;
            }

            // Always send processing status to UI if available
            if let Some(tx) = &ui_tx {
                let _ = tx.send("::status:processing".into());
            }

            let tool_name = tc.function.name.as_str();
            let res = tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    warn!("run_agent_loop cancelled before dispatch_tool_call");
                    return Err(anyhow!(LlmErrorKind::Cancelled));
                }
                res = crate::llm::tool_execution::dispatch::dispatch_tool_call(&runtime, &tc) => res,
            };

            // Extract success status and result summary from the structured output
            let (success, result_summary, output_value) = match &res {
                Ok(output) => (
                    output.is_success,
                    output.result_summary.clone(),
                    Some(&output.value),
                ),
                Err(e) => (
                    false,
                    truncate_string_with_graphemes(&e.to_string(), 200),
                    None,
                ),
            };

            // Centralized Session Recording
            if let Err(e) = fs.update_session_with_tool_call_count() {
                error!("Failed to update tool call count: {}", e);
            }
            if success {
                if let Err(e) = fs.record_tool_call_success(tool_name) {
                    error!("Failed to record tool success: {}", e);
                }
            } else if let Err(e) = fs.record_tool_call_failure(tool_name) {
                error!("Failed to record tool failure: {}", e);
            }

            let modifies_files = matches!(tool_name, "fs_write" | "edit" | "apply_patch");

            let ui_args = if success
                && ui_tx.is_some()
                && matches!(
                    tool_name,
                    "fs_read"
                        | "edit"
                        | "fs_list"
                        | "search_text"
                        | "search_repomap"
                        | "execute_bash"
                ) {
                serde_json::from_str::<serde_json::Value>(&tc.function.arguments).ok()
            } else {
                None
            };

            // Set file_was_written flag for tools that modify files
            if modifies_files && success {
                file_was_written = true;
            }

            // Build tool message content (full JSON) for feeding back to the LLM
            let mut tool_message_content = match &res {
                Ok(output) => {
                    let json_str = serde_json::to_string(&output.value).unwrap_or_else(|_e| {
                        "{\"error\":\"failed to serialize tool result\"}".to_string()
                    });
                    truncate_tool_output(json_str, tool_name)
                }
                Err(e) => {
                    error!(error = %e, "tool execution failed");
                    let err_json = serde_json::json!({ "error": e.to_string() });
                    let json_str = serde_json::to_string(&err_json).unwrap_or_else(|_e| {
                        "{\"error\":\"failed to serialize error\"}".to_string()
                    });
                    truncate_tool_output(json_str, tool_name)
                }
            };

            // Inject verification note if file was written
            if modifies_files && success {
                let verification_note = r#"

<SYSTEM_NOTE>
File modification detected. You MUST now verify your changes:

1. Read the file to confirm the content is correct.
2. Run tests to ensure no regressions.
</SYSTEM_NOTE>"#;
                tool_message_content.push_str(verification_note);
            }

            // Prepare a short result summary for UI log and truncate if necessary

            // Send a more visually appealing multi-line tool execution display
            if let Some(tx) = &ui_tx {
                let status_text = if success { "✅ SUCCESS" } else { "❌ FAILED" };

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
                let jst_offset =
                    FixedOffset::east_opt(9 * 3600).unwrap_or(FixedOffset::east_opt(0).unwrap()); // JST is UTC+9, fallback to UTC
                let jst_datetime = utc_datetime.with_timezone(&jst_offset);
                let timestamp_short = jst_datetime.format("%H:%M:%S").to_string(); // HH:MM:SS format in JST

                // Send indented lines to create a visually distinct tool execution display
                let header_line =
                    format!("🛠️  [{timestamp_short}] {tool_icon} {tool_name} => {status_text}");
                let _ = tx.send(header_line);

                // For fs_read, show the file path right after SUCCESS
                if tool_name == "fs_read"
                    && success
                    && let Some(args) = ui_args.as_ref()
                    && let Some(path) = args.get("path").and_then(|v| v.as_str())
                {
                    let _ = tx.send(path.to_string());
                }

                // For edit, show the file path right after SUCCESS
                if tool_name == "edit"
                    && success
                    && let Some(args) = ui_args.as_ref()
                    && let Some(path) = args.get("file_path").and_then(|v| v.as_str())
                {
                    let _ = tx.send(path.to_string());
                }

                // For fs_list, show the directory path right after SUCCESS
                if tool_name == "fs_list"
                    && success
                    && let Some(args) = ui_args.as_ref()
                    && let Some(path) = args.get("path").and_then(|v| v.as_str())
                {
                    let _ = tx.send(path.to_string());
                }

                // For search_text, show the search keyword right after SUCCESS
                if tool_name == "search_text"
                    && success
                    && let Some(args) = ui_args.as_ref()
                    && let Some(keyword) = args.get("search_pattern").and_then(|v| v.as_str())
                {
                    let _ = tx.send(format!("Keyword: {}", keyword));
                }

                // For search_repomap, show the search keywords right after SUCCESS
                if tool_name == "search_repomap"
                    && success
                    && let Some(args) = ui_args.as_ref()
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
                    && success
                    && let Some(args) = ui_args.as_ref()
                    && let Some(command) = args.get("command").and_then(|v| v.as_str())
                {
                    let _ = tx.send(format!("Command: {}", command));
                }

                // If failed, try to show the error message in the TUI log
                if !success {
                    let _ = tx.send(format!("    Error: {}", result_summary));
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
                && let Some(tool_result_value) = output_value
                && let Ok(plan_list) =
                    serde_json::from_value::<PlanList>((*tool_result_value).clone())
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
            history.push(ChatMessage {
                role: "tool".into(),
                content: Some(tool_message_content),
                tool_calls: vec![],
                tool_call_id: tc.id.clone(),
            });

            // Loop Detection
            loop_detector.record_tool_call(&tc);
            if let Some(loop_type) = loop_detector.detect_loop() {
                let warning_msg = loop_detector.loop_warning(&loop_type);

                warn!("Loop detected: {}", warning_msg);
                if let Some(tx) = &ui_tx {
                    let _ = tx.send("::status:warning:Loop detected. Intervening...".to_string());
                }

                history.push(ChatMessage {
                    role: "system".into(), // Escalated to system role
                    content: Some(warning_msg),
                    tool_calls: vec![],
                    tool_call_id: None,
                });
                loop_detected = true;
            }

            // Task Sentinel (Stalled Progress Check)
            // Use the authoritative success flag
            task_sentinel.record_tool_call(&tc.function.name, success);
            if let Some(stall_warning) = task_sentinel.check_stalled() {
                warn!("Stalled progress detected: {}", stall_warning);
                if let Some(tx) = &ui_tx {
                    let _ =
                        tx.send("::status:warning:Progress stalled. Intervening...".to_string());
                }
                history.push(ChatMessage {
                    role: "user".into(),
                    content: Some(stall_warning),
                    tool_calls: vec![],
                    tool_call_id: None,
                });
            }

            // Specific Error Recovery Hints
            if !success {
                // Try to look into the output value for an "error" field if it exists, or use result_summary
                let err_str = if let Some(val) = output_value
                    && let Some(err_field) = val.get("error").and_then(|v| v.as_str())
                {
                    err_field.to_string()
                } else {
                    result_summary.clone()
                };

                if let Some(hint) = crate::llm::tool_execution::error::get_error_hint(&err_str) {
                    history.push(ChatMessage {
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

        // Exception for plan_write
        let plan_content = "na".repeat(15000); // 30000 chars
        let not_truncated_plan = truncate_tool_output(plan_content.clone(), "plan_write");
        assert_eq!(not_truncated_plan.len(), 30000);
        assert!(!not_truncated_plan.contains("truncated"));

        // fs_read too huge
        let huge_read = "na".repeat(21000); // 42000 chars
        let huge_truncated = truncate_tool_output(huge_read.clone(), "fs_read");
        assert!(huge_truncated.contains("truncated"));
    }
}
