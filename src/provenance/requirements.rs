//! Requirement state reconstructed from `RequirementChanged` history.
//!
//! There is no separate `requirements.json`: the event store is the source
//! of truth. Current state is rebuilt by folding `before -> after`
//! transitions in `(timestamp, event_id)` order (callers must load with
//! `ProvenanceStore::load_all`).
//!
//! A requirement is an agent-structured interpretation of an observed user
//! directive — never a verbatim user quote. Satisfaction is never inferred:
//! statuses are only `Active` / `Withdrawn`.

use std::collections::BTreeMap;

use crate::provenance::types::{ProvenanceEvent, ProvenanceEventEnvelope, RequirementStatus};

/// Upper bound for a requirement statement (chars).
pub const MAX_REQUIREMENT_STATEMENT_CHARS: usize = 2000;

/// A currently-known requirement with its directive history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentRequirement {
    pub id: String,
    pub statement: String,
    pub status: RequirementStatus,
    pub source_directive_ids: Vec<String>,
}

/// Folded requirement state plus non-fatal diagnostics.
#[derive(Debug, Clone, Default)]
pub struct RequirementState {
    pub items: Vec<CurrentRequirement>,
    pub warnings: Vec<String>,
}

/// Validate a requirement id: 1..=64 chars, ASCII letters/digits/`. _ -`.
pub fn validate_requirement_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 64 {
        return Err(format!("Requirement id '{id}' must be 1..=64 characters"));
    }
    let ok = id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-');
    if !ok {
        return Err(format!(
            "Requirement id '{id}' may only contain ASCII letters, digits, '.', '_' and '-'"
        ));
    }
    Ok(())
}

/// Validate a requirement statement: non-empty, bounded.
pub fn validate_requirement_statement(statement: &str) -> Result<(), String> {
    if statement.trim().is_empty() {
        return Err("Requirement statement must not be empty".to_string());
    }
    if statement.chars().count() > MAX_REQUIREMENT_STATEMENT_CHARS {
        return Err(format!(
            "Requirement statement exceeds {MAX_REQUIREMENT_STATEMENT_CHARS} chars"
        ));
    }
    Ok(())
}

/// Rebuild current requirements from canonical events.
///
/// Later `Withdrawn -> Active` reactivations are allowed; the `directive_id`
/// of the reactivating event becomes part of the source history. Source ids
/// accumulate (never rewritten): a new directive never rewrites old sources.
pub fn current_requirements(events: &[ProvenanceEventEnvelope]) -> RequirementState {
    // requirement_id -> (snapshot fields, source ids in first-seen order)
    struct Entry {
        statement: String,
        status: RequirementStatus,
        sources: Vec<String>,
    }
    let mut map: BTreeMap<String, Entry> = BTreeMap::new();
    let mut warnings = Vec::new();

    for env in events {
        let ProvenanceEvent::RequirementChanged(e) = &env.event else {
            continue;
        };
        for t in &e.changes {
            // Validate ids defensively: corrupt history warns instead of failing.
            if validate_requirement_id(&t.requirement_id).is_err() {
                warnings.push(format!(
                    "Skipping requirement transition with invalid id '{}'",
                    t.requirement_id
                ));
                continue;
            }
            match (&t.before, &t.after) {
                (None, None) => {
                    warnings.push(format!(
                        "Requirement '{}' transition has no before or after; skipped",
                        t.requirement_id
                    ));
                }
                (None, Some(after)) => {
                    // Create.
                    let entry = map.entry(t.requirement_id.clone()).or_insert(Entry {
                        statement: after.statement.clone(),
                        status: after.status,
                        sources: Vec::new(),
                    });
                    entry.statement = after.statement.clone();
                    entry.status = after.status;
                    push_source(&mut entry.sources, &e.directive_id);
                }
                (Some(_before), Some(after)) => {
                    // Refine / withdraw / reactivate.
                    let entry = map.entry(t.requirement_id.clone()).or_insert(Entry {
                        statement: after.statement.clone(),
                        status: after.status,
                        sources: Vec::new(),
                    });
                    entry.statement = after.statement.clone();
                    entry.status = after.status;
                    push_source(&mut entry.sources, &e.directive_id);
                }
                (Some(_before), None) => {
                    // Deletion is not part of v1; warn and keep last state.
                    warnings.push(format!(
                        "Requirement '{}' has a deletion transition which v1 does not support; kept last state",
                        t.requirement_id
                    ));
                }
            }
        }
    }

    let items = map
        .into_iter()
        .map(|(id, e)| CurrentRequirement {
            id,
            statement: e.statement,
            status: e.status,
            source_directive_ids: e.sources,
        })
        .collect();

    RequirementState { items, warnings }
}

fn push_source(sources: &mut Vec<String>, directive_id: &str) {
    if !sources.iter().any(|s| s == directive_id) {
        sources.push(directive_id.to_string());
    }
}

/// Requirement evidence state. `ObservedPassing` means only that at least one
/// successful `VerificationObserved` covered an active change — never that the
/// requirement is proven, correct, or satisfied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequirementEvidenceState {
    NoLinkedWork,
    PlannedNoActiveChange,
    ActiveUnverified,
    ObservedPassing,
    Diverged,
    Reverted,
    Mixed,
}

impl RequirementEvidenceState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoLinkedWork => "no_linked_work",
            Self::PlannedNoActiveChange => "planned_no_active_change",
            Self::ActiveUnverified => "active_unverified",
            Self::ObservedPassing => "observed_passing",
            Self::Diverged => "diverged",
            Self::Reverted => "reverted",
            Self::Mixed => "mixed",
        }
    }
}

/// Per-requirement coverage derived from frozen attribution.
///
/// - Plan links come from `PlanChanged` history + current plan requirement ids.
/// - Change links come from frozen `ChangeCommitted.requirement_ids` (never
///   reinterpreted when later plan links move).
/// - Verification links come from frozen `VerificationObserved.requirement_ids`.
/// - Lifecycle (active/diverged/reverted) is resolved via the existing file
///   chain + symbol resolver in `query.rs`.
#[derive(Debug, Clone)]
pub struct RequirementCoverage {
    pub requirement_id: String,
    pub linked_plan_item_ids: Vec<String>,
    pub active_change_ids: Vec<String>,
    pub verified_active_change_ids: Vec<String>,
    pub unverified_active_change_ids: Vec<String>,
    pub diverged_change_ids: Vec<String>,
    pub reverted_change_ids: Vec<String>,
    pub evidence_state: RequirementEvidenceState,
}

/// Compute coverage for every known requirement.
///
/// `plan_requirement_links` maps `plan_item_id -> requirement_ids` from the
/// current plan plus `PlanChanged` history. Callers build it via
/// [`plan_requirement_links`]. `project_root` resolves active states.
pub fn compute_requirement_coverage(
    events: &[ProvenanceEventEnvelope],
    requirements: &[CurrentRequirement],
    plan_requirement_links: &std::collections::HashMap<String, Vec<String>>,
    project_root: &std::path::Path,
) -> Vec<RequirementCoverage> {
    use crate::provenance::query::{ActiveChangeState, resolve_active_states};
    use crate::provenance::types::ProvenanceEvent as Ev;

    let resolved = resolve_active_states(project_root, events);
    let mut state_by_change: std::collections::HashMap<&str, ActiveChangeState> =
        std::collections::HashMap::new();
    for r in &resolved {
        state_by_change.insert(r.change_id.as_str(), r.state);
    }

    // Successful verifications only (failed runs never verify).
    let mut successful_observed: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for env in events {
        if let Ev::VerificationObserved(v) = &env.event
            && v.outcome.success
        {
            for id in &v.observed_change_ids {
                successful_observed.insert(id.as_str());
            }
        }
    }

    // requirement -> changes (frozen ids on ChangeCommitted).
    let mut changes_by_req: std::collections::HashMap<&str, Vec<&str>> =
        std::collections::HashMap::new();
    for env in events {
        if let Ev::ChangeCommitted(c) = &env.event {
            for req in &c.requirement_ids {
                changes_by_req
                    .entry(req.as_str())
                    .or_default()
                    .push(env.event_id.as_str());
            }
        }
    }

    // requirement -> plan items (current links + history links).
    let mut plans_by_req: std::collections::HashMap<&str, Vec<String>> =
        std::collections::HashMap::new();
    for (plan_id, reqs) in plan_requirement_links {
        for req in reqs {
            plans_by_req
                .entry(req.as_str())
                .or_default()
                .push(plan_id.clone());
        }
    }
    for v in plans_by_req.values_mut() {
        v.sort();
        v.dedup();
    }

    let mut out = Vec::new();
    for req in requirements {
        let linked_plans = plans_by_req
            .get(req.id.as_str())
            .cloned()
            .unwrap_or_default();
        let change_ids: Vec<&str> = changes_by_req
            .get(req.id.as_str())
            .cloned()
            .unwrap_or_default();

        let mut active = Vec::new();
        let mut diverged = Vec::new();
        let mut reverted = Vec::new();
        for cid in change_ids {
            match state_by_change.get(cid).copied() {
                Some(ActiveChangeState::Active) => active.push(cid.to_string()),
                Some(ActiveChangeState::Diverged) | Some(ActiveChangeState::Missing) => {
                    diverged.push(cid.to_string())
                }
                Some(ActiveChangeState::Reverted) => reverted.push(cid.to_string()),
                Some(ActiveChangeState::Superseded) => {}
                None => {}
            }
        }
        active.sort();
        diverged.sort();
        reverted.sort();

        let mut verified: Vec<String> = active
            .iter()
            .filter(|id| successful_observed.contains(id.as_str()))
            .cloned()
            .collect();
        verified.sort();
        let verified_set: std::collections::HashSet<&str> =
            verified.iter().map(String::as_str).collect();
        let mut unverified: Vec<String> = active
            .iter()
            .filter(|id| !verified_set.contains(id.as_str()))
            .cloned()
            .collect();
        unverified.sort();

        let evidence_state = classify_requirement_evidence(
            linked_plans.is_empty(),
            &active,
            &verified,
            &unverified,
            &diverged,
            &reverted,
        );

        out.push(RequirementCoverage {
            requirement_id: req.id.clone(),
            linked_plan_item_ids: linked_plans,
            active_change_ids: active,
            verified_active_change_ids: verified,
            unverified_active_change_ids: unverified,
            diverged_change_ids: diverged,
            reverted_change_ids: reverted,
            evidence_state,
        });
    }
    out.sort_by(|a, b| a.requirement_id.cmp(&b.requirement_id));
    out
}

fn classify_requirement_evidence(
    no_plans: bool,
    active: &[String],
    verified: &[String],
    unverified: &[String],
    diverged: &[String],
    reverted: &[String],
) -> RequirementEvidenceState {
    // `verified`/`unverified` are subsets of `active`, so an empty `active`
    // implies both are empty. Branch on `active` directly.
    if active.is_empty() {
        // Nothing active remains: diverged wins over reverted when both are
        // present (deterministic priority), otherwise surface whichever
        // signal exists.
        if !diverged.is_empty() {
            return RequirementEvidenceState::Diverged;
        }
        if !reverted.is_empty() {
            return RequirementEvidenceState::Reverted;
        }
        if no_plans {
            return RequirementEvidenceState::NoLinkedWork;
        }
        return RequirementEvidenceState::PlannedNoActiveChange;
    }
    // Active changes exist. Any mix of distinct signals is Mixed so a single
    // label never hides coexisting evidence: verified+unverified, or verified
    // active alongside diverged/reverted history.
    if !verified.is_empty()
        && (!unverified.is_empty() || !diverged.is_empty() || !reverted.is_empty())
    {
        return RequirementEvidenceState::Mixed;
    }
    if !verified.is_empty() {
        return RequirementEvidenceState::ObservedPassing;
    }
    // Active but none verified. Diverged/reverted history coexisting with
    // unverified active work is also Mixed rather than hidden.
    if !diverged.is_empty() || !reverted.is_empty() {
        return RequirementEvidenceState::Mixed;
    }
    RequirementEvidenceState::ActiveUnverified
}

/// Build `plan_item_id -> requirement_ids` from the current plan plus
/// history for deleted plans.
///
/// Current plan is authoritative for live links: a plan item present in the
/// current plan reports exactly its current `requirement_ids` (including
/// empty after an unlink). History only backfills plan ids absent from the
/// current plan (deleted plans stay discoverable via their last link).
pub fn plan_requirement_links(
    current_plan: &[crate::tools::plan::PlanItem],
    events: &[ProvenanceEventEnvelope],
) -> std::collections::HashMap<String, Vec<String>> {
    let mut out: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let mut live_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in current_plan {
        live_ids.insert(item.id.clone());
        if !item.requirement_ids.is_empty() {
            out.insert(item.id.clone(), item.requirement_ids.clone());
        }
    }
    for env in events {
        if let ProvenanceEvent::PlanChanged(e) = &env.event {
            for t in &e.changes {
                if live_ids.contains(&t.plan_item_id) {
                    continue;
                }
                if !t.after_requirement_ids.is_empty() {
                    let entry = out.entry(t.plan_item_id.clone()).or_default();
                    for r in &t.after_requirement_ids {
                        if !entry.contains(r) {
                            entry.push(r.clone());
                        }
                    }
                }
            }
        }
    }
    for v in out.values_mut() {
        v.sort();
        v.dedup();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::types::{
        ProvenanceEvent, ProvenanceEventEnvelope, RequirementChangedEvent, RequirementSnapshot,
        RequirementStatus, RequirementTransition,
    };

    fn envelope(
        id: &str,
        ts: &str,
        directive_id: &str,
        transitions: Vec<RequirementTransition>,
    ) -> ProvenanceEventEnvelope {
        ProvenanceEventEnvelope {
            schema_version: 3,
            event_id: id.to_string(),
            session_id: "s".to_string(),
            timestamp: ts.to_string(),
            event: ProvenanceEvent::RequirementChanged(RequirementChangedEvent {
                directive_id: directive_id.to_string(),
                changes: transitions,
            }),
        }
    }

    fn active(id: &str, statement: &str) -> RequirementTransition {
        RequirementTransition {
            requirement_id: id.to_string(),
            before: None,
            after: Some(RequirementSnapshot {
                id: id.to_string(),
                statement: statement.to_string(),
                status: RequirementStatus::Active,
            }),
        }
    }

    #[test]
    fn test_create_refine_withdraw_reactivate() {
        let e1 = envelope(
            "e1",
            "2026-01-01T00:00:00+00:00",
            "d1",
            vec![active("r1", "first")],
        );
        let state = current_requirements(std::slice::from_ref(&e1));
        assert_eq!(state.items.len(), 1);
        assert_eq!(state.items[0].status, RequirementStatus::Active);
        assert_eq!(state.items[0].source_directive_ids, vec!["d1".to_string()]);

        // Refine statement.
        let e2 = envelope(
            "e2",
            "2026-01-02T00:00:00+00:00",
            "d1",
            vec![RequirementTransition {
                requirement_id: "r1".to_string(),
                before: Some(RequirementSnapshot {
                    id: "r1".to_string(),
                    statement: "first".to_string(),
                    status: RequirementStatus::Active,
                }),
                after: Some(RequirementSnapshot {
                    id: "r1".to_string(),
                    statement: "refined".to_string(),
                    status: RequirementStatus::Active,
                }),
            }],
        );
        let state = current_requirements(&[e1.clone(), e2]);
        assert_eq!(state.items[0].statement, "refined");

        // Withdraw.
        let e3 = envelope(
            "e3",
            "2026-01-03T00:00:00+00:00",
            "d2",
            vec![RequirementTransition {
                requirement_id: "r1".to_string(),
                before: Some(RequirementSnapshot {
                    id: "r1".to_string(),
                    statement: "refined".to_string(),
                    status: RequirementStatus::Active,
                }),
                after: Some(RequirementSnapshot {
                    id: "r1".to_string(),
                    statement: "refined".to_string(),
                    status: RequirementStatus::Withdrawn,
                }),
            }],
        );
        let state = current_requirements(&[e1.clone(), e3.clone()]);
        assert_eq!(state.items[0].status, RequirementStatus::Withdrawn);
        assert_eq!(
            state.items[0].source_directive_ids,
            vec!["d1".to_string(), "d2".to_string()]
        );

        // Reactivate.
        let e4 = envelope(
            "e4",
            "2026-01-04T00:00:00+00:00",
            "d3",
            vec![RequirementTransition {
                requirement_id: "r1".to_string(),
                before: Some(RequirementSnapshot {
                    id: "r1".to_string(),
                    statement: "refined".to_string(),
                    status: RequirementStatus::Withdrawn,
                }),
                after: Some(RequirementSnapshot {
                    id: "r1".to_string(),
                    statement: "refined".to_string(),
                    status: RequirementStatus::Active,
                }),
            }],
        );
        let state = current_requirements(&[e1, e3, e4]);
        assert_eq!(state.items[0].status, RequirementStatus::Active);
        assert_eq!(state.items[0].source_directive_ids.len(), 3);
    }

    #[test]
    fn test_multiple_directives_keep_history() {
        let e1 = envelope(
            "e1",
            "2026-01-01T00:00:00+00:00",
            "d1",
            vec![active("r1", "a")],
        );
        let e2 = envelope(
            "e2",
            "2026-01-02T00:00:00+00:00",
            "d2",
            vec![active("r2", "b")],
        );
        let state = current_requirements(&[e1, e2]);
        assert_eq!(state.items.len(), 2);
        let r1 = state.items.iter().find(|r| r.id == "r1").unwrap();
        assert_eq!(r1.source_directive_ids, vec!["d1".to_string()]);
    }

    #[test]
    fn test_id_validation() {
        assert!(validate_requirement_id("req-auth-latency").is_ok());
        assert!(validate_requirement_id("req.api_compat-1").is_ok());
        assert!(validate_requirement_id("").is_err());
        assert!(validate_requirement_id("has space").is_err());
        assert!(validate_requirement_id("x".repeat(65).as_str()).is_err());
        assert!(validate_requirement_statement("ok").is_ok());
        assert!(validate_requirement_statement("   ").is_err());
        assert!(validate_requirement_statement("x".repeat(2001).as_str()).is_err());
    }
}
