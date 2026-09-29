//! Provenance integration helpers (session-backed, no repomap DB).
//!
//! `ChangeCommitted` is recorded only from the transactional semantic edit
//! path; verification observations come only from real execution paths
//! (`execute_process`, `/test`, `/lint`). No manual `record_verification`
//! tool exists by design.

use crate::llm::types::{ToolDef, ToolFunctionDef};
use crate::provenance::{
    ChangeCommittedEvent, ChangeKind, ProvenanceEvent, ProvenanceEventEnvelope, ProvenanceStore,
    diff_hash_for,
};
use crate::tools::FsTools;

/// Record a transactional semantic edit as `ChangeCommitted`.
///
/// Returns `Ok(None)` when there is no current session (nothing to attach
/// to). Append failures are returned as `Err` so the caller can mark
/// `provenance_incomplete` without rolling back the source change.
pub fn record_semantic_change(
    fs_tools: &FsTools,
    result: &crate::features::semantic_edit::SemanticEditResult,
) -> anyhow::Result<Option<ProvenanceEventEnvelope>> {
    let Some(ctx) = fs_tools.current_session_storage_context() else {
        tracing::warn!("provenance.record_skipped: no current session");
        return Ok(None);
    };
    let plan_item_id = match fs_tools.plan_read() {
        Ok(plan) => crate::provenance::current_in_progress_plan_item(&plan.items),
        Err(e) => {
            tracing::debug!(error = %e, "provenance plan read failed; recording unlinked change");
            None
        }
    };
    let file =
        crate::analysis::normalize_relative_path(&fs_tools.config.project_root, &result.file)
            .map_err(|e| anyhow::anyhow!("provenance file relativization failed: {e}"))?;

    let event = ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
        transaction_id: String::new(),
        plan_item_id,
        change_kind: ChangeKind::SemanticEdit,
        file,
        symbol_id: result.symbol_id.as_str().to_string(),
        before_fingerprint: result.before_fingerprint.as_str().to_string(),
        after_fingerprint: result.after_fingerprint.as_str().to_string(),
        diff: result.diff.clone(),
        diff_hash: diff_hash_for(&result.diff),
        lines_added: result.lines_added,
        lines_removed: result.lines_removed,
    });
    let store = ProvenanceStore::new(ctx.session_dir);
    let envelope = store.append(&ctx.session_id, event)?;
    Ok(Some(envelope))
}

/// Load all provenance events for the current session, if any.
pub fn load_current_events(
    fs_tools: &FsTools,
) -> anyhow::Result<Option<crate::provenance::store::ProvenanceLoadResult>> {
    let Some(ctx) = fs_tools.current_session_storage_context() else {
        return Ok(None);
    };
    let store = ProvenanceStore::new(ctx.session_dir);
    Ok(Some(store.load_all()?))
}

/// Capture the pre-execution verification context: current plan item plus the
/// ids of currently active semantic changes.
///
/// Must be called before the process starts so mid-run changes are never
/// attributed to the running command.
pub fn capture_verification_context_for_fs(
    fs_tools: &FsTools,
) -> crate::provenance::VerificationContext {
    let plan_items = match fs_tools.plan_read() {
        Ok(plan) => plan.items,
        Err(_) => Vec::new(),
    };
    let active_ids = match load_current_events(fs_tools) {
        Ok(Some(loaded)) => {
            crate::provenance::active_change_ids(&fs_tools.config.project_root, &loaded.events)
        }
        _ => Vec::new(),
    };
    crate::provenance::capture_verification_context(&plan_items, &active_ids)
}

/// Project-relative cwd evidence, or `None` for the project root default.
///
/// Returns `None` when `cwd` is absent or cannot be relativized under the
/// project root. Absolute paths are never persisted into durable evidence.
pub fn relative_cwd_for_evidence(
    fs_tools: &FsTools,
    cwd: &Option<std::path::PathBuf>,
) -> Option<String> {
    let cwd_path = cwd.as_ref()?;
    crate::analysis::normalize_relative_path(&fs_tools.config.project_root, cwd_path).ok()
}

/// Append a TUI `/test` observation. Cancellation must be checked by the
/// caller; cancelled runs are never recorded.
///
/// Returns `true` when the event was persisted (or there was no session to
/// attach to, which is a skip, not a failure). Returns `false` when the
/// append failed and `provenance_incomplete` was marked, so the caller can
/// surface a UI warning like the other write paths.
#[allow(clippy::too_many_arguments)]
pub fn record_tui_test_verification(
    fs_tools: &FsTools,
    program: &str,
    args: &[String],
    success: bool,
    status: &str,
    exit_code: Option<i32>,
    timed_out: bool,
    stdout: &str,
    stderr: &str,
    capture_truncated: bool,
    extra_warnings: Vec<String>,
    context: crate::provenance::VerificationContext,
) -> bool {
    let event =
        crate::provenance::build_verification_event(crate::provenance::VerificationRecordInput {
            kind: crate::provenance::VerificationKind::Test,
            source: crate::provenance::VerificationSource::TuiTest,
            program,
            args,
            cwd_relative: None,
            success,
            status,
            exit_code,
            timed_out,
            stdout,
            stderr,
            capture_truncated,
            context: context.clone(),
            extra_warnings,
        });
    append_verification_event(fs_tools, event, context.observed_change_ids.len())
}

/// Append a TUI `/lint` observation with a pre-classified kind.
///
/// Same return contract as [`record_tui_test_verification`].
#[allow(clippy::too_many_arguments)]
pub fn record_tui_lint_verification(
    fs_tools: &FsTools,
    kind: crate::provenance::VerificationKind,
    program: &str,
    args: &[String],
    success: bool,
    status: &str,
    exit_code: Option<i32>,
    timed_out: bool,
    stdout: &str,
    stderr: &str,
    capture_truncated: bool,
    extra_warnings: Vec<String>,
    context: crate::provenance::VerificationContext,
) -> bool {
    let event =
        crate::provenance::build_verification_event(crate::provenance::VerificationRecordInput {
            kind,
            source: crate::provenance::VerificationSource::TuiLint,
            program,
            args,
            cwd_relative: None,
            success,
            status,
            exit_code,
            timed_out,
            stdout,
            stderr,
            capture_truncated,
            context: context.clone(),
            extra_warnings,
        });
    append_verification_event(fs_tools, event, context.observed_change_ids.len())
}

fn append_verification_event(
    fs_tools: &FsTools,
    event: crate::provenance::VerificationObservedEvent,
    change_count: usize,
) -> bool {
    let Some(ctx) = fs_tools.current_session_storage_context() else {
        return true;
    };
    let store = ProvenanceStore::new(ctx.session_dir);
    match store.append(
        &ctx.session_id,
        ProvenanceEvent::VerificationObserved(event),
    ) {
        Ok(_) => {
            tracing::info!(change_count, "provenance.verification_observed");
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "provenance.record_failed");
            let _ = fs_tools.mark_current_session_provenance_failure();
            false
        }
    }
}

/// Classify a `/lint` command for provenance. Uses the conservative
/// structured classifier first; lint fix commands without `--check` fall
/// back to their family kind. Returns `None` when the command is not a
/// recognizable verification.
pub fn classify_lint_command(
    program: &str,
    args: &[String],
) -> Option<crate::provenance::VerificationKind> {
    if let Some(kind) = crate::provenance::classify_verification(program, args) {
        return Some(kind);
    }
    let first = args.first().map(String::as_str);
    match (program, first) {
        ("cargo", Some("fmt")) => Some(crate::provenance::VerificationKind::FormatCheck),
        ("cargo", Some("clippy")) => Some(crate::provenance::VerificationKind::Lint),
        ("go", Some("fmt")) => Some(crate::provenance::VerificationKind::FormatCheck),
        ("golangci-lint", _) => Some(crate::provenance::VerificationKind::Lint),
        ("npm" | "pnpm" | "yarn" | "bun", Some(_)) => {
            // `npm run format|fmt` -> FormatCheck, `npm run lint` -> Lint.
            // Only exact script names count.
            if args.len() >= 2 && args[0] == "run" {
                match args[1].as_str() {
                    "format" | "fmt" => Some(crate::provenance::VerificationKind::FormatCheck),
                    "lint" | "lint:fix" => Some(crate::provenance::VerificationKind::Lint),
                    _ => None,
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Diff two plan snapshots into per-item transitions.
///
/// Only items whose status, content, or parent changed (plus new/deleted
/// items) are included. A no-op write produces no transitions.
pub fn diff_plan_transitions(
    before: &[crate::tools::plan::PlanItem],
    after: &[crate::tools::plan::PlanItem],
) -> Vec<crate::provenance::PlanItemTransition> {
    use std::collections::HashMap;
    let before_map: HashMap<&str, &crate::tools::plan::PlanItem> =
        before.iter().map(|i| (i.id.as_str(), i)).collect();
    let after_map: HashMap<&str, &crate::tools::plan::PlanItem> =
        after.iter().map(|i| (i.id.as_str(), i)).collect();

    let mut ids: Vec<&str> = before_map.keys().copied().collect();
    for id in after_map.keys() {
        if !before_map.contains_key(*id) {
            ids.push(*id);
        }
    }
    ids.sort();
    ids.dedup();

    let mut out = Vec::new();
    for id in ids {
        let b = before_map.get(id);
        let a = after_map.get(id);
        match (b, a) {
            (Some(b), Some(a)) => {
                if b.status != a.status || b.content != a.content || b.parent_id != a.parent_id {
                    out.push(crate::provenance::PlanItemTransition {
                        plan_item_id: id.to_string(),
                        parent_id: a.parent_id.clone(),
                        content: a.content.clone(),
                        before_status: Some(b.status.clone()),
                        after_status: Some(a.status.clone()),
                    });
                }
            }
            (None, Some(a)) => out.push(crate::provenance::PlanItemTransition {
                plan_item_id: id.to_string(),
                parent_id: a.parent_id.clone(),
                content: a.content.clone(),
                before_status: None,
                after_status: Some(a.status.clone()),
            }),
            (Some(b), None) => out.push(crate::provenance::PlanItemTransition {
                plan_item_id: id.to_string(),
                parent_id: b.parent_id.clone(),
                content: b.content.clone(),
                before_status: Some(b.status.clone()),
                after_status: None,
            }),
            (None, None) => {}
        }
    }
    out
}

/// Record a `PlanChanged` event for a successful plan write.
///
/// Only call when the plan actually changed. Failures are returned so the
/// caller can mark `provenance_incomplete` without rolling back the plan.
pub fn record_plan_changed(
    fs_tools: &FsTools,
    transitions: Vec<crate::provenance::PlanItemTransition>,
) -> anyhow::Result<Option<ProvenanceEventEnvelope>> {
    if transitions.is_empty() {
        return Ok(None);
    }
    let Some(ctx) = fs_tools.current_session_storage_context() else {
        return Ok(None);
    };
    let event = ProvenanceEvent::PlanChanged(crate::provenance::PlanChangedEvent {
        changes: transitions,
    });
    let store = ProvenanceStore::new(ctx.session_dir);
    Ok(Some(store.append(&ctx.session_id, event)?))
}

/// Warnings for newly completed items whose linked active changes have no
/// successful verification observation. Never blocks completion.
pub fn plan_completion_warnings(
    fs_tools: &FsTools,
    transitions: &[crate::provenance::PlanItemTransition],
) -> Vec<String> {
    let completed: Vec<&str> = transitions
        .iter()
        .filter(|t| {
            t.after_status.as_deref() == Some("completed")
                && t.before_status.as_deref() != Some("completed")
        })
        .map(|t| t.plan_item_id.as_str())
        .collect();
    if completed.is_empty() {
        return Vec::new();
    }
    let loaded = match load_current_events(fs_tools) {
        Ok(Some(loaded)) => loaded,
        _ => return Vec::new(),
    };
    let resolved =
        crate::provenance::resolve_active_states(&fs_tools.config.project_root, &loaded.events);
    let mut successful: std::collections::HashSet<String> = std::collections::HashSet::new();
    for env in &loaded.events {
        if let crate::provenance::ProvenanceEvent::VerificationObserved(v) = &env.event
            && v.outcome.success
        {
            for id in &v.observed_change_ids {
                successful.insert(id.clone());
            }
        }
    }
    let mut warnings = Vec::new();
    for item_id in completed {
        let linked_active: Vec<&crate::provenance::ResolvedChangeState> = resolved
            .iter()
            .filter(|r| {
                r.state == crate::provenance::ActiveChangeState::Active
                    && r.plan_item_id.as_deref() == Some(item_id)
            })
            .collect();
        if linked_active.is_empty() {
            continue;
        }
        let any_observed = linked_active
            .iter()
            .any(|r| successful.contains(&r.change_id));
        if !any_observed {
            warnings.push(format!(
                "Plan item '{item_id}' was completed, but one or more active changes have not been observed by a successful verification command."
            ));
        }
    }
    warnings
}

/// `provenance_read` tool: read-only inspection of plan/change/verification
/// links plus coverage. Never writes evidence.
pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "provenance_read".to_string(),
            description: "Read provenance linking plan steps, committed semantic changes, and observed verification commands. Use this to inspect what changed, which checks ran afterward, and where evidence is incomplete.".to_string(),
            strict: Some(true),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "plan_item_id": {"type": "string", "description": "Filter to one plan item id"},
                    "event_types": {
                        "type": "array",
                        "items": {"type": "string", "enum": ["plan_changed", "change_committed", "verification_observed"]},
                        "description": "Filter to these event types"
                    },
                    "include_diff": {"type": "boolean", "default": false, "description": "Include diffs/excerpts (still budgeted)"},
                    "cursor": {"type": "integer", "minimum": 0, "description": "0-based next position"},
                    "page_size": {"type": "integer", "minimum": 1, "maximum": 100, "description": "Events per page (default 20)"},
                    "response_budget_chars": {"type": "integer", "minimum": 1, "description": "Total response budget in chars (default 6000)"}
                },
                "required": [],
                "additionalProperties": false
            }),
        },
    }
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ProvenanceReadArgs {
    #[serde(default)]
    pub plan_item_id: Option<String>,
    #[serde(default)]
    pub event_types: Option<Vec<crate::provenance::ProvenanceEventType>>,
    #[serde(default)]
    pub include_diff: bool,
    #[serde(default)]
    pub cursor: Option<usize>,
    #[serde(default)]
    pub page_size: Option<usize>,
    #[serde(default)]
    pub response_budget_chars: Option<usize>,
}

pub const PROVENANCE_READ_DEFAULT_PAGE_SIZE: usize = 20;
pub const PROVENANCE_READ_MAX_PAGE_SIZE: usize = 100;
const PROVENANCE_READ_DEFAULT_BUDGET: usize = 6_000;
const PROVENANCE_READ_DIFF_BUDGET: usize = 2_000;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProvenanceReadResponse {
    pub events: Vec<serde_json::Value>,
    pub coverage: crate::provenance::ProvenanceCoverage,
    pub warnings: Vec<String>,
    pub next_cursor: Option<usize>,
}

/// Shrink coverage ID lists until the serialized coverage fits `cap`.
///
/// Drops from the longest list first so no single category starves the
/// others. Truncation is reported via `warnings` so the LLM knows the
/// coverage view is partial.
fn budget_coverage(
    coverage: &mut crate::provenance::ProvenanceCoverage,
    cap: usize,
    warnings: &mut Vec<String>,
) {
    let cap = cap.max(256);
    let mut truncated = false;
    while serde_json::to_string(&coverage)
        .map(|s| s.len())
        .unwrap_or(0)
        > cap
    {
        let lens = [
            coverage.tracked_active_change_ids.len(),
            coverage.verified_active_change_ids.len(),
            coverage.unverified_active_change_ids.len(),
            coverage.diverged_change_ids.len(),
            coverage.unlinked_change_ids.len(),
            coverage.untracked_changed_files.len(),
        ];
        let max = lens.into_iter().max().unwrap_or(0);
        if max == 0 {
            break;
        }
        // Pop from the first longest list (deterministic order).
        if coverage.tracked_active_change_ids.len() == max {
            coverage.tracked_active_change_ids.pop();
        } else if coverage.verified_active_change_ids.len() == max {
            coverage.verified_active_change_ids.pop();
        } else if coverage.unverified_active_change_ids.len() == max {
            coverage.unverified_active_change_ids.pop();
        } else if coverage.diverged_change_ids.len() == max {
            coverage.diverged_change_ids.pop();
        } else if coverage.unlinked_change_ids.len() == max {
            coverage.unlinked_change_ids.pop();
        } else {
            coverage.untracked_changed_files.pop();
        }
        truncated = true;
    }
    if truncated {
        warnings.push(
            "Provenance coverage ID lists truncated to stay within response_budget_chars; use plan_item_id/event_types filters for a narrower view."
                .to_string(),
        );
    }
}

/// Execute `provenance_read` against the current session.
pub fn provenance_read(
    fs_tools: &FsTools,
    args: ProvenanceReadArgs,
) -> anyhow::Result<ProvenanceReadResponse> {
    let budget = args
        .response_budget_chars
        .unwrap_or(PROVENANCE_READ_DEFAULT_BUDGET)
        .max(500);
    let page_size = args
        .page_size
        .unwrap_or(PROVENANCE_READ_DEFAULT_PAGE_SIZE)
        .clamp(1, PROVENANCE_READ_MAX_PAGE_SIZE);
    let cursor = args.cursor.unwrap_or(0);

    let Some(ctx) = fs_tools.current_session_storage_context() else {
        let session = fs_tools.get_current_session();
        let incomplete = session.as_ref().is_some_and(|s| s.provenance_incomplete);
        return Ok(ProvenanceReadResponse {
            events: vec![],
            coverage: crate::provenance::ProvenanceCoverage {
                provenance_incomplete: incomplete,
                ..Default::default()
            },
            warnings: vec!["No current session for provenance_read".to_string()],
            next_cursor: None,
        });
    };
    let store = ProvenanceStore::new(ctx.session_dir);
    let loaded = store.load_all()?;
    let mut warnings = loaded.warnings.clone();

    // Filter by type + plan item, preserving (timestamp, event_id) order.
    let wanted: Option<std::collections::HashSet<crate::provenance::ProvenanceEventType>> =
        args.event_types.map(|v| v.into_iter().collect());
    let mut filtered: Vec<&ProvenanceEventEnvelope> = loaded
        .events
        .iter()
        .filter(|env| {
            if let Some(wanted) = &wanted
                && !wanted.contains(&env.event_type())
            {
                return false;
            }
            if let Some(item) = &args.plan_item_id {
                match &env.event {
                    ProvenanceEvent::PlanChanged(e) => {
                        if !e.changes.iter().any(|c| &c.plan_item_id == item) {
                            return false;
                        }
                    }
                    ProvenanceEvent::ChangeCommitted(c) => {
                        if c.plan_item_id.as_deref() != Some(item.as_str()) {
                            return false;
                        }
                    }
                    ProvenanceEvent::VerificationObserved(v) => {
                        if v.plan_item_id.as_deref() != Some(item.as_str()) {
                            return false;
                        }
                    }
                }
            }
            true
        })
        .collect();

    // Coverage over the full event set (not the filtered page).
    // Coverage ID lists are budgeted so the tool owns its output cap instead
    // of relying on the global truncator safety net.
    let session = fs_tools.get_current_session();
    let changed_files = session
        .as_ref()
        .map(|s| s.changed_files.clone())
        .unwrap_or_default();
    let incomplete = session.as_ref().is_some_and(|s| s.provenance_incomplete);
    let mut coverage = crate::provenance::compute_coverage(
        &fs_tools.config.project_root,
        &loaded.events,
        &changed_files,
        incomplete,
    );
    budget_coverage(&mut coverage, budget / 2, &mut warnings);

    let total = filtered.len();
    let start = cursor.min(total);
    let end = (start + page_size).min(total);
    let next_cursor = if end < total { Some(end) } else { None };

    let mut events_out = Vec::new();
    let mut used = serde_json::to_string(&coverage)
        .map(|s| s.len())
        .unwrap_or(0)
        + 256;
    for env in filtered[start..end].iter() {
        let summary = summarize_event(env, args.include_diff);
        let len = serde_json::to_string(&summary)
            .map(|s| s.len())
            .unwrap_or(0)
            + 1;
        if !events_out.is_empty() && used + len > budget {
            warnings.push("Provenance events truncated to stay within response_budget_chars; use cursor/page_size to fetch more.".to_string());
            // next_cursor must point at the first omitted event.
            let omitted_start = start + events_out.len();
            return Ok(ProvenanceReadResponse {
                events: events_out,
                coverage,
                warnings,
                next_cursor: Some(omitted_start),
            });
        }
        used += len;
        events_out.push(summary);
    }
    // Silence unused mut warning when no budget cut happens.
    let _ = &mut filtered;

    Ok(ProvenanceReadResponse {
        events: events_out,
        coverage,
        warnings,
        next_cursor,
    })
}

fn summarize_event(env: &ProvenanceEventEnvelope, include_diff: bool) -> serde_json::Value {
    match &env.event {
        ProvenanceEvent::PlanChanged(e) => {
            serde_json::json!({
                "event_id": env.event_id,
                "type": "plan_changed",
                "timestamp": env.timestamp,
                "changes": e.changes.iter().map(|c| serde_json::json!({
                    "plan_item_id": c.plan_item_id,
                    "before_status": c.before_status,
                    "after_status": c.after_status,
                })).collect::<Vec<_>>(),
            })
        }
        ProvenanceEvent::ChangeCommitted(c) => {
            let mut v = serde_json::json!({
                "event_id": env.event_id,
                "type": "change_committed",
                "timestamp": env.timestamp,
                "plan_item_id": c.plan_item_id,
                "file": c.file,
                "symbol_id": c.symbol_id,
                "lines_added": c.lines_added,
                "lines_removed": c.lines_removed,
                "diff_hash": c.diff_hash,
            });
            if include_diff {
                let budgeted =
                    crate::tools::budget::head_tail_truncate(&c.diff, PROVENANCE_READ_DIFF_BUDGET)
                        .text;
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("diff".to_string(), serde_json::Value::String(budgeted));
                }
            }
            v
        }
        ProvenanceEvent::VerificationObserved(v) => {
            let mut value = serde_json::json!({
                "event_id": env.event_id,
                "type": "verification_observed",
                "timestamp": env.timestamp,
                "plan_item_id": v.plan_item_id,
                "verification_kind": v.verification_kind,
                "source": v.source,
                "program": v.command.program,
                "args": v.command.args,
                "success": v.outcome.success,
                "status": v.outcome.status,
                "exit_code": v.outcome.exit_code,
                "timed_out": v.outcome.timed_out,
                "observed_change_ids": v.observed_change_ids,
                "output_digest": v.output_digest,
                "output_truncated": v.output_truncated,
            });
            if include_diff && let Some(obj) = value.as_object_mut() {
                let out_b = crate::tools::budget::head_tail_truncate(
                    &v.stdout_excerpt,
                    PROVENANCE_READ_DIFF_BUDGET,
                )
                .text;
                let err_b = crate::tools::budget::head_tail_truncate(
                    &v.stderr_excerpt,
                    PROVENANCE_READ_DIFF_BUDGET,
                )
                .text;
                obj.insert(
                    "stdout_excerpt".to_string(),
                    serde_json::Value::String(out_b),
                );
                obj.insert(
                    "stderr_excerpt".to_string(),
                    serde_json::Value::String(err_b),
                );
            }
            if !v.warnings.is_empty()
                && let Some(obj) = value.as_object_mut()
            {
                obj.insert("warnings".to_string(), serde_json::json!(v.warnings));
            }
            value
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_fs_tools(project_root: &std::path::Path) -> FsTools {
        let config = std::sync::Arc::new(crate::config::AppConfig {
            project_root: project_root.to_path_buf(),
            ..crate::config::AppConfig::default()
        });
        FsTools::new(std::sync::Arc::new(tokio::sync::RwLock::new(None)), config)
    }

    fn fs_with_session(
        project_root: &std::path::Path,
    ) -> (FsTools, crate::session::SessionStorageContext) {
        let sessions_root = project_root.join(".doge/sessions");
        let store = crate::session::SessionStore::new(sessions_root).unwrap();
        let manager = std::sync::Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            store,
            current_session: None,
        }));
        {
            let mut mgr = manager.lock().unwrap();
            mgr.create_session(None).unwrap();
        }
        let config = std::sync::Arc::new(crate::config::AppConfig {
            project_root: project_root.to_path_buf(),
            ..crate::config::AppConfig::default()
        });
        let fs = FsTools::new(std::sync::Arc::new(tokio::sync::RwLock::new(None)), config)
            .with_session_manager(manager);
        let ctx = fs.current_session_storage_context().unwrap();
        (fs, ctx)
    }

    #[test]
    fn test_provenance_tool_def_registered_shape() {
        let def = tool_def();
        assert_eq!(def.function.name, "provenance_read");
        assert_eq!(def.function.strict, Some(true));
    }

    #[test]
    fn test_diff_plan_transitions_new_and_delete() {
        use crate::tools::plan::PlanItem;
        let before: Vec<PlanItem> = vec![];
        let after = vec![PlanItem {
            id: "step-1".into(),
            parent_id: None,
            content: "a".into(),
            status: "pending".into(),
        }];
        let t = diff_plan_transitions(&before, &after);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].before_status, None);
        assert_eq!(t[0].after_status.as_deref(), Some("pending"));
        // No-op produces no transitions.
        assert!(diff_plan_transitions(&after, &after).is_empty());
    }

    #[test]
    fn test_plan_link_prefers_in_progress_only() {
        use crate::tools::plan::PlanItem;
        let items = vec![
            PlanItem {
                id: "step-1".into(),
                parent_id: None,
                content: "a".into(),
                status: "completed".into(),
            },
            PlanItem {
                id: "step-2".into(),
                parent_id: None,
                content: "b".into(),
                status: "pending".into(),
            },
        ];
        assert_eq!(
            crate::provenance::current_in_progress_plan_item(&items),
            None
        );
    }

    #[test]
    fn test_provenance_read_pagination_and_filters() {
        let proj = tempfile::tempdir().unwrap();
        let (fs, _ctx) = fs_with_session(proj.path());
        // Two changes + one verification.
        for i in 0..3 {
            let store =
                ProvenanceStore::new(fs.current_session_storage_context().unwrap().session_dir);
            let sid = fs.current_session_storage_context().unwrap().session_id;
            store
                .append(
                    &sid,
                    ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                        transaction_id: String::new(),
                        plan_item_id: Some(format!("step-{}", i % 2 + 1)),
                        change_kind: ChangeKind::SemanticEdit,
                        file: format!("src/f{i}.rs"),
                        symbol_id: format!("sym-{i}"),
                        before_fingerprint: "a".into(),
                        after_fingerprint: "b".into(),
                        diff: "d".into(),
                        diff_hash: "blake3:x".into(),
                        lines_added: 1,
                        lines_removed: 0,
                    }),
                )
                .unwrap();
        }
        let args = ProvenanceReadArgs {
            page_size: Some(2),
            cursor: Some(0),
            ..Default::default()
        };
        let resp = provenance_read(&fs, args).unwrap();
        assert_eq!(resp.events.len(), 2);
        assert_eq!(resp.next_cursor, Some(2));
        let args2 = ProvenanceReadArgs {
            page_size: Some(2),
            cursor: Some(2),
            ..Default::default()
        };
        let resp2 = provenance_read(&fs, args2).unwrap();
        assert_eq!(resp2.events.len(), 1);
        assert_eq!(resp2.next_cursor, None);
        // Plan filter.
        let args3 = ProvenanceReadArgs {
            plan_item_id: Some("step-1".into()),
            ..Default::default()
        };
        let resp3 = provenance_read(&fs, args3).unwrap();
        assert!(
            resp3
                .events
                .iter()
                .all(|e| e.to_string().contains("step-1"))
        );
        // include_diff false omits diff.
        assert!(resp.events.iter().all(|e| e.get("diff").is_none()));
        let args4 = ProvenanceReadArgs {
            include_diff: true,
            ..Default::default()
        };
        let resp4 = provenance_read(&fs, args4).unwrap();
        assert!(resp4.events.iter().any(|e| e.get("diff").is_some()));
    }

    #[test]
    fn test_verification_context_race_snapshot() {
        // Change A captured; Change B lands before completion; observed keeps A only.
        let ctx_a = crate::provenance::VerificationContext {
            plan_item_id: Some("step-1".into()),
            observed_change_ids: vec!["change-A".to_string()],
        };
        let event = crate::provenance::build_verification_event(
            crate::provenance::VerificationRecordInput {
                kind: crate::provenance::VerificationKind::Test,
                source: crate::provenance::VerificationSource::ExecuteProcess,
                program: "cargo",
                args: &["test".to_string()],
                cwd_relative: None,
                success: true,
                status: "completed",
                exit_code: Some(0),
                timed_out: false,
                stdout: "ok",
                stderr: "",
                capture_truncated: false,
                context: ctx_a,
                extra_warnings: vec![],
            },
        );
        assert_eq!(event.observed_change_ids, vec!["change-A".to_string()]);
    }

    #[test]
    fn test_classify_lint_fallback() {
        assert_eq!(
            classify_lint_command("cargo", &["fmt".to_string()]),
            Some(crate::provenance::VerificationKind::FormatCheck)
        );
        assert_eq!(
            classify_lint_command("cargo", &["clippy".to_string(), "--fix".to_string()]),
            Some(crate::provenance::VerificationKind::Lint)
        );
        assert_eq!(classify_lint_command("echo", &["hi".to_string()]), None);
    }

    #[test]
    fn test_unused_helper_compiles() {
        let _ = test_fs_tools(std::path::Path::new("/tmp"));
    }
}

#[cfg(test)]
mod provenance_extra_tests {
    use super::*;
    use crate::provenance::{ProvenanceEvent, ProvenanceStore};

    fn setup_project_with_session() -> (tempfile::TempDir, FsTools, String) {
        let proj = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(proj.path().join("src")).unwrap();
        std::fs::write(proj.path().join("src/lib.rs"), "fn foo() {\n    1;\n}\n").unwrap();
        let sessions_root = proj.path().join(".doge/sessions");
        let store = crate::session::SessionStore::new(sessions_root).unwrap();
        let manager = std::sync::Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            store,
            current_session: None,
        }));
        {
            let mut mgr = manager.lock().unwrap();
            mgr.create_session(None).unwrap();
        }
        let config = std::sync::Arc::new(crate::config::AppConfig {
            project_root: proj.path().to_path_buf(),
            ..crate::config::AppConfig::default()
        });
        let fs = FsTools::new(std::sync::Arc::new(tokio::sync::RwLock::new(None)), config)
            .with_session_manager(manager);
        let sid = fs.ensure_current_session_id().unwrap();
        (proj, fs, sid)
    }

    #[test]
    fn test_session_delete_removes_provenance() {
        let (_proj, fs, sid) = setup_project_with_session();
        let ctx = fs.current_session_storage_context().unwrap();
        let store = ProvenanceStore::new(ctx.session_dir.clone());
        store
            .append(
                &sid,
                ProvenanceEvent::PlanChanged(crate::provenance::PlanChangedEvent {
                    changes: vec![],
                }),
            )
            .unwrap();
        assert!(store.events_dir().exists());
        // Delete via SessionStore: whole session dir must go.
        let sessions_root = _proj.path().join(".doge/sessions");
        let sstore = crate::session::SessionStore::new(sessions_root).unwrap();
        sstore.delete(&sid).unwrap();
        assert!(!ctx.session_dir.exists());
    }

    #[test]
    fn test_semantic_change_records_all_fields() {
        let (_proj, fs, _sid) = setup_project_with_session();
        // Plan with in_progress step-2.
        fs.plan_write(
            vec![
                crate::tools::plan::PlanItem {
                    id: "step-1".into(),
                    parent_id: None,
                    content: "a".into(),
                    status: "pending".into(),
                },
                crate::tools::plan::PlanItem {
                    id: "step-2".into(),
                    parent_id: None,
                    content: "b".into(),
                    status: "in_progress".into(),
                },
            ],
            crate::tools::plan::PlanWriteMode::Replace,
        )
        .unwrap();
        let root = fs.config.project_root.clone();
        let file = root.join("src/lib.rs");
        let prepared = crate::features::semantic_edit::prepare_edit(&root, &file, 2).unwrap();
        let current = std::fs::read_to_string(&file).unwrap();
        let (_candidate, result, _) = crate::features::semantic_edit::apply_with_snapshots(
            &prepared,
            "fn foo() {\n    2;\n}\n",
            &current,
            &current,
            &root,
        )
        .unwrap();
        let env = record_semantic_change(&fs, &result)
            .unwrap()
            .expect("recorded");
        match &env.event {
            ProvenanceEvent::ChangeCommitted(c) => {
                assert_eq!(c.file, "src/lib.rs");
                assert!(!c.file.starts_with('/'));
                assert_eq!(c.symbol_id, result.symbol_id.as_str());
                assert_eq!(c.before_fingerprint, result.before_fingerprint.as_str());
                assert_eq!(c.after_fingerprint, result.after_fingerprint.as_str());
                assert!(c.diff_hash.starts_with("blake3:"));
                assert_eq!(c.diff_hash, crate::provenance::diff_hash_for(&result.diff));
                assert_eq!(c.lines_added, result.lines_added);
                assert_eq!(c.lines_removed, result.lines_removed);
                assert_eq!(c.plan_item_id.as_deref(), Some("step-2"));
            }
            _ => panic!("expected change"),
        }
    }

    #[test]
    fn test_plan_link_none_when_no_in_progress() {
        let (_proj, fs, _sid) = setup_project_with_session();
        fs.plan_write(
            vec![
                crate::tools::plan::PlanItem {
                    id: "step-1".into(),
                    parent_id: None,
                    content: "a".into(),
                    status: "pending".into(),
                },
                crate::tools::plan::PlanItem {
                    id: "step-2".into(),
                    parent_id: None,
                    content: "b".into(),
                    status: "completed".into(),
                },
            ],
            crate::tools::plan::PlanWriteMode::Replace,
        )
        .unwrap();
        let root = fs.config.project_root.clone();
        let file = root.join("src/lib.rs");
        let prepared = crate::features::semantic_edit::prepare_edit(&root, &file, 2).unwrap();
        let current = std::fs::read_to_string(&file).unwrap();
        let (_c, result, _) = crate::features::semantic_edit::apply_with_snapshots(
            &prepared,
            "fn foo() {\n    2;\n}\n",
            &current,
            &current,
            &root,
        )
        .unwrap();
        let env = record_semantic_change(&fs, &result)
            .unwrap()
            .expect("recorded");
        match &env.event {
            ProvenanceEvent::ChangeCommitted(c) => assert_eq!(c.plan_item_id, None),
            _ => panic!("expected change"),
        }
    }

    #[test]
    fn test_provenance_failure_keeps_source_and_marks_session() {
        let (_proj, fs, _sid) = setup_project_with_session();
        let root = fs.config.project_root.clone();
        let file = root.join("src/lib.rs");
        // Real commit first.
        let prepared = crate::features::semantic_edit::prepare_edit(&root, &file, 2).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (result, _, _) = rt
            .block_on(crate::features::semantic_edit::apply_edit(
                &prepared,
                "fn foo() {\n    2;\n}\n",
                &root,
            ))
            .unwrap();
        let after_commit = std::fs::read_to_string(&file).unwrap();
        assert!(after_commit.contains("2;"));
        // Break provenance writes: a file where the events dir should be.
        let ctx = fs.current_session_storage_context().unwrap();
        let events_dir = ctx.session_dir.join("provenance/v1/events");
        std::fs::create_dir_all(events_dir.parent().unwrap()).unwrap();
        std::fs::write(&events_dir, "not-a-dir").unwrap();
        let err = record_semantic_change(&fs, &result).expect_err("must fail");
        assert!(!format!("{err}").is_empty());
        // Caller marks failure; source stays committed.
        fs.mark_current_session_provenance_failure().unwrap();
        let sess = fs.get_current_session().unwrap();
        assert!(sess.provenance_incomplete);
        assert_eq!(sess.provenance_record_failures, 1);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), after_commit);
    }

    #[test]
    fn test_untracked_changed_files_reported() {
        let (_proj, fs, _sid) = setup_project_with_session();
        let root = fs.config.project_root.clone();
        std::fs::write(root.join("src/b.rs"), "fn b() {}\n").unwrap();
        // Tracked: semantic change for src/lib.rs (record only, no write needed).
        let file = root.join("src/lib.rs");
        let prepared = crate::features::semantic_edit::prepare_edit(&root, &file, 2).unwrap();
        let current = std::fs::read_to_string(&file).unwrap();
        let (_c, result, _) = crate::features::semantic_edit::apply_with_snapshots(
            &prepared,
            "fn foo() {\n    2;\n}\n",
            &current,
            &current,
            &root,
        )
        .unwrap();
        record_semantic_change(&fs, &result).unwrap();
        // Simulate non-semantic edits via session changed_files.
        fs.update_session_with_changed_file(std::path::PathBuf::from("src/lib.rs"))
            .unwrap();
        fs.update_session_with_changed_file(std::path::PathBuf::from("src/b.rs"))
            .unwrap();
        let loaded = load_current_events(&fs).unwrap().unwrap();
        let sess = fs.get_current_session().unwrap();
        let cov = crate::provenance::compute_coverage(
            &root,
            &loaded.events,
            &sess.changed_files,
            sess.provenance_incomplete,
        );
        assert!(
            cov.untracked_changed_files
                .contains(&"src/b.rs".to_string())
        );
        assert!(
            !cov.untracked_changed_files
                .contains(&"src/lib.rs".to_string())
        );
    }

    #[test]
    fn test_plan_changed_transitions_cover_statuses() {
        use crate::tools::plan::PlanItem;
        let before = vec![
            PlanItem {
                id: "a".into(),
                parent_id: None,
                content: "x".into(),
                status: "pending".into(),
            },
            PlanItem {
                id: "b".into(),
                parent_id: None,
                content: "y".into(),
                status: "in_progress".into(),
            },
            PlanItem {
                id: "gone".into(),
                parent_id: None,
                content: "z".into(),
                status: "pending".into(),
            },
        ];
        let after = vec![
            PlanItem {
                id: "a".into(),
                parent_id: None,
                content: "x".into(),
                status: "in_progress".into(),
            },
            PlanItem {
                id: "b".into(),
                parent_id: None,
                content: "y".into(),
                status: "completed".into(),
            },
            PlanItem {
                id: "new".into(),
                parent_id: None,
                content: "n".into(),
                status: "pending".into(),
            },
        ];
        let t = diff_plan_transitions(&before, &after);
        let by_id: std::collections::HashMap<_, _> =
            t.into_iter().map(|x| (x.plan_item_id.clone(), x)).collect();
        assert_eq!(by_id["a"].before_status.as_deref(), Some("pending"));
        assert_eq!(by_id["a"].after_status.as_deref(), Some("in_progress"));
        assert_eq!(by_id["b"].after_status.as_deref(), Some("completed"));
        assert_eq!(by_id["new"].before_status, None);
        assert_eq!(by_id["gone"].after_status, None);
    }

    #[test]
    fn test_plan_noop_writes_no_event() {
        let (_proj, fs, _sid) = setup_project_with_session();
        let items = vec![crate::tools::plan::PlanItem {
            id: "step-1".into(),
            parent_id: None,
            content: "a".into(),
            status: "pending".into(),
        }];
        let first = fs
            .plan_write(items.clone(), crate::tools::plan::PlanWriteMode::Replace)
            .unwrap();
        assert!(first.changed);
        let count_events = || load_current_events(&fs).unwrap().unwrap().events.len();
        let after_first = count_events();
        assert!(after_first >= 1);
        let second = fs
            .plan_write(items, crate::tools::plan::PlanWriteMode::Replace)
            .unwrap();
        assert!(!second.changed);
        assert_eq!(count_events(), after_first);
    }

    #[test]
    fn test_plan_completion_warnings() {
        let (_proj, fs, _sid) = setup_project_with_session();
        // Plan with in_progress step-1, then link a change to it.
        fs.plan_write(
            vec![crate::tools::plan::PlanItem {
                id: "step-1".into(),
                parent_id: None,
                content: "work".into(),
                status: "in_progress".into(),
            }],
            crate::tools::plan::PlanWriteMode::Replace,
        )
        .unwrap();
        let root = fs.config.project_root.clone();
        let file = root.join("src/lib.rs");
        let prepared = crate::features::semantic_edit::prepare_edit(&root, &file, 2).unwrap();
        let current = std::fs::read_to_string(&file).unwrap();
        let (_c, result, _) = crate::features::semantic_edit::apply_with_snapshots(
            &prepared,
            "fn foo() {\n    2;\n}\n",
            &current,
            &current,
            &root,
        )
        .unwrap();
        // Write the candidate so the active resolver sees it as Active.
        {
            let (candidate, _, _) = crate::features::semantic_edit::apply_with_snapshots(
                &prepared,
                "fn foo() {\n    2;\n}\n",
                &current,
                &current,
                &root,
            )
            .unwrap();
            std::fs::write(&file, candidate).unwrap();
        }
        let change_env = record_semantic_change(&fs, &result).unwrap().unwrap();
        // Complete without verification -> warning.
        let res = fs
            .plan_write(
                vec![crate::tools::plan::PlanItem {
                    id: "step-1".into(),
                    parent_id: None,
                    content: "work".into(),
                    status: "completed".into(),
                }],
                crate::tools::plan::PlanWriteMode::Replace,
            )
            .unwrap();
        assert!(
            res.warnings
                .iter()
                .any(|w| w.contains("step-1") && w.contains("successful verification"))
        );
        // Research-only item (no linked changes) completes without that warning.
        let res2 = fs
            .plan_write(
                vec![
                    crate::tools::plan::PlanItem {
                        id: "step-1".into(),
                        parent_id: None,
                        content: "work".into(),
                        status: "completed".into(),
                    },
                    crate::tools::plan::PlanItem {
                        id: "research".into(),
                        parent_id: None,
                        content: "read docs".into(),
                        status: "completed".into(),
                    },
                ],
                crate::tools::plan::PlanWriteMode::Replace,
            )
            .unwrap();
        assert!(!res2.warnings.iter().any(|w| w.contains("'research'")));
        let _ = change_env;
    }

    #[test]
    fn test_plan_completion_no_warning_with_success() {
        let (_proj, fs, _sid) = setup_project_with_session();
        fs.plan_write(
            vec![crate::tools::plan::PlanItem {
                id: "step-1".into(),
                parent_id: None,
                content: "work".into(),
                status: "in_progress".into(),
            }],
            crate::tools::plan::PlanWriteMode::Replace,
        )
        .unwrap();
        let root = fs.config.project_root.clone();
        let file = root.join("src/lib.rs");
        let prepared = crate::features::semantic_edit::prepare_edit(&root, &file, 2).unwrap();
        let current = std::fs::read_to_string(&file).unwrap();
        let (candidate, result, _) = crate::features::semantic_edit::apply_with_snapshots(
            &prepared,
            "fn foo() {\n    2;\n}\n",
            &current,
            &current,
            &root,
        )
        .unwrap();
        std::fs::write(&file, candidate).unwrap();
        let change_env = record_semantic_change(&fs, &result).unwrap().unwrap();
        // Successful verification observing the change.
        let ctx = fs.current_session_storage_context().unwrap();
        let store = ProvenanceStore::new(ctx.session_dir);
        let event = crate::provenance::build_verification_event(
            crate::provenance::VerificationRecordInput {
                kind: crate::provenance::VerificationKind::Test,
                source: crate::provenance::VerificationSource::ExecuteProcess,
                program: "cargo",
                args: &["test".to_string()],
                cwd_relative: None,
                success: true,
                status: "completed",
                exit_code: Some(0),
                timed_out: false,
                stdout: "ok",
                stderr: "",
                capture_truncated: false,
                context: crate::provenance::VerificationContext {
                    plan_item_id: Some("step-1".into()),
                    observed_change_ids: vec![change_env.event_id.clone()],
                },
                extra_warnings: vec![],
            },
        );
        store
            .append(
                &ctx.session_id,
                ProvenanceEvent::VerificationObserved(event),
            )
            .unwrap();
        let res = fs
            .plan_write(
                vec![crate::tools::plan::PlanItem {
                    id: "step-1".into(),
                    parent_id: None,
                    content: "work".into(),
                    status: "completed".into(),
                }],
                crate::tools::plan::PlanWriteMode::Replace,
            )
            .unwrap();
        assert!(
            !res.warnings
                .iter()
                .any(|w| w.contains("successful verification"))
        );
    }

    #[test]
    fn test_provenance_incomplete_warning_on_completion() {
        let (_proj, fs, _sid) = setup_project_with_session();
        fs.plan_write(
            vec![crate::tools::plan::PlanItem {
                id: "step-1".into(),
                parent_id: None,
                content: "a".into(),
                status: "pending".into(),
            }],
            crate::tools::plan::PlanWriteMode::Replace,
        )
        .unwrap();
        fs.mark_current_session_provenance_failure().unwrap();
        let res = fs
            .plan_write(
                vec![crate::tools::plan::PlanItem {
                    id: "step-1".into(),
                    parent_id: None,
                    content: "a".into(),
                    status: "completed".into(),
                }],
                crate::tools::plan::PlanWriteMode::Replace,
            )
            .unwrap();
        assert!(res.warnings.iter().any(|w| w.contains("incomplete")));
    }

    #[test]
    fn test_provenance_read_budget_malformed_and_coverage() {
        let (_proj, fs, _sid) = setup_project_with_session();
        let ctx = fs.current_session_storage_context().unwrap();
        let store = ProvenanceStore::new(ctx.session_dir.clone());
        // Large diff to force budget cut.
        let big_diff = "x".repeat(20_000);
        store
            .append(
                &ctx.session_id,
                ProvenanceEvent::ChangeCommitted(crate::provenance::ChangeCommittedEvent {
                    transaction_id: String::new(),
                    plan_item_id: None,
                    change_kind: crate::provenance::ChangeKind::SemanticEdit,
                    file: "src/lib.rs".into(),
                    symbol_id: "s".into(),
                    before_fingerprint: "a".into(),
                    after_fingerprint: "b".into(),
                    diff: big_diff,
                    diff_hash: "blake3:x".into(),
                    lines_added: 1,
                    lines_removed: 0,
                }),
            )
            .unwrap();
        std::fs::write(store.events_dir().join("broken.json"), "{bad").unwrap();
        let resp = provenance_read(
            &fs,
            ProvenanceReadArgs {
                include_diff: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(resp.warnings.iter().any(|w| w.contains("malformed")));
        assert!(resp.coverage.unlinked_change_ids.len() == 1);
        // Budget: tiny budget forces truncation warning + next_cursor.
        let resp2 = provenance_read(
            &fs,
            ProvenanceReadArgs {
                response_budget_chars: Some(500),
                ..Default::default()
            },
        )
        .unwrap();
        let serialized = serde_json::to_string(&resp2).unwrap();
        assert!(
            serialized.len() <= 2000,
            "budgeted response too large: {}",
            serialized.len()
        );
    }

    #[test]
    fn test_no_secrets_in_provenance_payload() {
        let (_proj, fs, _sid) = setup_project_with_session();
        let ctx = fs.current_session_storage_context().unwrap();
        let store = ProvenanceStore::new(ctx.session_dir);
        let event = crate::provenance::build_verification_event(
            crate::provenance::VerificationRecordInput {
                kind: crate::provenance::VerificationKind::Test,
                source: crate::provenance::VerificationSource::ExecuteProcess,
                program: "cargo",
                args: &["test".to_string()],
                cwd_relative: None,
                success: true,
                status: "completed",
                exit_code: Some(0),
                timed_out: false,
                stdout: "ok",
                stderr: "",
                capture_truncated: false,
                context: Default::default(),
                extra_warnings: vec![],
            },
        );
        let env = store
            .append(
                &ctx.session_id,
                ProvenanceEvent::VerificationObserved(event),
            )
            .unwrap();
        let raw = serde_json::to_string(&env).unwrap();
        assert!(!raw.contains("env"));
        assert!(!raw.contains("API_KEY"));
        assert!(!raw.contains("TOKEN"));
    }
}

#[cfg(test)]
mod review_fix_tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, FsTools) {
        let proj = tempfile::tempdir().unwrap();
        let sessions_root = proj.path().join(".doge/sessions");
        let store = crate::session::SessionStore::new(sessions_root).unwrap();
        let manager = std::sync::Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            store,
            current_session: None,
        }));
        {
            let mut mgr = manager.lock().unwrap();
            mgr.create_session(None).unwrap();
        }
        let config = std::sync::Arc::new(crate::config::AppConfig {
            project_root: proj.path().to_path_buf(),
            ..crate::config::AppConfig::default()
        });
        let fs = FsTools::new(std::sync::Arc::new(tokio::sync::RwLock::new(None)), config)
            .with_session_manager(manager);
        (proj, fs)
    }

    #[test]
    fn test_load_skips_unreadable_entry_with_warning() {
        let (_proj, fs) = setup();
        let ctx = fs.current_session_storage_context().unwrap();
        let store = ProvenanceStore::new(ctx.session_dir.clone());
        store
            .append(
                &ctx.session_id,
                ProvenanceEvent::PlanChanged(crate::provenance::PlanChangedEvent {
                    changes: vec![],
                }),
            )
            .unwrap();
        // A subdirectory named *.json: read_to_string fails -> warning, not fatal.
        std::fs::create_dir_all(store.events_dir().join("sub.json")).unwrap();
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 1);
        assert!(loaded.warnings.iter().any(|w| w.contains("sub.json")));
    }

    #[test]
    fn test_relative_cwd_never_absolute() {
        let (_proj, fs) = setup();
        assert_eq!(relative_cwd_for_evidence(&fs, &None), None);
        let inside = fs.config.project_root.join("src");
        assert_eq!(
            relative_cwd_for_evidence(&fs, &Some(inside)),
            Some("src".to_string())
        );
        // Outside the project root must not be persisted.
        let outside = std::path::PathBuf::from("/etc");
        assert_eq!(relative_cwd_for_evidence(&fs, &Some(outside)), None);
    }

    #[test]
    fn test_tui_record_returns_false_and_marks_session_on_failure() {
        let (_proj, fs) = setup();
        let ctx = fs.current_session_storage_context().unwrap();
        let events_dir = ctx.session_dir.join("provenance/v1/events");
        std::fs::create_dir_all(events_dir.parent().unwrap()).unwrap();
        std::fs::write(&events_dir, "not-a-dir").unwrap();
        let ok = record_tui_test_verification(
            &fs,
            "cargo",
            &["test".to_string()],
            true,
            "completed",
            Some(0),
            false,
            "ok",
            "",
            false,
            vec![],
            Default::default(),
        );
        assert!(!ok);
        let sess = fs.get_current_session().unwrap();
        assert!(sess.provenance_incomplete);
    }

    #[test]
    fn test_budget_coverage_truncates_with_warning() {
        let mut coverage = crate::provenance::ProvenanceCoverage {
            tracked_active_change_ids: (0..50).map(|i| format!("change-{i:03}")).collect(),
            verified_active_change_ids: (0..50).map(|i| format!("change-{i:03}")).collect(),
            unverified_active_change_ids: vec![],
            diverged_change_ids: vec![],
            unlinked_change_ids: vec![],
            untracked_changed_files: vec![],
            provenance_incomplete: false,
        };
        let mut warnings = Vec::new();
        budget_coverage(&mut coverage, 600, &mut warnings);
        let len = serde_json::to_string(&coverage).unwrap().len();
        assert!(len <= 600, "coverage still too large: {len}");
        assert!(warnings.iter().any(|w| w.contains("truncated")));
        // Small coverage is untouched.
        let mut small = crate::provenance::ProvenanceCoverage::default();
        let mut w2 = Vec::new();
        budget_coverage(&mut small, 600, &mut w2);
        assert!(w2.is_empty());
    }
}
