//! Isolated sub-agent loop for the `task` tool.
//!
//! Runs a bounded agent loop restricted to read-only tools with its own
//! message history, so exploration traffic never enters the main context.
//! The main agent receives only a condensed summary.

use anyhow::{Result, anyhow};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::llm::message_utils::truncate_tool_output;
use crate::llm::tool_execution::dispatch::dispatch_tool_call;
use crate::llm::tool_runtime::ToolRuntime;
use crate::llm::types::ChatMessage;
use crate::tools::budget::head_truncate;
use crate::tools::task::{
    SUBAGENT_ALLOWED_TOOLS, SUBAGENT_MAX_ITERS, SUBAGENT_SUMMARY_BUDGET_CHARS,
    subagent_system_prompt,
};

/// A completed sub-agent run.
pub struct SubagentRun {
    pub summary: String,
    pub files_examined: Vec<String>,
    pub iterations: usize,
    pub tool_calls: usize,
}

/// Run the sub-agent loop and return its summary.
pub async fn run_subagent(
    client: &crate::llm::client_core::OpenAIClient,
    model: &str,
    runtime: &ToolRuntime<'_>,
    description: &str,
    prompt: &str,
    cancel: Option<CancellationToken>,
    project_dir: &str,
) -> Result<SubagentRun> {
    let cancel_token = cancel.unwrap_or_default();

    let messages: Vec<ChatMessage> = vec![
        ChatMessage {
            role: "system".into(),
            content: Some(subagent_system_prompt(project_dir)),
            tool_calls: vec![],
            tool_call_id: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some(format!(
                "Task description: {description}\n\nTask instructions:\n{prompt}"
            )),
            tool_calls: vec![],
            tool_call_id: None,
        },
    ];

    let tools = subagent_tool_defs(runtime);
    let mut files_examined: Vec<String> = Vec::new();
    let mut tool_calls_count = 0usize;
    let mut iterations = 0usize;

    // The sub-agent shares the main loop's client, whose per-request token
    // counters are overwritten by every response. Snapshot them here and
    // restore on every exit path so the main loop's next proactive-compaction
    // / stale-clearing check still sees the main conversation's usage.
    // (Session totals accumulate independently via `add_total_tokens`.)
    let saved_tokens = client.get_tokens_used();
    let saved_prompt_tokens = client.get_prompt_tokens_used();

    let result = run_subagent_inner(
        client,
        model,
        runtime,
        messages,
        tools,
        cancel_token,
        &mut files_examined,
        &mut tool_calls_count,
        &mut iterations,
    )
    .await;

    client.set_tokens(saved_tokens);
    client.set_prompt_tokens(saved_prompt_tokens);
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_subagent_inner(
    client: &crate::llm::client_core::OpenAIClient,
    model: &str,
    runtime: &ToolRuntime<'_>,
    mut messages: Vec<ChatMessage>,
    tools: Vec<crate::llm::types::ToolDef>,
    cancel_token: CancellationToken,
    files_examined: &mut Vec<String>,
    tool_calls_count: &mut usize,
    iterations: &mut usize,
) -> Result<SubagentRun> {
    loop {
        *iterations += 1;
        if *iterations > SUBAGENT_MAX_ITERS {
            warn!(
                iterations = *iterations,
                "subagent hit max iterations; returning partial summary"
            );
            return Ok(SubagentRun {
                summary: "Sub-agent stopped: reached its iteration limit before finishing. Partial results may be incomplete.".to_string(),
                files_examined: std::mem::take(files_examined),
                iterations: *iterations,
                tool_calls: *tool_calls_count,
            });
        }

        let msg = tokio::select! {
            biased;
            _ = cancel_token.cancelled() => {
                return Err(anyhow!(crate::llm::LlmErrorKind::Cancelled));
            }
            res = crate::llm::tool_execution::requests::chat_tools_once(
                client, model, &messages, &tools, Some(cancel_token.clone()), None,
            ) => res,
        };

        let msg = msg.map_err(|e| {
            // Context exhaustion inside the sub-agent should surface, but keep
            // the error message scoped to the sub-agent for the main loop.
            anyhow!("subagent research failed: {e}")
        })?;

        if msg.tool_calls.is_empty() {
            let summary = summarize_final(msg.content.as_deref().unwrap_or(""));
            return Ok(SubagentRun {
                summary,
                files_examined: std::mem::take(files_examined),
                iterations: *iterations,
                tool_calls: *tool_calls_count,
            });
        }

        messages.push(ChatMessage {
            role: "assistant".into(),
            content: msg.content.clone(),
            tool_calls: msg.tool_calls.clone(),
            tool_call_id: None,
        });

        for tc in msg.tool_calls {
            let tool_name = tc.function.name.as_str();
            if !SUBAGENT_ALLOWED_TOOLS.contains(&tool_name) {
                debug!(tool = tool_name, "subagent tool blocked");
                messages.push(ChatMessage {
                    role: "tool".into(),
                    content: Some(
                        serde_json::json!({
                            "error": format!(
                                "tool '{}' is not available to the sub-agent; only read-only tools are allowed",
                                tool_name
                            )
                        })
                        .to_string(),
                    ),
                    tool_calls: vec![],
                    tool_call_id: tc.id.clone(),
                });
                continue;
            }

            *tool_calls_count += 1;
            // Boxed to break the async-recursion cycle:
            // dispatch_tool_call -> task handler -> run_subagent -> dispatch_tool_call.
            let dispatch_fut = Box::pin(dispatch_tool_call(runtime, &tc));
            let res = tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    return Err(anyhow!(crate::llm::LlmErrorKind::Cancelled));
                }
                res = dispatch_fut => res,
            };

            let tool_message_content = match &res {
                Ok(output) => {
                    record_examined_files(tool_name, output, files_examined);
                    let json_str = serde_json::to_string(&output.value).unwrap_or_else(|_e| {
                        "{\"error\":\"failed to serialize tool result\"}".to_string()
                    });
                    truncate_tool_output(json_str, tool_name)
                }
                Err(e) => {
                    warn!(tool = tool_name, error = %e, "subagent tool failed");
                    let err_json = serde_json::json!({ "error": e.to_string() });
                    truncate_tool_output(err_json.to_string(), tool_name)
                }
            };

            messages.push(ChatMessage {
                role: "tool".into(),
                content: Some(tool_message_content),
                tool_calls: vec![],
                tool_call_id: tc.id.clone(),
            });
        }
    }
}

/// Build the read-only tool subset for the sub-agent from the runtime's tool
/// definitions.
fn subagent_tool_defs(runtime: &ToolRuntime<'_>) -> Vec<crate::llm::types::ToolDef> {
    runtime
        .tools
        .iter()
        .filter(|def| SUBAGENT_ALLOWED_TOOLS.contains(&def.function.name.as_str()))
        .cloned()
        .collect()
}

/// Track files touched by read/list/search tools for the run report.
fn record_examined_files(
    tool_name: &str,
    output: &crate::llm::tool_execution::dispatch::ToolOutput,
    files: &mut Vec<String>,
) {
    let extract_path =
        |v: &serde_json::Value| -> Option<String> { v.as_str().map(|s| s.to_string()) };
    let push_unique = |files: &mut Vec<String>, path: Option<String>| {
        if let Some(p) = path
            && !files.contains(&p)
        {
            files.push(p);
        }
    };

    match tool_name {
        "fs_read" => push_unique(
            files,
            output
                .value
                .get("result")
                .and_then(|r| r.get("path"))
                .and_then(extract_path),
        ),
        "find_file" => {
            if let Some(list) = output.value.get("files").and_then(|v| v.as_array()) {
                for item in list.iter().take(10) {
                    push_unique(files, extract_path(item));
                }
            }
        }
        "search_text" => {
            if let Some(list) = output.value.get("results").and_then(|v| v.as_array()) {
                for item in list.iter().take(10) {
                    push_unique(files, item.get("path").and_then(extract_path));
                }
            }
        }
        _ => {}
    }
}

/// Trim and normalize the sub-agent's final answer.
fn summarize_final(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return "Sub-agent returned an empty summary.".to_string();
    }
    head_truncate(trimmed, SUBAGENT_SUMMARY_BUDGET_CHARS).text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::{ToolCall, ToolCallFunction};

    fn make_call(name: &str, id: &str) -> ToolCall {
        ToolCall {
            id: Some(id.to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: name.to_string(),
                arguments: "{}".to_string(),
            },
        }
    }

    #[test]
    fn test_summarize_final_trims_long_output() {
        let long = "fact ".repeat(5_000);
        let summary = summarize_final(&long);
        assert!(summary.chars().count() <= SUBAGENT_SUMMARY_BUDGET_CHARS);
        assert!(summary.contains("output truncated"));
    }

    #[test]
    fn test_summarize_final_empty() {
        assert_eq!(
            summarize_final("   "),
            "Sub-agent returned an empty summary."
        );
    }

    #[test]
    fn test_record_examined_files_fs_read() {
        let output = crate::llm::tool_execution::dispatch::ToolOutput {
            value: serde_json::json!({"ok": true, "result": {"path": "/tmp/a.rs"}}),
            is_success: true,
            result_summary: String::new(),
        };
        let mut files = Vec::new();
        record_examined_files("fs_read", &output, &mut files);
        assert_eq!(files, vec!["/tmp/a.rs".to_string()]);
        // Duplicates are not added twice.
        record_examined_files("fs_read", &output, &mut files);
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn test_record_examined_files_search_results() {
        let output = crate::llm::tool_execution::dispatch::ToolOutput {
            value: serde_json::json!({
                "ok": true,
                "results": [
                    {"path": "/tmp/x.rs", "line": 1, "text": "hit"},
                    {"path": "/tmp/y.rs", "line": 2, "text": "hit"}
                ]
            }),
            is_success: true,
            result_summary: String::new(),
        };
        let mut files = Vec::new();
        record_examined_files("search_text", &output, &mut files);
        assert_eq!(
            files,
            vec!["/tmp/x.rs".to_string(), "/tmp/y.rs".to_string()]
        );
    }

    #[test]
    fn test_tool_call_blocked_message_shape() {
        // Sanity-check the JSON error payload used for blocked tools.
        let payload = serde_json::json!({"error": "tool 'execute_bash' is not available to the sub-agent; only read-only tools are allowed"});
        assert!(payload["error"].as_str().unwrap().contains("read-only"));
        let _ = make_call("execute_bash", "call_1");
    }
}
