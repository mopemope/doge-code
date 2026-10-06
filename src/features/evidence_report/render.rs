use std::fmt::Write;

use super::model::*;
use super::{MAX_OUTPUT_BYTES, ReportError, Result};

pub(super) fn render(report: &EvidenceReport, format: ReportFormat) -> Result<String> {
    // Render fully before touching stdout; errors never produce partial JSON.
    let json = serde_json::to_string_pretty(report)?;
    if json.len() > MAX_OUTPUT_BYTES {
        return Err(ReportError::Limit("output bytes"));
    }
    let output = match format {
        ReportFormat::Json => format!("{json}\n"),
        ReportFormat::Markdown => markdown(report, &json),
    };
    if output.len() > MAX_OUTPUT_BYTES {
        return Err(ReportError::Limit("output bytes"));
    }
    Ok(output)
}

/// Entities prevent user-controlled markdown/HTML/link syntax from becoming
/// active markup. Control characters are visible rather than terminal escapes.
fn cell(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '\n' | '\r' => out.push_str(" / "),
            c if c.is_control() => {
                let _ = write!(out, "U+{:04X}", c as u32);
            }
            '&' | '<' | '>' | '|' | '`' | '[' | ']' | '(' | ')' | '*' | '_' | '#' | '\\' | '!' => {
                let _ = write!(out, "&#{};", c as u32);
            }
            _ => out.push(c),
        }
    }
    out
}

fn observation_ids(ids: &[String]) -> String {
    if ids.is_empty() {
        "none recorded".into()
    } else {
        cell(&ids.join(", "))
    }
}

fn markdown(r: &EvidenceReport, json: &str) -> String {
    let mut out = String::from("# Dgc evidence report\n\n");
    let _ = writeln!(
        out,
        "Session: {}  \nGenerated: {}\n",
        cell(&r.session.id),
        cell(&r.generated_at)
    );
    out.push_str("Recorded command outcomes are observations, not a correctness proof. Requirement coverage does not imply every obligation passed.\n\n");
    let _ = writeln!(
        out,
        "Successful command observations: {}. Failed observations: {}. Unattributed files: {}.\n",
        r.summary.verification_successes,
        r.summary.verification_failures,
        r.summary.unattributed_files
    );
    let _ = writeln!(
        out,
        "HEAD: {}. Comparison base: {}.\nManifest: {}. Complete selected-file manifest: {}.\n",
        cell(r.repository.head_oid.as_deref().unwrap_or("unavailable")),
        cell(r.repository.base_oid.as_deref().unwrap_or("unavailable")),
        cell(&r.snapshot.manifest_digest),
        r.snapshot.complete
    );
    out.push_str("## Review handoff by recorded change\n\n");
    out.push_str("Matching successes require an active change matching its recorded file, stable recorded execution endpoints and current selected files matching the execution start. These are observations, not requirement satisfaction or approval. Other successes include historical, changed and unknown correspondence. Every linked failure remains visible; later successes do not erase it. See command observations for each outcome and correspondence.\n\n");
    out.push_str("| Change | File | Lifecycle / file match | Recorded requirements / plan | Matching successful observations | Failed observations (all correspondence) | Other successful observations |\n|---|---|---|---|---|---|---|\n");
    for (change, row) in r.changes.iter().zip(&r.review_handoff) {
        let _ = writeln!(
            out,
            "| {} | {} | {:?} / {:?} | {} / {} | {} | {} | {} |",
            cell(&row.change_id),
            cell(&change.recorded.file),
            change.lifecycle_state,
            change.current_file_match,
            cell(&change.recorded.requirement_ids.join(", ")),
            cell(
                change
                    .recorded
                    .plan_item_id
                    .as_deref()
                    .unwrap_or("not recorded")
            ),
            observation_ids(&row.matching_successful_observation_ids),
            observation_ids(&row.failed_observation_ids),
            observation_ids(&row.other_successful_observation_ids)
        );
    }
    if r.review_handoff.is_empty() {
        out.push_str("No recorded changes. Empty evidence does not establish success.\n");
    }
    out.push_str("\nUnlinked command observations and unattributed workspace files remain in the sections below. An empty matching-success cell requires review; it is not a failed test result.\n\n");
    out.push_str("## Items requiring attention\n\n");
    for w in &r.warnings {
        let _ = writeln!(out, "- {}", cell(&w.message));
    }
    for o in r
        .obligations
        .iter()
        .filter(|o| o.state != EvidenceState::ObservedPassing)
    {
        let _ = writeln!(
            out,
            "- Obligation {} / {}: {}",
            cell(&o.plan_item_id),
            cell(&o.id),
            cell(o.state.label())
        );
    }
    if r.warnings.is_empty()
        && r.obligations
            .iter()
            .all(|o| o.state == EvidenceState::ObservedPassing)
    {
        out.push_str("No additional diagnostics. Empty evidence does not establish success.\n");
    }
    for v in &r.verifications {
        let run = v
            .execution_workspace
            .as_ref()
            .map(|s| s.run_state)
            .unwrap_or(crate::features::verification_snapshot::RunState::NotRecorded);
        if run != crate::features::verification_snapshot::RunState::StableEndpoints
            || v.current_code_state.state
                != crate::features::verification_snapshot::CurrentState::MatchesStart
        {
            let _ = writeln!(
                out,
                "- Verification {}: execution endpoints {:?}; current code {:?}. Historical command outcome and coverage are independent.",
                cell(&v.id),
                run,
                v.current_code_state.state
            );
        }
    }
    out.push_str("\n## Requirements interpreted by the agent\n\n| ID | Statement | Evidence | Obligation states |\n|---|---|---|---|\n");
    for req in &r.requirements {
        let states = serde_json::to_string(&req.obligation_states).unwrap_or_default();
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            cell(&req.id),
            cell(&req.statement),
            cell(req.evidence_state.label()),
            cell(&states)
        );
    }
    out.push_str("\n## Current plan\n\n| Step | Description | Plan status |\n|---|---|---|\n");
    for p in &r.plan {
        let _ = writeln!(
            out,
            "| {} | {} | {} |",
            cell(&p.id),
            cell(&p.content),
            cell(&p.status)
        );
    }
    out.push_str("\n## Verification obligations\n\n| Step | Obligation | State |\n|---|---|---|\n");
    for o in &r.obligations {
        let _ = writeln!(
            out,
            "| {} | {} | {} |",
            cell(&o.plan_item_id),
            cell(&o.id),
            cell(o.state.label())
        );
    }
    out.push_str("\n## Recorded changes\n\n| Change | File | Lifecycle | Whole file comparison |\n|---|---|---|---|\n");
    for c in &r.changes {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {:?} |",
            cell(&c.id),
            cell(&c.recorded.file),
            cell(super::collect::change_label(c.lifecycle_state)),
            c.current_file_match
        );
    }
    out.push_str("\n## Workspace comparison\n\nSession linkage identifies associated records, not authorship of every hunk.\n\n| File | Attribution | Base difference | Staged | Unstaged | Untracked | Unmerged |\n|---|---|---|---|---|---|---|\n");
    for f in &r.workspace_comparison {
        let _ = writeln!(
            out,
            "| {} | {:?} | {} | {} | {} | {} | {} |",
            cell(&f.path),
            f.attribution,
            f.differs_from_base,
            f.staged,
            f.unstaged,
            f.untracked,
            f.unmerged
        );
    }
    out.push_str("\n## Command observations\n\n| Observation | Program and argv | Outcome | Exit code | Execution endpoints | Current code |\n|---|---|---|---|---|---|\n");
    for v in &r.verifications {
        let command = format!("{} {:?}", v.program, v.args);
        let _ = writeln!(
            out,
            "| {} | {} | {} | {:?} | {:?} | {:?} |",
            cell(&v.id),
            cell(&command),
            cell(&format!(
                "{} / success {}",
                v.outcome.status, v.outcome.success
            )),
            v.outcome.exit_code,
            v.execution_workspace
                .as_ref()
                .map(|s| s.run_state)
                .unwrap_or(crate::features::verification_snapshot::RunState::NotRecorded),
            v.current_code_state.state
        );
    }
    out.push_str("\n## Recorded execution context\n\n| Observation | OS family | Architecture | Primary tool | Version observation |\n|---|---|---|---|---|\n");
    for v in &r.verifications {
        if let Some(c) = &v.execution_context {
            let _ = writeln!(
                out,
                "| {} | {:?} | {:?} | {:?} | {} |",
                cell(&v.id),
                c.os_family,
                c.architecture,
                c.tool,
                cell(&format!("{:?}", c.version))
            );
        } else {
            let _ = writeln!(
                out,
                "| {} | unknown (not recorded) | unknown | unknown | unknown |",
                cell(&v.id)
            );
        }
    }
    out.push_str("\nRecorded context is partial; matching context does not prove reproducibility or correctness. No context is reconstructed at export.\n");
    for v in &r.verifications {
        let differences = &v.current_code_state.differences;
        if differences.differs() || !differences.unknown.is_empty() {
            let _ = writeln!(
                out,
                "\nVerification {} current comparison: changed {}, added {}, deleted {}, unknown {}.",
                cell(&v.id),
                differences.changed.len(),
                differences.added.len(),
                differences.deleted.len(),
                differences.unknown.len()
            );
            for (label, paths) in [
                ("changed", &differences.changed),
                ("added", &differences.added),
                ("deleted", &differences.deleted),
                ("unknown", &differences.unknown),
            ] {
                for path in paths {
                    let _ = writeln!(out, "- {}: {}", label, cell(path));
                }
            }
        }
    }
    out.push_str("\n## Limitations\n\n");
    for limitation in &r.limitations {
        let _ = writeln!(out, "- {}", cell(limitation));
    }
    // Full typed data keeps both formats semantically identical. JSON escapes
    // controls, and a variable fence protects included arbitrary code/logs.
    let longest = json.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    let _ = write!(
        out,
        "\n<details>\n<summary>Complete recorded data</summary>\n\n{fence}json\n{json}\n{fence}\n\n</details>\n"
    );
    out
}
