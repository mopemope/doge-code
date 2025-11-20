use crate::llm::tool_runtime::ToolRuntime;
use crate::llm::types::ToolCall;
use anyhow::{Result, anyhow};
use tracing::debug;

mod analysis;
mod fs;
mod tools;

pub async fn dispatch_tool_call(
    runtime: &ToolRuntime<'_>,
    call: ToolCall,
) -> Result<serde_json::Value> {
    debug!("dispatching tool call");
    if call.r#type != "function" {
        return Err(anyhow!("unsupported tool type: {}", call.r#type));
    }
    let name = call.function.name.as_str();
    let args_val: serde_json::Value = serde_json::from_str(&call.function.arguments)
        .map_err(|e| anyhow!("invalid tool args: {e}"))?;

    match name {
        // FS-related
        "fs_list" => fs::fs_list(runtime, &args_val).await,
        "fs_read" => fs::fs_read(runtime, &args_val).await,
        "search_text" => fs::search_text(runtime, &args_val).await,
        "fs_write" => fs::fs_write(runtime, &args_val).await,
        "find_file" => fs::find_file(runtime, &args_val).await,
        "fs_read_many_files" => fs::fs_read_many_files(runtime, &args_val).await,

        // Analysis / repomap
        "search_repomap" => analysis::search_repomap(runtime, &args_val).await,

        // Tools and helpers
        "execute_bash" => tools::execute_bash(runtime, &args_val).await,
        "edit" => tools::edit(runtime, &args_val).await,
        "apply_patch" => tools::apply_patch(runtime, &args_val).await,
        "plan_write" => tools::plan_write(runtime, &args_val).await,
        "plan_read" => tools::plan_read(runtime, &args_val).await,

        other => {
            if let Some(result) = runtime.fs.call_remote_tool(other, &args_val).await? {
                Ok(result)
            } else {
                Err(anyhow!("unknown tool: {other}"))
            }
        }
    }
}
