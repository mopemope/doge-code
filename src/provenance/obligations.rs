//! Verification obligation matching, binding, and evidence states.
//!
//! A passing command is evidence, not proof. Evidence must be linked to the
//! change state that the command actually observed; a later mutation can make
//! earlier evidence stale; historical attribution is never rewritten.

use std::collections::HashMap;
use std::path::Path;

use super::types::{
    PlanItemTransition, ProvenanceEvent, ProvenanceEventEnvelope, VerificationKind,
    VerificationObligation, VerificationObligationRef,
};

/// Current evidence state for one verification obligation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationObligationEvidenceState {
    NoLinkedChange,
    Pending,
    ObservedPassing,
    ObservedFailing,
    Stale,
    Diverged,
    Reverted,
    Mixed,
}

impl VerificationObligationEvidenceState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoLinkedChange => "no_linked_change",
            Self::Pending => "pending",
            Self::ObservedPassing => "observed_passing",
            Self::ObservedFailing => "observed_failing",
            Self::Stale => "stale",
            Self::Diverged => "diverged",
            Self::Reverted => "reverted",
            Self::Mixed => "mixed",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "no_linked_change" => Some(Self::NoLinkedChange),
            "pending" => Some(Self::Pending),
            "observed_passing" => Some(Self::ObservedPassing),
            "observed_failing" => Some(Self::ObservedFailing),
            "stale" => Some(Self::Stale),
            "diverged" => Some(Self::Diverged),
            "reverted" => Some(Self::Reverted),
            "mixed" => Some(Self::Mixed),
            _ => None,
        }
    }
}

/// Normalize an executable path to its basename (handles `/` and `\`).
///
/// Strips a trailing `.exe` case-insensitively so `cargo.exe` matches `cargo`
/// (Windows portability). Comparison itself remains case-sensitive.
pub fn executable_basename(program: &str) -> &str {
    let base = program.rsplit(['/', '\\']).next().unwrap_or(program).trim();
    if base.len() > 4 && base[base.len() - 4..].eq_ignore_ascii_case(".exe") {
        base[..base.len() - 4].trim()
    } else {
        base
    }
}

/// Check argv prefix match (no regex/glob/shell).
///
/// Each `prefix` token must be an exact string prefix of the corresponding
/// `actual` token (`actual[i].starts_with(prefix[i])`), and `actual` may
/// have trailing extra tokens. This lets `["test", "provenance::"]` match
/// `["test", "provenance::traceability_tests"]` without regex.
///
/// Note: single-character prefixes (e.g. `["t"]`) are deliberately broad and
/// will match any token with that initial. Prefer longer, scope-specific
/// prefixes such as `provenance::` in obligation definitions.
pub fn args_prefix_matches(actual: &[String], prefix: &[String]) -> bool {
    if prefix.len() > actual.len() {
        return false;
    }
    actual
        .iter()
        .zip(prefix.iter())
        .all(|(a, p)| a.starts_with(p.as_str()))
}

/// Does one invocation match one obligation definition?
///
/// - `kind` must always match.
/// - When `command` is `None`: kind match suffices (scope already narrowed to
///   candidate plan items by the caller).
/// - When `command` is `Some`: kind + basename + argv prefix must match.
pub fn obligation_matches_invocation(
    obligation: &VerificationObligation,
    kind: VerificationKind,
    program: &str,
    args: &[String],
) -> bool {
    if obligation.kind != kind {
        return false;
    }
    let Some(matcher) = &obligation.command else {
        return true;
    };
    if executable_basename(program) != executable_basename(&matcher.program) {
        return false;
    }
    // Empty prefix means "any argv for this program+kind".
    if matcher.args_prefix.is_empty() {
        return true;
    }
    args_prefix_matches(args, &matcher.args_prefix)
}

/// Compute the stable binding hash for an obligation.
///
/// Covers: `plan_item_id`, sorted+dedeuped `requirement_ids`,
/// `obligation.id`, `description`, `kind`, `command.program`,
/// `command.args_prefix` (order significant). Requirement order alone never
/// changes the hash.
///
/// Encoding uses `\x00` field separators and `\x1f` for args. Requirement IDs
/// are restricted to `[A-Za-z0-9._-]` (no commas/`\x00`), so the join is
/// unambiguous in practice.
pub fn obligation_binding_hash(
    plan_item_id: &str,
    requirement_ids: &[String],
    obligation: &VerificationObligation,
) -> String {
    let mut reqs: Vec<&str> = requirement_ids.iter().map(String::as_str).collect();
    reqs.sort_unstable();
    reqs.dedup();
    let kind_str = match obligation.kind {
        VerificationKind::Test => "test",
        VerificationKind::Build => "build",
        VerificationKind::Lint => "lint",
        VerificationKind::TypeCheck => "type_check",
        VerificationKind::FormatCheck => "format_check",
        VerificationKind::SyntaxCheck => "syntax_check",
    };
    let (program, args_joined) = match &obligation.command {
        None => (String::new(), String::new()),
        Some(c) => (c.program.clone(), c.args_prefix.join("\x1f")),
    };
    // Canonical encoding with nul separators (see doc above).
    let mut canonical = String::new();
    canonical.push_str("v1\x00");
    canonical.push_str(plan_item_id);
    canonical.push('\x00');
    canonical.push_str(&reqs.join(","));
    canonical.push('\x00');
    canonical.push_str(&obligation.id);
    canonical.push('\x00');
    canonical.push_str(&obligation.description);
    canonical.push('\x00');
    canonical.push_str(kind_str);
    canonical.push('\x00');
    canonical.push_str(&program);
    canonical.push('\x00');
    canonical.push_str(&args_joined);
    format!("blake3:{}", blake3::hash(canonical.as_bytes()).to_hex())
}

/// Build a frozen obligation ref for capture time.
pub fn obligation_ref_for(
    plan_item_id: &str,
    requirement_ids: &[String],
    obligation: &VerificationObligation,
) -> VerificationObligationRef {
    VerificationObligationRef {
        id: obligation.id.clone(),
        binding_hash: obligation_binding_hash(plan_item_id, requirement_ids, obligation),
    }
}

/// Candidate obligation with its plan scope.
#[derive(Debug, Clone)]
pub struct CandidateObligation {
    pub plan_item_id: String,
    pub requirement_ids: Vec<String>,
    pub obligation: VerificationObligation,
    pub binding_hash: String,
}

/// Collect candidate obligations from in-progress + active-change plan items.
///
/// `plan_items` is the current plan; `active_plan_ids` are plan ids resolved
/// from current active changes (so a prematurely-completed item still
/// contributes when its change is active). Unrelated items are excluded.
pub fn collect_candidate_obligations(
    plan_items: &[crate::tools::plan::PlanItem],
    active_plan_ids: &[String],
) -> Vec<CandidateObligation> {
    use std::collections::HashSet;
    let mut candidates: Vec<CandidateObligation> = Vec::new();
    let in_progress = super::verification::current_in_progress_plan_item(plan_items);
    let active_set: HashSet<&str> = active_plan_ids.iter().map(String::as_str).collect();
    for item in plan_items {
        let is_in_progress = in_progress.as_deref() == Some(item.id.as_str());
        let is_active_linked = active_set.contains(item.id.as_str());
        if !is_in_progress && !is_active_linked {
            continue;
        }
        for ob in &item.verification_obligations {
            let binding_hash = obligation_binding_hash(&item.id, &item.requirement_ids, ob);
            candidates.push(CandidateObligation {
                plan_item_id: item.id.clone(),
                requirement_ids: item.requirement_ids.clone(),
                obligation: ob.clone(),
                binding_hash,
            });
        }
    }
    candidates
}

/// Match an invocation against candidates, returning frozen refs.
///
/// Deterministic order (sorted by obligation id) for stable tests.
pub fn match_verification_obligations(
    candidates: &[CandidateObligation],
    kind: VerificationKind,
    program: &str,
    args: &[String],
) -> Vec<VerificationObligationRef> {
    let mut out: Vec<VerificationObligationRef> = Vec::new();
    let mut sorted: Vec<&CandidateObligation> = candidates.iter().collect();
    sorted.sort_by(|a, b| a.obligation.id.cmp(&b.obligation.id));
    for c in sorted {
        if obligation_matches_invocation(&c.obligation, kind, program, args) {
            out.push(VerificationObligationRef {
                id: c.obligation.id.clone(),
                binding_hash: c.binding_hash.clone(),
            });
        }
    }
    out
}

/// Capture obligation attribution for one invocation (pre-execution).
///
/// Callers must pass the current plan + active-change plan ids snapshot taken
/// before the process starts; never re-read the plan after execution.
pub fn capture_verification_context_for_invocation(
    plan_items: &[crate::tools::plan::PlanItem],
    active_plan_ids: &[String],
    kind: VerificationKind,
    program: &str,
    args: &[String],
) -> Vec<VerificationObligationRef> {
    let candidates = collect_candidate_obligations(plan_items, active_plan_ids);
    match_verification_obligations(&candidates, kind, program, args)
}

// ---------------------------------------------------------------------------
// Coverage resolver
// ---------------------------------------------------------------------------

/// One obligation's current definition + scope.
#[derive(Debug, Clone)]
pub struct CurrentObligation {
    pub plan_item_id: String,
    pub requirement_ids: Vec<String>,
    pub obligation: VerificationObligation,
    pub binding_hash: String,
}

/// Per-obligation coverage.
#[derive(Debug, Clone)]
pub struct ObligationCoverage {
    pub obligation_id: String,
    pub plan_item_id: String,
    pub kind: VerificationKind,
    pub binding_hash: String,
    pub state: VerificationObligationEvidenceState,
    pub active_change_ids: Vec<String>,
}

/// Collect current obligations from the live plan.
pub fn current_obligations(plan_items: &[crate::tools::plan::PlanItem]) -> Vec<CurrentObligation> {
    let mut out = Vec::new();
    for item in plan_items {
        for ob in &item.verification_obligations {
            out.push(CurrentObligation {
                plan_item_id: item.id.clone(),
                requirement_ids: item.requirement_ids.clone(),
                binding_hash: obligation_binding_hash(&item.id, &item.requirement_ids, ob),
                obligation: ob.clone(),
            });
        }
    }
    out.sort_by(|a, b| {
        a.plan_item_id
            .cmp(&b.plan_item_id)
            .then(a.obligation.id.cmp(&b.obligation.id))
    });
    out
}

/// Compute evidence state for every current obligation.
///
/// Rules:
/// - No tracked change ever for the plan item -> `NoLinkedChange`.
/// - Active changes exist but no matching observation -> `Pending`.
/// - Successful observation with id+hash match covering all current active
///   changes -> `ObservedPassing`.
/// - Matching observation covering current changes whose latest result is
///   failure -> `ObservedFailing`.
/// - Matching observation exists but doesn't fully cover current changes
///   (later mutation) or binding changed -> `Stale`.
/// - No active left but diverged/missing history -> `Diverged`.
/// - No active left but reverted -> `Reverted`.
/// - Mixed signals (e.g. passing + failing across changes) -> `Mixed` when
///   ambiguity would otherwise hide evidence.
pub fn compute_obligation_coverage(
    project_root: &Path,
    events: &[ProvenanceEventEnvelope],
    plan_items: &[crate::tools::plan::PlanItem],
) -> Vec<ObligationCoverage> {
    use super::query::{ActiveChangeState, resolve_active_states};

    let resolved = resolve_active_states(project_root, events);
    // change_id -> (state, plan_item_id, change_kind)
    let mut state_by_id: HashMap<
        &str,
        (
            ActiveChangeState,
            Option<&str>,
            Option<super::types::ChangeKind>,
        ),
    > = HashMap::new();
    for r in &resolved {
        // Find plan_item_id + kind from events (frozen).
        let (plan_id, kind) = events
            .iter()
            .find(|e| e.event_id == r.change_id)
            .and_then(|e| match &e.event {
                ProvenanceEvent::ChangeCommitted(c) => {
                    Some((c.plan_item_id.as_deref(), Some(c.change_kind)))
                }
                _ => None,
            })
            .unwrap_or((None, None));
        state_by_id.insert(r.change_id.as_str(), (r.state, plan_id, kind));
    }

    // All change ids per plan item (for NoLinkedChange vs Diverged/Reverted).
    // Stores (change_id, state, kind).
    type PlanChanges<'a> = Vec<(&'a str, ActiveChangeState, Option<super::types::ChangeKind>)>;
    let mut all_changes_by_plan: HashMap<&str, PlanChanges<'_>> = HashMap::new();
    for r in &resolved {
        if let Some((_, plan_id, kind)) = state_by_id.get(r.change_id.as_str())
            && let Some(pid) = plan_id
        {
            all_changes_by_plan.entry(pid).or_default().push((
                r.change_id.as_str(),
                r.state,
                *kind,
            ));
        }
    }

    let currents = current_obligations(plan_items);
    // Collect verifications in event order.
    let verifs: Vec<&ProvenanceEventEnvelope> = events
        .iter()
        .filter(|e| matches!(e.event, ProvenanceEvent::VerificationObserved(_)))
        .collect();

    let mut out = Vec::new();
    for cur in currents {
        // Relevant active changes: Active + plan_item matches.
        let mut active: Vec<String> = Vec::new();
        let mut active_is_undo: Vec<bool> = Vec::new();
        let mut diverged: Vec<String> = Vec::new();
        let mut reverted: Vec<String> = Vec::new();
        if let Some(list) = all_changes_by_plan.get(cur.plan_item_id.as_str()) {
            for (cid, st, kind) in list {
                match st {
                    ActiveChangeState::Active => {
                        active.push((*cid).to_string());
                        active_is_undo.push(matches!(kind, Some(super::types::ChangeKind::Undo)));
                    }
                    ActiveChangeState::Diverged | ActiveChangeState::Missing => {
                        diverged.push((*cid).to_string())
                    }
                    ActiveChangeState::Reverted => reverted.push((*cid).to_string()),
                    ActiveChangeState::Superseded => {}
                }
            }
        }
        active.sort();
        diverged.sort();
        reverted.sort();

        // No tracked change ever for this plan item.
        let ever_had = all_changes_by_plan
            .get(cur.plan_item_id.as_str())
            .is_some_and(|v| !v.is_empty());
        if !ever_had {
            out.push(ObligationCoverage {
                obligation_id: cur.obligation.id.clone(),
                plan_item_id: cur.plan_item_id.clone(),
                kind: cur.obligation.kind,
                binding_hash: cur.binding_hash.clone(),
                state: VerificationObligationEvidenceState::NoLinkedChange,
                active_change_ids: vec![],
            });
            continue;
        }
        // Only undo remains active while the original was reverted: the
        // implementation itself is gone, so surface Reverted rather than
        // Pending on the undo bookkeeping event.
        if !active.is_empty()
            && !reverted.is_empty()
            && !active_is_undo.is_empty()
            && active_is_undo.iter().all(|b| *b)
        {
            out.push(ObligationCoverage {
                obligation_id: cur.obligation.id.clone(),
                plan_item_id: cur.plan_item_id.clone(),
                kind: cur.obligation.kind,
                binding_hash: cur.binding_hash.clone(),
                state: VerificationObligationEvidenceState::Reverted,
                active_change_ids: vec![],
            });
            continue;
        }
        if active.is_empty() {
            // No active left: diverged wins, else reverted, else no-linked (e.g.
            // all superseded).
            let state = if !diverged.is_empty() {
                VerificationObligationEvidenceState::Diverged
            } else if !reverted.is_empty() {
                VerificationObligationEvidenceState::Reverted
            } else {
                VerificationObligationEvidenceState::NoLinkedChange
            };
            out.push(ObligationCoverage {
                obligation_id: cur.obligation.id.clone(),
                plan_item_id: cur.plan_item_id.clone(),
                kind: cur.obligation.kind,
                binding_hash: cur.binding_hash.clone(),
                state,
                active_change_ids: vec![],
            });
            continue;
        }

        // Matching observations with current binding hash.
        let active_set: std::collections::HashSet<&str> =
            active.iter().map(String::as_str).collect();
        let mut covering_pass: Vec<&ProvenanceEventEnvelope> = Vec::new();
        let mut covering_fail: Vec<&ProvenanceEventEnvelope> = Vec::new();
        let mut any_same_id_old_hash = false;
        let mut any_current_hash_obs = false;
        for v in &verifs {
            let ProvenanceEvent::VerificationObserved(vo) = &v.event else {
                continue;
            };
            let mut matched_current = false;
            let mut matched_old = false;
            for m in &vo.matched_obligations {
                if m.id == cur.obligation.id {
                    if m.binding_hash == cur.binding_hash {
                        matched_current = true;
                    } else {
                        matched_old = true;
                    }
                }
            }
            if matched_old {
                any_same_id_old_hash = true;
            }
            if !matched_current {
                continue;
            }
            any_current_hash_obs = true;
            // Does this observation cover all current active changes?
            let observed: std::collections::HashSet<&str> =
                vo.observed_change_ids.iter().map(String::as_str).collect();
            let covers = active_set.iter().all(|id| observed.contains(id));
            if !covers {
                continue;
            }
            if vo.outcome.success {
                covering_pass.push(*v);
            } else {
                covering_fail.push(*v);
            }
        }

        let state = if !covering_pass.is_empty() && !covering_fail.is_empty() {
            // Both pass and fail cover current changes: latest result wins so a
            // clean retry with identical `active_set` returns to
            // `ObservedPassing` instead of sticking at `Mixed` forever.
            // Find latest covering (pass or fail) by position in `verifs`.
            let mut latest_is_pass = false;
            for v in verifs.iter().rev() {
                let ProvenanceEvent::VerificationObserved(vo) = &v.event else {
                    continue;
                };
                let matched = vo
                    .matched_obligations
                    .iter()
                    .any(|m| m.id == cur.obligation.id && m.binding_hash == cur.binding_hash);
                if !matched {
                    continue;
                }
                let observed: std::collections::HashSet<&str> =
                    vo.observed_change_ids.iter().map(String::as_str).collect();
                if !active_set.iter().all(|id| observed.contains(id)) {
                    continue;
                }
                latest_is_pass = vo.outcome.success;
                break;
            }
            if latest_is_pass {
                VerificationObligationEvidenceState::ObservedPassing
            } else {
                VerificationObligationEvidenceState::ObservedFailing
            }
        } else if !covering_pass.is_empty() {
            VerificationObligationEvidenceState::ObservedPassing
        } else if !covering_fail.is_empty() {
            VerificationObligationEvidenceState::ObservedFailing
        } else if any_current_hash_obs || any_same_id_old_hash {
            // There was a matching observation but it doesn't cover current.
            VerificationObligationEvidenceState::Stale
        } else {
            VerificationObligationEvidenceState::Pending
        };

        // If active + diverged/reverted history coexists with passing, surface
        // Mixed rather than hiding history (mirrors requirement logic).
        let state = if state == VerificationObligationEvidenceState::ObservedPassing
            && (!diverged.is_empty() || !reverted.is_empty())
        {
            VerificationObligationEvidenceState::Mixed
        } else {
            state
        };

        out.push(ObligationCoverage {
            obligation_id: cur.obligation.id.clone(),
            plan_item_id: cur.plan_item_id.clone(),
            kind: cur.obligation.kind,
            binding_hash: cur.binding_hash.clone(),
            state,
            active_change_ids: active.clone(),
        });
    }
    out
}

/// Build `plan_item_id -> requirement_ids` including obligations? No, obligations
/// stay on the plan; this helper is for diff evidence (file -> plan -> reqs).
pub fn plan_item_for_change(events: &[ProvenanceEventEnvelope], change_id: &str) -> Option<String> {
    for e in events {
        if e.event_id == change_id
            && let ProvenanceEvent::ChangeCommitted(c) = &e.event
        {
            return c.plan_item_id.clone();
        }
    }
    None
}

#[allow(dead_code)]
pub fn _transition_obligation_helpers_example(_t: &PlanItemTransition) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::store::ProvenanceStore;
    use crate::provenance::types::{
        ChangeCommittedEvent, ChangeKind, ChangeTarget, FileStateEvidence, ProvenanceEvent,
        VerificationContext, VerificationSource, file_content_hash,
    };
    use crate::provenance::verification::{VerificationRecordInput, build_verification_event};
    use crate::tools::plan::{PlanItem, VerificationCommandMatcher, VerificationObligation};

    fn test_obligation(id: &str) -> VerificationObligation {
        VerificationObligation {
            id: id.to_string(),
            description: format!("desc for {id}"),
            kind: VerificationKind::Test,
            command: Some(VerificationCommandMatcher {
                program: "cargo".to_string(),
                args_prefix: vec!["test".to_string(), "provenance::".to_string()],
            }),
        }
    }

    fn lint_obligation(id: &str) -> VerificationObligation {
        VerificationObligation {
            id: id.to_string(),
            description: "lint".to_string(),
            kind: VerificationKind::Lint,
            command: Some(VerificationCommandMatcher {
                program: "cargo".to_string(),
                args_prefix: vec!["clippy".to_string()],
            }),
        }
    }

    fn plan_item_with(id: &str, obs: Vec<VerificationObligation>) -> PlanItem {
        PlanItem {
            id: id.to_string(),
            parent_id: None,
            content: "work".to_string(),
            status: "in_progress".to_string(),
            requirement_ids: vec!["req-1".to_string()],
            verification_obligations: obs,
        }
    }

    // --- Matcher tests ---

    #[test]
    fn test_kind_only_match() {
        let ob = VerificationObligation {
            id: "vo-1".to_string(),
            description: "d".to_string(),
            kind: VerificationKind::Test,
            command: None,
        };
        assert!(obligation_matches_invocation(
            &ob,
            VerificationKind::Test,
            "cargo",
            &["test".to_string()]
        ));
    }

    #[test]
    fn test_kind_mismatch() {
        let ob = VerificationObligation {
            id: "vo-1".to_string(),
            description: "d".to_string(),
            kind: VerificationKind::Test,
            command: None,
        };
        assert!(!obligation_matches_invocation(
            &ob,
            VerificationKind::Lint,
            "cargo",
            &["clippy".to_string()]
        ));
    }

    #[test]
    fn test_program_match_and_mismatch() {
        let ob = test_obligation("vo-1");
        assert!(obligation_matches_invocation(
            &ob,
            VerificationKind::Test,
            "cargo",
            &["test".to_string(), "provenance::x".to_string()]
        ));
        assert!(!obligation_matches_invocation(
            &ob,
            VerificationKind::Test,
            "go",
            &["test".to_string(), "provenance::x".to_string()]
        ));
    }

    #[test]
    fn test_args_prefix_match_and_mismatch() {
        let ob = test_obligation("vo-1");
        assert!(obligation_matches_invocation(
            &ob,
            VerificationKind::Test,
            "cargo",
            &[
                "test".to_string(),
                "provenance::traceability_tests".to_string()
            ]
        ));
        // Different scope must not match.
        assert!(!obligation_matches_invocation(
            &ob,
            VerificationKind::Test,
            "cargo",
            &["test".to_string(), "tui::".to_string()]
        ));
        // Shorter actual than prefix.
        assert!(!obligation_matches_invocation(
            &ob,
            VerificationKind::Test,
            "cargo",
            &["test".to_string()]
        ));
    }

    #[test]
    fn test_empty_args_prefix_matches_any_argv() {
        let ob = VerificationObligation {
            id: "vo-1".to_string(),
            description: "d".to_string(),
            kind: VerificationKind::Lint,
            command: Some(VerificationCommandMatcher {
                program: "cargo".to_string(),
                args_prefix: vec![],
            }),
        };
        assert!(obligation_matches_invocation(
            &ob,
            VerificationKind::Lint,
            "cargo",
            &["clippy".to_string(), "--all".to_string()]
        ));
    }

    #[test]
    fn test_absolute_path_basename_normalization() {
        let ob = test_obligation("vo-1");
        assert!(obligation_matches_invocation(
            &ob,
            VerificationKind::Test,
            "/usr/bin/cargo",
            &["test".to_string(), "provenance::x".to_string()]
        ));
        assert!(obligation_matches_invocation(
            &ob,
            VerificationKind::Test,
            "cargo",
            &["test".to_string(), "provenance::x".to_string()]
        ));
        assert_eq!(executable_basename("/usr/bin/cargo"), "cargo");
        assert_eq!(executable_basename("cargo"), "cargo");
        assert_eq!(executable_basename("cargo.exe"), "cargo");
        assert_eq!(executable_basename("C:\\Rust\\bin\\cargo.exe"), "cargo");
        assert!(obligation_matches_invocation(
            &ob,
            VerificationKind::Test,
            "cargo.exe",
            &["test".to_string(), "provenance::x".to_string()]
        ));
    }

    // --- Binding hash tests ---

    #[test]
    fn test_binding_hash_stable() {
        let ob = test_obligation("vo-1");
        let h1 = obligation_binding_hash("step-1", &["req-1".to_string()], &ob);
        let h2 = obligation_binding_hash("step-1", &["req-1".to_string()], &ob);
        assert_eq!(h1, h2);
        assert!(h1.starts_with("blake3:"));
    }

    #[test]
    fn test_binding_hash_requirement_order_insensitive() {
        let ob = test_obligation("vo-1");
        let h1 = obligation_binding_hash("step-1", &["b".to_string(), "a".to_string()], &ob);
        let h2 = obligation_binding_hash("step-1", &["a".to_string(), "b".to_string()], &ob);
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_binding_hash_changes_on_definition() {
        let mut ob = test_obligation("vo-1");
        let base = obligation_binding_hash("step-1", &["req-1".to_string()], &ob);
        ob.description = "different".to_string();
        assert_ne!(
            base,
            obligation_binding_hash("step-1", &["req-1".to_string()], &ob)
        );
        let mut ob2 = test_obligation("vo-1");
        ob2.kind = VerificationKind::Lint;
        assert_ne!(
            base,
            obligation_binding_hash("step-1", &["req-1".to_string()], &ob2)
        );
        let mut ob3 = test_obligation("vo-1");
        ob3.command = Some(VerificationCommandMatcher {
            program: "cargo".to_string(),
            args_prefix: vec!["test".to_string(), "other::".to_string()],
        });
        assert_ne!(
            base,
            obligation_binding_hash("step-1", &["req-1".to_string()], &ob3)
        );
        // Plan item change.
        assert_ne!(
            base,
            obligation_binding_hash("step-2", &["req-1".to_string()], &test_obligation("vo-1"))
        );
        // Requirement link change.
        assert_ne!(
            base,
            obligation_binding_hash("step-1", &["req-2".to_string()], &test_obligation("vo-1"))
        );
    }

    // --- Coverage helpers ---

    fn v2_file_commit(
        store: &ProvenanceStore,
        session: &str,
        file: &str,
        before_content: &str,
        after_content: &str,
        plan_item_id: Option<String>,
    ) -> ProvenanceEventEnvelope {
        let before = FileStateEvidence {
            exists: true,
            content_hash: Some(file_content_hash(before_content)),
            byte_len: Some(before_content.len() as u64),
        };
        let after = FileStateEvidence {
            exists: true,
            content_hash: Some(file_content_hash(after_content)),
            byte_len: Some(after_content.len() as u64),
        };
        let loaded = store.load_all().unwrap();
        let predecessor = ProvenanceStore::find_predecessor(&loaded.events, file, &before);
        store
            .append(
                session,
                ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                    transaction_id: String::new(),
                    directive_id: None,
                    plan_item_id,
                    requirement_ids: vec!["req-1".to_string()],
                    change_kind: ChangeKind::TextEdit,
                    file: file.to_string(),
                    target: ChangeTarget::File,
                    before,
                    after,
                    predecessor_change_id: predecessor,
                    reverts_change_id: None,
                    diff: "d".to_string(),
                    diff_hash: "blake3:x".to_string(),
                    lines_added: 1,
                    lines_removed: 1,
                }),
            )
            .unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn record_verification(
        store: &ProvenanceStore,
        session: &str,
        kind: VerificationKind,
        program: &str,
        args: &[String],
        success: bool,
        observed: Vec<String>,
        matched: Vec<VerificationObligationRef>,
    ) {
        let event = build_verification_event(VerificationRecordInput {
            structured_test_result: None,
            kind,
            source: VerificationSource::ExecuteProcess,
            program,
            args,
            cwd_relative: None,
            success,
            status: "completed",
            exit_code: Some(if success { 0 } else { 1 }),
            timed_out: false,
            stdout: "out",
            stderr: "",
            capture_truncated: false,
            context: VerificationContext {
                execution_context: None,
                execution_workspace: None,
                plan_item_id: Some("step-1".to_string()),
                observed_change_ids: observed,
                directive_id: None,
                requirement_ids: vec!["req-1".to_string()],
                matched_obligations: matched,
            },
            extra_warnings: vec![],
        });
        store
            .append(session, ProvenanceEvent::VerificationObserved(event))
            .unwrap();
    }

    fn coverage_for(
        proj: &std::path::Path,
        store: &ProvenanceStore,
        plan: &[PlanItem],
    ) -> Vec<ObligationCoverage> {
        let loaded = store.load_all().unwrap();
        compute_obligation_coverage(proj, &loaded.events, plan)
    }

    #[test]
    fn test_case_a_pending_no_verification() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join("a.txt"), "h0\n").unwrap();
        let sess = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(sess.path().join("s"));
        std::fs::write(proj.path().join("a.txt"), "h1\n").unwrap();
        v2_file_commit(
            &store,
            "s",
            "a.txt",
            "h0\n",
            "h1\n",
            Some("step-1".to_string()),
        );
        let plan = vec![plan_item_with("step-1", vec![test_obligation("vo-1")])];
        let cov = coverage_for(proj.path(), &store, &plan);
        assert_eq!(cov.len(), 1);
        assert_eq!(cov[0].state, VerificationObligationEvidenceState::Pending);
    }

    #[test]
    fn test_case_b_observed_passing() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join("a.txt"), "h0\n").unwrap();
        let sess = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(sess.path().join("s"));
        std::fs::write(proj.path().join("a.txt"), "h1\n").unwrap();
        let c1 = v2_file_commit(
            &store,
            "s",
            "a.txt",
            "h0\n",
            "h1\n",
            Some("step-1".to_string()),
        );
        let plan = vec![plan_item_with("step-1", vec![test_obligation("vo-1")])];
        let binding =
            obligation_binding_hash("step-1", &["req-1".to_string()], &test_obligation("vo-1"));
        record_verification(
            &store,
            "s",
            VerificationKind::Test,
            "cargo",
            &["test".to_string(), "provenance::x".to_string()],
            true,
            vec![c1.event_id.clone()],
            vec![VerificationObligationRef {
                id: "vo-1".to_string(),
                binding_hash: binding,
            }],
        );
        let cov = coverage_for(proj.path(), &store, &plan);
        assert_eq!(
            cov[0].state,
            VerificationObligationEvidenceState::ObservedPassing
        );
    }

    #[test]
    fn test_case_c_observed_failing() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join("a.txt"), "h0\n").unwrap();
        let sess = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(sess.path().join("s"));
        std::fs::write(proj.path().join("a.txt"), "h1\n").unwrap();
        let c1 = v2_file_commit(
            &store,
            "s",
            "a.txt",
            "h0\n",
            "h1\n",
            Some("step-1".to_string()),
        );
        let plan = vec![plan_item_with("step-1", vec![test_obligation("vo-1")])];
        let binding =
            obligation_binding_hash("step-1", &["req-1".to_string()], &test_obligation("vo-1"));
        record_verification(
            &store,
            "s",
            VerificationKind::Test,
            "cargo",
            &["test".to_string(), "provenance::x".to_string()],
            false,
            vec![c1.event_id.clone()],
            vec![VerificationObligationRef {
                id: "vo-1".to_string(),
                binding_hash: binding,
            }],
        );
        let cov = coverage_for(proj.path(), &store, &plan);
        assert_eq!(
            cov[0].state,
            VerificationObligationEvidenceState::ObservedFailing
        );
    }

    #[test]
    fn test_case_d_stale_after_new_mutation() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join("a.txt"), "h0\n").unwrap();
        let sess = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(sess.path().join("s"));
        std::fs::write(proj.path().join("a.txt"), "h1\n").unwrap();
        let c1 = v2_file_commit(
            &store,
            "s",
            "a.txt",
            "h0\n",
            "h1\n",
            Some("step-1".to_string()),
        );
        let plan = vec![plan_item_with("step-1", vec![test_obligation("vo-1")])];
        let binding =
            obligation_binding_hash("step-1", &["req-1".to_string()], &test_obligation("vo-1"));
        record_verification(
            &store,
            "s",
            VerificationKind::Test,
            "cargo",
            &["test".to_string(), "provenance::x".to_string()],
            true,
            vec![c1.event_id.clone()],
            vec![VerificationObligationRef {
                id: "vo-1".to_string(),
                binding_hash: binding,
            }],
        );
        // New mutation C2.
        std::fs::write(proj.path().join("a.txt"), "h2\n").unwrap();
        v2_file_commit(
            &store,
            "s",
            "a.txt",
            "h1\n",
            "h2\n",
            Some("step-1".to_string()),
        );
        let cov = coverage_for(proj.path(), &store, &plan);
        // Old PASS must not remain passing.
        assert_ne!(
            cov[0].state,
            VerificationObligationEvidenceState::ObservedPassing
        );
        assert_eq!(cov[0].state, VerificationObligationEvidenceState::Stale);
    }

    #[test]
    fn test_case_e_unrelated_lint_does_not_satisfy_test() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join("a.txt"), "h0\n").unwrap();
        let sess = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(sess.path().join("s"));
        std::fs::write(proj.path().join("a.txt"), "h1\n").unwrap();
        let c1 = v2_file_commit(
            &store,
            "s",
            "a.txt",
            "h0\n",
            "h1\n",
            Some("step-1".to_string()),
        );
        // Plan has test obligation.
        let plan = vec![plan_item_with("step-1", vec![test_obligation("vo-test")])];
        // Record lint PASS observing C1 with lint obligation (different id/kind).
        let lint_binding = obligation_binding_hash(
            "step-1",
            &["req-1".to_string()],
            &lint_obligation("vo-lint"),
        );
        record_verification(
            &store,
            "s",
            VerificationKind::Lint,
            "cargo",
            &["clippy".to_string()],
            true,
            vec![c1.event_id.clone()],
            vec![VerificationObligationRef {
                id: "vo-lint".to_string(),
                binding_hash: lint_binding,
            }],
        );
        let cov = coverage_for(proj.path(), &store, &plan);
        assert_eq!(cov[0].state, VerificationObligationEvidenceState::Pending);
    }

    #[test]
    fn test_case_f_binding_change_invalidates_old_pass() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join("a.txt"), "h0\n").unwrap();
        let sess = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(sess.path().join("s"));
        std::fs::write(proj.path().join("a.txt"), "h1\n").unwrap();
        let c1 = v2_file_commit(
            &store,
            "s",
            "a.txt",
            "h0\n",
            "h1\n",
            Some("step-1".to_string()),
        );
        let old_ob = test_obligation("vo-1");
        let old_binding = obligation_binding_hash("step-1", &["req-1".to_string()], &old_ob);
        record_verification(
            &store,
            "s",
            VerificationKind::Test,
            "cargo",
            &["test".to_string(), "provenance::x".to_string()],
            true,
            vec![c1.event_id.clone()],
            vec![VerificationObligationRef {
                id: "vo-1".to_string(),
                binding_hash: old_binding,
            }],
        );
        // Change definition (description) after PASS.
        let mut new_ob = test_obligation("vo-1");
        new_ob.description = "changed description".to_string();
        let plan = vec![plan_item_with("step-1", vec![new_ob])];
        let cov = coverage_for(proj.path(), &store, &plan);
        assert_ne!(
            cov[0].state,
            VerificationObligationEvidenceState::ObservedPassing
        );
    }

    #[test]
    fn test_case_g_diverged() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join("a.txt"), "h0\n").unwrap();
        let sess = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(sess.path().join("s"));
        std::fs::write(proj.path().join("a.txt"), "h1\n").unwrap();
        v2_file_commit(
            &store,
            "s",
            "a.txt",
            "h0\n",
            "h1\n",
            Some("step-1".to_string()),
        );
        // External diverged edit.
        std::fs::write(proj.path().join("a.txt"), "diverged\n").unwrap();
        let plan = vec![plan_item_with("step-1", vec![test_obligation("vo-1")])];
        let cov = coverage_for(proj.path(), &store, &plan);
        assert_eq!(cov[0].state, VerificationObligationEvidenceState::Diverged);
    }

    #[test]
    fn test_case_h_reverted() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join("a.txt"), "h0\n").unwrap();
        let sess = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(sess.path().join("s"));
        std::fs::write(proj.path().join("a.txt"), "h1\n").unwrap();
        let c1 = v2_file_commit(
            &store,
            "s",
            "a.txt",
            "h0\n",
            "h1\n",
            Some("step-1".to_string()),
        );
        // Undo: h1 -> h0.
        std::fs::write(proj.path().join("a.txt"), "h0\n").unwrap();
        let before = FileStateEvidence {
            exists: true,
            content_hash: Some(file_content_hash("h1\n")),
            byte_len: Some(3),
        };
        let after = FileStateEvidence {
            exists: true,
            content_hash: Some(file_content_hash("h0\n")),
            byte_len: Some(3),
        };
        let loaded = store.load_all().unwrap();
        let predecessor = ProvenanceStore::find_predecessor(&loaded.events, "a.txt", &before);
        store
            .append(
                "s",
                ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                    transaction_id: String::new(),
                    directive_id: None,
                    plan_item_id: Some("step-1".to_string()),
                    requirement_ids: vec!["req-1".to_string()],
                    change_kind: ChangeKind::Undo,
                    file: "a.txt".to_string(),
                    target: ChangeTarget::File,
                    before,
                    after,
                    predecessor_change_id: predecessor,
                    reverts_change_id: Some(c1.event_id.clone()),
                    diff: "d".to_string(),
                    diff_hash: "blake3:x".to_string(),
                    lines_added: 0,
                    lines_removed: 0,
                }),
            )
            .unwrap();
        let plan = vec![plan_item_with("step-1", vec![test_obligation("vo-1")])];
        let cov = coverage_for(proj.path(), &store, &plan);
        // Reverted target should surface as Reverted (no active left).
        assert_eq!(cov[0].state, VerificationObligationEvidenceState::Reverted);
    }

    #[test]
    fn test_no_linked_change() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join("a.txt"), "h0\n").unwrap();
        let sess = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(sess.path().join("s"));
        let plan = vec![plan_item_with("step-1", vec![test_obligation("vo-1")])];
        let cov = coverage_for(proj.path(), &store, &plan);
        assert_eq!(
            cov[0].state,
            VerificationObligationEvidenceState::NoLinkedChange
        );
    }

    #[test]
    fn test_candidate_scoping_excludes_unrelated() {
        let plan = vec![
            PlanItem {
                id: "step-1".to_string(),
                parent_id: None,
                content: "a".to_string(),
                status: "in_progress".to_string(),
                requirement_ids: vec![],
                verification_obligations: vec![test_obligation("vo-1")],
            },
            PlanItem {
                id: "step-2".to_string(),
                parent_id: None,
                content: "b".to_string(),
                status: "pending".to_string(),
                requirement_ids: vec![],
                verification_obligations: vec![test_obligation("vo-2")],
            },
        ];
        let cands = collect_candidate_obligations(&plan, &[]);
        // Only in_progress candidate.
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].obligation.id, "vo-1");
        // Active link adds second even if completed/pending.
        let cands2 = collect_candidate_obligations(&plan, &["step-2".to_string()]);
        assert_eq!(cands2.len(), 2);
    }

    #[test]
    fn test_fail_then_pass_returns_to_passing() {
        // Transient failure followed by clean retry with identical active set
        // must return to ObservedPassing (latest wins), not stick at Mixed.
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join("a.txt"), "h0\n").unwrap();
        let sess = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(sess.path().join("s"));
        std::fs::write(proj.path().join("a.txt"), "h1\n").unwrap();
        let c1 = v2_file_commit(
            &store,
            "s",
            "a.txt",
            "h0\n",
            "h1\n",
            Some("step-1".to_string()),
        );
        let plan = vec![plan_item_with("step-1", vec![test_obligation("vo-1")])];
        let binding =
            obligation_binding_hash("step-1", &["req-1".to_string()], &test_obligation("vo-1"));
        let matched = || VerificationObligationRef {
            id: "vo-1".to_string(),
            binding_hash: binding.clone(),
        };
        record_verification(
            &store,
            "s",
            VerificationKind::Test,
            "cargo",
            &["test".to_string(), "provenance::x".to_string()],
            false,
            vec![c1.event_id.clone()],
            vec![matched()],
        );
        record_verification(
            &store,
            "s",
            VerificationKind::Test,
            "cargo",
            &["test".to_string(), "provenance::x".to_string()],
            true,
            vec![c1.event_id.clone()],
            vec![matched()],
        );
        let cov = coverage_for(proj.path(), &store, &plan);
        assert_eq!(
            cov[0].state,
            VerificationObligationEvidenceState::ObservedPassing
        );
    }
}
