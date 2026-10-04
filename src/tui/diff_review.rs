use crate::diff_review::{DiffFileEvidence, DiffReviewPayload};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffReviewState {
    pub review_id: Option<String>,
    pub reject_reason: Option<String>,
    pub files: Vec<DiffFileState>,
    pub selected: usize,
    /// False for legacy raw-diff payloads whose file list was reconstructed
    /// from diff headers (possibly `change-N` placeholders); rejecting those
    /// cannot reliably revert files, so `r` is a no-op for them.
    pub rejectable: bool,
    pub rejecting: bool,
    pub reject_job_id: Option<crate::jobs::JobId>,
    /// Provenance evidence warnings (e.g. truncated, incomplete). Never fails
    /// the diff view.
    pub evidence_warnings: Vec<String>,
}

impl DiffReviewState {
    pub fn from_payload(payload: DiffReviewPayload) -> Self {
        let rejectable = payload.review_id.is_some()
            && payload.reject_reason.is_none()
            && !payload.files.is_empty();
        let mut files = Vec::new();
        let mut current: Option<DiffFileState> = None;
        let mut file_index = 0usize;
        let mut names_iter = payload.files.into_iter();

        for line in payload.diff.lines() {
            if line.starts_with("diff --git ") {
                if let Some(file) = current.take() {
                    files.push(file);
                }

                let from_list = names_iter.next();
                let parsed = parse_path_from_diff_header(line);
                let path = if let Some(name) = from_list {
                    name
                } else if let Some(parsed) = parsed {
                    parsed
                } else {
                    format!("change-{}", file_index + 1)
                };
                file_index += 1;

                let mut file_state = DiffFileState::new(path);
                file_state.push_line(line);
                current = Some(file_state);
                continue;
            }

            if current.is_none() {
                let fallback_name = names_iter.next().unwrap_or_else(|| "workspace".to_string());
                current = Some(DiffFileState::new(fallback_name));
            }

            if let Some(file) = current.as_mut() {
                file.push_line(line);
            }
        }

        if let Some(file) = current {
            files.push(file);
        }

        if files.is_empty() {
            files.push(DiffFileState::new("workspace".to_string()));
        }

        // Attach evidence by exact path match.
        let mut evidence_by_path: std::collections::HashMap<String, DiffFileEvidence> =
            std::collections::HashMap::new();
        for ev in payload.evidence {
            evidence_by_path.insert(ev.path.clone(), ev);
        }
        for f in files.iter_mut() {
            if let Some(ev) = evidence_by_path.remove(&f.path) {
                f.evidence = Some(ev);
            }
        }

        Self {
            review_id: payload.review_id,
            reject_reason: payload.reject_reason,
            files,
            selected: 0,
            rejectable,
            rejecting: false,
            reject_job_id: None,
            evidence_warnings: payload.evidence_warnings,
        }
    }

    pub fn current_file(&self) -> Option<&DiffFileState> {
        self.files.get(self.selected)
    }

    pub fn current_file_mut(&mut self) -> Option<&mut DiffFileState> {
        self.files.get_mut(self.selected)
    }

    pub fn file_paths(&self) -> Vec<String> {
        self.files.iter().map(|f| f.path.clone()).collect()
    }

    /// Compact evidence summary for small terminals, e.g.
    /// "Evidence 1/3 observed passing".
    pub fn evidence_summary(&self) -> Option<String> {
        let mut total = 0usize;
        let mut passing = 0usize;
        for f in &self.files {
            if let Some(ev) = &f.evidence {
                for ob in &ev.obligations {
                    total += 1;
                    if ob.state == "observed_passing" {
                        passing += 1;
                    }
                }
            }
        }
        if total == 0 {
            return None;
        }
        Some(format!("Evidence {passing}/{total} observed passing"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFileState {
    pub path: String,
    pub lines: Vec<DiffLine>,
    pub scroll: usize,
    pub evidence: Option<DiffFileEvidence>,
}

impl DiffFileState {
    fn new(path: String) -> Self {
        Self {
            path,
            lines: Vec::new(),
            scroll: 0,
            evidence: None,
        }
    }

    fn push_line(&mut self, line: &str) {
        let kind = DiffLineKind::from_line(line);
        self.lines.push(DiffLine {
            content: line.to_string(),
            kind,
        });
    }

    pub fn additions(&self) -> usize {
        self.lines
            .iter()
            .filter(|line| matches!(line.kind, DiffLineKind::Addition))
            .count()
    }

    pub fn removals(&self) -> usize {
        self.lines
            .iter()
            .filter(|line| matches!(line.kind, DiffLineKind::Removal))
            .count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub content: String,
    pub kind: DiffLineKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Header,
    FileMeta,
    HunkHeader,
    Addition,
    Removal,
    Context,
    Other,
}

impl DiffLineKind {
    fn from_line(line: &str) -> Self {
        if line.starts_with("diff --git") {
            Self::Header
        } else if line.starts_with("@@") {
            Self::HunkHeader
        } else if line.starts_with("+++") || line.starts_with("---") || line.starts_with("index ") {
            Self::FileMeta
        } else if line.starts_with('+') {
            Self::Addition
        } else if line.starts_with('-') {
            Self::Removal
        } else if line.starts_with(' ') {
            Self::Context
        } else {
            Self::Other
        }
    }
}

fn parse_path_from_diff_header(line: &str) -> Option<String> {
    let mut parts = line.split_whitespace();
    let _ = parts.next();
    let _ = parts.next();
    let _a = parts.next();
    let b = parts.next()?;
    Some(b.trim_start_matches("b/").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_review::DiffReviewPayload;

    #[test]
    fn test_builds_review_state_from_single_file_payload() {
        let payload = DiffReviewPayload {
            review_id: None,
            reject_reason: None,
            diff:
                "diff --git a/foo.rs b/foo.rs\n--- a/foo.rs\n+++ b/foo.rs\n@@ -1 +1 @@\n-old\n+new"
                    .to_string(),
            files: vec!["foo.rs".to_string()],
            evidence: Vec::new(),
            evidence_warnings: Vec::new(),
        };

        let state = DiffReviewState::from_payload(payload);
        assert_eq!(state.files.len(), 1);
        assert_eq!(state.files[0].path, "foo.rs");
        assert_eq!(state.files[0].lines.len(), 6);
        assert_eq!(state.files[0].additions(), 1);
        assert_eq!(state.files[0].removals(), 1);
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn test_groups_multiple_files() {
        let payload = DiffReviewPayload {
            review_id: None,
            reject_reason: None,
            diff: "diff --git a/foo.txt b/foo.txt\n--- a/foo.txt\n+++ b/foo.txt\n+hello\n\ndiff --git a/bar.txt b/bar.txt\n--- a/bar.txt\n+++ b/bar.txt\n+world\n".to_string(),
            files: vec!["foo.txt".to_string(), "bar.txt".to_string()],
            evidence: Vec::new(),
            evidence_warnings: Vec::new(),
        };

        let state = DiffReviewState::from_payload(payload);
        assert_eq!(state.files.len(), 2);
        assert_eq!(state.files[0].path, "foo.txt");
        assert_eq!(state.files[1].path, "bar.txt");
        assert_eq!(state.selected, 0);
        assert_eq!(state.files[0].scroll, 0);
        assert_eq!(state.files[0].additions(), 1);
        assert_eq!(state.files[1].additions(), 1);
    }

    #[test]
    fn test_untracked_file_diff_uses_no_index_header() {
        // `git diff --no-index /dev/null bar.txt` produces paths like
        // "a//dev/null b/bar.txt"; the file list should win over the header path.
        let payload = DiffReviewPayload {
            review_id: None,
            reject_reason: None,
            diff: "diff --git a//dev/null b/bar.txt\n--- /dev/null\n+++ b/bar.txt\n@@ -0,0 +1 @@\n+new file\n"
                .to_string(),
            files: vec!["bar.txt".to_string()],
            evidence: Vec::new(),
            evidence_warnings: Vec::new(),
        };

        let state = DiffReviewState::from_payload(payload);
        assert_eq!(state.files.len(), 1);
        assert_eq!(state.files[0].path, "bar.txt");
        assert_eq!(state.files[0].additions(), 1);
    }

    #[test]
    fn test_empty_diff_falls_back_to_workspace_entry() {
        let payload = DiffReviewPayload {
            review_id: None,
            reject_reason: None,
            diff: String::new(),
            files: vec![],
            evidence: Vec::new(),
            evidence_warnings: Vec::new(),
        };

        let state = DiffReviewState::from_payload(payload);
        assert_eq!(state.files.len(), 1);
        assert_eq!(state.files[0].path, "workspace");
        assert_eq!(state.files[0].lines.len(), 0);
    }

    #[test]
    fn test_file_paths_returns_all_paths() {
        let payload = DiffReviewPayload {
            review_id: None,
            reject_reason: None,
            diff: "diff --git a/a.txt b/a.txt\n+x\ndiff --git a/b.txt b/b.txt\n+y\n".to_string(),
            files: vec![],
            evidence: Vec::new(),
            evidence_warnings: Vec::new(),
        };

        let state = DiffReviewState::from_payload(payload);
        assert_eq!(state.file_paths(), vec!["a.txt", "b.txt"]);
    }

    fn evidence_payload() -> DiffReviewPayload {
        use crate::diff_review::{DiffFileEvidence, DiffObligationEvidence};
        DiffReviewPayload {
            review_id: None,
            reject_reason: None,
            diff: "diff --git a/a.txt b/a.txt\n+x\ndiff --git a/b.txt b/b.txt\n+y\n".to_string(),
            files: vec!["a.txt".to_string(), "b.txt".to_string()],
            evidence: vec![
                DiffFileEvidence {
                    path: "a.txt".to_string(),
                    requirement_ids: vec!["req-1".to_string()],
                    plan_item_ids: vec!["step-1".to_string()],
                    obligations: vec![DiffObligationEvidence {
                        id: "vo-1".to_string(),
                        description: "desc".to_string(),
                        kind: "test".to_string(),
                        state: "observed_passing".to_string(),
                        command_summary: Some("cargo test".to_string()),
                    }],
                },
                DiffFileEvidence {
                    path: "b.txt".to_string(),
                    requirement_ids: vec![],
                    plan_item_ids: vec!["step-2".to_string()],
                    obligations: vec![DiffObligationEvidence {
                        id: "vo-2".to_string(),
                        description: "desc".to_string(),
                        kind: "lint".to_string(),
                        state: "pending".to_string(),
                        command_summary: None,
                    }],
                },
            ],
            evidence_warnings: vec![],
        }
    }

    #[test]
    fn test_evidence_attached_by_path() {
        let state = DiffReviewState::from_payload(evidence_payload());
        assert_eq!(state.files.len(), 2);
        assert!(state.files[0].evidence.is_some());
        assert_eq!(state.files[0].evidence.as_ref().unwrap().path, "a.txt");
        assert_eq!(state.files[1].evidence.as_ref().unwrap().path, "b.txt");
    }

    #[test]
    fn test_current_file_switch_changes_evidence() {
        let mut state = DiffReviewState::from_payload(evidence_payload());
        assert_eq!(
            state
                .current_file()
                .unwrap()
                .evidence
                .as_ref()
                .unwrap()
                .obligations[0]
                .id,
            "vo-1"
        );
        state.selected = 1;
        assert_eq!(
            state
                .current_file()
                .unwrap()
                .evidence
                .as_ref()
                .unwrap()
                .obligations[0]
                .id,
            "vo-2"
        );
    }

    #[test]
    fn test_evidence_summary_counts() {
        let state = DiffReviewState::from_payload(evidence_payload());
        assert_eq!(
            state.evidence_summary(),
            Some("Evidence 1/2 observed passing".to_string())
        );
    }

    #[test]
    fn test_legacy_payload_without_evidence_renders() {
        let payload = DiffReviewPayload {
            review_id: None,
            reject_reason: None,
            diff: "diff --git a/a.txt b/a.txt\n+x\n".to_string(),
            files: vec!["a.txt".to_string()],
            evidence: Vec::new(),
            evidence_warnings: Vec::new(),
        };
        let state = DiffReviewState::from_payload(payload);
        assert!(state.current_file().unwrap().evidence.is_none());
        assert_eq!(state.evidence_summary(), None);
    }

    #[test]
    fn test_small_terminal_state_does_not_panic() {
        // State construction must not panic regardless of terminal size;
        // rendering fallback is in rendering.rs (area.height check).
        let state = DiffReviewState::from_payload(evidence_payload());
        assert_eq!(state.files.len(), 2);
        // Simulate selection change on tiny terminal.
        let mut tiny = state;
        tiny.selected = 10; // out of bounds
        assert!(tiny.current_file().is_none());
    }
}
