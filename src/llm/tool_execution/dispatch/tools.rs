use crate::llm::tool_execution::dispatch::ToolOutput;
use crate::llm::tool_runtime::ToolRuntime;
use anyhow::{Result, anyhow};
use serde_json::json;

pub async fn execute_bash(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<ToolOutput> {
    let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
    match runtime.fs.execute_bash(command).await {
        Ok(output_str) => {
            // output_str is a JSON string of ExecuteBashResult
            let result: crate::tools::execute::ExecuteBashResult =
                serde_json::from_str(&output_str)?;
            let value = json!({ "ok": true, "stdout": result.stdout, "stderr": result.stderr, "exit_code": result.exit_code, "success": result.success });
            Ok(ToolOutput {
                value: value.clone(),
                is_success: result.success,
                result_summary: format!(
                    "Command '{}' finished with exit code {:?}",
                    command, result.exit_code
                ),
            })
        }
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn edit(runtime: &ToolRuntime<'_>, args: &serde_json::Value) -> Result<ToolOutput> {
    let params: crate::tools::edit::EditParams = serde_json::from_value(args.clone())?;

    // Count the tool call attempt (to be removed once centralized)
    // Actually, per plan, we remove redundant recording here.
    // However, Plan Step 3 says "Remove redundant record_tool_call_success/failure".
    // "Count the tool call attempt" is `update_session_with_tool_call_count`.
    // We will leave the centralized recording for `agent_loop.rs` and REMOVE it here.

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
            if res.success {
                // We do NOT record success here anymore.
                // But we DO need to update lines edited count and context.
                if let Some(lines_edited) = res.lines_edited
                    && let Err(e) = runtime.fs.update_session_with_lines_edited(lines_edited)
                {
                    tracing::error!(?e, "Failed to update session with lines edited count");
                }

                let p = std::path::PathBuf::from(&file_path);
                runtime.fs.update_context(p.clone());
                let _ = runtime.fs.update_session_if_changed(&p);
            }
            // We do NOT record failure here anymore.

            let value = serde_json::to_value(&res)?;
            Ok(ToolOutput {
                value: value.clone(),
                is_success: res.success,
                result_summary: if res.success {
                    format!("Successfully edited {}", file_path)
                } else {
                    format!("Failed to edit {}: {:?}", file_path, res.message)
                },
            })
        }
        Err(e) => {
            // We do NOT record failure here anymore.
            Err(anyhow!("{e}"))
        }
    }
}

pub async fn apply_patch(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<ToolOutput> {
    let params: crate::tools::apply_patch::ApplyPatchParams = serde_json::from_value(args.clone())?;
    let file_path = params.file_path.clone();

    // Remove redundant session update

    match crate::tools::apply_patch::apply_patch_with_recovery(params, &runtime.fs.config).await {
        Ok(res) => {
            // Remove redundant recording
            if res.success {
                let p = std::path::PathBuf::from(&file_path);
                let _ = runtime.fs.update_session_if_changed(&p);
            }

            let value = serde_json::to_value(&res)?;
            Ok(ToolOutput {
                value: value.clone(),
                is_success: res.success,
                result_summary: if res.success {
                    "Successfully applied patch".to_string()
                } else {
                    "Failed to apply patch".to_string()
                },
            })
        }
        Err(e) => {
            // Remove redundant recording
            Err(anyhow!("{e}"))
        }
    }
}

pub async fn plan_write(runtime: &ToolRuntime<'_>, args: &serde_json::Value) -> Result<ToolOutput> {
    let params: crate::tools::plan::PlanWriteArgs = serde_json::from_value(args.clone())?;

    // Remove redundant session update

    let plan_items = params.items;
    match runtime.fs.plan_write(plan_items, params.mode) {
        Ok(res) => {
            // Remove redundant recording

            let value = serde_json::to_value(&res)?;
            Ok(ToolOutput {
                value: value.clone(),
                is_success: true,
                result_summary: if res.changed {
                    format!("Wrote plan with {} items", res.plan.items.len())
                } else {
                    format!("Plan unchanged with {} items", res.plan.items.len())
                },
            })
        }
        Err(e) => {
            // Remove redundant recording
            Err(anyhow!("{e}"))
        }
    }
}

pub async fn plan_read(runtime: &ToolRuntime<'_>, _args: &serde_json::Value) -> Result<ToolOutput> {
    // Remove redundant session update

    match runtime.fs.plan_read() {
        Ok(res) => {
            // Remove redundant recording
            let value = serde_json::to_value(&res)?;
            Ok(ToolOutput {
                value: value.clone(),
                is_success: true,
                result_summary: format!("Read plan with {} items", res.items.len()),
            })
        }
        Err(e) => {
            // Remove redundant recording
            Err(anyhow!("{e}"))
        }
    }
}

pub async fn undo(runtime: &ToolRuntime<'_>, _args: &serde_json::Value) -> Result<ToolOutput> {
    // Remove redundant session update

    match crate::tools::undo::undo(runtime.fs).await {
        Ok(res) => {
            // Remove redundant recording
            let value = serde_json::to_value(&res)?;
            Ok(ToolOutput {
                value: value.clone(),
                is_success: true, // Undo success depends on if there was something to undo, typically yes if Ok
                result_summary: "Undid last action".to_string(),
            })
        }
        Err(e) => {
            // Remove redundant recording
            Err(anyhow!("{e}"))
        }
    }
}

pub async fn read_memory(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<ToolOutput> {
    let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");
    match runtime.fs.read_memory(key).await {
        Ok(content) => {
            let value = json!({ "content": content });
            Ok(ToolOutput {
                value: value.clone(),
                is_success: true,
                result_summary: format!("Read memory '{}'", key),
            })
        }
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn write_memory(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<ToolOutput> {
    let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let tags = args
        .get("tags")
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    let metadata = args.get("metadata").cloned();

    match runtime.fs.write_memory(key, content, tags, metadata).await {
        Ok(msg) => {
            let value = json!({ "message": msg });
            Ok(ToolOutput {
                value: value.clone(),
                is_success: true,
                result_summary: format!("Wrote memory '{}'", key),
            })
        }
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn list_memories(
    runtime: &ToolRuntime<'_>,
    _args: &serde_json::Value,
) -> Result<ToolOutput> {
    match runtime.fs.list_memories().await {
        Ok(msg) => {
            let value = json!({ "result": msg });
            Ok(ToolOutput {
                value: value.clone(),
                is_success: true,
                result_summary: "Listed memories".to_string(),
            })
        }
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn search_memory(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<ToolOutput> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let tags = args
        .get("tags")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    match runtime.fs.search_memory(query, tags).await {
        Ok(msg) => {
            let value = json!({ "result": msg });
            Ok(ToolOutput {
                value: value.clone(),
                is_success: true,
                result_summary: "Searched memories".to_string(),
            })
        }
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn run_workflow(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<ToolOutput> {
    let workflow_name = args
        .get("workflow_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("workflow_name is required"))?;

    match crate::tools::workflow::run_workflow(
        workflow_name,
        &runtime.fs.config.project_root,
        runtime.fs,
    )
    .await
    {
        Ok(output) => {
            let value = json!({ "result": output });
            Ok(ToolOutput {
                value: value.clone(),
                is_success: true,
                result_summary: format!("Ran workflow '{}'", workflow_name),
            })
        }
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn doc_generate(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<ToolOutput> {
    let args = args.as_object().ok_or_else(|| anyhow!("invalid args"))?;
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("path is required"))?;
    let symbol = args.get("symbol").and_then(|v| v.as_str());

    match runtime.fs.doc_generate(path, symbol).await {
        Ok(result) => {
            let value = json!({ "ok": true, "doc": result });
            Ok(ToolOutput {
                value: value.clone(),
                is_success: true,
                result_summary: format!("Generated docs for {}", path),
            })
        }
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn search_history(
    _runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<ToolOutput> {
    let _query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let _limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;

    let value = json!({ "result": "Search history is disabled (RAG functionality removed)" });
    Ok(ToolOutput {
        value: value.clone(),
        is_success: true,
        result_summary: "Search history disabled".to_string(),
    })
}
