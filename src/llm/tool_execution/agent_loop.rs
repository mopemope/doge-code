use crate::llm::LlmErrorKind;
use crate::llm::tool_execution::error::{AgentLoopError, handle_agent_error};
use crate::llm::tool_runtime::ToolRuntime;
use crate::llm::types::{ChatMessage, ChoiceMessage};
use crate::tools::FsTools;
use crate::tools::plan::PlanList;
use anyhow::{Result, anyhow};
use chrono::{DateTime, FixedOffset, Utc};
use std::path::PathBuf;
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
    mut messages: Vec<ChatMessage>,
    ui_tx: Option<std::sync::mpsc::Sender<String>>,
    cancel: Option<CancellationToken>,
    cfg: &crate::config::AppConfig,
    _tui_executor: Option<&crate::tui::commands::core::TuiExecutor>,
) -> Result<(Vec<ChatMessage>, ChoiceMessage)> {
    debug!("run_agent_loop called");

    debug!("run_agent_loop called");

    // Inject System Prompt if not already present
    {
        // Check if there's already a system message in the history
        let has_system_prompt = messages.iter().any(|m| m.role == "system");

        if !has_system_prompt {
            debug!("Injecting default system prompt");
            let system_msg = ChatMessage {
                role: "system".into(),
                content: Some(build_system_prompt(cfg)),
                tool_calls: vec![],
                tool_call_id: None,
            };
            messages.insert(0, system_msg);
        } else {
            debug!("System prompt already present, skipping injection");
        }
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
    let verifier = crate::features::verification::AutoVerifier::new(cfg);
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

            match crate::llm::compact_conversation_history_ref(client, model, messages.as_slice())
                .await
            {
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
                messages.as_slice(),
                &runtime.tools,
                Some(cancel_token.clone()),
                ui_tx.clone(),
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

                            match crate::llm::compact_conversation_history_ref(
                                client,
                                model,
                                messages.as_slice(),
                            )
                            .await
                            {
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
                        if let Some(LlmErrorKind::Deserialize) = e.downcast_ref::<LlmErrorKind>() {
                            warn!("JSON parse error from LLM: {}", e);
                            let feedback = format!("Error: Invalid JSON format in your response: {}. Please correct your output to be valid JSON. Ensure you are not using markdown code blocks for the entire response if it's not required by the tool.", e);
                            messages.push(ChatMessage {
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

            let tool_name = tc.function.name.as_str();
            let res = tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    warn!("run_agent_loop cancelled before dispatch_tool_call");
                    return Err(anyhow!(LlmErrorKind::Cancelled));
                }
                res = crate::llm::tool_execution::dispatch::dispatch_tool_call(&runtime, &tc) => res,
            };

            let success = res.is_ok();
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
                Ok(value) => {
                    let json_str = serde_json::to_string(value).unwrap_or_else(|_e| {
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
                // Determine path from tool args
                let args: Option<serde_json::Value> =
                    serde_json::from_str(&tc.function.arguments).ok();
                let path_str = if let Some(args) = &args {
                    match tool_name {
                        "fs_write" => args.get("path").and_then(|v| v.as_str()),
                        "edit" => args.get("file_path").and_then(|v| v.as_str()),
                        "apply_patch" => args.get("file_path").and_then(|v| v.as_str()),
                        _ => None,
                    }
                } else {
                    None
                };

                if let Some(path_str) = path_str {
                    let path = PathBuf::from(path_str);

                    // Initial verification
                    if let Some(mut verification_result) = verifier.verify_path(&path).await {
                        let mut fixed = false;
                        let mut fixed_iters = 0;

                        // Attempt Auto-Fix if configured
                        if cfg.test_fix.max_iterations > 0 {
                            info!(
                                "Verification failed for {}. Attempting auto-fix...",
                                path.display()
                            );
                            if let Some(tx) = &ui_tx {
                                let _ = tx.send(format!("::status:working:Verification failed. Attempting auto-fix (max {} iters)...", cfg.test_fix.max_iterations));
                            }

                            let fixer = crate::features::auto_fix::DefaultFixerAgent::new(
                                fs.clone(),
                                Some(client.clone()),
                                cfg.clone(),
                            );
                            let mut auto_fixer = crate::features::auto_fix::AutoFixer::new(
                                fixer,
                                cfg.test_fix.max_iterations,
                            );

                            let verifier_clone = verifier.clone();
                            let path_clone = path.clone();

                            // Define check function for the loop
                            let check_fn = move || {
                                let v = verifier_clone.clone();
                                let p = path_clone.clone();
                                Box::pin(async move {
                                    match v.verify_path(&p).await {
                                        Some(res) => crate::features::auto_fix::FixableResult {
                                            success: false,
                                            stdout: res.stdout,
                                            stderr: res.stderr,
                                            exit_code: res.exit_code,
                                            context_prompt: None,
                                        },
                                        None => crate::features::auto_fix::FixableResult {
                                            success: true,
                                            stdout: String::new(),
                                            stderr: String::new(),
                                            exit_code: Some(0),
                                            context_prompt: None,
                                        },
                                    }
                                })
                            };

                            // Define prompt function
                            let prompt_fn =
                                |res: &crate::features::auto_fix::FixableResult, iter: usize| {
                                    format!(
                                        "<VERIFICATION_FAILURE iteration=\"{}\">\nThe verification command for {} failed.\n\nSTDOUT:\n{}\n\nSTDERR:\n{}\n\nPlease fix the code to resolve these errors.</VERIFICATION_FAILURE>",
                                        iter,
                                        path_str,
                                        truncate_string_with_graphemes(&res.stdout, 2000),
                                        truncate_string_with_graphemes(&res.stderr, 4000)
                                    )
                                };

                            // Run the fix loop
                            match auto_fixer.run_fix_loop(check_fn, prompt_fn).await {
                                Ok((fixed_res, iters)) => {
                                    if fixed_res.success {
                                        fixed = true;
                                        fixed_iters = iters;
                                    } else {
                                        // Update verification result with the final failure
                                        verification_result.stdout = fixed_res.stdout;
                                        verification_result.stderr = fixed_res.stderr;
                                        verification_result.exit_code = fixed_res.exit_code;
                                        verification_result.message = format!(
                                            "<verification_error>\nVerification Failed after {} auto-fix attempts:\n{}{}\n</verification_error>",
                                            iters,
                                            verification_result.stdout,
                                            verification_result.stderr
                                        );
                                    }
                                }
                                Err(e) => {
                                    error!("Auto-fix execution error: {}", e);
                                    if let Some(tx) = &ui_tx {
                                        let _ = tx
                                            .send(format!("::status:error:Auto-fix error: {}", e));
                                    }
                                }
                            }
                        }

                        if fixed {
                            info!("Auto-fix successful after {} iterations", fixed_iters);
                            tool_message_content.push_str(&format!(
                                "\n\n<AUTO_FIX>\nVerification failed initially, but was automatically fixed after {} iterations.\n</AUTO_FIX>",
                                fixed_iters
                            ));
                            if let Some(tx) = &ui_tx {
                                let _ = tx.send(format!(
                                    "::status:fixed:Auto-fixed after {} iterations.",
                                    fixed_iters
                                ));
                            }
                        } else {
                            // Auto-fix failed or was disabled. Proceed with failure reporting/reverting.

                            // Auto-revert logic (soft revert)
                            let should_revert = false; // verification_result.should_revert && false;

                            if should_revert {
                                // Attempt to revert the change using the undo stack
                                let reverted = {
                                    let mut stack = fs.undo_stack.write().await;
                                    if let Some(entry) = stack.pop() {
                                        // Write back the original content
                                        crate::tools::write::fs_write(
                                            entry.path.to_str().unwrap(),
                                            &entry.content,
                                            cfg,
                                        )
                                        .is_ok()
                                    } else {
                                        false
                                    }
                                };

                                let warning = if reverted {
                                    format!(
                                        r#"
        
        <AUTOMATED_VERIFICATION_FAILURE>
        The tool execution succeeded, but an automated check FAILED with strict mode enabled.
        Exit Code: {:?}
        
        STDOUT:
        {}
        
        STDERR:
        {}
        </AUTOMATED_VERIFICATION_FAILURE>
        
        <DIRECTIVE>
        ⚠️ CRITICAL: Verification failed and your changes have been AUTOMATICALLY REVERTED.
        The file has been restored to its previous state.
        
        1. Analyze the STDERR output above to understand why your code failed.
        2. You MUST apply a DIFFERENT solution. Do not try the same broken code again.
        3. Fix the logical error or syntax error that caused the failure.
        </DIRECTIVE>"#,
                                        verification_result.exit_code,
                                        truncate_string_with_graphemes(
                                            &verification_result.stdout,
                                            1000
                                        ),
                                        truncate_string_with_graphemes(
                                            &verification_result.stderr,
                                            2000
                                        )
                                    )
                                } else {
                                    format!(
                                        r#"
        
        <AUTOMATED_VERIFICATION_FAILURE>
        The tool execution succeeded, but an automated check FAILED.
        Exit Code: {:?}
        
        STDOUT:
        {}
        
        STDERR:
        {}
        </AUTOMATED_VERIFICATION_FAILURE>
        
        <DIRECTIVE>
        ⚠️ CRITICAL: Verification failed and automatic revert FAILED.
        The codebase is potentially in a broken state. You MUST fix this immediately.
        
        1. Analyze the STDERR output above.
        2. Fix the error in the current file state.
        </DIRECTIVE>"#,
                                        verification_result.exit_code,
                                        truncate_string_with_graphemes(
                                            &verification_result.stdout,
                                            1000
                                        ),
                                        truncate_string_with_graphemes(
                                            &verification_result.stderr,
                                            2000
                                        )
                                    )
                                };

                                tool_message_content.push_str(&warning);
                                if let Some(tx) = &ui_tx {
                                    let status_msg = if reverted {
                                        "::status:error:Verification failed. Changes reverted."
                                    } else {
                                        "::status:error:Verification failed. Revert failed."
                                    };
                                    let _ = tx.send(status_msg.to_string());
                                }
                            } else {
                                // Warning only (revert suppressed)
                                let warning = format!(
                                    r#"
        
        <AUTOMATED_VERIFICATION_FAILURE>
        The tool execution succeeded, but an automated check FAILED.
        (Auto-revert suppressed to allow fix)
        Exit Code: {:?}
        
        STDOUT:
        {}
        
        STDERR:
        {}
        </AUTOMATED_VERIFICATION_FAILURE>
        
        <DIRECTIVE>
        ⚠️ CRITICAL: Verification failed.
        The codebase is potentially in a broken state. You MUST fix this immediately.
        Do not proceed with other tasks until this is resolved.
        
        1. Analyze the STDERR output above.
        2. Fix the error in the current file state.
        </DIRECTIVE>"#,
                                    verification_result.exit_code,
                                    truncate_string_with_graphemes(
                                        &verification_result.stdout,
                                        1000
                                    ),
                                    truncate_string_with_graphemes(
                                        &verification_result.stderr,
                                        2000
                                    )
                                );
                                tool_message_content.push_str(&warning);
                                if let Some(tx) = &ui_tx {
                                    let _ = tx.send(
                                        "::status:warning:Auto-verification failed. Correction required."
                                            .to_string(),
                                    );
                                }
                            }
                        }
                    } else {
                        // Verification passed initially
                        let verification_note = r#"
        
        <SYSTEM_NOTE>
        File modification detected. Verification passed.
        </SYSTEM_NOTE>"#;
                        tool_message_content.push_str(verification_note);
                    }
                } else {
                    // Could not determine path, fallback to generic note
                    let verification_note = r#"

<SYSTEM_NOTE>
File modification detected. You MUST now verify your changes:

1. Read the file to confirm the content is correct.
2. Run tests to ensure no regressions.
</SYSTEM_NOTE>"#;
                    tool_message_content.push_str(verification_note);
                }
            }

            // Prepare a short result summary for UI log and truncate if necessary
            let result_summary = truncate_string_with_graphemes(&tool_message_content, 200);

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
                let jst_offset = FixedOffset::east_opt(9 * 3600).unwrap(); // JST is UTC+9
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
                let warning_msg = loop_detector.loop_warning(&loop_type);

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
