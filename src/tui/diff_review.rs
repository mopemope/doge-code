use crate::diff_review::DiffReviewPayload;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffReviewState {
    pub files: Vec<DiffFileState>,
    pub selected: usize,
    /// False for legacy raw-diff payloads whose file list was reconstructed
    /// from diff headers (possibly `change-N` placeholders); rejecting those
    /// cannot reliably revert files, so `r` is a no-op for them.
    pub rejectable: bool,
}

impl DiffReviewState {
    pub fn from_payload(payload: DiffReviewPayload) -> Self {
        let rejectable = !payload.files.is_empty();
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

        Self {
            files,
            selected: 0,
            rejectable,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFileState {
    pub path: String,
    pub lines: Vec<DiffLine>,
    pub scroll: usize,
}

impl DiffFileState {
    fn new(path: String) -> Self {
        Self {
            path,
            lines: Vec::new(),
            scroll: 0,
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
            diff:
                "diff --git a/foo.rs b/foo.rs\n--- a/foo.rs\n+++ b/foo.rs\n@@ -1 +1 @@\n-old\n+new"
                    .to_string(),
            files: vec!["foo.rs".to_string()],
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
            diff: "diff --git a/foo.txt b/foo.txt\n--- a/foo.txt\n+++ b/foo.txt\n+hello\n\ndiff --git a/bar.txt b/bar.txt\n--- a/bar.txt\n+++ b/bar.txt\n+world\n".to_string(),
            files: vec!["foo.txt".to_string(), "bar.txt".to_string()],
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
            diff: "diff --git a//dev/null b/bar.txt\n--- /dev/null\n+++ b/bar.txt\n@@ -0,0 +1 @@\n+new file\n"
                .to_string(),
            files: vec!["bar.txt".to_string()],
        };

        let state = DiffReviewState::from_payload(payload);
        assert_eq!(state.files.len(), 1);
        assert_eq!(state.files[0].path, "bar.txt");
        assert_eq!(state.files[0].additions(), 1);
    }

    #[test]
    fn test_empty_diff_falls_back_to_workspace_entry() {
        let payload = DiffReviewPayload {
            diff: String::new(),
            files: vec![],
        };

        let state = DiffReviewState::from_payload(payload);
        assert_eq!(state.files.len(), 1);
        assert_eq!(state.files[0].path, "workspace");
        assert_eq!(state.files[0].lines.len(), 0);
    }

    #[test]
    fn test_file_paths_returns_all_paths() {
        let payload = DiffReviewPayload {
            diff: "diff --git a/a.txt b/a.txt\n+x\ndiff --git a/b.txt b/b.txt\n+y\n".to_string(),
            files: vec![],
        };

        let state = DiffReviewState::from_payload(payload);
        assert_eq!(state.file_paths(), vec!["a.txt", "b.txt"]);
    }
}
