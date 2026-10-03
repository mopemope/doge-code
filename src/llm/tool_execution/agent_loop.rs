use crate::config::ContextBudgetMode;
use crate::llm::LlmErrorKind;
use crate::llm::context_budget::{
    BudgetPressure, ContextBudgetGovernor, ReactiveRetryGuard, RequestFootprint,
    TokenEstimateSource, cleanup_threshold, should_compact_for_pressure,
    should_offload_for_pressure,
};
use crate::llm::tool_execution::error::{AgentLoopError, handle_agent_error};
use crate::llm::tool_runtime::ToolRuntime;
use crate::llm::types::{ChatMessage, ChoiceMessage};
use crate::tools::FsTools;
use crate::tools::plan::{PlanList, PlanWriteArgs};
use anyhow::{Result, anyhow};
use chrono::{DateTime, FixedOffset, Utc};
use std::hash::{Hash, Hasher};

use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use super::ui_rendering::truncate_string_with_graphemes;
use crate::llm::message_utils::truncate_tool_output;
use crate::tui::commands::prompt::build_system_prompt;

const PLAN_WRITE_NO_CHANGE_BLOCK_THRESHOLD: usize = 2;
const PLAN_WRITE_TOOL_NAME: &str = "plan_write";

/// Snapshot observations into the session before returning messages.
/// History messages flow back to the TUI/session via the return value;
/// observations persist via the session store so restarts and resumes keep
/// `obs-*` retrieval working. Failures are warnings only.
///
/// Empty snapshots are valid state changes (e.g. after observation GC) and
/// must always be written: skipping them would resurrect previously GC'd
/// entries on the next restart. No session manager means no-op via the
/// existing wrapper.
fn persist_history_and_observations(
    history: &crate::llm::tool_execution::history::HistoryManager,
    fs: &FsTools,
) -> Vec<ChatMessage> {
    let (messages, store, unseen) = history.persistable();
    if let Err(e) = fs.update_session_with_observations(store, unseen) {
        warn!(error = %e, "failed to persist observation store");
    }
    messages
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanWriteBlockReason {
    RepeatedUnchanged,
    RepeatedIdenticalArgs,
}

fn plan_write_block_ui_status(reason: PlanWriteBlockReason) -> &'static str {
    match reason {
        PlanWriteBlockReason::RepeatedUnchanged => {
            "::status:warning:Blocked repeated unchanged plan_write call."
        }
        PlanWriteBlockReason::RepeatedIdenticalArgs => {
            "::status:warning:Blocked repeated identical plan_write call."
        }
    }
}

fn plan_write_block_error(reason: PlanWriteBlockReason) -> &'static str {
    match reason {
        PlanWriteBlockReason::RepeatedUnchanged => {
            "Repeated unchanged plan_write detected and blocked. The plan has not changed in consecutive updates."
        }
        PlanWriteBlockReason::RepeatedIdenticalArgs => {
            "Repeated identical plan_write detected and blocked. Do not call plan_write again unless plan items/status actually change."
        }
    }
}

fn plan_write_block_system_message(reason: PlanWriteBlockReason) -> &'static str {
    match reason {
        PlanWriteBlockReason::RepeatedUnchanged => {
            "`plan_write` has repeatedly returned unchanged results and was blocked. Stop repeating plan-only updates; proceed with code changes, another tool, or a direct response."
        }
        PlanWriteBlockReason::RepeatedIdenticalArgs => {
            "Repeated identical `plan_write` was blocked. Continue by either answering the user directly or using a different tool with new arguments."
        }
    }
}

fn plan_write_block_counter_field(reason: PlanWriteBlockReason) -> &'static str {
    match reason {
        PlanWriteBlockReason::RepeatedUnchanged => "no_change_count",
        PlanWriteBlockReason::RepeatedIdenticalArgs => "repeat_count",
    }
}

fn build_plan_write_blocked_value(reason: PlanWriteBlockReason, count: usize) -> serde_json::Value {
    let mut blocked = serde_json::Map::new();
    blocked.insert("success".to_string(), serde_json::Value::Bool(false));
    blocked.insert(
        "error".to_string(),
        serde_json::Value::String(plan_write_block_error(reason).to_string()),
    );
    blocked.insert(
        plan_write_block_counter_field(reason).to_string(),
        serde_json::Value::Number(serde_json::Number::from(count as u64)),
    );
    serde_json::Value::Object(blocked)
}

#[allow(clippy::too_many_arguments)]
fn block_plan_write_call(
    tc: &crate::llm::types::ToolCall,
    fs: &FsTools,
    ui_tx: &Option<std::sync::mpsc::Sender<String>>,
    history: &mut crate::llm::tool_execution::history::HistoryManager,
    task_sentinel: &mut crate::analysis::TaskSentinel,
    loop_detected: &mut bool,
    reason: PlanWriteBlockReason,
    count: usize,
) {
    let reason_name = match reason {
        PlanWriteBlockReason::RepeatedUnchanged => "repeated_unchanged",
        PlanWriteBlockReason::RepeatedIdenticalArgs => "repeated_identical_args",
    };
    warn!(reason = reason_name, count, "Blocking plan_write call");

    if let Err(e) = fs.update_session_with_tool_call_count() {
        error!("Failed to update tool call count: {}", e);
    }
    if let Err(e) = fs.record_tool_call_failure(PLAN_WRITE_TOOL_NAME) {
        error!("Failed to record tool failure: {}", e);
    }

    if let Some(tx) = ui_tx {
        let _ = tx.send(plan_write_block_ui_status(reason).to_string());
    }

    let blocked = build_plan_write_blocked_value(reason, count);
    let blocked_content = truncate_tool_output(blocked.to_string(), PLAN_WRITE_TOOL_NAME);
    history.push_tool_result(tc.id.clone(), blocked_content);
    history.push(ChatMessage {
        provider_state: None,
        role: "system".into(),
        content: Some(plan_write_block_system_message(reason).to_string()),
        tool_calls: vec![],
        tool_call_id: None,
    });

    *loop_detected = true;
    task_sentinel.record_tool_call_with_progress(PLAN_WRITE_TOOL_NAME, false, Some(false));
}

fn plan_write_arguments_hash(arguments: &str) -> u64 {
    // Normalize plan_write arguments to avoid whitespace/key-order bypass.
    let normalized = serde_json::from_str::<PlanWriteArgs>(arguments)
        .ok()
        .and_then(|v| serde_json::to_string(&v).ok())
        .unwrap_or_else(|| arguments.to_string());

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    normalized.hash(&mut hasher);
    hasher.finish()
}

/// Publish the canonical plan to the TUI over the existing `::plan_list:`
/// protocol. Reads from the plan store, never from the LLM-facing tool
/// result, so compact `plan_write` output stays UI-independent.
fn publish_plan_list(tx: &std::sync::mpsc::Sender<String>, plan: &PlanList) {
    match serde_json::to_string(&plan.items) {
        Ok(plan_list_json) => {
            let _ = tx.send(format!("::plan_list:{plan_list_json}"));
        }
        Err(err) => {
            warn!(error = %err, "failed to serialize canonical plan for UI");
        }
    }
}

/// Progress override for the TaskSentinel: a successful tool call that returns
/// *new information* counts as progress, not just state changes. This prevents
/// false stall interventions during legitimate multi-file research phases,
/// while failing tools and empty searches still count as stalls. Repeated
/// identical calls remain caught by the LoopDetector.
fn json_has_non_empty_array(value: Option<&serde_json::Value>, keys: &[&str]) -> bool {
    value.is_some_and(|value| {
        keys.iter().any(|key| {
            value
                .get(key)
                .and_then(|v| v.as_array())
                .is_some_and(|a| !a.is_empty())
        })
    })
}

fn tool_call_made_progress(
    tool_name: &str,
    success: bool,
    output: Option<&serde_json::Value>,
) -> Option<bool> {
    match tool_name {
        // Reads always yield content on success (re-reads are the LoopDetector's job).
        "fs_read" | "fs_read_many_files" | "read_memory" | "list_memories" => Some(success),
        // Searches/listings are only progress when they return results.
        // search_repomap wraps the whole SearchRepomapResponse under `results`,
        // so the actual array sits at `results.results`.
        "search_text" => Some(success && json_has_non_empty_array(output, &["results"])),
        "search_repomap" => Some(
            success
                && (json_has_non_empty_array(output, &["results"])
                    || output
                        .and_then(|v| v.get("results"))
                        .is_some_and(|inner| json_has_non_empty_array(Some(inner), &["results"]))),
        ),
        // fs_list dispatch output nests the response under `result`.
        "fs_list" => Some(
            success
                && (json_has_non_empty_array(output, &["entries"])
                    || output
                        .and_then(|v| v.get("result"))
                        .is_some_and(|inner| json_has_non_empty_array(Some(inner), &["entries"]))),
        ),
        "find_file" => Some(success && json_has_non_empty_array(output, &["files"])),
        // Deferred discovery is progress only when it activates new work.
        // A repeat search that surfaces only already-active matches (or no
        // matches) must not claim progress; real activation does.
        "tool_search" => Some(success && json_has_non_empty_array(output, &["activated"])),
        "search_memory" => Some(
            success
                && output
                    .and_then(|v| v.get("result"))
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| {
                        !s.starts_with("No matching") && !s.starts_with("No memories")
                    }),
        ),
        // Everything else keeps the default write-oriented heuristic.
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_agent_loop(
    client: &crate::llm::client_core::OpenAIClient,
    model: &str,
    fs: &FsTools,
    messages: Vec<ChatMessage>,
    ui_tx: Option<std::sync::mpsc::Sender<String>>,
    cancel: Option<CancellationToken>,
    cfg: &crate::config::AppConfig,
    _tui_executor: Option<&crate::tui::commands::core::TuiExecutor>,
    attribution: crate::provenance::ProvenanceAttribution,
) -> Result<(Vec<ChatMessage>, ChoiceMessage)> {
    debug!("run_agent_loop called");
    if let Some(manager) = fs.get_session_manager_wrapper().get_session_manager() {
        let binding = match client.account_label() {
            Some(account) => format!("openai-chatgpt:{account}:{model}"),
            None => "openai-compatible".to_owned(),
        };
        crate::utils::safe_std_lock(manager, "session_manager")?.bind_inference(binding)?;
    }

    let measure = |governor: &crate::llm::context_budget::ContextBudgetGovernor,
                   messages: &[ChatMessage],
                   tools: &[crate::llm::ToolDef],
                   overlay: u64| {
        if let Some(account) = client.account_label() {
            governor.measure_subscription(account, model, messages, tools, overlay)
        } else {
            governor.measure_with_overlay(messages, tools, overlay)
        }
    };
    // Initialize HistoryManager
    let mut history = crate::llm::tool_execution::history::HistoryManager::new(
        client.clone(),
        messages,
        ui_tx.clone(),
        fs.clone(),
        cfg.clone(),
    );
    // Restore the conversation-owned Observation Store so offloaded results
    // survive shell restarts and agent resume within the same session.
    if let Some((stored, unseen)) = fs.load_current_observations()
        && (!stored.is_empty() || !unseen.is_empty())
    {
        history.restore_observations(stored, unseen);
    }

    // Inject System Prompt if not already present
    {
        // Check if there's already a system message in the history
        let has_system_prompt = history.iter().any(|m| m.role == "system");

        if !has_system_prompt {
            debug!("Injecting default system prompt");
            let system_msg = ChatMessage {
                provider_state: None,
                role: "system".into(),
                content: Some(build_system_prompt(cfg)),
                tool_calls: vec![],
                tool_call_id: None,
            };
            history.insert(0, system_msg);
        } else {
            debug!("System prompt already present, skipping injection");
        }
    }

    // Build the request-scoped runtime context once per turn. Recent-file /
    // automatic-memory hints are bootstrap-only overlays for the first LLM
    // request; they never enter durable history, session persistence, or
    // compaction input.
    let user_goal = crate::llm::runtime_context::last_user_goal(history.as_slice());
    let runtime_context = crate::llm::runtime_context::RuntimeContextBuilder::new(fs)
        .build(user_goal.as_deref())
        .await;
    let mut runtime_context_pending = !runtime_context.is_empty();
    if runtime_context_pending {
        debug!(
            runtime_context_present = true,
            runtime_context_chars = runtime_context.char_len(),
            active_context_present = runtime_context.active_context.is_some(),
            memory_context_present = runtime_context.memory_context.is_some(),
            runtime_context_truncated = runtime_context.truncated,
            "built request-scoped runtime context"
        );
    } else {
        debug!("no runtime context for this turn");
    }

    let cancel_token = cancel.unwrap_or_default();
    let mut runtime = ToolRuntime::build_with_attribution(
        fs,
        Some(client.clone()),
        model.to_string(),
        Some(cancel_token.clone()),
        attribution,
    )
    .await?;
    // Share the conversation-owned Observation Store handle so
    // `observation_read` retrieves only this run's offloads.
    runtime.set_observation_store(history.observation_handle());
    // Session resume compatibility: re-activate catalog tools referenced by
    // prior assistant tool calls so resumed history stays coherent even when
    // those tools would otherwise start deferred.
    let reactivated = runtime
        .tool_catalog
        .activate_known_from_history(history.as_slice())
        .await;
    if !reactivated.is_empty() {
        debug!(
            reactivated = reactivated.len(),
            "reactivated tools from resumed history"
        );
    }
    let mut iters = 0usize;
    let mut file_was_written = false;
    let mut loop_detector = crate::analysis::LoopDetector::new();
    let mut task_sentinel = crate::analysis::TaskSentinel::new();
    let mut reasoning_controller =
        crate::llm::reasoning::ReasoningController::new(cfg.reasoning.clone());
    let mut last_plan_write_args_hash: Option<u64> = None;
    let mut repeated_plan_write_count = 0usize;
    let mut consecutive_plan_write_no_change_count = 0usize;
    let mut budget_governor = ContextBudgetGovernor::new(cfg.context_budget.clone());
    let mut reactive_guard = ReactiveRetryGuard::new();
    // Client-side prefix-stability diagnostics (observational only).
    // Session-local; no persistence needed.
    let mut previous_prefix_signature: Option<crate::llm::prompt_cache::PromptPrefixSignature> =
        None;

    loop {
        iters += 1;
        debug!(
            iteration = iters,
            messages_len = history.len(),
            "agent loop iteration"
        );
        if iters > runtime.max_iters {
            warn!(iters, "max tool iterations reached");
            return Err(AgentLoopError::MaxIterations(iters).into());
        }

        // Fresh active-tool snapshot every iteration so `tool_search`
        // activations appear in the very next request. Activation is
        // sticky: tools are only added, never evicted mid-run. The
        // governor must measure this snapshot (not the previous usage)
        // before deciding any reduction.
        let active_tools = runtime.active_tool_defs().await;
        let reasoning_effort = reasoning_controller.current_effort();
        let reasoning_mode = cfg.reasoning.mode;
        debug!(
            reasoning_phase = reasoning_controller.current_phase().as_str(),
            reasoning_effort = reasoning_effort.map(|e| e.as_api_str()),
            reasoning_mode = reasoning_mode.as_str(),
            "reasoning decision for next request",
        );

        let effective_limit = cfg.get_effective_compaction_limit() as u64;
        let cleanup_limit = cleanup_threshold(effective_limit);
        let context_window = cfg.get_context_window_size();
        let budget_mode = cfg.context_budget.mode;

        // Footprint of the exact bytes about to be sent (after reductions).
        // Used for post-response calibration. `None` in Off mode.
        let mut sent_footprint: Option<RequestFootprint> = None;
        // Whether the final request carries the one-shot runtime overlay.
        let send_with_overlay: bool;

        if budget_mode == ContextBudgetMode::Off {
            // Legacy path only: previous-usage proactive check, no measuring.
            match history.check_and_compact_proactive().await {
                Ok(true) => {
                    reasoning_controller.observe_compaction();
                }
                Ok(false) => {}
                Err(e) => {
                    error!("Proactive compaction error: {}", e);
                }
            }
            send_with_overlay = runtime_context_pending;
            runtime_context_pending = false;
        } else if budget_mode == ContextBudgetMode::Observe {
            // Migration/debug: log the new estimate but keep legacy behavior.
            let overlay_present = runtime_context_pending;
            let overlay_bytes = if overlay_present {
                runtime_context
                    .render()
                    .map(|s| s.len() as u64)
                    .unwrap_or(0)
            } else {
                0
            };
            let observed = (|| -> anyhow::Result<(RequestFootprint, u64, BudgetPressure)> {
                let footprint = if overlay_present {
                    let projected =
                        crate::llm::runtime_context::RequestMessages::with_runtime_context(
                            history.as_slice(),
                            &runtime_context,
                        );
                    measure(
                        &budget_governor,
                        projected.as_slice(),
                        &active_tools,
                        overlay_bytes,
                    )?
                } else {
                    measure(&budget_governor, history.as_slice(), &active_tools, 0)?
                };
                let estimate = budget_governor.estimate(footprint);
                let pressure = budget_governor.classify(estimate, effective_limit);
                let tokens = estimate.prompt_tokens;
                debug!(
                    context_budget_mode = budget_mode.as_str(),
                    estimate_source = match estimate.source {
                        TokenEstimateSource::Heuristic => "heuristic",
                        TokenEstimateSource::Calibrated => "calibrated",
                        TokenEstimateSource::ProviderExact => "provider_exact",
                    },
                    message_json_bytes = footprint.message_json_bytes,
                    tool_schema_json_bytes = footprint.tool_schema_json_bytes,
                    total_json_bytes = footprint.total_json_bytes,
                    estimated_prompt_tokens = tokens,
                    cleanup_threshold = cleanup_limit,
                    effective_limit,
                    context_window,
                    active_tool_count = active_tools.len(),
                    runtime_overlay_present = overlay_present,
                    unseen_tool_results = history.unseen_count(),
                    "context budget observe (no governor action)"
                );
                Ok((footprint, tokens, pressure))
            })();
            if let Err(e) = observed {
                warn!(error = %e, "context budget observe measurement failed");
            }
            match history.check_and_compact_proactive().await {
                Ok(true) => {
                    reasoning_controller.observe_compaction();
                }
                Ok(false) => {}
                Err(e) => {
                    error!("Proactive compaction error: {}", e);
                }
            }
            send_with_overlay = runtime_context_pending;
            runtime_context_pending = false;
            // Re-measure the exact bytes about to be sent for calibration.
            let final_fp: anyhow::Result<RequestFootprint> = if send_with_overlay {
                let projected = crate::llm::runtime_context::RequestMessages::with_runtime_context(
                    history.as_slice(),
                    &runtime_context,
                );
                measure(
                    &budget_governor,
                    projected.as_slice(),
                    &active_tools,
                    overlay_bytes,
                )
            } else {
                measure(&budget_governor, history.as_slice(), &active_tools, 0)
            };
            match final_fp {
                Ok(fp) => sent_footprint = Some(fp),
                Err(e) => warn!(error = %e, "context budget observe remeasure failed"),
            }
        } else {
            // Auto: current-request preflight.
            // A. active tools already snapshotted above.
            // B/C/D. candidate projection + footprint + estimate.
            let mut use_overlay = runtime_context_pending;
            let mut overlay_bytes = if use_overlay {
                runtime_context
                    .render()
                    .map(|s| s.len() as u64)
                    .unwrap_or(0)
            } else {
                0
            };
            let measure_current = |hist: &[ChatMessage],
                                   overlay: bool,
                                   overlay_b: u64|
             -> anyhow::Result<RequestFootprint> {
                if overlay {
                    let projected =
                        crate::llm::runtime_context::RequestMessages::with_runtime_context(
                            hist,
                            &runtime_context,
                        );
                    measure(
                        &budget_governor,
                        projected.as_slice(),
                        &active_tools,
                        overlay_b,
                    )
                } else {
                    measure(&budget_governor, hist, &active_tools, 0)
                }
            };
            let initial_footprint = measure_current(history.as_slice(), use_overlay, overlay_bytes);
            let mut measurement_failed = false;
            let mut footprint = match initial_footprint {
                Ok(fp) => fp,
                Err(e) => {
                    warn!(error = %e, "context budget measurement failed; sending best-effort");
                    measurement_failed = true;
                    // Dummy footprint keeps types; reductions are skipped and
                    // the shared send path below delivers best-effort.
                    RequestFootprint::new(0, 0, 0)
                }
            };
            if !measurement_failed {
                let mut estimate = budget_governor.estimate(footprint);
                let mut pressure = budget_governor.classify(estimate, effective_limit);
                debug!(
                    context_budget_mode = budget_mode.as_str(),
                    estimate_source = match estimate.source {
                        TokenEstimateSource::Heuristic => "heuristic",
                        TokenEstimateSource::Calibrated => "calibrated",
                        TokenEstimateSource::ProviderExact => "provider_exact",
                    },
                    message_json_bytes = footprint.message_json_bytes,
                    tool_schema_json_bytes = footprint.tool_schema_json_bytes,
                    total_json_bytes = footprint.total_json_bytes,
                    estimated_prompt_tokens = estimate.prompt_tokens,
                    cleanup_threshold = cleanup_limit,
                    effective_limit,
                    context_window,
                    active_tool_count = active_tools.len(),
                    runtime_overlay_present = use_overlay,
                    unseen_tool_results = history.unseen_count(),
                    "context budget preflight initial"
                );

                // E. Runtime overlay drop first (cache-aware ordering). A dropped
                // overlay is never deferred to the next iteration (one-shot).
                if !matches!(pressure, BudgetPressure::Healthy) && use_overlay {
                    use_overlay = false;
                    overlay_bytes = 0;
                    runtime_context_pending = false;
                    match measure_current(history.as_slice(), false, 0) {
                        Ok(fp) => {
                            let before = estimate.prompt_tokens;
                            footprint = fp;
                            estimate = budget_governor.estimate(footprint);
                            pressure = budget_governor.classify(estimate, effective_limit);
                            debug!(
                                budget_action = "drop_runtime_overlay",
                                before_estimate = before,
                                estimated_prompt_tokens = estimate.prompt_tokens,
                                total_json_bytes = footprint.total_json_bytes,
                                "dropped runtime overlay for budget pressure"
                            );
                        }
                        Err(e) => warn!(error = %e, "remeasure after overlay drop failed"),
                    }
                } else if use_overlay {
                    // Healthy with overlay: consume the one-shot flag but keep
                    // the overlay for this request.
                    runtime_context_pending = false;
                } else {
                    // No overlay pending; ensure the flag stays consumed.
                    runtime_context_pending = false;
                }

                // F/G. Recoverable Observation Store offload, then remeasure.
                // Never uses the lossy `clear_stale_tool_results` path.
                if should_offload_for_pressure(pressure) {
                    let report = history.offload_stale_tool_results_for_pressure(3);
                    if report.recoverable_offloads > 0
                        || report.fallback_elisions > 0
                        || report.skipped_unseen > 0
                    {
                        debug!(
                            budget_action = "observation_offload",
                            recoverable_offloads = report.recoverable_offloads,
                            fallback_elisions = report.fallback_elisions,
                            skipped_unseen = report.skipped_unseen,
                            reclaimed_json_bytes = report.reclaimed_bytes,
                            "preflight observation offload"
                        );
                        if let Some(tx) = &ui_tx
                            && (report.recoverable_offloads > 0 || report.fallback_elisions > 0)
                        {
                            let _ = tx.send(format!(
                            "::status:waiting:Offloaded {} tool result(s) ({} recoverable) to free context...",
                            report.recoverable_offloads + report.fallback_elisions,
                            report.recoverable_offloads
                        ));
                        }
                    }
                    match measure_current(history.as_slice(), use_overlay, overlay_bytes) {
                        Ok(fp) => {
                            footprint = fp;
                            estimate = budget_governor.estimate(footprint);
                            pressure = budget_governor.classify(estimate, effective_limit);
                            debug!(
                                estimated_prompt_tokens = estimate.prompt_tokens,
                                total_json_bytes = footprint.total_json_bytes,
                                "context budget post-offload remeasure"
                            );
                        }
                        Err(e) => warn!(error = %e, "remeasure after offload failed"),
                    }
                }

                // H/I. Calibrated-only proactive compaction (last resort).
                // Heuristic-only Compact never compacts: best-effort send to
                // avoid false-positive LLM compaction and cache destruction.
                if should_compact_for_pressure(pressure, estimate.source) {
                    match history.compact_for_budget_pressure().await {
                        Ok(true) => {
                            reasoning_controller.observe_compaction();
                            match measure_current(history.as_slice(), use_overlay, overlay_bytes) {
                                Ok(fp) => {
                                    footprint = fp;
                                    estimate = budget_governor.estimate(footprint);
                                    pressure = budget_governor.classify(estimate, effective_limit);
                                    debug!(
                                        budget_action = "compact",
                                        estimated_prompt_tokens = estimate.prompt_tokens,
                                        total_json_bytes = footprint.total_json_bytes,
                                        "context budget post-compact remeasure"
                                    );
                                }
                                Err(e) => warn!(error = %e, "remeasure after compact failed"),
                            }
                        }
                        Ok(false) => {
                            debug!(
                                budget_action = "compact_skipped",
                                "preflight compaction made no progress; sending best-effort"
                            );
                        }
                        Err(e) => {
                            error!("Preflight compaction error: {}", e);
                        }
                    }
                } else if matches!(pressure, BudgetPressure::Compact) {
                    debug!(
                        budget_action = "compact_deferred_heuristic",
                        "heuristic-only Compact: skipping compaction, best-effort send"
                    );
                }

                debug!(
                    context_budget_mode = budget_mode.as_str(),
                    estimate_source = match estimate.source {
                        TokenEstimateSource::Heuristic => "heuristic",
                        TokenEstimateSource::Calibrated => "calibrated",
                        TokenEstimateSource::ProviderExact => "provider_exact",
                    },
                    message_json_bytes = footprint.message_json_bytes,
                    tool_schema_json_bytes = footprint.tool_schema_json_bytes,
                    total_json_bytes = footprint.total_json_bytes,
                    estimated_prompt_tokens = estimate.prompt_tokens,
                    cleanup_threshold = cleanup_limit,
                    effective_limit,
                    context_window,
                    active_tool_count = active_tools.len(),
                    runtime_overlay_present = use_overlay,
                    unseen_tool_results = history.unseen_count(),
                    "context budget preflight final"
                );
            }
            if measurement_failed {
                runtime_context_pending = false;
                send_with_overlay = use_overlay;
                // sent_footprint stays None (no calibration without measurement).
            } else {
                send_with_overlay = use_overlay;
                sent_footprint = Some(footprint);
            }
        }

        // Prefix-stability diagnostics: fingerprint the exact cache-relevant
        // components about to be sent (final active tools + canonical
        // history + effort + model). Computed only when DEBUG is enabled;
        // cache token counters themselves are always recorded via
        // `record_usage`. The leading-system hash excludes the one-shot
        // `<RuntimeContext>` overlay by construction.
        let current_prefix_signature: Option<crate::llm::prompt_cache::PromptPrefixSignature> =
            if tracing::enabled!(tracing::Level::DEBUG) {
                Some(crate::llm::prompt_cache::compute_prefix_signature(
                    model,
                    history.as_slice(),
                    &active_tools,
                    reasoning_effort,
                ))
            } else {
                None
            };

        // J. Send (best-effort; provider is the final authority).
        // First logical request carries the runtime overlay; the pending flag
        // was consumed during preflight so retries, reactive compaction, and
        // JSON correction never re-inject it.
        let chat_result = {
            let request_messages = if send_with_overlay {
                crate::llm::runtime_context::RequestMessages::with_runtime_context(
                    history.as_slice(),
                    &runtime_context,
                )
            } else {
                crate::llm::runtime_context::RequestMessages::borrowed(history.as_slice())
            };
            tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    warn!("run_agent_loop cancelled before chat_tools_once");
                    Err(anyhow!(LlmErrorKind::Cancelled))
                }
                res = crate::llm::tool_execution::requests::chat_tools_once(
                    client,
                    model,
                    request_messages.as_slice(),
                    &active_tools,
                    reasoning_effort,
                    reasoning_mode,
                    Some(cancel_token.clone()),
                    ui_tx.clone(),
                ) => res,
            }
        };
        let msg = match chat_result {
            Ok(msg) => msg,
            Err(e) => {
                // Check if the error is due to context length exceeded.
                // Unseen-safe reactive compaction, at most one retry per
                // logical request (guard resets on success).
                if let Some(LlmErrorKind::ContextLengthExceeded) = e.downcast_ref::<LlmErrorKind>()
                {
                    if !reactive_guard.should_attempt() {
                        error!("context length exceeded after reactive compaction; not retrying");
                    } else {
                        match history.compact_reactive().await {
                            Ok(true) => {
                                info!("History compaction successful (reactive). Resuming.");
                                reasoning_controller.observe_compaction();
                                reactive_guard.record_attempt();
                                continue;
                            }
                            Ok(false) => {
                                // No progress (e.g. huge protected unseen
                                // suffix): retrying the same bytes is futile.
                            }
                            Err(compact_err) => {
                                error!("Error during reactive history compaction: {}", compact_err);
                            }
                        }
                    }
                }

                if let Some(LlmErrorKind::Deserialize) = e.downcast_ref::<LlmErrorKind>() {
                    warn!("JSON parse error from LLM: {}", e);
                    reasoning_controller.observe_json_recovery();
                    let feedback = format!(
                        "Error: Invalid JSON format in your response: {}. Please correct your output to be valid JSON. Ensure you are not using markdown code blocks for the entire response if it's not required by the tool.",
                        e
                    );
                    history.push(ChatMessage {
                        provider_state: None,
                        role: "user".into(),
                        content: Some(feedback),
                        tool_calls: vec![],
                        tool_call_id: None,
                    });
                    if let Some(tx) = &ui_tx {
                        let _ = tx.send(
                            "::status:warning:Invalid JSON received. Requesting correction..."
                                .to_string(),
                        );
                    }
                    continue;
                }

                let agent_error = AgentLoopError::Llm(e.to_string());
                handle_agent_error(&agent_error, &ui_tx);
                return Err(agent_error.into());
            }
        };
        // Calibrate before the sub-agent can overwrite the shared
        // per-request counter, then mark results seen. Failed requests above
        // never reach here so unseen results stay inline.
        if let Some(fp) = sent_footprint {
            budget_governor.observe_actual(fp, client.get_prompt_tokens_used());
        }
        reactive_guard.record_success();
        // A complete model response means every tool result in the request
        // was successfully consumed. Mark them seen before pushing the new
        // assistant/tool messages; network errors above never reach here so
        // failed requests keep their results inline.
        history.mark_sent_tool_results_seen();

        // Prompt-cache telemetry + prefix-stability diagnostics (DEBUG only).
        // Token counters were already recorded via `record_usage` on every
        // path; this only logs content-free fingerprints, counts, flags,
        // and ratios. Never log raw prompt / tool / user content here.
        // Causation is never asserted: a schema/effort change alongside a
        // cache miss is correlation for investigation, not proof.
        if let Some(current) = current_prefix_signature.as_ref() {
            let change = current.diff(previous_prefix_signature.as_ref());
            let last_cache = client.last_prompt_cache_usage();
            let last_ratio = client.last_prompt_cache_hit_ratio();
            let session_ratio = client.prompt_cache_hit_ratio();
            if last_cache.cached_tokens.is_some() || last_cache.cache_write_tokens.is_some() {
                debug!(
                    prompt_tokens = client.get_prompt_tokens_used(),
                    cached_tokens = last_cache.cached_tokens.unwrap_or(0),
                    cached_reported = last_cache.cached_tokens.is_some(),
                    cache_write_tokens = last_cache.cache_write_tokens.unwrap_or(0),
                    cache_write_reported = last_cache.cache_write_tokens.is_some(),
                    last_cache_hit_ratio = ?last_ratio,
                    session_cache_hit_ratio = ?session_ratio,
                    tool_count = current.tool_count,
                    tool_schema_hash = %current.short_tool_hash(),
                    system_prefix_hash = %current.short_system_hash(),
                    tool_schema_changed = change.tool_schema_changed,
                    leading_system_changed = change.leading_system_changed,
                    reasoning_effort_changed = change.reasoning_effort_changed,
                    model_changed = change.model_changed,
                    "prompt cache telemetry with prefix diagnostics",
                );
            } else {
                debug!(
                    prompt_tokens = client.get_prompt_tokens_used(),
                    tool_count = current.tool_count,
                    tool_schema_hash = %current.short_tool_hash(),
                    system_prefix_hash = %current.short_system_hash(),
                    tool_schema_changed = change.tool_schema_changed,
                    leading_system_changed = change.leading_system_changed,
                    reasoning_effort_changed = change.reasoning_effort_changed,
                    model_changed = change.model_changed,
                    "prompt cache telemetry: no cached tokens reported for this request; client-side cache-relevant prefix components changed",
                );
            }
            previous_prefix_signature = Some(current.clone());
        }

        // If assistant returned final content without tool calls, we are done.
        if msg.tool_calls.is_empty() {
            // Send final assistant content to UI (if present)
            if let Some(content) = &msg.content
                && !content.is_empty()
                && let Some(tx) = &ui_tx
            {
                debug!(response_content = ?content, "Sending LLM response content (final).");
                let _ = tx.send(format!("::status:done:{}", content));
            }

            history.push(ChatMessage {
                provider_state: msg.provider_state.clone(),
                role: "assistant".into(),
                content: msg.content.clone(),
                tool_calls: msg.tool_calls.clone(),
                tool_call_id: None,
            });

            // If files were written during tool execution, compute and send git diff
            if cfg.show_diff
                && file_was_written
                && let Some(tx) = &ui_tx
            {
                // Scope the diff to files the agent modified in this session so
                // unrelated uncommitted work in the worktree is not shown/reverted.
                // Session paths are project-root relative, matching the cwd of the
                // git commands run inside collect_diff_review_payload.
                let filter_paths = fs.get_session_changed_files();
                match crate::llm::tool_execution::collect_diff_review_payload(
                    &cfg.project_root,
                    &filter_paths,
                )
                .await
                {
                    Ok(Some(payload)) => {
                        let enriched =
                            crate::tools::provenance::enrich_diff_review_with_evidence(fs, payload);
                        match serde_json::to_string(&enriched) {
                            Ok(json) => {
                                let _ = tx.send(format!("::diff_review:{}", json));
                            }
                            Err(e) => {
                                let agent_error = AgentLoopError::Serialization(e.to_string());
                                handle_agent_error(&agent_error, &ui_tx);
                                let _ = tx.send(format!(
                                    "::diff_review:{}",
                                    serde_json::json!({
                                        "error": format!("Failed to serialize diff review payload: {}", e)
                                    })
                                ));
                            }
                        }
                    }
                    Ok(None) => {
                        debug!("No diff detected after tool execution");
                    }
                    Err(e) => {
                        let agent_error = AgentLoopError::DiffCollection(e.to_string());
                        handle_agent_error(&agent_error, &ui_tx);
                        let _ = tx.send(format!(
                            "::diff_review:{}",
                            serde_json::json!({
                                "error": format!("Failed to collect diff review payload: {}", e)
                            })
                        ));
                    }
                }
            }

            return Ok((
                persist_history_and_observations(&history, fs),
                ChoiceMessage {
                    role: "assistant".into(),
                    content: msg.content.clone().unwrap_or_default(),
                },
            ));
        }

        // There are tool calls to process. Send intermediate content if available.
        if let Some(content) = &msg.content
            && !content.is_empty()
            && let Some(tx) = &ui_tx
        {
            debug!(response_content = ?content, "Sending intermediate LLM response content.");
            let _ = tx.send(content.clone());
        }

        history.push(ChatMessage {
            provider_state: msg.provider_state.clone(),
            role: "assistant".into(),
            content: msg.content.clone(),
            tool_calls: msg.tool_calls.clone(),
            tool_call_id: None,
        });

        let mut loop_detected = false;
        let mut batch_observations: Vec<crate::llm::reasoning::ToolObservation> = Vec::new();
        let mut batch_stall_detected = false;
        for tc in msg.tool_calls {
            if loop_detected {
                // Skip remaining tool calls in the batch
                debug!(tool = %tc.function.name, "Skipping tool call due to loop detection in same batch");
                history.push_tool_result(
                    tc.id.clone(),
                    "{\"error\":\"Loop detected in current tool batch. Execution skipped to allow for immediate reassessment.\"}".to_string(),
                );
                continue;
            }

            // Always send processing status to UI if available
            if let Some(tx) = &ui_tx {
                let _ = tx.send("::status:processing".into());
            }

            let tool_name = tc.function.name.as_str();
            if tool_name == "plan_write" {
                if consecutive_plan_write_no_change_count >= PLAN_WRITE_NO_CHANGE_BLOCK_THRESHOLD {
                    block_plan_write_call(
                        &tc,
                        fs,
                        &ui_tx,
                        &mut history,
                        &mut task_sentinel,
                        &mut loop_detected,
                        PlanWriteBlockReason::RepeatedUnchanged,
                        consecutive_plan_write_no_change_count,
                    );
                    batch_observations.push(crate::llm::reasoning::ToolObservation::new(
                        PLAN_WRITE_TOOL_NAME,
                        false,
                    ));
                    continue;
                }

                let args_hash = plan_write_arguments_hash(&tc.function.arguments);

                if last_plan_write_args_hash == Some(args_hash) {
                    repeated_plan_write_count += 1;
                } else {
                    repeated_plan_write_count = 1;
                    last_plan_write_args_hash = Some(args_hash);
                }

                // Hard-stop repeated identical plan_write arguments to break infinite A->A loops.
                if repeated_plan_write_count >= 3 {
                    block_plan_write_call(
                        &tc,
                        fs,
                        &ui_tx,
                        &mut history,
                        &mut task_sentinel,
                        &mut loop_detected,
                        PlanWriteBlockReason::RepeatedIdenticalArgs,
                        repeated_plan_write_count,
                    );
                    batch_observations.push(crate::llm::reasoning::ToolObservation::new(
                        PLAN_WRITE_TOOL_NAME,
                        false,
                    ));
                    continue;
                }
            } else {
                repeated_plan_write_count = 0;
                last_plan_write_args_hash = None;
                consecutive_plan_write_no_change_count = 0;
            }

            let res = tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    warn!("run_agent_loop cancelled before dispatch_tool_call");
                    return Err(anyhow!(LlmErrorKind::Cancelled));
                }
                res = crate::llm::tool_execution::dispatch::dispatch_tool_call(&runtime, &tc) => res,
            };

            // Extract success status and result summary from the structured output
            let (success, result_summary, output_value) = match &res {
                Ok(output) => (
                    output.is_success,
                    output.result_summary.clone(),
                    Some(&output.value),
                ),
                Err(e) => (
                    false,
                    truncate_string_with_graphemes(&e.to_string(), 200),
                    None,
                ),
            };
            batch_observations.push(crate::llm::reasoning::ToolObservation::new(
                tool_name, success,
            ));
            let plan_write_changed = if tool_name == "plan_write" {
                output_value.and_then(|value| value.get("changed").and_then(|v| v.as_bool()))
            } else {
                None
            };

            if tool_name == "plan_write" {
                if success && plan_write_changed == Some(false) {
                    consecutive_plan_write_no_change_count += 1;
                } else {
                    consecutive_plan_write_no_change_count = 0;
                }
            }

            // Centralized Session Recording
            if let Err(e) = fs.update_session_with_tool_call_count() {
                error!("Failed to update tool call count: {}", e);
            }
            if success {
                if let Err(e) = fs.record_tool_call_success(tool_name) {
                    error!("Failed to record tool success: {}", e);
                }
            } else if let Err(e) = fs.record_tool_call_failure(tool_name) {
                error!("Failed to record tool failure: {}", e);
            }

            let modifies_files = matches!(tool_name, "fs_write" | "edit" | "apply_patch" | "undo");

            // Actual-mutation flag: only `success && changed` counts for diff
            // review and verification notes. No-op writes never trigger them.
            let output_changed = output_value
                .and_then(|v| v.get("changed"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let actually_changed = success && output_changed;

            let ui_args = if success
                && ui_tx.is_some()
                && matches!(
                    tool_name,
                    "fs_read"
                        | "edit"
                        | "fs_list"
                        | "search_text"
                        | "search_repomap"
                        | "execute_bash"
                        | "execute_process"
                        | "execute_shell"
                ) {
                serde_json::from_str::<serde_json::Value>(&tc.function.arguments).ok()
            } else {
                None
            };

            // Set file_was_written flag for tools that actually mutated files
            if modifies_files && actually_changed {
                file_was_written = true;
            }

            // Build tool message content (full JSON) for feeding back to the LLM
            let mut tool_message_content = match &res {
                Ok(output) => {
                    let json_str = serde_json::to_string(&output.value).unwrap_or_else(|_e| {
                        "{\"error\":\"failed to serialize tool result\"}".to_string()
                    });
                    truncate_tool_output(json_str, tool_name)
                }
                Err(e) => {
                    error!(error = %e, "tool execution failed");
                    let err_json = serde_json::json!({ "error": e.to_string() });
                    let json_str = serde_json::to_string(&err_json).unwrap_or_else(|_e| {
                        "{\"error\":\"failed to serialize error\"}".to_string()
                    });
                    truncate_tool_output(json_str, tool_name)
                }
            };

            // Inject verification note if a file was actually mutated
            if modifies_files && actually_changed {
                let verification_note = r#"

<SYSTEM_NOTE>
File modification detected. You MUST now verify your changes:

1. Read the file to confirm the content is correct.
2. Run tests to ensure no regressions.
</SYSTEM_NOTE>"#;
                tool_message_content.push_str(verification_note);
            }

            // Prepare a short result summary for UI log and truncate if necessary

            // Send a more visually appealing multi-line tool execution display
            if let Some(tx) = &ui_tx {
                let status_text = if success { "✅ SUCCESS" } else { "❌ FAILED" };

                // Map tool names to appropriate icons
                let tool_icon = match tool_name {
                    "fs_list" => "🗂️",
                    "fs_read" => "📖",
                    "fs_read_many_files" => "📚",
                    "fs_write" => "📝",
                    "search_text" => "🔍",
                    "execute_bash" => "🔧",
                    "execute_process" => "⚙️",
                    "execute_shell" => "🐚",
                    "find_file" => "📁",
                    "search_repomap" => "🗺️",
                    "edit" => "✏️",
                    "apply_patch" => "🧩",
                    "plan_write" => "🗂️",
                    "plan_read" => "🗂️",
                    _ => "🔧", // default icon
                };

                let start_time = std::time::SystemTime::now();
                let utc_datetime: DateTime<Utc> = start_time.into();
                let jst_offset =
                    FixedOffset::east_opt(9 * 3600).unwrap_or(FixedOffset::east_opt(0).unwrap()); // JST is UTC+9, fallback to UTC
                let jst_datetime = utc_datetime.with_timezone(&jst_offset);
                let timestamp_short = jst_datetime.format("%H:%M:%S").to_string(); // HH:MM:SS format in JST

                // Send indented lines to create a visually distinct tool execution display
                let header_line =
                    format!("🛠️  [{timestamp_short}] {tool_icon} {tool_name} => {status_text}");
                let _ = tx.send(header_line);

                // For fs_read, show the file path right after SUCCESS
                if tool_name == "fs_read"
                    && success
                    && let Some(args) = ui_args.as_ref()
                    && let Some(path) = args.get("path").and_then(|v| v.as_str())
                {
                    let _ = tx.send(path.to_string());
                }

                // For edit, show the file path right after SUCCESS
                if tool_name == "edit"
                    && success
                    && let Some(args) = ui_args.as_ref()
                    && let Some(path) = args.get("file_path").and_then(|v| v.as_str())
                {
                    let _ = tx.send(path.to_string());
                }

                // For fs_list, show the directory path right after SUCCESS
                if tool_name == "fs_list"
                    && success
                    && let Some(args) = ui_args.as_ref()
                    && let Some(path) = args.get("path").and_then(|v| v.as_str())
                {
                    let _ = tx.send(path.to_string());
                }

                // For search_text, show the search keyword right after SUCCESS
                if tool_name == "search_text"
                    && success
                    && let Some(args) = ui_args.as_ref()
                    && let Some(keyword) = args.get("search_pattern").and_then(|v| v.as_str())
                {
                    let _ = tx.send(format!("Keyword: {}", keyword));
                }

                // For search_repomap, show the search keywords right after SUCCESS
                if tool_name == "search_repomap"
                    && success
                    && let Some(args) = ui_args.as_ref()
                {
                    // Check keyword_search field
                    if let Some(keyword_search) =
                        args.get("keyword_search").and_then(|v| v.as_array())
                        && !keyword_search.is_empty()
                    {
                        let keywords: Vec<String> = keyword_search
                            .iter()
                            .filter_map(|v| v.as_str())
                            .map(|s| s.to_string())
                            .collect();
                        if !keywords.is_empty() {
                            let _ = tx.send(format!("Keywords: {}", keywords.join(", ")));
                        }
                    }
                }

                // For execute_bash/execute_shell, show the command that was executed right after SUCCESS
                if (tool_name == "execute_bash" || tool_name == "execute_shell")
                    && success
                    && let Some(args) = ui_args.as_ref()
                    && let Some(command) = args.get("command").and_then(|v| v.as_str())
                {
                    let _ = tx.send(format!("Command: {}", command));
                }

                // For execute_process, show program + args without building a
                // shell string (and never show env values).
                if tool_name == "execute_process"
                    && success
                    && let Some(args) = ui_args.as_ref()
                {
                    let program = args.get("program").and_then(|v| v.as_str()).unwrap_or("?");
                    let arg_count = args
                        .get("args")
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    let preview: Vec<String> = args
                        .get("args")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .take(5)
                                .filter_map(|v| v.as_str())
                                .map(|s| {
                                    // Char-based truncation: byte slicing (`&s[..60]`)
                                    // panics on multi-byte UTF-8 boundaries.
                                    if s.chars().count() > 60 {
                                        format!("{}…", s.chars().take(60).collect::<String>())
                                    } else {
                                        s.to_string()
                                    }
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    if preview.is_empty() {
                        let _ = tx.send(format!("Process: {} ({} args)", program, arg_count));
                    } else {
                        let _ = tx.send(format!(
                            "Process: {} Args: [{}]{}",
                            program,
                            preview.join(", "),
                            if arg_count > preview.len() {
                                format!(" (+{} more)", arg_count - preview.len())
                            } else {
                                String::new()
                            }
                        ));
                    }
                }

                // If failed, try to show the error message in the TUI log
                if !success {
                    let _ = tx.send(format!("    Error: {}", result_summary));
                }

                // Tool arguments and results are intentionally not displayed in the TUI to avoid leaking sensitive data.

                let _ = tx.send("".to_string()); // Extra blank line for spacing
            }

            // Also emit structured debug/error logs (include truncated result summary for debugging)
            match &res {
                Ok(_) if success => {
                    debug!("[tool] {} succeeded: {}", tc.function.name, result_summary)
                }
                Ok(_) => warn!(
                    "[tool] {} reported failure: {}",
                    tc.function.name, result_summary
                ),
                Err(e) => error!("[tool] {} failed: {}", tc.function.name, e),
            }

            // Inform the UI that tool processing is complete and we are waiting for the LLM
            if let Some(tx) = &ui_tx {
                let _ = tx.send("::status:waiting".into());
            }

            // Refresh the TUI plan projection from the canonical store after a
            // successful plan_write/plan_read. The LLM-facing ToolOutput shape
            // is intentionally not the source: compact plan_write results carry
            // no plan items. A UI refresh failure must never fail the write.
            if success
                && matches!(tc.function.name.as_str(), "plan_write" | "plan_read")
                && ui_tx.is_some()
            {
                match fs.plan_read() {
                    Ok(plan_list) => {
                        debug!(tool = %tc.function.name, "Updated plan list from canonical store");
                        if let Some(tx) = &ui_tx {
                            publish_plan_list(tx, &plan_list);
                        }
                    }
                    Err(err) => {
                        warn!(error = %err, tool = %tc.function.name, "plan UI refresh failed; write remains successful");
                    }
                }
            }

            // tool message to feed back to the LLM
            history.push_tool_result(tc.id.clone(), tool_message_content);

            // Loop Detection
            if tool_name == "plan_write" && success && plan_write_changed == Some(false) {
                loop_detector.record_plan_write_no_change();
            } else {
                loop_detector.record_tool_call(&tc);
            }
            if let Some(loop_type) = loop_detector.detect_loop() {
                let warning_msg = loop_detector.loop_warning(&loop_type);

                warn!("Loop detected: {}", warning_msg);
                if let Some(tx) = &ui_tx {
                    let _ = tx.send("::status:warning:Loop detected. Intervening...".to_string());
                }

                history.push(ChatMessage {
                    provider_state: None,
                    role: "system".into(), // Escalated to system role
                    content: Some(warning_msg),
                    tool_calls: vec![],
                    tool_call_id: None,
                });
                loop_detected = true;
            }

            // Task Sentinel (Stalled Progress Check)
            // Use the authoritative success flag
            if tool_name == "plan_write" && success && plan_write_changed == Some(false) {
                task_sentinel.record_tool_call_with_progress(
                    &tc.function.name,
                    success,
                    Some(false),
                );
            } else {
                let progress_override = tool_call_made_progress(tool_name, success, output_value);
                task_sentinel.record_tool_call_with_progress(
                    &tc.function.name,
                    success,
                    progress_override,
                );
            }
            if let Some(stall_warning) = task_sentinel.check_stalled() {
                batch_stall_detected = true;
                warn!("Stalled progress detected: {}", stall_warning);
                if let Some(tx) = &ui_tx {
                    let _ =
                        tx.send("::status:warning:Progress stalled. Intervening...".to_string());
                }
                history.push(ChatMessage {
                    provider_state: None,
                    role: "user".into(),
                    content: Some(stall_warning),
                    tool_calls: vec![],
                    tool_call_id: None,
                });
            }

            // Specific Error Recovery Hints
            if !success {
                // Try to look into the output value for an "error" field if it exists, or use result_summary
                let err_str = if let Some(val) = output_value
                    && let Some(err_field) = val.get("error").and_then(|v| v.as_str())
                {
                    err_field.to_string()
                } else {
                    result_summary.clone()
                };

                if let Some(hint) = crate::llm::tool_execution::error::get_error_hint(&err_str) {
                    history.push(ChatMessage {
                        provider_state: None,
                        role: "user".into(),
                        content: Some(hint.to_string()),
                        tool_calls: vec![],
                        tool_call_id: None,
                    });
                }
            }
        }
        history.checkpoint_subscription()?;
        // Aggregate the finished batch once (order-independent): the heaviest
        // phase wins, so a single failure escalates the next request to
        // Recovery while a clean batch decays back to Routine/Deliberative.
        reasoning_controller.observe_tool_batch(crate::llm::reasoning::ToolBatchObservation::new(
            batch_observations,
            loop_detected,
            batch_stall_detected,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_runtime_context_sent_only_on_first_request() {
        use httptest::{Expectation, ServerBuilder, matchers::*, responders::*};

        if std::env::var("DOGE_SKIP_HTTPTEST").is_ok() {
            eprintln!("Skipping httptest-based test (DOGE_SKIP_HTTPTEST set)");
            return;
        }
        let server = match ServerBuilder::new().run() {
            Ok(server) => server,
            Err(err) => {
                eprintln!("Skipping httptest-based test (server start failed: {err})");
                return;
            }
        };

        // First response: a real tool call; second response: final answer.
        let tool_call_response = serde_json::json!({
            "id": "test-tool",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "search_memory",
                            "arguments": "{\"query\":\"cache\"}"
                        }
                    }]
                }
            }]
        });
        let done_response = serde_json::json!({
            "id": "test-done",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "done"}
            }]
        });

        // Request #1 carries the bootstrap overlay plus normal tool schemas;
        // request #2 carries normal history only.
        server.expect(
            Expectation::matching(all_of![
                request::method_path("POST", "/v1/chat/completions"),
                request::body(matches("RuntimeContext")),
                request::body(matches("search_memory")),
            ])
            .times(1)
            .respond_with(json_encoded(tool_call_response)),
        );
        server.expect(
            Expectation::matching(all_of![
                request::method_path("POST", "/v1/chat/completions"),
                request::body(not(matches("RuntimeContext"))),
                request::body(matches("search_memory")),
            ])
            .times(1)
            .respond_with(json_encoded(done_response)),
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = crate::config::AppConfig {
            project_root: dir.path().to_path_buf(),
            tool_routing: crate::config::ToolRoutingConfig {
                mode: crate::config::ToolRoutingMode::Eager,
                search_result_limit: 5,
            },
            ..Default::default()
        };
        let fs = FsTools::new(
            std::sync::Arc::new(tokio::sync::RwLock::new(None)),
            std::sync::Arc::new(cfg.clone()),
        );
        // Seed bootstrap state: a recent file and a memory matching the goal.
        fs.context_manager
            .write()
            .await
            .add_file(std::path::Path::new("src/cache.rs"));
        fs.write_memory("cache", "cache design notes", None, None)
            .await
            .expect("write memory");

        let client = crate::llm::client_core::OpenAIClient::new(
            format!("{}/", server.url_str("")),
            "test-key",
        )
        .expect("test client");
        let messages = vec![
            ChatMessage {
                provider_state: None,
                role: "system".into(),
                content: Some("test system prompt".to_string()),
                tool_calls: vec![],
                tool_call_id: None,
            },
            ChatMessage {
                provider_state: None,
                role: "user".into(),
                content: Some("cache".to_string()),
                tool_calls: vec![],
                tool_call_id: None,
            },
        ];

        let (updated_messages, final_msg) = run_agent_loop(
            &client,
            "test-model",
            &fs,
            messages,
            None,
            None,
            &cfg,
            None,
            crate::provenance::ProvenanceAttribution::none(),
        )
        .await
        .expect("agent loop completes");
        assert_eq!(final_msg.content, "done");
        // The overlay must never leak into durable history.
        assert!(
            !updated_messages.iter().any(|m| m
                .content
                .as_deref()
                .is_some_and(|c| c.contains("RuntimeContext"))),
            "runtime overlay leaked into updated_messages"
        );
    }

    #[test]
    fn test_reactive_guard_finite_retry() {
        use crate::llm::context_budget::ReactiveRetryGuard;
        // Simulate two consecutive provider overflows: first compacts and
        // retries, second errors instead of compacting again.
        let mut guard = ReactiveRetryGuard::new();
        assert!(guard.should_attempt(), "first overflow may compact");
        guard.record_attempt();
        assert!(
            !guard.should_attempt(),
            "second overflow must not compact again"
        );
        guard.record_success();
        assert!(guard.should_attempt(), "success resets for later overflows");
    }

    #[test]
    fn test_truncate_tool_output() {
        let short = "short output";
        assert_eq!(truncate_tool_output(short.to_string(), "any_tool"), short);

        let long = "na".repeat(5000); // 10000 chars
        assert!(long.len() > 8000);
        let truncated = truncate_tool_output(long.clone(), "any_tool");
        assert!(truncated.contains("truncated"));
        assert!(truncated.len() < long.len());

        // Exception for fs_read
        let read_content = "na".repeat(15000); // 30000 chars
        let not_truncated = truncate_tool_output(read_content.clone(), "fs_read");
        assert_eq!(not_truncated.len(), 30000);
        assert!(!not_truncated.contains("truncated"));

        // plan_write is compact metadata: default 8k tier.
        let plan_content = "na".repeat(15000); // 30000 chars
        let truncated_plan = truncate_tool_output(plan_content.clone(), "plan_write");
        assert!(truncated_plan.contains("truncated"));
        assert!(truncated_plan.chars().count() <= 8000);

        // fs_read too huge
        let huge_read = "na".repeat(21000); // 42000 chars
        let huge_truncated = truncate_tool_output(huge_read.clone(), "fs_read");
        assert!(huge_truncated.contains("truncated"));
    }

    #[test]
    fn test_tool_call_made_progress_overrides() {
        use serde_json::json;

        // Reads: success is progress, failure is not.
        assert_eq!(
            tool_call_made_progress("fs_read", true, Some(&json!({"ok": true}))),
            Some(true)
        );
        assert_eq!(tool_call_made_progress("fs_read", false, None), Some(false));

        // Empty search results are not progress.
        assert_eq!(
            tool_call_made_progress(
                "search_text",
                true,
                Some(&json!({"ok": true, "results": [], "meta": {}}))
            ),
            Some(false)
        );
        // Non-empty search results are progress.
        assert_eq!(
            tool_call_made_progress(
                "search_text",
                true,
                Some(&json!({"ok": true, "results": [{"path": "a.rs"}]}))
            ),
            Some(true)
        );

        // search_repomap nests the whole response under `results`.
        assert_eq!(
            tool_call_made_progress(
                "search_repomap",
                true,
                Some(&json!({"ok": true, "results": {"results": [{"file": "a.rs"}]}}))
            ),
            Some(true)
        );
        assert_eq!(
            tool_call_made_progress(
                "search_repomap",
                true,
                Some(&json!({"ok": true, "results": {"results": [], "warnings": []}}))
            ),
            Some(false)
        );

        // fs_list nests under `result`.
        assert_eq!(
            tool_call_made_progress(
                "fs_list",
                true,
                Some(&json!({"ok": true, "result": {"entries": [{"path": "x"}]}}))
            ),
            Some(true)
        );

        // find_file empty.
        assert_eq!(
            tool_call_made_progress("find_file", true, Some(&json!({"files": []}))),
            Some(false)
        );

        // Write tools keep the default heuristic.
        assert_eq!(tool_call_made_progress("edit", true, None), None);
        assert_eq!(tool_call_made_progress("execute_bash", true, None), None);

        // tool_search: real activation is progress; zero activation is not.
        assert_eq!(
            tool_call_made_progress(
                "tool_search",
                true,
                Some(&json!({"ok": true, "activated": [{"name": "edit"}]}))
            ),
            Some(true)
        );
        assert_eq!(
            tool_call_made_progress(
                "tool_search",
                true,
                Some(&json!({"ok": true, "activated": [], "already_active": [{"name": "edit"}]}))
            ),
            Some(false)
        );
        assert_eq!(
            tool_call_made_progress(
                "tool_search",
                true,
                Some(&json!({"ok": true, "activated": []}))
            ),
            Some(false)
        );
        assert_eq!(
            tool_call_made_progress("tool_search", false, None),
            Some(false)
        );
    }

    #[test]
    fn test_plan_write_arguments_hash_normalizes_json_shape() {
        let args1 =
            r#"{"items":[{"id":"step-1","content":"A","status":"pending"}],"mode":"replace"}"#;
        let args2 =
            r#"{ "mode":"replace", "items":[{"status":"pending","content":"A","id":"step-1"}] }"#;
        assert_eq!(
            plan_write_arguments_hash(args1),
            plan_write_arguments_hash(args2)
        );
    }

    #[test]
    fn test_plan_write_arguments_hash_falls_back_for_invalid_json() {
        let invalid = "{invalid json";
        let hash1 = plan_write_arguments_hash(invalid);
        let hash2 = plan_write_arguments_hash(invalid);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_build_plan_write_blocked_value_repeated_unchanged() {
        let value = build_plan_write_blocked_value(PlanWriteBlockReason::RepeatedUnchanged, 2);
        assert_eq!(value.get("success").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(
            value.get("error").and_then(|v| v.as_str()),
            Some(plan_write_block_error(
                PlanWriteBlockReason::RepeatedUnchanged
            ))
        );
        assert_eq!(
            value.get("no_change_count").and_then(|v| v.as_u64()),
            Some(2)
        );
        assert!(value.get("repeat_count").is_none());
    }

    #[test]
    fn test_build_plan_write_blocked_value_repeated_identical_args() {
        let value = build_plan_write_blocked_value(PlanWriteBlockReason::RepeatedIdenticalArgs, 3);
        assert_eq!(value.get("success").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(
            value.get("error").and_then(|v| v.as_str()),
            Some(plan_write_block_error(
                PlanWriteBlockReason::RepeatedIdenticalArgs
            ))
        );
        assert_eq!(value.get("repeat_count").and_then(|v| v.as_u64()), Some(3));
        assert!(value.get("no_change_count").is_none());
    }

    #[test]
    fn test_publish_plan_list_sends_canonical_items() {
        use crate::tools::plan::PlanItem;

        let plan = PlanList {
            session_id: Some("s".to_string()),
            items: vec![
                PlanItem {
                    id: "step-1".to_string(),
                    parent_id: None,
                    content: "first".to_string(),
                    status: "pending".to_string(),
                    requirement_ids: Vec::new(),
                    verification_obligations: Vec::new(),
                },
                PlanItem {
                    id: "step-2".to_string(),
                    parent_id: None,
                    content: "second".to_string(),
                    status: "completed".to_string(),
                    requirement_ids: Vec::new(),
                    verification_obligations: Vec::new(),
                },
            ],
        };
        let (tx, rx) = std::sync::mpsc::channel();
        publish_plan_list(&tx, &plan);
        let msg = rx.recv().expect("plan message sent");
        assert!(msg.starts_with("::plan_list:"));
        let payload = msg.trim_start_matches("::plan_list:");
        let items: Vec<serde_json::Value> =
            serde_json::from_str(payload).expect("valid items JSON");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["id"], "step-1");
        // Full content reaches the TUI (unlike the compact LLM result).
        assert_eq!(items[0]["content"], "first");
    }

    #[test]
    fn test_gc_empty_store_persists_and_does_not_resurrect() {
        use crate::llm::observation::ObservationStore;
        use std::collections::BTreeSet;

        let dir = tempfile::tempdir().expect("tempdir");
        let store_path = dir.path().join(".doge/sessions");
        let store = crate::session::SessionStore::new(store_path.clone()).expect("session store");
        let manager = std::sync::Arc::new(std::sync::Mutex::new(
            crate::session::SessionManager::with_store(store),
        ));
        {
            let mut mgr = manager.lock().expect("session lock");
            mgr.create_session(None).expect("create session");
        }
        let session_id = {
            let mgr = manager.lock().expect("session lock");
            mgr.current_session_id().expect("session id")
        };
        let cfg = crate::config::AppConfig {
            project_root: dir.path().to_path_buf(),
            ..Default::default()
        };
        let fs = FsTools::new(
            std::sync::Arc::new(tokio::sync::RwLock::new(None)),
            std::sync::Arc::new(cfg.clone()),
        )
        .with_session_manager(manager.clone());

        // Legacy disk state: one dead observation with no live reference.
        let mut dead = ObservationStore::new();
        dead.insert(
            "call-dead".into(),
            "fs_read".into(),
            "dead-body".repeat(50),
            600,
        )
        .expect("dead insert");
        assert!(!dead.is_empty());
        {
            let mut mgr = manager.lock().expect("session lock");
            mgr.update_current_session_with_observations(dead.clone(), BTreeSet::new())
                .expect("seed dead");
        }

        // Runtime history has no reference to the dead id.
        let client =
            crate::llm::client_core::OpenAIClient::new("http://127.0.0.1:1", "k").expect("client");
        let messages = vec![ChatMessage {
            provider_state: None,
            role: "user".into(),
            content: Some("fresh task".into()),
            tool_calls: vec![],
            tool_call_id: None,
        }];
        let mut history = crate::llm::tool_execution::history::HistoryManager::new(
            client,
            messages,
            None,
            fs.clone(),
            cfg,
        );
        history.restore_observations(dead, BTreeSet::new());
        assert!(
            history.observations_snapshot().is_empty(),
            "restore GC must collect the unreferenced entry"
        );

        // Empty snapshot must still be persisted (no skip).
        let _ = persist_history_and_observations(&history, &fs);

        // Reopen the store like a process restart: the entry must not return.
        let reopened_store = crate::session::SessionStore::new(store_path).expect("reopen store");
        let restored = reopened_store.load(&session_id).expect("load session");
        assert!(
            restored.observations.is_empty(),
            "GC'd observation resurrected after restart"
        );
        assert!(restored.unseen_tool_results.is_empty());
    }
}
