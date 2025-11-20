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
    let params = serde_json::from_value(args.clone())?;

    // Count the tool call attempt
    if let Err(e) = runtime.fs.update_session_with_tool_call_count() {
        tracing::error!(?e, "Failed to update session with tool call count");
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

    match crate::tools::apply_patch::apply_patch(params, &runtime.fs.config).await {
        Ok(res) => {
            // Treat only a logically successful patch as a successful tool call.
            // Non-successful ApplyPatchResult values are recorded as failures
            // to keep metrics aligned with actual outcomes.
            if res.success {
                if let Err(e) = runtime.fs.record_tool_call_success("apply_patch") {
                    tracing::error!(?e, "Failed to record tool call success for apply_patch");
                }
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
