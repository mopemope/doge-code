//! Read-only recovery diagnosis. Session ownership/transitions remain in session.
//! No provider calls, retry, history repair or evidence reclassification.
use crate::{
    provenance::ProvenanceStore,
    session::{SessionData, SessionStore},
};
use anyhow::Result;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryRoute {
    InspectThenResume,
    FreshSessionRequired,
}

#[derive(Debug, serde::Serialize)]
pub struct RecoveryReport {
    pub schema_version: u32,
    pub session_id: String,
    pub view: &'static str,
    pub checkpoint_unsaved: bool,
    pub recorded_write_gap: bool,
    pub recorded_write_failures: u64,
    pub provenance_status: &'static str,
    pub conversation_status: &'static str,
    pub pending_tail_calls: Option<usize>,
    pub route: RecoveryRoute,
    pub archive_source: String,
}

/// Saved metadata, not a lease transition or a claim about a running process.
pub fn inspect(project_root: &Path, prefix: &str) -> Result<RecoveryReport> {
    let store = SessionStore::open_existing(project_root.join(".doge/sessions"))?;
    let unavailable = || {
        anyhow::anyhow!(
            "Selected checkpoint unavailable. Preserve its session directory outside the managed store and inspect the workspace before starting fresh. No history was repaired or replayed."
        )
    };
    let data = match store.load(prefix) {
        Ok(data) => data,
        Err(crate::session::error::SessionError::NotFound(_)) => {
            let id = store.resolve_id_prefix(prefix).map_err(|_| unavailable())?;
            store.load(&id).map_err(|_| unavailable())?
        }
        Err(_) => return Err(unavailable()),
    };
    let id = data.meta.id.clone();
    Ok(diagnose(
        &data,
        &store.session_dir(&id),
        false,
        "saved_checkpoint",
    ))
}

pub(crate) fn diagnose(
    data: &SessionData,
    session_dir: &Path,
    unsaved: bool,
    view: &'static str,
) -> RecoveryReport {
    let provenance_status = if data.provenance_incomplete {
        "recorded_write_gap"
    } else {
        match ProvenanceStore::new(session_dir.to_path_buf()).load_complete(&data.meta.id) {
            Ok(loaded)
                if crate::provenance::requirements::current_requirements(&loaded.events)
                    .warnings
                    .is_empty() =>
            {
                "readable_history"
            }
            Ok(_) => "invalid_requirement_history",
            Err(_) => "history_unavailable",
        }
    };
    let conversation = data.conversation_messages().and_then(|messages| {
        crate::llm::validate_tool_blocks(&crate::llm::durable_conversation_messages(messages), true)
    });
    let (conversation_status, pending_tail_calls) = match conversation {
        Ok(pending) if pending.is_empty() => ("protocol_valid", Some(0)),
        Ok(pending) => ("pending_tail", Some(pending.len())),
        Err(_) => ("unavailable", None),
    };
    let route =
        if provenance_status == "readable_history" && conversation_status == "protocol_valid" {
            RecoveryRoute::InspectThenResume
        } else {
            RecoveryRoute::FreshSessionRequired
        };
    RecoveryReport {
        schema_version: 1,
        session_id: data.meta.id.clone(),
        view,
        checkpoint_unsaved: unsaved,
        recorded_write_gap: data.provenance_incomplete,
        recorded_write_failures: data.provenance_record_failures,
        provenance_status,
        conversation_status,
        pending_tail_calls,
        route,
        archive_source: session_dir.to_string_lossy().into_owned(),
    }
}

pub fn render(report: &RecoveryReport) -> String {
    let mut text = format!(
        "Recovery diagnosis ({})\nSession: {:?}\nCheckpoint unsaved: {}\nProvenance: {} (recorded write failures: {})\nConversation: {}\nReadability does not establish prior tool effects or current verification.\nInspect actual files, tracked/untracked changes and failed/unknown tool outcomes before retrying anything.\n",
        report.view,
        report.session_id,
        report.checkpoint_unsaved,
        report.provenance_status,
        report.recorded_write_failures,
        report.conversation_status
    );
    if report.checkpoint_unsaved {
        text.push_str("First retry /session save. If it fails, /session export preserves the in-memory checkpoint; the export is not resumable and does not clear dirty state. Resolve the save/capacity cause before transitioning.\n");
    }
    match report.route {
        RecoveryRoute::InspectThenResume => text.push_str(
            "After inspection, explicitly resume with the original provider/model/account: dgc exec --resume=<id> <new-instruction> (CLI) or /session switch <id> (TUI). Use the displayed session ID as data, with appropriate shell quoting. No tool is replayed by this diagnosis.\n"
        ),
        RecoveryRoute::FreshSessionRequired => text.push_str(&format!(
            "Current state authority is unavailable; resume, a passing test or /session clear cannot repair the gap.\nPreserve the session directory {:?} outside the managed session store before starting fresh; normal new/save retention may remove old sessions. Keep its checkpoint and event bytes unchanged.\nThen explicitly start /session new (TUI) or dgc exec <new-instruction> without --resume (CLI), in the same inspected workspace. Do not copy old history/flags/verification IDs or clear the failure flag.\nSubmit the intended task again and run /verify refresh <explicit-paths> in TUI, or an explicit checker in CLI. A fresh session may have no tracked changes; rerun checks as new evidence, retaining old outcomes.\n", report.archive_source
        )),
    }
    text
}

#[cfg(test)]
mod tests;
