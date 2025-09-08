use crate::llm::tool_runtime::ToolRuntime;
use anyhow::{Result, anyhow};
use serde_json::json;

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
