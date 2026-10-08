//! Separate comment input and confirmation; never routes through prompt commands.
use crate::{
    features::review_feedback::{Anchor, Comment, FeedbackBatch, identity},
    tui::state::{DiffReviewFocus, TuiApp},
};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui_textarea::TextArea;

pub struct FeedbackDraft {
    pub batch: FeedbackBatch,
    pub source: crate::tui::diff_review::DiffReviewState,
    pub latest_review: Option<crate::tui::diff_review::DiffReviewState>,
    pub job_id: Option<crate::jobs::JobId>,
    pub submitted_revision: Option<u64>,
    pub repair_review_id: Option<String>,
    pub outcome: Option<String>,
    pub stale: bool,
}

impl FeedbackDraft {
    pub(crate) fn empty(source: crate::tui::diff_review::DiffReviewState) -> Self {
        Self {
            batch: FeedbackBatch {
                id: uuid::Uuid::now_v7().to_string(),
                revision: 0,
                source: source.source.clone(),
                comments: vec![],
                original_directive_ids: vec![],
            },
            source,
            latest_review: None,
            job_id: None,
            submitted_revision: None,
            repair_review_id: None,
            outcome: None,
            stale: false,
        }
    }
    fn archived(&self) -> crate::features::review_feedback::history::ArchivedFeedback {
        crate::features::review_feedback::history::ArchivedFeedback {
            batch: self.batch.clone(),
            job_id: self.job_id,
            submitted_revision: self.submitted_revision,
            repair_review_id: self.repair_review_id.clone(),
            outcome: self.outcome.clone(),
            stale: self.stale,
        }
    }
}

pub struct HistoryView {
    pub selected: usize,
    pub scroll: usize,
    pub source: bool,
    pub confirm_delete: bool,
}

pub struct LineSelector {
    pub anchor: Anchor,
    pub cursor: usize,
    pub pivot: usize,
}

pub struct CommentEditor {
    pub anchor: Anchor,
    pub textarea: TextArea<'static>,
}

impl TuiApp {
    pub(crate) fn feedback_anchor(&self) -> Option<Anchor> {
        let review = self.diff_review.as_ref()?;
        if !review.rejectable || review.rejecting {
            return None;
        }
        let file = review.current_file()?;
        Some(Anchor {
            selection: None,
            version: 1,
            session_id: review.session_id.clone()?,
            review_id: review.review_id.clone()?,
            review_identity: identity(&review.source.diff),
            path: file.path.clone(),
            hunk: file.hunks.get(file.selected_hunk)?.clone(),
        })
    }

    pub(crate) fn start_comment(&mut self) {
        let Some(anchor) = self.feedback_anchor() else {
            self.push_log("[feedback] This review/hunk cannot be targeted. Unsupported diffs remain viewable.");
            return;
        };
        self.start_comment_at(anchor);
    }

    pub(crate) fn start_comment_at(&mut self, anchor: Anchor) {
        if self
            .review_feedback
            .as_ref()
            .is_some_and(|d| d.batch.source.review_id.as_ref() != Some(&anchor.review_id))
        {
            self.push_log("[feedback] Saved comments belong to another review. n archives them and starts an empty batch on this review; h browses history. s inspects the active comments.");
            return;
        }
        if self.foreground_busy() {
            self.push_log("[feedback] Foreground work is running; saved comments retained.");
            return;
        }
        if self.review_feedback.is_none() {
            let Some(source) = self.diff_review.as_ref().cloned() else {
                return;
            };
            self.review_feedback = Some(FeedbackDraft::empty(source));
        }
        let text = self
            .review_feedback
            .as_ref()
            .and_then(|d| d.batch.comments.iter().find(|c| c.anchor == anchor))
            .map(|c| c.text.clone())
            .unwrap_or_default();
        let textarea = TextArea::from(text.split('\n').map(str::to_owned).collect::<Vec<_>>());
        self.comment_editor = Some(CommentEditor { anchor, textarea });
        self.dirty = true;
    }

    pub(crate) fn start_line_selection(&mut self) {
        let Some(anchor) = self.feedback_anchor() else {
            return;
        };
        if self.foreground_busy()
            || self
                .review_feedback
                .as_ref()
                .is_some_and(|d| d.batch.source.review_id.as_ref() != Some(&anchor.review_id))
        {
            self.push_log("[feedback] Wait for work or press n on the current review; saved comments retained.");
            return;
        }
        if !crate::features::review_feedback::selection::rows(&anchor.hunk).is_empty() {
            self.line_selector = Some(LineSelector {
                anchor,
                cursor: 0,
                pivot: 0,
            });
            self.dirty = true;
        }
    }

    fn handle_line_selection_key(&mut self, key: KeyEvent) -> bool {
        let Some(view) = self.line_selector.as_mut() else {
            return false;
        };
        if key.kind == KeyEventKind::Release {
            return true;
        }
        let count = crate::features::review_feedback::selection::rows(&view.anchor.hunk).len();
        if key.code == KeyCode::Esc && key.kind == KeyEventKind::Press {
            self.line_selector = None;
        } else if key.code == KeyCode::Enter
            && key.modifiers.is_empty()
            && key.kind == KeyEventKind::Press
        {
            let mut anchor = view.anchor.clone();
            match crate::features::review_feedback::selection::select(
                &anchor.hunk,
                view.cursor.min(view.pivot),
                view.cursor.max(view.pivot),
            ) {
                Ok(selection) => {
                    anchor.selection = Some(selection);
                    self.line_selector = None;
                    // A new review may have arrived while selecting; never reattach the frozen target.
                    if self.feedback_anchor().is_some_and(|current| {
                        current.review_id == anchor.review_id && current.hunk == anchor.hunk
                    }) {
                        self.start_comment_at(anchor);
                    } else {
                        self.push_log("[feedback] Source changed while selecting; selection canceled, comments retained.");
                    }
                }
                Err(error) => self.push_log(format!("[feedback] {error}")),
            }
        } else if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
            let next = match key.code {
                KeyCode::Up => Some(view.cursor.saturating_sub(1)),
                KeyCode::Down => Some(view.cursor.saturating_add(1).min(count.saturating_sub(1))),
                KeyCode::Home => Some(0),
                KeyCode::End => Some(count.saturating_sub(1)),
                _ => None,
            };
            if let Some(next) = next {
                view.cursor = next;
                if !key.modifiers.contains(KeyModifiers::SHIFT) {
                    view.pivot = next;
                }
            }
        }
        self.dirty = true;
        true
    }

    pub(crate) fn save_comment(&mut self) {
        let Some(editor) = self.comment_editor.take() else {
            return;
        };
        let text = editor.textarea.lines().join("\n");
        let Some(draft) = self.review_feedback.as_mut() else {
            return;
        };
        if text.len() > 64 * 1024 {
            self.comment_editor = Some(editor);
            self.push_log("[feedback] Comment exceeds 64 KiB; shorten it before saving.");
            return;
        }
        if let Some(index) = draft
            .batch
            .comments
            .iter()
            .position(|c| c.anchor == editor.anchor)
        {
            if text.trim().is_empty() {
                draft.batch.comments.remove(index);
            } else {
                draft.batch.comments[index].text = text;
            }
        } else if !text.trim().is_empty() {
            draft.batch.comments.push(Comment {
                anchor: editor.anchor,
                text,
            });
        }
        draft.batch.revision += 1;
        self.sync_comment_list();
        self.push_log("[feedback] Comment saved locally. No model request was made.");
        self.dirty = true;
    }

    pub(crate) fn delete_comment(&mut self) {
        if let Some(anchor) = self.feedback_anchor()
            && let Some(draft) = self.review_feedback.as_mut()
        {
            draft.batch.comments.retain(|c| c.anchor != anchor);
            draft.batch.revision += 1;
            self.dirty = true;
        }
    }

    pub(crate) fn confirm_feedback(&mut self) {
        if self.review_feedback.is_none() {
            self.push_log("[feedback] Add hunk comments with c first.");
            return;
        }
        self.feedback_confirmation = true;
        self.feedback_confirmation_scroll = 0;
        self.feedback_error = self.feedback_validation_error();
        self.dirty = true;
    }

    pub(crate) fn feedback_validation_error(&self) -> Option<String> {
        let draft = self.review_feedback.as_ref()?;
        if draft.submitted_revision == Some(draft.batch.revision) {
            return Some("This revision was already submitted. Comments remain available; no automatic resolution is assumed.".into());
        }
        if draft.stale
            || self.diff_review.as_ref().and_then(|r| r.review_id.as_ref())
                != draft.batch.source.review_id.as_ref()
        {
            return Some("Another review arrived; saved comments cannot be reattached. n archives this batch and starts a new one on the current live review.".into());
        }
        if self.foreground_busy() || self.diff_review.as_ref().is_some_and(|r| r.rejecting) {
            return Some("Foreground work/rollback is running. Wait before submitting.".into());
        }
        match self.handler.as_ref() {
            Some(handler) => handler
                .validate_review_feedback(&draft.batch)
                .err()
                .map(|e| e.to_string()),
            None => Some("No command handler; comments retained.".into()),
        }
    }

    pub(crate) fn submit_feedback(&mut self) {
        if let Some(error) = self.feedback_validation_error() {
            self.feedback_error = Some(error);
            self.dirty = true;
            return;
        }
        let Some(batch) = self.review_feedback.as_ref().map(|d| d.batch.clone()) else {
            return;
        };
        let Some(mut handler) = self.handler.take() else {
            return;
        };
        let result = handler.submit_review_feedback(batch, self);
        self.handler = Some(handler);
        match result {
            Ok(job) => {
                if let Some(draft) = self.review_feedback.as_mut() {
                    draft.job_id = Some(job);
                    draft.submitted_revision = Some(draft.batch.revision);
                    draft.outcome = None;
                }
                self.feedback_confirmation = false;
                self.diff_review_focus = DiffReviewFocus::Input;
                self.push_log(format!("[feedback] One batch repair job {job} started. Comments remain saved; review all resulting changes."));
            }
            Err(error) => self.feedback_error = Some(format!("{error}; comments retained.")),
        }
        self.dirty = true;
    }

    /// Archive only after validating the current source; no model/directive dispatch.
    pub(crate) fn start_fresh_feedback(&mut self) {
        let result = self.prepare_fresh_feedback();
        if let Err(error) = result {
            self.feedback_error = Some(error.to_string());
            self.push_log(format!("[feedback] {error}"));
        }
        self.dirty = true;
    }

    fn prepare_fresh_feedback(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.comment_editor.is_none()
                && self.line_selector.is_none()
                && self.comment_list.is_none(),
            "Save or cancel comment editing first; comments retained."
        );
        anyhow::ensure!(
            !self.foreground_busy(),
            "Foreground work is running; comments retained."
        );
        let source = self
            .diff_review
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No current review; comments retained."))?;
        anyhow::ensure!(
            source.rejectable
                && !source.rejecting
                && source.files.iter().any(|f| !f.hunks.is_empty()),
            "Current review has no available repair capture/hunks; comments retained."
        );
        if let Some(draft) = &self.review_feedback {
            anyhow::ensure!(
                source.review_id != draft.batch.source.review_id,
                "This batch already belongs to the current review. Continue with c; comments retained."
            );
        }
        self.handler
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No handler; comments retained."))?
            .validate_review_feedback_source(&source.source)?;
        let source = source.clone();
        if let Some(draft) = &self.review_feedback {
            self.feedback_history.archive(draft.archived())?;
        }
        self.review_feedback = Some(FeedbackDraft::empty(source));
        self.feedback_confirmation = false;
        self.feedback_error = None;
        self.diff_review_focus = DiffReviewFocus::Review;
        self.push_log("[feedback] Empty batch attached to current review. Previous comments stay read-only in h history. c adds new comments; no model request was made.");
        Ok(())
    }

    pub(crate) fn open_feedback_history(&mut self) {
        self.feedback_history_view = Some(HistoryView {
            selected: self.feedback_history.len().saturating_sub(1),
            scroll: 0,
            source: false,
            confirm_delete: false,
        });
        self.dirty = true;
    }

    fn handle_feedback_history_key(&mut self, key: KeyEvent) -> bool {
        let Some(view) = self.feedback_history_view.as_mut() else {
            return false;
        };
        if key.kind == KeyEventKind::Release {
            return true;
        }
        if view.confirm_delete {
            if key.kind == KeyEventKind::Press && key.modifiers.is_empty() {
                match key.code {
                    KeyCode::Esc => view.confirm_delete = false,
                    KeyCode::Enter => {
                        self.feedback_history.remove(view.selected);
                        view.selected = view
                            .selected
                            .min(self.feedback_history.len().saturating_sub(1));
                        view.scroll = 0;
                        view.confirm_delete = false;
                    }
                    _ => {}
                }
            }
        } else {
            match key.code {
                KeyCode::Esc if key.kind == KeyEventKind::Press => {
                    self.feedback_history_view = None
                }
                KeyCode::Left => {
                    view.selected = view.selected.saturating_sub(1);
                    view.scroll = 0;
                }
                KeyCode::Right => {
                    view.selected = view
                        .selected
                        .saturating_add(1)
                        .min(self.feedback_history.len().saturating_sub(1));
                    view.scroll = 0;
                }
                KeyCode::Up => view.scroll = view.scroll.saturating_sub(1),
                KeyCode::Down => view.scroll = view.scroll.saturating_add(1),
                KeyCode::PageUp => view.scroll = view.scroll.saturating_sub(10),
                KeyCode::PageDown => view.scroll = view.scroll.saturating_add(10),
                KeyCode::Home => view.scroll = 0,
                KeyCode::End => view.scroll = usize::MAX,
                KeyCode::Char('v')
                    if key.kind == KeyEventKind::Press && key.modifiers.is_empty() =>
                {
                    view.source = !view.source;
                    view.scroll = 0;
                }
                KeyCode::Char('d')
                    if key.kind == KeyEventKind::Press
                        && key.modifiers.is_empty()
                        && self.feedback_history.get(view.selected).is_some() =>
                {
                    view.confirm_delete = true
                }
                _ => {}
            }
        }
        self.dirty = true;
        true
    }

    pub(crate) fn scroll_feedback_history(&mut self, delta: isize) -> bool {
        if let Some(view) = self.feedback_history_view.as_mut() {
            if !view.confirm_delete {
                view.scroll = view.scroll.saturating_add_signed(delta);
            }
            self.dirty = true;
            true
        } else {
            false
        }
    }

    pub(crate) fn archived_job_message(&mut self, producer: crate::jobs::JobId, message: &str) {
        if let Some(json) = message.strip_prefix("::feedback_outcome:")
            && let Ok(outcome) =
                serde_json::from_str::<crate::features::review_feedback::FeedbackOutcome>(json)
            && outcome.job_id == producer
        {
            self.feedback_history.record_outcome(&outcome);
            self.dirty = true;
        }
    }

    pub(crate) fn apply_feedback_outcome(
        &mut self,
        outcome: crate::features::review_feedback::FeedbackOutcome,
    ) {
        if let Some(draft) = self.review_feedback.as_mut().filter(|d| {
            d.batch.id == outcome.batch_id
                && d.batch.revision == outcome.revision
                && d.job_id == Some(outcome.job_id)
        }) && !draft
            .outcome
            .as_ref()
            .is_some_and(|s| s.starts_with("Failed:"))
        {
            draft.outcome = Some(outcome.outcome.clone());
        }
        self.feedback_history.record_outcome(&outcome);
        self.dirty = true;
    }

    /// Higher priority than normal input, including modified keys and repeats.
    pub(crate) fn handle_feedback_key(&mut self, key: KeyEvent) -> bool {
        if self.handle_feedback_history_key(key) {
            return true;
        }
        if self.handle_line_selection_key(key) {
            return true;
        }
        if self.comment_editor.is_none() && self.handle_comment_list_key(key) {
            return true;
        }
        if self.comment_editor.is_none() && !self.feedback_confirmation {
            return false;
        }
        if key.kind == KeyEventKind::Release {
            return true;
        }
        if let Some(editor) = self.comment_editor.as_mut() {
            match key.code {
                KeyCode::Esc if key.kind == KeyEventKind::Press => self.comment_editor = None,
                KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                    editor.textarea.insert_newline();
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    if key.kind == KeyEventKind::Press {
                        self.save_comment();
                    }
                }
                KeyCode::Enter => {}
                _ => {
                    editor.textarea.input(key);
                }
            }
        } else {
            match key.code {
                KeyCode::Esc if key.kind == KeyEventKind::Press => {
                    self.feedback_confirmation = false
                }
                KeyCode::Enter if key.kind == KeyEventKind::Press && key.modifiers.is_empty() => {
                    self.submit_feedback()
                }
                KeyCode::Char('m')
                    if key.kind == KeyEventKind::Press && key.modifiers.is_empty() =>
                {
                    self.open_comment_list()
                }
                KeyCode::Char('n')
                    if key.kind == KeyEventKind::Press && key.modifiers.is_empty() =>
                {
                    self.start_fresh_feedback()
                }
                KeyCode::Char('h')
                    if key.kind == KeyEventKind::Press && key.modifiers.is_empty() =>
                {
                    self.open_feedback_history()
                }
                KeyCode::Char('v')
                    if key.kind == KeyEventKind::Press && key.modifiers.is_empty() =>
                {
                    self.show_feedback_source();
                    self.feedback_confirmation = false;
                }
                KeyCode::Char('d')
                    if key.kind == KeyEventKind::Press && key.modifiers.is_empty() =>
                {
                    self.review_feedback = None;
                    self.feedback_confirmation = false;
                    self.feedback_error = None;
                }
                KeyCode::Down => {
                    self.feedback_confirmation_scroll =
                        self.feedback_confirmation_scroll.saturating_add(1)
                }
                KeyCode::Up => {
                    self.feedback_confirmation_scroll =
                        self.feedback_confirmation_scroll.saturating_sub(1)
                }
                KeyCode::PageDown => {
                    self.feedback_confirmation_scroll =
                        self.feedback_confirmation_scroll.saturating_add(10)
                }
                KeyCode::PageUp => {
                    self.feedback_confirmation_scroll =
                        self.feedback_confirmation_scroll.saturating_sub(10)
                }
                _ => {}
            }
        }
        self.dirty = true;
        true
    }

    pub(crate) fn handle_paste(&mut self, text: &str) {
        let clean: String = text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .collect();
        if let Some(editor) = self.comment_editor.as_mut() {
            if clean.len()
                + editor
                    .textarea
                    .lines()
                    .iter()
                    .map(String::len)
                    .sum::<usize>()
                <= 64 * 1024
            {
                editor.textarea.insert_str(clean);
            }
        } else if !self.feedback_confirmation
            && self.feedback_history_view.is_none()
            && self.line_selector.is_none()
            && self.comment_list.is_none()
            && self.diff_review_focus == DiffReviewFocus::Input
            && self.input_mode == crate::tui::state::InputMode::Normal
        {
            self.textarea.insert_str(clean);
        }
        self.dirty = true;
    }

    pub(crate) fn feedback_review_arrived(
        &mut self,
        review: &crate::tui::diff_review::DiffReviewState,
    ) {
        if let Some(draft) = self.review_feedback.as_mut() {
            if review.review_id != draft.batch.source.review_id {
                draft.latest_review = Some(review.clone());
            }
            if draft.job_id.is_some_and(|id| {
                review
                    .review_id
                    .as_ref()
                    .is_some_and(|r| r.starts_with(&format!("{id}-")))
            }) {
                draft.repair_review_id = review.review_id.clone();
            } else if review.review_id != draft.batch.source.review_id {
                draft.stale = true;
            }
        }
    }

    pub(crate) fn show_feedback_source(&mut self) {
        if let Some(draft) = &self.review_feedback {
            let showing_source = self
                .diff_review
                .as_ref()
                .is_some_and(|r| r.review_id == draft.source.review_id);
            self.diff_review = if showing_source {
                draft
                    .latest_review
                    .clone()
                    .or_else(|| Some(draft.source.clone()))
            } else {
                Some(draft.source.clone())
            };
            self.diff_review_focus = DiffReviewFocus::Review;
            self.dirty = true;
        }
    }

    pub(crate) fn move_hunk(&mut self, delta: isize) {
        if let Some(file) = self.diff_review.as_mut().and_then(|r| r.current_file_mut()) {
            if file.hunks.is_empty() {
                return;
            }
            file.selected_hunk = file
                .selected_hunk
                .saturating_add_signed(delta)
                .min(file.hunks.len() - 1);
            // Projection includes comment rows, so locate the raw hunk in it.
            let row = file.hunks[file.selected_hunk].start_row;
            let projected = self
                .diff_projection()
                .iter()
                .position(|r| r.raw_row == Some(row))
                .unwrap_or(row);
            if let Some(file) = self.diff_review.as_mut().and_then(|r| r.current_file_mut()) {
                file.scroll = projected;
            }
            self.dirty = true;
        }
    }
}

pub struct DisplayRow {
    pub content: String,
    pub kind: crate::tui::diff_review::DiffLineKind,
    pub selected: bool,
    pub raw_row: Option<usize>,
}

/// Wrap comments by terminal cells; never change raw diff or anchor coordinates.
pub(crate) fn wrap_cells(text: &str, width: usize) -> Vec<String> {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    let width = width.max(1);
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let mut row = String::new();
        let mut cells = 0;
        for g in line.graphemes(true) {
            let size = g.width();
            if cells + size > width && !row.is_empty() {
                rows.push(row);
                row = String::new();
                cells = 0;
            }
            row.push_str(g);
            cells += size;
        }
        rows.push(row);
    }
    rows
}

impl TuiApp {
    pub(crate) fn diff_projection(&self) -> Vec<DisplayRow> {
        use crate::tui::diff_review::DiffLineKind;
        let Some(review) = self.diff_review.as_ref() else {
            return vec![];
        };
        let Some(file) = review.current_file() else {
            return vec![];
        };
        let mut rows = Vec::new();
        let (mut hunk_index, mut old, mut new) = (0, 0, 0);
        for (index, line) in file.lines.iter().enumerate() {
            while file
                .hunks
                .get(hunk_index)
                .is_some_and(|h| index >= h.end_row)
            {
                hunk_index += 1;
            }
            let hunk = file
                .hunks
                .get(hunk_index)
                .filter(|h| index >= h.start_row && index < h.end_row);
            if let Some(h) = hunk.filter(|h| index == h.start_row) {
                old = h.old.start;
                new = h.new.start;
            }
            let selected = hunk.is_some() && hunk_index == file.selected_hunk;
            let prefix = if hunk.is_some_and(|h| index > h.start_row) {
                match line.content.as_bytes().first() {
                    Some(b' ') => format!("{old:>4} {new:>4} "),
                    Some(b'-') => format!("{old:>4}      "),
                    Some(b'+') => format!("     {new:>4} "),
                    _ => "           ".into(),
                }
            } else {
                String::new()
            };
            if hunk.is_some_and(|h| index > h.start_row) {
                match line.content.as_bytes().first() {
                    Some(b' ') => {
                        old += 1;
                        new += 1;
                    }
                    Some(b'-') => old += 1,
                    Some(b'+') => new += 1,
                    _ => {}
                }
            }
            rows.push(DisplayRow {
                content: format!("{prefix}{}", line.content),
                kind: line.kind,
                selected,
                raw_row: Some(index),
            });
            if let Some(h) = hunk.filter(|h| index + 1 == h.end_row)
                && let Some(draft) = self
                    .review_feedback
                    .as_ref()
                    .filter(|d| d.batch.source.review_id == review.review_id)
            {
                for comment in draft
                    .batch
                    .comments
                    .iter()
                    .filter(|c| c.anchor.path == file.path && c.anchor.hunk == *h)
                {
                    for text in wrap_cells(
                        &format!(
                            "Comment ({}): {}",
                            comment.anchor.target_label(),
                            comment.text
                        ),
                        self.diff_viewport_width.get(),
                    ) {
                        rows.push(DisplayRow {
                            content: text,
                            kind: DiffLineKind::Other,
                            selected,
                            raw_row: None,
                        });
                    }
                }
            }
        }
        rows
    }
}
