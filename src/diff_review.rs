use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffReviewPayload {
    #[serde(default)]
    pub review_id: Option<String>,
    #[serde(default)]
    pub reject_reason: Option<String>,
    pub diff: String,
    pub files: Vec<String>,
    #[serde(default)]
    pub evidence: Vec<DiffFileEvidence>,
    #[serde(default)]
    pub evidence_warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffFileEvidence {
    pub path: String,
    #[serde(default)]
    pub requirement_ids: Vec<String>,
    #[serde(default)]
    pub plan_item_ids: Vec<String>,
    #[serde(default)]
    pub obligations: Vec<DiffObligationEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffObligationEvidence {
    pub id: String,
    pub description: String,
    pub kind: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_summary: Option<String>,
}

/// Bounds for evidence payloads (TUI message safety).
pub const DIFF_EVIDENCE_MAX_OBLIGATIONS_PER_FILE: usize = 10;
pub const DIFF_EVIDENCE_MAX_DESCRIPTION_CHARS: usize = 200;
pub const DIFF_EVIDENCE_MAX_COMMAND_CHARS: usize = 200;
pub const DIFF_EVIDENCE_MAX_IDS_PER_FILE: usize = 20;

/// Build per-file evidence from provenance state.
///
/// `files` are project-relative paths from the git diff (already scoped to
/// agent-modified files). Only current (non-superseded) changes contribute;
/// superseded history is never shown as current evidence. Failures are
/// returned as warnings, never as hard errors — callers must keep the diff
/// usable even when evidence is incomplete.
pub fn build_diff_review_evidence(
    project_root: &std::path::Path,
    events: &[crate::provenance::ProvenanceEventEnvelope],
    plan_items: &[crate::tools::plan::PlanItem],
    files: &[String],
) -> (Vec<DiffFileEvidence>, Vec<String>) {
    use crate::provenance::query::{ActiveChangeState, resolve_active_states};
    use std::collections::{HashMap, HashSet};

    let mut warnings = Vec::new();
    // Resolve once for all files.
    let resolved = resolve_active_states(project_root, events);
    // change_id -> (file, state, plan_item_id, requirement_ids)
    let mut by_file: HashMap<String, Vec<(String, ActiveChangeState, Option<String>)>> =
        HashMap::new();
    // Map change_id -> event for requirement lookup.
    let mut event_by_id: HashMap<&str, &crate::provenance::ProvenanceEventEnvelope> =
        HashMap::new();
    for e in events {
        event_by_id.insert(e.event_id.as_str(), e);
    }
    for r in &resolved {
        if r.state == ActiveChangeState::Superseded {
            continue;
        }
        by_file.entry(r.file.clone()).or_default().push((
            r.change_id.clone(),
            r.state,
            r.plan_item_id.clone(),
        ));
    }

    // Current obligation coverage for state labels.
    let obligation_coverages = crate::provenance::obligations::compute_obligation_coverage(
        project_root,
        events,
        plan_items,
    );
    let mut coverage_by_key: HashMap<
        (String, String),
        &crate::provenance::obligations::ObligationCoverage,
    > = HashMap::new();
    for c in &obligation_coverages {
        coverage_by_key.insert((c.plan_item_id.clone(), c.obligation_id.clone()), c);
    }
    // Actually map plan_id -> &PlanItem (single).
    let mut plan_by_id: HashMap<&str, &crate::tools::plan::PlanItem> = HashMap::new();
    for item in plan_items {
        plan_by_id.insert(item.id.as_str(), item);
    }

    let mut out = Vec::new();
    for file in files {
        let normalized = file.trim().trim_start_matches("./").replace('\\', "/");
        // Find changes for this file (exact match; git paths are relative).
        let changes = by_file.get(&normalized).cloned().unwrap_or_default();
        // Also try without normalization (exact payload string).
        let changes = if changes.is_empty() && by_file.contains_key(file) {
            by_file.get(file).cloned().unwrap_or_default()
        } else {
            changes
        };
        if changes.is_empty() {
            continue;
        }
        // Collect plan ids + requirement ids from non-superseded changes.
        let mut plan_ids: HashSet<String> = HashSet::new();
        let mut req_ids: HashSet<String> = HashSet::new();
        for (cid, _state, pid) in &changes {
            if let Some(pid) = pid {
                plan_ids.insert(pid.clone());
                // Current plan links for this plan item.
                if let Some(item) = plan_by_id.get(pid.as_str()) {
                    for r in &item.requirement_ids {
                        req_ids.insert(r.clone());
                    }
                }
            }
            if let Some(env) = event_by_id.get(cid.as_str())
                && let crate::provenance::ProvenanceEvent::ChangeCommitted(c) = &env.event
            {
                for r in &c.requirement_ids {
                    req_ids.insert(r.clone());
                }
            }
        }
        let mut plan_ids: Vec<String> = plan_ids.into_iter().collect();
        plan_ids.sort();
        let mut req_ids: Vec<String> = req_ids.into_iter().collect();
        req_ids.sort();
        // Bound id lists.
        if plan_ids.len() > DIFF_EVIDENCE_MAX_IDS_PER_FILE {
            warnings.push(format!(
                "Evidence for '{normalized}' truncated plan items to {DIFF_EVIDENCE_MAX_IDS_PER_FILE}."
            ));
            plan_ids.truncate(DIFF_EVIDENCE_MAX_IDS_PER_FILE);
        }
        if req_ids.len() > DIFF_EVIDENCE_MAX_IDS_PER_FILE {
            warnings.push(format!(
                "Evidence for '{normalized}' truncated requirements to {DIFF_EVIDENCE_MAX_IDS_PER_FILE}."
            ));
            req_ids.truncate(DIFF_EVIDENCE_MAX_IDS_PER_FILE);
        }

        // Obligations for these plan items.
        let mut obligations: Vec<DiffObligationEvidence> = Vec::new();
        for pid in &plan_ids {
            let Some(item) = plan_by_id.get(pid.as_str()) else {
                continue;
            };
            for ob in &item.verification_obligations {
                let state = coverage_by_key
                    .get(&(pid.clone(), ob.id.clone()))
                    .map(|c| c.state.as_str().to_string())
                    .unwrap_or_else(|| "pending".to_string());
                let description = crate::tools::budget::head_tail_truncate(
                    &ob.description,
                    DIFF_EVIDENCE_MAX_DESCRIPTION_CHARS,
                )
                .text;
                let command_summary = ob.command.as_ref().map(|c| {
                    let mut s = c.program.clone();
                    if !c.args_prefix.is_empty() {
                        s.push(' ');
                        s.push_str(&c.args_prefix.join(" "));
                    }
                    crate::tools::budget::head_tail_truncate(&s, DIFF_EVIDENCE_MAX_COMMAND_CHARS)
                        .text
                });
                let kind_str = match ob.kind {
                    crate::provenance::VerificationKind::Test => "test".to_string(),
                    crate::provenance::VerificationKind::Build => "build".to_string(),
                    crate::provenance::VerificationKind::Lint => "lint".to_string(),
                    crate::provenance::VerificationKind::TypeCheck => "type_check".to_string(),
                    crate::provenance::VerificationKind::FormatCheck => "format_check".to_string(),
                    crate::provenance::VerificationKind::SyntaxCheck => "syntax_check".to_string(),
                };
                obligations.push(DiffObligationEvidence {
                    id: ob.id.clone(),
                    description,
                    kind: kind_str,
                    state,
                    command_summary,
                });
            }
        }
        // Obligations for these plan items. Sort non-passing first so
        // truncation never hides a failing/stale obligation behind passing
        // `a-*` ids while a failing `z-*` is cut.
        fn obligation_priority(state: &str) -> u8 {
            match state {
                "observed_passing" => 1,
                _ => 0,
            }
        }
        obligations.sort_by(|a, b| {
            obligation_priority(&a.state)
                .cmp(&obligation_priority(&b.state))
                .then(a.id.cmp(&b.id))
        });
        if obligations.len() > DIFF_EVIDENCE_MAX_OBLIGATIONS_PER_FILE {
            warnings.push(format!(
                "Evidence for '{normalized}' truncated obligations to {DIFF_EVIDENCE_MAX_OBLIGATIONS_PER_FILE}."
            ));
            obligations.truncate(DIFF_EVIDENCE_MAX_OBLIGATIONS_PER_FILE);
        }
        // Only emit files with some linkage; pure superseded already skipped.
        if plan_ids.is_empty() && req_ids.is_empty() && obligations.is_empty() {
            continue;
        }
        out.push(DiffFileEvidence {
            path: file.clone(),
            requirement_ids: req_ids,
            plan_item_ids: plan_ids,
            obligations,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    (out, warnings)
}

/// Enrich a plain diff payload with provenance evidence.
///
/// Never fails the diff: provenance load failures or missing sessions become
/// `evidence_warnings`, and the original diff/files are preserved.
pub fn enrich_diff_review_with_evidence(
    payload: DiffReviewPayload,
    project_root: &std::path::Path,
    events: &[crate::provenance::ProvenanceEventEnvelope],
    plan_items: &[crate::tools::plan::PlanItem],
) -> DiffReviewPayload {
    let (evidence, mut warnings) =
        build_diff_review_evidence(project_root, events, plan_items, &payload.files);
    warnings.extend(payload.evidence_warnings.clone());
    DiffReviewPayload {
        review_id: payload.review_id,
        reject_reason: payload.reject_reason,
        diff: payload.diff,
        files: payload.files,
        evidence,
        evidence_warnings: warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_legacy_payload_deserializes_with_defaults() {
        let json = serde_json::json!({
            "diff": "d",
            "files": ["a.txt"],
        });
        let payload: DiffReviewPayload = serde_json::from_value(json).unwrap();
        assert!(payload.evidence.is_empty());
        assert!(payload.evidence_warnings.is_empty());
    }

    #[test]
    fn test_evidence_roundtrip() {
        let payload = DiffReviewPayload {
            review_id: None,
            reject_reason: None,
            diff: "d".to_string(),
            files: vec!["a.txt".to_string()],
            evidence: vec![DiffFileEvidence {
                path: "a.txt".to_string(),
                requirement_ids: vec!["r1".to_string()],
                plan_item_ids: vec!["s1".to_string()],
                obligations: vec![DiffObligationEvidence {
                    id: "vo-1".to_string(),
                    description: "desc".to_string(),
                    kind: "test".to_string(),
                    state: "pending".to_string(),
                    command_summary: Some("cargo test".to_string()),
                }],
            }],
            evidence_warnings: vec![],
        };
        let json = serde_json::to_value(&payload).unwrap();
        let back: DiffReviewPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back.evidence.len(), 1);
    }

    fn make_plan_with_obligation() -> Vec<crate::tools::plan::PlanItem> {
        vec![crate::tools::plan::PlanItem {
            id: "step-1".to_string(),
            parent_id: None,
            content: "work".to_string(),
            status: "in_progress".to_string(),
            requirement_ids: vec!["req-1".to_string()],
            verification_obligations: vec![crate::tools::plan::VerificationObligation {
                id: "vo-1".to_string(),
                description: "desc".to_string(),
                kind: crate::provenance::VerificationKind::Test,
                command: Some(crate::tools::plan::VerificationCommandMatcher {
                    program: "cargo".to_string(),
                    args_prefix: vec!["test".to_string()],
                }),
            }],
        }]
    }

    fn make_change_event(
        event_id: &str,
        file: &str,
        plan_item_id: Option<&str>,
        before: &str,
        after: &str,
    ) -> crate::provenance::ProvenanceEventEnvelope {
        use crate::provenance::types::*;
        ProvenanceEventEnvelope {
            schema_version: crate::provenance::types::PROVENANCE_SCHEMA_VERSION,
            event_id: event_id.to_string(),
            session_id: "s".to_string(),
            timestamp: "2026-01-01T00:00:00+00:00".to_string(),
            event: ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                transaction_id: event_id.to_string(),
                directive_id: None,
                plan_item_id: plan_item_id.map(str::to_string),
                requirement_ids: vec!["req-1".to_string()],
                change_kind: ChangeKind::TextEdit,
                file: file.to_string(),
                target: ChangeTarget::File,
                before: FileStateEvidence {
                    exists: true,
                    content_hash: Some(file_content_hash(before)),
                    byte_len: Some(before.len() as u64),
                },
                after: FileStateEvidence {
                    exists: true,
                    content_hash: Some(file_content_hash(after)),
                    byte_len: Some(after.len() as u64),
                },
                predecessor_change_id: None,
                reverts_change_id: None,
                diff: String::new(),
                diff_hash: String::new(),
                lines_added: 0,
                lines_removed: 0,
            }),
        }
    }

    #[test]
    fn test_evidence_shows_active_requirement_only() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join("a.txt"), "after\n").unwrap();
        let c1 = make_change_event("c1", "a.txt", Some("step-1"), "before\n", "after\n");
        let plan = make_plan_with_obligation();
        let (ev, _) = build_diff_review_evidence(proj.path(), &[c1], &plan, &["a.txt".to_string()]);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].requirement_ids, vec!["req-1".to_string()]);
        assert_eq!(ev[0].plan_item_ids, vec!["step-1".to_string()]);
    }

    #[test]
    fn test_superseded_not_shown_as_current() {
        // Two changes for same file where only latest matches workspace.
        // Old change is superseded and must not contribute.
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join("a.txt"), "h2\n").unwrap();
        let c1 = make_change_event("c1", "a.txt", Some("step-1"), "h0\n", "h1\n");
        let mut c2 = make_change_event("c2", "a.txt", Some("step-1"), "h1\n", "h2\n");
        // Link predecessor so resolver treats c1 as superseded.
        if let crate::provenance::ProvenanceEvent::ChangeCommitted(ref mut cc) = c2.event {
            cc.predecessor_change_id = Some("c1".to_string());
        }
        let plan = make_plan_with_obligation();
        let (ev, _) =
            build_diff_review_evidence(proj.path(), &[c1, c2], &plan, &["a.txt".to_string()]);
        // Still one file entry (current), not duplicated superseded.
        assert_eq!(ev.len(), 1);
    }

    #[test]
    fn test_unrelated_file_not_included() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join("a.txt"), "after\n").unwrap();
        let c1 = make_change_event("c1", "a.txt", Some("step-1"), "before\n", "after\n");
        let plan = make_plan_with_obligation();
        // Files list contains only unrelated file.
        let (ev, _) =
            build_diff_review_evidence(proj.path(), &[c1], &plan, &["user_file.txt".to_string()]);
        assert!(ev.is_empty());
    }

    #[test]
    fn test_file_a_evidence_not_assigned_to_b() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join("a.txt"), "after\n").unwrap();
        std::fs::write(proj.path().join("b.txt"), "b-after\n").unwrap();
        let c1 = make_change_event("c1", "a.txt", Some("step-1"), "before\n", "after\n");
        let plan = make_plan_with_obligation();
        let (ev, _) = build_diff_review_evidence(
            proj.path(),
            &[c1],
            &plan,
            &["a.txt".to_string(), "b.txt".to_string()],
        );
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].path, "a.txt");
    }

    #[test]
    fn test_description_budget_truncated() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join("a.txt"), "after\n").unwrap();
        let c1 = make_change_event("c1", "a.txt", Some("step-1"), "before\n", "after\n");
        let mut plan = make_plan_with_obligation();
        plan[0].verification_obligations[0].description = "x".repeat(1000);
        let (ev, warnings) =
            build_diff_review_evidence(proj.path(), &[c1], &plan, &["a.txt".to_string()]);
        assert_eq!(ev.len(), 1);
        assert!(
            ev[0].obligations[0].description.chars().count() <= DIFF_EVIDENCE_MAX_DESCRIPTION_CHARS
        );
        let _ = warnings;
    }

    #[test]
    fn test_missing_provenance_still_succeeds() {
        let proj = tempfile::tempdir().unwrap();
        let plan = make_plan_with_obligation();
        let (ev, _) = build_diff_review_evidence(proj.path(), &[], &plan, &["a.txt".to_string()]);
        assert!(ev.is_empty());
    }

    #[test]
    fn test_obligation_count_bounded_with_warning() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join("a.txt"), "after\n").unwrap();
        let c1 = make_change_event("c1", "a.txt", Some("step-1"), "before\n", "after\n");
        let mut obs = Vec::new();
        for i in 0..30 {
            obs.push(crate::tools::plan::VerificationObligation {
                id: format!("vo-{i:02}"),
                description: "d".to_string(),
                kind: crate::provenance::VerificationKind::Test,
                command: None,
            });
        }
        let plan = vec![crate::tools::plan::PlanItem {
            id: "step-1".to_string(),
            parent_id: None,
            content: "w".to_string(),
            status: "in_progress".to_string(),
            requirement_ids: vec![],
            verification_obligations: obs,
        }];
        let (ev, warnings) =
            build_diff_review_evidence(proj.path(), &[c1], &plan, &["a.txt".to_string()]);
        assert_eq!(
            ev[0].obligations.len(),
            DIFF_EVIDENCE_MAX_OBLIGATIONS_PER_FILE
        );
        assert!(!warnings.is_empty());
    }
}
