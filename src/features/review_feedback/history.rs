//! Bounded process-local snapshots. Browsing cannot restore submission authority.
use super::{FeedbackBatch, FeedbackOutcome};
use serde::Serialize;
use std::collections::VecDeque;

pub const MAX_HISTORY_BATCHES: usize = 8;
pub const MAX_HISTORY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct ArchivedFeedback {
    pub batch: FeedbackBatch,
    pub job_id: Option<crate::jobs::JobId>,
    pub submitted_revision: Option<u64>,
    pub repair_review_id: Option<String>,
    pub outcome: Option<String>,
    pub stale: bool,
}

#[derive(Debug)]
pub struct FeedbackHistory {
    entries: VecDeque<(ArchivedFeedback, usize)>,
    bytes: usize,
    max_batches: usize,
    max_bytes: usize,
}
impl Default for FeedbackHistory {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            max_batches: MAX_HISTORY_BATCHES,
            max_bytes: MAX_HISTORY_BYTES,
        }
    }
}
impl FeedbackHistory {
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn get(&self, index: usize) -> Option<&ArchivedFeedback> {
        self.entries.get(index).map(|(entry, _)| entry)
    }
    pub fn archive(&mut self, entry: ArchivedFeedback) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(&entry)?.len().saturating_add(8 * 1024);
        anyhow::ensure!(
            self.entries.len() < self.max_batches
                && self.bytes.saturating_add(bytes) <= self.max_bytes,
            "Feedback history is full ({} batches / {} MiB limit). Active comments retained. Open h, then d and Enter to explicitly remove a history record.",
            self.max_batches,
            self.max_bytes / (1024 * 1024)
        );
        self.bytes += bytes;
        self.entries.push_back((entry, bytes));
        Ok(())
    }
    /// Only an explicitly confirmed UI action may call this.
    pub fn remove(&mut self, index: usize) {
        if let Some((_, bytes)) = self.entries.remove(index) {
            self.bytes = self.bytes.saturating_sub(bytes);
        }
    }
    /// Delayed job metadata is derived display state, not editable comments.
    /// Reserve a bounded outcome allowance in the archive budget on insertion.
    pub fn record_outcome(&mut self, outcome: &FeedbackOutcome) {
        for (entry, _) in &mut self.entries {
            if entry.batch.id == outcome.batch_id
                && entry.batch.revision == outcome.revision
                && entry.job_id == Some(outcome.job_id)
                && !entry
                    .outcome
                    .as_ref()
                    .is_some_and(|s| s.starts_with("Failed:"))
            {
                entry.outcome = Some(crate::jobs::types::bound_error(&outcome.outcome));
            }
        }
    }
    pub fn record_terminal(&mut self, job: &crate::jobs::JobSnapshot) {
        for (entry, _) in &mut self.entries {
            if entry.job_id == Some(job.id) {
                if job.status == crate::jobs::JobStatus::Failed {
                    entry.outcome = Some(format!(
                        "Failed: {}",
                        job.error.as_deref().unwrap_or("Repair failed")
                    ));
                } else if entry.outcome.is_none() {
                    entry.outcome = Some(format!(
                        "Repair job {}: {}; comments are not automatically resolved.",
                        job.id, job.status
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(id: &str) -> ArchivedFeedback {
        ArchivedFeedback {
            batch: FeedbackBatch {
                id: id.into(),
                revision: 1,
                source: crate::diff_review::DiffReviewPayload {
                    session_id: Some("session".into()),
                    review_id: Some("source".into()),
                    reject_reason: None,
                    diff: "saved source 日本語".into(),
                    files: vec![],
                    evidence: vec![],
                    evidence_warnings: vec![],
                },
                comments: vec![],
                original_directive_ids: vec![],
            },
            job_id: Some(crate::jobs::JobId(1)),
            submitted_revision: None,
            repair_review_id: None,
            outcome: None,
            stale: false,
        }
    }
    #[test]
    fn feedback_history_capacity_never_evicts_and_explicit_removal_releases_budget() {
        let mut history = FeedbackHistory {
            max_batches: 1,
            ..Default::default()
        };
        history.archive(entry("unsent")).unwrap();
        let bytes = history.bytes;
        assert!(history.archive(entry("new")).is_err());
        assert_eq!(history.get(0).unwrap().batch.id, "unsent");
        assert_eq!(history.bytes, bytes);
        history.remove(0);
        assert_eq!(history.bytes, 0);
        history.archive(entry("new")).unwrap();
        let mut small = FeedbackHistory {
            max_bytes: bytes - 1,
            ..Default::default()
        };
        assert!(small.archive(entry("unsent")).is_err());
        assert_eq!(small.len(), 0);
        assert_eq!(small.bytes, 0);
    }
    #[test]
    fn feedback_history_delayed_outcomes_require_all_ids_and_preserve_failure() {
        let mut history = FeedbackHistory::default();
        history.archive(entry("first")).unwrap();
        let mut second = entry("second");
        second.job_id = Some(crate::jobs::JobId(2));
        history.archive(second).unwrap();
        let mut outcome = FeedbackOutcome {
            batch_id: "first".into(),
            revision: 2,
            job_id: crate::jobs::JobId(1),
            outcome: "wrong revision".into(),
        };
        history.record_outcome(&outcome);
        assert!(history.get(0).unwrap().outcome.is_none());
        outcome.revision = 1;
        outcome.job_id = crate::jobs::JobId(2);
        history.record_outcome(&outcome);
        assert!(history.get(0).unwrap().outcome.is_none());
        outcome.job_id = crate::jobs::JobId(1);
        outcome.outcome = "Failed: checkpoint".into();
        history.record_outcome(&outcome);
        outcome.outcome = "Completed".into();
        history.record_outcome(&outcome);
        assert_eq!(
            history.get(0).unwrap().outcome.as_deref(),
            Some("Failed: checkpoint")
        );
        assert!(history.get(1).unwrap().outcome.is_none());
    }
}
