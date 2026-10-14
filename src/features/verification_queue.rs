//! Session-local suggestions and observed outcomes. Never marks a plan complete.
pub mod history;
use crate::features::{
    change_impact::VerificationRecommendation,
    structured_test_results::ResultObservation,
    testing::TestResult,
    verification_snapshot::{self, CurrentState, ExecutionWorkspace, RunState, Snapshot},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Unexecuted,
    Running,
    Passed,
    Failed,
    ZeroTests,
    Skipped,
    Incomplete,
    Unknown,
    TimedOut,
    Cancelled,
    SpawnFailed,
    CollectionError,
    Interrupted,
    RunnerError,
}
impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unexecuted => "unexecuted",
            Self::Running => "running",
            Self::Passed => "reported tests passed",
            Self::Failed => "failed",
            Self::ZeroTests => "zero tests",
            Self::Skipped => "all reported tests skipped",
            Self::Incomplete => "incomplete",
            Self::Unknown => "test counts unknown",
            Self::TimedOut => "timed out",
            Self::Cancelled => "cancelled",
            Self::SpawnFailed => "could not start",
            Self::CollectionError => "collection error",
            Self::Interrupted => "interrupted",
            Self::RunnerError => "test runner error",
        }
    }
}
pub fn classify(result: &TestResult) -> Outcome {
    if result.cancelled {
        return Outcome::Cancelled;
    }
    if result.timed_out {
        return Outcome::TimedOut;
    }
    if !result.execution_observed {
        return Outcome::SpawnFailed;
    }
    classify_reported_tests(
        result.structured_test_result.as_deref(),
        result.success,
        result.output_truncated || !result.warnings.is_empty(),
    )
}

/// Shared interpretation for queue and model feedback; process success alone
/// never establishes that tests ran.
pub(crate) fn classify_reported_tests(
    structured: Option<&crate::features::structured_test_results::StructuredTestResult>,
    success: bool,
    incomplete: bool,
) -> Outcome {
    if let Some(outcome) =
        structured.and_then(crate::features::structured_test_results::pytest::outcome)
    {
        use crate::features::structured_test_results::pytest::Outcome as Python;
        return match outcome {
            Python::Passed => Outcome::Passed,
            Python::Failed => Outcome::Failed,
            Python::ZeroTests => Outcome::ZeroTests,
            Python::AllSkipped => Outcome::Skipped,
            Python::CollectionError => Outcome::CollectionError,
            Python::Interrupted => Outcome::Interrupted,
            Python::RunnerError => Outcome::RunnerError,
            Python::Incomplete => Outcome::Incomplete,
            Python::Unknown => Outcome::Unknown,
        };
    }
    if !success {
        return Outcome::Failed;
    }
    if incomplete {
        return Outcome::Incomplete;
    }
    match structured.map(|s| &s.observation) {
        Some(ResultObservation::Complete { test_events, .. }) if test_events.failed > 0 => {
            Outcome::Failed
        }
        Some(ResultObservation::Complete { test_events, .. }) if test_events.total() == 0 => {
            Outcome::ZeroTests
        }
        Some(ResultObservation::Complete { test_events, .. }) if test_events.passed == 0 => {
            Outcome::Skipped
        }
        Some(ResultObservation::Complete { .. }) => Outcome::Passed,
        Some(ResultObservation::Unknown { .. }) => Outcome::Incomplete,
        Some(ResultObservation::Pytest { .. }) | None => Outcome::Unknown,
    }
}
#[derive(Clone)]
pub struct Entry {
    pub failure: Option<crate::features::verification_failure::Observation>,
    pub history: Option<history::HistoricalObservation>,
    pub recommendation: VerificationRecommendation,
    pub outcome: Outcome,
    pub workspace: Option<ExecutionWorkspace>,
    pub test_counts: Option<crate::features::structured_test_results::Counts>,
    pub exit_code: Option<i32>,
}
#[derive(Default)]
pub struct VerificationQueue {
    pub history_warning: Option<String>,
    pub session_id: Option<String>,
    generation: String,
    pub planned: Option<Snapshot>,
    pub entries: Vec<Entry>,
    pub paths: Option<Vec<String>>,
}
impl VerificationQueue {
    pub fn invalidate_selection(&mut self) {
        self.generation = uuid::Uuid::new_v4().simple().to_string();
    }
    pub fn refresh(
        &mut self,
        session_id: Option<String>,
        planned: Snapshot,
        recommendations: Vec<VerificationRecommendation>,
        paths: Option<Vec<String>>,
    ) {
        self.invalidate_selection();
        self.history_warning = None;
        let old = if self.session_id == session_id {
            std::mem::take(&mut self.entries)
        } else {
            vec![]
        };
        self.entries = recommendations
            .into_iter()
            .map(|recommendation| {
                let previous = old.iter().find(|e| {
                    e.recommendation.program == recommendation.program
                        && e.recommendation.args == recommendation.args
                        && e.recommendation.cwd == recommendation.cwd
                });
                match previous {
                    Some(e) => Entry {
                        recommendation,
                        ..e.clone()
                    },
                    None => Entry {
                        failure: None,
                        history: None,
                        recommendation,
                        outcome: Outcome::Unexecuted,
                        workspace: None,
                        test_counts: None,
                        exit_code: None,
                    },
                }
            })
            .collect();
        self.session_id = session_id;
        self.planned = Some(planned);
        self.paths = paths;
    }
    /// Opaque selection authority, valid only for this displayed queue generation.
    /// Keep the row index as a discriminator so duplicate commands retain all
    /// reasons/scopes, rather than collapsing one recommendation into another.
    pub fn selection_id(&self, index: usize) -> Option<String> {
        let recommendation = &self.entries.get(index)?.recommendation;
        let bytes = serde_json::to_vec(recommendation).ok()?;
        let digest = blake3::hash(&bytes).to_hex();
        Some(format!(
            "q{}-{}-{}",
            self.generation,
            index + 1,
            &digest[..16]
        ))
    }
    pub fn selection(
        &self,
        id: &str,
        session_id: &Option<String>,
        current: &Snapshot,
    ) -> anyhow::Result<(usize, Entry)> {
        anyhow::ensure!(
            &self.session_id == session_id,
            "Session changed; use /verify refresh."
        );
        let index = (0..self.entries.len()).find(|&i| self.selection_id(i).as_deref() == Some(id))
            .ok_or_else(|| anyhow::anyhow!("Stale or unknown selection ID; use /verify and copy the displayed /verify run command."))?;
        let planned = self
            .planned
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Use /verify first."))?;
        anyhow::ensure!(
            matches_current(planned, current),
            "Workspace changed or snapshot is incomplete; use /verify refresh before selecting a command."
        );
        Ok((index, self.entries[index].clone()))
    }
    pub fn lines(&self, current: &Snapshot) -> Vec<String> {
        let mut lines = vec!["[verify] Suggestions are not verification. Select one with /verify run <id>; /verify refresh [relative paths...] rebuilds suggestions.".into()];
        if let Some(warning) = &self.history_warning {
            lines.push(format!("[verify] {warning}"));
        }
        if self.entries.is_empty() {
            lines.push("[verify] No candidates (no active changes or supported test configuration); nothing was executed.".into());
        }
        for (i, entry) in self.entries.iter().enumerate() {
            let correspondence = match entry.workspace.as_ref() {
                Some(record) if record.run_state == RunState::ChangedBetweenEndpoints => {
                    "changed during run; stale"
                }
                Some(record) if record.run_state != RunState::StableEndpoints => {
                    "snapshot correspondence unknown"
                }
                Some(record) => {
                    match verification_snapshot::compare_current(Some(record), Some(current)).state
                    {
                        CurrentState::MatchesStart if record.start.same_observation(current) => {
                            "pre/post observations match current files"
                        }
                        CurrentState::MatchesStart => "stale for current changes",
                        CurrentState::DiffersFromStart => "stale for current changes",
                        _ => "snapshot correspondence unknown",
                    }
                }
                None if self.planned.as_ref().is_some_and(|s| {
                    s.status != verification_snapshot::CaptureStatus::Complete
                }) =>
                {
                    "candidate snapshot unknown; refresh required"
                }
                None if self
                    .planned
                    .as_ref()
                    .is_some_and(|s| !matches_current(s, current)) =>
                {
                    "candidate snapshot stale; refresh required"
                }
                None => "no execution evidence",
            };
            if let Some(history) = &entry.history {
                lines.extend(history.lines(current));
            }
            let r = &entry.recommendation;
            lines.push(format!(
                "[verify] {}. {} {:?} cwd={:?} | {} | {} | scope={} confidence={}; {}",
                i + 1,
                r.program,
                r.args,
                r.cwd.as_deref().unwrap_or("."),
                entry.outcome.label(),
                correspondence,
                r.coverage_scope,
                r.confidence,
                r.reason
            ));
            if let Some(id) = self.selection_id(i) {
                lines.push(format!("[verify] Select: /verify run {id}"));
                if entry.failure.is_some() {
                    lines.push(format!(
                        "[verify] Stage failure on selected live review hunk: /verify feedback {id}"
                    ));
                }
            }
            if let Some(counts) = entry.test_counts {
                lines.push(format!("[verify] reported tests: passed={} failed={} skipped={} (only the selected command's scope)", counts.passed, counts.failed, counts.skipped));
            } else {
                lines.push(format!("[verify] test counts unknown; process exit={:?}. A local command does not establish whole-project correctness.", entry.exit_code));
            }
        }
        lines
    }
}
/// A complete end observation must never upgrade unknown planning into authority.
pub fn bind_planning_snapshot(before: &Snapshot, mut after: Snapshot) -> anyhow::Result<Snapshot> {
    use verification_snapshot::CaptureStatus;
    if before.status == CaptureStatus::Complete && after.status == CaptureStatus::Complete {
        anyhow::ensure!(
            !verification_snapshot::differences(before, &after).differs()
                && before.same_observation(&after),
            "Workspace changed while planning; no candidates published. Retry /verify refresh."
        );
    }
    if !matches_current(before, &after) {
        after.status = CaptureStatus::Partial;
        after
            .diagnostics
            .push(verification_snapshot::Diagnostic::FileUnavailable);
    }
    Ok(after)
}

pub fn matches_current(planned: &Snapshot, current: &Snapshot) -> bool {
    let record = ExecutionWorkspace {
        version: 1,
        start: planned.clone(),
        end: None,
        run_state: RunState::Indeterminate,
        differences: Default::default(),
    };
    verification_snapshot::compare_current(Some(&record), Some(current)).state
        == CurrentState::MatchesStart
        && planned.same_observation(current)
}

#[cfg(test)]
mod tests;
