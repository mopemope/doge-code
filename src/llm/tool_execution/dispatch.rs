use crate::llm::tool_runtime::ToolRuntime;
use crate::llm::types::ToolCall;
use anyhow::{Result, anyhow};
use tokio::time::Duration;
use tracing::debug;

mod analysis;
mod fs;
mod tools;

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub value: serde_json::Value,
    pub is_success: bool,
    pub result_summary: String,
}

pub async fn dispatch_tool_call(
    runtime: &ToolRuntime<'_>,
    call: &ToolCall,
) -> Result<ToolOutput> {
    debug!("dispatching tool call");
    if call.r#type != "function" {
        return Err(anyhow!("unsupported tool type: {}", call.r#type));
    }
    let name = call.function.name.as_str();
    let args_val: serde_json::Value = serde_json::from_str(&call.function.arguments)
        .map_err(|e| anyhow!("invalid tool args: {e}"))?;

    // Use configured command timeout + buffer, or default to 2 minutes if config is lower.
    // This ensures that tools wrapping long-running commands (like builds) aren't cut off prematurely.
    let config_timeout = runtime.fs.config.command_timeout_ms;
    let timeout_duration = Duration::from_millis(std::cmp::max(config_timeout, 120_000) + 5000);

    let fut = async {
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
            "undo" => tools::undo(runtime, &args_val).await,
            "read_memory" => tools::read_memory(runtime, &args_val).await,
            "write_memory" => tools::write_memory(runtime, &args_val).await,
            "list_memories" => tools::list_memories(runtime, &args_val).await,
            "search_memory" => tools::search_memory(runtime, &args_val).await,
            "run_workflow" => tools::run_workflow(runtime, &args_val).await,
            "doc_generate" => tools::doc_generate(runtime, &args_val).await,
            "search_history" => tools::search_history(runtime, &args_val).await,

            other => {
                if let Some(result) = runtime.fs.call_remote_tool(other, &args_val).await? {
                    Ok(ToolOutput {
                        value: result.clone(),
                        is_success: true, // Remote tools don't yet have a standardized success flag, assume true if Ok
                        result_summary: format!("Remote tool result: {}", serde_json::to_string(&result).unwrap_or_default()),
                    })
                } else {
                    Err(anyhow!("unknown tool: {other}"))
                }
            }
        }
    };

    match tokio::time::timeout(timeout_duration, fut).await {
        Ok(result) => result,
        Err(_) => Err(anyhow!(
            "Tool execution timed out after {} seconds",
            timeout_duration.as_secs()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::llm::types::ToolCallFunction;
    use crate::tools::FsTools;
    use serde_json::json;
    use tempfile::tempdir;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[tokio::test]
    async fn test_execute_bash_failure_flag() -> Result<()> {
        let dir = tempdir()?;
        let config = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            ..AppConfig::default()
        });
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), config);
        let runtime = ToolRuntime::build(&fs_tools).await?;

        let tool_call = ToolCall {
            id: Some("call_1".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "execute_bash".to_string(),
                arguments: json!({
                    "command": "exit 1"
                }).to_string(),
            },
        };

        let output = dispatch_tool_call(&runtime, &tool_call).await?;
        assert!(!output.is_success, "execute_bash(exit 1) should be marked as failure");
        assert_eq!(output.value["exit_code"], 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_bash_success_flag() -> Result<()> {
        let dir = tempdir()?;
        let config = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            ..AppConfig::default()
        });
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), config);
        let runtime = ToolRuntime::build(&fs_tools).await?;

        let tool_call = ToolCall {
            id: Some("call_2".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "execute_bash".to_string(),
                arguments: json!({
                    "command": "echo hello"
                }).to_string(),
            },
        };

        let output = dispatch_tool_call(&runtime, &tool_call).await?;
        assert!(output.is_success, "execute_bash(echo hello) should be marked as success");
        assert_eq!(output.value["exit_code"], 0);
        Ok(())
    }
}
