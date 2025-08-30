use crate::llm::tool_runtime::ToolRuntime;
use anyhow::{Result, anyhow};
use serde_json::json;

pub async fn get_symbol_info(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let include = args.get("include").and_then(|v| v.as_str());
    let kind = args.get("kind").and_then(|v| v.as_str());

    match runtime.fs.get_symbol_info(query, include, kind).await {
        Ok(items) => Ok(json!({ "ok": true, "symbols": items })),
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn search_repomap(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let args =
        serde_json::from_value::<crate::tools::search_repomap::SearchRepomapArgs>(args.clone())?;
    match runtime.fs.search_repomap(args).await {
        Ok(results) => Ok(json!({ "ok": true, "results": results })),
        Err(e) => Err(anyhow!("{e}")),
    }
}
