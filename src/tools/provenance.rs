//! Provenance integration helpers (session-backed, no repomap DB).
//!
//! `ChangeCommitted` is recorded for every tracked workspace mutation
//! (`fs_write`, `edit`, `apply_patch`, transactional semantic edit, `undo`)
//! as a Doge-observed `before -> after` transaction; verification
//! observations come only from real execution paths (`execute_process`,
//! `/test`, `/lint`). No manual `record_verification` tool exists by design.

use crate::llm::types::{ToolDef, ToolFunctionDef};
use crate::provenance::{
    ChangeCommittedEvent, ChangeKind, ChangeTarget, DirectiveObservedEvent, DirectiveOrigin,
    FileStateEvidence, ProvenanceAttribution, ProvenanceEvent, ProvenanceEventEnvelope,
    ProvenanceStore, diff_hash_for, directive_content_hash,
};
use crate::tools::FsTools;

/// Record an observed user directive. Never logs content — only ids, origin,
/// and hashes.
///
/// Returns the directive id (envelope event id) on success. On failure,
/// returns an error so the caller can mark `provenance_incomplete` and
/// continue with `directive_id = None` (a directive failure never aborts the
/// agent turn itself).
pub fn record_directive_observed(
    fs_tools: &FsTools,
    origin: DirectiveOrigin,
    raw_input: &str,
    effective_instruction: &str,
) -> anyhow::Result<ProvenanceEventEnvelope> {
    let Some(ctx) = fs_tools.current_session_storage_context() else {
        anyhow::bail!("no current session for directive recording");
    };
    let event = ProvenanceEvent::DirectiveObserved(DirectiveObservedEvent {
        origin,
        raw_input: raw_input.to_string(),
        raw_input_hash: directive_content_hash(raw_input),
        effective_instruction: effective_instruction.to_string(),
        effective_instruction_hash: directive_content_hash(effective_instruction),
    });
    let store = ProvenanceStore::new(ctx.session_dir);
    let envelope = store.append(&ctx.session_id, event)?;
    tracing::info!(
        directive_id = %envelope.event_id,
        origin = origin.as_str(),
        "provenance.directive_observed"
    );
    Ok(envelope)
}

/// Generic commit recorder for any [`crate::tools::mutation::MutationReceipt`].
///
/// Resolves the current plan item, freezes its requirement links, computes
/// `predecessor_change_id` from the file mutation chain, and appends a v3
/// `ChangeCommitted`. Returns `Ok(None)` when there is no current session
/// (nothing to attach to) or when the path cannot relativize under the
/// project root (outside tracked scope; the caller reports this as a scope
/// warning, not a failure). Append failures are returned as `Err` so the
/// caller can mark `provenance_incomplete` without rolling back the source
/// change.
///
/// `attribution` carries the current directive id. Planned mutations freeze
/// `plan_item_id` + its requirement ids; unplanned mutations still carry the
/// directive id with empty requirement ids (never fully orphaned when a turn
/// context exists).
pub fn record_committed_mutation(
    fs_tools: &FsTools,
    receipt: &crate::tools::mutation::MutationReceipt,
    reverts_change_id: Option<String>,
) -> anyhow::Result<Option<ProvenanceEventEnvelope>> {
    record_committed_mutation_with_attribution(
        fs_tools,
        receipt,
        reverts_change_id,
        &ProvenanceAttribution::none(),
    )
}

/// Attribution-aware commit recorder. Tool handlers must pass
/// `runtime.attribution`; non-agent paths use `ProvenanceAttribution::none()`.
pub fn record_committed_mutation_with_attribution(
    fs_tools: &FsTools,
    receipt: &crate::tools::mutation::MutationReceipt,
    reverts_change_id: Option<String>,
    attribution: &ProvenanceAttribution,
) -> anyhow::Result<Option<ProvenanceEventEnvelope>> {
    let Some(ctx) = fs_tools.current_session_storage_context() else {
        tracing::warn!("provenance.record_skipped: no current session");
        return Ok(None);
    };
    // Single snapshot: read plan once, derive both plan item and requirement
    // links (avoids duplicate I/O per spec §149).
    let (plan_item_id, requirement_ids) = match fs_tools.plan_read() {
        Ok(plan) => {
            let current = crate::provenance::current_in_progress_plan_item(&plan.items);
            let reqs = current
                .as_deref()
                .and_then(|id| plan.items.iter().find(|i| i.id == id))
                .map(|item| item.requirement_ids.clone())
                .unwrap_or_default();
            (current, reqs)
        }
        Err(e) => {
            tracing::debug!(error = %e, "provenance plan read failed; recording unlinked change");
            (None, Vec::new())
        }
    };
    let canonical_path = crate::tools::mutation::canonicalize_for_scope(&receipt.path);
    let canonical_root =
        crate::tools::mutation::canonicalize_for_scope(&fs_tools.config.project_root);
    let Ok(file) = crate::analysis::normalize_relative_path(&canonical_root, &canonical_path)
    else {
        // Outside-project path: durable provenance stores project-relative
        // paths only, so skip without failing. The caller (`finalize_mutation`)
        // reports this as a scope warning.
        tracing::debug!("provenance.record_skipped: outside project root");
        return Ok(None);
    };

    let before = FileStateEvidence {
        exists: receipt.before.exists,
        content_hash: receipt.before.content_hash.clone(),
        byte_len: receipt.before.byte_len,
    };
    let after = FileStateEvidence {
        exists: receipt.after.exists,
        content_hash: receipt.after.content_hash.clone(),
        byte_len: receipt.after.byte_len,
    };
    let target = match &receipt.target {
        crate::tools::mutation::MutationTargetReceipt::File => ChangeTarget::File,
        crate::tools::mutation::MutationTargetReceipt::SemanticSymbol {
            symbol_id,
            before_fingerprint,
            after_fingerprint,
        } => ChangeTarget::SemanticSymbol {
            symbol_id: symbol_id.clone(),
            before_fingerprint: before_fingerprint.clone(),
            after_fingerprint: after_fingerprint.clone(),
        },
    };
    // Predecessor: latest tracked event for this file whose `after` equals
    // this receipt's `before`. Broken chains stay unlinked (never guessed).
    let predecessor_change_id = match ProvenanceStore::new(ctx.session_dir.clone()).load_all() {
        Ok(loaded) => ProvenanceStore::find_predecessor(&loaded.events, &file, &before),
        Err(e) => {
            tracing::debug!(error = %e, "provenance predecessor lookup failed");
            None
        }
    };

    tracing::info!(
        kind = ?receipt.kind,
        file = %file,
        predecessor = predecessor_change_id.as_deref().unwrap_or("none"),
        "provenance.change_committed"
    );

    let event = ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
        transaction_id: String::new(),
        directive_id: attribution.directive_id.clone(),
        plan_item_id,
        requirement_ids,
        change_kind: receipt.kind,
        file,
        target,
        before,
        after,
        predecessor_change_id,
        reverts_change_id,
        diff: receipt.diff.clone(),
        diff_hash: diff_hash_for(&receipt.diff),
        lines_added: receipt.lines_added,
        lines_removed: receipt.lines_removed,
    });
    let store = ProvenanceStore::new(ctx.session_dir);
    let envelope = store.append(&ctx.session_id, event)?;
    Ok(Some(envelope))
}

/// Record a transactional semantic edit as `ChangeCommitted`.
///
/// Takes the exact pre-write content so the receipt (and any future undo
/// linkage) carries the real `before` state. Prefer the generic
/// [`record_committed_mutation`] path for new mutation kinds.
pub fn record_semantic_change(
    fs_tools: &FsTools,
    result: &crate::features::semantic_edit::SemanticEditResult,
    before_content: &str,
) -> anyhow::Result<Option<ProvenanceEventEnvelope>> {
    let receipt = crate::tools::mutation::MutationReceipt {
        kind: ChangeKind::SemanticEdit,
        path: result.file.clone(),
        before: crate::tools::mutation::MutationSnapshot {
            resolved_path: None,
            identity: None,
            exists: true,
            content: Some(before_content.to_string()),
            content_hash: result.before_file_hash.clone(),
            byte_len: result.before_byte_len,
        },
        after: crate::tools::mutation::MutationSnapshot {
            resolved_path: None,
            identity: None,
            exists: true,
            content: None,
            content_hash: result.after_file_hash.clone(),
            byte_len: result.after_byte_len,
        },
        target: crate::tools::mutation::MutationTargetReceipt::SemanticSymbol {
            symbol_id: result.symbol_id.as_str().to_string(),
            before_fingerprint: result.before_fingerprint.as_str().to_string(),
            after_fingerprint: result.after_fingerprint.as_str().to_string(),
        },
        diff: result.diff.clone(),
        lines_added: result.lines_added,
        lines_removed: result.lines_removed,
    };
    record_committed_mutation(fs_tools, &receipt, None)
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
/// ids of currently active changes, with frozen requirement ids.
///
/// Must be called before the process starts so mid-run changes are never
/// attributed to the running command. Requirement ids are the union of active
/// change requirement ids, falling back to current plan item links.
pub fn capture_verification_context_for_fs(
    fs_tools: &FsTools,
) -> crate::provenance::VerificationContext {
    capture_verification_context_for_fs_with_attribution(fs_tools, &ProvenanceAttribution::none())
}

/// Attribution-aware capture. Agent paths pass `runtime.attribution`;
/// manual `/test` / `/lint` use `None` (plan/requirement links still saved).
pub fn capture_verification_context_for_fs_with_attribution(
    fs_tools: &FsTools,
    attribution: &ProvenanceAttribution,
) -> crate::provenance::VerificationContext {
    let plan_items = match fs_tools.plan_read() {
        Ok(plan) => plan.items,
        Err(_) => Vec::new(),
    };
    let (active_ids, change_reqs) = match load_current_events(fs_tools) {
        Ok(Some(loaded)) => {
            let active =
                crate::provenance::active_change_ids(&fs_tools.config.project_root, &loaded.events);
            let active_set: std::collections::HashSet<&str> =
                active.iter().map(String::as_str).collect();
            // change_id -> requirement_ids for active changes only.
            let mut per_change: Vec<Vec<String>> = Vec::new();
            for env in &loaded.events {
                if !active_set.contains(env.event_id.as_str()) {
                    continue;
                }
                if let crate::provenance::ProvenanceEvent::ChangeCommitted(c) = &env.event
                    && !c.requirement_ids.is_empty()
                {
                    per_change.push(c.requirement_ids.clone());
                }
            }
            (active, per_change)
        }
        _ => (Vec::new(), Vec::new()),
    };
    let plan_links: std::collections::HashMap<String, Vec<String>> = plan_items
        .iter()
        .map(|i| (i.id.clone(), i.requirement_ids.clone()))
        .collect();
    crate::provenance::verification::capture_verification_context_full(
        &plan_items,
        &active_ids,
        &change_reqs,
        attribution.directive_id.clone(),
        &plan_links,
    )
}

/// Pre-execution capture with obligation attribution frozen.
///
/// Must be called before the process starts. In addition to the base
/// requirement/change snapshot, it freezes which current obligations this
/// invocation matches (id + binding hash). Later plan edits never rewrite it.
pub fn capture_verification_context_for_invocation(
    fs_tools: &FsTools,
    attribution: &ProvenanceAttribution,
    kind: crate::provenance::VerificationKind,
    program: &str,
    args: &[String],
) -> crate::provenance::VerificationContext {
    let plan_items = match fs_tools.plan_read() {
        Ok(plan) => plan.items,
        Err(_) => Vec::new(),
    };
    let (active_ids, change_reqs, active_plan_ids) = match load_current_events(fs_tools) {
        Ok(Some(loaded)) => {
            let active =
                crate::provenance::active_change_ids(&fs_tools.config.project_root, &loaded.events);
            let active_set: std::collections::HashSet<&str> =
                active.iter().map(String::as_str).collect();
            let mut per_change: Vec<Vec<String>> = Vec::new();
            let mut plan_ids: Vec<String> = Vec::new();
            for env in &loaded.events {
                if !active_set.contains(env.event_id.as_str()) {
                    continue;
                }
                if let crate::provenance::ProvenanceEvent::ChangeCommitted(c) = &env.event {
                    if !c.requirement_ids.is_empty() {
                        per_change.push(c.requirement_ids.clone());
                    }
                    if let Some(pid) = &c.plan_item_id
                        && !plan_ids.contains(pid)
                    {
                        plan_ids.push(pid.clone());
                    }
                }
            }
            (active, per_change, plan_ids)
        }
        _ => (Vec::new(), Vec::new(), Vec::new()),
    };
    let plan_links: std::collections::HashMap<String, Vec<String>> = plan_items
        .iter()
        .map(|i| (i.id.clone(), i.requirement_ids.clone()))
        .collect();
    let mut ctx = crate::provenance::verification::capture_verification_context_full(
        &plan_items,
        &active_ids,
        &change_reqs,
        attribution.directive_id.clone(),
        &plan_links,
    );
    ctx.matched_obligations =
        crate::provenance::obligations::capture_verification_context_for_invocation(
            &plan_items,
            &active_plan_ids,
            kind,
            program,
            args,
        );
    ctx
}

/// Add bounded code-state observations without changing frozen attribution.
pub async fn prepare_verification_snapshot(
    fs_tools: &FsTools,
    context: &mut crate::provenance::VerificationContext,
    cancel: Option<tokio_util::sync::CancellationToken>,
) {
    let Some(storage) = fs_tools.current_session_storage_context() else {
        return;
    };
    let loaded = load_current_events(fs_tools);
    let mut references = std::collections::BTreeSet::new();
    let mut incomplete;
    match loaded {
        Ok(Some(events)) => {
            incomplete = !events.warnings.is_empty();
            for env in events.events {
                if env.session_id != storage.session_id {
                    incomplete = true;
                    continue;
                }
                if let ProvenanceEvent::ChangeCommitted(c) = env.event {
                    references.insert(c.file);
                }
            }
        }
        _ => incomplete = true,
    }
    let mut record = crate::features::verification_snapshot::begin(
        &fs_tools.config.project_root,
        references,
        cancel,
    )
    .await;
    if incomplete {
        record.start.status = crate::features::verification_snapshot::CaptureStatus::Partial;
        record
            .start
            .diagnostics
            .push(crate::features::verification_snapshot::Diagnostic::FileUnavailable);
    }
    context.execution_workspace = Some(record);
}
pub async fn finish_verification_snapshot(
    fs_tools: &FsTools,
    context: &mut crate::provenance::VerificationContext,
    cancel: Option<tokio_util::sync::CancellationToken>,
) {
    if let Some(record) = context.execution_workspace.take() {
        context.execution_workspace = Some(
            crate::features::verification_snapshot::finish(
                &fs_tools.config.project_root,
                record,
                cancel,
            )
            .await,
        );
    }
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
    structured_test_result: Option<
        Box<crate::features::structured_test_results::StructuredTestResult>,
    >,
) -> bool {
    let event =
        crate::provenance::build_verification_event(crate::provenance::VerificationRecordInput {
            structured_test_result,
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
            structured_test_result: None,
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

/// Requirement ids as a sorted set for order-insensitive comparison.
fn sorted_ids(ids: &[String]) -> Vec<&str> {
    let mut sorted: Vec<&str> = ids.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted
}

/// Diff two plan snapshots into per-item transitions.
///
/// Only items whose status, content, parent, requirement links, or
/// verification obligations changed (plus new/deleted items) are included.
/// A no-op write produces no transitions. Requirement-link-only and
/// obligation-only edits also produce a transition.
/// Requirement id order is insignificant: links compare as sorted sets so a
/// pure reorder is a no-op. Obligation order is insignificant (sorted by id);
/// `args_prefix` order is significant and preserved.
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
                if b.status != a.status
                    || b.content != a.content
                    || b.parent_id != a.parent_id
                    || sorted_ids(&b.requirement_ids) != sorted_ids(&a.requirement_ids)
                    || !crate::tools::plan::obligations_equal(
                        &b.verification_obligations,
                        &a.verification_obligations,
                    )
                {
                    out.push(crate::provenance::PlanItemTransition {
                        plan_item_id: id.to_string(),
                        parent_id: a.parent_id.clone(),
                        content: a.content.clone(),
                        before_status: Some(b.status.clone()),
                        after_status: Some(a.status.clone()),
                        before_requirement_ids: b.requirement_ids.clone(),
                        before_verification_obligations: b.verification_obligations.clone(),
                        after_requirement_ids: a.requirement_ids.clone(),
                        after_verification_obligations: a.verification_obligations.clone(),
                    });
                }
            }
            (None, Some(a)) => out.push(crate::provenance::PlanItemTransition {
                plan_item_id: id.to_string(),
                parent_id: a.parent_id.clone(),
                content: a.content.clone(),
                before_status: None,
                after_status: Some(a.status.clone()),
                before_requirement_ids: Vec::new(),
                before_verification_obligations: Vec::new(),
                after_requirement_ids: a.requirement_ids.clone(),
                after_verification_obligations: a.verification_obligations.clone(),
            }),
            (Some(b), None) => out.push(crate::provenance::PlanItemTransition {
                plan_item_id: id.to_string(),
                parent_id: b.parent_id.clone(),
                content: b.content.clone(),
                before_status: Some(b.status.clone()),
                after_status: None,
                before_requirement_ids: b.requirement_ids.clone(),
                before_verification_obligations: b.verification_obligations.clone(),
                after_requirement_ids: Vec::new(),
                after_verification_obligations: Vec::new(),
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
    record_plan_changed_with_attribution(fs_tools, transitions, &ProvenanceAttribution::none())
}

/// Attribution-aware plan recorder. Agent tool paths pass
/// `runtime.attribution`; direct UI operations use `None`.
pub fn record_plan_changed_with_attribution(
    fs_tools: &FsTools,
    transitions: Vec<crate::provenance::PlanItemTransition>,
    attribution: &ProvenanceAttribution,
) -> anyhow::Result<Option<ProvenanceEventEnvelope>> {
    if transitions.is_empty() {
        return Ok(None);
    }
    let Some(ctx) = fs_tools.current_session_storage_context() else {
        return Ok(None);
    };
    let event = ProvenanceEvent::PlanChanged(crate::provenance::PlanChangedEvent {
        directive_id: attribution.directive_id.clone(),
        changes: transitions,
    });
    let store = ProvenanceStore::new(ctx.session_dir);
    Ok(Some(store.append(&ctx.session_id, event)?))
}

/// Warnings for newly completed items.
///
/// - When a completed item has verification obligations, warn for obligations
///   not in `ObservedPassing` (pending/failing/stale/diverged/reverted/mixed).
///   Passing items may be omitted for brevity. Research-only items with
///   `NoLinkedChange` produce no warning. Never blocks completion.
/// - When an item has no obligations, keep the legacy behavior: warn when
///   linked active changes have no successful verification observation.
pub fn plan_completion_warnings(
    fs_tools: &FsTools,
    transitions: &[crate::provenance::PlanItemTransition],
) -> Vec<String> {
    let completed: Vec<&crate::provenance::PlanItemTransition> = transitions
        .iter()
        .filter(|t| {
            t.after_status.as_deref() == Some("completed")
                && t.before_status.as_deref() != Some("completed")
        })
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
    // Obligation coverage (current plan + events). Best-effort: on plan-read
    // failure fall back to transition obligations only.
    let current_plan_items = fs_tools.plan_read().map(|p| p.items).unwrap_or_default();
    let obligation_coverages = crate::provenance::obligations::compute_obligation_coverage(
        &fs_tools.config.project_root,
        &loaded.events,
        &current_plan_items,
    );
    let mut warnings = Vec::new();
    for t in completed {
        // Obligations for this item: prefer current plan, fall back to transition.
        // Check `before` as well so deleting obligations in the completion write
        // cannot silence the warning (must not rewrite history to skip checks).
        let item_coverages: Vec<&crate::provenance::obligations::ObligationCoverage> =
            obligation_coverages
                .iter()
                .filter(|c| c.plan_item_id == t.plan_item_id)
                .collect();
        // If current plan has no obligations for this item (e.g. plan read
        // failed, item deleted, or obligations removed in this write), use the
        // transition's before/after obligations with a lightweight state note.
        // For now, empty means legacy path.
        let has_obligations = if !item_coverages.is_empty() {
            true
        } else {
            !t.after_verification_obligations.is_empty()
                || !t.before_verification_obligations.is_empty()
        };
        if has_obligations && !item_coverages.is_empty() {
            let mut incomplete: Vec<String> = Vec::new();
            for c in &item_coverages {
                match c.state {
                    crate::provenance::obligations::VerificationObligationEvidenceState::ObservedPassing => {}
                    crate::provenance::obligations::VerificationObligationEvidenceState::NoLinkedChange => {}
                    _ => {
                        incomplete.push(format!(
                            "- {}: {}",
                            c.obligation_id,
                            c.state.as_str()
                        ));
                    }
                }
            }
            if !incomplete.is_empty() {
                warnings.push(format!(
                    "Plan item '{}' was completed with incomplete verification obligations:\n{}",
                    t.plan_item_id,
                    incomplete.join("\n")
                ));
            }
            // Obligation path wins; skip legacy duplicate for this item.
            continue;
        }
        if has_obligations {
            // Transition has obligations but coverage missing (plan read failed).
            // Fall back to a generic obligation warning rather than legacy.
            warnings.push(format!(
                "Plan item '{}' was completed with verification obligations; obligation state could not be resolved.",
                t.plan_item_id
            ));
            continue;
        }
        // Legacy path: no obligations.
        let item_id = t.plan_item_id.as_str();
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

/// Soft warning for completed items with committed changes but no requirement
/// link. Never blocks. Research-only items (no linked changes) produce no
/// warning.
pub fn plan_requirement_link_warnings(
    fs_tools: &FsTools,
    transitions: &[crate::provenance::PlanItemTransition],
) -> Vec<String> {
    let completed: Vec<&crate::provenance::PlanItemTransition> = transitions
        .iter()
        .filter(|t| {
            t.after_status.as_deref() == Some("completed")
                && t.before_status.as_deref() != Some("completed")
        })
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
    let mut out = Vec::new();
    for t in completed {
        if !t.after_requirement_ids.is_empty() {
            continue;
        }
        let has_changes = resolved.iter().any(|r| {
            r.state == crate::provenance::ActiveChangeState::Active
                && r.plan_item_id.as_deref() == Some(t.plan_item_id.as_str())
        });
        if has_changes {
            out.push(format!(
                "Plan item '{}' has committed changes but no requirement link.",
                t.plan_item_id
            ));
        }
    }
    out
}

/// Enrich a plain diff payload with provenance evidence.
///
/// Git diff collection stays separate; this helper only queries provenance.
/// Never fails the diff: missing sessions, plan-read failures, or provenance
/// load failures become `evidence_warnings` and the original diff is kept.
pub fn enrich_diff_review_with_evidence(
    fs_tools: &FsTools,
    payload: crate::diff_review::DiffReviewPayload,
) -> crate::diff_review::DiffReviewPayload {
    let project_root = &fs_tools.config.project_root;
    let (events, mut warnings) = match load_current_events(fs_tools) {
        Ok(Some(loaded)) => (loaded.events, loaded.warnings),
        Ok(None) => (
            Vec::new(),
            vec!["No current session for diff evidence.".to_string()],
        ),
        Err(e) => {
            let mut p = payload;
            p.evidence_warnings.push(format!(
                "Diff evidence unavailable: provenance load failed: {e}"
            ));
            return p;
        }
    };
    let plan_items = match fs_tools.plan_read() {
        Ok(plan) => plan.items,
        Err(e) => {
            let mut p = payload;
            p.evidence_warnings
                .push(format!("Diff evidence incomplete: plan read failed: {e}"));
            // Still try with empty plan (file -> change linkage only).
            let (evidence, mut w) = crate::diff_review::build_diff_review_evidence(
                project_root,
                &events,
                &[],
                &p.files,
            );
            p.evidence = evidence;
            p.evidence_warnings.append(&mut w);
            p.evidence_warnings.append(&mut warnings);
            return p;
        }
    };
    let (evidence, mut build_warnings) = crate::diff_review::build_diff_review_evidence(
        project_root,
        &events,
        &plan_items,
        &payload.files,
    );
    let mut out = payload;
    out.evidence = evidence;
    out.evidence_warnings.append(&mut build_warnings);
    out.evidence_warnings.append(&mut warnings);
    out
}

/// `provenance_read` tool: read-only inspection of plan/change/verification
/// links plus coverage. Never writes evidence.
pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "provenance_read".to_string(),
            description: "Read provenance linking directives, requirements, plan steps, committed changes, and observed verification commands. Use this to inspect what changed, which checks ran afterward, and where evidence is incomplete.".to_string(),
            // Optional fields retain omission/default semantics; strict requires all properties.
            strict: Some(false),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "plan_item_id": {"type": "string", "description": "Filter to one plan item id"},
                    "directive_id": {"type": "string", "description": "Filter to one directive id"},
                    "requirement_id": {"type": "string", "description": "Filter to one requirement id"},
                    "verification_obligation_id": {"type": "string", "description": "Filter to one verification obligation id (plan obligation transitions + matched verifications)"},
                    "event_types": {
                        "type": "array",
                        "items": {"type": "string", "enum": ["directive_observed", "requirement_changed", "plan_changed", "change_committed", "verification_observed"]},
                        "description": "Filter to these event types"
                    },
                    "include_diff": {"type": "boolean", "default": false, "description": "Include diffs/excerpts (still budgeted)"},
                    "include_content": {"type": "boolean", "default": false, "description": "Include full directive text (default returns preview + hashes only)"},
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
    pub directive_id: Option<String>,
    #[serde(default)]
    pub requirement_id: Option<String>,
    #[serde(default)]
    pub verification_obligation_id: Option<String>,
    #[serde(default)]
    pub event_types: Option<Vec<crate::provenance::ProvenanceEventType>>,
    #[serde(default)]
    pub include_diff: bool,
    #[serde(default)]
    pub include_content: bool,
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
            coverage.reverted_change_ids.len(),
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
        } else if coverage.untracked_changed_files.len() == max {
            coverage.untracked_changed_files.pop();
        } else {
            coverage.reverted_change_ids.pop();
        }
        truncated = true;
    }
    if truncated {
        warnings.push(
            "Provenance coverage ID lists truncated to stay within response_budget_chars; use plan_item_id/directive_id/requirement_id/event_types filters for a narrower view."
                .to_string(),
        );
    }
}

/// Execute `provenance_read` against the current session.
pub async fn provenance_read(
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

    // Filter by type + plan/directive/requirement/obligation, preserving (timestamp, event_id) order.
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
            if let Some(item) = &args.plan_item_id
                && !event_matches_plan_item(&env.event, item)
            {
                return false;
            }
            if let Some(directive) = &args.directive_id
                && !event_matches_directive(env, directive)
            {
                return false;
            }
            if let Some(req) = &args.requirement_id
                && !event_matches_requirement(&env.event, req)
            {
                return false;
            }
            if let Some(ob) = &args.verification_obligation_id
                && !event_matches_obligation(&env.event, ob)
            {
                return false;
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
    // Lifecycle states for change summaries (computed once over all events).
    let states: std::collections::HashMap<String, String> =
        crate::provenance::resolve_active_states(&fs_tools.config.project_root, &loaded.events)
            .into_iter()
            .map(|r| {
                let s = match r.state {
                    crate::provenance::ActiveChangeState::Active => "active",
                    crate::provenance::ActiveChangeState::Superseded => "superseded",
                    crate::provenance::ActiveChangeState::Diverged => "diverged",
                    crate::provenance::ActiveChangeState::Missing => "missing",
                    crate::provenance::ActiveChangeState::Reverted => "reverted",
                };
                (r.change_id, s.to_string())
            })
            .collect();
    let snapshot_records: Vec<_> = filtered[start..end]
        .iter()
        .filter_map(|env| match &env.event {
            ProvenanceEvent::VerificationObserved(v) => v.execution_workspace.as_deref(),
            _ => None,
        })
        .collect();
    let current = if snapshot_records.is_empty() {
        None
    } else {
        let references = snapshot_records
            .iter()
            .flat_map(|r| {
                r.start
                    .files
                    .iter()
                    .chain(r.end.iter().flat_map(|s| s.files.iter()))
            })
            .map(|f| f.path.clone())
            .collect();
        Some(
            crate::features::verification_snapshot::capture(
                &fs_tools.config.project_root,
                references,
                None,
            )
            .await,
        )
    };
    for env in filtered[start..end].iter() {
        let mut summary = summarize_event(env, args.include_diff, args.include_content, &states);
        if let ProvenanceEvent::VerificationObserved(v) = &env.event {
            let comparison = crate::features::verification_snapshot::compare_current(
                v.execution_workspace.as_deref(),
                current.as_ref(),
            );
            if let Some(obj) = summary.as_object_mut() {
                let state = comparison.state;
                obj.insert("current_code_state".into(), serde_json::json!({"state":state,
                    "changed":comparison.differences.changed.len(),"added":comparison.differences.added.len(),
                    "deleted":comparison.differences.deleted.len(),"unknown":comparison.differences.unknown.len()}));
                if let Some(record) = &v.execution_workspace {
                    obj.insert(
                        "execution_workspace".into(),
                        crate::features::verification_snapshot::summary(record),
                    );
                }
            }
            if comparison.state
                != crate::features::verification_snapshot::CurrentState::MatchesStart
                && !warnings
                    .iter()
                    .any(|w| w.starts_with("Displayed verification code-state"))
            {
                warnings.push("Displayed verification code-state correspondence is not fully confirmed; inspect per-event current_code_state. Command outcome and historical coverage are independent.".into());
            }
        }
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

fn event_matches_plan_item(event: &ProvenanceEvent, item: &str) -> bool {
    match event {
        ProvenanceEvent::DirectiveObserved(_) | ProvenanceEvent::RequirementChanged(_) => false,
        ProvenanceEvent::PlanChanged(e) => e.changes.iter().any(|c| c.plan_item_id == item),
        ProvenanceEvent::ChangeCommitted(c) => c.plan_item_id.as_deref() == Some(item),
        ProvenanceEvent::VerificationObserved(v) => v.plan_item_id.as_deref() == Some(item),
    }
}

fn event_matches_directive(env: &ProvenanceEventEnvelope, directive: &str) -> bool {
    match &env.event {
        ProvenanceEvent::DirectiveObserved(_) => env.event_id == directive,
        ProvenanceEvent::RequirementChanged(e) => e.directive_id == directive,
        ProvenanceEvent::PlanChanged(e) => e.directive_id.as_deref() == Some(directive),
        ProvenanceEvent::ChangeCommitted(c) => c.directive_id.as_deref() == Some(directive),
        ProvenanceEvent::VerificationObserved(v) => v.directive_id.as_deref() == Some(directive),
    }
}

fn event_matches_requirement(event: &ProvenanceEvent, req: &str) -> bool {
    match event {
        ProvenanceEvent::DirectiveObserved(_) => false,
        ProvenanceEvent::RequirementChanged(e) => e.changes.iter().any(|c| c.requirement_id == req),
        ProvenanceEvent::PlanChanged(e) => e.changes.iter().any(|c| {
            c.before_requirement_ids.iter().any(|r| r == req)
                || c.after_requirement_ids.iter().any(|r| r == req)
        }),
        ProvenanceEvent::ChangeCommitted(c) => c.requirement_ids.iter().any(|r| r == req),
        ProvenanceEvent::VerificationObserved(v) => v.requirement_ids.iter().any(|r| r == req),
    }
}

fn event_matches_obligation(event: &ProvenanceEvent, obligation_id: &str) -> bool {
    match event {
        ProvenanceEvent::DirectiveObserved(_)
        | ProvenanceEvent::RequirementChanged(_)
        | ProvenanceEvent::ChangeCommitted(_) => false,
        ProvenanceEvent::PlanChanged(e) => e.changes.iter().any(|c| {
            c.before_verification_obligations
                .iter()
                .any(|o| o.id == obligation_id)
                || c.after_verification_obligations
                    .iter()
                    .any(|o| o.id == obligation_id)
        }),
        ProvenanceEvent::VerificationObserved(v) => {
            v.matched_obligations.iter().any(|m| m.id == obligation_id)
        }
    }
}

fn summarize_event(
    env: &ProvenanceEventEnvelope,
    include_diff: bool,
    include_content: bool,
    states: &std::collections::HashMap<String, String>,
) -> serde_json::Value {
    match &env.event {
        ProvenanceEvent::DirectiveObserved(d) => {
            let mut v = serde_json::json!({
                "event_id": env.event_id,
                "type": "directive_observed",
                "timestamp": env.timestamp,
                "directive_id": env.event_id,
                "origin": d.origin,
                "raw_input_hash": d.raw_input_hash,
                "effective_instruction_hash": d.effective_instruction_hash,
                "preview": d.preview(),
            });
            if include_content && let Some(obj) = v.as_object_mut() {
                obj.insert(
                    "raw_input".to_string(),
                    serde_json::Value::String(d.raw_input.clone()),
                );
                obj.insert(
                    "effective_instruction".to_string(),
                    serde_json::Value::String(d.effective_instruction.clone()),
                );
            }
            // Budget directive text even when explicitly requested.
            if include_content && let Some(obj) = v.as_object_mut() {
                for key in ["raw_input", "effective_instruction"] {
                    if let Some(s) = obj.get(key).and_then(|v| v.as_str()).map(str::to_string) {
                        let budgeted = crate::tools::budget::head_tail_truncate(
                            &s,
                            PROVENANCE_READ_DIFF_BUDGET,
                        )
                        .text;
                        obj.insert(key.to_string(), serde_json::Value::String(budgeted));
                    }
                }
            }
            v
        }
        ProvenanceEvent::RequirementChanged(e) => serde_json::json!({
            "event_id": env.event_id,
            "type": "requirement_changed",
            "timestamp": env.timestamp,
            "directive_id": e.directive_id,
            "changes": e.changes.iter().map(|c| serde_json::json!({
                "requirement_id": c.requirement_id,
                "before": c.before,
                "after": c.after,
            })).collect::<Vec<_>>(),
        }),
        ProvenanceEvent::PlanChanged(e) => {
            serde_json::json!({
                "event_id": env.event_id,
                "type": "plan_changed",
                "timestamp": env.timestamp,
                "directive_id": e.directive_id,
                "changes": e.changes.iter().map(|c| serde_json::json!({
                    "plan_item_id": c.plan_item_id,
                    "before_status": c.before_status,
                    "after_status": c.after_status,
                    "before_requirement_ids": c.before_requirement_ids,
                    "after_requirement_ids": c.after_requirement_ids,
                    "before_verification_obligations": c.before_verification_obligations.iter().map(|o| serde_json::json!({
                        "id": o.id,
                        "kind": o.kind,
                    })).collect::<Vec<_>>(),
                    "after_verification_obligations": c.after_verification_obligations.iter().map(|o| serde_json::json!({
                        "id": o.id,
                        "kind": o.kind,
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            })
        }
        ProvenanceEvent::ChangeCommitted(c) => {
            let (target_scope, symbol_id) = match &c.target {
                crate::provenance::ChangeTarget::File => ("file", None),
                crate::provenance::ChangeTarget::SemanticSymbol { symbol_id, .. } => {
                    ("semantic_symbol", Some(symbol_id.clone()))
                }
            };
            let mut v = serde_json::json!({
                "event_id": env.event_id,
                "type": "change_committed",
                "timestamp": env.timestamp,
                "directive_id": c.directive_id,
                "plan_item_id": c.plan_item_id,
                "requirement_ids": c.requirement_ids,
                "change_kind": c.change_kind,
                "target_scope": target_scope,
                "file": c.file,
                "lines_added": c.lines_added,
                "lines_removed": c.lines_removed,
                "diff_hash": c.diff_hash,
                "before_hash": c.before.content_hash,
                "after_hash": c.after.content_hash,
                "predecessor_change_id": c.predecessor_change_id,
                "reverts_change_id": c.reverts_change_id,
            });
            if let Some(symbol_id) = symbol_id
                && let Some(obj) = v.as_object_mut()
            {
                obj.insert(
                    "symbol_id".to_string(),
                    serde_json::Value::String(symbol_id),
                );
            }
            // Attach resolved lifecycle state when the workspace is available.
            // Best-effort: never fails the read.
            let state = states
                .get(&env.event_id)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            if let Some(obj) = v.as_object_mut() {
                obj.insert("state".to_string(), serde_json::Value::String(state));
            }
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
                "directive_id": v.directive_id,
                "plan_item_id": v.plan_item_id,
                "requirement_ids": v.requirement_ids,
                "verification_kind": v.verification_kind,
                "source": v.source,
                "program": v.command.program,
                "args": v.command.args,
                "success": v.outcome.success,
                "status": v.outcome.status,
                "exit_code": v.outcome.exit_code,
                "timed_out": v.outcome.timed_out,
                "observed_change_ids": v.observed_change_ids,
                "matched_obligations": v.matched_obligations,
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
            save_state: Default::default(),
            current_lease: None,
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
        assert_eq!(def.function.strict, Some(false));
        let args: ProvenanceReadArgs = serde_json::from_value(serde_json::json!({}))
            .expect("all read filters remain optional");
        assert!(args.plan_item_id.is_none());
        assert!(!args.include_diff && !args.include_content);
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
            requirement_ids: Vec::new(),
            verification_obligations: Vec::new(),
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
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
            PlanItem {
                id: "step-2".into(),
                parent_id: None,
                content: "b".into(),
                status: "pending".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
        ];
        assert_eq!(
            crate::provenance::current_in_progress_plan_item(&items),
            None
        );
    }

    #[tokio::test]
    async fn test_provenance_read_pagination_and_filters() {
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
                        target: crate::provenance::ChangeTarget::SemanticSymbol {
                            symbol_id: format!("sym-{i}"),
                            before_fingerprint: "a".into(),
                            after_fingerprint: "b".into(),
                        },
                        before: crate::provenance::FileStateEvidence {
                            exists: true,
                            content_hash: None,
                            byte_len: None,
                        },
                        after: crate::provenance::FileStateEvidence {
                            exists: true,
                            content_hash: None,
                            byte_len: None,
                        },
                        predecessor_change_id: None,
                        reverts_change_id: None,
                        diff: "d".into(),
                        diff_hash: "blake3:x".into(),
                        lines_added: 1,
                        lines_removed: 0,
                        directive_id: None,
                        requirement_ids: Vec::new(),
                    }),
                )
                .unwrap();
        }
        let args = ProvenanceReadArgs {
            page_size: Some(2),
            cursor: Some(0),
            ..Default::default()
        };
        let resp = provenance_read(&fs, args).await.unwrap();
        assert_eq!(resp.events.len(), 2);
        assert_eq!(resp.next_cursor, Some(2));
        let args2 = ProvenanceReadArgs {
            page_size: Some(2),
            cursor: Some(2),
            ..Default::default()
        };
        let resp2 = provenance_read(&fs, args2).await.unwrap();
        assert_eq!(resp2.events.len(), 1);
        assert_eq!(resp2.next_cursor, None);
        // Plan filter.
        let args3 = ProvenanceReadArgs {
            plan_item_id: Some("step-1".into()),
            ..Default::default()
        };
        let resp3 = provenance_read(&fs, args3).await.unwrap();
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
        let resp4 = provenance_read(&fs, args4).await.unwrap();
        assert!(resp4.events.iter().any(|e| e.get("diff").is_some()));
    }

    #[test]
    fn test_verification_context_race_snapshot() {
        // Change A captured; Change B lands before completion; observed keeps A only.
        let ctx_a = crate::provenance::VerificationContext {
            execution_context: None,
            execution_workspace: None,
            plan_item_id: Some("step-1".into()),
            observed_change_ids: vec!["change-A".to_string()],
            matched_obligations: Vec::new(),
            directive_id: None,
            requirement_ids: Vec::new(),
        };
        let event = crate::provenance::build_verification_event(
            crate::provenance::VerificationRecordInput {
                structured_test_result: None,
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
    #[tokio::test]
    async fn snapshot_current_summary_is_compact_and_detects_later_changes() {
        let root = tempfile::tempdir().expect("project");
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(root.path())
                .status()
                .expect("init git")
                .success()
        );
        std::fs::write(root.path().join("Cargo.lock"), "PRIVATE-LOCKFILE").expect("lockfile");
        let (fs, _) = fs_with_session(root.path());
        let mut context = crate::provenance::VerificationContext::default();
        prepare_verification_snapshot(&fs, &mut context, None).await;
        finish_verification_snapshot(&fs, &mut context, None).await;
        assert!(record_tui_test_verification(
            &fs,
            "cargo",
            &["test".into()],
            true,
            "completed",
            Some(0),
            false,
            "",
            "",
            false,
            vec![],
            context,
            None,
        ));
        let before = provenance_read(&fs, ProvenanceReadArgs::default())
            .await
            .expect("before read");
        assert_eq!(
            before.events[0]["current_code_state"]["state"],
            "matches_start"
        );
        std::fs::write(root.path().join("Cargo.lock"), "changed lockfile").expect("change");
        let after = provenance_read(&fs, ProvenanceReadArgs::default())
            .await
            .expect("after read");
        assert_eq!(
            after.events[0]["current_code_state"]["state"],
            "differs_from_start"
        );
        assert_eq!(after.events[0]["success"], true);
        let json = serde_json::to_string(&after).expect("JSON");
        assert!(!json.contains("PRIVATE-LOCKFILE"));
        assert!(!json.contains("manifest_digest"));
        assert!(json.len() < 6000);
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
            save_state: Default::default(),
            current_lease: None,
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
                    directive_id: None,
                }),
            )
            .unwrap();
        assert!(store.events_dir().exists());
        drop(fs); // External delete follows completion of the active owner.
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
                    requirement_ids: Vec::new(),
                    verification_obligations: Vec::new(),
                },
                crate::tools::plan::PlanItem {
                    id: "step-2".into(),
                    parent_id: None,
                    content: "b".into(),
                    status: "in_progress".into(),
                    requirement_ids: Vec::new(),
                    verification_obligations: Vec::new(),
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
        let env = record_semantic_change(&fs, &result, &current)
            .unwrap()
            .expect("recorded");
        match &env.event {
            ProvenanceEvent::ChangeCommitted(c) => {
                assert_eq!(c.file, "src/lib.rs");
                assert!(!c.file.starts_with('/'));
                match &c.target {
                    crate::provenance::ChangeTarget::SemanticSymbol {
                        symbol_id,
                        before_fingerprint,
                        after_fingerprint,
                    } => {
                        assert_eq!(symbol_id, result.symbol_id.as_str());
                        assert_eq!(before_fingerprint, result.before_fingerprint.as_str());
                        assert_eq!(after_fingerprint, result.after_fingerprint.as_str());
                    }
                    _ => panic!("expected semantic target"),
                }
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
                    requirement_ids: Vec::new(),
                    verification_obligations: Vec::new(),
                },
                crate::tools::plan::PlanItem {
                    id: "step-2".into(),
                    parent_id: None,
                    content: "b".into(),
                    status: "completed".into(),
                    requirement_ids: Vec::new(),
                    verification_obligations: Vec::new(),
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
        let env = record_semantic_change(&fs, &result, &current)
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
        let events_dir =
            crate::provenance::ProvenanceStore::new(ctx.session_dir.clone()).current_events_path();
        std::fs::create_dir_all(events_dir.parent().unwrap()).unwrap();
        std::fs::write(&events_dir, "not-a-dir").unwrap();
        // `before_content` is irrelevant here: the append fails before any
        // receipt content is used.
        let err = record_semantic_change(&fs, &result, "").expect_err("must fail");
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
        // Tracked: semantic change for src/lib.rs, written then recorded
        // (production write-then-record order, so the recorded `after`
        // state matches the workspace).
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
        std::fs::write(&file, &candidate).unwrap();
        record_semantic_change(&fs, &result, &current).unwrap();
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
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
            PlanItem {
                id: "b".into(),
                parent_id: None,
                content: "y".into(),
                status: "in_progress".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
            PlanItem {
                id: "gone".into(),
                parent_id: None,
                content: "z".into(),
                status: "pending".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
        ];
        let after = vec![
            PlanItem {
                id: "a".into(),
                parent_id: None,
                content: "x".into(),
                status: "in_progress".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
            PlanItem {
                id: "b".into(),
                parent_id: None,
                content: "y".into(),
                status: "completed".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
            PlanItem {
                id: "new".into(),
                parent_id: None,
                content: "n".into(),
                status: "pending".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
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
            requirement_ids: Vec::new(),
            verification_obligations: Vec::new(),
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
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
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
        let change_env = record_semantic_change(&fs, &result, &current)
            .unwrap()
            .unwrap();
        // Complete without verification -> warning.
        let res = fs
            .plan_write(
                vec![crate::tools::plan::PlanItem {
                    id: "step-1".into(),
                    parent_id: None,
                    content: "work".into(),
                    status: "completed".into(),
                    requirement_ids: Vec::new(),
                    verification_obligations: Vec::new(),
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
                        requirement_ids: Vec::new(),
                        verification_obligations: Vec::new(),
                    },
                    crate::tools::plan::PlanItem {
                        id: "research".into(),
                        parent_id: None,
                        content: "read docs".into(),
                        status: "completed".into(),
                        requirement_ids: Vec::new(),
                        verification_obligations: Vec::new(),
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
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
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
        let change_env = record_semantic_change(&fs, &result, &current)
            .unwrap()
            .unwrap();
        // Successful verification observing the change.
        let ctx = fs.current_session_storage_context().unwrap();
        let store = ProvenanceStore::new(ctx.session_dir);
        let event = crate::provenance::build_verification_event(
            crate::provenance::VerificationRecordInput {
                structured_test_result: None,
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
                    execution_context: None,
                    execution_workspace: None,
                    plan_item_id: Some("step-1".into()),
                    observed_change_ids: vec![change_env.event_id.clone()],
                    matched_obligations: Vec::new(),
                    directive_id: None,
                    requirement_ids: Vec::new(),
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
                    requirement_ids: Vec::new(),
                    verification_obligations: Vec::new(),
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
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
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
                    requirement_ids: Vec::new(),
                    verification_obligations: Vec::new(),
                }],
                crate::tools::plan::PlanWriteMode::Replace,
            )
            .unwrap();
        assert!(res.warnings.iter().any(|w| w.contains("incomplete")));
    }

    #[tokio::test]
    async fn test_provenance_read_budget_malformed_and_coverage() {
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
                    target: crate::provenance::ChangeTarget::SemanticSymbol {
                        symbol_id: "s".into(),
                        before_fingerprint: "a".into(),
                        after_fingerprint: "b".into(),
                    },
                    before: crate::provenance::FileStateEvidence {
                        exists: true,
                        content_hash: None,
                        byte_len: None,
                    },
                    after: crate::provenance::FileStateEvidence {
                        exists: true,
                        content_hash: None,
                        byte_len: None,
                    },
                    predecessor_change_id: None,
                    reverts_change_id: None,
                    diff: big_diff,
                    diff_hash: "blake3:x".into(),
                    lines_added: 1,
                    lines_removed: 0,
                    directive_id: None,
                    requirement_ids: Vec::new(),
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
        .await
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
        .await
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
                structured_test_result: None,
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
            save_state: Default::default(),
            current_lease: None,
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
                    directive_id: None,
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
        let events_dir =
            crate::provenance::ProvenanceStore::new(ctx.session_dir.clone()).current_events_path();
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
            None,
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
            reverted_change_ids: vec![],
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

    fn obligation_for_test(id: &str) -> crate::tools::plan::VerificationObligation {
        crate::tools::plan::VerificationObligation {
            id: id.to_string(),
            description: "desc".to_string(),
            kind: crate::provenance::VerificationKind::Test,
            command: Some(crate::tools::plan::VerificationCommandMatcher {
                program: "cargo".to_string(),
                args_prefix: vec!["test".to_string()],
            }),
        }
    }

    #[test]
    fn test_diff_plan_transitions_obligation_only() {
        use crate::tools::plan::PlanItem;
        let before = vec![PlanItem {
            id: "step-1".into(),
            parent_id: None,
            content: "a".into(),
            status: "pending".into(),
            requirement_ids: Vec::new(),
            verification_obligations: vec![],
        }];
        let after = vec![PlanItem {
            id: "step-1".into(),
            parent_id: None,
            content: "a".into(),
            status: "pending".into(),
            requirement_ids: Vec::new(),
            verification_obligations: vec![obligation_for_test("vo-1")],
        }];
        let t = diff_plan_transitions(&before, &after);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].after_verification_obligations.len(), 1);
        // Pure reorder is no-op.
        let a = vec![PlanItem {
            id: "step-1".into(),
            parent_id: None,
            content: "a".into(),
            status: "pending".into(),
            requirement_ids: Vec::new(),
            verification_obligations: vec![
                obligation_for_test("vo-1"),
                obligation_for_test("vo-2"),
            ],
        }];
        let b = vec![PlanItem {
            id: "step-1".into(),
            parent_id: None,
            content: "a".into(),
            status: "pending".into(),
            requirement_ids: Vec::new(),
            verification_obligations: vec![
                obligation_for_test("vo-2"),
                obligation_for_test("vo-1"),
            ],
        }];
        assert!(diff_plan_transitions(&a, &b).is_empty());
    }

    #[test]
    fn test_completion_warning_all_passing_no_warning() {
        // Obligation passing -> no warning. Uses file-based active change +
        // verification with frozen binding.
        let (_proj, fs) = setup();
        let ob = obligation_for_test("vo-1");
        fs.plan_write(
            vec![crate::tools::plan::PlanItem {
                id: "step-1".into(),
                parent_id: None,
                content: "work".into(),
                status: "in_progress".into(),
                requirement_ids: Vec::new(),
                verification_obligations: vec![ob.clone()],
            }],
            crate::tools::plan::PlanWriteMode::Replace,
        )
        .unwrap();
        let root = fs.config.project_root.clone();
        std::fs::write(root.join("a.txt"), "h0\n").unwrap();
        std::fs::write(root.join("a.txt"), "h1\n").unwrap();
        // Record change via FsTools finalize path (text edit).
        let receipt = crate::tools::mutation::MutationReceipt {
            kind: crate::provenance::ChangeKind::TextEdit,
            path: root.join("a.txt"),
            before: crate::tools::mutation::MutationSnapshot {
                resolved_path: None,
                identity: None,
                exists: true,
                content: Some("h0\n".to_string()),
                content_hash: Some(crate::provenance::file_content_hash("h0\n")),
                byte_len: Some(3),
            },
            after: crate::tools::mutation::MutationSnapshot {
                resolved_path: None,
                identity: None,
                exists: true,
                content: Some("h1\n".to_string()),
                content_hash: Some(crate::provenance::file_content_hash("h1\n")),
                byte_len: Some(3),
            },
            target: crate::tools::mutation::MutationTargetReceipt::File,
            diff: "d".to_string(),
            lines_added: 1,
            lines_removed: 1,
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let report = rt.block_on(fs.finalize_mutation(
            receipt,
            crate::tools::FinalizeMutationOptions {
                record_undo: false,
                reverts_change_id: None,
                attribution: crate::provenance::ProvenanceAttribution::none(),
            },
        ));
        let change_id = report.change_id.expect("change recorded");
        // Verification observing the change with correct binding.
        let binding = crate::provenance::obligations::obligation_binding_hash("step-1", &[], &ob);
        let ctx = crate::provenance::VerificationContext {
            execution_context: None,
            execution_workspace: None,
            directive_id: None,
            plan_item_id: Some("step-1".to_string()),
            requirement_ids: vec![],
            observed_change_ids: vec![change_id.clone()],
            matched_obligations: vec![crate::provenance::VerificationObligationRef {
                id: "vo-1".to_string(),
                binding_hash: binding,
            }],
        };
        let event = crate::provenance::build_verification_event(
            crate::provenance::VerificationRecordInput {
                structured_test_result: None,
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
                context: ctx,
                extra_warnings: vec![],
            },
        );
        let store = ProvenanceStore::new(fs.current_session_storage_context().unwrap().session_dir);
        let sid = fs.current_session_storage_context().unwrap().session_id;
        store
            .append(&sid, ProvenanceEvent::VerificationObserved(event))
            .unwrap();
        // Complete -> no obligation warning.
        let res = fs
            .plan_write(
                vec![crate::tools::plan::PlanItem {
                    id: "step-1".into(),
                    parent_id: None,
                    content: "work".into(),
                    status: "completed".into(),
                    requirement_ids: Vec::new(),
                    verification_obligations: vec![ob],
                }],
                crate::tools::plan::PlanWriteMode::Replace,
            )
            .unwrap();
        assert!(
            !res.warnings
                .iter()
                .any(|w| w.contains("verification obligations")),
            "warnings: {:?}",
            res.warnings
        );
    }

    #[test]
    fn test_completion_warning_pending_and_stale() {
        let (_proj, fs) = setup();
        let ob = obligation_for_test("vo-pending");
        fs.plan_write(
            vec![crate::tools::plan::PlanItem {
                id: "step-1".into(),
                parent_id: None,
                content: "work".into(),
                status: "in_progress".into(),
                requirement_ids: Vec::new(),
                verification_obligations: vec![ob.clone()],
            }],
            crate::tools::plan::PlanWriteMode::Replace,
        )
        .unwrap();
        let root = fs.config.project_root.clone();
        std::fs::write(root.join("a.txt"), "h0\n").unwrap();
        std::fs::write(root.join("a.txt"), "h1\n").unwrap();
        let receipt = crate::tools::mutation::MutationReceipt {
            kind: crate::provenance::ChangeKind::TextEdit,
            path: root.join("a.txt"),
            before: crate::tools::mutation::MutationSnapshot {
                resolved_path: None,
                identity: None,
                exists: true,
                content: Some("h0\n".to_string()),
                content_hash: Some(crate::provenance::file_content_hash("h0\n")),
                byte_len: Some(3),
            },
            after: crate::tools::mutation::MutationSnapshot {
                resolved_path: None,
                identity: None,
                exists: true,
                content: Some("h1\n".to_string()),
                content_hash: Some(crate::provenance::file_content_hash("h1\n")),
                byte_len: Some(3),
            },
            target: crate::tools::mutation::MutationTargetReceipt::File,
            diff: "d".to_string(),
            lines_added: 1,
            lines_removed: 1,
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(fs.finalize_mutation(
            receipt,
            crate::tools::FinalizeMutationOptions {
                record_undo: false,
                reverts_change_id: None,
                attribution: crate::provenance::ProvenanceAttribution::none(),
            },
        ));
        // Complete with pending (no verification) -> warning.
        let res = fs
            .plan_write(
                vec![crate::tools::plan::PlanItem {
                    id: "step-1".into(),
                    parent_id: None,
                    content: "work".into(),
                    status: "completed".into(),
                    requirement_ids: Vec::new(),
                    verification_obligations: vec![ob],
                }],
                crate::tools::plan::PlanWriteMode::Replace,
            )
            .unwrap();
        assert!(
            res.warnings
                .iter()
                .any(|w| w.contains("vo-pending") && w.contains("pending"))
        );
    }

    #[test]
    fn test_completion_legacy_when_no_obligation() {
        // No obligation -> legacy semantics preserved (covered by existing
        // test_plan_completion_warnings, but assert explicitly here).
        let (_proj, fs) = setup();
        fs.plan_write(
            vec![crate::tools::plan::PlanItem {
                id: "step-1".into(),
                parent_id: None,
                content: "work".into(),
                status: "in_progress".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            }],
            crate::tools::plan::PlanWriteMode::Replace,
        )
        .unwrap();
        let root = fs.config.project_root.clone();
        std::fs::write(root.join("a.txt"), "h0\n").unwrap();
        // No linked change -> no warning (research-only).
        let res = fs
            .plan_write(
                vec![crate::tools::plan::PlanItem {
                    id: "step-1".into(),
                    parent_id: None,
                    content: "work".into(),
                    status: "completed".into(),
                    requirement_ids: Vec::new(),
                    verification_obligations: Vec::new(),
                }],
                crate::tools::plan::PlanWriteMode::Replace,
            )
            .unwrap();
        assert!(!res.warnings.iter().any(|w| w.contains("step-1")));
    }

    #[tokio::test]
    async fn test_provenance_read_obligation_filter() {
        let (_proj, fs) = setup();
        let ob = obligation_for_test("vo-filter");
        // Plan with obligation.
        fs.plan_write(
            vec![crate::tools::plan::PlanItem {
                id: "step-1".into(),
                parent_id: None,
                content: "work".into(),
                status: "pending".into(),
                requirement_ids: Vec::new(),
                verification_obligations: vec![ob],
            }],
            crate::tools::plan::PlanWriteMode::Replace,
        )
        .unwrap();
        let args = ProvenanceReadArgs {
            verification_obligation_id: Some("vo-filter".to_string()),
            ..Default::default()
        };
        let resp = provenance_read(&fs, args).await.unwrap();
        // PlanChanged with obligation should be found.
        assert!(!resp.events.is_empty());
        assert!(
            resp.events
                .iter()
                .any(|e| e.to_string().contains("vo-filter"))
        );
    }

    #[test]
    fn test_capture_invocation_includes_matched_obligations() {
        let (_proj, fs) = setup();
        let ob = obligation_for_test("vo-cap");
        fs.plan_write(
            vec![crate::tools::plan::PlanItem {
                id: "step-1".into(),
                parent_id: None,
                content: "work".into(),
                status: "in_progress".into(),
                requirement_ids: Vec::new(),
                verification_obligations: vec![ob],
            }],
            crate::tools::plan::PlanWriteMode::Replace,
        )
        .unwrap();
        let ctx = capture_verification_context_for_invocation(
            &fs,
            &crate::provenance::ProvenanceAttribution::none(),
            crate::provenance::VerificationKind::Test,
            "cargo",
            &["test".to_string()],
        );
        assert!(ctx.matched_obligations.iter().any(|m| m.id == "vo-cap"));
        assert!(
            ctx.matched_obligations[0]
                .binding_hash
                .starts_with("blake3:")
        );
    }

    #[test]
    fn test_tui_test_verification_preserves_matched_obligations() {
        let (_proj, fs) = setup();
        let ob = obligation_for_test("vo-tui");
        fs.plan_write(
            vec![crate::tools::plan::PlanItem {
                id: "step-1".into(),
                parent_id: None,
                content: "work".into(),
                status: "in_progress".into(),
                requirement_ids: Vec::new(),
                verification_obligations: vec![ob],
            }],
            crate::tools::plan::PlanWriteMode::Replace,
        )
        .unwrap();
        let ctx = capture_verification_context_for_invocation(
            &fs,
            &crate::provenance::ProvenanceAttribution::none(),
            crate::provenance::VerificationKind::Test,
            "cargo",
            &["test".to_string()],
        );
        assert!(!ctx.matched_obligations.is_empty());
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
            ctx,
            None,
        );
        assert!(ok);
        let loaded = load_current_events(&fs).unwrap().unwrap();
        let verif = loaded
            .events
            .iter()
            .find_map(|e| match &e.event {
                ProvenanceEvent::VerificationObserved(v) => Some(v),
                _ => None,
            })
            .expect("verification recorded");
        assert!(verif.matched_obligations.iter().any(|m| m.id == "vo-tui"));
    }

    #[test]
    fn test_tui_lint_verification_preserves_matched_obligations() {
        let (_proj, fs) = setup();
        let lint_ob = crate::tools::plan::VerificationObligation {
            id: "vo-lint".to_string(),
            description: "lint".to_string(),
            kind: crate::provenance::VerificationKind::Lint,
            command: Some(crate::tools::plan::VerificationCommandMatcher {
                program: "cargo".to_string(),
                args_prefix: vec!["clippy".to_string()],
            }),
        };
        fs.plan_write(
            vec![crate::tools::plan::PlanItem {
                id: "step-1".into(),
                parent_id: None,
                content: "work".into(),
                status: "in_progress".into(),
                requirement_ids: Vec::new(),
                verification_obligations: vec![lint_ob],
            }],
            crate::tools::plan::PlanWriteMode::Replace,
        )
        .unwrap();
        let kind = classify_lint_command("cargo", &["clippy".to_string()]).unwrap();
        let ctx = capture_verification_context_for_invocation(
            &fs,
            &crate::provenance::ProvenanceAttribution::none(),
            kind,
            "cargo",
            &["clippy".to_string()],
        );
        assert!(ctx.matched_obligations.iter().any(|m| m.id == "vo-lint"));
        let ok = record_tui_lint_verification(
            &fs,
            kind,
            "cargo",
            &["clippy".to_string()],
            true,
            "completed",
            Some(0),
            false,
            "ok",
            "",
            false,
            vec![],
            ctx,
        );
        assert!(ok);
        let loaded = load_current_events(&fs).unwrap().unwrap();
        let verif = loaded
            .events
            .iter()
            .find_map(|e| match &e.event {
                ProvenanceEvent::VerificationObserved(v) => Some(v),
                _ => None,
            })
            .expect("verification recorded");
        assert!(verif.matched_obligations.iter().any(|m| m.id == "vo-lint"));
    }

    #[test]
    fn test_completion_deleting_obligations_still_warns() {
        // Deleting obligations in the completion write must not silence the
        // warning: before-obligations count as obligations.
        let (_proj, fs) = setup();
        let ob = obligation_for_test("vo-keep");
        fs.plan_write(
            vec![crate::tools::plan::PlanItem {
                id: "step-1".into(),
                parent_id: None,
                content: "work".into(),
                status: "in_progress".into(),
                requirement_ids: Vec::new(),
                verification_obligations: vec![ob],
            }],
            crate::tools::plan::PlanWriteMode::Replace,
        )
        .unwrap();
        let root = fs.config.project_root.clone();
        std::fs::write(root.join("a.txt"), "h0\n").unwrap();
        std::fs::write(root.join("a.txt"), "h1\n").unwrap();
        let receipt = crate::tools::mutation::MutationReceipt {
            kind: crate::provenance::ChangeKind::TextEdit,
            path: root.join("a.txt"),
            before: crate::tools::mutation::MutationSnapshot {
                resolved_path: None,
                identity: None,
                exists: true,
                content: Some("h0\n".to_string()),
                content_hash: Some(crate::provenance::file_content_hash("h0\n")),
                byte_len: Some(3),
            },
            after: crate::tools::mutation::MutationSnapshot {
                resolved_path: None,
                identity: None,
                exists: true,
                content: Some("h1\n".to_string()),
                content_hash: Some(crate::provenance::file_content_hash("h1\n")),
                byte_len: Some(3),
            },
            target: crate::tools::mutation::MutationTargetReceipt::File,
            diff: "d".to_string(),
            lines_added: 1,
            lines_removed: 1,
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(fs.finalize_mutation(
            receipt,
            crate::tools::FinalizeMutationOptions {
                record_undo: false,
                reverts_change_id: None,
                attribution: crate::provenance::ProvenanceAttribution::none(),
            },
        ));
        // Complete while removing the obligation: must still warn (generic
        // obligation warning, not silent legacy pass).
        let res = fs
            .plan_write(
                vec![crate::tools::plan::PlanItem {
                    id: "step-1".into(),
                    parent_id: None,
                    content: "work".into(),
                    status: "completed".into(),
                    requirement_ids: Vec::new(),
                    verification_obligations: Vec::new(),
                }],
                crate::tools::plan::PlanWriteMode::Replace,
            )
            .unwrap();
        assert!(
            res.warnings
                .iter()
                .any(|w| w.contains("verification obligations")),
            "warnings: {:?}",
            res.warnings
        );
    }
    #[test]
    fn go_test_workflow_records_typed_counts_and_keeps_failure_and_frozen_links() {
        let (_root, fs) = setup();
        let raw = "{\"Action\":\"start\",\"Package\":\"example.invalid/fixture\"}\n{\"Action\":\"fail\",\"Package\":\"example.invalid/fixture\"}\n";
        let args = vec!["test".into(), "-json".into(), "./...".into()];
        let result = crate::features::structured_test_results::observe(
            "go",
            &args,
            raw,
            crate::features::structured_test_results::CaptureState::Complete,
            false,
        );
        let context = crate::provenance::VerificationContext {
            observed_change_ids: vec!["frozen-change".into()],
            matched_obligations: vec![crate::provenance::VerificationObligationRef {
                id: "frozen-obligation".into(),
                binding_hash: "blake3:frozen".into(),
            }],
            ..Default::default()
        };
        assert!(record_tui_test_verification(
            &fs,
            "go",
            &args,
            false,
            "completed",
            Some(1),
            false,
            raw,
            "compile details",
            false,
            vec![],
            context,
            result
        ));
        let loaded = load_current_events(&fs).unwrap().unwrap();
        let verification = loaded
            .events
            .iter()
            .find_map(|e| match &e.event {
                ProvenanceEvent::VerificationObserved(v) => Some(v),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            verification.source,
            crate::provenance::VerificationSource::TuiTest
        );
        assert!(!verification.outcome.success);
        assert_eq!(
            verification
                .structured_test_result
                .as_ref()
                .unwrap()
                .test_count(),
            Some(0)
        );
        assert_eq!(verification.observed_change_ids, vec!["frozen-change"]);
        assert_eq!(verification.matched_obligations[0].id, "frozen-obligation");
        assert!(verification.stdout_excerpt.contains("Action"));
        assert_eq!(verification.stderr_excerpt, "compile details");
    }
}
