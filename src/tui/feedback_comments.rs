//! Active saved-comment navigation; archived history never enters this path.
use crate::features::review_feedback::Comment;
use crate::tui::state::{DiffReviewFocus, TuiApp};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

pub struct CommentList {
    pub batch_id: String,
    pub revision: u64,
    pub selected: usize,
    pub detail_scroll: usize,
    pub confirm_delete: bool,
    pub error: Option<String>,
}
impl TuiApp {
    pub(crate) fn open_comment_list(&mut self) {
        let Some(draft) = &self.review_feedback else {
            self.push_log("[feedback] No active saved comments. c or l adds comments; h browses archived history.");
            return;
        };
        self.comment_list = Some(CommentList {
            batch_id: draft.batch.id.clone(),
            revision: draft.batch.revision,
            selected: 0,
            detail_scroll: 0,
            confirm_delete: false,
            error: None,
        });
        self.feedback_confirmation = false;
        self.dirty = true;
    }
    fn listed_comment(&self) -> anyhow::Result<Comment> {
        let list = self
            .comment_list
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No comment list."))?;
        let draft = self
            .review_feedback
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Active batch changed; reopen m. Comments retained."))?;
        anyhow::ensure!(
            draft.batch.id == list.batch_id && draft.batch.revision == list.revision,
            "Active batch changed; reopen m. Comments retained."
        );
        draft
            .batch
            .comments
            .get(list.selected)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("No selected comment."))
    }
    fn comment_list_live(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.foreground_busy() && !self.diff_review.as_ref().is_some_and(|r| r.rejecting),
            "Foreground work/rollback is running; comments retained."
        );
        let draft = self
            .review_feedback
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No active batch."))?;
        anyhow::ensure!(
            !draft.stale,
            "Saved target is stale; viewing remains available. Comments retained."
        );
        self.handler
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No handler; comments retained."))?
            .validate_review_feedback_source(&draft.batch.source)
    }
    fn jump_listed_comment(&mut self, comment: &Comment) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.foreground_busy() && !self.diff_review.as_ref().is_some_and(|r| r.rejecting),
            "Foreground work/rollback is running; current review retained."
        );
        let draft = self
            .review_feedback
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No active batch."))?;
        let mut review = draft.source.clone();
        let selected = review
            .files
            .iter()
            .position(|f| f.path == comment.anchor.path)
            .ok_or_else(|| anyhow::anyhow!("Saved path missing; current review retained."))?;
        let file = &mut review.files[selected];
        let hunk = file
            .hunks
            .iter()
            .position(|h| *h == comment.anchor.hunk)
            .ok_or_else(|| anyhow::anyhow!("Saved hunk missing; current review retained."))?;
        let raw_row = if let Some(selection) = &comment.anchor.selection {
            anyhow::ensure!(
                crate::features::review_feedback::selection::select(
                    &comment.anchor.hunk,
                    selection.start_row,
                    selection.end_row
                )? == *selection,
                "Saved selection changed; current review retained."
            );
            (comment.anchor.hunk.start_row + 1..comment.anchor.hunk.end_row)
                .filter(|&i| {
                    matches!(
                        file.lines[i].content.as_bytes().first(),
                        Some(b' ' | b'-' | b'+')
                    )
                })
                .nth(selection.start_row)
                .ok_or_else(|| anyhow::anyhow!("Saved rows missing; current review retained."))?
        } else {
            comment.anchor.hunk.start_row
        };
        file.selected_hunk = hunk;
        review.selected = selected;
        if let Err(error) = self.comment_list_live() {
            review.rejectable = false;
            review.reject_reason = Some(format!("Retained source view only: {error}"));
        }
        self.diff_review = Some(review);
        let projected = self
            .diff_projection()
            .iter()
            .position(|r| r.raw_row == Some(raw_row))
            .unwrap_or(raw_row);
        if let Some(file) = self.diff_review.as_mut().and_then(|r| r.current_file_mut()) {
            file.scroll = projected;
        }
        self.diff_review_focus = DiffReviewFocus::Review;
        Ok(())
    }
    fn edit_listed_comment(&mut self, comment: &Comment) -> anyhow::Result<()> {
        self.comment_list_live()?;
        self.jump_listed_comment(comment)?;
        if let Some(list) = self.comment_list.as_mut() {
            list.error = None;
        }
        self.start_comment_at(comment.anchor.clone());
        Ok(())
    }
    pub(crate) fn sync_comment_list(&mut self) {
        if let Some(list) = self.comment_list.as_mut()
            && let Some(draft) = self
                .review_feedback
                .as_ref()
                .filter(|d| d.batch.id == list.batch_id)
        {
            list.revision = draft.batch.revision;
            list.selected = list
                .selected
                .min(draft.batch.comments.len().saturating_sub(1));
            list.detail_scroll = 0;
            list.confirm_delete = false;
            list.error = None;
        }
    }
    fn delete_listed_comment(&mut self, comment: &Comment) -> anyhow::Result<()> {
        self.comment_list_live()?;
        let draft = self
            .review_feedback
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("No active batch."))?;
        draft.batch.comments.retain(|c| c.anchor != comment.anchor);
        draft.batch.revision += 1;
        self.sync_comment_list();
        self.push_log("[feedback] Selected comment removed locally. Files and other comments remain unchanged; no model request.");
        Ok(())
    }
    pub(crate) fn handle_comment_list_key(&mut self, key: KeyEvent) -> bool {
        let Some(list) = self.comment_list.as_ref() else {
            return false;
        };
        if key.kind == KeyEventKind::Release {
            return true;
        }
        let delete = list.confirm_delete;
        let result = if key.kind == KeyEventKind::Press
            && key.modifiers.is_empty()
            && key.code == KeyCode::Esc
        {
            if delete {
                if let Some(list) = self.comment_list.as_mut() {
                    list.confirm_delete = false;
                    list.error = None;
                }
            } else {
                self.comment_list = None;
            }
            Ok(())
        } else if delete {
            if key.kind == KeyEventKind::Press
                && key.modifiers.is_empty()
                && key.code == KeyCode::Enter
            {
                self.listed_comment()
                    .and_then(|c| self.delete_listed_comment(&c))
            } else {
                Ok(())
            }
        } else if key.kind == KeyEventKind::Press && key.modifiers.is_empty() {
            match key.code {
                KeyCode::Enter => self.listed_comment().and_then(|c| {
                    self.jump_listed_comment(&c)?;
                    self.comment_list = None;
                    Ok(())
                }),
                KeyCode::Char('e') => self
                    .listed_comment()
                    .and_then(|c| self.edit_listed_comment(&c)),
                KeyCode::Char('d') => self.listed_comment().and_then(|_| {
                    self.comment_list_live()?;
                    if let Some(list) = self.comment_list.as_mut() {
                        list.confirm_delete = true;
                        list.error = None;
                    }
                    Ok(())
                }),
                KeyCode::Char('s') => {
                    self.comment_list = None;
                    self.confirm_feedback();
                    Ok(())
                }
                _ => {
                    self.move_comment_list(key.code);
                    Ok(())
                }
            }
        } else if key.kind == KeyEventKind::Repeat && key.modifiers.is_empty() {
            self.move_comment_list(key.code);
            Ok(())
        } else {
            Ok(())
        };
        if let Err(error) = result
            && let Some(list) = self.comment_list.as_mut()
        {
            list.error = Some(error.to_string());
        }
        self.dirty = true;
        true
    }
    fn move_comment_list(&mut self, key: KeyCode) {
        let count = self
            .review_feedback
            .as_ref()
            .map(|d| d.batch.comments.len())
            .unwrap_or(0);
        let Some(list) = self.comment_list.as_mut() else {
            return;
        };
        let next = match key {
            KeyCode::Up => Some(list.selected.saturating_sub(1)),
            KeyCode::Down => Some(list.selected.saturating_add(1).min(count.saturating_sub(1))),
            KeyCode::PageUp => Some(list.selected.saturating_sub(5)),
            KeyCode::PageDown => Some(list.selected.saturating_add(5).min(count.saturating_sub(1))),
            KeyCode::Home => Some(0),
            KeyCode::End => Some(count.saturating_sub(1)),
            KeyCode::Left => {
                list.detail_scroll = list.detail_scroll.saturating_sub(3);
                None
            }
            KeyCode::Right => {
                list.detail_scroll = list.detail_scroll.saturating_add(3);
                None
            }
            _ => None,
        };
        if let Some(next) = next {
            list.selected = next;
            list.detail_scroll = 0;
            list.error = None;
        }
    }
    pub(crate) fn scroll_comment_list(&mut self, delta: isize) -> bool {
        if let Some(list) = self.comment_list.as_mut() {
            if !list.confirm_delete && self.comment_editor.is_none() {
                list.detail_scroll = list.detail_scroll.saturating_add_signed(delta);
            }
            self.dirty = true;
            true
        } else {
            false
        }
    }
}
