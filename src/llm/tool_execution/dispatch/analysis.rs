use crate::llm::tool_execution::dispatch::ToolOutput;
use crate::llm::tool_runtime::ToolRuntime;
use anyhow::{Result, anyhow};
use serde_json::json;

pub async fn search_repomap(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<ToolOutput> {
    let args =
        serde_json::from_value::<crate::tools::search_repomap::SearchRepomapArgs>(args.clone())?;
    match runtime.fs.search_repomap(args).await {
        Ok(results) => {
            let value = json!({ "ok": true, "results": results });
            Ok(ToolOutput {
                value: value.clone(),
                is_success: true,
                result_summary: format!("Found {} results in repomap", results.results.len()),
            })
        }
        Err(e) => Err(anyhow!("{e}")),
    }
}
