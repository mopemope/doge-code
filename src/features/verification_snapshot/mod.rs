//! Bounded, read-only endpoint observations. Not an atomic execution snapshot.
mod capture;
mod model;
pub use model::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tokio_util::sync::CancellationToken;

pub const MAX_PATHS: usize = 10_000;
pub const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_INPUT_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_JSON_BYTES: usize = 4 * 1024 * 1024;

pub(crate) use capture::open_relative;

pub async fn capture(
    root: &Path,
    extra: BTreeSet<String>,
    cancel: Option<CancellationToken>,
) -> Snapshot {
    capture::capture(root, extra, cancel).await
}

pub async fn begin(
    root: &Path,
    references: BTreeSet<String>,
    cancel: Option<CancellationToken>,
) -> ExecutionWorkspace {
    ExecutionWorkspace {
        version: 1,
        start: capture(root, references, cancel).await,
        end: None,
        run_state: RunState::Indeterminate,
        differences: Differences::default(),
    }
}
pub async fn finish(
    root: &Path,
    mut record: ExecutionWorkspace,
    cancel: Option<CancellationToken>,
) -> ExecutionWorkspace {
    let paths = record.start.files.iter().map(|f| f.path.clone()).collect();
    let end = capture(root, paths, cancel).await;
    record.differences = differences(&record.start, &end);
    record.run_state = if record.differences.differs() {
        RunState::ChangedBetweenEndpoints
    } else if record.differences.unknown.is_empty() && comparable(&record.start, &end) {
        RunState::StableEndpoints
    } else {
        RunState::Indeterminate
    };
    record.end = Some(end);
    record
}
fn comparable(a: &Snapshot, b: &Snapshot) -> bool {
    a.version == 1
        && b.version == 1
        && a.status == CaptureStatus::Complete
        && b.status == CaptureStatus::Complete
        && !a.files.is_empty()
        && !b.files.is_empty()
        && a.diagnostics.is_empty()
        && b.diagnostics.is_empty()
        && a.files.iter().all(FileEntry::known)
        && b.files.iter().all(FileEntry::known)
        && a.files
            .iter()
            .map(|f| &f.path)
            .collect::<BTreeSet<_>>()
            .len()
            == a.files.len()
        && b.files
            .iter()
            .map(|f| &f.path)
            .collect::<BTreeSet<_>>()
            .len()
            == b.files.len()
}
pub fn compare_current(
    record: Option<&ExecutionWorkspace>,
    current: Option<&Snapshot>,
) -> CurrentComparison {
    let Some(record) = record else {
        return CurrentComparison {
            state: CurrentState::NotRecorded,
            differences: Differences::default(),
        };
    };
    let Some(current) = current else {
        return CurrentComparison {
            state: CurrentState::Indeterminate,
            differences: Differences::default(),
        };
    };
    if record.version != 1 || record.start.version != 1 || current.version != 1 {
        return CurrentComparison {
            state: CurrentState::Indeterminate,
            differences: Differences::default(),
        };
    }
    let differences = differences(&record.start, current);
    let state = if differences.differs() {
        CurrentState::DiffersFromStart
    } else if differences.unknown.is_empty() && comparable(&record.start, current) {
        CurrentState::MatchesStart
    } else {
        CurrentState::Indeterminate
    };
    CurrentComparison { state, differences }
}
pub fn differences(a: &Snapshot, b: &Snapshot) -> Differences {
    let before: BTreeMap<_, _> = a.files.iter().map(|f| (f.path.as_str(), f)).collect();
    let after: BTreeMap<_, _> = b.files.iter().map(|f| (f.path.as_str(), f)).collect();
    let mut out = Differences::default();
    for path in before
        .keys()
        .chain(after.keys())
        .copied()
        .collect::<BTreeSet<_>>()
    {
        match (before.get(path), after.get(path)) {
            (Some(a), Some(b)) if a.known() && b.known() => {
                if a.exists == Some(false) && b.exists == Some(true) {
                    out.added.push(path.into());
                } else if a.exists == Some(true) && b.exists == Some(false) {
                    out.deleted.push(path.into());
                } else if a != b {
                    out.changed.push(path.into());
                }
            }
            (None, Some(b))
                if b.known() && b.exists == Some(true) && a.status == CaptureStatus::Complete =>
            {
                out.added.push(path.into())
            }
            (Some(a), None)
                if a.known() && a.exists == Some(true) && b.status == CaptureStatus::Complete =>
            {
                out.deleted.push(path.into())
            }
            _ => out.unknown.push(path.into()),
        }
    }
    out
}
/// Compact, source-budgeted output: no file arrays or user-controlled diagnostics.
pub fn summary(record: &ExecutionWorkspace) -> serde_json::Value {
    serde_json::json!({"run_state": record.run_state, "start_status": record.start.status,
        "end_status": record.end.as_ref().map(|s| s.status),
        "changed": record.differences.changed.len(), "added": record.differences.added.len(),
        "deleted": record.differences.deleted.len(), "unknown": record.differences.unknown.len()})
}
pub fn warning(record: &ExecutionWorkspace) -> Option<&'static str> {
    match record.run_state {
        RunState::StableEndpoints => None,
        RunState::ChangedBetweenEndpoints => Some(
            "Verification workspace changed between pre/post observations; command outcome is preserved, but it does not establish verification of one code state.",
        ),
        _ => Some(
            "Verification workspace could not be fully observed; command outcome is preserved, code-state correspondence is indeterminate.",
        ),
    }
}
#[cfg(test)]
mod tests;
