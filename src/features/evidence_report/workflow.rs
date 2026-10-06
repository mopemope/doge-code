//! Explicit review navigation; no automatic export, execution or session writes.
use super::{EvidenceReport, build_with, workspace::GitReader};
use crate::features::verification_snapshot::{CurrentState, RunState};
use serde::Serialize;
use std::{collections::BTreeSet, path::Path};

#[derive(Debug, Serialize)]
pub struct ReviewLink {
    pub session_id: String,
    pub project_root: String,
    pub evidence_command: Vec<String>,
}
impl ReviewLink {
    pub fn new(root: &Path, session_id: &str) -> Option<Self> {
        // Full captured IDs only; never interpolate untrusted paths or prefixes
        // into a runnable command, and never fall back to the latest session.
        uuid::Uuid::parse_str(session_id).ok()?;
        Some(Self {
            session_id: session_id.into(),
            project_root: root.to_string_lossy().into_owned(),
            evidence_command: vec![
                "dgc".into(),
                "session".into(),
                "evidence".into(),
                session_id.into(),
            ],
        })
    }
    pub fn lines(&self) -> Vec<String> {
        vec![
            format!("[evidence] Saved session: {}", self.session_id),
            format!(
                "[evidence] Run from project: {}",
                safe_text(&self.project_root)
            ),
            format!(
                "[evidence] Inspect/export to stdout: {}",
                self.evidence_command.join(" ")
            ),
        ]
    }
}
fn safe_text(value: &str) -> String {
    value
        .chars()
        .flat_map(|c| {
            if c.is_control() {
                format!("U+{:04X}", c as u32).chars().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

#[derive(Debug)]
pub struct ReviewSummary {
    pub link: ReviewLink,
    pub generated_at: String,
    pub changes: usize,
    pub matching_successes: usize,
    pub failures: usize,
    pub other_successes: usize,
    pub unlinked_observations: usize,
    pub unknown_correspondence: usize,
    pub unattributed_files: usize,
    pub warnings: usize,
    pub incomplete: bool,
}
impl ReviewSummary {
    pub(super) fn from_report(root: &Path, report: &EvidenceReport) -> Option<Self> {
        let matching: BTreeSet<_> = report
            .review_handoff
            .iter()
            .flat_map(|r| &r.matching_successful_observation_ids)
            .collect();
        let failed: BTreeSet<_> = report
            .review_handoff
            .iter()
            .flat_map(|r| &r.failed_observation_ids)
            .collect();
        let other: BTreeSet<_> = report
            .review_handoff
            .iter()
            .flat_map(|r| &r.other_successful_observation_ids)
            .collect();
        let linked: BTreeSet<_> = matching
            .iter()
            .chain(&failed)
            .chain(&other)
            .copied()
            .collect();
        Some(Self {
            link: ReviewLink::new(root, &report.session.id)?,
            generated_at: report.generated_at.clone(),
            changes: report.changes.len(),
            matching_successes: matching.len(),
            failures: failed.len(),
            other_successes: other.len(),
            unlinked_observations: report
                .verifications
                .iter()
                .filter(|v| !linked.contains(&v.id))
                .count(),
            unknown_correspondence: report
                .verifications
                .iter()
                .filter(|v| {
                    matches!(
                        v.current_code_state.state,
                        CurrentState::NotRecorded | CurrentState::Indeterminate
                    ) || v.execution_workspace.as_ref().is_none_or(|w| {
                        matches!(w.run_state, RunState::NotRecorded | RunState::Indeterminate)
                    })
                })
                .count(),
            unattributed_files: report.summary.unattributed_files,
            warnings: report.warnings.len(),
            incomplete: !report.summary.record_collection_complete
                || report.session.provenance_incomplete
                || report.session.provenance_record_failures > 0
                || !report.plan_available
                || !report.repository.comparison_available,
        })
    }
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![format!("[evidence] Fresh comparison of saved evidence at {}", self.generated_at),
            format!("[evidence] Recorded changes: {}. Collection/comparison: {}. Warnings: {}. Unattributed files: {}.", self.changes, if self.incomplete { "incomplete/unavailable" } else { "available" }, self.warnings, self.unattributed_files),
            format!("[evidence] Linked observation IDs: matching successes {}, failures {}, other successes {} (unique per category; categories may overlap across changes).", self.matching_successes, self.failures, self.other_successes),
            format!("[evidence] Unlinked observations: {}. Unknown execution/current correspondence: {}.", self.unlinked_observations, self.unknown_correspondence),
            "[evidence] Historical success, including legacy or stale observations, is not current confirmation. Empty evidence is not success or approval. Read the per-change matrix and diagnostics in the full report.".into()];
        lines.extend(self.link.lines());
        lines
    }
}
pub async fn review_summary(root: &Path, session_id: &str) -> super::Result<ReviewSummary> {
    let report = build_with(
        root,
        session_id,
        None,
        false,
        chrono::Utc::now().to_rfc3339(),
        &GitReader::default(),
        &mut |_| {},
    )
    .await?;
    // A UUID denotes an exact captured session, not a prefix or latest alias.
    if uuid::Uuid::parse_str(session_id).is_ok() && report.session.id != session_id {
        return Err(super::ReportError::UnsafeInput);
    }
    ReviewSummary::from_report(root, &report).ok_or(super::ReportError::UnsafeInput)
}
