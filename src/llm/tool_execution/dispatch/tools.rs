use crate::llm::tool_execution::dispatch::ToolOutput;
use crate::llm::tool_runtime::ToolRuntime;
use anyhow::{Result, anyhow};
use serde_json::json;

/// Handler for the `task` sub-agent tool. Runs an isolated read-only agent
/// loop and returns its summary.
pub async fn task(runtime: &ToolRuntime<'_>, args: &serde_json::Value) -> Result<ToolOutput> {
    let params: crate::tools::task::TaskParams = serde_json::from_value(args.clone())?;
    let client = runtime
        .subagent_client
        .as_ref()
        .ok_or_else(|| anyhow!("LLM client is not configured for the task tool"))?;
    let model = runtime.subagent_model.clone();
    let cancel = runtime.cancel_token.clone();

    let run = crate::llm::tool_execution::subagent::run_subagent(
        client,
        &model,
        runtime,
        &params.description,
        &params.prompt,
        cancel,
        &runtime.fs.config.project_root.to_string_lossy(),
    )
    .await?;

    let value = json!({
        "ok": true,
        "summary": run.summary,
        "files_examined": run.files_examined,
        "iterations": run.iterations,
        "tool_calls": run.tool_calls,
    });
    Ok(ToolOutput {
        value: value.clone(),
        is_success: true,
        result_summary: format!(
            "Sub-agent '{}' finished in {} iterations ({} tool calls)",
            params.description, run.iterations, run.tool_calls
        ),
    })
}

pub async fn execute_process(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<ToolOutput> {
    let params: crate::execution::ExecuteProcessParams = serde_json::from_value(args.clone())
        .map_err(|e| anyhow!("invalid execute_process args: {e}"))?;
    let program = params.program.clone();
    let process_args = params.args.clone();
    let cwd_param = params.cwd.clone();
    let arg_count = params.args.len();
    // Classify before execution; only structured `execute_process` is auto
    // evidence. Bash/shell strings are never inferred as verification.
    let verification_kind = crate::provenance::classify_verification(&program, &process_args);
    let verification_context = verification_kind.map(|kind| {
        crate::tools::provenance::capture_verification_context_for_invocation(
            runtime.fs,
            &runtime.attribution,
            kind,
            &program,
            &process_args,
        )
    });
    let cwd_relative = crate::tools::provenance::relative_cwd_for_evidence(runtime.fs, &cwd_param);
    match runtime
        .fs
        .execute_process(params, runtime.cancel_token.clone())
        .await
    {
        Ok(output_str) => {
            let mut value = serde_json::from_str::<serde_json::Value>(&output_str)
                .unwrap_or_else(|e| serde_json::json!({ "error": e.to_string() }));
            let success = value
                .get("success")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            // Invariant: `ok == success`.
            if let Some(obj) = value.as_object_mut() {
                obj.insert("ok".to_string(), serde_json::Value::Bool(success));
            }
            let status = value
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            // Record observed verification for completed/timed-out runs only.
            // Policy denials and spawn failures never started execution;
            // cancellation propagates as Err below and is never recorded.
            if let (Some(kind), Some(context)) = (verification_kind, verification_context)
                && (status == "completed" || status == "timed_out")
            {
                record_execute_process_verification(
                    runtime,
                    kind,
                    &program,
                    &process_args,
                    cwd_relative,
                    &context,
                    &value,
                    status,
                );
            }
            Ok(ToolOutput {
                value: value.clone(),
                is_success: success,
                result_summary: format!(
                    "Process '{}' ({} args) finished with status {}",
                    program, arg_count, status
                ),
            })
        }
        Err(error) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
fn record_execute_process_verification(
    runtime: &ToolRuntime<'_>,
    kind: crate::provenance::VerificationKind,
    program: &str,
    process_args: &[String],
    cwd_relative: Option<String>,
    context: &crate::provenance::VerificationContext,
    value: &serde_json::Value,
    status: &str,
) {
    let success = value
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let exit_code = value
        .get("exit_code")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);
    let stdout = value.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
    let stderr = value.get("stderr").and_then(|v| v.as_str()).unwrap_or("");
    let output_truncated = value
        .get("output_truncated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let timed_out = status == "timed_out";
    let mut extra_warnings = Vec::new();
    if let Some(warnings) = value.get("warnings").and_then(|v| v.as_array()) {
        for w in warnings.iter().filter_map(|v| v.as_str()) {
            extra_warnings.push(w.to_string());
        }
    }
    let event =
        crate::provenance::build_verification_event(crate::provenance::VerificationRecordInput {
            kind,
            source: crate::provenance::VerificationSource::ExecuteProcess,
            program,
            args: process_args,
            cwd_relative,
            success,
            status,
            exit_code,
            timed_out,
            stdout,
            stderr,
            capture_truncated: output_truncated,
            context: context.clone(),
            extra_warnings,
        });
    let change_count = event.observed_change_ids.len();
    let Some(ctx) = runtime.fs.current_session_storage_context() else {
        return;
    };
    let store = crate::provenance::ProvenanceStore::new(ctx.session_dir);
    match store.append(
        &ctx.session_id,
        crate::provenance::ProvenanceEvent::VerificationObserved(event),
    ) {
        Ok(_) => {
            tracing::info!(
                kind = ?kind,
                success,
                change_count,
                "provenance.verification_observed"
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, "provenance.record_failed");
            let _ = runtime.fs.mark_current_session_provenance_failure();
        }
    }
}

pub async fn execute_bash(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<ToolOutput> {
    let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
    match runtime
        .fs
        .execute_bash_with_cancel(command, runtime.cancel_token.clone())
        .await
    {
        Ok(output_str) => {
            let mut value = serde_json::from_str::<serde_json::Value>(&output_str)
                .unwrap_or_else(|e| serde_json::json!({ "error": e.to_string() }));
            let success = value
                .get("success")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            // Invariant: `ok == success` (a failing command is not `ok`).
            if let Some(obj) = value.as_object_mut() {
                obj.insert("ok".to_string(), serde_json::Value::Bool(success));
            }
            let exit_code = value.get("exit_code").and_then(|v| v.as_i64());
            Ok(ToolOutput {
                value: value.clone(),
                is_success: success,
                result_summary: format!(
                    "Command '{}' finished with exit code {:?}",
                    command, exit_code
                ),
            })
        }
        Err(error) => Err(error),
    }
}

pub async fn execute_shell(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<ToolOutput> {
    let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
    match runtime
        .fs
        .execute_shell_with_cancel(command, runtime.cancel_token.clone())
        .await
    {
        Ok(output_str) => {
            let mut value = serde_json::from_str::<serde_json::Value>(&output_str)
                .unwrap_or_else(|e| serde_json::json!({ "error": e.to_string() }));
            let success = value
                .get("success")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            // Invariant: `ok == success`.
            if let Some(obj) = value.as_object_mut() {
                obj.insert("ok".to_string(), serde_json::Value::Bool(success));
            }
            let exit_code = value.get("exit_code").and_then(|v| v.as_i64());
            Ok(ToolOutput {
                value: value.clone(),
                is_success: success,
                result_summary: format!(
                    "Shell command '{}' finished with exit code {:?}",
                    command, exit_code
                ),
            })
        }
        Err(error) => Err(error),
    }
}

pub async fn edit(runtime: &ToolRuntime<'_>, args: &serde_json::Value) -> Result<ToolOutput> {
    let params: crate::tools::edit::EditParams = serde_json::from_value(args.clone())?;

    let file_path = params.file_path.clone();

    match crate::tools::edit::edit_with_receipt(params, &runtime.fs.config).await {
        Ok(mut exec) => {
            if let Some(receipt) = exec.receipt.take() {
                let report = runtime
                    .fs
                    .finalize_mutation(
                        receipt,
                        crate::tools::FinalizeMutationOptions {
                            record_undo: true,
                            reverts_change_id: None,
                            attribution: runtime.attribution.clone(),
                        },
                    )
                    .await;
                exec.result.warnings.extend(report.warnings);
            }

            let value = serde_json::to_value(&exec.result)?;
            Ok(ToolOutput {
                value: value.clone(),
                is_success: exec.result.success,
                result_summary: if exec.result.success {
                    format!("Successfully edited {}", file_path)
                } else {
                    format!("Failed to edit {}: {:?}", file_path, exec.result.message)
                },
            })
        }
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn apply_patch(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<ToolOutput> {
    let params: crate::tools::apply_patch::ApplyPatchParams = serde_json::from_value(args.clone())?;

    match crate::tools::apply_patch::apply_patch_with_recovery_and_receipt(
        params,
        &runtime.fs.config,
    )
    .await
    {
        Ok(mut exec) => {
            if let Some(receipt) = exec.receipt.take() {
                let report = runtime
                    .fs
                    .finalize_mutation(
                        receipt,
                        crate::tools::FinalizeMutationOptions {
                            record_undo: true,
                            reverts_change_id: None,
                            attribution: runtime.attribution.clone(),
                        },
                    )
                    .await;
                exec.result.warnings.extend(report.warnings);
            }

            let value = serde_json::to_value(&exec.result)?;
            Ok(ToolOutput {
                value: value.clone(),
                is_success: exec.result.success,
                result_summary: if exec.result.success {
                    "Successfully applied patch".to_string()
                } else {
                    "Failed to apply patch".to_string()
                },
            })
        }
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn plan_write(runtime: &ToolRuntime<'_>, args: &serde_json::Value) -> Result<ToolOutput> {
    let params: crate::tools::plan::PlanWriteArgs = serde_json::from_value(args.clone())?;

    // Remove redundant session update

    let plan_items = params.items;
    match runtime
        .fs
        .plan_write_with_attribution(plan_items, params.mode, &runtime.attribution)
    {
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

pub async fn provenance_read(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<ToolOutput> {
    let params: crate::tools::provenance::ProvenanceReadArgs =
        serde_json::from_value(args.clone())?;
    match crate::tools::provenance::provenance_read(runtime.fs, params) {
        Ok(res) => {
            let value = serde_json::to_value(&res)?;
            let count = res.events.len();
            Ok(ToolOutput {
                value: value.clone(),
                is_success: true,
                result_summary: format!("Read {count} provenance events"),
            })
        }
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn requirements_write(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<ToolOutput> {
    let params: crate::tools::requirements::RequirementsWriteArgs =
        serde_json::from_value(args.clone())?;
    match crate::tools::requirements::requirements_write(runtime.fs, params, &runtime.attribution) {
        Ok(res) => {
            let value = serde_json::to_value(&res)?;
            let changed = res.changed;
            Ok(ToolOutput {
                value: value.clone(),
                is_success: true,
                result_summary: if changed {
                    format!("Wrote {} requirements", res.requirements.len())
                } else {
                    "Requirements unchanged".to_string()
                },
            })
        }
        Err(e) => {
            let err_json = serde_json::json!({ "error": e.to_string() });
            Ok(ToolOutput {
                value: err_json.clone(),
                is_success: false,
                result_summary: e.to_string(),
            })
        }
    }
}

pub async fn requirements_read(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<ToolOutput> {
    let params: crate::tools::requirements::RequirementsReadArgs =
        serde_json::from_value(args.clone())?;
    match crate::tools::requirements::requirements_read(runtime.fs, params) {
        Ok(res) => {
            let value = serde_json::to_value(&res)?;
            let count = res.requirements.len();
            Ok(ToolOutput {
                value: value.clone(),
                is_success: true,
                result_summary: format!("Read {count} requirements"),
            })
        }
        Err(e) => Err(anyhow!("{e}")),
    }
}

pub async fn undo(runtime: &ToolRuntime<'_>, _args: &serde_json::Value) -> Result<ToolOutput> {
    match crate::tools::undo::undo_with_attribution(runtime.fs, &runtime.attribution).await {
        Ok(res) => {
            let value = serde_json::to_value(&res)?;
            Ok(ToolOutput {
                value: value.clone(),
                // Empty-stack and conflict undos are failures so the LLM
                // does not mistake them for successful reverts.
                is_success: res.success,
                result_summary: if res.success {
                    format!("Undid last action: {}", res.path)
                } else if res.conflict {
                    format!("Undo conflict for {}: {}", res.path, res.message)
                } else {
                    res.message.clone()
                },
            })
        }
        Err(e) => Err(anyhow!("{e}")),
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

    match crate::tools::workflow::run_workflow_with_cancel(
        workflow_name,
        &runtime.fs.config.project_root,
        runtime.fs,
        runtime.cancel_token.clone(),
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
        Err(error) => Err(error),
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

/// Handler for the `tool_search` deferred-tool discovery tool.
///
/// Ranks catalog tools lexically, activates the matches, and returns a
/// compact summary. Full JSON schemas are never echoed here; they appear
/// only in the next LLM request's `tools` array.
pub async fn tool_search(
    runtime: &ToolRuntime<'_>,
    args: &serde_json::Value,
) -> Result<ToolOutput> {
    use crate::config::tool_routing::MAX_TOOL_SEARCH_RESULT_LIMIT;
    use crate::llm::tool_execution::ui_rendering::truncate_string_with_graphemes;
    use crate::tools::tool_search::{TOOL_SEARCH_RESULT_BUDGET_CHARS, ToolSearchParams};

    // Per-field lenient parsing: a wrongly typed `limit`/`server` falls back
    // to defaults instead of rejecting an otherwise valid query.
    let parsed = match ToolSearchParams::parse(args) {
        Ok(parsed) => parsed,
        Err(_) => {
            let value = json!({
                "ok": false,
                "error": {
                    "kind": "invalid_query",
                    "message": "tool_search query must not be empty. Describe the capability, resource, or service you need.",
                },
                "warnings": [],
            });
            return Ok(ToolOutput {
                value: value.clone(),
                is_success: false,
                result_summary: "tool_search rejected: empty query".to_string(),
            });
        }
    };
    let query = parsed.query;
    let limit = parsed
        .limit
        .map(|v| v.clamp(1, MAX_TOOL_SEARCH_RESULT_LIMIT))
        .unwrap_or_else(|| runtime.fs.config.tool_routing.effective_limit());
    let server = parsed
        .server
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let result = runtime.tool_catalog.search(&query, limit, server).await;

    let mut to_activate = Vec::new();
    let mut already_active = Vec::new();
    for hit in &result.hits {
        if runtime.tool_catalog.is_active(&hit.name).await {
            already_active.push(hit.clone());
        } else {
            to_activate.push(hit.clone());
        }
    }
    let names: Vec<String> = to_activate.iter().map(|h| h.name.clone()).collect();
    let newly = runtime.tool_catalog.activate(&names).await;
    let newly_set: std::collections::HashSet<&str> = newly.iter().map(String::as_str).collect();

    let activated: Vec<serde_json::Value> = to_activate
        .iter()
        .filter(|h| newly_set.contains(h.name.as_str()))
        .map(|h| {
            let mut item = json!({
                "name": h.name,
                "source": h.source.label(),
                "description": h.description,
            });
            if let Some(server) = &h.server {
                item["server"] = json!(server);
            }
            item
        })
        .collect();
    let already: Vec<serde_json::Value> = already_active
        .iter()
        .map(|h| {
            let mut item = json!({
                "name": h.name,
                "source": h.source.label(),
                "description": h.description,
            });
            if let Some(server) = &h.server {
                item["server"] = json!(server);
            }
            item
        })
        .collect();
    let remaining_deferred = runtime.tool_catalog.deferred_count().await;
    let mut warnings = Vec::new();
    if result.hits.is_empty() {
        warnings.push(
            "No matching tools found. Try different keywords (capability, resource, or service name).".to_string(),
        );
    }

    let value = json!({
        "ok": true,
        "query": result.query,
        "activated": activated,
        "already_active": already,
        "remaining_deferred": remaining_deferred,
        "warnings": warnings,
    });
    // Self-budget: descriptions are pre-truncated and hits are capped, so
    // the envelope stays far below the global default tier.
    debug_assert!(
        serde_json::to_string(&value)
            .map(|s| s.chars().count() <= TOOL_SEARCH_RESULT_BUDGET_CHARS)
            .unwrap_or(false),
        "tool_search result exceeded its budget"
    );
    Ok(ToolOutput {
        value: value.clone(),
        is_success: true,
        result_summary: format!(
            "tool_search '{}': activated {} tool(s), {} already active",
            // The query is model-controlled: bound it before logging.
            truncate_string_with_graphemes(query.trim(), 200),
            activated.len(),
            already.len()
        ),
    })
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
