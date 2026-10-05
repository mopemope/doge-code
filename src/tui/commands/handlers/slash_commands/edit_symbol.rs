use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc::Sender};

use anyhow::{Context, Result, anyhow};
use diffy::create_patch;
use tokio::fs;
use tokio_util::sync::CancellationToken;

use crate::config::AppConfig;
use crate::features::semantic_edit::{
    PreparedSemanticEdit, SemanticEditError, apply_edit, prepare_edit_async,
};
use crate::jobs::{JobKind, JobRunOutcome, JobScope, JobSpec, JobStartError, WorkspaceAccess};
use crate::llm::{
    EditTarget, LlmErrorKind, OpenAIClient, SymbolEditRequest, SymbolEditResponse,
    build_symbol_edit_chat_request, parse_legacy_symbol_edit_response, parse_symbol_edit_response,
};
use crate::tools::FsTools;
use crate::tools::apply_patch::{ApplyPatchParams, apply_patch as apply_patch_tool};
use crate::tui::channel::SenderExt;
use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;

/// Handle `/edit-symbol` via a transactional semantic edit job.
pub fn handle_edit_symbol(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    let (file, line) = match current_file_and_line(ui) {
        Some(v) => v,
        None => {
            ui.push_log("No active file/line context for symbol edit.");
            return;
        }
    };

    let instruction = match latest_instruction(ui) {
        Some(text) => text,
        None => {
            ui.push_log("Provide edit instruction before running /edit-symbol.");
            return;
        }
    };

    if let Some(active_id) = executor.jobs.foreground_id()
        && let Some(active) = executor.jobs.get_snapshot(active_id)
    {
        ui.push_log(format!(
            "[Job] {} is already running. Use /jobs or /cancel {}.",
            active.id, active.id
        ));
        return;
    }

    if executor.ui_tx.is_none() {
        executor.ui_tx = ui.sender();
    }
    let Some(ui_tx) = executor.ui_tx.clone() else {
        ui.push_log("UI channel unavailable - cannot run semantic edit.");
        return;
    };

    let project_root = executor.cfg.project_root.clone();
    let model = executor.cfg.model.clone();
    let client = executor.client.clone();
    let tools = executor.tools.clone();
    let repomap = executor.repomap.clone();
    let file_input = PathBuf::from(&file);

    let target_display = make_relative_display(&file_input, &project_root);
    ui.push_log(format!(
        "[edit-symbol] Targeting {target_display}:{line} (instruction: {instruction})"
    ));

    let spec = JobSpec::new(
        JobKind::SemanticEdit,
        JobScope::Foreground,
        WorkspaceAccess::Write,
        format!("semantic edit {target_display}:{line}"),
    );
    let spawn = executor.jobs.spawn(spec, move |ctx| async move {
        run_semantic_edit_job(
            project_root,
            file_input,
            line,
            instruction,
            model,
            client,
            tools,
            repomap,
            ui_tx,
            ctx.cancellation_token(),
        )
        .await
    });
    match spawn {
        Ok(id) => {
            ui.push_log(format!("Semantic edit started as {id}."));
        }
        Err(JobStartError::ForegroundBusy { active }) => {
            ui.push_log(format!(
                "[Job] {} is already running. Use /jobs or /cancel {}.",
                active.id, active.id
            ));
        }
        Err(JobStartError::ShuttingDown) => {
            ui.push_log("Job manager is shutting down.");
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_semantic_edit_job(
    project_root: PathBuf,
    file_input: PathBuf,
    line: u32,
    instruction: String,
    model: String,
    client: Option<OpenAIClient>,
    tools: FsTools,
    repomap: Arc<tokio::sync::RwLock<Option<crate::analysis::RepoMap>>>,
    ui_tx: Sender<String>,
    cancellation: CancellationToken,
) -> JobRunOutcome {
    ui_tx.send_logged("::status:processing".to_string());

    if cancellation.is_cancelled() {
        ui_tx.send_logged("[edit-symbol] Cancelled before prepare.".to_string());
        ui_tx.send_logged("::status:cancelled".to_string());
        return JobRunOutcome::Cancelled;
    }

    let prepared = match prepare_edit_async(&project_root, &file_input, line).await {
        Ok(p) => p,
        Err(e) => {
            ui_tx.send_logged(format!("[edit-symbol][error] Prepare failed: {e}"));
            ui_tx.send_logged("::status:error".to_string());
            return match &e {
                SemanticEditError::WriteFailed(_) | SemanticEditError::AnalysisFailed(_) => {
                    JobRunOutcome::Failed {
                        message: e.to_string(),
                    }
                }
                _ => JobRunOutcome::Completed,
            };
        }
    };

    ui_tx.send_logged(format!(
        "[edit-symbol] Targeting {} ({}) {}",
        prepared.name,
        prepared.file.display(),
        prepared.symbol_id
    ));

    let Some(client) = client else {
        ui_tx.send_logged(
            "[edit-symbol][error] LLM client is not configured. Use --api-key to set it."
                .to_string(),
        );
        ui_tx.send_logged("::status:error".to_string());
        return JobRunOutcome::Failed {
            message: "LLM client is not configured".to_string(),
        };
    };

    // Record the observed directive after the job is accepted, the target
    // prepared, and the client confirmed — matching TUI agent-turn semantics
    // (busy/missing-key records nothing). Even when the text matches an
    // earlier agent directive, this is a new activity. A recording failure
    // never aborts the edit: continue with None.
    let attribution = match crate::tools::provenance::record_directive_observed(
        &tools,
        crate::provenance::DirectiveOrigin::SemanticEdit,
        &instruction,
        &instruction,
    ) {
        Ok(env) => crate::provenance::ProvenanceAttribution::with_directive(env.event_id),
        Err(e) => {
            tracing::warn!(error = %e, "provenance.directive_record_failed");
            let _ = tools.mark_current_session_provenance_failure();
            crate::provenance::ProvenanceAttribution::none()
        }
    };

    let req = SymbolEditRequest {
        model: model.clone(),
        target: EditTarget {
            file: prepared.file.clone(),
            start_line: prepared.start_line as u32,
            end_line: prepared.end_line as u32,
            name: Some(prepared.name.clone()),
            kind: kind_display(&prepared.kind).to_string(),
        },
        original_code: prepared.original_source.clone(),
        instruction: instruction.clone(),
        symbol_id: Some(prepared.symbol_id.as_str().to_string()),
        parent: prepared.parent.clone(),
    };
    let chat_req = build_symbol_edit_chat_request(&req);
    let crate::llm::types::ChatRequest { messages, .. } = chat_req;

    ui_tx.send_logged(format!(
        "[edit-symbol] Requesting LLM edit for {}...",
        prepared.symbol_id
    ));
    // Usage checkpoint immediately before the provider request. The shared
    // client ledger is observed before/after so the session persists the
    // provider attempt even when the response later fails validation.
    let usage_checkpoint = match capture_manual_usage_checkpoint(&client, &tools) {
        Ok(checkpoint) => checkpoint,
        Err(e) => {
            ui_tx.send_logged(format!("[edit-symbol][error] Usage checkpoint failed: {e}"));
            ui_tx.send_logged("::status:error".to_string());
            return JobRunOutcome::Failed {
                message: e.to_string(),
            };
        }
    };
    let llm_result = client
        .chat_once(&model, messages, Some(cancellation.clone()))
        .await;
    let usage_after = client.usage_snapshot();
    // Attribute immediately after the request returns, before any response
    // validation: empty/parse failures still consumed provider usage.
    if let Err(e) = finish_manual_usage_attribution(usage_checkpoint, &usage_after, &tools) {
        ui_tx.send_logged(format!(
            "[edit-symbol][error] Usage attribution failed: {e}"
        ));
        ui_tx.send_logged("::status:error".to_string());
        return JobRunOutcome::Failed {
            message: e.to_string(),
        };
    }
    let response = match llm_result {
        Ok(choice) => choice.content,
        Err(e) => {
            if cancellation.is_cancelled()
                || e.downcast_ref::<LlmErrorKind>() == Some(&LlmErrorKind::Cancelled)
            {
                ui_tx.send_logged("[edit-symbol] Cancelled while waiting for LLM.".to_string());
                ui_tx.send_logged("::status:cancelled".to_string());
                return JobRunOutcome::Cancelled;
            }
            ui_tx.send_logged(format!("[edit-symbol][error] LLM request failed: {e}"));
            ui_tx.send_logged("::status:error".to_string());
            return JobRunOutcome::Failed {
                message: e.to_string(),
            };
        }
    };

    if cancellation.is_cancelled() {
        ui_tx.send_logged("[edit-symbol] Cancelled after LLM response.".to_string());
        ui_tx.send_logged("::status:cancelled".to_string());
        return JobRunOutcome::Cancelled;
    }

    if response.trim().is_empty() {
        ui_tx.send_logged("[edit-symbol][error] LLM returned an empty response.".to_string());
        ui_tx.send_logged("::status:error".to_string());
        return JobRunOutcome::Completed;
    }

    let parsed = match parse_symbol_edit_response(&response) {
        Ok(resp) => resp,
        Err(e) => {
            ui_tx.send_logged(format!(
                "[edit-symbol][error] Failed to parse response: {e}"
            ));
            ui_tx.send_logged(format!(
                "[edit-symbol] Raw response snippet:\n{}",
                truncate_for_log(&response)
            ));
            ui_tx.send_logged("::status:error".to_string());
            return JobRunOutcome::Completed;
        }
    };
    let Some(replacement) = parsed.replacement else {
        ui_tx.send_logged(
            "[edit-symbol][error] LLM response did not include a replacement block.".to_string(),
        );
        ui_tx.send_logged("::status:error".to_string());
        return JobRunOutcome::Completed;
    };

    if cancellation.is_cancelled() {
        ui_tx.send_logged("[edit-symbol] Cancelled before mutation.".to_string());
        ui_tx.send_logged("::status:cancelled".to_string());
        return JobRunOutcome::Cancelled;
    }

    match apply_edit(&prepared, &replacement, &project_root).await {
        Ok((result, candidate_map, before_content)) => {
            commit_semantic_edit_success(
                &project_root,
                &tools,
                &repomap,
                &ui_tx,
                &prepared,
                &result,
                candidate_map,
                before_content,
                &attribution,
            )
            .await;
            JobRunOutcome::Completed
        }
        Err(e) => {
            if cancellation.is_cancelled() {
                ui_tx.send_logged("[edit-symbol] Cancelled during commit.".to_string());
                ui_tx.send_logged("::status:cancelled".to_string());
                return JobRunOutcome::Cancelled;
            }
            ui_tx.send_logged(format!("[edit-symbol][error] Failed to apply edit: {e}"));
            ui_tx.send_logged("::status:error".to_string());
            match &e {
                SemanticEditError::WriteFailed(_) | SemanticEditError::AnalysisFailed(_) => {
                    JobRunOutcome::Failed {
                        message: e.to_string(),
                    }
                }
                _ => JobRunOutcome::Completed,
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn commit_semantic_edit_success(
    project_root: &Path,
    tools: &FsTools,
    repomap: &Arc<tokio::sync::RwLock<Option<crate::analysis::RepoMap>>>,
    ui_tx: &Sender<String>,
    prepared: &PreparedSemanticEdit,
    result: &crate::features::semantic_edit::SemanticEditResult,
    candidate_map: crate::analysis::RepoMap,
    before_content: String,
    attribution: &crate::provenance::ProvenanceAttribution,
) {
    // Unified commit bookkeeping: build the observed receipt and finalize
    // (provenance -> undo -> session -> context). Source already succeeded;
    // failures here warn without rollback.
    let after_content = std::fs::read_to_string(&prepared.file).unwrap_or_default();
    let receipt = crate::tools::mutation::MutationReceipt {
        kind: crate::provenance::ChangeKind::SemanticEdit,
        path: prepared.file.clone(),
        before: crate::tools::mutation::MutationSnapshot {
            resolved_path: None,
            identity: None,
            exists: true,
            content: Some(before_content),
            content_hash: result.before_file_hash.clone(),
            byte_len: result.before_byte_len,
        },
        after: crate::tools::mutation::MutationSnapshot {
            resolved_path: None,
            identity: None,
            exists: true,
            content: Some(after_content),
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
    let report = tools
        .finalize_mutation(
            receipt,
            crate::tools::FinalizeMutationOptions {
                record_undo: true,
                reverts_change_id: None,
                attribution: attribution.clone(),
            },
        )
        .await;
    for w in &report.warnings {
        ui_tx.send_logged(format!("[provenance][warning] {w}"));
    }

    let relative =
        make_relative_path(&prepared.file, project_root).unwrap_or_else(|| prepared.file.clone());

    {
        let mut guard = repomap.write().await;
        if let Some(map) = guard.as_mut() {
            map.replace_file(&prepared.file, candidate_map);
        }
    }

    ui_tx.send_logged(format!(
        "[edit-symbol] Applied {} ({} +{} -{})",
        prepared.symbol_id,
        make_relative_display(&prepared.file, project_root),
        result.lines_added,
        result.lines_removed
    ));

    match crate::llm::tool_execution::collect_diff_review_payload(project_root, &[relative]).await {
        Ok(Some(payload)) => {
            let enriched =
                crate::tools::provenance::enrich_diff_review_with_evidence(tools, payload);
            if let Ok(json) = serde_json::to_string(&enriched) {
                ui_tx.send_logged(format!("::diff_review:{json}"));
            }
        }
        Ok(None) => {}
        Err(e) => {
            ui_tx.send_logged(format!("[edit-symbol] Diff review unavailable: {e}"));
        }
    }
    ui_tx.send_logged("::status:done".to_string());
}

/// Legacy line-range enqueue used by `/fix`.
///
/// Kept separate from the transactional semantic path. Runs as a managed job
/// so no user-visible work uses a bare `tokio::spawn`.
/// Intentionally shares the Foreground/Write slot with semantic edits (both
/// mutate files, so they must not overlap) and reuses `JobKind::SemanticEdit`
/// so `/jobs` shows a single edit family; the label (`fix ...` vs
/// `semantic edit ...`) distinguishes them. The lenient legacy parser is kept
/// here so `/fix` still accepts diffs, while `/edit-symbol` uses the strict
/// semantic parser.
pub fn enqueue_symbol_edit_request(
    executor: &mut TuiExecutor,
    request: SymbolEditRequest,
    chat_req: crate::llm::ChatRequest,
) -> Result<()> {
    let client = executor
        .client
        .clone()
        .ok_or_else(|| anyhow!("LLM client is not configured. Use --api-key to set it."))?;

    let ui_tx = executor
        .ui_tx
        .clone()
        .ok_or_else(|| anyhow!("UI channel is not available yet"))?;

    if let Some(active_id) = executor.jobs.foreground_id()
        && let Some(active) = executor.jobs.get_snapshot(active_id)
    {
        anyhow::bail!(
            "Foreground job {} is already running. Use /jobs to inspect it or /cancel to stop it.",
            active.id
        );
    }

    let fs_tools = executor.tools.clone();
    let cfg = executor.cfg.clone();

    let crate::llm::types::ChatRequest {
        model, messages, ..
    } = chat_req;

    let symbol_label = format!(
        "{} ({})",
        request.target.name.as_deref().unwrap_or("target"),
        make_relative_display(&request.target.file, &cfg.project_root)
    );

    let spec = JobSpec::new(
        JobKind::SemanticEdit,
        JobScope::Foreground,
        WorkspaceAccess::Write,
        format!("fix {symbol_label}"),
    );
    executor
        .jobs
        .spawn(spec, move |ctx| async move {
            run_legacy_line_edit_job(
                client,
                fs_tools,
                cfg,
                ui_tx,
                model,
                messages,
                request,
                symbol_label,
                ctx.cancellation_token(),
            )
            .await
        })
        .map(|_| ())
        .map_err(|e| anyhow!("{e}"))
}

#[allow(clippy::too_many_arguments)]
async fn run_legacy_line_edit_job(
    client: OpenAIClient,
    fs_tools: FsTools,
    cfg: AppConfig,
    ui_tx: Sender<String>,
    model: String,
    messages: Vec<crate::llm::ChatMessage>,
    request: SymbolEditRequest,
    symbol_label: String,
    cancellation: CancellationToken,
) -> JobRunOutcome {
    ui_tx.send_logged(format!(
        "[edit-symbol] Requesting LLM edit for {symbol_label}..."
    ));
    ui_tx.send_logged("::status:processing".to_string());

    if cancellation.is_cancelled() {
        ui_tx.send_logged("[edit-symbol] Cancelled.".to_string());
        ui_tx.send_logged("::status:cancelled".to_string());
        return JobRunOutcome::Cancelled;
    }

    let usage_checkpoint = match capture_manual_usage_checkpoint(&client, &fs_tools) {
        Ok(checkpoint) => checkpoint,
        Err(e) => {
            ui_tx.send_logged(format!("[edit-symbol][error] Usage checkpoint failed: {e}"));
            ui_tx.send_logged("::status:error".to_string());
            return JobRunOutcome::Failed {
                message: e.to_string(),
            };
        }
    };
    let llm_result = client
        .chat_once(&model, messages, Some(cancellation.clone()))
        .await;
    let usage_after = client.usage_snapshot();
    if let Err(e) = finish_manual_usage_attribution(usage_checkpoint, &usage_after, &fs_tools) {
        ui_tx.send_logged(format!(
            "[edit-symbol][error] Usage attribution failed: {e}"
        ));
        ui_tx.send_logged("::status:error".to_string());
        return JobRunOutcome::Failed {
            message: e.to_string(),
        };
    }
    let response = match llm_result {
        Ok(choice) => choice.content,
        Err(e) => {
            if cancellation.is_cancelled()
                || e.downcast_ref::<LlmErrorKind>() == Some(&LlmErrorKind::Cancelled)
            {
                ui_tx.send_logged("[edit-symbol] Cancelled.".to_string());
                ui_tx.send_logged("::status:cancelled".to_string());
                return JobRunOutcome::Cancelled;
            }
            ui_tx.send_logged(format!("[edit-symbol][error] LLM request failed: {e}"));
            ui_tx.send_logged("::status:error".to_string());
            return JobRunOutcome::Failed {
                message: e.to_string(),
            };
        }
    };

    if cancellation.is_cancelled() {
        ui_tx.send_logged("[edit-symbol] Cancelled.".to_string());
        ui_tx.send_logged("::status:cancelled".to_string());
        return JobRunOutcome::Cancelled;
    }

    if response.trim().is_empty() {
        ui_tx.send_logged("[edit-symbol][error] LLM returned an empty response.".to_string());
        ui_tx.send_logged("::status:error".to_string());
        return JobRunOutcome::Completed;
    }

    let parsed = match parse_legacy_symbol_edit_response(&response) {
        Ok(resp) => resp,
        Err(e) => {
            ui_tx.send_logged(format!(
                "[edit-symbol][error] Failed to parse response: {e}"
            ));
            ui_tx.send_logged(format!(
                "[edit-symbol] Raw response snippet:\n{}",
                truncate_for_log(&response)
            ));
            ui_tx.send_logged("::status:error".to_string());
            return JobRunOutcome::Completed;
        }
    };

    if cancellation.is_cancelled() {
        ui_tx.send_logged("[edit-symbol] Cancelled before apply.".to_string());
        ui_tx.send_logged("::status:cancelled".to_string());
        return JobRunOutcome::Cancelled;
    }

    match apply_legacy_line_edit_response_via_tools(parsed, request, &cfg, &fs_tools).await {
        Ok(changed_path) => {
            ui_tx.send_logged(format!(
                "[edit-symbol] Patch applied to {}",
                make_relative_display(&changed_path, &cfg.project_root)
            ));

            ui_tx.send_logged("::status:done".to_string());
            JobRunOutcome::Completed
        }
        Err(e) => {
            ui_tx.send_logged(format!(
                "[edit-symbol][error] Failed to apply suggestion: {e}"
            ));
            ui_tx.send_logged("::status:error".to_string());
            JobRunOutcome::Completed
        }
    }
}

fn latest_instruction(ui: &TuiApp) -> Option<String> {
    ui.last_user_input.as_ref().and_then(|s| {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn truncate_for_log(raw: &str) -> String {
    const MAX_CHARS: usize = 2000;
    if raw.chars().count() <= MAX_CHARS {
        raw.to_string()
    } else {
        let truncated: String = raw.chars().take(MAX_CHARS).collect();
        format!("{truncated}…")
    }
}

/// Legacy line-range apply for `/fix` (diff or replacement via `apply_patch`).
///
/// Routed through the unified mutation transaction
/// (`apply_patch_with_receipt` + `finalize_mutation`) so undo, session,
/// provenance, and verification attribution stay consistent.
pub async fn apply_legacy_line_edit_response_via_tools(
    response: SymbolEditResponse,
    request: SymbolEditRequest,
    cfg: &AppConfig,
    fs_tools: &FsTools,
) -> Result<PathBuf> {
    let absolute_path = resolve_absolute_path(&request.target.file, &cfg.project_root);
    let file_content = fs::read_to_string(&absolute_path)
        .await
        .with_context(|| format!("failed to read {}", absolute_path.display()))?;

    let normalized_file = normalize_newlines(&file_content);
    let normalized_original = normalize_newlines(&request.original_code);
    let current_target = extract_target_block_from_content(
        &normalized_file,
        request.target.start_line,
        request.target.end_line,
    );

    if current_target != normalized_original {
        anyhow::bail!(
            "Symbol content changed on disk since the request was created. Please rerun /edit-symbol."
        );
    }

    let patch_content = if let Some(patch) = response.patch {
        normalize_llm_patch(&patch, &absolute_path, &cfg.project_root)
    } else if let Some(replacement) = response.replacement {
        build_patch_from_replacement(
            &normalized_file,
            request.target.start_line,
            request.target.end_line,
            &replacement,
        )?
    } else {
        anyhow::bail!("LLM response did not include a diff or replacement block.");
    };

    if patch_content.trim().is_empty() {
        anyhow::bail!("LLM response produced an empty patch.");
    }

    let params = ApplyPatchParams {
        file_path: absolute_path
            .canonicalize()
            .unwrap_or_else(|_| absolute_path.clone())
            .to_string_lossy()
            .to_string(),
        patch_content,
    };

    let mut exec = crate::tools::apply_patch::apply_patch_with_recovery_and_receipt(params, cfg)
        .await
        .context("failed to apply patch")?;

    if !exec.result.success {
        anyhow::bail!(exec.result.message);
    }
    if let Some(receipt) = exec.receipt.take() {
        let report = fs_tools
            .finalize_mutation(
                receipt,
                crate::tools::FinalizeMutationOptions {
                    record_undo: true,
                    reverts_change_id: None,
                    attribution: Default::default(),
                },
            )
            .await;
        exec.result.warnings.extend(report.warnings);
    }

    Ok(absolute_path)
}

/// Legacy line-range apply for `/fix` (diff or replacement via `apply_patch`).
///
/// Kept as a pure fallback without session/provenance side effects.
/// Prefer [`apply_legacy_line_edit_response_via_tools`] for job paths.
#[allow(dead_code)]
pub async fn apply_legacy_line_edit_response(
    response: SymbolEditResponse,
    request: SymbolEditRequest,
    cfg: &AppConfig,
) -> Result<PathBuf> {
    apply_symbol_edit_response(response, request, cfg).await
}

#[allow(dead_code)]
pub async fn apply_symbol_edit_response(
    response: SymbolEditResponse,
    request: SymbolEditRequest,
    cfg: &AppConfig,
) -> Result<PathBuf> {
    let absolute_path = resolve_absolute_path(&request.target.file, &cfg.project_root);
    let file_content = fs::read_to_string(&absolute_path)
        .await
        .with_context(|| format!("failed to read {}", absolute_path.display()))?;

    let normalized_file = normalize_newlines(&file_content);
    let normalized_original = normalize_newlines(&request.original_code);
    let current_target = extract_target_block_from_content(
        &normalized_file,
        request.target.start_line,
        request.target.end_line,
    );

    if current_target != normalized_original {
        anyhow::bail!(
            "Symbol content changed on disk since the request was created. Please rerun /edit-symbol."
        );
    }

    let patch_content = if let Some(patch) = response.patch {
        normalize_llm_patch(&patch, &absolute_path, &cfg.project_root)
    } else if let Some(replacement) = response.replacement {
        build_patch_from_replacement(
            &normalized_file,
            request.target.start_line,
            request.target.end_line,
            &replacement,
        )?
    } else {
        anyhow::bail!("LLM response did not include a diff or replacement block.");
    };

    if patch_content.trim().is_empty() {
        anyhow::bail!("LLM response produced an empty patch.");
    }

    let params = ApplyPatchParams {
        file_path: absolute_path
            .canonicalize()
            .unwrap_or_else(|_| absolute_path.clone())
            .to_string_lossy()
            .to_string(),
        patch_content,
    };

    let result = apply_patch_tool(params, cfg)
        .await
        .context("failed to apply patch")?;

    if !result.success {
        anyhow::bail!(result.message);
    }

    Ok(absolute_path)
}

fn resolve_absolute_path(path: &Path, root: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn normalize_newlines(input: &str) -> String {
    input.replace("\r\n", "\n")
}

fn extract_target_block_from_content(content: &str, start_line: u32, end_line: u32) -> String {
    let mut buf = String::new();
    for (idx, line) in content.lines().enumerate() {
        let line_no = idx as u32 + 1;
        if line_no >= start_line && line_no <= end_line {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    buf
}

fn build_patch_from_replacement(
    normalized_file: &str,
    start_line: u32,
    end_line: u32,
    replacement: &str,
) -> Result<String> {
    let mut normalized_replacement = normalize_newlines(replacement);
    if !normalized_replacement.ends_with('\n') {
        normalized_replacement.push('\n');
    }

    let (start, end) = target_byte_range(normalized_file, start_line, end_line);
    let mut updated = String::with_capacity(normalized_file.len());
    updated.push_str(&normalized_file[..start]);
    updated.push_str(&normalized_replacement);
    updated.push_str(&normalized_file[end..]);

    if updated == normalized_file {
        anyhow::bail!("Replacement produced no changes.");
    }

    Ok(create_patch(normalized_file, &updated).to_string())
}

fn target_byte_range(content: &str, start_line: u32, end_line: u32) -> (usize, usize) {
    let mut start = None;
    let mut end = None;
    let mut cursor = 0usize;

    for (idx, segment) in content.split_inclusive('\n').enumerate() {
        let line_no = idx as u32 + 1;
        if line_no == start_line && start.is_none() {
            start = Some(cursor);
        }
        cursor += segment.len();
        if line_no == end_line {
            end = Some(cursor);
            break;
        }
    }

    let start_idx = start.unwrap_or(content.len());
    let end_idx = end.unwrap_or(content.len());
    (start_idx, end_idx)
}

fn normalize_llm_patch(raw_patch: &str, file_path: &Path, project_root: &Path) -> String {
    let mut body = normalize_newlines(raw_patch.trim());
    if !body.ends_with('\n') {
        body.push('\n');
    }

    let has_headers = body.lines().any(|line| line.starts_with("--- "));
    if has_headers && body.lines().any(|line| line.starts_with("+++ ")) {
        return body;
    }

    let rel = make_relative_display(file_path, project_root);
    format!("--- a/{rel}\n+++ b/{rel}\n{body}")
}

fn make_relative_path(path: &Path, root: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        path.strip_prefix(root).map(|p| p.to_path_buf()).ok()
    } else {
        Some(path.to_path_buf())
    }
}

fn make_relative_display(path: &Path, root: &Path) -> String {
    make_relative_path(path, root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

fn kind_display(kind: &crate::analysis::SymbolKind) -> &'static str {
    use crate::analysis::SymbolKind;
    match kind {
        SymbolKind::Function => "function",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Trait => "trait",
        SymbolKind::Impl => "impl",
        SymbolKind::Method => "method",
        SymbolKind::AssocFn => "assoc_fn",
        SymbolKind::Mod => "mod",
        SymbolKind::Variable => "var",
        SymbolKind::Comment => "comment",
    }
}

/// Capture the manual usage checkpoint just before a provider request.
///
/// Fails closed when no session manager or current session exists; the
/// caller must not start the source mutation in that case.
fn capture_manual_usage_checkpoint(
    client: &OpenAIClient,
    tools: &FsTools,
) -> anyhow::Result<crate::llm::usage_attribution::SessionUsageCheckpoint> {
    let manager = tools
        .get_session_manager_wrapper()
        .get_session_manager()
        .as_ref()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no session manager for usage attribution"))?;
    crate::llm::usage_attribution::SessionUsageCheckpoint::capture(client, &manager)
}

/// Attribute the provider delta immediately after the request returns.
///
/// Must run before response validation so empty/parse failures still persist
/// the consumed usage. On failure the caller must not mutate the workspace.
fn finish_manual_usage_attribution(
    checkpoint: crate::llm::usage_attribution::SessionUsageCheckpoint,
    after: &crate::llm::usage_ledger::UsageLedger,
    tools: &FsTools,
) -> anyhow::Result<crate::llm::usage_attribution::UsageAttributionResult> {
    let manager = tools
        .get_session_manager_wrapper()
        .get_session_manager()
        .as_ref()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no session manager for usage attribution"))?;
    let mut guard = crate::utils::safe_std_lock(&manager, "session_manager")?;
    checkpoint.finish(after, &mut guard)
}

fn current_file_and_line(ui: &TuiApp) -> Option<(String, u32)> {
    inline_path_reference(ui)
}

fn inline_path_reference(ui: &TuiApp) -> Option<(String, u32)> {
    let last_input = ui.last_user_input.as_deref()?;
    parse_inline_file_reference(last_input)
}

fn parse_inline_file_reference(input: &str) -> Option<(String, u32)> {
    let at_pos = input.rfind('@')?;
    let after = &input[at_pos + 1..];
    if after.is_empty() {
        return None;
    }

    let end = after.find(char::is_whitespace).unwrap_or(after.len());
    let token = after[..end].trim_start_matches(['(', '[', '{', '"', '\'', '`']);
    let token = token.trim_end_matches([',', '.', ';', ')', ']', '}', '"', '\'', '`']);

    extract_path_and_line(token)
}

fn extract_path_and_line(token: &str) -> Option<(String, u32)> {
    if token.is_empty() {
        return None;
    }

    let (path_part, marker) = match token.rfind([':', '#']) {
        Some(idx) => (&token[..idx], Some(&token[idx + 1..])),
        None => (token, None),
    };

    let mut path = path_part
        .trim()
        .trim_matches(|c| matches!(c, '"' | '\'' | '`'));
    if let Some(stripped) = path.strip_prefix("./") {
        path = stripped;
    }
    if path.is_empty() {
        return None;
    }

    let line = marker.and_then(parse_line_marker).unwrap_or(1);

    Some((path.to_string(), line))
}

fn parse_line_marker(marker: &str) -> Option<u32> {
    if marker.is_empty() {
        return None;
    }

    let trimmed = marker
        .trim_start_matches(['L', 'l'])
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>();

    if trimmed.is_empty() {
        None
    } else {
        trimmed.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_inline_reference_with_colon() {
        let text = "Apply change to @src/lib.rs:42 based on review.";
        assert_eq!(
            parse_inline_file_reference(text),
            Some(("src/lib.rs".into(), 42))
        );
    }

    #[test]
    fn parses_inline_reference_with_hash_marker() {
        let text = "Please inspect @src/main.rs#L120 while editing.";
        assert_eq!(
            parse_inline_file_reference(text),
            Some(("src/main.rs".into(), 120))
        );
    }

    #[test]
    fn build_patch_from_replacement_produces_diff() {
        let content = "fn foo() {}\nfn bar() {}\n";
        let replacement = "fn foo() { println!(\"ok\"); }\n";
        let patch = build_patch_from_replacement(content, 1, 1, replacement).unwrap();
        assert!(patch.contains("-fn foo() {}"));
        assert!(patch.contains("+fn foo() { println!(\"ok\"); }"));
    }

    #[test]
    fn normalize_llm_patch_inserts_headers_when_missing() {
        let patch = "+fn foo() {}\n";
        let normalized = normalize_llm_patch(patch, Path::new("src/lib.rs"), Path::new("/proj"));
        assert!(normalized.contains("--- a/src/lib.rs"));
        assert!(normalized.contains("+++ b/src/lib.rs"));
        assert!(normalized.ends_with('\n'));
    }

    #[allow(clippy::type_complexity)]
    fn edit_test_setup(
        content: &str,
    ) -> (
        tempfile::TempDir,
        FsTools,
        std::sync::Arc<std::sync::Mutex<crate::session::SessionManager>>,
        std::sync::Arc<tokio::sync::RwLock<Option<crate::analysis::RepoMap>>>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src/lib.rs");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, content).unwrap();
        let cfg = std::sync::Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            ..Default::default()
        });
        let store = crate::session::SessionStore::new(dir.path().join(".doge/sessions")).unwrap();
        let manager = std::sync::Arc::new(std::sync::Mutex::new(
            crate::session::SessionManager::with_store(store),
        ));
        manager.lock().unwrap().create_session(None).unwrap();
        let tools = FsTools::new(std::sync::Arc::new(tokio::sync::RwLock::new(None)), cfg)
            .with_session_manager(manager.clone());
        let repomap = std::sync::Arc::new(tokio::sync::RwLock::new(None));
        (dir, tools, manager, repomap)
    }

    fn edit_test_client_with_body(body: serde_json::Value) -> (OpenAIClient, httptest::Server) {
        use httptest::{Expectation, matchers::*, responders::*};
        let server = httptest::Server::run();
        server.expect(
            Expectation::matching(request::method_path("POST", "/v1/chat/completions"))
                .times(1..)
                .respond_with(json_encoded(body)),
        );
        let client = OpenAIClient::new(format!("{}/", server.url_str("")), "test-key")
            .unwrap()
            .with_llm_config(crate::config::LlmConfig {
                max_retries: 0,
                ..Default::default()
            });
        (client, server)
    }

    fn symbol_response(
        content: &str,
        prompt: u32,
        completion: u32,
        total: u32,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": "edit-1",
            "choices": [{"index": 0, "finish_reason": "stop",
                "message": {"role": "assistant", "content": content}}],
            "usage": {"prompt_tokens": prompt, "completion_tokens": completion, "total_tokens": total}
        })
    }

    #[tokio::test]
    async fn edit_symbol_success_persists_usage_and_mutates() {
        let content = "fn foo() {\n    1;\n}\nfn bar() {}\n";
        let (_dir, tools, manager, repomap) = edit_test_setup(content);
        let project_root = tools.config.project_root.clone();
        let replacement = "fn foo() {\n    2;\n}\n";
        let body = symbol_response(&format!("```rust\n{replacement}```"), 50, 10, 60);
        let (client, _server) = edit_test_client_with_body(body);
        let (tx, _rx) = std::sync::mpsc::channel();
        let outcome = super::run_semantic_edit_job(
            project_root.clone(),
            PathBuf::from("src/lib.rs"),
            2,
            "change 1 to 2".to_string(),
            "test-model".to_string(),
            Some(client),
            tools.clone(),
            repomap,
            tx,
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(outcome, crate::jobs::JobRunOutcome::Completed));
        assert!(
            std::fs::read_to_string(project_root.join("src/lib.rs"))
                .unwrap()
                .contains("2;")
        );
        let session = manager.lock().unwrap().current_session.clone().unwrap();
        let usage = session.usage.as_ref().unwrap();
        assert_eq!(usage.total_tokens, 60);
        assert_eq!(usage.attempts, 1);
        assert_eq!(session.requests, 1);
        assert_eq!(session.token_count, 60);
        assert!(session.changed_files.iter().any(|p| p.contains("lib.rs")));
    }

    #[tokio::test]
    async fn edit_symbol_parse_failure_persists_usage_without_mutation() {
        let content = "fn foo() {\n    1;\n}\n";
        let (_dir, tools, manager, repomap) = edit_test_setup(content);
        let project_root = tools.config.project_root.clone();
        let before = std::fs::read_to_string(project_root.join("src/lib.rs")).unwrap();
        let body = symbol_response("hello without a code block", 12, 3, 15);
        let (client, _server) = edit_test_client_with_body(body);
        let (tx, _rx) = std::sync::mpsc::channel();
        let outcome = super::run_semantic_edit_job(
            project_root.clone(),
            PathBuf::from("src/lib.rs"),
            2,
            "do something".to_string(),
            "test-model".to_string(),
            Some(client),
            tools,
            repomap,
            tx,
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(outcome, crate::jobs::JobRunOutcome::Completed));
        assert_eq!(
            std::fs::read_to_string(project_root.join("src/lib.rs")).unwrap(),
            before,
            "file unchanged on parse failure"
        );
        let session = manager.lock().unwrap().current_session.clone().unwrap();
        assert_eq!(session.usage.as_ref().unwrap().total_tokens, 15);
        assert_eq!(session.requests, 1);
    }

    #[tokio::test]
    async fn edit_symbol_provider_failure_records_attempt() {
        use httptest::{Expectation, matchers::*, responders::*};
        let content = "fn foo() {\n    1;\n}\n";
        let (_dir, tools, manager, repomap) = edit_test_setup(content);
        let project_root = tools.config.project_root.clone();
        let before = std::fs::read_to_string(project_root.join("src/lib.rs")).unwrap();
        let server = httptest::Server::run();
        server.expect(
            Expectation::matching(request::method_path("POST", "/v1/chat/completions"))
                .times(1)
                .respond_with(status_code(500).body("error")),
        );
        let client = OpenAIClient::new(format!("{}/", server.url_str("")), "test-key")
            .unwrap()
            .with_llm_config(crate::config::LlmConfig {
                max_retries: 0,
                ..Default::default()
            });
        let (tx, _rx) = std::sync::mpsc::channel();
        let outcome = super::run_semantic_edit_job(
            project_root.clone(),
            PathBuf::from("src/lib.rs"),
            1,
            "do something".to_string(),
            "test-model".to_string(),
            Some(client),
            tools,
            repomap,
            tx,
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(outcome, crate::jobs::JobRunOutcome::Failed { .. }));
        assert_eq!(
            std::fs::read_to_string(project_root.join("src/lib.rs")).unwrap(),
            before
        );
        let session = manager.lock().unwrap().current_session.clone().unwrap();
        let usage = session.usage.as_ref().unwrap();
        assert_eq!(usage.attempts, 1);
        assert_eq!(usage.usage_records, 0);
        assert_eq!(usage.unknown_usage_attempts(), 1);
    }

    #[tokio::test]
    async fn edit_symbol_cancellation_persists_attempt_without_mutation() {
        let content = "fn foo() {\n    1;\n}\n";
        let (_dir, tools, manager, repomap) = edit_test_setup(content);
        let project_root = tools.config.project_root.clone();
        let before = std::fs::read_to_string(project_root.join("src/lib.rs")).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        let release_clone = release.clone();
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(
                move |axum::Json(_body): axum::Json<serde_json::Value>| async move {
                    release_clone.notified().await;
                    (
                        axum::http::StatusCode::OK,
                        axum::Json(serde_json::json!({
                            "id": "late",
                            "choices": [{"index": 0, "message": {"role": "assistant", "content": "late"}}]
                        })),
                    )
                },
            ),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = OpenAIClient::new(&url, "test-key")
            .unwrap()
            .with_llm_config(crate::config::LlmConfig::default());
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            cancel_clone.cancel();
        });
        let (tx, _rx) = std::sync::mpsc::channel();
        let outcome = super::run_semantic_edit_job(
            project_root.clone(),
            PathBuf::from("src/lib.rs"),
            1,
            "do something".to_string(),
            "test-model".to_string(),
            Some(client),
            tools,
            repomap,
            tx,
            cancel,
        )
        .await;
        assert!(matches!(outcome, crate::jobs::JobRunOutcome::Cancelled));
        assert_eq!(
            std::fs::read_to_string(project_root.join("src/lib.rs")).unwrap(),
            before
        );
        let session = manager.lock().unwrap().current_session.clone().unwrap();
        assert_eq!(session.usage.as_ref().unwrap().attempts, 1);
        release.notify_one();
        task.abort();
    }

    #[tokio::test]
    async fn legacy_fix_persists_usage_to_session() {
        let content = "fn foo() {\n    1;\n}\n";
        let (_dir, tools, manager, _repomap) = edit_test_setup(content);
        let cfg = crate::config::AppConfig {
            project_root: tools.config.project_root.clone(),
            ..Default::default()
        };
        let replacement = "fn foo() {\n    2;\n}\n";
        let body = symbol_response(&format!("```rust\n{replacement}```"), 40, 10, 50);
        let (client, _server) = edit_test_client_with_body(body);
        let request = crate::llm::SymbolEditRequest {
            model: "test-model".to_string(),
            target: crate::llm::EditTarget {
                file: PathBuf::from("src/lib.rs"),
                start_line: 1,
                end_line: 3,
                name: None,
                kind: "lines".to_string(),
            },
            original_code: content.to_string(),
            instruction: "change 1 to 2".to_string(),
            symbol_id: None,
            parent: None,
        };
        let chat_req = crate::llm::build_symbol_edit_chat_request(&request);
        let crate::llm::types::ChatRequest { messages, .. } = chat_req;
        let (tx, _rx) = std::sync::mpsc::channel();
        let outcome = super::run_legacy_line_edit_job(
            client,
            tools.clone(),
            cfg.clone(),
            tx,
            "test-model".to_string(),
            messages,
            request,
            "fix src/lib.rs".to_string(),
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(outcome, crate::jobs::JobRunOutcome::Completed));
        assert!(
            std::fs::read_to_string(cfg.project_root.join("src/lib.rs"))
                .unwrap()
                .contains("2;")
        );
        let session = manager.lock().unwrap().current_session.clone().unwrap();
        assert_eq!(session.usage.as_ref().unwrap().total_tokens, 50);
        assert_eq!(session.requests, 1);
    }
}
