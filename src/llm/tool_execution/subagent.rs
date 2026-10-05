//! Ephemeral read-only worker: measure, bound, finalize, preserve evidence.
use crate::llm::client_core::OpenAIClient;
use crate::llm::context_budget::{ContextBudgetGovernor, RequestFootprint};
use crate::llm::message_utils::truncate_tool_output;
use crate::llm::tool_execution::dispatch::dispatch_subagent_tool_call;
use crate::llm::tool_runtime::ToolRuntime;
use crate::llm::types::{ChatMessage, ToolCall, ToolDef};
use crate::tools::budget::head_truncate;
use crate::tools::task::{
    SUBAGENT_ALLOWED_TOOLS, SUBAGENT_SUMMARY_BUDGET_CHARS, subagent_system_prompt,
};
use anyhow::{Result, anyhow};
use tokio_util::sync::CancellationToken;
use tracing::debug;
mod budget;
mod evidence;
use budget::SubagentBudgetTracker;
pub use budget::{SubagentRunStatus, SubagentStopReason};
use evidence::SubagentEvidenceLedger;

pub struct SubagentRun {
    pub summary: String,
    pub files_examined: Vec<String>,
    pub files_examined_truncated: bool,
    pub iterations: usize,
    pub tool_calls: usize,
    pub status: SubagentRunStatus,
    pub stop_reason: Option<SubagentStopReason>,
}

pub async fn run_subagent(
    client: &OpenAIClient,
    model: &str,
    runtime: &ToolRuntime<'_>,
    description: &str,
    prompt: &str,
    cancel: Option<CancellationToken>,
    project_dir: &str,
) -> Result<SubagentRun> {
    let messages = vec![
        message("system", subagent_system_prompt(project_dir)),
        message(
            "user",
            format!("Task description: {description}\n\nTask instructions:\n{prompt}"),
        ),
    ];
    // Restore only the main last-request view on every returned exit path.
    // Provider session totals (including cache/reasoning) keep worker traffic.
    let _last_request = LastRequestGuard::new(client);
    run_subagent_inner(client, model, runtime, messages, cancel.unwrap_or_default()).await
}

/// The parent agent selects on cancellation around dispatch and may drop this
/// future without letting it return. Restore only last-request telemetry on Drop.
struct LastRequestGuard<'a> {
    client: &'a OpenAIClient,
    tokens: u32,
    prompt_tokens: u32,
    reasoning_tokens: u32,
    cache: crate::llm::prompt_cache::PromptCacheUsageSnapshot,
}

impl<'a> LastRequestGuard<'a> {
    fn new(client: &'a OpenAIClient) -> Self {
        Self {
            client,
            tokens: client.get_tokens_used(),
            prompt_tokens: client.get_prompt_tokens_used(),
            reasoning_tokens: client.get_reasoning_tokens_used(),
            cache: client.last_prompt_cache_usage(),
        }
    }
}

impl Drop for LastRequestGuard<'_> {
    fn drop(&mut self) {
        self.client.set_tokens(self.tokens);
        self.client.set_prompt_tokens(self.prompt_tokens);
        self.client.set_reasoning_tokens(self.reasoning_tokens);
        self.client.restore_last_prompt_cache_usage(self.cache);
    }
}

fn message(role: &str, content: String) -> ChatMessage {
    ChatMessage {
        provider_state: None,
        role: role.into(),
        content: Some(content),
        tool_calls: vec![],
        tool_call_id: None,
    }
}

fn check_cancel(token: &CancellationToken) -> Result<()> {
    if token.is_cancelled() {
        return Err(anyhow!(crate::llm::LlmErrorKind::Cancelled));
    }
    Ok(())
}

fn measure(
    governor: &ContextBudgetGovernor,
    client: &OpenAIClient,
    model: &str,
    messages: &[ChatMessage],
    tools: &[ToolDef],
) -> Result<RequestFootprint> {
    if let Some(account) = client.account_label() {
        governor.measure_subscription(
            account,
            model,
            messages,
            tools,
            0,
            client.responses_compact_threshold(),
        )
    } else {
        governor.measure(messages, tools)
    }
}

fn synthetic_results(
    messages: &mut Vec<ChatMessage>,
    calls: &[ToolCall],
    reason: SubagentStopReason,
) {
    for call in calls {
        let mut result = message("tool", serde_json::json!({"ok":false,"error":{"kind":"subagent_budget_exhausted","reason":reason,"message":"Research stopped before executing this read-only tool call."}}).to_string());
        result.tool_call_id = call.id.clone();
        messages.push(result);
    }
}

async fn run_subagent_inner(
    client: &OpenAIClient,
    model: &str,
    runtime: &ToolRuntime<'_>,
    mut messages: Vec<ChatMessage>,
    cancel: CancellationToken,
) -> Result<SubagentRun> {
    let cfg = &runtime.fs.config;
    // Use the selected worker model, the existing override table and configured
    // window. Off disables main optimization, never worker safety measurement.
    let mut model_cfg = (**cfg).clone();
    model_cfg.model = model.to_string();
    let context_limit = u64::from(model_cfg.get_effective_compaction_limit());
    let mut tracker = SubagentBudgetTracker::new(cfg.subagent.clone(), context_limit);
    debug!(
        max_iterations = cfg.subagent.max_iterations,
        max_tool_calls = cfg.subagent.max_tool_calls,
        max_elapsed_ms = cfg.subagent.max_elapsed_ms,
        effective_total_token_budget = tracker.total_limit,
        effective_context_limit = context_limit,
        "subagent resource budgets"
    );
    let mut governor = ContextBudgetGovernor::new(cfg.context_budget.clone());
    let mut ledger = SubagentEvidenceLedger::default();
    let tools = subagent_tool_defs(runtime);
    let mut reasoning = crate::llm::reasoning::ReasoningController::new(cfg.reasoning.clone());

    let mut observed_files_count = 0usize;
    let result = async {
        let reason = 'research: loop {
            check_cancel(&cancel)?;
            let footprint = measure(&governor, client, model, &messages, &tools)?;
            let estimate = governor.estimate(footprint).prompt_tokens;
            if let Some(reason) = tracker.request_stop(estimate, true) {
                break reason;
            }
            tracker.record_research_request();
            let policy = tracker.request_policy(estimate);
            let request_client = client.with_request_attempt_policy(policy.clone());
            let response = crate::llm::tool_execution::requests::chat_tools_once(
                &request_client,
                model,
                &messages,
                &tools,
                reasoning.current_effort(),
                cfg.reasoning.mode,
                Some(cancel.clone()),
                None,
            )
            .await;
            if let Some(prompt_tokens) = tracker.charge(&policy) {
                governor.observe_actual(footprint, prompt_tokens);
            }
            check_cancel(&cancel)?;
            let msg = match response {
                Ok(msg) => msg,
                Err(error) => {
                    if let Some(exceeded) = error.downcast_ref::<budget::RequestBudgetExceeded>() {
                        break exceeded.0;
                    }
                    if matches!(
                        error.downcast_ref::<crate::llm::LlmErrorKind>(),
                        Some(crate::llm::LlmErrorKind::ContextLengthExceeded)
                    ) {
                        break SubagentStopReason::ProviderContextExceeded;
                    }
                    return Err(error.context("subagent research failed"));
                }
            };
            if msg.tool_calls.is_empty() {
                return Ok(finish(
                    summarize_final(msg.content.as_deref().unwrap_or("")),
                    None,
                    &tracker,
                    ledger,
                ));
            }
            messages.push(ChatMessage {
                provider_state: msg.provider_state.clone(),
                role: "assistant".into(),
                content: msg.content.clone(),
                tool_calls: msg.tool_calls.clone(),
                tool_call_id: None,
            });
            // A tool-cap overflow skips the entire batch. No prefix execution.
            if let Some(reason) = tracker.batch_stop(msg.tool_calls.len()) {
                synthetic_results(&mut messages, &msg.tool_calls, reason);
                break reason;
            }
            let mut observations = Vec::new();
            for (index, tc) in msg.tool_calls.iter().enumerate() {
                check_cancel(&cancel)?;
                // Safe boundary only: elapsed expiry never drops an active tool.
                if let Some(reason) = tracker.elapsed_stop() {
                    synthetic_results(&mut messages, &msg.tool_calls[index..], reason);
                    break 'research reason;
                }
                let name = tc.function.name.as_str();
                if !SUBAGENT_ALLOWED_TOOLS.contains(&name) {
                    let mut output = message("tool", serde_json::json!({"error":"Only read-only tools are available to the sub-agent."}).to_string());
                    output.tool_call_id = tc.id.clone();
                    messages.push(output);
                    observations.push(crate::llm::reasoning::ToolObservation::new(name, false));
                    continue;
                }
                tracker.record_tool_call();
                let dispatch = Box::pin(dispatch_subagent_tool_call(runtime, tc));
                // Owned read workers must observe cancellation and finish before
                // returning; an outer select would drop their cleanup future.
                let result = if matches!(name, "fs_read" | "fs_read_many_files" | "search_text") {
                    dispatch.await
                } else {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => return Err(anyhow!(crate::llm::LlmErrorKind::Cancelled)),
                        result = dispatch => result,
                    }
                };
                let (success, content) = match result {
                    Ok(output) => {
                        ledger.record(name, output.is_success, &output.result_summary);
                        ledger.record_files(name, &output);
                        observed_files_count = ledger.file_count();
                        (
                            output.is_success,
                            truncate_tool_output(serde_json::to_string(&output.value)?, name),
                        )
                    }
                    Err(error) if matches!(error.downcast_ref::<crate::llm::LlmErrorKind>(), Some(crate::llm::LlmErrorKind::Cancelled)) => return Err(error),
                    Err(error) => {
                        let bounded = head_truncate(&error.to_string(), 240).text;
                        ledger.record(name, false, &bounded);
                        (
                            false,
                            truncate_tool_output(
                                serde_json::json!({"error":bounded}).to_string(),
                                name,
                            ),
                        )
                    }
                };
                observations.push(crate::llm::reasoning::ToolObservation::new(name, success));
                let mut output = message("tool", content);
                output.tool_call_id = tc.id.clone();
                messages.push(output);
            }
            reasoning.observe_tool_batch(crate::llm::reasoning::ToolBatchObservation::new(
                observations,
                false,
                false,
            ));
        };

        check_cancel(&cancel)?;
        messages.push(message("user", format!("Research resource budget has been reached ({}). Do not call tools. Return the best partial result from evidence already collected. Use exactly these sections: Facts, Files, Recommendation. Explicitly identify unknown or incomplete items.", reason.as_str())));
        let footprint = measure(&governor, client, model, &messages, &[])?;
        let estimate = governor.estimate(footprint).prompt_tokens;
        let mut summary = None;
        if tracker.request_stop(estimate, false).is_none() {
            check_cancel(&cancel)?;
            tracker.finalization_attempted = true;
            let policy = tracker.request_policy(estimate);
            let request_client = client.with_request_attempt_policy(policy.clone());
            let result = crate::llm::tool_execution::requests::chat_tools_once(
                &request_client,
                model,
                &messages,
                &[],
                reasoning.current_effort(),
                cfg.reasoning.mode,
                Some(cancel.clone()),
                None,
            )
            .await;
            tracker.charge(&policy);
            check_cancel(&cancel)?;
            match result {
                Ok(msg) => {
                    // Provider violations never trigger tool dispatch in finalization.
                    if msg.tool_calls.is_empty()
                        && msg.content.as_deref().is_some_and(|s| !s.trim().is_empty())
                    {
                        tracker.finalization_succeeded = true;
                        summary = Some(summarize_final(msg.content.as_deref().unwrap_or("")));
                    }
                }
                Err(error)
                    if matches!(
                        error.downcast_ref::<crate::llm::LlmErrorKind>(),
                        Some(crate::llm::LlmErrorKind::Cancelled)
                    ) =>
                {
                    return Err(error);
                }
                Err(_) => {
                    debug!("subagent finalization failed; preserving local evidence");
                }
            }
        }
        Ok(finish(
            summary.unwrap_or_else(|| ledger.fallback(reason)),
            Some(reason),
            &tracker,
            ledger,
        ))
    }.await;
    if let Err(error) = &result {
        let cancelled = matches!(
            error.downcast_ref::<crate::llm::LlmErrorKind>(),
            Some(crate::llm::LlmErrorKind::Cancelled)
        );
        debug!(
            status = if cancelled { "cancelled" } else { "failed" },
            stop_reason = "none",
            research_iterations = tracker.research_iterations,
            executed_tool_calls = tracker.executed_tool_calls,
            charged_tokens = tracker.charged_tokens,
            reported_usage_requests = tracker.reported_usage_requests,
            estimated_usage_requests = tracker.estimated_usage_requests,
            elapsed_ms = tracker.started_at.elapsed().as_millis(),
            files_examined_count = observed_files_count,
            finalization_attempted = tracker.finalization_attempted,
            finalization_succeeded = tracker.finalization_succeeded,
            "subagent finished"
        );
    }
    result
}

fn finish(
    summary: String,
    stop_reason: Option<SubagentStopReason>,
    tracker: &SubagentBudgetTracker,
    ledger: SubagentEvidenceLedger,
) -> SubagentRun {
    let status = if stop_reason.is_some() {
        SubagentRunStatus::Partial
    } else {
        SubagentRunStatus::Completed
    };
    debug!(
        ?status,
        ?stop_reason,
        research_iterations = tracker.research_iterations,
        executed_tool_calls = tracker.executed_tool_calls,
        charged_tokens = tracker.charged_tokens,
        reported_usage_requests = tracker.reported_usage_requests,
        estimated_usage_requests = tracker.estimated_usage_requests,
        elapsed_ms = tracker.started_at.elapsed().as_millis(),
        files_examined_count = ledger.file_count(),
        finalization_attempted = tracker.finalization_attempted,
        finalization_succeeded = tracker.finalization_succeeded,
        "subagent finished"
    );
    SubagentRun {
        summary,
        files_examined: ledger.files,
        files_examined_truncated: ledger.files_truncated,
        iterations: tracker.research_iterations,
        tool_calls: tracker.executed_tool_calls,
        status,
        stop_reason,
    }
}

fn subagent_tool_defs(runtime: &ToolRuntime<'_>) -> Vec<ToolDef> {
    runtime
        .tool_catalog
        .all_tool_defs()
        .into_iter()
        .filter(|def| SUBAGENT_ALLOWED_TOOLS.contains(&def.function.name.as_str()))
        .collect()
}

fn summarize_final(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return "Sub-agent returned an empty summary.".into();
    }
    head_truncate(trimmed, SUBAGENT_SUMMARY_BUDGET_CHARS).text
}

#[cfg(test)]
mod tests;
