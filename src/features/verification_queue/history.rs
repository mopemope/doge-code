//! Restore display-only recorded observations, never execution authority.
use super::*;
use crate::{
    features::verification_context::identity::{self},
    provenance::{
        ProvenanceEvent, ProvenanceLoadResult, VerificationKind, VerificationObservedEvent,
        VerificationSource,
    },
};
use std::{
    collections::BTreeSet,
    path::{Component, Path},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMatch {
    Matches,
    Differs,
    Unknown,
}
impl InputMatch {
    fn label(self) -> &'static str {
        match self {
            Self::Matches => "bounded invocation inputs match",
            Self::Differs => "bounded invocation inputs differ",
            Self::Unknown => "invocation input correspondence unknown",
        }
    }
}
#[derive(Clone)]
pub struct HistoricalObservation {
    pub event_id: String,
    pub timestamp: String,
    pub outcome: Outcome,
    pub counts: Option<crate::features::structured_test_results::Counts>,
    pub workspace: Option<ExecutionWorkspace>,
    pub inputs: InputMatch,
    pub exit_code: Option<i32>,
}
impl HistoricalObservation {
    pub fn lines(&self, current: &Snapshot) -> Vec<String> {
        let correspondence = match self.workspace.as_ref() {
            Some(w) if w.run_state == RunState::ChangedBetweenEndpoints => {
                "changed during run; stale"
            }
            Some(w)
                if w.run_state == RunState::StableEndpoints
                    && w.end.as_ref().is_some_and(|e| w.start.same_observation(e)) =>
            {
                match verification_snapshot::compare_current(Some(w), Some(current)).state {
                    CurrentState::MatchesStart if w.start.same_observation(current) => {
                        "pre/post observations match current files"
                    }
                    CurrentState::DiffersFromStart | CurrentState::MatchesStart => {
                        "stale for current files"
                    }
                    _ => "snapshot correspondence unknown",
                }
            }
            _ => "snapshot correspondence unknown",
        };
        let mut lines = vec![format!(
            "[verify] Historical observation {} at {}: {}; exit={:?}; {}; {}. Latest saved timestamp, not a new verification or proof that no later run is missing.",
            self.event_id,
            self.timestamp,
            self.outcome.label(),
            self.exit_code,
            self.inputs.label(),
            correspondence
        )];
        if let Some(c) = self.counts {
            lines.push(format!("[verify] historical reported tests: passed={} failed={} skipped={} (recorded invocation only).",c.passed,c.failed,c.skipped));
        }
        lines
    }
}
/// None and '.' both mean the project root. Reject ambiguous/escaping spellings.
pub fn cwd(root: &Path, relative: Option<&str>) -> Option<std::path::PathBuf> {
    let root = root.canonicalize().ok()?;
    let path = Path::new(relative.unwrap_or("."));
    if path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    let cwd = root.join(path).canonicalize().ok()?;
    cwd.starts_with(&root).then_some(cwd)
}
fn normalized(relative: Option<&str>) -> Option<String> {
    let path = Path::new(relative.unwrap_or("."));
    if path.is_absolute() {
        return None;
    }
    let mut parts = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str()?),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(parts.join("/"))
}
fn observed(
    v: &VerificationObservedEvent,
) -> (
    Outcome,
    Option<crate::features::structured_test_results::Counts>,
) {
    use crate::features::structured_test_results::{ResultFormat, ResultObservation, pytest};
    if v.outcome.status == "cancelled" {
        return (Outcome::Cancelled, None);
    }
    if v.outcome.timed_out || v.outcome.status == "timed_out" {
        return (Outcome::TimedOut, None);
    }
    if v.outcome.status != "completed"
        || v.outcome.exit_code.is_none()
        || v.outcome.success != (v.outcome.exit_code == Some(0))
    {
        return (Outcome::Unknown, None);
    }
    let counts = v
        .structured_test_result
        .as_ref()
        .and_then(|s| match s.observation {
            ResultObservation::Complete { test_events, .. } => Some(test_events),
            ResultObservation::Pytest { test_events, .. } => test_events,
            _ => None,
        });
    if let Some(s) = v.structured_test_result.as_ref() {
        let valid = s.format_version == 1
            && match (&s.format, &s.observation) {
                (
                    ResultFormat::GoTestJson,
                    ResultObservation::Complete {
                        test_events,
                        packages,
                    },
                ) => !(v.outcome.success && (test_events.failed > 0 || packages.failed > 0)),
                (ResultFormat::GoTestJson, ResultObservation::Unknown { .. }) => true,
                (
                    ResultFormat::PytestJunit,
                    ResultObservation::Pytest {
                        outcome,
                        test_events,
                    },
                ) => match (outcome, test_events) {
                    (pytest::Outcome::Passed, Some(c)) => {
                        v.outcome.exit_code == Some(0) && c.passed > 0 && c.failed == 0
                    }
                    (pytest::Outcome::Failed, Some(c)) => {
                        v.outcome.exit_code == Some(1) && c.failed > 0
                    }
                    (pytest::Outcome::ZeroTests, Some(c)) => {
                        v.outcome.exit_code == Some(5) && c.total() == 0
                    }
                    (pytest::Outcome::AllSkipped, Some(c)) => {
                        v.outcome.exit_code == Some(0)
                            && c.passed == 0
                            && c.failed == 0
                            && c.skipped > 0
                    }
                    (pytest::Outcome::CollectionError, None) => v.outcome.exit_code == Some(2),
                    (pytest::Outcome::Interrupted, None) => v.outcome.exit_code == Some(2),
                    (pytest::Outcome::RunnerError, None) => {
                        matches!(v.outcome.exit_code, Some(3 | 4 | 6))
                    }
                    (pytest::Outcome::Incomplete | pytest::Outcome::Unknown, None) => true,
                    _ => false,
                },
                _ => false,
            };
        if !valid {
            return (Outcome::Unknown, None);
        }
    }
    let result = TestResult {
        structured_test_result: v.structured_test_result.clone(),
        diagnostic_stdout: None,
        execution_observed: true,
        command: String::new(),
        stdout: String::new(),
        stderr: String::new(),
        success: v.outcome.success,
        exit_code: v.outcome.exit_code,
        failed_tests: vec![],
        timed_out: false,
        output_truncated: v.structured_test_result.is_none() && v.output_truncated,
        warnings: vec![],
        cancelled: false,
    };
    (classify(&result), counts)
}
impl VerificationQueue {
    /// Current queue results always win; restoration is confined to new rows.
    pub fn restore_history(&mut self, loaded: &ProvenanceLoadResult, root: &Path) {
        for entry in &mut self.entries {
            if entry.outcome == Outcome::Unexecuted {
                entry.history = None;
            }
        }
        let Some(session) = self.session_id.as_deref() else {
            return;
        };
        let mut ids = BTreeSet::new();
        let invalid = !loaded.warnings.is_empty()
            || loaded.events.len() > 4096
            || loaded.events.iter().any(|e| {
                e.session_id != session
                    || e.event_id.is_empty()
                    || !ids.insert(&e.event_id)
                    || chrono::DateTime::parse_from_rfc3339(&e.timestamp).is_err()
            });
        self.history_warning=invalid.then(||"Recorded history is incomplete, duplicated or unordered; no historical results restored.".into());
        if invalid {
            return;
        }
        for entry in &mut self.entries {
            if entry.outcome != Outcome::Unexecuted {
                continue;
            }
            let r = &entry.recommendation;
            let Some(expected_cwd) = normalized(r.cwd.as_deref()) else {
                continue;
            };
            let mut matching = loaded
                .events
                .iter()
                .filter_map(|e| match &e.event {
                    ProvenanceEvent::VerificationObserved(v)
                        if v.verification_kind == VerificationKind::Test
                            && matches!(
                                v.source,
                                VerificationSource::TuiTest | VerificationSource::ExecuteProcess
                            )
                            && v.command.program == r.program
                            && v.command.args == r.args
                            && normalized(v.command.cwd.as_deref()).as_ref()
                                == Some(&expected_cwd) =>
                    {
                        Some((e, v))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            matching.sort_by_key(|(e, _)| chrono::DateTime::parse_from_rfc3339(&e.timestamp).ok());
            let Some((latest, v)) = matching.last().copied() else {
                continue;
            };
            let tied = matching.len() > 1
                && chrono::DateTime::parse_from_rfc3339(&matching[matching.len() - 2].0.timestamp)
                    .ok()
                    == chrono::DateTime::parse_from_rfc3339(&latest.timestamp).ok();
            let current = cwd(root, r.cwd.as_deref())
                .and_then(|cwd| identity::capture(root, &cwd, &r.program));
            let inputs = match (v.execution_identity.as_ref(), current.as_ref()) {
                (Some(old), Some(new)) if old.known() && new.known() => {
                    if old == new {
                        InputMatch::Matches
                    } else {
                        InputMatch::Differs
                    }
                }
                _ => InputMatch::Unknown,
            };
            let (outcome, counts) = if tied {
                (Outcome::Unknown, None)
            } else {
                observed(v)
            };
            entry.history = Some(HistoricalObservation {
                event_id: latest.event_id.clone(),
                timestamp: latest.timestamp.clone(),
                outcome,
                counts,
                workspace: v.execution_workspace.as_deref().cloned(),
                inputs,
                exit_code: v.outcome.exit_code,
            });
        }
    }
}
/// Bound the existing store reader before loading; errors never revive earlier success.
pub fn load(tools: &crate::tools::FsTools) -> anyhow::Result<Option<ProvenanceLoadResult>> {
    let Some(context) = tools.current_session_storage_context() else {
        return Ok(None);
    };
    Ok(Some(
        crate::provenance::ProvenanceStore::new(context.session_dir).load_for_history()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        features::{
            structured_test_results::{
                Counts, ResultFormat, ResultObservation, StructuredTestResult,
            },
            verification_snapshot::{CaptureStatus, FileEntry, FileKind},
        },
        provenance::{ProvenanceEventEnvelope, VerificationContext, VerificationRecordInput},
    };
    fn snapshot() -> Snapshot {
        let mut s = Snapshot::unavailable(verification_snapshot::Diagnostic::GitUnavailable);
        s.status = CaptureStatus::Complete;
        s.diagnostics.clear();
        s.head_oid = Some("head".into());
        s.project_relative_to_git_root = Some(".".into());
        s.files = vec![FileEntry {
            path: "file.rs".into(),
            kind: FileKind::Regular,
            exists: Some(true),
            byte_len: Some(1),
            content_hash: Some(format!("blake3:{}", "a".repeat(64))),
            executable_bits: Some(0),
        }];
        s
    }
    fn recommendation() -> crate::features::change_impact::VerificationRecommendation {
        crate::features::change_impact::VerificationRecommendation {
            kind: "test".into(),
            program: "printf".into(),
            args: vec!["literal".into()],
            cwd: None,
            reason: "current suggestion".into(),
            confidence: "unknown".into(),
            coverage_scope: "file".into(),
        }
    }
    fn queue() -> VerificationQueue {
        let mut q = VerificationQueue::default();
        q.refresh(Some("s".into()), snapshot(), vec![recommendation()], None);
        q
    }
    fn event(root: &Path, id: &str, second: u32) -> ProvenanceEventEnvelope {
        let s = snapshot();
        let v =
            crate::provenance::verification::build_verification_event(VerificationRecordInput {
                kind: VerificationKind::Test,
                source: VerificationSource::TuiTest,
                program: "printf",
                args: &["literal".into()],
                cwd_relative: None,
                success: true,
                status: "completed",
                exit_code: Some(0),
                timed_out: false,
                stdout: "",
                stderr: "",
                capture_truncated: false,
                extra_warnings: vec![],
                context: VerificationContext {
                    execution_identity: identity::capture(root, root, "printf"),
                    execution_workspace: Some(ExecutionWorkspace {
                        version: 1,
                        start: s.clone(),
                        end: Some(s),
                        run_state: RunState::StableEndpoints,
                        differences: Default::default(),
                    }),
                    ..Default::default()
                },
                structured_test_result: Some(Box::new(StructuredTestResult {
                    format_version: 1,
                    format: ResultFormat::GoTestJson,
                    observation: ResultObservation::Complete {
                        test_events: Counts {
                            passed: 1,
                            ..Default::default()
                        },
                        packages: Counts {
                            passed: 1,
                            ..Default::default()
                        },
                    },
                })),
            });
        ProvenanceEventEnvelope {
            schema_version: 8,
            event_id: id.into(),
            session_id: "s".into(),
            timestamp: format!("2026-10-11T01:00:{second:02}Z"),
            event: ProvenanceEvent::VerificationObserved(v),
        }
    }
    fn observation(e: &mut ProvenanceEventEnvelope) -> &mut VerificationObservedEvent {
        match &mut e.event {
            ProvenanceEvent::VerificationObserved(v) => v,
            _ => panic!("observation"),
        }
    }
    #[test]
    fn history_restores_latest_recorded_failure_and_keeps_current_queue_unexecuted() {
        let root = tempfile::tempdir().expect("root");
        let pass = event(root.path(), "first", 1);
        let mut failed = event(root.path(), "last", 2);
        let v = observation(&mut failed);
        v.outcome.success = false;
        v.outcome.exit_code = Some(1);
        v.structured_test_result = None;
        let mut q = queue();
        let old = q.selection_id(0).expect("ID");
        q.restore_history(
            &ProvenanceLoadResult {
                events: vec![failed, pass],
                warnings: vec![],
            },
            root.path(),
        );
        let h = q.entries[0].history.as_ref().expect("restored");
        assert_eq!(h.outcome, Outcome::Failed);
        assert_eq!(h.event_id, "last");
        assert_eq!(q.entries[0].outcome, Outcome::Unexecuted);
        assert_eq!(q.selection_id(0).as_deref(), Some(old.as_str()));
        assert!(
            q.lines(&snapshot())
                .join("\n")
                .contains("not a new verification")
        );
    }
    #[test]
    fn history_identity_unknown_changed_cwd_and_program_never_infer_current_success() {
        let root = tempfile::tempdir().expect("root");
        let base = event(root.path(), "one", 1);
        for (mode, expected, present) in [
            (0, InputMatch::Matches, true),
            (1, InputMatch::Unknown, true),
            (2, InputMatch::Differs, true),
            (3, InputMatch::Unknown, false),
            (4, InputMatch::Unknown, false),
            (5, InputMatch::Unknown, false),
        ] {
            let mut e = base.clone();
            let v = observation(&mut e);
            match mode {
                1 => v.execution_identity = None,
                2 => {
                    v.execution_identity = Some(identity::ExecutionIdentity {
                        version: 1,
                        digest: "b".repeat(64),
                    })
                }
                3 => v.command.cwd = Some("different".into()),
                4 => v.command.program = "other".into(),
                5 => v.command.args.push("different".into()),
                _ => {}
            }
            let mut q = queue();
            q.restore_history(
                &ProvenanceLoadResult {
                    events: vec![e],
                    warnings: vec![],
                },
                root.path(),
            );
            assert_eq!(q.entries[0].history.is_some(), present);
            if present {
                assert_eq!(
                    q.entries[0].history.as_ref().expect("history").inputs,
                    expected
                );
            }
            assert_eq!(q.entries[0].outcome, Outcome::Unexecuted);
            assert!(q.entries[0].test_counts.is_none());
        }
    }
    #[test]
    fn history_corrupt_foreign_duplicate_invalid_and_tied_order_never_revive_success() {
        let root = tempfile::tempdir().expect("root");
        let first = event(root.path(), "one", 1);
        for mode in 0..5 {
            let mut latest = event(root.path(), "two", 2);
            let mut loaded = ProvenanceLoadResult {
                events: vec![first.clone()],
                warnings: vec![],
            };
            match mode {
                0 => loaded
                    .warnings
                    .push("missing or corrupt later record".into()),
                1 => latest.session_id = "other".into(),
                2 => latest.event_id = "one".into(),
                3 => latest.timestamp = "unordered".into(),
                4 => latest.timestamp = first.timestamp.clone(),
                _ => {}
            }
            loaded.events.push(latest);
            let mut q = queue();
            q.restore_history(
                &ProvenanceLoadResult {
                    events: vec![first.clone()],
                    warnings: vec![],
                },
                root.path(),
            );
            q.restore_history(&loaded, root.path());
            assert!(
                q.entries[0]
                    .history
                    .as_ref()
                    .is_none_or(|h| h.outcome == Outcome::Unknown)
            );
            assert_eq!(q.entries[0].outcome, Outcome::Unexecuted);
        }
    }
    #[test]
    fn history_snapshot_stale_unknown_and_changed_during_run_remain_distinct() {
        let root = tempfile::tempdir().expect("root");
        for mode in 0..4 {
            let mut e = event(root.path(), "one", 1);
            let mut current = snapshot();
            match mode {
                1 => current.files[0].content_hash = Some(format!("blake3:{}", "b".repeat(64))),
                2 => {
                    observation(&mut e)
                        .execution_workspace
                        .as_mut()
                        .expect("workspace")
                        .run_state = RunState::ChangedBetweenEndpoints
                }
                3 => observation(&mut e).execution_workspace = None,
                _ => {}
            }
            let mut q = queue();
            q.restore_history(
                &ProvenanceLoadResult {
                    events: vec![e],
                    warnings: vec![],
                },
                root.path(),
            );
            let lines = q.lines(&current).join("\n");
            assert!(
                lines.contains(match mode {
                    0 => "pre/post observations match",
                    1 => "stale for current files",
                    2 => "changed during run; stale",
                    _ => "snapshot correspondence unknown",
                }),
                "{lines}"
            );
        }
    }
    #[test]
    fn history_cancel_incomplete_and_semantically_broken_reports_do_not_pass() {
        let root = tempfile::tempdir().expect("root");
        for (mode, want) in [
            (0, Outcome::Cancelled),
            (1, Outcome::Incomplete),
            (2, Outcome::Unknown),
            (3, Outcome::TimedOut),
            (4, Outcome::Unknown),
        ] {
            let mut e = event(root.path(), "one", 1);
            let v = observation(&mut e);
            match mode {0=>{v.outcome.status="cancelled".into();v.outcome.success=false;v.structured_test_result=None},1=>{v.structured_test_result=Some(Box::new(StructuredTestResult {format_version:1,format:ResultFormat::GoTestJson,observation:ResultObservation::Unknown {reason:crate::features::structured_test_results::UnknownReason::CaptureIncomplete}}))},2=>v.outcome.exit_code=Some(1),3=>{v.outcome.timed_out=true;v.outcome.success=false},4=>{v.structured_test_result.as_mut().expect("report").format=ResultFormat::PytestJunit},_=>{}}
            let mut q = queue();
            q.restore_history(
                &ProvenanceLoadResult {
                    events: vec![e],
                    warnings: vec![],
                },
                root.path(),
            );
            let h = q.entries[0].history.as_ref().expect("history");
            assert_eq!(h.outcome, want);
            assert!(h.counts.is_none());
        }
    }
    #[test]
    fn history_missing_and_live_cancelled_rows_are_not_replaced_by_saved_pass() {
        let root = tempfile::tempdir().expect("root");
        let mut q = queue();
        q.restore_history(&ProvenanceLoadResult::default(), root.path());
        assert!(q.entries[0].history.is_none());
        q.entries[0].outcome = Outcome::Cancelled;
        q.restore_history(
            &ProvenanceLoadResult {
                events: vec![event(root.path(), "one", 1)],
                warnings: vec![],
            },
            root.path(),
        );
        assert_eq!(q.entries[0].outcome, Outcome::Cancelled);
        assert!(q.entries[0].history.is_none());
    }
    #[test]
    fn history_pytest_error_outcomes_require_producer_count_contract() {
        use crate::features::structured_test_results::pytest;
        let root = tempfile::tempdir().expect("root");
        for (outcome, code, expected) in [
            (
                pytest::Outcome::CollectionError,
                2,
                Outcome::CollectionError,
            ),
            (pytest::Outcome::Interrupted, 2, Outcome::Interrupted),
            (pytest::Outcome::RunnerError, 3, Outcome::RunnerError),
            (pytest::Outcome::RunnerError, 4, Outcome::RunnerError),
            (pytest::Outcome::RunnerError, 6, Outcome::RunnerError),
        ] {
            for counts in [
                None,
                Some(Counts {
                    failed: 1,
                    ..Default::default()
                }),
            ] {
                let mut e = event(root.path(), "error", 1);
                let v = observation(&mut e);
                v.outcome.success = false;
                v.outcome.exit_code = Some(code);
                v.structured_test_result = Some(Box::new(StructuredTestResult {
                    format_version: 1,
                    format: ResultFormat::PytestJunit,
                    observation: ResultObservation::Pytest {
                        outcome,
                        test_events: counts,
                    },
                }));
                let (actual, actual_counts) = observed(v);
                assert_eq!(
                    actual,
                    if counts.is_none() {
                        expected
                    } else {
                        Outcome::Unknown
                    }
                );
                assert!(actual_counts.is_none());
            }
        }
    }
}
