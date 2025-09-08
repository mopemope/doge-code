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
    match crate::tools::edit::edit(params).await {
        Ok(res) => {
            // If the edit was successful, update the session with the lines edited
            if res.success {
                // Update session with tool call count
                if let Some(session_manager) = &runtime.fs.session_manager {
                    let mut session_mgr = session_manager.lock().unwrap();
                    if let Err(e) = session_mgr.update_current_session_with_tool_call_count() {
                        tracing::error!(?e, "Failed to update session with tool call count");
                    }

                    // Update session with lines edited count
                    if let Some(lines_edited) = res.lines_edited {
                        let session_mgr = &mut *session_mgr; // Release the immutable borrow
                        if let Some(ref mut session) = session_mgr.current_session {
                            session.increment_lines_edited(lines_edited);
                            if let Err(e) = session_mgr.store.save(session) {
                                tracing::error!(?e, "Failed to save session data");
                            }
                        }
                    }
                }
            }
            Ok(serde_json::to_value(res)?)
        }
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn create_patch(
    _runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let params = serde_json::from_value(args.clone())?;
    match crate::tools::create_patch::create_patch(params).await {
        Ok(res) => Ok(serde_json::to_value(res)?),
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn apply_patch(
    _runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let params = serde_json::from_value(args.clone())?;
    match crate::tools::apply_patch::apply_patch(params).await {
        Ok(res) => Ok(serde_json::to_value(res)?),
        Err(e) => Err(anyhow!("{e}")),
    }
}
