//! Selected-file projection of a fresh report. Only recorded IDs establish links.
use super::{EvidenceReport, Result};
use std::{collections::BTreeSet, path::Path};

const MAX_LINES: usize = 256;
const MAX_FIELD_CHARS: usize = 1024;

/// Escape terminal controls and bound every source field, preserving literal paths.
fn field(value: impl AsRef<str>) -> String {
    let value = value.as_ref();
    let mut output = String::new();
    for c in value.chars().take(MAX_FIELD_CHARS) {
        if c.is_control() {
            output.push_str(&format!("\\u{{{:x}}}", c as u32));
        } else {
            output.push(c);
        }
    }
    if value.chars().count() > MAX_FIELD_CHARS {
        output.push_str("… [field truncated]");
    }
    output
}

pub async fn file_detail(root: &Path, session_id: &str, path: &str) -> Result<Vec<String>> {
    // The UI captures a full ID; never fall back to the latest/current session.
    super::ReviewLink::new(root, session_id).ok_or(super::ReportError::UnsafeInput)?;
    from_report(&super::inspect(root, session_id).await?, path)
}

pub(super) fn from_report(report: &EvidenceReport, path: &str) -> Result<Vec<String>> {
    let mut lines = vec![
        format!("File: {}", field(path)),
        format!("Session: {}", field(&report.session.id)),
        format!("Compared at: {}", field(&report.generated_at)),
        "Read-only observation; no execution or approval. Reopen after edits to compare again."
            .into(),
        "Recorded links and endpoint snapshots are not runtime coverage or correctness proof."
            .into(),
        format!(
            "Collection: {}; snapshot complete: {}",
            if report.summary.record_collection_complete {
                "complete"
            } else {
                "incomplete / unavailable"
            },
            report.snapshot.complete
        ),
    ];
    for warning in report.warnings.iter().take(8) {
        lines.push(format!(
            "Warning {:?}: {}",
            warning.code,
            field(&warning.message)
        ));
    }
    if report.warnings.len() > 8 {
        lines.push("Additional report warnings omitted; inspect /evidence or CLI report.".into());
    }
    let changes: Vec<_> = report
        .changes
        .iter()
        .filter(|c| c.recorded.file == path)
        .collect();
    if changes.is_empty() {
        lines.push(
            "No recorded change IDs for this exact file; verification correspondence unconfirmed."
                .into(),
        );
    }
    let ids: BTreeSet<_> = changes.iter().map(|c| c.id.as_str()).collect();
    for change in changes.iter().take(32) {
        lines.push(format!(
            "Change {}: {:?}; current file {:?}",
            field(&change.id),
            change.lifecycle_state,
            change.current_file_match
        ));
    }
    if changes.len() > 32 {
        lines.push("Additional change IDs omitted; inspect CLI report.".into());
    }
    let linked: Vec<_> = report
        .verifications
        .iter()
        .filter(|v| {
            v.observed_change_ids
                .iter()
                .any(|id| ids.contains(id.as_str()))
        })
        .collect();
    if linked.is_empty() {
        lines.push(
            "No explicitly linked verification observations; outcome and counts unknown.".into(),
        );
    }
    for v in linked.iter().take(20) {
        lines.push(String::new());
        lines.push(format!(
            "Verification {} ({:?}, {:?})",
            field(&v.id),
            v.kind,
            v.source
        ));
        let linked_ids: Vec<_> = v
            .observed_change_ids
            .iter()
            .filter(|id| ids.contains(id.as_str()))
            .collect();
        lines.push(format!(
            "Recorded change IDs: {}",
            field(serde_json::to_string(&linked_ids)?)
        ));
        lines.push(format!(
            "Program: {}",
            field(serde_json::to_string(&v.program)?)
        ));
        lines.push(format!("Argv: {}", field(serde_json::to_string(&v.args)?)));
        lines.push(format!("Cwd: {}", field(serde_json::to_string(&v.cwd)?)));
        lines.push(format!(
            "Process outcome: {}; success={}; exit={:?}; timed_out={}",
            field(&v.outcome.status),
            v.outcome.success,
            v.outcome.exit_code,
            v.outcome.timed_out
        ));
        match &v.structured_test_result {
            Some(result) => lines.push(format!(
                "Test observation: {}",
                field(serde_json::to_string(&result.observation)?)
            )),
            None => lines.push("Test observation: unknown (no supported structured result)".into()),
        }
        lines.push(format!(
            "Test count: {}; unit: {}",
            v.test_count
                .map_or_else(|| "unknown".into(), |n| n.to_string()),
            v.test_count_unit.unwrap_or("unknown")
        ));
        lines.push(format!(
            "Execution snapshots: {:?}; current correspondence: {:?}",
            v.execution_workspace.as_ref().map_or(
                crate::features::verification_snapshot::RunState::NotRecorded,
                |w| w.run_state
            ),
            v.current_code_state.state
        ));
        if let Some(workspace) = &v.execution_workspace {
            lines.push(format!(
                "Start digest: {}; end digest: {}",
                field(
                    workspace
                        .start
                        .manifest_digest
                        .as_deref()
                        .unwrap_or("unknown")
                ),
                field(
                    workspace
                        .end
                        .as_ref()
                        .and_then(|s| s.manifest_digest.as_deref())
                        .unwrap_or("unknown")
                )
            ));
        }
        let matching = report.review_handoff.iter().any(|r| {
            ids.contains(r.change_id.as_str())
                && r.matching_successful_observation_ids.contains(&v.id)
        });
        lines.push(if matching { "Process success matches a recorded active change and observed endpoints; test outcome remains separate." } else { "Current success unconfirmed: stale, missing, failed or unavailable correspondence; old success is historical." }.into());
        for warning in v.warnings.iter().take(2) {
            lines.push(format!("Verification warning: {}", field(warning)));
        }
    }
    if linked.len() > 20 {
        lines.push("Additional linked observations omitted; inspect CLI report.".into());
    }
    if lines.len() > MAX_LINES {
        lines.truncate(MAX_LINES - 1);
        lines.push("Detail truncated; inspect the full CLI evidence report.".into());
    }
    Ok(lines)
}
