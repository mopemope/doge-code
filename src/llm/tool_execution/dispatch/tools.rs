use crate::llm::tool_runtime::ToolRuntime;
use anyhow::{Result, anyhow};
use serde_json::json;

pub async fn execute_bash(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
    match runtime.fs.execute_bash(command).await {
        Ok(output) => Ok(json!({ "ok": true, "stdout": output })),
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn edit(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let params: crate::tools::edit::EditParams = serde_json::from_value(args.clone())?;

    // Count the tool call attempt
    if let Err(e) = runtime.fs.update_session_with_tool_call_count() {
        tracing::error!(?e, "Failed to update session with tool call count");
    }

    let file_path = params.file_path.clone();

    // Backup existing file before editing
    if let Err(e) = runtime
        .fs
        .backup_file(std::path::Path::new(&file_path))
        .await
    {
        tracing::warn!("Failed to backup file {}: {}", file_path, e);
    }

    match crate::tools::edit::edit(params, &runtime.fs.config).await {
        Ok(res) => {
            // Record success/failure for this tool call
            if res.success {
                if let Err(e) = runtime.fs.record_tool_call_success("edit") {
                    tracing::error!(?e, "Failed to record tool call success for edit");
                }

                // Update session with lines edited count and log on error
                if let Some(lines_edited) = res.lines_edited
                    && let Err(e) = runtime.fs.update_session_with_lines_edited(lines_edited)
                {
                    tracing::error!(?e, "Failed to update session with lines edited count");
                }
                runtime
                    .fs
                    .update_context(std::path::PathBuf::from(&file_path));

                let _ = runtime
                    .fs
                    .log_action(
                        "edit",
                        &format!("Edited file: {}", file_path),
                        Some(serde_json::json!({
                            "path": file_path,
                            "lines_edited": res.lines_edited,
                        })),
                    )
                    .await;
            } else if let Err(e) = runtime.fs.record_tool_call_failure("edit") {
                tracing::error!(?e, "Failed to record tool call failure for edit");
            }

            Ok(serde_json::to_value(res)?)
        }
        Err(e) => {
            // Record failure for the tool call
            if let Err(rec_err) = runtime.fs.record_tool_call_failure("edit") {
                tracing::error!(
                    ?rec_err,
                    "Failed to record tool call failure for edit on error"
                );
            }
            Err(anyhow!("{e}"))
        }
    }
}

pub async fn apply_patch(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let params = serde_json::from_value(args.clone())?;

    // Count the tool call attempt
    if let Err(e) = runtime.fs.update_session_with_tool_call_count() {
        tracing::error!(?e, "Failed to update session with tool call count");
    }

    match crate::tools::apply_patch::apply_patch_with_recovery(params, &runtime.fs.config).await {
        Ok(res) => {
            // Treat only a logically successful patch as a successful tool call.
            // Non-successful ApplyPatchResult values are recorded as failures
            // to keep metrics aligned with actual outcomes.
            if res.success {
                if let Err(e) = runtime.fs.record_tool_call_success("apply_patch") {
                    tracing::error!(?e, "Failed to record tool call success for apply_patch");
                }

                let _ = runtime
                    .fs
                    .log_action(
                        "apply_patch",
                        "Applied patch",
                        Some(serde_json::json!({
                            "success": true
                        })),
                    )
                    .await;

                // We should probably track context for all files in patch, but params doesn't easily give list?
                // Actually apply_patch params is defined in src/tools/apply_patch.rs.
                // Let's assume for now we don't track context for apply_patch (multi-file) or implemented later.
                // But typically apply_patch is the result of a plan, maybe not critical to track "read" since LLM wrote it.
                // However, editing files puts them in working set.
                // Let's skip for now as I can't easily get the file list from `params` without parsing.
            } else if let Err(e) = runtime.fs.record_tool_call_failure("apply_patch") {
                tracing::error!(
                    ?e,
                    "Failed to record tool call failure for apply_patch with unsuccessful result"
                );
            }

            Ok(serde_json::to_value(res)?)
        }
        Err(e) => {
            // Record failure for the tool call
            if let Err(rec_err) = runtime.fs.record_tool_call_failure("apply_patch") {
                tracing::error!(
                    ?rec_err,
                    "Failed to record tool call failure for apply_patch on error"
                );
            }
            Err(anyhow!("{e}"))
        }
    }
}

pub async fn plan_write(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let params: crate::tools::plan::PlanWriteArgs = serde_json::from_value(args.clone())?;

    // Count the tool call attempt
    if let Err(e) = runtime.fs.update_session_with_tool_call_count() {
        tracing::error!(?e, "Failed to update session with tool call count");
    }

    let plan_items = params.items;
    match runtime.fs.plan_write(plan_items.clone(), params.mode) {
        Ok(res) => {
            // Record success for this tool call
            if let Err(e) = runtime.fs.record_tool_call_success("plan_write") {
                tracing::error!(?e, "Failed to record tool call success for plan_write");
            }

            let _ = runtime
                .fs
                .log_action(
                    "plan_write",
                    "Updated plan",
                    Some(serde_json::json!({
                        "items_count": plan_items.len(),
                        "mode": format!("{:?}", params.mode)
                    })),
                )
                .await;

            // Return the plan as the tool result so the agent loop can forward them to the UI
            Ok(serde_json::to_value(res)?)
        }
        Err(e) => {
            // Record failure for the tool call
            if let Err(rec_err) = runtime.fs.record_tool_call_failure("plan_write") {
                tracing::error!(
                    ?rec_err,
                    "Failed to record tool call failure for plan_write on error"
                );
            }
            Err(anyhow!("{e}"))
        }
    }
}

pub async fn plan_read(
    runtime: &ToolRuntime<'_>,
    _args: &serde_json::Value,
) -> Result<serde_json::Value> {
    if let Err(e) = runtime.fs.update_session_with_tool_call_count() {
        tracing::error!(?e, "Failed to update session with tool call count");
    }

    match runtime.fs.plan_read() {
        Ok(res) => {
            if let Err(e) = runtime.fs.record_tool_call_success("plan_read") {
                tracing::error!(?e, "Failed to record tool call success for plan_read");
            }
            Ok(serde_json::to_value(res)?)
        }
        Err(e) => {
            if let Err(rec_err) = runtime.fs.record_tool_call_failure("plan_read") {
                tracing::error!(
                    ?rec_err,
                    "Failed to record tool call failure for plan_read on error"
                );
            }
            Err(anyhow!("{e}"))
        }
    }
}

pub async fn undo(
    runtime: &ToolRuntime<'_>,
    _args: &serde_json::Value,
) -> Result<serde_json::Value> {
    if let Err(e) = runtime.fs.update_session_with_tool_call_count() {
        tracing::error!(?e, "Failed to update session with tool call count");
    }

    match crate::tools::undo::undo(runtime.fs).await {
        Ok(res) => {
            if let Err(e) = runtime.fs.record_tool_call_success("undo") {
                tracing::error!(?e, "Failed to record tool call success for undo");
            }
            Ok(serde_json::to_value(res)?)
        }
        Err(e) => {
            if let Err(rec_err) = runtime.fs.record_tool_call_failure("undo") {
                tracing::error!(
                    ?rec_err,
                    "Failed to record tool call failure for undo on error"
                );
            }
            Err(anyhow!("{e}"))
        }
    }
}

pub async fn read_memory(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");
    match runtime.fs.read_memory(key).await {
        Ok(content) => Ok(json!({ "content": content })),
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn write_memory(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
    match runtime.fs.write_memory(key, content).await {
        Ok(msg) => Ok(json!({ "message": msg })),
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn list_memories(
    runtime: &ToolRuntime<'_>,
    _args: &serde_json::Value,
) -> Result<serde_json::Value> {
    match runtime.fs.list_memories().await {
        Ok(msg) => Ok(json!({ "result": msg })),
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn doc_generate(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let args = args.as_object().ok_or_else(|| anyhow!("invalid args"))?;
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("path is required"))?;
    let symbol = args.get("symbol").and_then(|v| v.as_str());

    match runtime.fs.doc_generate(path, symbol).await {
        Ok(result) => Ok(json!({ "ok": true, "doc": result })),
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn search_history(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;

    match runtime.fs.search_history(query, limit).await {
        Ok(result) => Ok(json!({ "result": result })),
        Err(e) => Err(anyhow!("{e}")),
    }
}
