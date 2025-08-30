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
        Ok(res) => Ok(serde_json::to_value(res)?),
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn create_patch(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let params = serde_json::from_value(args.clone())?;
    match crate::tools::create_patch::create_patch(params).await {
        Ok(res) => Ok(serde_json::to_value(res)?),
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn apply_patch(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let params = serde_json::from_value(args.clone())?;
    match crate::tools::apply_patch::apply_patch(params).await {
        Ok(res) => Ok(serde_json::to_value(res)?),
        Err(e) => Err(anyhow!("{e}")),
    }
}
