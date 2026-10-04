//! `requirements_write` / `requirements_read` tools.
//!
//! Requirements are agent-structured interpretations of an observed user
//! directive — never verbatim user quotes. The event store is the source of
//! truth; current state is rebuilt from `RequirementChanged` history.
//!
//! Display rule: requirement statements are described as
//! "Structured requirement derived from directive D1", never as
//! "user said exactly this".

use crate::llm::types::{ToolDef, ToolFunctionDef};
use crate::provenance::ProvenanceAttribution;
use crate::provenance::ProvenanceStore;
use crate::provenance::requirements::{
    CurrentRequirement, RequirementState, compute_requirement_coverage, current_requirements,
    plan_requirement_links, validate_requirement_id, validate_requirement_statement,
};
use crate::provenance::types::{
    ProvenanceEvent, RequirementChangedEvent, RequirementSnapshot, RequirementStatus,
    RequirementTransition,
};
use crate::tools::FsTools;

const REQUIREMENTS_READ_DEFAULT_BUDGET: usize = 6_000;
const REQUIREMENTS_READ_DEFAULT_PAGE_SIZE: usize = 20;
const REQUIREMENTS_READ_MAX_PAGE_SIZE: usize = 100;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RequirementInput {
    pub id: String,
    pub statement: String,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct RequirementsWriteArgs {
    #[serde(default)]
    pub upserts: Vec<RequirementInput>,
    #[serde(default)]
    pub withdraw_ids: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RequirementsWriteResult {
    pub requirements: Vec<CurrentRequirementView>,
    pub changed: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CurrentRequirementView {
    pub id: String,
    pub statement: String,
    pub status: String,
    pub source_directive_ids: Vec<String>,
}

impl From<&CurrentRequirement> for CurrentRequirementView {
    fn from(r: &CurrentRequirement) -> Self {
        Self {
            id: r.id.clone(),
            statement: r.statement.clone(),
            status: match r.status {
                RequirementStatus::Active => "active".to_string(),
                RequirementStatus::Withdrawn => "withdrawn".to_string(),
            },
            source_directive_ids: r.source_directive_ids.clone(),
        }
    }
}

pub fn requirements_write_tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "requirements_write".to_string(),
            description: "Structure explicit user requirements/constraints from the observed directive. Upserts activate (create or refine) requirements; withdraw_ids explicitly withdraws. Never invent requirements unsupported by the directive. Requires an observed directive.".to_string(),
            strict: Some(true),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "upserts": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string"},
                                "statement": {"type": "string"}
                            },
                            "required": ["id", "statement"],
                            "additionalProperties": false,
                        }
                    },
                    "withdraw_ids": {
                        "type": "array",
                        "items": {"type": "string"}
                    }
                },
                "required": [],
                "additionalProperties": false,
            }),
        },
    }
}

pub fn requirements_read_tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "requirements_read".to_string(),
            description: "Read structured requirements derived from observed directives, with plan/change/verification coverage. Use to answer what changed for a requirement and which requirements lack verification.".to_string(),
            strict: Some(true),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "status": {"type": "string", "enum": ["active", "withdrawn", "all"], "description": "Filter by status (default all)"},
                    "requirement_id": {"type": "string", "description": "Read one requirement by id"},
                    "cursor": {"type": "integer", "minimum": 0, "description": "0-based next position"},
                    "page_size": {"type": "integer", "minimum": 1, "maximum": 100, "description": "Requirements per page (default 20)"},
                    "response_budget_chars": {"type": "integer", "minimum": 1, "description": "Total response budget in chars (default 6000)"}
                },
                "required": [],
                "additionalProperties": false
            }),
        },
    }
}

/// Execute `requirements_write`.
///
/// Requires `attribution.directive_id`; without it the call fails with
/// "Cannot create or modify requirements without an observed directive."
/// No-op upserts (identical to current) produce `changed = false` and no
/// event. Event write failures are failures (the store is the source of
/// truth) and mark `provenance_incomplete`.
pub fn requirements_write(
    fs_tools: &FsTools,
    args: RequirementsWriteArgs,
    attribution: &ProvenanceAttribution,
) -> anyhow::Result<RequirementsWriteResult> {
    let Some(directive_id) = attribution.directive_id.clone() else {
        anyhow::bail!("Cannot create or modify requirements without an observed directive.");
    };
    // Duplicate + shape validation before touching the store.
    validate_write_args(&args)?;

    let Some(ctx) = fs_tools.current_session_storage_context() else {
        anyhow::bail!("No current session for requirements_write");
    };
    let store = ProvenanceStore::new(ctx.session_dir.clone());
    let loaded = store.load_all()?;
    let state: RequirementState = current_requirements(&loaded.events);
    let by_id: std::collections::HashMap<&str, &CurrentRequirement> =
        state.items.iter().map(|r| (r.id.as_str(), r)).collect();

    // Unknown withdraw ids fail.
    for wid in &args.withdraw_ids {
        if !by_id.contains_key(wid.as_str()) {
            anyhow::bail!("Unknown requirement id in withdraw_ids: '{wid}'");
        }
    }

    // Build transitions; skip no-ops.
    let mut transitions: Vec<RequirementTransition> = Vec::new();
    for up in &args.upserts {
        match by_id.get(up.id.as_str()) {
            None => transitions.push(RequirementTransition {
                requirement_id: up.id.clone(),
                before: None,
                after: Some(RequirementSnapshot {
                    id: up.id.clone(),
                    statement: up.statement.clone(),
                    status: RequirementStatus::Active,
                }),
            }),
            Some(cur) => {
                if cur.status == RequirementStatus::Active && cur.statement == up.statement {
                    continue; // no-op
                }
                transitions.push(RequirementTransition {
                    requirement_id: up.id.clone(),
                    before: Some(RequirementSnapshot {
                        id: cur.id.clone(),
                        statement: cur.statement.clone(),
                        status: cur.status,
                    }),
                    after: Some(RequirementSnapshot {
                        id: up.id.clone(),
                        statement: up.statement.clone(),
                        status: RequirementStatus::Active,
                    }),
                });
            }
        }
    }
    for wid in &args.withdraw_ids {
        let cur = by_id.get(wid.as_str()).expect("validated above");
        if cur.status == RequirementStatus::Withdrawn {
            continue; // no-op
        }
        transitions.push(RequirementTransition {
            requirement_id: wid.clone(),
            before: Some(RequirementSnapshot {
                id: cur.id.clone(),
                statement: cur.statement.clone(),
                status: cur.status,
            }),
            after: Some(RequirementSnapshot {
                id: cur.id.clone(),
                statement: cur.statement.clone(),
                status: RequirementStatus::Withdrawn,
            }),
        });
    }

    if transitions.is_empty() {
        let requirements = state
            .items
            .iter()
            .map(CurrentRequirementView::from)
            .collect();
        return Ok(RequirementsWriteResult {
            requirements,
            changed: false,
            warnings: state.warnings,
        });
    }

    let event = ProvenanceEvent::RequirementChanged(RequirementChangedEvent {
        directive_id: directive_id.clone(),
        changes: transitions,
    });
    let append = store.append(&ctx.session_id, event);
    match append {
        Ok(_) => {}
        Err(e) => {
            let _ = fs_tools.mark_current_session_provenance_failure();
            anyhow::bail!("requirements_write failed to persist RequirementChanged: {e}");
        }
    }
    // Rebuild after write.
    let reloaded = store.load_all()?;
    let new_state = current_requirements(&reloaded.events);
    let mut warnings = reloaded.warnings;
    warnings.extend(new_state.warnings.clone());
    if fs_tools
        .get_current_session()
        .is_some_and(|s| s.provenance_incomplete)
    {
        warnings.push(
            "Provenance trace is incomplete because one or more event writes failed.".to_string(),
        );
    }
    Ok(RequirementsWriteResult {
        requirements: new_state
            .items
            .iter()
            .map(CurrentRequirementView::from)
            .collect(),
        changed: true,
        warnings,
    })
}

fn validate_write_args(args: &RequirementsWriteArgs) -> anyhow::Result<()> {
    use std::collections::HashSet;
    let mut seen: HashSet<&str> = HashSet::new();
    for up in &args.upserts {
        if !seen.insert(up.id.as_str()) {
            anyhow::bail!("Duplicate requirement id in upserts: '{}'", up.id);
        }
        validate_requirement_id(&up.id).map_err(|e| anyhow::anyhow!("{e}"))?;
        validate_requirement_statement(&up.statement).map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    let mut seen_w: HashSet<&str> = HashSet::new();
    for wid in &args.withdraw_ids {
        if !seen_w.insert(wid.as_str()) {
            anyhow::bail!("Duplicate requirement id in withdraw_ids: '{wid}'");
        }
        validate_requirement_id(wid).map_err(|e| anyhow::anyhow!("{e}"))?;
        if seen.contains(wid.as_str()) {
            anyhow::bail!("Requirement id '{wid}' appears in both upserts and withdraw_ids");
        }
    }
    if args.upserts.is_empty() && args.withdraw_ids.is_empty() {
        anyhow::bail!("requirements_write requires at least one upsert or withdraw_id");
    }
    Ok(())
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct RequirementsReadArgs {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub requirement_id: Option<String>,
    #[serde(default)]
    pub cursor: Option<usize>,
    #[serde(default)]
    pub page_size: Option<usize>,
    #[serde(default)]
    pub response_budget_chars: Option<usize>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RequirementObligationSummary {
    pub id: String,
    pub plan_item_id: String,
    pub kind: String,
    pub state: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RequirementReadEntry {
    pub id: String,
    pub statement: String,
    pub status: String,
    pub source_directive_ids: Vec<String>,
    pub plan_item_ids: Vec<String>,
    pub active_change_ids: Vec<String>,
    pub verified_change_ids: Vec<String>,
    pub unverified_change_ids: Vec<String>,
    pub evidence_state: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification_obligations: Vec<RequirementObligationSummary>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RequirementsReadResponse {
    pub requirements: Vec<RequirementReadEntry>,
    pub warnings: Vec<String>,
    pub next_cursor: Option<usize>,
}

/// Execute `requirements_read` with budgeting (default 6k chars).
pub fn requirements_read(
    fs_tools: &FsTools,
    args: RequirementsReadArgs,
) -> anyhow::Result<RequirementsReadResponse> {
    let budget = args
        .response_budget_chars
        .unwrap_or(REQUIREMENTS_READ_DEFAULT_BUDGET)
        .max(500);
    let page_size = args
        .page_size
        .unwrap_or(REQUIREMENTS_READ_DEFAULT_PAGE_SIZE)
        .clamp(1, REQUIREMENTS_READ_MAX_PAGE_SIZE);
    let cursor = args.cursor.unwrap_or(0);

    let Some(ctx) = fs_tools.current_session_storage_context() else {
        return Ok(RequirementsReadResponse {
            requirements: vec![],
            warnings: vec!["No current session for requirements_read".to_string()],
            next_cursor: None,
        });
    };
    let store = ProvenanceStore::new(ctx.session_dir);
    let loaded = store.load_all()?;
    let mut warnings = loaded.warnings.clone();
    let state = current_requirements(&loaded.events);
    warnings.extend(state.warnings.clone());

    let mut items: Vec<&CurrentRequirement> = state.items.iter().collect();
    if let Some(single) = &args.requirement_id {
        items.retain(|r| &r.id == single);
    }
    match args.status.as_deref() {
        Some("active") => items.retain(|r| r.status == RequirementStatus::Active),
        Some("withdrawn") => items.retain(|r| r.status == RequirementStatus::Withdrawn),
        Some("all") | None => {}
        Some(other) => {
            anyhow::bail!("Unknown status filter: '{other}' (expected active|withdrawn|all)")
        }
    }
    items.sort_by(|a, b| a.id.cmp(&b.id));

    // Coverage integration: current plan + history links, frozen change ids.
    let current_plan = fs_tools.plan_read().map(|p| p.items).unwrap_or_default();
    let plan_links = plan_requirement_links(&current_plan, &loaded.events);
    let coverages = compute_requirement_coverage(
        &loaded.events,
        &state.items,
        &plan_links,
        &fs_tools.config.project_root,
    );
    let cov_by_id: std::collections::HashMap<
        &str,
        &crate::provenance::requirements::RequirementCoverage,
    > = coverages
        .iter()
        .map(|c| (c.requirement_id.as_str(), c))
        .collect();

    // Obligation coverage: compact per-requirement summary (no descriptions).
    let obligation_coverages = crate::provenance::obligations::compute_obligation_coverage(
        &fs_tools.config.project_root,
        &loaded.events,
        &current_plan,
    );
    // requirement_id -> obligation summaries via current plan links.
    let mut obligations_by_req: std::collections::HashMap<
        String,
        Vec<RequirementObligationSummary>,
    > = std::collections::HashMap::new();
    // plan_item_id -> requirement_ids for quick lookup.
    let plan_reqs: std::collections::HashMap<&str, &[String]> = current_plan
        .iter()
        .map(|item| (item.id.as_str(), item.requirement_ids.as_slice()))
        .collect();
    for cov in &obligation_coverages {
        if let Some(reqs) = plan_reqs.get(cov.plan_item_id.as_str()) {
            for req in reqs.iter() {
                obligations_by_req.entry((*req).clone()).or_default().push(
                    RequirementObligationSummary {
                        id: cov.obligation_id.clone(),
                        plan_item_id: cov.plan_item_id.clone(),
                        kind: match cov.kind {
                            crate::provenance::VerificationKind::Test => "test".to_string(),
                            crate::provenance::VerificationKind::Build => "build".to_string(),
                            crate::provenance::VerificationKind::Lint => "lint".to_string(),
                            crate::provenance::VerificationKind::TypeCheck => {
                                "type_check".to_string()
                            }
                            crate::provenance::VerificationKind::FormatCheck => {
                                "format_check".to_string()
                            }
                            crate::provenance::VerificationKind::SyntaxCheck => {
                                "syntax_check".to_string()
                            }
                        },
                        state: cov.state.as_str().to_string(),
                    },
                );
            }
        }
    }
    for v in obligations_by_req.values_mut() {
        v.sort_by(|a, b| a.id.cmp(&b.id));
        // Compact summary: cap per-requirement obligations to keep budget.
        if v.len() > 20 {
            v.truncate(20);
        }
    }

    // Build full entries, then paginate + budget.
    let mut entries: Vec<RequirementReadEntry> = Vec::new();
    for r in items {
        let cov = cov_by_id.get(r.id.as_str());
        entries.push(RequirementReadEntry {
            id: r.id.clone(),
            statement: r.statement.clone(),
            status: match r.status {
                RequirementStatus::Active => "active".to_string(),
                RequirementStatus::Withdrawn => "withdrawn".to_string(),
            },
            source_directive_ids: r.source_directive_ids.clone(),
            plan_item_ids: cov
                .map(|c| c.linked_plan_item_ids.clone())
                .unwrap_or_default(),
            active_change_ids: cov.map(|c| c.active_change_ids.clone()).unwrap_or_default(),
            verified_change_ids: cov
                .map(|c| c.verified_active_change_ids.clone())
                .unwrap_or_default(),
            unverified_change_ids: cov
                .map(|c| c.unverified_active_change_ids.clone())
                .unwrap_or_default(),
            evidence_state: cov
                .map(|c| c.evidence_state.as_str().to_string())
                .unwrap_or_else(|| "no_linked_work".to_string()),
            verification_obligations: obligations_by_req.get(&r.id).cloned().unwrap_or_default(),
        });
    }

    let total = entries.len();
    let start = cursor.min(total);
    let end = (start + page_size).min(total);
    let next_cursor = if end < total { Some(end) } else { None };

    // Budget: fit entries + overhead; truncate statements first, then drop.
    let mut out: Vec<RequirementReadEntry> = Vec::new();
    let mut used = 256usize;
    for e in entries[start..end].iter() {
        let mut candidate = e.clone();
        let len = serde_json::to_string(&candidate)
            .map(|s| s.len())
            .unwrap_or(0)
            + 1;
        if !out.is_empty() && used + len > budget {
            warnings.push("Requirements truncated to stay within response_budget_chars; use cursor/page_size for more.".to_string());
            let omitted_start = start + out.len();
            return Ok(RequirementsReadResponse {
                requirements: out,
                warnings,
                next_cursor: Some(omitted_start),
            });
        }
        // Shrink an overlong statement to fit rather than dropping the entry.
        if len > budget && candidate.statement.chars().count() > 200 {
            let truncated: String = candidate.statement.chars().take(200).collect();
            candidate.statement = format!("{truncated}…");
        }
        used += serde_json::to_string(&candidate)
            .map(|s| s.len())
            .unwrap_or(0)
            + 1;
        out.push(candidate);
    }

    // Silence unused mut when no budget cut happens.
    let _ = &mut warnings;

    Ok(RequirementsReadResponse {
        requirements: out,
        warnings,
        next_cursor,
    })
}

/// Helper for verification capture: map active change ids to their frozen
/// requirement ids (used to build `VerificationContext.requirement_ids`).
pub fn requirement_ids_for_changes(
    events: &[crate::provenance::ProvenanceEventEnvelope],
    change_ids: &[String],
) -> Vec<Vec<String>> {
    let wanted: std::collections::HashSet<&str> = change_ids.iter().map(String::as_str).collect();
    let mut out = Vec::new();
    for env in events {
        if !wanted.contains(env.event_id.as_str()) {
            continue;
        }
        if let ProvenanceEvent::ChangeCommitted(c) = &env.event
            && !c.requirement_ids.is_empty()
        {
            out.push(c.requirement_ids.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::ProvenanceAttribution;

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
    fn test_requirements_write_needs_directive() {
        let proj = tempfile::tempdir().unwrap();
        let (fs, _) = fs_with_session(proj.path());
        let args = RequirementsWriteArgs {
            upserts: vec![RequirementInput {
                id: "r1".to_string(),
                statement: "do things".to_string(),
            }],
            withdraw_ids: vec![],
        };
        let err = requirements_write(&fs, args, &ProvenanceAttribution::none()).unwrap_err();
        assert!(err.to_string().contains("without an observed directive"));
    }

    #[test]
    fn test_requirements_write_validation() {
        let proj = tempfile::tempdir().unwrap();
        let (fs, _) = fs_with_session(proj.path());
        let attr = ProvenanceAttribution::with_directive("d1");
        // Duplicate ids.
        let args = RequirementsWriteArgs {
            upserts: vec![
                RequirementInput {
                    id: "r1".to_string(),
                    statement: "a".to_string(),
                },
                RequirementInput {
                    id: "r1".to_string(),
                    statement: "b".to_string(),
                },
            ],
            withdraw_ids: vec![],
        };
        assert!(requirements_write(&fs, args, &attr).is_err());
        // Upsert + withdraw same id.
        let args = RequirementsWriteArgs {
            upserts: vec![RequirementInput {
                id: "r1".to_string(),
                statement: "a".to_string(),
            }],
            withdraw_ids: vec!["r1".to_string()],
        };
        assert!(requirements_write(&fs, args, &attr).is_err());
        // Invalid id.
        let args = RequirementsWriteArgs {
            upserts: vec![RequirementInput {
                id: "bad id".to_string(),
                statement: "a".to_string(),
            }],
            withdraw_ids: vec![],
        };
        assert!(requirements_write(&fs, args, &attr).is_err());
        // Empty statement.
        let args = RequirementsWriteArgs {
            upserts: vec![RequirementInput {
                id: "r1".to_string(),
                statement: "   ".to_string(),
            }],
            withdraw_ids: vec![],
        };
        assert!(requirements_write(&fs, args, &attr).is_err());
        // Too long.
        let args = RequirementsWriteArgs {
            upserts: vec![RequirementInput {
                id: "r1".to_string(),
                statement: "x".repeat(2001),
            }],
            withdraw_ids: vec![],
        };
        assert!(requirements_write(&fs, args, &attr).is_err());
        // Unknown withdraw.
        let args = RequirementsWriteArgs {
            upserts: vec![],
            withdraw_ids: vec!["nope".to_string()],
        };
        assert!(requirements_write(&fs, args, &attr).is_err());
    }

    #[test]
    fn test_requirements_write_noop_produces_no_event() {
        let proj = tempfile::tempdir().unwrap();
        let (fs, _) = fs_with_session(proj.path());
        let attr = ProvenanceAttribution::with_directive("d1");
        let args = RequirementsWriteArgs {
            upserts: vec![RequirementInput {
                id: "r1".to_string(),
                statement: "first".to_string(),
            }],
            withdraw_ids: vec![],
        };
        let res = requirements_write(&fs, args, &attr).unwrap();
        assert!(res.changed);
        let again = RequirementsWriteArgs {
            upserts: vec![RequirementInput {
                id: "r1".to_string(),
                statement: "first".to_string(),
            }],
            withdraw_ids: vec![],
        };
        let res2 = requirements_write(&fs, again, &attr).unwrap();
        assert!(!res2.changed);
        let store = ProvenanceStore::new(fs.current_session_storage_context().unwrap().session_dir);
        let loaded = store.load_all().unwrap();
        let count = loaded
            .events
            .iter()
            .filter(|e| matches!(e.event, ProvenanceEvent::RequirementChanged(_)))
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_requirements_read_filters_and_budget() {
        let proj = tempfile::tempdir().unwrap();
        let (fs, _) = fs_with_session(proj.path());
        let attr = ProvenanceAttribution::with_directive("d1");
        requirements_write(
            &fs,
            RequirementsWriteArgs {
                upserts: vec![
                    RequirementInput {
                        id: "r1".to_string(),
                        statement: "a".to_string(),
                    },
                    RequirementInput {
                        id: "r2".to_string(),
                        statement: "b".to_string(),
                    },
                ],
                withdraw_ids: vec![],
            },
            &attr,
        )
        .unwrap();
        // Withdraw r2.
        requirements_write(
            &fs,
            RequirementsWriteArgs {
                upserts: vec![],
                withdraw_ids: vec!["r2".to_string()],
            },
            &ProvenanceAttribution::with_directive("d2"),
        )
        .unwrap();
        let active = requirements_read(
            &fs,
            RequirementsReadArgs {
                status: Some("active".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(active.requirements.len(), 1);
        assert_eq!(active.requirements[0].id, "r1");
        let single = requirements_read(
            &fs,
            RequirementsReadArgs {
                requirement_id: Some("r2".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(single.requirements.len(), 1);
        assert_eq!(single.requirements[0].status, "withdrawn");
        // Pagination.
        let p1 = requirements_read(
            &fs,
            RequirementsReadArgs {
                page_size: Some(1),
                cursor: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(p1.requirements.len(), 1);
        assert_eq!(p1.next_cursor, Some(1));
    }
}
